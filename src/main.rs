mod crawler;
mod load;
mod reporter;
mod types;

use anyhow::Result;
use chrono::Local;
use clap::{Parser, Subcommand};
use console::style;
use std::path::PathBuf;
use types::{AuthConfig, Report};

/// webprobe — Exhaustive web crawler & load tester for localhost development
#[derive(Parser)]
#[command(name = "webprobe", version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Crawl every route, audit for issues, then run a load test
    Crawl {
        /// Target: port number (3000), host:port (localhost:3000), or full URL
        url: String,

        /// Maximum link-follow depth
        #[arg(short, long, default_value_t = 5)]
        depth: usize,

        /// Path to write the JSON report (default: report-YYYYMMDD-HHMMSS.json)
        #[arg(short, long)]
        output: Option<PathBuf>,

        /// Number of concurrent users for load testing (0 = skip)
        #[arg(short, long, default_value_t = 1)]
        users: u32,

        /// Load test duration in seconds
        #[arg(long, default_value_t = 30)]
        duration: u64,

        /// Run browser in headed (visible) mode
        #[arg(long)]
        headed: bool,

        /// Milliseconds to wait for JS to settle after DOM ready
        #[arg(long, default_value_t = 300)]
        wait_ms: u64,

        /// Skip the load test phase
        #[arg(long)]
        no_load: bool,

        // ── Auth ──────────────────────────────────────────────────────────────
        /// Login page path or URL (e.g. /login or http://localhost:3000/login)
        #[arg(long, value_name = "URL")]
        auth_url: Option<String>,

        /// Username or email to fill into the login form
        #[arg(long, value_name = "USERNAME")]
        auth_username: Option<String>,

        /// Password to fill into the login form
        #[arg(long, value_name = "PASSWORD")]
        auth_password: Option<String>,

        /// CSS selector for the username/email input (auto-detected if omitted)
        #[arg(long, value_name = "SELECTOR")]
        auth_username_selector: Option<String>,

        /// CSS selector for the password input (auto-detected if omitted)
        #[arg(long, value_name = "SELECTOR")]
        auth_password_selector: Option<String>,

        /// CSS selector for the submit button (auto-detected if omitted)
        #[arg(long, value_name = "SELECTOR")]
        auth_submit_selector: Option<String>,

        /// Path to a JSON cookie file to inject (export from browser DevTools)
        #[arg(long, value_name = "FILE")]
        cookies: Option<PathBuf>,
    },

    /// Run a standalone load test
    Load {
        /// Target: port number (3000), host:port (localhost:3000), or full URL
        url: String,

        /// Number of concurrent users
        #[arg(short, long, default_value_t = 10)]
        users: u32,

        /// Duration in seconds
        #[arg(short, long, default_value_t = 30)]
        duration: u64,

        /// Path to write the JSON report (default: load-YYYYMMDD-HHMMSS.json)
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
}

/// Accept a port number, host:port, or full URL → always returns a full URL.
fn normalize_url(input: &str) -> String {
    let trimmed = input.trim();
    if trimmed.chars().all(|c| c.is_ascii_digit()) {
        return format!("http://localhost:{}", trimmed);
    }
    if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        return trimmed.to_string();
    }
    format!("http://{}", trimmed)
}

/// Return `path` if given, otherwise generate a unique timestamped filename.
fn resolve_output(path: Option<PathBuf>, prefix: &str) -> PathBuf {
    if let Some(p) = path {
        return p;
    }
    let ts = Local::now().format("%Y%m%d-%H%M%S");
    PathBuf::from(format!("{}-{}.json", prefix, ts))
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Crawl {
            url,
            depth,
            output,
            users,
            duration,
            headed,
            wait_ms,
            no_load,
            auth_url,
            auth_username,
            auth_password,
            auth_username_selector,
            auth_password_selector,
            auth_submit_selector,
            cookies,
        } => {
            let url = normalize_url(&url);
            let output = resolve_output(output, "report");

            println!(
                "\n  {} Crawling {}\n",
                style("webprobe").bold().cyan(),
                style(&url).underlined()
            );

            let auth = AuthConfig {
                login_url: auth_url,
                username: auth_username,
                password: auth_password,
                username_selector: auth_username_selector,
                password_selector: auth_password_selector,
                submit_selector: auth_submit_selector,
                cookies_file: cookies,
            };

            let crawler_config = crawler::CrawlerConfig {
                start_url: url.clone(),
                max_depth: depth,
                concurrency: 1,
                headless: !headed,
                settle_ms: wait_ms,
                auth,
            };

            let crawl_result = crawler::run_crawler(crawler_config).await?;

            let mut report = Report::new(&url);
            report.issues = crawl_result.issues;
            report.perf_metrics = crawl_result.perf_metrics;
            report.network_stats = crawl_result.network_stats;
            report.crawl_stats = crawl_result.stats;

            if !no_load && users > 0 {
                println!(
                    "\n  {} Running load test ({} user{}, {}s)…\n",
                    style("→").cyan(),
                    users,
                    if users == 1 { "" } else { "s" },
                    duration
                );
                let load_result = load::run_load_test(load::LoadConfig {
                    url: url.clone(),
                    users,
                    duration_secs: duration,
                })
                .await?;
                report.load_test = Some(load_result);
            }

            report.compute_summary();
            reporter::console::print_report(&report);

            reporter::json::write_report(&report, &output)?;
            println!(
                "  {} Report saved to {}\n",
                style("✓").green(),
                style(output.display()).underlined()
            );
        }

        Commands::Load {
            url,
            users,
            duration,
            output,
        } => {
            let url = normalize_url(&url);
            let output = resolve_output(output, "load");

            println!(
                "\n  {} Load testing {} ({} user{}, {}s)\n",
                style("webprobe").bold().cyan(),
                style(&url).underlined(),
                users,
                if users == 1 { "" } else { "s" },
                duration
            );

            let load_result = load::run_load_test(load::LoadConfig {
                url: url.clone(),
                users,
                duration_secs: duration,
            })
            .await?;

            let mut report = Report::new(&url);
            report.load_test = Some(load_result);
            report.compute_summary();
            reporter::console::print_report(&report);

            reporter::json::write_report(&report, &output)?;
            println!(
                "  {} Report saved to {}\n",
                style("✓").green(),
                style(output.display()).underlined()
            );
        }
    }

    Ok(())
}
