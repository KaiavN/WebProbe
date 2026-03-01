use anyhow::Result;
use chromiumoxide::cdp::browser_protocol::page::AddScriptToEvaluateOnNewDocumentParams;
use chromiumoxide::Page;

/// JavaScript injected into every page *before* any page scripts run.
/// Captures console.error/warn, uncaught exceptions, unhandled promise rejections,
/// and sets up PerformanceObservers for LCP, CLS, and Long Tasks (TBT).
const COLLECTOR_JS: &str = r#"
window.__wp_errors = [];
window.__wp_lcp = null;
window.__wp_cls = 0;
window.__wp_tbt = 0;

(function () {
    const push = (type, msg) => window.__wp_errors.push({ type, msg: String(msg).slice(0, 500) });

    const origError = console.error.bind(console);
    console.error = function (...args) {
        push('error', args.map(a => (typeof a === 'object' ? JSON.stringify(a) : String(a))).join(' '));
        origError(...args);
    };
    const origWarn = console.warn.bind(console);
    console.warn = function (...args) {
        push('warning', args.map(a => (typeof a === 'object' ? JSON.stringify(a) : String(a))).join(' '));
        origWarn(...args);
    };

    window.addEventListener('error', function (e) {
        push('uncaught', e.message + (e.filename ? ' at ' + e.filename + ':' + e.lineno : ''));
    });
    window.addEventListener('unhandledrejection', function (e) {
        push('rejection', e.reason && e.reason.message ? e.reason.message : String(e.reason));
    });

    // Largest Contentful Paint
    try {
        new PerformanceObserver(function(list) {
            const entries = list.getEntries();
            if (entries.length) window.__wp_lcp = entries[entries.length - 1].startTime;
        }).observe({ type: 'largest-contentful-paint', buffered: true });
    } catch(_) {}

    // Cumulative Layout Shift
    try {
        new PerformanceObserver(function(list) {
            for (const entry of list.getEntries()) {
                if (!entry.hadRecentInput) window.__wp_cls += entry.value;
            }
        }).observe({ type: 'layout-shift', buffered: true });
    } catch(_) {}

    // Total Blocking Time (sum of blocking portions of long tasks >50ms)
    try {
        new PerformanceObserver(function(list) {
            for (const entry of list.getEntries()) {
                if (entry.duration > 50) window.__wp_tbt += entry.duration - 50;
            }
        }).observe({ type: 'longtask', buffered: true });
    } catch(_) {}
})();
"#;

pub struct JsCollector;

impl JsCollector {
    /// Register the error collector script so it runs before any page scripts.
    pub async fn inject_on_new_document(page: &Page) -> Result<()> {
        page.execute(AddScriptToEvaluateOnNewDocumentParams::new(COLLECTOR_JS))
            .await?;
        Ok(())
    }
}
