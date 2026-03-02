use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fmt;

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

#[derive(Debug, Clone, Serialize, Deserialize)]
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
    /// The page URL where this was found
    pub page_url: String,
    /// The element selector or description that triggered this (if applicable)
    pub element: Option<String>,
    /// The action path taken to reach this state
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

impl PageState {
    pub fn new(url: impl Into<String>) -> Self {
        let raw: String = url.into();
        // Normalize trailing slash (except bare "/" or "scheme://host/")
        let url = if raw.ends_with('/') && raw.len() > 1 && !raw.ends_with("://") {
            raw.trim_end_matches('/').to_string()
        } else {
            raw
        };
        Self {
            url,
            action_path: vec![],
            depth: 0,
        }
    }

    /// Fingerprint used for deduplication
    pub fn fingerprint(&self) -> String {
        let mut sorted_path = self.action_path.clone();
        sorted_path.sort();
        // Normalize URL: strip trailing slash (except root /)
        let norm_url = if self.url.ends_with('/') && self.url.len() > 1
            && !self.url.ends_with("://")
        {
            self.url.trim_end_matches('/').to_string()
        } else {
            self.url.clone()
        };
        format!("{}|{}", norm_url, sorted_path.join(";"))
    }

    pub fn child(&self, url: impl Into<String>, action: &str) -> Self {
        let mut new_path = self.action_path.clone();
        new_path.push(action.to_string());
        Self {
            url: url.into(),
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
    pub fcp_ms: Option<f64>,
    /// Largest Contentful Paint (ms)
    pub lcp_ms: Option<f64>,
    /// Time to Interactive (ms)
    pub tti_ms: Option<f64>,
    /// Cumulative Layout Shift score
    pub cls_score: Option<f64>,
    /// Total Blocking Time (ms)
    pub tbt_ms: Option<f64>,
    /// DOM Content Loaded (ms)
    pub dom_content_loaded_ms: Option<f64>,
    /// Load event (ms)
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
    pub page_url: String,
    /// DNS lookup time for the main document (ms)
    pub dns_ms: Option<f64>,
    /// TCP connection setup time (ms)
    pub tcp_connect_ms: Option<f64>,
    /// TLS/SSL negotiation time (ms); null for plain HTTP
    pub tls_ms: Option<f64>,
    /// Time To First Byte — time from request start until first byte of response (ms)
    pub ttfb_ms: Option<f64>,
    /// Response body download time (ms)
    pub download_ms: Option<f64>,
    /// Total number of sub-resources loaded (JS, CSS, images, XHR…)
    pub resource_count: usize,
    /// Number of resources that appear to have failed (0-byte transfer, 0ms duration)
    pub failed_resource_count: usize,
    /// Estimated total transfer size across all resources (KB); may be 0 for cross-origin resources
    pub total_transfer_kb: f64,
    /// Duration of the slowest sub-resource (ms)
    pub slowest_resource_ms: Option<f64>,
    /// URL of the slowest sub-resource
    pub slowest_resource_url: Option<String>,
}

// ── Crawl Stats ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CrawlStats {
    pub pages_visited: usize,
    pub elements_interacted: usize,
    pub states_explored: usize,
    pub duration_secs: f64,
}

// ── Full Report ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Report {
    pub tool: String,
    pub version: String,
    pub timestamp: DateTime<Utc>,
    pub target_url: String,
    pub issues: Vec<Issue>,
    pub perf_metrics: Vec<PerfMetrics>,
    pub network_stats: Vec<NetworkStats>,
    pub crawl_stats: CrawlStats,
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
            perf_metrics: vec![],
            network_stats: vec![],
            crawl_stats: CrawlStats::default(),
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
