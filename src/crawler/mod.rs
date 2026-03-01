pub mod browser;
pub mod collectors;
pub mod element;
pub mod state;

use crate::types::{AuthConfig, CookieEntry, CrawlStats, Issue, IssueCategory, NetworkStats, PageState, PerfMetrics, Severity};
use anyhow::{Context, Result};
use async_channel;
use browser::HeadlessBrowser;
use collectors::JsCollector;
use console::style;
use indicatif::{ProgressBar, ProgressStyle};
use state::StateTracker;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use url::Url;

/// Timeout for a single page audit (goto + dom ready + JS eval).
const PAGE_TIMEOUT: Duration = Duration::from_secs(20);

pub struct CrawlerConfig {
    pub start_url: String,
    pub max_depth: usize,
    /// Number of concurrent browser tabs to use for crawling.
    pub concurrency: usize,
    pub headless: bool,
    pub settle_ms: u64,
    pub auth: AuthConfig,
}

pub struct CrawlResult {
    pub issues: Vec<Issue>,
    pub perf_metrics: Vec<PerfMetrics>,
    pub network_stats: Vec<NetworkStats>,
    pub stats: CrawlStats,
    /// All successfully crawled page URLs (for multi-URL load testing).
    pub discovered_urls: Vec<String>,
}

