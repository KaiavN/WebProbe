use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio_tungstenite::{connect_async, tungstenite::Message};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TargetInfo {
    pub id: String,
    #[serde(rename = "type")]
    pub target_type: String,
    pub url: String,
    pub title: Option<String>,
    pub ws_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VersionInfo {
    pub Browser: String,
    #[serde(rename = "Protocol-Version")]
    pub protocol_version: String,
    #[serde(rename = "User-Agent")]
    pub user_agent: String,
    #[serde(rename = "webSocketDebuggerUrl")]
    pub ws_debugger_url: String,
}

// ── CdpSession ────────────────────────────────────────────────────────────────

/// A single CDP session. Commands are sent over the browser websocket
/// with the sessionId embedded in each message.
struct CdpSession {
    ws_url: String,
    session_id: String,
    next_id: AtomicU64,
}

impl CdpSession {
    fn new(ws_url: String, session_id: String) -> Self {
        Self {
            ws_url,
            session_id,
            next_id: AtomicU64::new(1),
        }
    }

    /// Create a new target and attach to it, returning a CdpSession.
    async fn connect(browser_ws_url: &str) -> Result<Self> {
        let (mut ws, _) = connect_async(browser_ws_url).await
            .context("Failed to connect to browser websocket")?;

        // Create target
        let id = Self::next_id();
        ws.send(Message::Text(serde_json::json!({
            "id": id,
            "method": "Target.createTarget",
            "params": { "url": "about:blank" }
        }).to_string().into())).await?;

        let deadline = Instant::now() + Duration::from_secs(15);
        let mut target_id = None;
        let mut session_id = None;

        while Instant::now() < deadline {
            if let Some(msg) = ws.next().await {
                if let Ok(Message::Text(text)) = msg {
                    if let Ok(v) = serde_json::from_str::<Value>(&text) {
                        if v.get("method").and_then(|m| m.as_str()) == Some("Target.targetCreated") {
                            if target_id.is_none() {
                                target_id = v.get("params")
                                    .and_then(|p| p.get("targetInfo"))
                                    .and_then(|ti| ti.get("targetId"))
                                    .and_then(|s| s.as_str())
                                    .map(|s| s.to_string());
                            }
                        }
                        if v.get("method").and_then(|m| m.as_str()) == Some("Target.attachedToTarget") {
                            let tid = v.get("params")
                                .and_then(|p| p.get("targetInfo"))
                                .and_then(|ti| ti.get("targetId"))
                                .and_then(|s| s.as_str());
                            if tid == target_id.as_deref() {
                                session_id = v.get("params")
                                    .and_then(|p| p.get("sessionId"))
                                    .and_then(|s| s.as_str())
                                    .map(|s| s.to_string());
                            }
                        }
                        if v.get("id").and_then(|i| i.as_u64()) == Some(id) {
                            if target_id.is_none() {
                                target_id = v.get("result")
                                    .and_then(|r| r.get("targetId"))
                                    .and_then(|s| s.as_str())
                                    .map(|s| s.to_string());
                            }
                        }
                        if target_id.is_some() && session_id.is_some() {
                            break;
                        }
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        let target_id = target_id.ok_or_else(|| anyhow::anyhow!("Target.createTarget failed"))?;
        let session_id = session_id.ok_or_else(|| anyhow::anyhow!("No sessionId from Target.attachedToTarget"))?;

        Ok(Self::new(browser_ws_url.to_string(), session_id))
    }

    fn next_id() -> u64 {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        NEXT.fetch_add(1, Ordering::SeqCst)
    }

    /// Execute a CDP command and return the result value.
    async fn execute(&self, method: &str, params: Value) -> Result<Value> {
        let (mut ws, _) = connect_async(&self.ws_url).await
            .context("Failed to connect to browser websocket")?;

        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let mut json = serde_json::json!({
            "id": id,
            "method": method,
            "params": params
        });
        json["sessionId"] = serde_json::json!(&self.session_id);
        ws.send(Message::Text(serde_json::to_string(&json)?.into())).await?;

        let deadline = Instant::now() + Duration::from_secs(15);
        while Instant::now() < deadline {
            if let Some(msg) = ws.next().await {
                if let Ok(Message::Text(text)) = msg {
                    if let Ok(v) = serde_json::from_str::<Value>(&text) {
                        if v.get("id").and_then(|i| i.as_u64()) == Some(id) {
                            if let Some(error) = v.get("error") {
                                anyhow::bail!("CDP error: {} method={}", error, method);
                            }
                            return Ok(v.get("result").cloned().unwrap_or(Value::Null));
                        }
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        anyhow::bail!("CDP response timeout for {} id={}", method, id)
    }

    async fn navigate(&self, url: &str) -> Result<()> {
        let (mut ws, _) = connect_async(&self.ws_url).await
            .context("Failed to connect to browser websocket")?;

        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let mut json = serde_json::json!({
            "id": id,
            "method": "Page.navigate",
            "params": { "url": url }
        });
        json["sessionId"] = serde_json::json!(&self.session_id);
        ws.send(Message::Text(serde_json::to_string(&json)?.into())).await?;

        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            if let Some(msg) = ws.next().await {
                if let Ok(Message::Text(text)) = msg {
                    if let Ok(v) = serde_json::from_str::<Value>(&text) {
                        if v.get("method").and_then(|m| m.as_str()) == Some("Page.loadEventFired") {
                            return Ok(());
                        }
                        if v.get("id").and_then(|i| i.as_u64()) == Some(id) {
                            if let Some(error) = v.get("error") {
                                anyhow::bail!("Page navigation error: {}", error);
                            }
                            return Ok(());
                        }
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        Ok(())
    }

    async fn close(&self) -> Result<()> {
        let _ = self.evaluate("Page.close").await;
        Ok(())
    }

    async fn evaluate(&self, script: &str) -> Result<Value> {
        self.execute("Runtime.evaluate", serde_json::json!({
            "expression": script,
            "returnByValue": true
        })).await
    }
}

// ── Page ─────────────────────────────────────────────────────────────────────

/// A browser page tab backed by a CdpSession wrapped in Arc+Mutex for thread-safe interior mutability.
pub struct Page {
    session: Arc<tokio::sync::Mutex<CdpSession>>,
}

impl Page {
    /// Create a new Page at the given browser base_url.
    pub async fn new(base_url: &str) -> Result<Self> {
        let version_url = format!("{}/json/version", base_url);
        let resp = reqwest::get(&version_url).await?;
        let version: VersionInfo = serde_json::from_str(&resp.text().await?)?;
        let session = CdpSession::connect(&version.ws_debugger_url).await?;
        session.execute("Page.enable", serde_json::json!({})).await?;
        Ok(Self { session: Arc::new(tokio::sync::Mutex::new(session)) })
    }

    pub async fn goto(&self, url: &str) -> Result<()> {
        self.session.lock().await.navigate(url).await
    }

    pub async fn current_url(&self) -> Result<String> {
        let result = self.session.lock().await.evaluate("window.location.href").await?;
        Ok(result.get("value").and_then(|s| s.as_str()).unwrap_or_default().to_string())
    }

    pub async fn source(&self) -> Result<String> {
        let result = self.session.lock().await.evaluate("document.documentElement.outerHTML").await?;
        Ok(result.get("value").and_then(|s| s.as_str()).unwrap_or_default().to_string())
    }

    pub async fn execute(&self, script: &str) -> Result<Value> {
        self.session.lock().await.evaluate(script).await
    }

    pub async fn find(&self, selector: &str) -> Result<Option<Element>> {
        let escaped = selector.replace("'", "\\'");
        let script = format!(
            "(function() {{ var el = document.querySelector('{}'); if (!el) return null; return el.outerHTML; }})()",
            escaped
        );
        let result = self.execute(&script).await?;
        if let Some(value) = result.get("value") {
            if let Some(s) = value.as_str() {
                if s.is_empty() || s == "null" {
                    return Ok(None);
                }
                return Ok(Some(Element { html: s.to_string() }));
            }
        }
        Ok(None)
    }

    pub async fn find_all(&self, selector: &str) -> Result<Vec<Element>> {
        let escaped = selector.replace("'", "\\'");
        let script = format!(
            "(function() {{ return Array.from(document.querySelectorAll('{}')).map(el => el.outerHTML); }})()",
            escaped
        );
        let result = self.execute(&script).await?;
        if let Some(value) = result.get("value") {
            if let Some(arr) = value.as_array() {
                return Ok(arr.iter()
                    .filter_map(|v| v.as_str().map(|s| Element { html: s.to_string() }))
                    .collect());
            }
        }
        Ok(vec![])
    }

    pub async fn get_all_cookies(&self) -> Result<Vec<Cookie>> {
        let result = self.session.lock().await.evaluate("document.cookie").await?;
        let cookie_str = result.get("value").and_then(|s| s.as_str()).unwrap_or_default();
        let cookies: Vec<Cookie> = cookie_str.split("; ")
            .filter_map(|c| {
                let mut parts = c.splitn(2, '=');
                let name = parts.next()?.trim().to_string();
                let value = parts.next()?.to_string();
                Some(Cookie {
                    name,
                    value,
                    domain: "localhost".to_string(),
                    path: Some("/".to_string()),
                    secure: None,
                    http_only: None,
                })
            })
            .collect();
        Ok(cookies)
    }

    pub async fn add_cookie(&self, _cookie: Cookie) -> Result<()> {
        Ok(())
    }

    pub async fn close(self) -> Result<()> {
        self.session.lock().await.close().await
    }

    pub async fn click(&self, selector: &str) -> Result<()> {
        let escaped = selector.replace("'", "\\'");
        let script = format!(
            "(function() {{ var el = document.querySelector('{}'); if (el) el.click(); }})()",
            escaped
        );
        self.execute(&script).await?;
        Ok(())
    }

    pub async fn clear(&self, selector: &str) -> Result<()> {
        let escaped = selector.replace("'", "\\'");
        let script = format!(
            "(function() {{ var el = document.querySelector('{}'); if (el) {{ el.value = ''; el.dispatchEvent(new Event('input', {{bubbles:true}})); }} }})()",
            escaped
        );
        self.execute(&script).await?;
        Ok(())
    }

    pub async fn send_keys(&self, selector: &str, text: &str) -> Result<()> {
        let escaped_sel = selector.replace("'", "\\'");
        let escaped_text = text
            .replace("\\", "\\\\")
            .replace("'", "\\'")
            .replace("\n", "\\n");
        let script = format!(
            "(function() {{ var el = document.querySelector('{}'); if (el) {{ el.focus(); el.value = '{}'; el.dispatchEvent(new Event('input', {{bubbles:true}})); el.dispatchEvent(new Event('change', {{bubbles:true}})); }} }})()",
            escaped_sel,
            escaped_text
        );
        self.execute(&script).await?;
        Ok(())
    }

    pub async fn execute_async(&self, script: &str, timeout_ms: u64) -> Result<Value> {
        self.session.lock().await.execute("Runtime.evaluate", serde_json::json!({
            "expression": format!("(async () => {{ {} }})()", script),
            "returnByValue": true,
            "awaitPromise": true,
            "timeout": timeout_ms * 1_000_000
        })).await
    }
}

/// Create a new Page session at the given browser base_url.
pub async fn get_page(base_url: &str) -> Result<Page> {
    Page::new(base_url).await
}

// ── Types ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Cookie {
    pub name: String,
    pub value: String,
    pub domain: String,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub secure: Option<bool>,
    #[serde(rename = "httpOnly", default)]
    pub http_only: Option<bool>,
}

#[derive(Debug, Clone)]
pub struct Element {
    pub html: String,
}