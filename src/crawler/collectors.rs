use anyhow::Result;
use chromiumoxide::cdp::browser_protocol::page::AddScriptToEvaluateOnNewDocumentParams;
use chromiumoxide::Page;

/// JavaScript injected into every page *before* any page scripts run.
/// Captures console.error/warn, uncaught exceptions, and unhandled promise rejections.
const COLLECTOR_JS: &str = r#"
window.__wp_errors = [];
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
