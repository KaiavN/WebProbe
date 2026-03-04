pub mod browser;
pub mod element;
pub mod state;

use crate::types::{
    AuthConfig, CookieEntry, CrawlStats, InteractiveElement, Issue, IssueCategory, NetworkStats,
    PageInteractions, PageState, PerfMetrics, Severity,
};
use anyhow::{Context, Result};
use async_channel;
use browser::{DriverKind, DriverProcess};
use console::style;
use fantoccini::{Client, ClientBuilder, Locator};
use indicatif::{ProgressBar, ProgressStyle};
use state::StateTracker;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use url::Url;

const PAGE_TIMEOUT: Duration = Duration::from_secs(30);
const LAUNCH_TIMEOUT: Duration = Duration::from_secs(30);

pub struct CrawlerConfig {
    pub start_url: String,
    pub max_depth: usize,
    pub concurrency: usize,
    pub headless: bool,
    pub settle_ms: u64,
    pub auth: AuthConfig,
    /// URL paths to never visit (e.g. ["/admin", "/logout"]).
    pub skip_paths: Vec<String>,
    /// CSS selector to scope link discovery (only follow links inside matching elements).
    /// None = discover all <a href> links on the page.
    pub link_selector: Option<String>,
}

pub struct CrawlResult {
    pub issues: Vec<Issue>,
    pub perf_metrics: Vec<PerfMetrics>,
    pub network_stats: Vec<NetworkStats>,
    pub stats: CrawlStats,
    pub discovered_urls: Vec<String>,
    pub interactions: Vec<PageInteractions>,
}

/// Open a browser session — capabilities differ between Firefox, Chrome, and Safari.
async fn new_session(driver_url: &str, headless: bool, kind: DriverKind) -> Result<Client> {
    let mut caps = serde_json::Map::new();

    // "eager" page load strategy: goto returns on DOMContentLoaded instead of waiting
    // for every asset (images, fonts, Vite HMR modules, etc.) to finish loading.
    // DOM auditing and buffered PerformanceObservers still capture everything we need.
    caps.insert("pageLoadStrategy".to_string(), serde_json::json!("eager"));

    match kind {
        DriverKind::Gecko => {
            // Firefox supports headless via -headless arg.
            let args: Vec<&str> = if headless { vec!["-headless"] } else { vec![] };
            caps.insert(
                "moz:firefoxOptions".to_string(),
                serde_json::json!({ "args": args }),
            );
        }
        DriverKind::Chrome => {
            // --disable-gpu avoids GPU process crashes on macOS (headless or not).
            // --window-size ensures a consistent viewport for layout-sensitive tests.
            let mut args = vec![
                "--no-sandbox",
                "--disable-dev-shm-usage",
                "--disable-gpu",
                "--window-size=1920,1080",
            ];
            if headless {
                args.push("--headless");
            }
            caps.insert(
                "goog:chromeOptions".to_string(),
                serde_json::json!({ "args": args }),
            );
        }
        DriverKind::Safari => {
            // safaridriver uses browserName capability; no headless support.
            caps.insert("browserName".to_string(), serde_json::json!("safari"));
        }
    }
    ClientBuilder::native()
        .capabilities(caps)
        .connect(driver_url)
        .await
        .map_err(|e| {
            let msg = e.to_string();
            if kind == DriverKind::Chrome
                && (msg.contains("version")
                    || msg.contains("session not created")
                    || msg.contains("Chrome version must be"))
            {
                anyhow::anyhow!(
                    "Failed to create {} session: {}\n\
                     Hint: Chrome and chromedriver versions must match. Run: brew upgrade chromedriver",
                    kind.label(),
                    e
                )
            } else {
                anyhow::anyhow!("Failed to create {} session: {}", kind.label(), e)
            }
        })
}

