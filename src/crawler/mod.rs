pub mod browser;
pub mod element;
pub mod state;

use crate::types::{
    AuthConfig, CookieEntry, CrawlStats, Issue, IssueCategory, NetworkStats, PageState,
    PerfMetrics, Severity,
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

const PAGE_TIMEOUT: Duration = Duration::from_secs(20);
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
}

/// Open a browser session — capabilities differ between Firefox, Chrome, and Safari.
async fn new_session(driver_url: &str, headless: bool, kind: DriverKind) -> Result<Client> {
    let mut caps = serde_json::Map::new();
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
            caps.insert(
                "browserName".to_string(),
                serde_json::json!("safari"),
            );
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

    // ── Auth ─────────────────────────────────────────────────────────────────
    if let Some(cookies_file) = &config.auth.cookies_file {
        if let Err(e) = inject_cookies(&sessions[0], &config.start_url, cookies_file).await {
            eprintln!("  {} Cookie injection failed: {}", style("⚠").yellow(), e);
        } else {
            println!("  {} Cookies injected", style("✓").green());
        }
    }

    if config.auth.login_url.is_some() {
        print!("  {} Authenticating… ", style("→").cyan());
        match perform_login(&sessions[0], &config.auth, &config.start_url, config.settle_ms).await
        {
            Ok(()) => println!("{}", style("ok").green().bold()),
            Err(e) => {
                println!("{}", style("failed").red().bold());
                eprintln!("  {} Login failed: {}", style("⚠").red().bold(), e);
                eprintln!("  {} The crawl will continue without authentication.", style("│").red().dim());
                eprintln!("  {} If credentials are correct, try specifying selectors:", style("│").red().dim());
                eprintln!("  {}   --auth-username-selector \"#email\"", style("│").red().dim());
                eprintln!("  {}   --auth-password-selector \"#password\"", style("│").red().dim());
                eprintln!("  {}   --auth-submit-selector   \"button[type='submit']\"", style("│").red().dim());
            }
        }
    }

    // ── BFS queue ────────────────────────────────────────────────────────────
    let tracker = StateTracker::new();
    let (tx, rx) = async_channel::bounded::<PageState>(2000);
    let first = PageState::new(&config.start_url);
    tracker.visit(&first.fingerprint());
    tx.send(first).await.ok();
    let pending = Arc::new(AtomicUsize::new(1));

    let all_issues: Arc<Mutex<Vec<Issue>>> = Arc::new(Mutex::new(Vec::new()));
    let all_perf: Arc<Mutex<Vec<PerfMetrics>>> = Arc::new(Mutex::new(Vec::new()));
    let all_network: Arc<Mutex<Vec<NetworkStats>>> = Arc::new(Mutex::new(Vec::new()));
    let all_discovered: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let pages_visited = Arc::new(AtomicUsize::new(0));
    let issue_count = Arc::new(AtomicUsize::new(0));

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
                    Ok(Ok((issues, perf, net, links))) => {
                        let n_issues = issues.len();
                        let mut new_states = vec![];
                        for link in links {
                            // Skip any link whose path matches a skip_paths entry.
                            if !skip_paths.is_empty() {
                                if let Ok(u) = Url::parse(&link) {
                                    let path = u.path();
                                    if skip_paths.iter().any(|s| {
                                        path == s.as_str()
                                            || path.starts_with(&format!("{}/", s))
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
                        if let Ok(mut g) = all_issues.lock() { g.extend(issues); }
                        if let Ok(mut g) = all_perf.lock() { g.push(perf); }
                        if let Ok(mut g) = all_network.lock() { g.push(net); }
                        if let Ok(mut g) = all_discovered.lock() { g.push(state.url.clone()); }
                        let visited = pages_visited.fetch_add(1, Ordering::Relaxed) + 1;
                        issue_count.fetch_add(n_issues, Ordering::Relaxed);
                        pb.set_position(visited as u64);
                        pb.set_length(issue_count.load(Ordering::Relaxed) as u64);
                        for child in new_states {
                            tx.send(child).await.ok();
                        }
                    }
                    Ok(Err(e)) => {
                        pb.println(format!(
                            "  {} {} — {}",
                            style("⚠").yellow(),
                            state.url,
                            e
                        ));
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
    let all_issues = Arc::try_unwrap(all_issues).expect("worker tasks still holding Arc").into_inner().unwrap_or_default();
    let all_perf = Arc::try_unwrap(all_perf).expect("worker tasks still holding Arc").into_inner().unwrap_or_default();
    let all_network = Arc::try_unwrap(all_network).expect("worker tasks still holding Arc").into_inner().unwrap_or_default();
    let all_discovered = Arc::try_unwrap(all_discovered).expect("worker tasks still holding Arc").into_inner().unwrap_or_default();

    println!(
        "  {} Crawled {} page{}  ({} issue{})",
        style("✓").green().bold(),
        pages_visited,
        if pages_visited == 1 { "" } else { "s" },
        all_issues.len(),
        if all_issues.len() == 1 { "" } else { "s" }
    );

    Ok(CrawlResult {
        issues: all_issues,
        perf_metrics: all_perf,
        network_stats: all_network,
        stats: CrawlStats {
            pages_visited,
            elements_interacted: 0,
            states_explored: pages_visited,
            duration_secs: start.elapsed().as_secs_f64(),
        },
        discovered_urls: all_discovered,
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
) -> Result<(Vec<Issue>, PerfMetrics, NetworkStats, Vec<String>)> {
    // Navigate (ignore "soft" errors that still land on the page)
    let goto = tokio::time::timeout(Duration::from_secs(12), client.goto(url)).await;
    match goto {
        Err(_) => anyhow::bail!("goto timed out after 12s"),
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

    wait_for_dom_ready(client, Duration::from_secs(8)).await;

    if settle_ms > 0 {
        tokio::time::sleep(Duration::from_millis(settle_ms)).await;
    }

    // Build and execute the async collection script
    let js = build_collect_js(base_host, discover, link_selector);

    const EMPTY_RESULT: &str = r#"{"issues":[],"links":[],"perf":{},"net":{},"lcp":null,"cls":null}"#;
    let raw = match client.execute_async(&js, vec![]).await {
        Ok(v) => v.as_str().map(|s| s.to_string()).unwrap_or_else(|| EMPTY_RESULT.to_string()),
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
    if let Some(l) = load {
        if l > 3000.0 {
            page_issues.push(Issue {
                severity: Severity::Warning,
                category: IssueCategory::Performance,
                message: format!("Slow page load: {:.0}ms (threshold 3s)", l),
                page_url: url.to_string(),
                element: None,
                action_path: vec![],
            });
        }
    }

    let perf = PerfMetrics {
        page_url: url.to_string(),
        fcp_ms: p["fcp"].as_f64(),
        lcp_ms: v["lcp"].as_f64(),
        tti_ms: p["tti"].as_f64(),
        // CLS comes from the async observer result at the top level
        cls_score: v["cls"].as_f64(),
        // TBT requires a pre-load observer (addScriptToEvaluateOnNewDocument);
        // not available via standard WebDriver without a pre-load hook.
        tbt_ms: None,
        dom_content_loaded_ms: p["dcl"].as_f64(),
        load_ms: load,
    };

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
        total_transfer_kb: n["total_transfer_kb"].as_f64().unwrap_or(0.0),
        slowest_resource_ms: n["slowest_ms"].as_f64(),
        slowest_resource_url: n["slowest_url"].as_str().map(|s| s.to_string()),
    };

    Ok((page_issues, perf, net, links))
}

/// Build the async JS collection script for a given page.
/// The script:
///   1. Runs all static audits (accessibility, SEO) synchronously.
///   2. Discovers links synchronously.
///   3. Reads Navigation Timing, paint entries, and resource stats synchronously.
///   4. Sets up PerformanceObservers with `buffered:true` for LCP and CLS.
///   5. Calls the WebDriver async callback once both observers have fired
///      (or after a 500 ms safety timeout, whichever comes first).
fn build_collect_js(base_host: &str, discover: bool, link_selector: Option<&str>) -> String {
    // Build the JS expression that produces the NodeList of <a> elements to scan.
    // If a link_selector is given, only follow links inside the first matching element.
    let anchor_query = match link_selector {
        Some(sel) => format!(
            "(document.querySelector({sel_json}) || document).querySelectorAll('a[href]')",
            sel_json = serde_json::to_string(sel).unwrap_or_else(|_| "\"a\"".into()),
        ),
        None => "document.querySelectorAll('a[href]')".to_string(),
    };
    format!(
        r#"
(function() {{
    var cb = arguments[arguments.length - 1];
    var _called = false;
    var done = function(data) {{
        if (!_called) {{ _called = true; cb(JSON.stringify(data)); }}
    }};

    try {{
        // ── Accessibility & SEO issues ────────────────────────────────────────
        var issues = [];

        document.querySelectorAll('img:not([alt])').forEach(function(el) {{
            issues.push({{ sev:'warning', cat:'accessibility',
                msg:'Image missing alt attribute', el:el.outerHTML.slice(0,120) }});
        }});

        document.querySelectorAll(
            'input:not([type="hidden"]):not([aria-label]):not([aria-labelledby])'
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

        var titleEl = document.querySelector('title');
        if (!titleEl || !titleEl.textContent.trim()) {{
            issues.push({{ sev:'warning', cat:'seo', msg:'Page is missing a <title> element' }});
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

        // ── Link discovery ────────────────────────────────────────────────────
        var links = [];
        if ({discover}) {{
            var seen = new Set();
            {anchor_query}.forEach(function(el) {{
                try {{
                    var u = new URL(el.href, location.href);
                    if (u.host !== '{base_host}') return;
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

        var resources = performance.getEntriesByType('resource');
        var totalBytes = 0, failedCount = 0, slowestMs = 0, slowestUrl = null;
        for (var i = 0; i < resources.length; i++) {{
            var r = resources[i];
            if (r.transferSize) totalBytes += r.transferSize;
            if (r.duration < 1 && r.transferSize === 0 && r.decodedBodySize === 0) failedCount++;
            if (r.duration > slowestMs) {{ slowestMs = r.duration; slowestUrl = r.name; }}
        }}

        var collected = {{
            issues: issues,
            links:  links,
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
                total_transfer_kb:     totalBytes / 1024,
                slowest_ms:  slowestMs  > 0    ? slowestMs  : null,
                slowest_url: slowestUrl !== null ? slowestUrl : null
            }},
            lcp: null,
            cls: null
        }};

        // ── LCP + CLS via buffered PerformanceObservers ───────────────────────
        // The buffered flag causes already-recorded entries to be delivered
        // synchronously into the callback queue, so this resolves very quickly.
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

        // Safety net: deliver whatever we have after 500 ms
        setTimeout(function() {{ lcpDone = true; clsDone = true; checkDone(); }}, 500);

    }} catch(err) {{
        // Never leave the WebDriver async callback hanging
        done({{ issues:[], links:[], perf:{{}}, net:{{}}, lcp:null, cls:null, _error: err.toString() }});
    }}
}})()
"#,
        discover = if discover { "true" } else { "false" },
        base_host = base_host,
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
        let mut cookie =
            fantoccini::cookies::Cookie::new(e.name.clone(), e.value.clone());
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
) -> Result<()> {
    let login_url = match &auth.login_url {
        Some(u) if u.starts_with("http://") || u.starts_with("https://") => u.clone(),
        Some(path) => format!(
            "{}/{}",
            base_url.trim_end_matches('/'),
            path.trim_start_matches('/')
        ),
        None => anyhow::bail!("No login URL provided"),
    };

    tokio::time::timeout(Duration::from_secs(12), client.goto(&login_url))
        .await
        .context("goto login page timed out")??;
    wait_for_dom_ready(client, Duration::from_secs(8)).await;

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
                "input[name='email']",
                "input[name='username']",
                "input[name='login']",
                "input[name='user']",
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
    if (best.id) return '#' + best.id;
    if (best.name) return 'input[name="' + best.name + '"]';
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
            .ok_or_else(|| anyhow::anyhow!("Could not find a username/email field on the login page"))?
        }
    };
    user_el.click().await.ok();
    user_el.clear().await.ok();
    user_el
        .send_keys(username)
        .await
        .context("Failed to type into username field")?;

    // ── Multi-step login: if password field isn't visible yet, click "Next" ──
    let password_immediately_visible = client.find(Locator::Css("input[type='password']")).await.is_ok();
    if !password_immediately_visible {
        if let Some(next_btn) = find_first(client, &[
            "button[type='submit']",
            "input[type='submit']",
        ]).await {
            next_btn.click().await.ok();
            let step2_deadline = tokio::time::Instant::now() + Duration::from_secs(5);
            loop {
                if client.find(Locator::Css("input[type='password']")).await.is_ok() {
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

    // ── Submit / sign-in button ───────────────────────────────────────────────
    let submit_el = if let Some(sel) = auth.submit_selector.as_deref() {
        client
            .find(Locator::Css(sel))
            .await
            .map_err(|_| anyhow::anyhow!("Submit selector '{}' matched nothing", sel))?
    } else {
        // 1. Explicit type=submit or type=image (image submit buttons)
        if let Some(el) = find_first(client, &[
            "button[type='submit']",
            "input[type='submit']",
            "input[type='image']",
        ]).await {
            el
        } else {
            // 2. Button or role=button whose visible text or aria-label contains login keyword
            let xp = "//button[contains(translate(normalize-space(.), \
                'ABCDEFGHIJKLMNOPQRSTUVWXYZ', 'abcdefghijklmnopqrstuvwxyz'), 'sign in')] | \
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
            if let Ok(el) = client.find(Locator::XPath(xp)).await {
                el
            } else {
                // 3. Last button inside a <form>, then last button on the page
                let all = client
                    .find_all(Locator::Css("form button, form [role='button'], button, [role='button']"))
                    .await
                    .unwrap_or_default();
                all.into_iter()
                    .last()
                    .ok_or_else(|| anyhow::anyhow!("Could not find any button on login page"))?
            }
        }
    };

    submit_el.click().await.context("Failed to click sign-in button")?;

    tokio::time::sleep(Duration::from_millis(settle_ms.max(1500))).await;
    wait_for_dom_ready(client, Duration::from_secs(8)).await;

    // ── Login verification ────────────────────────────────────────────────────
    if let Ok(current_url) = client.current_url().await {
        let current = current_url.as_str();
        if current.starts_with(login_url.as_str()) || current.contains("/login") || current.contains("/signin") {
            eprintln!(
                "  {} Login may have failed — still on login page: {}",
                console::style("⚠").yellow().bold(),
                current
            );
            eprintln!(
                "  {} Check credentials or use --auth-username-selector / --auth-password-selector",
                console::style("│").yellow().dim()
            );
        }
    }

    Ok(())
}
