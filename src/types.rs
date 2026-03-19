use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fmt;

/// serde skip helper: skip Option<f64> when it's None or Some(0.0)
pub fn is_zero_f64(v: &Option<f64>) -> bool {
    match v {
        None => true,
        Some(x) => *x == 0.0,
    }
}

// ── Auth Config ──────────────────────────────────────────────────────────────

/// Authentication configuration for crawling sites that require login.
#[derive(Debug, Clone, Default)]
pub struct AuthConfig {
    /// URL of the login page (path like "/login" or full URL)
    pub login_url: Option<String>,
    /// Username / email to fill in (requires browser; ignored in HTTP mode)
    pub username: Option<String>,
    /// Password to fill in (requires browser; ignored in HTTP mode)
    pub password: Option<String>,
    /// CSS selector for the username/email field (requires browser; ignored in HTTP mode)
    pub username_selector: Option<String>,
    /// CSS selector for the password field (requires browser; ignored in HTTP mode)
    pub password_selector: Option<String>,
    /// CSS selector for the submit button (requires browser; ignored in HTTP mode)
    pub submit_selector: Option<String>,
    /// Path to a JSON cookie file to inject before crawling
    pub cookies_file: Option<std::path::PathBuf>,
}

/// A single cookie entry from a JSON cookie file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CookieEntry {
    pub name: String,
    pub value: String,
    #[serde(default)]
    pub domain: Option<String>,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub secure: Option<bool>,
    #[serde(rename = "httpOnly", default)]
    pub http_only: Option<bool>,
}

// ── Severity ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Info,
    Warning,
    Error,
    Critical,
}

impl fmt::Display for Severity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Severity::Info => write!(f, "INFO"),
            Severity::Warning => write!(f, "WARN"),
            Severity::Error => write!(f, "ERROR"),
            Severity::Critical => write!(f, "CRITICAL"),
        }
    }
}

// ── Issue Category ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum IssueCategory {
    ConsoleError,
    NetworkError,
    BrokenLink,
    FailedResource,
    Accessibility,
    Performance,
    Security,
    Seo,
    LoadTest,
    UnhandledRejection,
}

impl fmt::Display for IssueCategory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IssueCategory::ConsoleError => write!(f, "Console Error"),
            IssueCategory::NetworkError => write!(f, "Network Error"),
            IssueCategory::BrokenLink => write!(f, "Broken Link"),
            IssueCategory::FailedResource => write!(f, "Failed Resource"),
            IssueCategory::Accessibility => write!(f, "Accessibility"),
            IssueCategory::Performance => write!(f, "Performance"),
            IssueCategory::Security => write!(f, "Security"),
            IssueCategory::Seo => write!(f, "SEO"),
            IssueCategory::LoadTest => write!(f, "Load Test"),
            IssueCategory::UnhandledRejection => write!(f, "Unhandled Rejection"),
        }
    }
}

// ── Issue ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Issue {
    pub severity: Severity,
    pub category: IssueCategory,
    pub message: String,
    /// Sample of affected page URLs (truncated for compactness). See `affected_pages_count` for total.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub page_urls: Vec<String>,
    /// Total number of unique pages affected (may exceed page_urls.len() if truncated)
    #[serde(default)]
    pub affected_pages_count: Option<usize>,
    /// The element selector or description that triggered this (if applicable)
    pub element: Option<String>,
    /// The action path taken to reach this state (not serialized to keep reports compact)
    #[serde(skip)]
    pub action_path: Vec<String>,
}

// ── Page State ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct PageState {
    pub url: String,
    /// Ordered sequence of actions taken to arrive at this state
    pub action_path: Vec<String>,
    pub depth: usize,
}

pub fn normalize_url(raw: &str) -> String {
    if raw.ends_with('/') && raw.len() > 1 && !raw.ends_with("://") {
        raw.trim_end_matches('/').to_string()
    } else {
        raw.to_string()
    }
}

impl PageState {
    pub fn new(url: impl Into<String>) -> Self {
        let raw: String = url.into();
        Self {
            url: normalize_url(&raw),
            action_path: vec![],
            depth: 0,
        }
    }