pub async fn run_crawler(config: CrawlerConfig) -> Result<CrawlResult> {
    let start = Instant::now();
    let concurrency = config.concurrency.max(1);
    let base_url = Url::parse(&config.start_url)?;
    let base_host = base_url
        .host_str()
        .filter(|h| !h.is_empty())
        .ok_or_else(|| anyhow::anyhow!("Start URL has no host: {}", config.start_url))?
        .to_string();

    // ── HTTP pre-check (fast-fail before launching browser) ──────────────────
    let http_client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()?;
    print!("  {} Checking {}… ", style("→").cyan(), &config.start_url);
    match http_client.get(&config.start_url).send().await {
        Ok(_) => println!("{}", style("ok").green().bold()),
        Err(_) => {
            println!("{}", style("unreachable").red().bold());
            anyhow::bail!(
                "Cannot reach {} — is your server running?",
                config.start_url
            );
        }
    }

    // ── Detect browser + launch driver ──────────────────────────────────────
    let launch_start = Instant::now();
    let driver = tokio::time::timeout(LAUNCH_TIMEOUT, DriverProcess::detect_and_spawn())
        .await
        .context("Driver took too long to start")??;
    let driver_url = driver.url();
    let driver_kind = driver.kind;
    println!(
        "  {} Driver ready  ({:.1}s)",
        style("✓").green(),
        launch_start.elapsed().as_secs_f64()
    );

    // ── Open N browser sessions ──────────────────────────────────────────────
    if concurrency > 1 {
        println!("  {} Opening {} sessions…", style("→").cyan(), concurrency);
    }
    let mut sessions: Vec<Client> = Vec::with_capacity(concurrency);
    for _ in 0..concurrency {
        let s = tokio::time::timeout(
            Duration::from_secs(20),
            new_session(&driver_url, config.headless, driver_kind),
        )
        .await
        .context("Browser session timed out")??;
        sessions.push(s);
    }

    let all_issues: Arc<Mutex<Vec<Issue>>> = Arc::new(Mutex::new(Vec::new()));
    let all_perf: Arc<Mutex<Vec<PerfMetrics>>> = Arc::new(Mutex::new(Vec::new()));
    let all_network: Arc<Mutex<Vec<NetworkStats>>> = Arc::new(Mutex::new(Vec::new()));
    let all_discovered: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let all_crawled: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let all_interactions: Arc<Mutex<Vec<PageInteractions>>> = Arc::new(Mutex::new(Vec::new()));
    let pages_visited = Arc::new(AtomicUsize::new(0));
    let issue_count = Arc::new(AtomicUsize::new(0));

    // ── Auth ─────────────────────────────────────────────────────────────────
    if let Some(cookies_file) = &config.auth.cookies_file {
        // Inject file cookies into ALL sessions so each worker is authenticated.
        for (idx, sess) in sessions.iter().enumerate() {
            match inject_cookies(sess, &config.start_url, cookies_file).await {
                Ok(()) => {
                    if idx == 0 {
                        println!("  {} Cookies injected", style("✓").green());
                    }
                }
                Err(e) => {
                    if idx == 0 {
                        eprintln!("  {} Cookie injection failed: {}", style("⚠").yellow(), e);
                    }
                }
            }
        }
    }

    // URL the browser lands on after login (may differ from start_url).
    let mut post_login_url: Option<String> = None;
    let mut login_discovered_links = Vec::new();
    let mut login_url_for_stats = String::new();

    if config.auth.login_url.is_some() {
        print!("  {} Authenticating… ", style("→").cyan());
        use std::io::Write;
        std::io::stdout().flush().ok();
        match perform_login(
            &sessions[0],
            &config.auth,
            &config.start_url,
            config.settle_ms,
            true,
            config.link_selector.as_deref(),
        )
        .await
        {
            Ok((issues, perf, net, links, page_interactions)) => {
                println!("{}", style("ok").green().bold());
                // Record the post-login URL so we can seed it into the BFS below.
                // After a successful login the browser is usually on the dashboard/home
                // page, which may be a different route than start_url.
                if let Ok(landed) = sessions[0].current_url().await {
                    let landed_str = landed.to_string();
                    login_url_for_stats = landed_str.clone(); // The URL we are auditing
                    let norm_landed = landed_str.trim_end_matches('/');
                    let norm_start = config.start_url.trim_end_matches('/');
                    if norm_landed != norm_start {
                        post_login_url = Some(landed_str);
                    }
                }
                login_discovered_links = links;
                // Replicate auth cookies from sessions[0] to all other sessions so that
                // every concurrent worker crawls as the authenticated user.
                if sessions.len() > 1 {
                    if let Ok(cookies) = sessions[0].get_all_cookies().await {
                        for sess in sessions.iter().skip(1) {
                            // Must be on the target domain before adding cookies.
                            sess.goto(&config.start_url).await.ok();
                            wait_for_dom_ready(sess, Duration::from_secs(5)).await;
                            for c in &cookies {
                                sess.add_cookie(c.clone()).await.ok();
                            }
                        }
                    }
                }

                // Add the login page data to global crawler stats
                if let Ok(mut g) = all_issues.lock() {
                    g.extend(issues);
                }
                if let Ok(mut g) = all_perf.lock() {
                    g.push(perf);
                }
                if let Ok(mut g) = all_network.lock() {
                    g.push(net);
                }
                if let Ok(mut g) = all_interactions.lock() {
                    g.push(page_interactions);
                }
                // Use the initial login_url (or current url) for crawled stats
                if let Ok(mut g) = all_crawled.lock() {
                    g.push(login_url_for_stats.clone());
                }
            }
            Err(e) => {
                println!("{}", style("failed").red().bold());
                eprintln!("  {} Login failed: {}", style("⚠").red().bold(), e);
                eprintln!("  {} The crawl has been aborted.", style("│").red().dim());
                eprintln!(
                    "  {} Check that your credentials and selectors are correct.",
                    style("│").red().dim()
                );
                anyhow::bail!("Login failed");
            }
        }
    }

    // ── BFS queue ────────────────────────────────────────────────────────────
    let tracker = StateTracker::new();
    let (tx, rx) = async_channel::bounded::<PageState>(2000);
    let first = PageState::new(&config.start_url);
    tracker.visit(&first.fingerprint());
    tx.send(first).await.ok();
    let mut initial_pending = 1usize;
    // Seed the post-login page into the BFS so it gets audited even if start_url
    // immediately redirects to it (which would mark it visited via the first entry).
    if let Some(ref post_url) = post_login_url {
        let post_state = PageState::new(post_url.as_str());
        let fp = post_state.fingerprint();
        if tracker.visit(&fp) {
            println!(
                "  {} Also crawling post-login page: {}",
                style("→").cyan(),
                style(post_url).bold()
            );
            initial_pending += 1;
            tx.send(post_state).await.ok();
        }
    }
    // Also add any links discovered on the login page itself
    if let Ok(auth_u) = Url::parse(&login_url_for_stats) {
        let login_state = PageState::new(&login_url_for_stats);
        for link in login_discovered_links {
            if !config.skip_paths.is_empty() {
                if let Ok(u) = Url::parse(&link) {
                    let path = u.path();
                    if config
                        .skip_paths
                        .iter()
                        .any(|s| path == s.as_str() || path.starts_with(&format!("{}/", s)))
                    {
                        continue;
                    }
                }
            }
            let child = login_state.child(&link, "link");
            let fp = child.fingerprint();
            if tracker.visit(&fp) {
                initial_pending += 1;
                tx.send(child).await.ok();
            }
            if let Ok(mut g) = all_discovered.lock() {
                g.push(link);
            }
        }
    }
    let pending = Arc::new(AtomicUsize::new(initial_pending));

    let pb = ProgressBar::new_spinner();
    pb.set_style(
        ProgressStyle::with_template(
            " {spinner:.cyan} Crawling {wide_msg}  [{elapsed_precise}]  pages:{pos}  issues:{len}",
        )
        .expect("invalid progress template"),
    );
    pb.enable_steady_tick(Duration::from_millis(80));
    pb.set_message(config.start_url.clone());

    // ── Spawn one worker task per session ────────────────────────────────────
    let mut handles = vec![];
    for session in sessions {
        let rx = rx.clone();
        let tx = tx.clone();
        let pending = Arc::clone(&pending);
        let tracker = tracker.clone();
        let all_issues = Arc::clone(&all_issues);
        let all_perf = Arc::clone(&all_perf);
        let all_network = Arc::clone(&all_network);
        let all_discovered = Arc::clone(&all_discovered);
        let all_crawled = Arc::clone(&all_crawled);
        let all_interactions = Arc::clone(&all_interactions);
        let pages_visited = Arc::clone(&pages_visited);
        let issue_count = Arc::clone(&issue_count);
        let base_host = base_host.clone();
        let settle_ms = config.settle_ms;
        let max_depth = config.max_depth;
        let pb = pb.clone();
        let skip_paths = config.skip_paths.clone();
        let link_selector = config.link_selector.clone();

        handles.push(tokio::spawn(async move {
            while let Ok(state) = rx.recv().await {
                pb.set_message(state.url.clone());

                let result = tokio::time::timeout(
                    PAGE_TIMEOUT,
                    audit_page(
                        &session,
                        &state.url,
                        &base_host,
                        settle_ms,
                        state.depth < max_depth,
                        link_selector.as_deref(),
                    ),
                )
                .await;

                match result {
                    Ok(Ok((issues, perf, net, links, page_interactions))) => {
                        let n_issues = issues.len();
                        let mut new_states = vec![];
                        for link in links {
                            // Skip any link whose path matches a skip_paths entry.
                            if !skip_paths.is_empty() {
                                if let Ok(u) = Url::parse(&link) {
                                    let path = u.path();
                                    if skip_paths.iter().any(|s| {
                                        path == s.as_str() || path.starts_with(&format!("{}/", s))
                                    }) {
                                        continue;
                                    }
                                }
                            }
                            let child = state.child(&link, "link");
                            let fp = child.fingerprint();
                            if tracker.visit(&fp) {
                                pending.fetch_add(1, Ordering::SeqCst);
                                new_states.push(child);
                            }
                        }
                        if let Ok(mut g) = all_issues.lock() {
                            g.extend(issues);
                        }
                        if let Ok(mut g) = all_perf.lock() {
                            g.push(perf);
                        }
                        if let Ok(mut g) = all_network.lock() {
                            g.push(net);
                        }
                        // crawled = pages audited; discovered = all links found across all pages
                        if let Ok(mut g) = all_crawled.lock() {
                            g.push(state.url.clone());
                        }
                        // store per-page interactive elements
                        if let Ok(mut g) = all_interactions.lock() {
                            g.push(page_interactions);
                        }
                        for link in &new_states {
                            if let Ok(mut g) = all_discovered.lock() {
                                g.push(link.url.clone());
                            }
                        }
                        let visited = pages_visited.fetch_add(1, Ordering::Relaxed) + 1;
                        issue_count.fetch_add(n_issues, Ordering::Relaxed);
                        pb.set_position(visited as u64);
                        pb.set_length(issue_count.load(Ordering::Relaxed) as u64);
                        for child in new_states {
                            tx.send(child).await.ok();
                        }
                    }
                    Ok(Err(e)) => {
                        pb.println(format!("  {} {} — {}", style("⚠").yellow(), state.url, e));
                    }
                    Err(_) => {
                        pb.println(format!(
                            "  {} {} — timed out (>{}s), skipped",
                            style("⚠").yellow(),
                            state.url,
                            PAGE_TIMEOUT.as_secs()
                        ));
                    }
                }

                if pending.fetch_sub(1, Ordering::SeqCst) == 1 {
                    tx.close();
                }
            }

            session.close().await.ok();
        }));
    }

    for h in handles {
        h.await.ok();
    }
    pb.finish_and_clear();

    // Drop driver — terminates the geckodriver process.
    drop(driver);

    let pages_visited = pages_visited.load(Ordering::Relaxed);
    let all_issues = Arc::try_unwrap(all_issues)
        .expect("worker tasks still holding Arc")
        .into_inner()
        .unwrap_or_default();
    let all_perf = Arc::try_unwrap(all_perf)
        .expect("worker tasks still holding Arc")
        .into_inner()
        .unwrap_or_default();
    let all_network = Arc::try_unwrap(all_network)
        .expect("worker tasks still holding Arc")
        .into_inner()
        .unwrap_or_default();
    let mut all_crawled = Arc::try_unwrap(all_crawled)
        .expect("worker tasks still holding Arc")
        .into_inner()
        .unwrap_or_default();
    all_crawled.sort();
    let mut all_discovered_links = Arc::try_unwrap(all_discovered)
        .expect("worker tasks still holding Arc")
        .into_inner()
        .unwrap_or_default();
    let mut all_interactions = Arc::try_unwrap(all_interactions)
        .expect("worker tasks still holding Arc")
        .into_inner()
        .unwrap_or_default();
    all_interactions.sort_by(|a, b| a.page_url.cmp(&b.page_url));
    let total_elements: usize = all_interactions.iter().map(|p| p.elements_found).sum();
    // Merge crawled URLs into discovered so the set is a true superset
    all_discovered_links.extend(all_crawled.clone());
    all_discovered_links.sort();
    all_discovered_links.dedup();

    println!(
        "  {} Crawled {} page{}  │  {} interactive element{}  │  {} issue{}",
        style("✓").green().bold(),
        pages_visited,
        if pages_visited == 1 { "" } else { "s" },
        total_elements,
        if total_elements == 1 { "" } else { "s" },
        all_issues.len(),
        if all_issues.len() == 1 { "" } else { "s" }
    );

    Ok(CrawlResult {
        issues: all_issues,
        perf_metrics: all_perf,
        network_stats: all_network,
        stats: CrawlStats {
            pages_visited,
            duration_secs: start.elapsed().as_secs_f64(),
            elements_interacted: total_elements,
            crawled_urls: all_crawled,
        },
        discovered_urls: all_discovered_links,
        interactions: all_interactions,
    })
}