pub async fn run_crawler(config: CrawlerConfig) -> Result<CrawlResult> {
    let start = Instant::now();
    let concurrency = config.concurrency.max(1);
    let base_url = Url::parse(&config.start_url)?;
    let base_host = base_url.host_str().unwrap_or("").to_string();

    // ── HTTP pre-check (fast-fail before Chrome launch) ─────────────────────
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

    // ── Launch browser ───────────────────────────────────────────────────────
    print!("  {} Launching browser… ", style("→").cyan());
    let launch_start = Instant::now();
    let (browser, driver) = HeadlessBrowser::launch(config.headless)
        .await
        .context("Failed to launch Chrome — is Google Chrome installed in /Applications?")?;
    println!(
        "{}  ({:.1}s)",
        style("ready").green().bold(),
        launch_start.elapsed().as_secs_f64()
    );
    tokio::spawn(driver);

    // ── Open `concurrency` tabs ──────────────────────────────────────────────
    if concurrency > 1 {
        println!("  {} Opening {} tabs…", style("→").cyan(), concurrency);
    }
    let mut pages = Vec::with_capacity(concurrency);
    for _ in 0..concurrency {
        let p = browser.new_page().await?;
        JsCollector::inject_on_new_document(&p).await?;
        block_heavy_resources(&p).await;
        pages.push(p);
    }

    // ── Auth: inject cookies and/or perform login (first tab only) ───────────
    if config.auth.cookies_file.is_some() {
        if let Err(e) = inject_cookies(&pages[0], &config.auth).await {
            eprintln!("  {} Cookie injection failed: {}", style("⚠").yellow(), e);
        } else {
            println!("  {} Cookies injected", style("✓").green());
        }
    }

    if config.auth.login_url.is_some() && config.auth.username.is_some() {
        print!("  {} Authenticating… ", style("→").cyan());
        match perform_login(&pages[0], &config.auth, &config.start_url, config.settle_ms).await {
            Ok(()) => println!("{}", style("ok").green().bold()),
            Err(e) => println!("{} ({})", style("failed").red().bold(), e),
        }
    }

    // ── BFS with pending-counter completion detection ────────────────────────
    //
    // `pending` counts items in flight (in queue + actively being processed).
    // Starting at 1 for the seed URL.  Each new link discovered increments
    // pending before being sent to the channel; each completed state decrements
    // pending.  When pending hits 0 the channel is closed and all workers exit.
    let tracker = StateTracker::new();
    let (tx, rx) = async_channel::bounded::<PageState>(2000);
    let first = PageState::new(&config.start_url);
    tracker.visit(&first.fingerprint());
    tx.send(first).await.ok();
    let pending = Arc::new(AtomicUsize::new(1));

    // Shared result accumulators
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
        .unwrap(),
    );
    pb.enable_steady_tick(Duration::from_millis(80));
    pb.set_message(config.start_url.clone());

    // ── Spawn one worker task per tab ────────────────────────────────────────
    let mut handles = vec![];
    for page in pages {
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

        handles.push(tokio::spawn(async move {
            while let Ok(state) = rx.recv().await {
                pb.set_message(state.url.clone());

                let result = tokio::time::timeout(
                    PAGE_TIMEOUT,
                    audit_and_discover(
                        &page,
                        &state.url,
                        &base_host,
                        settle_ms,
                        state.depth < max_depth,
                    ),
                )
                .await;

                match result {
                    Ok(Ok((issues, perf, net, links))) => {
                        let n_issues = issues.len();

                        // Increment pending for each new child BEFORE sending to
                        // the channel, so the counter never hits 0 prematurely.
                        let mut new_states = vec![];
                        for link in links {
                            let child = state.child(&link, "link");
                            let fp = child.fingerprint();
                            if tracker.visit(&fp) {
                                pending.fetch_add(1, Ordering::SeqCst);
                                new_states.push(child);
                            }
                        }

                        {
                            let mut g = all_issues.lock().unwrap();
                            g.extend(issues);
                        }
                        all_perf.lock().unwrap().push(perf);
                        all_network.lock().unwrap().push(net);
                        all_discovered.lock().unwrap().push(state.url.clone());

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

                // Decrement pending; close channel when all work is exhausted.
                if pending.fetch_sub(1, Ordering::SeqCst) == 1 {
                    tx.close();
                }
            }
        }));
    }

    for h in handles {
        h.await.ok();
    }
    pb.finish_and_clear();

    let pages_visited = pages_visited.load(Ordering::Relaxed);
    let all_issues = Arc::try_unwrap(all_issues).unwrap().into_inner().unwrap();
    let all_perf = Arc::try_unwrap(all_perf).unwrap().into_inner().unwrap();
    let all_network = Arc::try_unwrap(all_network).unwrap().into_inner().unwrap();
    let all_discovered = Arc::try_unwrap(all_discovered).unwrap().into_inner().unwrap();

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

/// Navigate to a URL, collect issues + perf, and optionally discover links.
/// Returns (issues, perf, links).
async fn audit_and_discover(
    page: &chromiumoxide::Page,
    url: &str,
    base_host: &str,
    settle_ms: u64,
    discover: bool,
) -> Result<(Vec<Issue>, PerfMetrics, NetworkStats, Vec<String>)> {
    // Navigate — ignore non-fatal goto errors (SPA might "fail" on hash routes)
    let goto_result = tokio::time::timeout(Duration::from_secs(12), page.goto(url)).await;
    match goto_result {
        Err(_) => anyhow::bail!("goto timed out after 12s"),
        Ok(Err(e)) => {
            // Log but don't abort: chromiumoxide sometimes returns an error even
            // when the page did load (e.g., net::ERR_ABORTED on redirected resources).
            // We continue and let the readyState poll tell us if it actually loaded.
            let msg = e.to_string();
            if msg.contains("net::ERR_CONNECTION_REFUSED")
                || msg.contains("net::ERR_NAME_NOT_RESOLVED")
            {
                anyhow::bail!("{}", msg);
            }
        }
        Ok(Ok(_)) => {}
    }

    wait_for_dom_ready(page, Duration::from_secs(8)).await;

    if settle_ms > 0 {
        tokio::time::sleep(Duration::from_millis(settle_ms)).await;
    }

    // Collect issues, perf, and links in a single JS round-trip
    let base_host_js = base_host.to_string();
    let js = format!(
        r#"
    (() => {{
        const issues = [];

        // Console errors / warnings captured by injected collector
        for (const e of (window.__wp_errors || [])) {{
            issues.push({{
                sev: (e.type === 'rejection' || e.type === 'uncaught') ? 'error' : e.type,
                cat: 'console_error',
                msg: e.msg
            }});
        }}

        // Accessibility: images without alt
        document.querySelectorAll('img:not([alt])').forEach(el => {{
            issues.push({{ sev: 'warning', cat: 'accessibility',
                msg: 'Image missing alt attribute',
                el: el.outerHTML.slice(0, 120) }});
        }});

        // Accessibility: inputs without label
        document.querySelectorAll(
            'input:not([type="hidden"]):not([aria-label]):not([aria-labelledby])'
        ).forEach(el => {{
            if (!el.id || !document.querySelector('label[for="' + el.id + '"]')) {{
                issues.push({{ sev: 'warning', cat: 'accessibility',
                    msg: 'Input missing label / aria-label',
                    el: el.outerHTML.slice(0, 120) }});
            }}
        }});

        // Accessibility: buttons with no accessible text
        document.querySelectorAll('button:not([aria-label]):not([aria-labelledby])').forEach(el => {{
            if (!el.textContent.trim()) {{
                issues.push({{ sev: 'warning', cat: 'accessibility',
                    msg: 'Button has no accessible text',
                    el: el.outerHTML.slice(0, 120) }});
            }}
        }});

        // Accessibility: links with no text or aria-label
        document.querySelectorAll('a:not([aria-label]):not([aria-labelledby])').forEach(el => {{
            if (!el.textContent.trim() && !el.querySelector('img[alt]')) {{
                issues.push({{ sev: 'warning', cat: 'accessibility',
                    msg: 'Link has no accessible text',
                    el: el.outerHTML.slice(0, 120) }});
            }}
        }});

        // SEO: missing or empty <title>
        const title = document.querySelector('title');
        if (!title || !title.textContent.trim()) {{
            issues.push({{ sev: 'warning', cat: 'seo',
                msg: 'Page is missing a <title> element' }});
        }}

        // SEO/Accessibility: <html> missing lang attribute
        if (!document.documentElement.getAttribute('lang')) {{
            issues.push({{ sev: 'warning', cat: 'accessibility',
                msg: '<html> element is missing a lang attribute' }});
        }}

        // SEO: missing viewport meta (important for mobile)
        if (!document.querySelector('meta[name="viewport"]')) {{
            issues.push({{ sev: 'info', cat: 'seo',
                msg: 'Missing <meta name="viewport"> — page may render poorly on mobile' }});
        }}

        // SEO: missing meta description
        if (!document.querySelector('meta[name="description"]')) {{
            issues.push({{ sev: 'info', cat: 'seo',
                msg: 'Missing <meta name="description">' }});
        }}

        // Link discovery (SPA-aware — uses rendered DOM hrefs)
        const links = [];
        if ({discover}) {{
            const seen = new Set();
            document.querySelectorAll('a[href]').forEach(el => {{
                try {{
                    const u = new URL(el.href, location.href);
                    if (u.host !== '{base_host}') return;
                    u.hash = '';
                    u.search = '';
                    const key = u.toString();
                    if (!seen.has(key)) {{
                        seen.add(key);
                        links.push(key);
                    }}
                }} catch (_) {{}}
            }});
        }}

        // Performance timing
        const nav = performance.getEntriesByType('navigation')[0] || {{}};
        const paint = {{}};
        performance.getEntriesByType('paint').forEach(e => {{ paint[e.name] = e.startTime; }});
        // LCP/CLS/TBT collected by the injected PerformanceObserver in COLLECTOR_JS
        const lcp = window.__wp_lcp || null;
        const cls = (typeof window.__wp_cls === 'number' && window.__wp_cls > 0) ? window.__wp_cls : null;
        const tbt = (typeof window.__wp_tbt === 'number' && window.__wp_tbt > 0) ? window.__wp_tbt : null;
        const tti = nav.domInteractive > 0 ? nav.domInteractive : null;

        // Network statistics from Navigation Timing API
        const dns = (nav.domainLookupEnd != null && nav.domainLookupStart != null)
            ? nav.domainLookupEnd - nav.domainLookupStart : null;
        const tcp = (nav.connectEnd != null && nav.connectStart != null)
            ? nav.connectEnd - nav.connectStart : null;
        const tls = (nav.secureConnectionStart > 0 && nav.connectEnd != null)
            ? nav.connectEnd - nav.secureConnectionStart : null;
        const ttfb = (nav.responseStart != null && nav.requestStart != null)
            ? nav.responseStart - nav.requestStart : null;
        const download = (nav.responseEnd != null && nav.responseStart != null)
            ? nav.responseEnd - nav.responseStart : null;

        // Sub-resource stats
        const resources = performance.getEntriesByType('resource');
        let totalBytes = 0;
        let failedCount = 0;
        let slowestMs = 0;
        let slowestUrl = null;
        for (const r of resources) {{
            if (r.transferSize) totalBytes += r.transferSize;
            if (r.duration < 1 && r.transferSize === 0 && r.decodedBodySize === 0) failedCount++;
            if (r.duration > slowestMs) {{ slowestMs = r.duration; slowestUrl = r.name; }}
        }}

        return JSON.stringify({{
            issues,
            links,
            perf: {{
                fcp: paint['first-contentful-paint'] || null,
                lcp: lcp,
                cls: cls,
                tbt: tbt,
                tti: tti,
                dcl: nav.domContentLoadedEventEnd || null,
                load: nav.loadEventEnd || null
            }},
            net: {{
                dns,
                tcp,
                tls,
                ttfb,
                download,
                resource_count: resources.length,
                failed_resource_count: failedCount,
                total_transfer_kb: totalBytes / 1024,
                slowest_ms: slowestMs > 0 ? slowestMs : null,
                slowest_url: slowestUrl
            }}
        }});
    }})()
    "#,
        discover = if discover { "true" } else { "false" },
        base_host = base_host_js,
    );

    let raw = page
        .evaluate(js.as_str())
        .await
        .ok()
        .and_then(|r| r.value().and_then(|v| v.as_str().map(|s| s.to_string())))
        .unwrap_or_else(|| r#"{"issues":[],"links":[],"perf":{},"net":{}}"#.to_string());

    let v: serde_json::Value = serde_json::from_str(&raw).unwrap_or_default();

    // Parse issues
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

    // Parse links
    let links: Vec<String> = v["links"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();

    // Parse perf + flag slow pages
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
        lcp_ms: p["lcp"].as_f64(),
        tti_ms: p["tti"].as_f64(),
        cls_score: p["cls"].as_f64(),
        tbt_ms: p["tbt"].as_f64(),
        dom_content_loaded_ms: p["dcl"].as_f64(),
        load_ms: load,
    };

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

/// Block images, fonts, and media via CDP — reduces Chrome rendering overhead.
async fn block_heavy_resources(page: &chromiumoxide::Page) {
    use chromiumoxide::cdp::browser_protocol::network::{EnableParams, SetBlockedUrLsParams};
    page.execute(EnableParams::default()).await.ok();
    page.execute(SetBlockedUrLsParams::new(vec![
        "*.png".into(),
        "*.jpg".into(),
        "*.jpeg".into(),
        "*.gif".into(),
        "*.webp".into(),
        "*.avif".into(),
        "*.svg".into(),
        "*.ico".into(),
        "*.woff".into(),
        "*.woff2".into(),
        "*.ttf".into(),
        "*.otf".into(),
        "*.mp4".into(),
        "*.webm".into(),
        "*.mp3".into(),
    ]))
    .await
    .ok();
}

/// Poll `document.readyState` until interactive/complete or timeout.
async fn wait_for_dom_ready(page: &chromiumoxide::Page, timeout: Duration) {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if tokio::time::Instant::now() >= deadline {
            break;
        }
        let state = page
            .evaluate("document.readyState")
            .await
            .ok()
            .and_then(|r| r.value().and_then(|v| v.as_str().map(|s| s.to_string())));

        match state.as_deref() {
            Some("complete") | Some("interactive") => break,
            _ => tokio::time::sleep(Duration::from_millis(80)).await,
        }
    }
}

/// Inject cookies from a JSON file into the browser before crawling.
/// Cookie file format: JSON array of `{ name, value, domain?, path?, secure?, httpOnly? }`.
async fn inject_cookies(page: &chromiumoxide::Page, auth: &AuthConfig) -> Result<()> {
    use chromiumoxide::cdp::browser_protocol::network::{CookieParam, EnableParams, SetCookiesParams};

    let path = auth.cookies_file.as_ref().context("no cookie file")?;
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("Cannot read cookie file: {}", path.display()))?;
    let entries: Vec<CookieEntry> = serde_json::from_str(&raw)
        .context("Cookie file must be a JSON array of { name, value, domain?, path?, ... }")?;

    page.execute(EnableParams::default()).await.ok();

    let cookies: Vec<CookieParam> = entries
        .into_iter()
        .map(|e| {
            let mut b = CookieParam::builder().name(e.name).value(e.value);
            if let Some(d) = e.domain { b = b.domain(d); }
            if let Some(path_val) = e.path { b = b.path(path_val); }
            if let Some(s) = e.secure { b = b.secure(s); }
            if let Some(h) = e.http_only { b = b.http_only(h); }
            b.build().unwrap()
        })
        .collect();

    page.execute(SetCookiesParams::new(cookies)).await?;
    Ok(())
}

/// Navigate to the login page, fill credentials, submit the form,
/// and wait for the resulting navigation (auth redirect).
async fn perform_login(
    page: &chromiumoxide::Page,
    auth: &AuthConfig,
    base_url: &str,
    settle_ms: u64,
) -> Result<()> {
    let login_url = match &auth.login_url {
        Some(u) if u.starts_with("http://") || u.starts_with("https://") => u.clone(),
        Some(path) => format!("{}/{}", base_url.trim_end_matches('/'), path.trim_start_matches('/')),
        None => anyhow::bail!("No login URL provided"),
    };

    tokio::time::timeout(Duration::from_secs(12), page.goto(&login_url))
        .await
        .context("goto login page timed out")??;
    wait_for_dom_ready(page, Duration::from_secs(8)).await;

    let username = auth.username.as_deref().unwrap_or("");
    let password = auth.password.as_deref().unwrap_or("");

    // CSS selectors with sensible defaults — try user-provided selector first
    let user_sel = auth
        .username_selector
        .as_deref()
        .unwrap_or("input[type='email'],input[name='username'],input[name='email'],input[id*='user' i],input[id*='email' i],input[placeholder*='email' i],input[placeholder*='username' i]");
    let pass_sel = auth
        .password_selector
        .as_deref()
        .unwrap_or("input[type='password']");
    let submit_sel = auth
        .submit_selector
        .as_deref()
        .unwrap_or("button[type='submit'],input[type='submit'],button:last-of-type");

    // Fill form using React-compatible native value setter trick
    let fill_js = format!(
        r#"
    (() => {{
        function fill(sel, val) {{
            const el = document.querySelector(sel);
            if (!el) return false;
            const nativeSetter = Object.getOwnPropertyDescriptor(window.HTMLInputElement.prototype, 'value').set;
            nativeSetter.call(el, val);
            el.dispatchEvent(new Event('input', {{ bubbles: true }}));
            el.dispatchEvent(new Event('change', {{ bubbles: true }}));
            return true;
        }}
        const userOk = fill({user_sel:?}, {username:?});
        const passOk = fill({pass_sel:?}, {password:?});
        return JSON.stringify({{ userOk, passOk }});
    }})()
    "#,
        user_sel = user_sel,
        username = username,
        pass_sel = pass_sel,
        password = password,
    );

    let fill_result = page
        .evaluate(fill_js.as_str())
        .await
        .ok()
        .and_then(|r| r.value().and_then(|v| v.as_str().map(|s| s.to_string())))
        .unwrap_or_default();

    let fill_v: serde_json::Value = serde_json::from_str(&fill_result).unwrap_or_default();
    if fill_v["userOk"].as_bool() != Some(true) {
        anyhow::bail!("Could not find username field (tried: {})", user_sel);
    }
    if fill_v["passOk"].as_bool() != Some(true) {
        anyhow::bail!("Could not find password field (tried: {})", pass_sel);
    }

    // Submit and wait for navigation
    let submit_js = format!(
        r#"
    (() => {{
        const btn = document.querySelector({submit_sel:?});
        if (!btn) return false;
        btn.click();
        return true;
    }})()
    "#,
        submit_sel = submit_sel,
    );

    page.evaluate(submit_js.as_str()).await.ok();

    // Wait for the post-login navigation to settle
    tokio::time::sleep(Duration::from_millis(settle_ms.max(800))).await;
    wait_for_dom_ready(page, Duration::from_secs(8)).await;

    Ok(())
}
