mod crawler;
mod load;
mod profiles;
mod reporter;
mod types;

use anyhow::Result;
use chrono::Local;
use clap::{Parser, Subcommand};
use console::style;
use profiles::{AuthProfile, ProfileStore};
use std::ffi::OsStr;
use std::fs;
use std::io::{self, Write};
use std::mem;
use std::path::Path;
use std::path::PathBuf;
use types::{AuthConfig, Issue, IssueCategory, Report, Severity, deduplicate_issues};

/// webprobe — Exhaustive web crawler & load tester for localhost SPAs
///
/// QUICK START
///   webprobe crawl 3000
///   webprobe crawl localhost:3000 --depth 10 --users 20 --no-load
///
/// AUTHENTICATION
///   When login is required, a browser window opens for you to log in manually.
///   Your credentials and selectors are auto-captured and saved as a profile.
///
///   Use a saved profile:
///     webprobe crawl 3000 --profile myapp
///
/// SAVED PROFILES
///   Reuse credentials across runs:
///     webprobe profile add
///     webprobe crawl 3000     ← prompted to load a profile
#[derive(Parser)]
#[command(name = "webprobe", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Crawl every route, audit for issues, then run a load test
    ///
    /// AUTHENTICATION
    ///   When login is required, a browser window opens for you to log in manually.
    ///   Your credentials and form selectors are automatically captured and saved as a profile.
    ///
    ///   Use a saved profile:
    ///     webprobe crawl 3000 --profile myapp
    Crawl {
        /// Target: port number (3000), host:port (localhost:3000), or full URL
        url: String,

        /// Maximum BFS link-follow depth (default covers most SPAs)
        #[arg(short, long, default_value_t = 10)]
        depth: usize,

        /// Path to write the report (default: report-YYYYMMDD-HHMMSS with .json or .msgpack)
        #[arg(short, long)]
        output: Option<PathBuf>,

        /// Number of concurrent users for load testing (0 = skip)
        #[arg(short, long, default_value_t = 1)]
        users: u32,

        /// Load test duration in seconds
        #[arg(long, default_value_t = 30)]
        duration: u64,

        /// Number of concurrent browser tabs for crawling
        #[arg(short = 'c', long, default_value_t = 4)]
        concurrency: usize,

        /// Run browser in headed (visible) mode
        #[arg(long)]
        headed: bool,

        /// Milliseconds to wait for JS to settle after DOM ready (default: 200ms)
        #[arg(long, default_value_t = 200)]
        wait_ms: u64,

        /// Skip the load test phase
        #[arg(long)]
        no_load: bool,

        /// Increase verbosity (show info-level issues, more page samples)
        #[arg(long, short = 'v')]
        verbose: bool,

        /// Write three separate report files: (basename).crawl.json (crawl issues),
        /// (basename).pentest.json (security issues only), and (basename).load.json
        /// (load test). The combined report is still printed to the console.
        #[arg(long)]
        separate_reports: bool,


        /// Disable penetration testing checks (enabled by default).
        /// Run comprehensive security audit: security headers, cookie security, CORS,
        /// CSRF, JWT analysis, IDOR, SQL injection, XSS, open redirect, directory
        /// traversal, SSTI, SSRF, file upload risks, and mixed content detection.
        #[arg(long)]
        no_pentest: bool,

        /// URL paths to skip, comma-separated (each must start with /).
        /// The crawler will not visit — or follow any link to — any of these paths.
        /// Example: --skip /admin,/logout
        #[arg(long, value_name = "PATHS")]
        skip: Option<String>,

        /// CSS selector to scope link discovery.
        /// Only links inside the first matching element are followed.
        /// Example: --selector "nav"
        #[arg(long, value_name = "SELECTOR")]
        selector: Option<String>,

        /// Load a saved auth profile by name.
        /// Run `webprobe profile list` to see saved profiles.
        #[arg(long, value_name = "NAME")]
        profile: Option<String>,

        /// Output format for the report: json (default) or msgpack (smaller binary)
        #[arg(long, default_value = "msgpack")]
        format: String,

        /// Minimum severity level to include in the report (info, warning, error, critical).
        /// Defaults to "info" (all issues included). Higher levels exclude lower severities.
        #[arg(long, default_value = "info")]
        min_severity: String,

        /// Maximum number of discovered URLs to include in the report (default: unlimited).
        /// Truncates the discovered_urls list to prevent huge files on large sites.
        #[arg(long)]
        max_discovered: Option<usize>,

        /// Maximum number of page URL samples to include per issue (default: 20, or 100 with --verbose).
        /// Overrides the default truncation when specified.
        #[arg(long)]
        max_issue_urls: Option<usize>,

        /// Maximum total number of issues to include in the report (default: unlimited).
        /// Issues are sorted by severity and impact, so the most important are kept.
        #[arg(long)]
        max_total_issues: Option<usize>,
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

        /// Path to write the report (default: load-YYYYMMDD-HHMMSS with .json or .msgpack)
        #[arg(short, long)]
        output: Option<PathBuf>,

        /// Output format: json or msgpack (default: msgpack)
        #[arg(long, default_value = "msgpack")]
        format: String,
    },

    /// Manage saved authentication profiles
    Profile {
        #[command(subcommand)]
        action: ProfileAction,
    },
}