// ── Page audit ───────────────────────────────────────────────────────────────

/// Navigate to a URL, collect metrics + issues, discover links.
/// All metrics come from the browser's Performance API (Navigation Timing,
/// paint entries, PerformanceObserver for LCP/CLS).
async fn audit_page(
    client: &Client,
    url: &str,
    base_host: &str,
    settle_ms: u64,
    discover: bool,
    link_selector: Option<&str>,
) -> Result<(
    Vec<Issue>,
    PerfMetrics,
    NetworkStats,
    Vec<String>,
    PageInteractions,
)> {
    // Navigate (ignore "soft" errors that still land on the page)
    let goto = tokio::time::timeout(Duration::from_secs(15), client.goto(url)).await;
    match goto {
        Err(_) => anyhow::bail!("goto timed out after 15s"),
        Ok(Err(e)) => {
            let msg = e.to_string();
            if msg.contains("net::ERR_CONNECTION_REFUSED")
                || msg.contains("net::ERR_NAME_NOT_RESOLVED")
                || msg.contains("NS_ERROR_CONNECTION_REFUSED")
            {
                anyhow::bail!("{}", msg);
            }
        }
        Ok(Ok(())) => {}
    }

    wait_for_dom_ready(client, Duration::from_secs(5)).await;

    // Inject error listeners immediately after DOM ready, before the settle
    // delay, so we capture any errors that fire during JS hydration.
    let _ = client
        .execute(
            r#"
        if (!window.__webprobe_init) {
            window.__webprobe_init = true;
            window.__webprobe_errors = window.__webprobe_errors || [];
            window.addEventListener('error', function(e) {
                var msg = e.message || String(e);
                if (e.filename) msg += ' @ ' + e.filename + ':' + e.lineno;
                window.__webprobe_errors.push(msg);
            }, { capture: true, passive: true });
            window.addEventListener('unhandledrejection', function(e) {
                try {
                    var r = e.reason;
                    var msg = (r && r.message) ? r.message : String(r);
                    window.__webprobe_errors.push('UnhandledRejection: ' + msg);
                } catch(_) {}
            }, { capture: true, passive: true });
            /* Intercept console.error too */
            var _origErr = console.error.bind(console);
            console.error = function() {
                try {
                    var parts = Array.prototype.slice.call(arguments).map(function(a) {
                        return (typeof a === 'object') ? JSON.stringify(a) : String(a);
                    });
                    window.__webprobe_errors.push('console.error: ' + parts.join(' '));
                } catch(_) {}
                _origErr.apply(console, arguments);
            };
        }
        "#,
            vec![],
        )
        .await;

    if settle_ms > 0 {
        tokio::time::sleep(Duration::from_millis(settle_ms)).await;
    }

    // Build and execute the async collection script
    let js = build_collect_js(base_host, discover, link_selector);

    const EMPTY_RESULT: &str =
        r#"{"issues":[],"links":[],"perf":{},"net":{},"lcp":null,"cls":null}"#;
    let raw = match client.execute_async(&js, vec![]).await {
        Ok(v) => v
            .as_str()
            .map(|s| s.to_string())
            .unwrap_or_else(|| EMPTY_RESULT.to_string()),
        Err(e) => {
            eprintln!("  ⚠  JS collection failed on {}: {}", url, e);
            EMPTY_RESULT.to_string()
        }
    };

    let v: serde_json::Value = serde_json::from_str(&raw).unwrap_or_default();

    // ── Issues ───────────────────────────────────────────────────────────────
    let mut page_issues: Vec<Issue> = Vec::new();
    if let Some(arr) = v["issues"].as_array() {
        for item in arr {
            let severity = match item["sev"].as_str().unwrap_or("info") {
                "error" | "uncaught" | "rejection" => Severity::Error,
                "warning" => Severity::Warning,
                _ => Severity::Info,
            };
            let category = match item["cat"].as_str().unwrap_or("") {
                "accessibility" => IssueCategory::Accessibility,
                "seo" => IssueCategory::Seo,
                "console_error" => IssueCategory::ConsoleError,
                _ => IssueCategory::ConsoleError,
            };
            page_issues.push(Issue {
                severity,
                category,
                message: item["msg"].as_str().unwrap_or("").to_string(),
                page_url: url.to_string(),
                element: item["el"].as_str().map(|s| s.to_string()),
                action_path: vec![],
            });
        }
    }

    // ── Links ────────────────────────────────────────────────────────────────
    let links: Vec<String> = v["links"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();

    // ── Performance ──────────────────────────────────────────────────────────
    let p = &v["perf"];
    let load = p["load"].as_f64();
    let perf = PerfMetrics {
        page_url: url.to_string(),
        fcp_ms: p["fcp"].as_f64(),
        lcp_ms: v["lcp"].as_f64(),
        tti_ms: p["tti"].as_f64(),
        cls_score: v["cls"].as_f64(),
        dom_content_loaded_ms: p["dcl"].as_f64(),
        load_ms: load,
    };

    // Flag performance anomalies
    if let Some(fcp) = perf.fcp_ms {
        if fcp >= 3000.0 {
            page_issues.push(Issue {
                severity: Severity::Error,
                category: IssueCategory::Performance,
                message: format!("FCP is very slow: {:.0}ms (error > 3s)", fcp),
                page_url: url.to_string(),
                element: None,
                action_path: vec![],
            });
        } else if fcp >= 1800.0 {
            page_issues.push(Issue {
                severity: Severity::Warning,
                category: IssueCategory::Performance,
                message: format!("FCP is slow: {:.0}ms (warn > 1.8s)", fcp),
                page_url: url.to_string(),
                element: None,
                action_path: vec![],
            });
        }
    }
    if let Some(lcp) = perf.lcp_ms {
        if lcp >= 4000.0 {
            page_issues.push(Issue {
                severity: Severity::Error,
                category: IssueCategory::Performance,
                message: format!("LCP is very slow: {:.0}ms (error > 4s)", lcp),
                page_url: url.to_string(),
                element: None,
                action_path: vec![],
            });
        } else if lcp >= 2500.0 {
            page_issues.push(Issue {
                severity: Severity::Warning,
                category: IssueCategory::Performance,
                message: format!("LCP is slow: {:.0}ms (warn > 2.5s)", lcp),
                page_url: url.to_string(),
                element: None,
                action_path: vec![],
            });
        }
    }
    if let Some(tti) = perf.tti_ms {
        if tti >= 7300.0 {
            page_issues.push(Issue {
                severity: Severity::Error,
                category: IssueCategory::Performance,
                message: format!("TTI is very slow: {:.0}ms (error > 7.3s)", tti),
                page_url: url.to_string(),
                element: None,
                action_path: vec![],
            });
        } else if tti >= 3800.0 {
            page_issues.push(Issue {
                severity: Severity::Warning,
                category: IssueCategory::Performance,
                message: format!("TTI is slow: {:.0}ms (warn > 3.8s)", tti),
                page_url: url.to_string(),
                element: None,
                action_path: vec![],
            });
        }
    }
    if let Some(cls) = perf.cls_score {
        if cls >= 0.25 {
            page_issues.push(Issue {
                severity: Severity::Error,
                category: IssueCategory::Performance,
                message: format!("CLS is extremely high: {:.3} (error > 0.25)", cls),
                page_url: url.to_string(),
                element: None,
                action_path: vec![],
            });
        } else if cls >= 0.1 {
            page_issues.push(Issue {
                severity: Severity::Warning,
                category: IssueCategory::Performance,
                message: format!("CLS is high: {:.3} (warn > 0.1)", cls),
                page_url: url.to_string(),
                element: None,
                action_path: vec![],
            });
        }
    }
    if let Some(ld) = perf.load_ms {
        if ld >= 4000.0 {
            page_issues.push(Issue {
                severity: Severity::Error,
                category: IssueCategory::Performance,
                message: format!("Page load is very slow: {:.0}ms (error > 4s)", ld),
                page_url: url.to_string(),
                element: None,
                action_path: vec![],
            });
        } else if ld >= 2000.0 {
            page_issues.push(Issue {
                severity: Severity::Warning,
                category: IssueCategory::Performance,
                message: format!("Page load is slow: {:.0}ms (warn > 2s)", ld),
                page_url: url.to_string(),
                element: None,
                action_path: vec![],
            });
        }
    }

    // ── Network ──────────────────────────────────────────────────────────────
    let n = &v["net"];
    let net = NetworkStats {
        page_url: url.to_string(),
        dns_ms: n["dns"].as_f64(),
        tcp_connect_ms: n["tcp"].as_f64(),
        tls_ms: n["tls"].as_f64(),
        ttfb_ms: n["ttfb"].as_f64(),
        download_ms: n["download"].as_f64(),
        resource_count: n["resource_count"].as_u64().unwrap_or(0) as usize,
        failed_resource_count: n["failed_resource_count"].as_u64().unwrap_or(0) as usize,
        failed_resource_urls: n["failed_resource_urls"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default(),
        total_transfer_kb: n["total_transfer_kb"].as_f64().unwrap_or(0.0),
        slowest_resource_ms: n["slowest_ms"].as_f64(),
        slowest_resource_url: n["slowest_url"].as_str().map(|s| s.to_string()),
    };

    // Flag network anomalies
    if let Some(ttfb) = net.ttfb_ms {
        if ttfb >= 600.0 {
            page_issues.push(Issue {
                severity: Severity::Error,
                category: IssueCategory::NetworkError,
                message: format!("TTFB is very slow: {:.0}ms (error > 600ms)", ttfb),
                page_url: url.to_string(),
                element: None,
                action_path: vec![],
            });
        } else if ttfb >= 200.0 {
            page_issues.push(Issue {
                severity: Severity::Warning,
                category: IssueCategory::NetworkError,
                message: format!("TTFB is slow: {:.0}ms (warn > 200ms)", ttfb),
                page_url: url.to_string(),
                element: None,
                action_path: vec![],
            });
        }
    }
    if net.failed_resource_count > 0 {
        let detail = if net.failed_resource_urls.is_empty() {
            String::new()
        } else {
            format!(" — {}", net.failed_resource_urls.join(", "))
        };
        page_issues.push(Issue {
            severity: Severity::Error,
            category: IssueCategory::FailedResource,
            message: format!(
                "{} resource{} failed to load{}",
                net.failed_resource_count,
                if net.failed_resource_count == 1 {
                    ""
                } else {
                    "s"
                },
                detail
            ),
            page_url: url.to_string(),
            element: None,
            action_path: vec![],
        });
    }

    // ── Interactive elements ──────────────────────────────────────────────────
    let mut elements: Vec<InteractiveElement> = Vec::new();
    if let Some(arr) = v["interactive"].as_array() {
        for item in arr {
            let kind = item["kind"].as_str().unwrap_or("unknown").to_string();
            let label = item["label"]
                .as_str()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty());
            let href = item["href"]
                .as_str()
                .map(|s| s.to_string())
                .filter(|s| !s.is_empty() && !s.starts_with("javascript"));
            let input_type = item["input_type"].as_str().map(|s| s.to_string());
            elements.push(InteractiveElement {
                kind,
                label,
                href,
                input_type,
            });
        }
    }
    let page_interactions = PageInteractions {
        page_url: url.to_string(),
        elements_found: elements.len(),
        elements,
    };

    Ok((page_issues, perf, net, links, page_interactions))
}

/// Build the async JS collection script for a given page.
/// The script:
///   1. If the DOM already has meaningful content, runs audits immediately.
///   2. Otherwise (SPA shell), installs a MutationObserver and waits for
///      content to appear before auditing — no Rust-side polling required.
///   3. Sets up PerformanceObservers with `buffered:true` for LCP and CLS.
///   4. Hard safety timeout (8 s) ensures the callback is always called.
fn build_collect_js(base_host: &str, discover: bool, link_selector: Option<&str>) -> String {
    let anchor_query = match link_selector {
        Some(sel) => format!(
            "(document.querySelector({sel_json}) || document).querySelectorAll('a[href]')",
            sel_json = serde_json::to_string(sel).unwrap_or_else(|_| "\"a\"".into()),
        ),
        None => "document.querySelectorAll('a[href]')".to_string(),
    };
    format!(
        r#"
(function(cb) {{
    var _called = false;
    var done = function(data) {{
        if (!_called) {{ _called = true; cb(JSON.stringify(data)); }}
    }};

    // Hard safety net: always resolve within 10s regardless
    var safetyTimer = setTimeout(function() {{
        done({{ issues:[], links:[], perf:{{}}, net:{{}}, lcp:null, cls:null }});
    }}, 10000);

    function runCollect() {{
        clearTimeout(safetyTimer);
        try {{
            // ── Runtime JS errors (captured by the pre-injected listener) ──────────
            var issues = [];
            var runtimeErrors = window.__webprobe_errors || [];
            for (var rei = 0; rei < runtimeErrors.length; rei++) {{
                issues.push({{ sev:'error', cat:'console_error', msg: runtimeErrors[rei] }});
            }}

            // ── Accessibility issues ───────────────────────────────────────────────
            document.querySelectorAll('img:not([alt])').forEach(function(el) {{
                issues.push({{ sev:'warning', cat:'accessibility',
                    msg:'Image missing alt attribute', el:el.outerHTML.slice(0,120) }});
            }});

            document.querySelectorAll(
                'input:not([type="hidden"]):not([type="submit"]):not([type="button"]):not([type="reset"]):not([type="image"]):not([aria-label]):not([aria-labelledby])'
            ).forEach(function(el) {{
                if (!el.id || !document.querySelector('label[for="' + el.id + '"]')) {{
                    issues.push({{ sev:'warning', cat:'accessibility',
                        msg:'Input missing label / aria-label', el:el.outerHTML.slice(0,120) }});
                }}
            }});

            document.querySelectorAll('button:not([aria-label]):not([aria-labelledby])').forEach(function(el) {{
                if (!el.textContent.trim()) {{
                    issues.push({{ sev:'warning', cat:'accessibility',
                        msg:'Button has no accessible text', el:el.outerHTML.slice(0,120) }});
                }}
            }});

            document.querySelectorAll('a:not([aria-label]):not([aria-labelledby])').forEach(function(el) {{
                if (!el.textContent.trim() && !el.querySelector('img[alt]')) {{
                    issues.push({{ sev:'warning', cat:'accessibility',
                        msg:'Link has no accessible text', el:el.outerHTML.slice(0,120) }});
                }}
            }});

            // Images without explicit width/height — causes layout shifts (CLS)
            document.querySelectorAll('img').forEach(function(el) {{
                if (!el.getAttribute('width') || !el.getAttribute('height')) {{
                    issues.push({{ sev:'info', cat:'accessibility',
                        msg:'Image missing width/height attributes (may cause layout shift)',
                        el: el.outerHTML.slice(0,120) }});
                }}
            }});

            // ── SEO / Security issues ─────────────────────────────────────────────
            var titleEl = document.querySelector('title');
            if (!titleEl || !titleEl.textContent.trim()) {{
                issues.push({{ sev:'warning', cat:'seo', msg:'Page is missing a <title> element' }});
            }} else {{
                var titleLen = titleEl.textContent.trim().length;
                if (titleLen > 60) {{
                    issues.push({{ sev:'info', cat:'seo',
                        msg:'<title> is ' + titleLen + ' characters (recommended max: 60)' }});
                }}
            }}
            if (!document.documentElement.getAttribute('lang')) {{
                issues.push({{ sev:'warning', cat:'accessibility',
                    msg:'<html> element is missing a lang attribute' }});
            }}
            if (!document.querySelector('meta[name="viewport"]')) {{
                issues.push({{ sev:'info', cat:'seo',
                    msg:'Missing <meta name="viewport"> \u2014 page may render poorly on mobile' }});
            }}
            if (!document.querySelector('meta[name="description"]')) {{
                issues.push({{ sev:'info', cat:'seo', msg:'Missing <meta name="description">' }});
            }}

            // Duplicate <h1> — bad for SEO
            var h1Count = document.querySelectorAll('h1').length;
            if (h1Count > 1) {{
                issues.push({{ sev:'warning', cat:'seo',
                    msg: h1Count + ' <h1> elements found on this page (should be exactly one)' }});
            }}

            // <a target="_blank"> without rel="noopener" — security risk
            document.querySelectorAll('a[target="_blank"]').forEach(function(el) {{
                var rel = (el.getAttribute('rel') || '').toLowerCase();
                if (rel.indexOf('noopener') < 0) {{
                    issues.push({{ sev:'warning', cat:'seo',
                        msg:'External link opens in new tab without rel="noopener noreferrer"',
                        el: el.outerHTML.slice(0,120) }});
                }}
            }});

            // ── Interactive element discovery ─────────────────────────────────────
            var interactive = [];

            // Links (all <a href> on the page, including cross-origin for completeness)
            document.querySelectorAll('a[href]').forEach(function(el) {{
                var label = (el.textContent || '').trim()
                    || el.getAttribute('aria-label')
                    || el.getAttribute('title')
                    || null;
                interactive.push({{ kind:'link', label: label || null, href: el.href || null }});
            }});

            // Buttons — <button>, role="button", input[submit/button/reset]
            document.querySelectorAll('button, [role="button"], input[type="submit"], input[type="button"], input[type="reset"]').forEach(function(el) {{
                var label = (el.textContent || '').trim()
                    || el.getAttribute('aria-label')
                    || el.getAttribute('value')
                    || el.getAttribute('title')
                    || null;
                interactive.push({{ kind:'button', label: label || null }});
            }});

            // Text-like inputs
            document.querySelectorAll('input:not([type="hidden"]):not([type="submit"]):not([type="button"]):not([type="reset"])').forEach(function(el) {{
                var label = el.getAttribute('aria-label')
                    || el.getAttribute('placeholder')
                    || el.getAttribute('name')
                    || null;
                interactive.push({{ kind:'input', label: label || null, input_type: el.type || 'text' }});
            }});

            // Select dropdowns
            document.querySelectorAll('select').forEach(function(el) {{
                var label = el.getAttribute('aria-label')
                    || el.getAttribute('name')
                    || null;
                interactive.push({{ kind:'select', label: label || null }});
            }});

            // Textareas
            document.querySelectorAll('textarea').forEach(function(el) {{
                var label = el.getAttribute('aria-label')
                    || el.getAttribute('placeholder')
                    || el.getAttribute('name')
                    || null;
                interactive.push({{ kind:'textarea', label: label || null }});
            }});

            // ── Link discovery (for BFS) ──────────────────────────────────────────
            var links = [];
            if ({discover}) {{
                var seen = new Set();
                {anchor_query}.forEach(function(el) {{
                    try {{
                        var u = new URL(el.href, location.href);
                        if (u.hostname !== {base_host_json}) return;
                        u.hash = ''; u.search = '';
                        if (u.pathname !== '/' && u.pathname.endsWith('/')) {{
                            u.pathname = u.pathname.slice(0, -1);
                        }}
                        var k = u.toString();
                        if (!seen.has(k)) {{ seen.add(k); links.push(k); }}
                    }} catch(_) {{}}
                }});
            }}

            // ── Navigation Timing (synchronous) ──────────────────────────────────
            var nav = performance.getEntriesByType('navigation')[0] || {{}};
            var paint = {{}};
            performance.getEntriesByType('paint').forEach(function(e) {{ paint[e.name] = e.startTime; }});

            var dns  = (nav.domainLookupEnd  != null && nav.domainLookupStart != null)
                       ? nav.domainLookupEnd  - nav.domainLookupStart : null;
            var tcp  = (nav.connectEnd       != null && nav.connectStart      != null)
                       ? nav.connectEnd       - nav.connectStart      : null;
            var tls  = (nav.secureConnectionStart > 0 && nav.connectEnd != null)
                       ? nav.connectEnd - nav.secureConnectionStart    : null;
            var ttfb = (nav.responseStart    != null && nav.requestStart      != null)
                       ? nav.responseStart   - nav.requestStart       : null;
            var dl   = (nav.responseEnd      != null && nav.responseStart     != null)
                       ? nav.responseEnd     - nav.responseStart      : null;

            // Tightened heuristic: a resource truly failed if:
            //   - transferSize is 0 AND decodedBodySize is 0 (not cached, not prefetch)
            //   - duration is suspiciously short (< 5ms — genuine failures resolve instantly)
            //   - not a data URI or blob URI (those are always "0-byte" by design)
            //   - responseStatus (Chrome 109+) is 4xx/5xx when available
            var resources = performance.getEntriesByType('resource');
            var totalBytes = 0, failedCount = 0, failedUrls = [], slowestMs = 0, slowestUrl = null;
            for (var i = 0; i < resources.length; i++) {{
                var r = resources[i];
                if (r.transferSize) totalBytes += r.transferSize;
                var name = r.name || '';
                var isDataOrBlob = name.startsWith('data:') || name.startsWith('blob:');
                var likelyFailed = !isDataOrBlob
                    && r.transferSize === 0
                    && r.decodedBodySize === 0
                    && r.duration < 5;
                // If responseStatus is available (Chrome 109+) use it as ground truth
                if (r.responseStatus && r.responseStatus >= 400) {{
                    likelyFailed = true;
                }}
                if (likelyFailed) {{
                    failedCount++;
                    failedUrls.push(name);
                }}
                if (r.duration > slowestMs) {{ slowestMs = r.duration; slowestUrl = name; }}
            }}

            var collected = {{
                issues: issues,
                links:  links,
                interactive: interactive,
                perf: {{
                    fcp:  paint['first-contentful-paint'] || null,
                    dcl:  nav.domContentLoadedEventEnd > 0 ? nav.domContentLoadedEventEnd : null,
                    load: nav.loadEventEnd   > 0 ? nav.loadEventEnd   : null,
                    tti:  nav.domInteractive > 0 ? nav.domInteractive : null
                }},
                net: {{
                    dns:  dns,  tcp: tcp, tls: tls, ttfb: ttfb, download: dl,
                    resource_count:        resources.length,
                    failed_resource_count: failedCount,
                    failed_resource_urls:  failedUrls,
                    total_transfer_kb:     totalBytes / 1024,
                    slowest_ms:  slowestMs  > 0    ? slowestMs  : null,
                    slowest_url: slowestUrl !== null ? slowestUrl : null
                }},
                lcp: null,
                cls: null
            }};

            // ── LCP + CLS via buffered PerformanceObservers ───────────────────────
            var lcpDone = false, clsDone = false;
            var clsTotal = 0;
            var checkDone = function() {{ if (lcpDone && clsDone) done(collected); }};

            try {{
                new PerformanceObserver(function(list) {{
                    var e = list.getEntries();
                    if (e.length) collected.lcp = e[e.length - 1].startTime;
                    lcpDone = true; checkDone();
                }}).observe({{ type: 'largest-contentful-paint', buffered: true }});
            }} catch(_) {{ lcpDone = true; checkDone(); }}

            try {{
                new PerformanceObserver(function(list) {{
                    list.getEntries().forEach(function(e) {{
                        if (!e.hadRecentInput) clsTotal += e.value;
                    }});
                    collected.cls = clsTotal > 0 ? clsTotal : null;
                    clsDone = true; checkDone();
                }}).observe({{ type: 'layout-shift', buffered: true }});
            }} catch(_) {{ clsDone = true; checkDone(); }}

            // Safety net: deliver whatever we have after 500ms
            setTimeout(function() {{ lcpDone = true; clsDone = true; checkDone(); }}, 500);

        }} catch(err) {{
            done({{ issues:[], links:[], perf:{{}}, net:{{}}, lcp:null, cls:null, _error: err.toString() }});
        }}
    }}

    // ── SPA content detection ─────────────────────────────────────────────────
    // With eager page load strategy, goto returns at DOMContentLoaded before
    // SPA frameworks (React, Vue, etc.) have had a chance to render. We use a
    // MutationObserver to detect when real content appears, then collect.
    function hasContent() {{
        var b = document.body;
        if (!b) return false;
        // Consider the page "ready" once it has either a meaningful amount of
        // HTML or at least one navigable link.
        return b.innerHTML.length >= 200 || b.querySelectorAll('a[href]').length > 0;
    }}

    if (hasContent() || document.readyState === 'complete') {{
        runCollect();
    }} else {{
        var mo = new MutationObserver(function() {{
            if (hasContent()) {{
                mo.disconnect();
                runCollect();
            }}
        }});
        mo.observe(document.body || document.documentElement, {{ childList: true, subtree: true }});
        // Fallback: collect after 6s even if content threshold not met
        setTimeout(function() {{ mo.disconnect(); runCollect(); }}, 6000);
    }}
}})(arguments[arguments.length - 1]);
"#,
        discover = if discover { "true" } else { "false" },
        base_host_json =
            serde_json::to_string(base_host).unwrap_or_else(|_| format!("\"{}\"", base_host)),
        anchor_query = anchor_query,
    )
}

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Poll `document.readyState` until complete/interactive or timeout.
async fn wait_for_dom_ready(client: &Client, timeout: Duration) {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if tokio::time::Instant::now() >= deadline {
            break;
        }
        let state = client
            .execute("return document.readyState", vec![])
            .await
            .ok()
            .and_then(|v| v.as_str().map(|s| s.to_string()));

        match state.as_deref() {
            Some("complete") | Some("interactive") => break,
            _ => tokio::time::sleep(Duration::from_millis(80)).await,
        }
    }
}

