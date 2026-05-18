use anyhow::{Context, Result};
use console::style;
use std::net::TcpListener;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

/// Which WebDriver backend is running.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DriverKind {
    /// safaridriver — built into every macOS, no install needed.
    /// Does NOT support headless; browser window will be visible.
    Safari,
    /// geckodriver — requires Firefox to be installed.
    Gecko,
    /// chromedriver — requires Google Chrome to be installed.
    Chrome,
}

impl DriverKind {
    pub fn label(self) -> &'static str {
        match self {
            DriverKind::Safari => "Safari (safaridriver)",
            DriverKind::Gecko => "Firefox (geckodriver)",
            DriverKind::Chrome => "Chrome (chromedriver)",
        }
    }
}

/// Manages a WebDriver subprocess. Killed automatically on drop.
pub struct DriverProcess {
    child: Child,
    pub port: u16,
    pub kind: DriverKind,
}

impl DriverProcess {
    /// Pick the best available driver, trying not to require any new installs:
    ///   1. geckodriver + Firefox  (headless, full capability)
    ///   2. chromedriver + Chrome  (headless, full capability)
    ///   3. Install missing driver piece if the browser is already present
    ///   4. safaridriver            (built-in macOS, no install, no headless)
    ///      → also kicks off a background Chrome install for next run
    pub async fn detect_and_spawn() -> Result<Self> {
        let has_gecko = cmd_in_path("geckodriver");
        let has_firefox = firefox_installed();
        let has_chrome_driver = cmd_in_path("chromedriver");
        let has_chrome = chrome_installed();

        // ── Best case: geckodriver + Firefox both ready ───────────────────────
        if has_gecko && has_firefox {
            println!(
                "  {} Browser: {}",
                style("→").cyan(),
                style("Firefox (geckodriver)").bold()
            );
            return Self::spawn_gecko().await;
        }

        // ── Best case: chromedriver + Chrome both ready ───────────────────────
        if has_chrome_driver && has_chrome {
            println!(
                "  {} Browser: {}",
                style("→").cyan(),
                style("Chrome (chromedriver)").bold()
            );
            return Self::spawn_chrome().await;
        }

        // ── Firefox present but no geckodriver — install geckodriver (~2 MB) ──
        if has_firefox && !has_gecko {
            print!(
                "  {} geckodriver not found — installing… ",
                style("→").cyan()
            );
            if brew_install("geckodriver").await {
                println!("{}", style("ok").green().bold());
                println!(
                    "  {} Browser: {}",
                    style("→").cyan(),
                    style("Firefox (geckodriver)").bold()
                );
                return Self::spawn_gecko().await;
            }
            println!("{}", style("skipped").yellow());
            // Fall through to try Chrome or Safari
        }

        // ── Chrome present but no chromedriver — install chromedriver (~5 MB) ─
        if has_chrome && !has_chrome_driver {
            print!(
                "  {} chromedriver not found — installing… ",
                style("→").cyan()
            );
            if brew_install("chromedriver").await {
                println!("{}", style("ok").green().bold());
                println!(
                    "  {} Browser: {}",
                    style("→").cyan(),
                    style("Chrome (chromedriver)").bold()
                );
                return Self::spawn_chrome().await;
            }
            println!("{}", style("skipped").yellow());
            // Fall through to Safari
        }

        // ── No browser installed: fall back to safaridriver, install Chrome bg ─
        let safari_available =
            cmd_in_path("safaridriver") || std::path::Path::new("/usr/bin/safaridriver").exists();

        if safari_available {
            let missing = if !has_firefox && !has_chrome {
                "Firefox/Chrome not installed"
            } else {
                "driver not found"
            };
            println!(
                "  {} {} — using {} for now",
                style("→").cyan(),
                missing,
                style("Safari (safaridriver)").bold(),
            );
            println!(
                "  {} {} {}",
                style("│").cyan().dim(),
                style("Note:").yellow(),
                style("browser window will be visible (Safari has no headless mode)").dim()
            );
            println!(
                "  {} If this fails run: {}",
                style("│").cyan().dim(),
                style("safaridriver --enable").bold()
            );

            let driver = Self::spawn_safari().await?;

            // Install Chrome in the background — silently, don't block crawl.
            tokio::spawn(async {
                if brew_install_cask("google-chrome").await {
                    println!(
                        "\n  {} Chrome installed — next run will use headless Chrome",
                        style("✓").green()
                    );
                }
            });

            return Ok(driver);
        }

        // ── Last resort: try to install Firefox + geckodriver synchronously ───
        print!(
            "  {} No browser found — installing Firefox… ",
            style("→").cyan()
        );
        if brew_install_cask("firefox").await {
            println!("{}", style("ok").green().bold());
            print!("  {} Installing geckodriver… ", style("→").cyan());
            if brew_install("geckodriver").await {
                println!("{}", style("ok").green().bold());
                println!(
                    "  {} Browser: {}",
                    style("→").cyan(),
                    style("Firefox (geckodriver)").bold()
                );
                return Self::spawn_gecko().await;
            }
            println!("{}", style("failed").red().bold());
        } else {
            println!("{}", style("failed").red().bold());
        }

        anyhow::bail!(
            "No supported browser found.\n\
             Fastest fix — uses Safari which is already on your Mac:\n\
             \t• safaridriver --enable\n\n\
             For headless support (pick one):\n\
             \t• brew install --cask google-chrome && brew install chromedriver\n\
             \t• brew install --cask firefox      && brew install geckodriver"
        )
    }