#[derive(Subcommand)]
enum ProfileAction {
    /// List all saved profiles
    List,
    /// Create a new auth profile interactively
    Add,
    /// Edit a saved profile
    Edit {
        /// Profile name to edit
        name: String,
    },
    /// Delete a saved profile
    Delete {
        /// Profile name to delete
        name: String,
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

/// Parse a comma-separated list of skip paths, enforcing a leading `/`.
/// Invalid entries (missing leading slash) are silently dropped with a warning.
fn parse_skip_paths(input: &str) -> Vec<String> {
    input
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| {
            if s.is_empty() {
                return false;
            }
            if !s.starts_with('/') {
                eprintln!(
                    "  {} Skipping invalid path {:?} — must start with /",
                    console::style("⚠").yellow(),
                    s
                );
                return false;
            }
            true
        })
        .collect()
}

fn save_profile(name: &str, login_url: Option<String>, captured: &types::AuthConfig) {
    let new_profile = AuthProfile {
        name: name.to_string(),
        login_url,
        username: captured.username.clone(),
        password: captured.password.clone(),
        username_selector: captured.username_selector.clone(),
        password_selector: captured.password_selector.clone(),
        submit_selector: captured.submit_selector.clone(),
    };
    match ProfileStore::load() {
        Ok(mut store) => {
            store.upsert(new_profile);
            if let Err(e) = store.save() {
                eprintln!("  {} Could not save profile: {}", style("⚠").yellow(), e);
            } else {
                println!(
                    "  {} Auth profile '{}' saved.",
                    style("✓").green(),
                    name
                );
            }
        }
        Err(e) => {
            eprintln!("  {} Could not load profiles: {}", style("⚠").yellow(), e);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OutputFormat {
    Json,
    Msgpack,
}

impl OutputFormat {
    fn parse(s: &str) -> Result<Self> {
        match s.to_lowercase().as_str() {
            "json" => Ok(OutputFormat::Json),
            "msgpack" => Ok(OutputFormat::Msgpack),
            _ => anyhow::bail!("Unsupported output format: {}. Use 'json' or 'msgpack'.", s),
        }
    }
}

/// Parse severity level from string (info, warning, error, critical)
fn parse_severity(s: &str) -> Result<Severity> {
    match s.to_lowercase().as_str() {
        "info" => Ok(Severity::Info),
        "warning" => Ok(Severity::Warning),
        "warn" => Ok(Severity::Warning),
        "error" => Ok(Severity::Error),
        "critical" => Ok(Severity::Critical),
        "crit" => Ok(Severity::Critical),
        _ => anyhow::bail!("Invalid severity '{}'. Use: info, warning, error, critical", s),
    }
}

/// Return `path` if given, otherwise generate a unique timestamped filename.
fn resolve_output(path: Option<PathBuf>, prefix: &str, format: OutputFormat) -> PathBuf {
    if let Some(p) = path {
        return p;
    }
    let ts = Local::now().format("%Y%m%d-%H%M%S");
    let ext = match format {
        OutputFormat::Json => "json",
        OutputFormat::Msgpack => "msgpack",
    };
    PathBuf::from(format!("{}-{}.{}", prefix, ts, ext))
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
            concurrency,
            headed,
            wait_ms,
            no_load,
            verbose,
            separate_reports,
            no_pentest,
            skip,
            selector,
            profile,
            format,
            min_severity,
            max_discovered,
            max_issue_urls,
            max_total_issues,
        } => {
            let url = normalize_url(&url);
            let format_enum = OutputFormat::parse(&format)?;
            let explicit_output = output.clone();
            let output = resolve_output(output, "report", format_enum);

            // ── Skip paths ────────────────────────────────────────────────────
            let skip_paths: Vec<String> = match skip {
                Some(s) => parse_skip_paths(&s),
                None => {
                    println!();
                    println!(
                        "  {}  {}",
                        style("◆").cyan().bold(),
                        style("Pages to skip").bold()
                    );
                    println!(
                        "  {}  Comma-separated paths the crawler should never visit.",
                        style("│").cyan().dim()
                    );
                    println!(
                        "  {}  Each must start with  {}  (e.g. {}).",
                        style("│").cyan().dim(),
                        style("/").yellow(),
                        style("/admin,/logout").dim()
                    );
                    print!(
                        "  {}  {} ",
                        style("└").cyan().dim(),
                        style("Skip paths (Enter = none):").dim()
                    );
                    io::stdout().flush().ok();
                    let mut input = String::new();
                    io::stdin().read_line(&mut input).ok();
                    let trimmed = input.trim().to_string();
                    if trimmed.is_empty() {
                        vec![]
                    } else {
                        parse_skip_paths(&trimmed)
                    }
                }
            };

            // ── Authentication ────────────────────────────────────────────────
            let mut auth_url: Option<String> = None;
            let mut auth_username: Option<String> = None;
            let mut auth_password: Option<String> = None;
            let mut auth_username_selector: Option<String> = None;
            let mut auth_password_selector: Option<String> = None;
            let mut auth_submit_selector: Option<String> = None;

            // Apply --profile defaults (explicit flags take precedence)
            if let Some(profile_name) = &profile {
                let store = ProfileStore::load()?;
                if let Some(p) = store.get(profile_name) {
                    if auth_url.is_none() {
                        auth_url = p.login_url.clone();
                    }
                    if auth_username.is_none() {
                        auth_username = p.username.clone();
                    }
                    if auth_password.is_none() {
                        auth_password = p.password.clone();
                    }
                    if auth_username_selector.is_none() {
                        auth_username_selector = p.username_selector.clone();
                    }
                    if auth_password_selector.is_none() {
                        auth_password_selector = p.password_selector.clone();
                    }
                    if auth_submit_selector.is_none() {
                        auth_submit_selector = p.submit_selector.clone();
                    }
                    println!(
                        "  {}  Using profile: {}",
                        style("→").cyan(),
                        style(profile_name).bold()
                    );
                } else {
                    println!(
                        "  {}  Profile {:?} not found — continuing without it.",
                        style("⚠").yellow(),
                        profile_name
                    );
                }
            }

            // Only ask about auth if no profile is loaded.
            if auth_url.is_none() {
                println!();
                println!(
                    "  {}  {}",
                    style("◆").cyan().bold(),
                    style("Authentication").bold()
                );
                // Offer to load a saved profile if any exist
                let store_for_load = ProfileStore::load().unwrap_or_default();
                if !store_for_load.is_empty() {
                    let profiles_list = store_for_load.list();
                    println!();
                    println!(
                        "  {}  {}",
                        style("◆").cyan().bold(),
                        style("Saved profiles").bold()
                    );
                    for (i, p) in profiles_list.iter().enumerate() {
                        println!(
                            "  {}  {}  {}  {}",
                            style("│").cyan().dim(),
                            style(format!("[{}]", i + 1)).yellow(),
                            style(&p.name).bold(),
                            p.login_url.as_deref().unwrap_or(""),
                        );
                    }
                    print!(
                        "  {}  {} ",
                        style("└").cyan().dim(),
                        style("Load a profile? Enter number or name (Enter to skip):").dim()
                    );
                    io::stdout().flush().ok();
                    let mut pick = String::new();
                    io::stdin().read_line(&mut pick).ok();
                    let pick = pick.trim().to_string();
                    if !pick.is_empty() {
                        let found = if let Ok(n) = pick.parse::<usize>() {
                            profiles_list.get(n.saturating_sub(1)).copied()
                        } else {
                            store_for_load.get(&pick)
                        };
                        if let Some(p) = found {
                            auth_url = auth_url.or_else(|| p.login_url.clone());
                            auth_username = auth_username.or_else(|| p.username.clone());
                            auth_password = auth_password.or_else(|| p.password.clone());
                            auth_username_selector =
                                auth_username_selector.or_else(|| p.username_selector.clone());
                            auth_password_selector =
                                auth_password_selector.or_else(|| p.password_selector.clone());
                            auth_submit_selector =
                                auth_submit_selector.or_else(|| p.submit_selector.clone());
                            println!(
                                "  {}  Loaded profile: {}",
                                style("✓").green(),
                                style(&p.name).bold()
                            );
                        }
                    }
                }

                // Only ask login questions if a profile didn't already fill everything in.
                let profile_loaded = auth_url.is_some();
                if !profile_loaded {
                    print!(
                        "  {}  {} ",
                        style("└").cyan().dim(),
                        style("Does this site require login? [y/N]:").dim()
                    );
                    io::stdout().flush().ok();
                    let mut input = String::new();
                    io::stdin().read_line(&mut input).ok();

                    if input.trim().eq_ignore_ascii_case("y") {
                        // Get login URL from user
                        println!();
                        println!(
                            "  {}  {}",
                            style("◆").cyan().bold(),
                            style("Login details").bold()
                        );

                        // Login URL (required)
                        loop {
                            print!(
                                "  {}  {} ",
                                style("├").cyan().dim(),
                                style("Login page path or full URL (e.g. /login):").dim()
                            );
                            io::stdout().flush().ok();
                            let mut url_input = String::new();
                            io::stdin().read_line(&mut url_input).ok();
                            let trimmed = url_input.trim().to_string();
                            if !trimmed.is_empty() {
                                auth_url = Some(trimmed);
                                break;
                            }
                            eprintln!("  {}  Login URL is required.", style("⚠").yellow());
                        }

                        // For manual login, we'll use empty credentials so perform_login
                        // doesn't auto-fill anything, allowing the user to type manually
                        auth_username = None;
                        auth_password = None;
                        auth_username_selector = None;
                        auth_password_selector = None;
                        auth_submit_selector = None;
                    }
                } // end if !profile_loaded
            }

            // For manual login, force headed mode so user can interact with the browser
            let manual_login = auth_url.is_some() && auth_username.is_none() && auth_password.is_none();
            let is_headed = headed || manual_login;

            // ── Summary line before crawling ──────────────────────────────────
            println!();
            if !skip_paths.is_empty() {
                println!(
                    "  {}  Skipping: {}",
                    style("→").cyan(),
                    style(skip_paths.join(", ")).yellow()
                );
            }
            if let Some(sel) = &selector {
                println!("  {}  Link scope: {}", style("→").cyan(), style(sel).bold());
            }

            println!(
                "\n  {} Crawling {}\n",
                style("webprobe").bold().cyan(),
                style(&url).underlined()
            );

            let auth_login_url = auth_url.clone();

            let auth = AuthConfig {
                login_url: auth_url,
                username: auth_username,
                password: auth_password,
                username_selector: auth_username_selector,
                password_selector: auth_password_selector,
                submit_selector: auth_submit_selector,
                cookies_file: None,
            };

            let crawler_config = crawler::CrawlerConfig {
                start_url: url.clone(),
                max_depth: depth,
                concurrency,
                headless: !is_headed,
                settle_ms: wait_ms,
                auth,
                skip_paths,
                link_selector: selector,
                pentest: !no_pentest,
            };

            let crawl_result = crawler::run_crawler(crawler_config).await?;

            // Auto-save captured auth as profile after successful manual login
            if let Some(ref captured) = crawl_result.captured_auth {
                if captured.username.is_some() || captured.password.is_some() || captured.username_selector.is_some() {
                    println!();
                    print!(
                        "  {}  {} ",
                        style("◆").cyan().dim(),
                        style("Save as profile? Enter name (or press Enter for 'default'):").dim()
                    );
                    io::stdout().flush().ok();
                    let mut name_input = String::new();
                    io::stdin().read_line(&mut name_input).ok();
                    let profile_name = name_input.trim().to_string();
                    
                    let final_name = if profile_name.is_empty() { "default".to_string() } else { profile_name.clone() };
                    
                    // Check if profile already exists and ask to overwrite
                    if let Ok(store) = ProfileStore::load() {
                        if store.get(&final_name).is_some() && !profile_name.is_empty() {
                            print!(
                                "  {}  Profile '{}' exists. Overwrite? [y/N]: ",
                                style("│").cyan().dim(),
                                final_name
                            );
                            io::stdout().flush().ok();
                            let mut confirm = String::new();
                            io::stdin().read_line(&mut confirm).ok();
                            if !confirm.trim().eq_ignore_ascii_case("y") {
                                println!("  {} Profile not saved.", style("→").cyan());
                            } else {
                                save_profile(&final_name, auth_login_url.clone(), captured);
                            }
                        } else {
                            save_profile(&final_name, auth_login_url.clone(), captured);
                        }
                    } else {
                        save_profile(&final_name, auth_login_url.clone(), captured);
                    }
                }
            }

            let mut report = Report::new(&url);
            report.issues = crawl_result.issues;

            // Determine max_pages for issue URL samples
            let max_pages_for_issues = max_issue_urls.unwrap_or_else(|| if verbose { 100 } else { 20 });

            // Deduplicate issues (merges duplicates, truncates page samples)
            report.issues = deduplicate_issues(report.issues, max_pages_for_issues);

            // Filter by minimum severity level
            let min_sev = parse_severity(&min_severity)?;
            report.issues.retain(|issue| issue.severity >= min_sev);

            // Apply total issue limit if specified (keeps most severe due to deduplication sorting)
            if let Some(limit) = max_total_issues {
                report.issues.truncate(limit);
            }

            report.crawl_stats = crawl_result.stats;

            // Skip collecting per-page performance, network, and interaction details
            // to keep the JSON report compact. Only essential stats and issues are retained.

            let discovered = {
                let mut urls = crawl_result.discovered_urls;
                urls.sort();
                urls.dedup();
                urls
            };
            report.discovered_urls = discovered.clone();

            // Apply max_discovered limit if specified (truncate both lists)
            if let Some(limit) = max_discovered {
                report.discovered_urls.truncate(limit);
                report.crawl_stats.crawled_urls.truncate(limit);
            }

            if !no_load && users > 0 {
                let load_urls = if discovered.is_empty() {
                    vec![url.clone()]
                } else {
                    discovered
                };
                let url_msg = if load_urls.len() > 1 {
                    format!("{} URLs, ", load_urls.len())
                } else {
                    String::new()
                };
                println!(
                    "\n  {} Running load test ({}{}s, {} user{})…\n",
                    style("→").cyan(),
                    url_msg,
                    duration,
                    users,
                    if users == 1 { "" } else { "s" },
                );
                let load_result = load::run_load_test(load::LoadConfig {
                    urls: load_urls,
                    users,
                    duration_secs: duration,
                })
                .await?;
                report.load_test = Some(load_result);
            }

            report.compute_summary();

            if separate_reports {
                // Print combined report to console (same as non-separate mode)
                let console_output = reporter::console::format_report(&report);
                println!("{}", console_output);

                // Separate issues by category: Security issues (pentest) vs others (crawl)
                let (crawl_issues, pentest_issues): (Vec<Issue>, Vec<Issue>) = report.issues
                    .into_iter()
                    .partition(|issue| issue.category != IssueCategory::Security);

                // ── Build crawl report ──
                let mut crawl_report = Report::new(&url);
                crawl_report.issues = crawl_issues;
                crawl_report.crawl_stats = mem::take(&mut report.crawl_stats);
                crawl_report.discovered_urls = mem::take(&mut report.discovered_urls);
                crawl_report.compute_summary();
                // Remove verbose data to keep compact
                crawl_report.crawl_stats.crawled_urls.clear();
                crawl_report.discovered_urls.clear();

                // ── Build pentest report ──
                let mut pentest_report = Report::new(&url);
                pentest_report.issues = pentest_issues;
                pentest_report.compute_summary();

                // ── Determine output paths ──
                // Helper to generate path: if user provided output, insert type before extension; else use timestamped prefix
                let get_output_path = |suffix: &str| -> PathBuf {
                    if let Some(ref p) = explicit_output {
                        // Insert suffix before extension: e.g., /path/report.json -> /path/report.crawl.json
                        let parent = p.parent().unwrap_or_else(|| Path::new("."));
                        let stem = p.file_stem()
                            .and_then(|s: &OsStr| s.to_str())
                            .unwrap_or("report");
                        let ext = p.extension()
                            .and_then(|s: &OsStr| s.to_str())
                            .unwrap_or_else(|| match format_enum {
                                OutputFormat::Json => "json",
                                OutputFormat::Msgpack => "msgpack",
                            });
                        parent.join(format!("{}.{}.{}", stem, suffix, ext))
                    } else {
                        resolve_output(None, suffix, format_enum)
                    }
                };

                let crawl_path = get_output_path("crawl");
                let pentest_path = get_output_path("pentest");

                // ── Write crawl report ──
                match format_enum {
                    OutputFormat::Json => reporter::json::write_report(&crawl_report, &crawl_path)?,
                    OutputFormat::Msgpack => reporter::msgpack::write_report(&crawl_report, &crawl_path)?,
                }
                let crawl_txt = crawl_path.with_extension("txt");
                fs::write(&crawl_txt, reporter::console::format_report(&crawl_report))?;

                // ── Write pentest report ──
                match format_enum {
                    OutputFormat::Json => reporter::json::write_report(&pentest_report, &pentest_path)?,
                    OutputFormat::Msgpack => reporter::msgpack::write_report(&pentest_report, &pentest_path)?,
                }
                let pentest_txt = pentest_path.with_extension("txt");
                fs::write(&pentest_txt, reporter::console::format_report(&pentest_report))?;

                // ── Load report (if present) ──
                if report.load_test.is_some() {
                    let mut load_report = Report::new(&url);
                    let load_test = report.load_test.take().unwrap();
                    load_report.load_test = Some(load_test);
                    load_report.compute_summary();
                    let load_path = get_output_path("load");
                    match format_enum {
                        OutputFormat::Json => reporter::json::write_report(&load_report, &load_path)?,
                        OutputFormat::Msgpack => reporter::msgpack::write_report(&load_report, &load_path)?,
                    }
                    let load_txt = load_path.with_extension("txt");
                    fs::write(&load_txt, reporter::console::format_report(&load_report))?;

                    println!(
                        "  {} Separate reports saved to:\n     {} (crawl)\n     {} (pentest)\n     {} (load)",
                        style("✓").green(),
                        style(crawl_path.display()).underlined(),
                        style(pentest_path.display()).underlined(),
                        style(load_path.display()).underlined()
                    );
                } else {
                    println!(
                        "  {} Separate reports saved to:\n     {} (crawl)\n     {} (pentest)",
                        style("✓").green(),
                        style(crawl_path.display()).underlined(),
                        style(pentest_path.display()).underlined()
                    );
                }
            } else {
                // Remove verbose data to keep reports compact
                report.crawl_stats.crawled_urls.clear();
                report.discovered_urls.clear();

                // Generate compact console output
                let console_output = reporter::console::format_report(&report);
                println!("{}", console_output);

                // Write report in selected format
                match format_enum {
                    OutputFormat::Json => reporter::json::write_report(&report, &output)?,
                    OutputFormat::Msgpack => reporter::msgpack::write_report(&report, &output)?,
                }

                // Write TXT report (same basename, .txt extension)
                let txt_path = output.with_extension("txt");
                fs::write(&txt_path, &console_output)?;

                println!(
                    "  {} Reports saved to {} and {}\n",
                    style("✓").green(),
                    style(output.display()).underlined(),
                    style(txt_path.display()).underlined()
                );
            }
        }

        Commands::Load {
            url,
            users,
            duration,
            output,
            format,
        } => {
            let url = normalize_url(&url);
            let format_enum = OutputFormat::parse(&format)?;
            let output = resolve_output(output, "load", format_enum);

            println!(
                "\n  {} Load testing {} ({} user{}, {}s)\n",
                style("webprobe").bold().cyan(),
                style(&url).underlined(),
                users,
                if users == 1 { "" } else { "s" },
                duration
            );

            let load_result = load::run_load_test(load::LoadConfig {
                urls: vec![url.clone()],
                users,
                duration_secs: duration,
            })
            .await?;

            let mut report = Report::new(&url);
            report.load_test = Some(load_result);
            report.compute_summary();

            // Remove verbose data to keep reports compact
            report.crawl_stats.crawled_urls.clear();
            report.discovered_urls.clear();

            // Generate compact console output
            let console_output = reporter::console::format_report(&report);
            println!("{}", console_output);

            // Write report in selected format
            match format_enum {
                OutputFormat::Json => reporter::json::write_report(&report, &output)?,
                OutputFormat::Msgpack => reporter::msgpack::write_report(&report, &output)?,
            }

            // Write TXT report (same basename, .txt extension)
            let txt_path = output.with_extension("txt");
            fs::write(&txt_path, &console_output)?;

            println!(
                "  {} Reports saved to {} and {}\n",
                style("✓").green(),
                style(output.display()).underlined(),
                style(txt_path.display()).underlined()
            );
        }

        Commands::Profile { action } => {
            match action {
                ProfileAction::List => {
                    let store = ProfileStore::load()?;
                    let profiles = store.list();
                    if profiles.is_empty() {
                        println!("\n  {} No saved profiles.\n", style("→").cyan());
                    } else {
                        println!("\n  {} Saved profiles:\n", style("◆").cyan().bold());
                        let last_idx = profiles.len().saturating_sub(1);
                        for (i, p) in profiles.iter().enumerate() {
                            println!(
                                "  {}  {:<24}{}",
                                style("│").cyan().dim(),
                                style(&p.name).bold(),
                                p.login_url.as_deref().unwrap_or("")
                            );
                            if let Some(u) = &p.username {
                                println!("  {}    username:  {}", style("│").cyan().dim(), u);
                            }
                            let pwd_status = if p.password.is_some() {
                                "(saved)"
                            } else {
                                "(none)"
                            };
                            println!("  {}    password:  {}", style("│").cyan().dim(), pwd_status);
                            if let Some(s) = &p.username_selector {
                                println!("  {}    user sel:  {}", style("│").cyan().dim(), s);
                            }
                            if let Some(s) = &p.password_selector {
                                println!("  {}    pass sel:  {}", style("│").cyan().dim(), s);
                            }
                            let submit_val = p.submit_selector.as_deref().unwrap_or("(none)");
                            println!("  {}    submit:    {}", style("│").cyan().dim(), submit_val);
                            if i < last_idx {
                                println!("  {}", style("│").cyan().dim());
                            }
                        }
                        println!();
                    }
                }
                ProfileAction::Add => {
                    print!("\n  {}  Profile name: ", style("◆").cyan().bold());
                    io::stdout().flush().ok();
                    let mut profile_name = String::new();
                    io::BufRead::read_line(&mut io::stdin().lock(), &mut profile_name).ok();
                    let profile_name = profile_name.trim().to_string();
                    if profile_name.is_empty() {
                        println!(
                            "\n  {} Profile name cannot be empty.\n",
                            style("⚠").yellow()
                        );
                    } else {
                        let mut store = ProfileStore::load()?;
                        if store.get(&profile_name).is_some() {
                            print!(
                                "  {}  Profile {:?} already exists. Overwrite? [y/N]: ",
                                style("│").cyan().dim(),
                                profile_name
                            );
                            io::stdout().flush().ok();
                            let mut confirm = String::new();
                            io::BufRead::read_line(&mut io::stdin().lock(), &mut confirm).ok();
                            if confirm.trim().to_lowercase() != "y" {
                                println!("\n  {} Aborted.\n", style("→").cyan());
                                return Ok(());
                            }
                        }

                        println!(
                            "\n  {}  Creating profile: {}\n",
                            style("◆").cyan().bold(),
                            style(&profile_name).bold()
                        );

                        fn prompt_new(label: &str) -> Option<String> {
                            print!(
                                "  {}  {}: ",
                                console::style("├").cyan().dim(),
                                console::style(label).dim(),
                            );
                            std::io::Write::flush(&mut std::io::stdout()).ok();
                            let mut buf = String::new();
                            std::io::BufRead::read_line(&mut std::io::stdin().lock(), &mut buf)
                                .ok();
                            let v = buf.trim().to_string();
                            if v.is_empty() { None } else { Some(v) }
                        }

                        let login_url = prompt_new("Login URL         ");
                        let username = prompt_new("Username          ");

                        print!(
                            "  {}  {}: ",
                            style("├").cyan().dim(),
                            style("Password          ").dim(),
                        );
                        io::stdout().flush().ok();
                        let pwd_input = rpassword::read_password().unwrap_or_default();
                        let password = if pwd_input.is_empty() {
                            None
                        } else {
                            Some(pwd_input)
                        };

                        let username_selector = prompt_new("Username selector ");
                        let password_selector = prompt_new("Password selector ");
                        let submit_selector = prompt_new("Submit selector   ");

                        let new_profile = AuthProfile {
                            name: profile_name.clone(),
                            login_url,
                            username,
                            password,
                            username_selector,
                            password_selector,
                            submit_selector,
                        };
                        store.upsert(new_profile);
                        store.save()?;
                        println!(
                            "\n  {}  Profile {:?} saved.\n",
                            style("✓").green(),
                            profile_name
                        );
                    }
                }
                ProfileAction::Delete { name } => {
                    let mut store = ProfileStore::load()?;
                    if store.delete(&name) {
                        store.save()?;
                        println!("\n  {} Profile {:?} deleted.\n", style("✓").green(), name);
                    } else {
                        println!("\n  {} No profile named {:?}.\n", style("⚠").yellow(), name);
                    }
                }
                ProfileAction::Edit { name } => {
                    let mut store = ProfileStore::load()?;
                    let existing = store.get(&name).cloned();
                    match existing {
                        None => {
                            println!("\n  {} No profile named {:?}.\n", style("⚠").yellow(), name)
                        }
                        Some(p) => {
                            println!(
                                "\n  {}  Editing profile: {}\n",
                                style("◆").cyan().bold(),
                                style(&p.name).bold()
                            );
                            println!(
                                "  {}  Press Enter to keep the current value, or {} to clear a field.",
                                style("│").cyan().dim(),
                                style("-").yellow()
                            );
                            println!();

                            fn prompt_optional(
                                label: &str,
                                current: &Option<String>,
                            ) -> Option<String> {
                                let cur = current.as_deref().unwrap_or("(none)");
                                print!(
                                    "  {}  {} [{}]: ",
                                    console::style("├").cyan().dim(),
                                    console::style(label).dim(),
                                    console::style(cur).yellow()
                                );
                                std::io::Write::flush(&mut std::io::stdout()).ok();
                                let mut buf = String::new();
                                std::io::BufRead::read_line(&mut std::io::stdin().lock(), &mut buf)
                                    .ok();
                                let v = buf.trim().to_string();
                                if v.is_empty() {
                                    current.clone()
                                } else if v == "-" {
                                    None
                                } else {
                                    Some(v)
                                }
                            }

                            let login_url = prompt_optional("Login URL         ", &p.login_url);
                            let username = prompt_optional("Username          ", &p.username);

                            // Password — hidden input
                            print!(
                                "  {}  {} [{}]: ",
                                style("├").cyan().dim(),
                                style("Password          ").dim(),
                                style(if p.password.is_some() {
                                    "(saved)"
                                } else {
                                    "(none)"
                                })
                                .yellow()
                            );
                            io::stdout().flush().ok();
                            let new_pwd = rpassword::read_password().unwrap_or_default();
                            let password = if new_pwd.is_empty() {
                                p.password.clone()
                            } else if new_pwd == "-" {
                                None
                            } else {
                                Some(new_pwd)
                            };

                            let username_selector =
                                prompt_optional("Username selector ", &p.username_selector);
                            let password_selector =
                                prompt_optional("Password selector ", &p.password_selector);
                            let submit_selector =
                                prompt_optional("Submit selector   ", &p.submit_selector);

                            let updated = AuthProfile {
                                name: p.name.clone(),
                                login_url,
                                username,
                                password,
                                username_selector,
                                password_selector,
                                submit_selector,
                            };
                            store.upsert(updated);
                            store.save()?;
                            println!(
                                "\n  {}  Profile {:?} updated.\n",
                                style("✓").green(),
                                p.name
                            );
                        }
                    }
                }
            }
        }
    }

    Ok(())
}