/// Inject cookies from a JSON file using the WebDriver AddCookie protocol.
/// This correctly handles HttpOnly and Secure cookies, unlike document.cookie.
/// The browser must already be on the target origin for the cookies to apply.
async fn inject_cookies(
    client: &Client,
    base_url: &str,
    cookies_file: &std::path::Path,
) -> Result<()> {
    client.goto(base_url).await.ok();
    wait_for_dom_ready(client, Duration::from_secs(5)).await;

    let raw = std::fs::read_to_string(cookies_file)
        .with_context(|| format!("Cannot read cookie file: {}", cookies_file.display()))?;
    let entries: Vec<CookieEntry> = serde_json::from_str(&raw)
        .context("Cookie file must be a JSON array of { name, value, ... }")?;

    let mut failed = 0usize;
    for e in &entries {
        let mut cookie = fantoccini::cookies::Cookie::new(e.name.clone(), e.value.clone());
        if let Some(path) = &e.path {
            cookie.set_path(path.clone());
        }
        if let Some(domain) = &e.domain {
            // WebDriver requires domain without leading dot for most drivers
            let domain_clean = domain.trim_start_matches('.').to_string();
            cookie.set_domain(domain_clean);
        }
        if e.secure == Some(true) {
            cookie.set_secure(true);
        }
        if e.http_only == Some(true) {
            cookie.set_http_only(true);
        }

        if let Err(_) = client.add_cookie(cookie).await {
            // Fallback to document.cookie for cookies that fail via WebDriver
            let mut parts = format!("{}={}", e.name, e.value);
            if let Some(path) = &e.path {
                parts.push_str(&format!("; path={}", path));
            }
            let js = format!("document.cookie = {:?};", parts);
            if client.execute(&js, vec![]).await.is_err() {
                failed += 1;
            }
        }
    }

    if failed > 0 {
        eprintln!(
            "  {} {} of {} cookies failed to inject",
            console::style("⚠").yellow(),
            failed,
            entries.len()
        );
    }
    Ok(())
}

