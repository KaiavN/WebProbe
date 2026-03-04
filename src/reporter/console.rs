use crate::types::{Report, Severity};
use console::style;

pub fn print_report(report: &Report) {
    println!();
    println!(
        "{}",
        style("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━").cyan()
    );
    println!(
        "  {} {}",
        style("webprobe").bold().cyan(),
        style(format!("v{}", report.version)).dim()
    );
    println!("  Target : {}", style(&report.target_url).underlined());
    println!(
        "  Crawled: {} page{}  |  {} interactive element{}  |  {:.1}s",
        report.crawl_stats.pages_visited,
        if report.crawl_stats.pages_visited == 1 {
            ""
        } else {
            "s"
        },
        report.crawl_stats.elements_interacted,
        if report.crawl_stats.elements_interacted == 1 {
            ""
        } else {
            "s"
        },
        report.crawl_stats.duration_secs
    );
    println!(
        "{}",
        style("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━").cyan()
    );

    // ── Issues grouped by page ─────────────────────────────────────────────
    if report.issues.is_empty() {
        println!("\n  {} No issues found!\n", style("✓").green().bold());
    } else {
        // Deduplicate: issues appearing on many pages are shown once as "global" issues
        // to avoid flooding the output (e.g. "missing lang attr" on every page).
        let dedup_threshold = 3usize;
        let (mut global_issues, local_issues): (Vec<_>, Vec<_>) = report.issues.iter().partition(|i| i.page_urls.len() >= dedup_threshold);

        // ── Global / site-wide issues ──────────────────────────────────────
        if !global_issues.is_empty() {
            println!(
                "\n{}",
                style("  ── Site-wide Issues ──────────────────────────────────────").dim()
            );
            global_issues.sort_by(|a, b| b.severity.cmp(&a.severity));
            for issue in global_issues {
                let n_pages = issue.page_urls.len();
                let (icon, sev_str) = severity_style(&issue.severity);
                let cat = style(format!("[{}]", issue.category)).dim();
                println!(
                    "    {} {} {} {}  {}",
                    icon,
                    sev_str,
                    cat,
                    issue.message,
                    style(format!("(on {} pages)", n_pages)).dim()
                );
            }
        }

        // ── Per-page issues (excluding global ones) ────────────────────────
        let mut by_page: std::collections::BTreeMap<&str, Vec<_>> =
            std::collections::BTreeMap::new();
        for issue in &local_issues {
            for url in &issue.page_urls {
                by_page
                    .entry(url.as_str())
                    .or_default()
                    .push(*issue);
            }
        }

        for (page, issues) in &by_page {
            println!("\n  {}", style(*page).underlined().bold());
            let mut sorted = issues.clone();
            sorted.sort_by(|a, b| b.severity.cmp(&a.severity));
            for issue in sorted {
                let (icon, sev_str) = severity_style(&issue.severity);
                let cat = style(format!("[{}]", issue.category)).dim();
                println!("    {} {} {} {}", icon, sev_str, cat, issue.message);
                if let Some(el) = &issue.element {
                    println!("       {}", style(format!("↳ {}", el)).dim());
                }
            }
        }
    }

    // ── Per-page interactive elements ──────────────────────────────────────
    let pages_with_interactions: Vec<_> = report.pages.iter().filter_map(|p| p.interactions.as_ref().map(|i| (p.url.as_str(), i))).collect();
    if !pages_with_interactions.is_empty() {
        println!(
            "\n{}",
            style("  ── Interactive Elements ─────────────────────────────────").dim()
        );
        for (url, pi) in pages_with_interactions {
            println!(
                "  {}  →  {} element{}",
                style(url).dim(),
                pi.elements_found,
                if pi.elements_found == 1 { "" } else { "s" }
            );
            let mut links = 0usize;
            let mut buttons = 0usize;
            let mut inputs = 0usize;
            let mut selects = 0usize;
            let mut textareas = 0usize;
            for el in &pi.elements {
                match el.kind.as_str() {
                    "link" => links += 1,
                    "button" => buttons += 1,
                    "input" => inputs += 1,
                    "select" => selects += 1,
                    "textarea" => textareas += 1,
                    _ => {}
                }
            }
            let mut parts = Vec::new();
            if links > 0 {
                parts.push(format!(
                    "{} link{}",
                    links,
                    if links == 1 { "" } else { "s" }
                ));
            }
            if buttons > 0 {
                parts.push(format!(
                    "{} button{}",
                    buttons,
                    if buttons == 1 { "" } else { "s" }
                ));
            }
            if inputs > 0 {
                parts.push(format!(
                    "{} input{}",
                    inputs,
                    if inputs == 1 { "" } else { "s" }
                ));
            }
            if selects > 0 {
                parts.push(format!(
                    "{} select{}",
                    selects,
                    if selects == 1 { "" } else { "s" }
                ));
            }
            if textareas > 0 {
                parts.push(format!(
                    "{} textarea{}",
                    textareas,
                    if textareas == 1 { "" } else { "s" }
                ));
            }
            if !parts.is_empty() {
                println!("      {}", style(parts.join("  ·  ")).dim());
            }
        }
    }

    // ── Performance ────────────────────────────────────────────────────────
    let pages_with_perf: Vec<_> = report.pages.iter().filter_map(|p| p.perf_metrics.as_ref().map(|i| (p.url.as_str(), i))).collect();
    if !pages_with_perf.is_empty() {
        println!(
            "\n{}",
            style("  ── Performance ──────────────────────────────────────────").dim()
        );
        for (url, p) in pages_with_perf {
            let has_data = p.fcp_ms.is_some()
                || p.lcp_ms.is_some()
                || p.load_ms.is_some()
                || p.dom_content_loaded_ms.is_some()
                || p.cls_score.is_some();
            if !has_data {
                continue;
            }
            println!("  {}", style(url).dim());
            if let Some(fcp) = p.fcp_ms {
                let s = perf_color(fcp, 1800.0, 3000.0);
                println!("    FCP:  {}ms", s(format!("{:.0}", fcp)));
            }
            if let Some(lcp) = p.lcp_ms {
                let s = perf_color(lcp, 2500.0, 4000.0);
                println!("    LCP:  {}ms", s(format!("{:.0}", lcp)));
            }
            if let Some(tti) = p.tti_ms {
                let s = perf_color(tti, 3800.0, 7300.0);
                println!("    TTI:  {}ms", s(format!("{:.0}", tti)));
            }
            if let Some(cls) = p.cls_score {
                let s = perf_color(cls, 0.1, 0.25);
                println!("    CLS:  {}", s(format!("{:.3}", cls)));
            }
            if let Some(load) = p.load_ms {
                let s = perf_color(load, 2000.0, 4000.0);
                println!("    Load: {}ms", s(format!("{:.0}", load)));
            }
            if let Some(dcl) = p.dom_content_loaded_ms {
                println!("    DCL:  {:.0}ms", dcl);
            }
        }
    }

    // ── Network Stats ──────────────────────────────────────────────────────
    let pages_with_net: Vec<_> = report.pages.iter().filter_map(|p| p.network_stats.as_ref().map(|i| (p.url.as_str(), i))).collect();
    if !pages_with_net.is_empty() {
        println!(
            "\n{}",
            style("  ── Network Stats ─────────────────────────────────────────").dim()
        );
        for (url, n) in pages_with_net {
            println!("  {}", style(url).dim());
            if let Some(dns) = n.dns_ms {
                if dns > 0.1 { println!("    DNS:      {:.1}ms", dns); }
            }
            if let Some(tcp) = n.tcp_connect_ms {
                if tcp > 0.1 { println!("    TCP:      {:.1}ms", tcp); }
            }
            if let Some(tls) = n.tls_ms {
                if tls > 0.1 { println!("    TLS:      {:.1}ms", tls); }
            }
            if let Some(ttfb) = n.ttfb_ms {
                if ttfb > 0.1 {
                    let s = perf_color(ttfb, 200.0, 600.0);
                    println!("    TTFB:     {}ms", s(format!("{:.1}", ttfb)));
                }
            }
            if let Some(dl) = n.download_ms {
                if dl > 0.1 { println!("    Download: {:.1}ms", dl); }
            }
            println!(
                "    Resources: {}  failed: {}  transferred: {:.1}KB",
                n.resource_count, n.failed_resource_count, n.total_transfer_kb
            );
            if let (Some(ms), Some(url)) = (n.slowest_resource_ms, &n.slowest_resource_url) {
                let short_url_owned;
                let short_url = if url.len() > 60 {
                    let start = url
                        .char_indices()
                        .rev()
                        .nth(59)
                        .map(|(i, _)| i)
                        .unwrap_or(0);
                    short_url_owned = &url[start..];
                    short_url_owned
                } else {
                    url.as_str()
                };
                let ms_str = if ms > 500.0 { style(format!("{:.0}ms", ms)).red() } else if ms > 200.0 { style(format!("{:.0}ms", ms)).yellow() } else { style(format!("{:.0}ms", ms)).dim() };
                println!("    Slowest:   {}  …{}", ms_str, style(short_url).dim().italic());
            }
        }
    }

    // ── Load Test ──────────────────────────────────────────────────────────
    if let Some(lt) = &report.load_test {
        println!(
            "\n{}",
            style("  ── Load Test ─────────────────────────────────────────────").dim()
        );
        println!(
            "  {} users  ×  {}s  →  {:.1} req/s",
            lt.users, lt.duration_secs, lt.throughput_rps
        );
        println!(
            "  Requests: {} total  {} ok  {} failed  ({:.1}% error rate)",
            lt.total_requests,
            style(lt.successful_requests).green(),
            style(lt.failed_requests).red(),
            lt.error_rate_pct
        );
        println!(
            "  Latency  mean:{:.0}ms  p50:{:.0}ms  p90:{:.0}ms  p95:{:.0}ms  p99:{:.0}ms  max:{:.0}ms",
            lt.latency_mean_ms,
            lt.latency_p50_ms,
            lt.latency_p90_ms,
            lt.latency_p95_ms,
            lt.latency_p99_ms,
            lt.latency_max_ms
        );
    }

    // ── Discovered URLs ────────────────────────────────────────────────────
    if !report.discovered_urls.is_empty() {
        println!(
            "\n{}",
            style("  ── Discovered URLs ───────────────────────────────────────").dim()
        );
        for url in &report.discovered_urls {
            println!("    {}", style(url).dim());
        }
    }

    // ── Summary ────────────────────────────────────────────────────────────
    println!(
        "\n{}",
        style("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━").cyan()
    );
    println!(
        "  Summary  {} critical  {} errors  {} warnings  {} info",
        style(report.summary.critical).red().bold(),
        style(report.summary.errors).red(),
        style(report.summary.warnings).yellow(),
        style(report.summary.infos).dim()
    );
    println!(
        "{}",
        style("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━").cyan()
    );
    println!();
}

fn severity_style(s: &Severity) -> (console::StyledObject<&str>, console::StyledObject<&str>) {
    match s {
        Severity::Critical => (style("●").red().bold(), style("CRITICAL").red().bold()),
        Severity::Error => (style("●").red(), style("ERROR   ").red()),
        Severity::Warning => (style("◆").yellow(), style("WARN    ").yellow()),
        Severity::Info => (style("·").dim(), style("INFO    ").dim()),
    }
}

fn perf_color(
    val: f64,
    warn_threshold: f64,
    error_threshold: f64,
) -> impl Fn(String) -> console::StyledObject<String> {
    move |s: String| {
        if val >= error_threshold {
            style(s).red()
        } else if val >= warn_threshold {
            style(s).yellow()
        } else {
            style(s).green()
        }
    }
}