    /// Fingerprint used for deduplication
    pub fn fingerprint(&self) -> String {
        normalize_url(&self.url)
    }

    pub fn child(&self, url: impl Into<String>, action: &str) -> Self {
        let mut new_path = self.action_path.clone();
        new_path.push(action.to_string());
        let raw_url: String = url.into();
        Self {
            url: normalize_url(&raw_url),
            action_path: new_path,
            depth: self.depth + 1,
        }
    }
}

// ── Performance Metrics ──────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PerfMetrics {
    pub page_url: String,
    /// First Contentful Paint (ms)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fcp_ms: Option<f64>,
    /// Largest Contentful Paint (ms)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lcp_ms: Option<f64>,
    /// Time to Interactive (ms)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tti_ms: Option<f64>,
    /// Cumulative Layout Shift score (null on Firefox / no layout shifts)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cls_score: Option<f64>,
    /// DOM Content Loaded (ms)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dom_content_loaded_ms: Option<f64>,
    /// Load event (ms)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub load_ms: Option<f64>,
}

// ── Load Test Results ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoadTestResult {
    pub url: String,
    pub users: u32,
    pub duration_secs: u64,
    pub total_requests: u64,
    pub successful_requests: u64,
    pub failed_requests: u64,
    pub error_rate_pct: f64,
    pub throughput_rps: f64,
    pub latency_p50_ms: f64,
    pub latency_p90_ms: f64,
    pub latency_p95_ms: f64,
    pub latency_p99_ms: f64,
    pub latency_min_ms: f64,
    pub latency_max_ms: f64,
    pub latency_mean_ms: f64,
}

// ── Network Stats ─────────────────────────────────────────────────────────────

/// Per-page network timing collected via the browser's Performance API.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct NetworkStats {
    #[serde(skip)]
    pub page_url: String,
    /// DNS lookup time for the main document (ms) — always 0 on localhost
    #[serde(skip_serializing_if = "crate::types::is_zero_f64")]
    pub dns_ms: Option<f64>,
    /// TCP connection setup time (ms) — always 0 on localhost
    #[serde(skip_serializing_if = "crate::types::is_zero_f64")]
    pub tcp_connect_ms: Option<f64>,
    /// TLS/SSL negotiation time (ms) — always null on plain HTTP
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tls_ms: Option<f64>,
    /// Time To First Byte — time from request start until first byte of response (ms)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ttfb_ms: Option<f64>,
    /// Response body download time (ms) — usually 0 on localhost
    #[serde(skip_serializing_if = "crate::types::is_zero_f64")]
    pub download_ms: Option<f64>,
    /// Total number of sub-resources loaded (JS, CSS, images, XHR…)
    pub resource_count: usize,
    /// Number of resources that appear to have failed
    pub failed_resource_count: usize,
    /// URLs of resources that failed to load
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub failed_resource_urls: Vec<String>,
    /// Estimated total transfer size across all resources (KB)
    pub total_transfer_kb: f64,
    /// Duration of the slowest sub-resource (ms)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub slowest_resource_ms: Option<f64>,
    /// URL of the slowest sub-resource
    #[serde(skip_serializing_if = "Option::is_none")]
    pub slowest_resource_url: Option<String>,
}

// ── Page Links ──────────────────────────────────────────────────────────────

/// A single interactive element found on a page.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct InteractiveElement {
    /// Element type: "link", "button", "input", "select", "textarea"
    pub kind: String,
    /// Visible label, accessible name, or placeholder text
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// href (links only)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub href: Option<String>,
    /// input type attribute (inputs only, e.g. "text", "checkbox")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_type: Option<String>,
}

/// All interactive elements found on a single page.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PageInteractions {
    #[serde(skip)]
    pub page_url: String,
    /// Total interactive elements found
    pub elements_found: usize,
    pub elements: Vec<InteractiveElement>,
}

