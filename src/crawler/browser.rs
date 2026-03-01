use anyhow::{Context, Result};
use chromiumoxide::{Browser, BrowserConfig, Page};
use futures::StreamExt;
use std::time::Duration;

const LAUNCH_TIMEOUT: Duration = Duration::from_secs(30);

pub struct HeadlessBrowser {
    pub browser: Browser,
}

impl HeadlessBrowser {
    pub async fn launch(headless: bool) -> Result<(Self, impl futures::Future<Output = ()>)> {
        let mut builder = BrowserConfig::builder()
            .no_sandbox()
            .window_size(1280, 900)
            .chrome_executable("/Applications/Google Chrome.app/Contents/MacOS/Google Chrome")
            .arg("--disable-dev-shm-usage")
            .arg("--disable-gpu")
            .arg("--no-first-run")
            .arg("--no-default-browser-check")
            .arg("--disable-extensions")
            .arg("--disable-background-networking")
            .arg("--disable-default-apps")
            .arg("--disable-sync")
            .arg("--disable-translate")
            .arg("--disable-features=Translate,OptimizationHints")
            .arg("--safebrowsing-disable-auto-update")
            .arg("--password-store=basic")
            .arg("--use-mock-keychain")
            // Speed: disable costly rendering features not needed for auditing
            .arg("--disable-background-timer-throttling")
            .arg("--disable-renderer-backgrounding")
            .arg("--disable-ipc-flooding-protection");

        if headless {
            builder = builder.arg("--headless").arg("--hide-scrollbars");
        }

        let config = builder.build().map_err(|e| anyhow::anyhow!("{}", e))?;

        let (browser, mut handler) = tokio::time::timeout(LAUNCH_TIMEOUT, Browser::launch(config))
            .await
            .context("Chrome took too long to start (>30s)")?
            .context("Failed to launch Chrome — is Google Chrome installed in /Applications?")?;

        let driver = async move {
            loop {
                if handler.next().await.is_none() {
                    break;
                }
            }
        };

        Ok((Self { browser }, driver))
    }

    pub async fn new_page(&self) -> Result<Page> {
        tokio::time::timeout(
            Duration::from_secs(10),
            self.browser.new_page("about:blank"),
        )
        .await
        .context("Timed out opening browser tab")?
        .context("Failed to open new browser tab")
    }
}
