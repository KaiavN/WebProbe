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
        "  Crawled: {} pages  |  {} states  |  {:.1}s",
        report.crawl_stats.pages_visited,
        report.crawl_stats.states_explored,
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
        // Group by page_url
        let mut by_page: std::collections::BTreeMap<&str, Vec<_>> =
            std::collections::BTreeMap::new();
        for issue in &report.issues {
            by_page
                .entry(issue.page_url.as_str())
                .or_default()
                .push(issue);
        }

        for (page, issues) in &by_page {
            println!("\n  {}", style(page).underlined().bold());
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

    // ── Performance ────────────────────────────────────────────────────────
    if !report.perf_metrics.is_empty() {
        println!(
            "\n{}",
            style("  ── Performance ──────────────────────────────────────────").dim()
        );
        for p in &report.perf_metrics {
            println!("  {}", style(&p.page_url).dim());
            if let Some(fcp) = p.fcp_ms {
                let s = perf_color(fcp, 1800.0, 3000.0);
                println!("    FCP:  {}ms", s(format!("{:.0}", fcp)));
            }
            if let Some(lcp) = p.lcp_ms {
                let s = perf_color(lcp, 2500.0, 4000.0);
                println!("    LCP:  {}ms", s(format!("{:.0}", lcp)));
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
    if !report.network_stats.is_empty() {
        println!(
            "\n{}",
            style("  ── Network Stats ─────────────────────────────────────────").dim()
        );
        for n in &report.network_stats {
            println!("  {}", style(&n.page_url).dim());
            if let Some(dns) = n.dns_ms {
                println!("    DNS:      {:.1}ms", dns);
            }
            if let Some(tcp) = n.tcp_connect_ms {
                println!("    TCP:      {:.1}ms", tcp);
            }
            if let Some(tls) = n.tls_ms {
                println!("    TLS:      {:.1}ms", tls);
            }
            if let Some(ttfb) = n.ttfb_ms {
                let s = perf_color(ttfb, 200.0, 600.0);
                println!("    TTFB:     {}ms", s(format!("{:.1}", ttfb)));
            }
            if let Some(dl) = n.download_ms {
                println!("    Download: {:.1}ms", dl);
            }
            println!(
                "    Resources: {}  failed: {}  transferred: {:.1}KB",
                n.resource_count, n.failed_resource_count, n.total_transfer_kb
            );
            if let (Some(ms), Some(url)) = (n.slowest_resource_ms, &n.slowest_resource_url) {
                let short_url = if url.len() > 60 { &url[url.len()-60..] } else { url.as_str() };
                println!("    Slowest:   {:.0}ms  …{}", ms, style(short_url).dim());
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
            "  Latency  p50:{:.0}ms  p90:{:.0}ms  p99:{:.0}ms  max:{:.0}ms",
            lt.latency_p50_ms, lt.latency_p90_ms, lt.latency_p99_ms, lt.latency_max_ms
        );
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
