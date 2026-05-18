use crate::types::{Report, Severity};
use console::style;
use std::fmt::Write;

/// Format the report as a compact human-readable string.
/// Only includes actionable findings (issues) and load test summary.
/// All other data (performance, network, interactions) is excluded to keep
/// the report concise and focused on errors.
pub fn format_report(report: &Report) -> String {
    let mut out = String::new();

    // Header
    writeln!(out).unwrap();
    writeln!(out, "{}", style("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━").cyan()).unwrap();
    writeln!(
        out,
        "  {} {}",
        style("webprobe").bold().cyan(),
        style(format!("v{}", report.version)).dim()
    ).unwrap();
    writeln!(out, "  Target : {}", style(&report.target_url).underlined()).unwrap();
    writeln!(
        out,
        "  Crawled: {} page{}  |  {:.1}s",
        report.crawl_stats.pages_visited,
        if report.crawl_stats.pages_visited == 1 { "" } else { "s" },
        report.crawl_stats.duration_secs
    ).unwrap();
    writeln!(out, "{}", style("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━").cyan()).unwrap();

    // Issues (already deduplicated by deduplicate_issues)
    if report.issues.is_empty() {
        writeln!(out, "\n  {} No issues found!\n", style("✓").green().bold()).unwrap();
    } else {
        // Issues are already deduped; sort by severity then affected pages count
        let mut sorted_issues: Vec<_> = report.issues.iter().collect();
        sorted_issues.sort_by(|a, b| {
            let sev_cmp = b.severity.cmp(&a.severity);
            if !sev_cmp.is_eq() {
                return sev_cmp;
            }
            b.affected_pages_count.cmp(&a.affected_pages_count)
        });

        writeln!(out, "\n{}", style("  ── Issues ──────────────────────────────────────────────────").dim()).unwrap();

        for issue in sorted_issues {
            let (icon, sev_str) = severity_style(&issue.severity);
            let cat = style(format!("[{}]", issue.category)).dim();
            let total_pages = issue.affected_pages_count.unwrap_or(issue.page_urls.len());

            // Main line
            writeln!(out, "  {} {} {} {} ({} pages)", icon, sev_str, cat, issue.message, total_pages).unwrap();

            // Element detail if any
            if let Some(el) = &issue.element {
                writeln!(out, "       {}", style(format!("↳ {}", el)).dim()).unwrap();
            }

            // List up to 5 affected pages (sample, with URL prefix stripped)
            let mut displayed_pages: Vec<String> = issue.page_urls.iter()
                .map(|url| truncate_path(url, &report.target_url))
                .collect();
            displayed_pages.sort();

            for page in displayed_pages.iter().take(5) {
                writeln!(out, "    {}", style(page).dim()).unwrap();
            }
            if total_pages > displayed_pages.len() {
                writeln!(out, "    {} {} more", style("·").dim(), style(total_pages - displayed_pages.len()).dim()).unwrap();
            }
        }
    }

    // Load Test (if present)
    if let Some(lt) = &report.load_test {
        writeln!(out).unwrap();
        writeln!(out, "{}", style("  ── Load Test ─────────────────────────────────────────────").dim()).unwrap();
        writeln!(out, "  {} users  ×  {}s  →  {:.1} req/s", lt.users, lt.duration_secs, lt.throughput_rps).unwrap();
        writeln!(
            out,
            "  Requests: {} total  {} ok  {} failed  ({:.1}% error rate)",
            lt.total_requests,
            style(lt.successful_requests).green(),
            style(lt.failed_requests).red(),
            lt.error_rate_pct
        ).unwrap();
        writeln!(
            out,
            "  Latency  mean:{:.0}ms  p50:{:.0}ms  p90:{:.0}ms  p95:{:.0}ms  p99:{:.0}ms  max:{:.0}ms",
            lt.latency_mean_ms,
            lt.latency_p50_ms,
            lt.latency_p90_ms,
            lt.latency_p95_ms,
            lt.latency_p99_ms,
            lt.latency_max_ms
        ).unwrap();
    }

    // Summary
    writeln!(out).unwrap();
    writeln!(out, "{}", style("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━").cyan()).unwrap();
    writeln!(
        out,
        "  Summary  {} critical  {} errors  {} warnings  {} info",
        style(report.summary.critical).red().bold(),
        style(report.summary.errors).red(),
        style(report.summary.warnings).yellow(),
        style(report.summary.infos).dim()
    ).unwrap();
    writeln!(out, "{}", style("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━").cyan()).unwrap();
    writeln!(out).unwrap();

    out
}

fn severity_style(s: &Severity) -> (String, String) {
    match s {
        Severity::Critical => (style("●").red().bold().to_string(), style("CRITICAL").red().bold().to_string()),
        Severity::Error => (style("●").red().to_string(), style("ERROR   ").red().to_string()),
        Severity::Warning => (style("◆").yellow().to_string(), style("WARN    ").yellow().to_string()),
        Severity::Info => (style("·").dim().to_string(), style("INFO    ").dim().to_string()),
    }
}

fn truncate_path(url: &str, target_url: &str) -> String {
    let target_norm = normalize_url(target_url);
    if url.starts_with(&target_norm) {
        let rest = &url[target_norm.len()..];
        if target_norm.ends_with('/') {
            return rest.to_string();
        }
        if rest.starts_with('/') {
            return rest[1..].to_string();
        }
        rest.to_string()
    } else {
        url.to_string()
    }
}

fn normalize_url(raw: &str) -> String {
    if raw.ends_with('/') && raw.len() > 1 && !raw.ends_with("://") {
        raw.trim_end_matches('/').to_string()
    } else {
        raw.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_truncate_urls_removes_common_prefix() {
        let target = "http://localhost:3000";
        let urls = vec![
            "http://localhost:3000/",
            "http://localhost:3000/dashboard",
            "http://localhost:3000/settings",
        ];
        let mut result = Vec::new();
        for url in &urls {
            result.push(truncate_path(url, target));
        }
        assert_eq!(result, vec!["", "dashboard", "settings"]);
    }

    #[test]
    fn test_truncate_urls_removes_trailing_slash() {
        let target = "http://localhost:3000/";
        let urls = vec![
            "http://localhost:3000/",
            "http://localhost:3000/dashboard",
        ];
        let mut result = Vec::new();
        for url in &urls {
            result.push(truncate_path(url, target));
        }
        assert_eq!(result, vec!["", "dashboard"]);
    }
}