    // ── Spawn helpers ─────────────────────────────────────────────────────────

    async fn spawn_gecko() -> Result<Self> {
        let port = Self::free_port()?;
        let child = Command::new("geckodriver")
            .args(["--port", &port.to_string()])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| "Failed to start geckodriver — run: brew install geckodriver")?;
        Self::wait_ready(child, port, DriverKind::Gecko, "geckodriver").await
    }

    async fn spawn_chrome() -> Result<Self> {
        let port = Self::free_port()?;
        let child = Command::new("chromedriver")
            .args(["--port", &port.to_string()])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| "Failed to start chromedriver — run: brew install chromedriver")?;
        Self::wait_ready(child, port, DriverKind::Chrome, "chromedriver").await
    }

    async fn spawn_safari() -> Result<Self> {
        let port = Self::free_port()?;
        let safari_driver = if std::path::Path::new("/usr/bin/safaridriver").exists() {
            "/usr/bin/safaridriver"
        } else {
            "safaridriver"
        };
        let child = Command::new(safari_driver)
            .args(["--port", &port.to_string()])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| {
                "Failed to start safaridriver.\n\
                 Enable it with: safaridriver --enable"
            })?;
        Self::wait_ready(child, port, DriverKind::Safari, "safaridriver").await
    }

    async fn wait_ready(child: Child, port: u16, kind: DriverKind, name: &str) -> Result<Self> {
        let status_url = format!("http://127.0.0.1:{}/status", port);
        let poll_client = reqwest::Client::builder()
            .timeout(Duration::from_secs(2))
            .build()?;
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        loop {
            if std::time::Instant::now() > deadline {
                anyhow::bail!("{} did not become ready within 20s", name);
            }
            match poll_client.get(&status_url).send().await {
                Ok(resp) if resp.status().is_success() => break,
                _ => {}
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        Ok(Self { child, port, kind })
    }

    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    fn free_port() -> Result<u16> {
        let l = TcpListener::bind("127.0.0.1:0").context("Could not bind to a free port")?;
        Ok(l.local_addr()?.port())
    }
}

impl Drop for DriverProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

// ── Detection helpers ─────────────────────────────────────────────────────────

fn cmd_in_path(cmd: &str) -> bool {
    Command::new("which")
        .arg(cmd)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn firefox_installed() -> bool {
    std::path::Path::new("/Applications/Firefox.app/Contents/MacOS/firefox").exists()
        || std::path::Path::new("/usr/bin/firefox").exists()
        || cmd_in_path("firefox")
}

fn chrome_installed() -> bool {
    std::path::Path::new("/Applications/Google Chrome.app/Contents/MacOS/Google Chrome").exists()
        || std::path::Path::new("/Applications/Chromium.app/Contents/MacOS/Chromium").exists()
        || cmd_in_path("google-chrome")
        || cmd_in_path("chromium")
}

async fn brew_install(package: &str) -> bool {
    if !cmd_in_path("brew") {
        return false;
    }
    tokio::process::Command::new("brew")
        .args(["install", package])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
        .map(|s| s.success())
        .unwrap_or(false)
}

async fn brew_install_cask(package: &str) -> bool {
    if !cmd_in_path("brew") {
        return false;
    }
    tokio::process::Command::new("brew")
        .args(["install", "--cask", package])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
        .map(|s| s.success())
        .unwrap_or(false)
}