// ── Crawl Stats ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CrawlStats {
    pub pages_visited: usize,
    pub duration_secs: f64,
    /// Total interactive elements found across all pages
    pub elements_interacted: usize,
    /// Every URL that was successfully crawled (audited)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub crawled_urls: Vec<String>,
}

// ── Full Report ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PageReport {
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub perf_metrics: Option<PerfMetrics>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub network_stats: Option<NetworkStats>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub interactions: Option<PageInteractions>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Report {
    pub tool: String,
    pub version: String,
    pub timestamp: DateTime<Utc>,
    pub target_url: String,
    pub issues: Vec<Issue>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pages: Vec<PageReport>,
    pub crawl_stats: CrawlStats,
    /// All unique internal URLs discovered via link-following (superset of crawled_urls)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub discovered_urls: Vec<String>,
    pub load_test: Option<LoadTestResult>,
    /// Issue counts by severity
    pub summary: ReportSummary,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ReportSummary {
    pub critical: usize,
    pub errors: usize,
    pub warnings: usize,
    pub infos: usize,
    pub total: usize,
}

impl Report {
    pub fn new(target_url: impl Into<String>) -> Self {
        Self {
            tool: "webprobe".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            timestamp: Utc::now(),
            target_url: target_url.into(),
            issues: vec![],
            pages: vec![],
            crawl_stats: CrawlStats::default(),
            discovered_urls: vec![],
            load_test: None,
            summary: ReportSummary::default(),
        }
    }

    pub fn compute_summary(&mut self) {
        let mut s = ReportSummary::default();
        for issue in &self.issues {
            match issue.severity {
                Severity::Critical => s.critical += 1,
                Severity::Error => s.errors += 1,
                Severity::Warning => s.warnings += 1,
                Severity::Info => s.infos += 1,
            }
        }
        s.total = self.issues.len();
        self.summary = s;
    }
}

/// Deduplicate issues by merging duplicates (same severity, category, message, element).
/// Aggregates page_urls and clears action_path to reduce report size.
/// Also sets `affected_pages_count` and truncates `page_urls` to a maximum to keep JSON compact.
pub fn deduplicate_issues(issues: Vec<Issue>, max_pages_per_issue: usize) -> Vec<Issue> {
    use std::collections::BTreeMap;
    let mut groups: BTreeMap<(Severity, IssueCategory, String, Option<String>), Vec<Issue>> =
        BTreeMap::new();
    for issue in issues {
        groups
            .entry((
                issue.severity.clone(),
                issue.category.clone(),
                issue.message.clone(),
                issue.element.clone(),
            ))
            .or_default()
            .push(issue);
    }
    let mut result = Vec::new();
    for (_, group) in groups {
        // Combine all page_urls from the group
        let mut all_urls = Vec::new();
        for issue in &group {
            all_urls.extend(issue.page_urls.iter().cloned());
        }
        all_urls.sort();
        all_urls.dedup();
        let total_count = all_urls.len();
        // Truncate to max_pages_per_issue to keep JSON size manageable
        if all_urls.len() > max_pages_per_issue {
            all_urls.truncate(max_pages_per_issue);
        }
        // Use the first issue as a template; clear action_path and set combined URLs
        let mut base = group.into_iter().next().unwrap();
        base.page_urls = all_urls;
        base.affected_pages_count = Some(total_count);
        base.action_path = Vec::new();
        result.push(base);
    }
    // Sort: severity descending, then by number of affected pages
    result.sort_by(|a, b| {
        let sev = b.severity.cmp(&a.severity);
        if !sev.is_eq() {
            return sev;
        }
        b.affected_pages_count.cmp(&a.affected_pages_count)
    });
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_page_state_fingerprint_deduplication() {
        let mut state1 = PageState::new("http://example.com/login");
        let mut state2 = PageState::new("http://example.com/login");

        // state1 got here directly
        state1.action_path = vec![];

        // state2 got here via some other page (ping-pong)
        state2.action_path = vec!["link".to_string(), "button_click".to_string()];

        // Their fingerprints MUST be identical so they deduplicate correctly
        assert_eq!(
            state1.fingerprint(),
            state2.fingerprint(),
            "Fingerprints must match ignoring action path to prevent infinite ping-ponging across cross-linked pages"
        );

        // Also test root URL normalization
        assert_eq!(PageState::new("http://example.com").fingerprint(), "http://example.com");
        assert_eq!(PageState::new("http://example.com/").fingerprint(), "http://example.com");
        assert_eq!(PageState::new("http://example.com/path").fingerprint(), "http://example.com/path");
        assert_eq!(PageState::new("http://example.com/path/").fingerprint(), "http://example.com/path");
    }

    #[test]
    fn test_deduplicate_issues() {
        let issue1 = Issue {
            severity: Severity::Error,
            category: IssueCategory::ConsoleError,
            message: "Uncaught TypeError".to_string(),
            page_urls: vec![
                "http://localhost/".to_string(),
                "http://localhost/page1".to_string(),
            ],
            element: Some("#root".to_string()),
            action_path: vec!["click".to_string()],
            affected_pages_count: None,
        };
        let issue2 = Issue {
            severity: Severity::Error,
            category: IssueCategory::ConsoleError,
            message: "Uncaught TypeError".to_string(),
            page_urls: vec!["http://localhost/page2".to_string()],
            element: Some("#root".to_string()),
            action_path: vec!["link".to_string()],
            affected_pages_count: None,
        };
        let issue3 = Issue {
            severity: Severity::Warning,
            category: IssueCategory::Performance,
            message: "Slow resource".to_string(),
            page_urls: vec!["http://localhost/".to_string()],
            element: None,
            action_path: vec![],
            affected_pages_count: None,
        };
        let input = vec![issue1.clone(), issue2, issue3, issue1.clone()]; // includes Error and Warning
        let result = deduplicate_issues(input, 100);

        assert_eq!(result.len(), 2);

        // Error group should have combined page URLs
        let error_issue = result
            .iter()
            .find(|i| matches!(i.severity, Severity::Error))
            .unwrap();
        assert_eq!(error_issue.message, "Uncaught TypeError");
        assert_eq!(error_issue.category, IssueCategory::ConsoleError);
        assert_eq!(error_issue.element, Some("#root".to_string()));
        let mut urls = error_issue.page_urls.clone();
        urls.sort();
        assert_eq!(
            urls,
            vec![
                "http://localhost/".to_string(),
                "http://localhost/page1".to_string(),
                "http://localhost/page2".to_string()
            ]
        );
        assert!(error_issue.action_path.is_empty());

        // Warning issue unchanged
        let warning_issue = result
            .iter()
            .find(|i| matches!(i.severity, Severity::Warning))
            .unwrap();
        assert_eq!(warning_issue.message, "Slow resource");
        assert_eq!(warning_issue.page_urls, vec!["http://localhost/".to_string()]);
        assert!(warning_issue.action_path.is_empty());
    }

    #[test]
    fn test_deduplicate_issues_truncates_large_url_sets() {
        // Create an issue that appears on 150 pages
        let mut many_urls = Vec::new();
        for i in 0..150 {
            many_urls.push(format!("http://localhost/page{}", i));
        }
        let issue = Issue {
            severity: Severity::Warning,
            category: IssueCategory::ConsoleError,
            message: "Many pages".to_string(),
            page_urls: many_urls.clone(),
            element: None,
            action_path: vec![],
            affected_pages_count: None,
        };
        let result = deduplicate_issues(vec![issue], 100);

        assert_eq!(result.len(), 1);
        let deduped = &result[0];
        // Should be truncated to MAX_PAGES_PER_ISSUE (100)
        assert_eq!(deduped.page_urls.len(), 100);
        // But total count should be 150
        assert_eq!(deduped.affected_pages_count, Some(150));
        // The sample should be first 100 alphabetically after sort/dedup
        let mut expected_sample = many_urls;
        expected_sample.sort();
        expected_sample.truncate(100);
        assert_eq!(deduped.page_urls, expected_sample);
    }
}