/// Try a list of CSS selectors in order; return the first element found.
async fn find_first(client: &Client, selectors: &[&str]) -> Option<fantoccini::elements::Element> {
    for sel in selectors {
        if let Ok(el) = client.find(Locator::Css(sel)).await {
            return Some(el);
        }
    }
    None
}

/// Perform form login using native WebDriver commands (find → send_keys → click).
/// Falls back through a ranked list of selectors for each field so that most
/// login forms are handled without any custom selector flags.
async fn perform_login(
    client: &Client,
    auth: &AuthConfig,
    base_url: &str,
    settle_ms: u64,
    discover: bool,
    link_selector: Option<&str>,
) -> Result<(
    Vec<Issue>,
    PerfMetrics,
    NetworkStats,
    Vec<String>,
    PageInteractions,
)> {
    let login_url = match &auth.login_url {
        Some(u) if u.starts_with("http://") || u.starts_with("https://") => u.clone(),
        Some(path) if path.starts_with('/') => {
            // Root-relative path: use only the origin (scheme + host + port), ignore base_url path.
            // e.g. base_url="http://localhost:3000/app", path="/login" → "http://localhost:3000/login"
            let base =
                Url::parse(base_url).unwrap_or_else(|_| Url::parse("http://localhost").unwrap());
            let mut origin = format!(
                "{}://{}",
                base.scheme(),
                base.host_str().unwrap_or("localhost")
            );
            if let Some(port) = base.port() {
                origin.push_str(&format!(":{}", port));
            }
            format!("{}{}", origin, path)
        }
        Some(path) => format!(
            "{}/{}",
            base_url.trim_end_matches('/'),
            path.trim_start_matches('/')
        ),
        None => anyhow::bail!("No login URL provided"),
    };

    // Call audit_page on the login view directly. This handles .goto(), waits
    // for DOM ready, injects the JS collectors, and gathers interactive elements.
    let base_host = Url::parse(base_url)
        .unwrap_or_else(|_| Url::parse("http://localhost").unwrap())
        .host_str()
        .unwrap_or("localhost")
        .to_string();

    let page_stats = audit_page(
        client,
        &login_url,
        &base_host,
        settle_ms,
        discover,
        link_selector,
    )
    .await?;

    // Now we are actively on the login page; wait for the form input to appear

    // Poll until at least one <input> appears — handles SPAs that render the
    // login form asynchronously after the initial DOM ready event.
    let form_deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        if client.find(Locator::Css("input")).await.is_ok() {
            break;
        }
        if tokio::time::Instant::now() >= form_deadline {
            anyhow::bail!(
                "Navigated to {} but no <input> elements appeared within 10s.\n\
                 Is that the correct login URL?",
                login_url
            );
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
    // Small extra wait for the rest of the form to finish rendering
    tokio::time::sleep(Duration::from_millis(300)).await;

    let username = auth.username.as_deref().unwrap_or("");
    let password = auth.password.as_deref().unwrap_or("");

    // ── Username / email field ────────────────────────────────────────────────
    let user_el = if let Some(sel) = auth.username_selector.as_deref() {
        client
            .find(Locator::Css(sel))
            .await
            .map_err(|_| anyhow::anyhow!("Username selector '{}' matched nothing", sel))?
    } else {
        // 1. Try ranked CSS selectors (type/autocomplete/name/id/aria/placeholder)
        let css_found = find_first(
            client,
            &[
                "input[type='email']",
                "input[autocomplete='email']",
                "input[autocomplete='username']",
                "input[name*='email' i]",
                "input[name*='username' i]",
                "input[name*='login' i]",
                "input[name*='user' i]",
                "input[id*='email' i]",
                "input[id*='user' i]",
                "input[id*='login' i]",
                "input[aria-label*='email' i]",
                "input[aria-label*='user' i]",
                "input[aria-label*='login' i]",
                "input[title*='email' i]",
                "input[title*='user' i]",
                "input[placeholder*='email' i]",
                "input[placeholder*='user' i]",
                "input[placeholder*='login' i]",
            ],
        )
        .await;

        if let Some(el) = css_found {
            el
        } else {
            // 2. JS fallback: scan label text, aria-labelledby, and surrounding
            //    text for any visible text input whose label mentions "email" or
            //    "user" — catches React/Vue/Angular forms with generated class names.
            let js = r#"
(function() {
    var keywords = ['email', 'username', 'user name', 'e-mail', 'login', 'account'];
    var inputs = Array.from(document.querySelectorAll(
        'input:not([type="hidden"]):not([type="submit"]):not([type="button"])' +
        ':not([type="checkbox"]):not([type="radio"]):not([type="password"])'));
    var best = null, bestScore = 0;
    inputs.forEach(function(el) {
        var r = el.getBoundingClientRect();
        if (r.width === 0 || r.height === 0 || el.disabled) return;
        var texts = [
            el.getAttribute('aria-label') || '',
            el.getAttribute('placeholder') || '',
            el.getAttribute('title') || '',
            el.getAttribute('name') || '',
            el.getAttribute('id') || ''
        ];
        if (el.id) {
            var lbl = document.querySelector('label[for="' + el.id + '"]');
            if (lbl) texts.push(lbl.textContent);
        }
        var labelled = el.getAttribute('aria-labelledby');
        if (labelled) {
            labelled.split(' ').forEach(function(id) {
                var n = document.getElementById(id);
                if (n) texts.push(n.textContent);
            });
        }
        var combined = texts.join(' ').toLowerCase();
        for (var i = 0; i < keywords.length; i++) {
            if (combined.indexOf(keywords[i]) >= 0) {
                var score = keywords.length - i;
                if (score > bestScore) { bestScore = score; best = el; }
                break;
            }
        }
    });
    if (!best) return null;
    if (best.id) return '[id="' + best.id.replace(/"/g, '\\"') + '"]';
    if (best.name) return 'input[name="' + best.name.replace(/"/g, '\\"') + '"]';
    return null;
})()
"#;
            let sel_val = client.execute(js, vec![]).await.ok();
            let js_sel = sel_val
                .as_ref()
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty());

            if let Some(sel) = js_sel {
                client.find(Locator::Css(sel)).await.ok()
            } else {
                // 3. Last resort: first visible text input on the page
                find_first(client, &["input[type='text']"]).await
            }
            .ok_or_else(|| {
                anyhow::anyhow!("Could not find a username/email field on the login page")
            })?
        }
    };
    user_el.click().await.ok();
    user_el.clear().await.ok();
    user_el
        .send_keys(username)
        .await
        .context("Failed to type into username field")?;

    // Dispatch native events to ensure SPA frameworks (React/Vue) detect the change
    let _ = client
        .execute(
            "if (document.activeElement) { \
             document.activeElement.dispatchEvent(new Event('input', { bubbles: true })); \
             document.activeElement.dispatchEvent(new Event('change', { bubbles: true })); \
         }",
            vec![],
        )
        .await;

    // ── Multi-step login: if password field isn't visible yet, click "Next" ──
    // Use JS visibility check (offsetParent/offsetWidth) rather than WebDriver find(),
    // which returns true for hidden elements too — avoids clicking submit prematurely
    // on single-page forms where the password field is in the DOM but display:none.
    // Shared XPath for finding buttons like "Next", "Continue", "Submit", "Login", "Sign In"
    let button_xpath = "//button[contains(translate(normalize-space(.), \
        'ABCDEFGHIJKLMNOPQRSTUVWXYZ', 'abcdefghijklmnopqrstuvwxyz'), 'sign in')] | \
        //button[contains(translate(normalize-space(.), \
        'ABCDEFGHIJKLMNOPQRSTUVWXYZ', 'abcdefghijklmnopqrstuvwxyz'), 'sign-in')] | \
        //button[contains(translate(normalize-space(.), \
        'ABCDEFGHIJKLMNOPQRSTUVWXYZ', 'abcdefghijklmnopqrstuvwxyz'), 'log in')] | \
        //button[contains(translate(normalize-space(.), \
        'ABCDEFGHIJKLMNOPQRSTUVWXYZ', 'abcdefghijklmnopqrstuvwxyz'), 'login')] | \
        //button[contains(translate(normalize-space(.), \
        'ABCDEFGHIJKLMNOPQRSTUVWXYZ', 'abcdefghijklmnopqrstuvwxyz'), 'continue')] | \
        //button[contains(translate(normalize-space(.), \
        'ABCDEFGHIJKLMNOPQRSTUVWXYZ', 'abcdefghijklmnopqrstuvwxyz'), 'next')] | \
        //button[contains(translate(normalize-space(.), \
        'ABCDEFGHIJKLMNOPQRSTUVWXYZ', 'abcdefghijklmnopqrstuvwxyz'), 'submit')] | \
        //*[@role='button' and contains(translate(normalize-space(.), \
        'ABCDEFGHIJKLMNOPQRSTUVWXYZ', 'abcdefghijklmnopqrstuvwxyz'), 'sign in')] | \
        //*[@role='button' and contains(translate(normalize-space(.), \
        'ABCDEFGHIJKLMNOPQRSTUVWXYZ', 'abcdefghijklmnopqrstuvwxyz'), 'log in')] | \
        //*[@role='button' and contains(translate(normalize-space(.), \
        'ABCDEFGHIJKLMNOPQRSTUVWXYZ', 'abcdefghijklmnopqrstuvwxyz'), 'login')] | \
        //input[@type='button' and contains(translate(@value, \
        'ABCDEFGHIJKLMNOPQRSTUVWXYZ', 'abcdefghijklmnopqrstuvwxyz'), 'login')] | \
        //input[@type='button' and contains(translate(@value, \
        'ABCDEFGHIJKLMNOPQRSTUVWXYZ', 'abcdefghijklmnopqrstuvwxyz'), 'sign in')] | \
        //button[contains(translate(@aria-label, \
        'ABCDEFGHIJKLMNOPQRSTUVWXYZ', 'abcdefghijklmnopqrstuvwxyz'), 'sign in')] | \
        //button[contains(translate(@aria-label, \
        'ABCDEFGHIJKLMNOPQRSTUVWXYZ', 'abcdefghijklmnopqrstuvwxyz'), 'log in')] | \
        //button[contains(translate(@aria-label, \
        'ABCDEFGHIJKLMNOPQRSTUVWXYZ', 'abcdefghijklmnopqrstuvwxyz'), 'login')]";

    let password_immediately_visible = client
        .execute(
            "var el = document.querySelector(\"input[type='password']\"); \
             return !!(el && el.offsetParent !== null && el.offsetWidth > 0 && el.offsetHeight > 0);",
            vec![],
        )
        .await
        .ok()
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    if !password_immediately_visible {
        let next_xpath = "//button[contains(translate(normalize-space(.), 'ABCDEFGHIJKLMNOPQRSTUVWXYZ', 'abcdefghijklmnopqrstuvwxyz'), 'continue')] | \
            //button[contains(translate(normalize-space(.), 'ABCDEFGHIJKLMNOPQRSTUVWXYZ', 'abcdefghijklmnopqrstuvwxyz'), 'next')] | \
            //input[@type='button' and contains(translate(@value, 'ABCDEFGHIJKLMNOPQRSTUVWXYZ', 'abcdefghijklmnopqrstuvwxyz'), 'continue')] | \
            //input[@type='button' and contains(translate(@value, 'ABCDEFGHIJKLMNOPQRSTUVWXYZ', 'abcdefghijklmnopqrstuvwxyz'), 'next')]";

        // Try precise next buttons first
        let mut next_btn_opt = client.find(Locator::XPath(next_xpath)).await.ok();

        if next_btn_opt.is_none() {
            next_btn_opt =
                find_first(client, &["button[type='submit']", "input[type='submit']"]).await;
        }

        if next_btn_opt.is_none() {
            // Fallback to the broad XPath for things like a generic "Next" button
            next_btn_opt = client.find(Locator::XPath(button_xpath)).await.ok();
        }

        if let Some(next_btn) = next_btn_opt {
            next_btn.click().await.ok();
            let step2_deadline = tokio::time::Instant::now() + Duration::from_secs(5);
            loop {
                let is_visible = client
                    .execute(
                        "var el = document.querySelector(\"input[type='password']\"); \
                         return !!(el && el.offsetParent !== null && el.offsetWidth > 0 && el.offsetHeight > 0);",
                        vec![],
                    )
                    .await
                    .ok()
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                if is_visible {
                    break;
                }
                if tokio::time::Instant::now() >= step2_deadline {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        }
    }

    // ── Password field ────────────────────────────────────────────────────────
    let pass_el = if let Some(sel) = auth.password_selector.as_deref() {
        client
            .find(Locator::Css(sel))
            .await
            .map_err(|_| anyhow::anyhow!("Password selector '{}' matched nothing", sel))?
    } else {
        find_first(
            client,
            &[
                "input[type='password']",
                "input[autocomplete='current-password']",
                "input[autocomplete='new-password']",
                "input[aria-label*='password' i]",
                "input[title*='password' i]",
                "input[placeholder*='password' i]",
                "input[name*='password' i]",
                "input[id*='password' i]",
                "input[name*='pass' i]",
                "input[id*='pass' i]",
            ],
        )
        .await
        .ok_or_else(|| anyhow::anyhow!("Could not find a password field on the login page"))?
    };
    pass_el.click().await.ok();
    pass_el.clear().await.ok();
    pass_el
        .send_keys(password)
        .await
        .context("Failed to type into password field")?;

    // Dispatch native events for password
    let _ = client
        .execute(
            "if (document.activeElement) { \
             document.activeElement.dispatchEvent(new Event('input', { bubbles: true })); \
             document.activeElement.dispatchEvent(new Event('change', { bubbles: true })); \
         }",
            vec![],
        )
        .await;

    let submit_el_opt = if let Some(sel) = auth.submit_selector.as_deref() {
        client.find(Locator::Css(sel)).await.ok()
    } else {
        // 1. Explicit type=submit or type=image (image submit buttons)
        if let Some(el) = find_first(
            client,
            &[
                "button[type='submit']",
                "input[type='submit']",
                "input[type='image']",
            ],
        )
        .await
        {
            Some(el)
        } else {
            // 2. Button or role=button whose visible text or aria-label contains login keyword
            if let Ok(el) = client.find(Locator::XPath(button_xpath)).await {
                Some(el)
            } else {
                // 3. Last button inside a <form>, then last button on the page
                let all = client
                    .find_all(Locator::Css(
                        "form button, form [role='button'], button, [role='button']",
                    ))
                    .await
                    .unwrap_or_default();
                all.into_iter().last()
            }
        }
    };

    if let Some(submit_el) = submit_el_opt {
        submit_el
            .click()
            .await
            .context("Failed to click sign-in button")?;
    } else {
        // No submit button found. Fall back to pressing Enter in the password field.
        pass_el
            .send_keys("\u{e007}")
            .await
            .context("Failed to send Enter key to password field as a fallback submit")?;
    }

    // Wait up to 10 seconds for the login transition to complete
    let login_deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let mut final_url = String::new();
    let mut still_on_login = true;

    loop {
        tokio::time::sleep(Duration::from_millis(250)).await;

        if let Ok(current_url) = client.current_url().await {
            let current = current_url.as_str();
            final_url = current.to_string();

            let url_indicates_login_page = Url::parse(current)
                .ok()
                .map(|mut cu| {
                    if let Ok(mut lu) = Url::parse(&login_url) {
                        cu.set_query(None);
                        cu.set_fragment(None);
                        lu.set_query(None);
                        lu.set_fragment(None);
                        if cu == lu {
                            return true;
                        }
                    }
                    let path = cu.path().to_lowercase();
                    path.split('/').any(|seg| {
                        matches!(
                            seg,
                            "login" | "signin" | "sign-in" | "auth" | "authenticate"
                        )
                    })
                })
                .unwrap_or(false);

            let password_visible = client
                .execute(
                    "var el = document.querySelector(\"input[type='password']\"); \
                     return !!(el && el.offsetParent !== null && el.offsetWidth > 0 && el.offsetHeight > 0);",
                    vec![],
                )
                .await
                .ok()
                .and_then(|v| v.as_bool())
                .unwrap_or(false);

            still_on_login = url_indicates_login_page && password_visible;

            if !still_on_login {
                break;
            }
        }

        if tokio::time::Instant::now() >= login_deadline {
            break;
        }
    }

    tokio::time::sleep(Duration::from_millis(settle_ms.max(500))).await;
    wait_for_dom_ready(client, Duration::from_secs(8)).await;

    // Additionally check if the page body indicates a generic auth failure
    let body_text = client
        .execute(
            "return document.body ? document.body.innerText.toLowerCase() : '';",
            vec![],
        )
        .await
        .ok()
        .and_then(|v| v.as_str().map(|s| s.to_string()))
        .unwrap_or_default();

    // Check for CAPTCHA / Anti-Bot signatures
    let indicates_bot_check = body_text.contains("cf-turnstile")
        || body_text.contains("cloudflare")
        || body_text.contains("recaptcha")
        || body_text.contains("hcaptcha")
        || body_text.contains("datadome")
        || body_text.contains("security check")
        || body_text.contains("verify you are human")
        || body_text.contains("pardon our interruption");

    if indicates_bot_check {
        anyhow::bail!(
            "{} Login blocked by an anti-bot challenge (CAPTCHA/Cloudflare). The crawler cannot proceed.",
            console::style("⚠").red().bold()
        );
    }

    let indicates_error = body_text.contains("forbidden")
        || body_text.contains("unauthorized")
        || body_text.contains("invalid credentials")
        || body_text.contains("incorrect password")
        || body_text.contains("wrong password")
        || body_text.contains("invalid email")
        || body_text.contains("incorrect email")
        || body_text.contains("login failed")
        || body_text.contains("authentication failed");

    // ── Login verification ────────────────────────────────────────────────────
    if still_on_login || indicates_error {
        let msg = format!(
            "{} Login failed — still on login page or received error: {}",
            console::style("⚠").red().bold(),
            final_url
        );
        anyhow::bail!(msg);
    }

    Ok(page_stats)
}
