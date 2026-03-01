use anyhow::Result;
use chromiumoxide::Page;

const DISCOVER_LINKS_JS: &str = r#"
(() => {
    const seen = new Set();
    const links = [];
    document.querySelectorAll('a[href]').forEach(el => {
        const href = el.href;
        if (!href || href.startsWith('javascript:') || href.startsWith('mailto:') || href.startsWith('tel:')) return;
        if (seen.has(href)) return;
        seen.add(href);
        links.push(href);
    });
    return JSON.stringify(links);
})()
"#;

/// Discover all anchor hrefs from the rendered DOM (SPA route discovery).
#[allow(dead_code)]
pub async fn discover_links(page: &Page) -> Result<Vec<String>> {
    let result = page.evaluate(DISCOVER_LINKS_JS).await?;
    let json_str = result
        .value()
        .and_then(|v| v.as_str().map(|s| s.to_string()))
        .unwrap_or_else(|| "[]".to_string());
    Ok(serde_json::from_str(&json_str).unwrap_or_default())
}
