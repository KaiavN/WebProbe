mod crawler;
mod load;
mod profiles;
mod reporter;
mod types;

use anyhow::Result;
use chrono::Local;
use clap::{Parser, Subcommand};
use console::style;
use std::io::{self, Write};
use std::path::PathBuf;
use profiles::{AuthProfile, ProfileStore};
use types::{AuthConfig, Report};

/// webprobe — Exhaustive web crawler & load tester for localhost development
///
/// QUICK START
///   webprobe crawl 3000
///   webprobe crawl localhost:3000 --depth 10 --users 20
///
/// LOGIN / AUTH
///   Minimal usage — prompts for credentials at the terminal (safest):
///     webprobe crawl 3000 --auth-url /login
///
///   Or export env vars so you don't repeat them:
///     export WEBPROBE_USERNAME=admin@example.com
///     export WEBPROBE_PASSWORD=secret
///     webprobe crawl 3000 --auth-url /login
///
///   Inline flags (avoid special characters like ! without single-quoting):
///     webprobe crawl 3000 --auth-url /login \
///       --auth-username admin@example.com \
///       --auth-password 'My!Password'
///
///   CSS selectors for the login form fields (required):
///     --auth-username-selector "#email"
///     --auth-password-selector "#password"
///     --auth-submit-selector   "button[type='submit']"
///
///   Alternatively inject a cookie file exported from browser DevTools:
///     webprobe crawl 3000 --cookies cookies.json
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
    /// LOGIN
    ///   CSS selectors for the login form fields are required:
    ///   Minimal usage — prompts for credentials at the terminal (safest):
    ///     webprobe crawl 3000 --auth-url /login
    ///
    ///   Or pass inline (use single quotes around passwords with special chars):
    ///     webprobe crawl 3000 --auth-url /login \
    ///       --auth-username you@example.com --auth-password 'My!Pass'
    ///
    ///   CSS selectors for the login form fields (required):
    ///     --auth-username-selector "#email"
    ///     --auth-password-selector "#password"
    ///     --auth-submit-selector   "button.login-btn"
    ///
    ///   Cookie-based auth (export from DevTools → Application → Cookies):
    ///     webprobe crawl 3000 --cookies cookies.json
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

        /// Number of concurrent browser tabs for crawling
        #[arg(short = 'c', long, default_value_t = 1)]
        concurrency: usize,

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
        /// Login page path or URL — triggers form-based auth before crawling
        /// (e.g. /login or http://localhost:3000/login)
        #[arg(long, value_name = "URL")]
        auth_url: Option<String>,

        /// Username or email to fill into the login form.
        /// Falls back to WEBPROBE_USERNAME env var, then prompts interactively.
        /// Tip: omit this flag to avoid shell quoting / history issues.
        #[arg(long, value_name = "USERNAME")]
        auth_username: Option<String>,

        /// Password to fill into the login form.
        /// Falls back to WEBPROBE_PASSWORD env var, then prompts interactively (hidden input).
        /// Tip: omit this flag to avoid shell quoting / history issues.
        #[arg(long, value_name = "PASSWORD")]
        auth_password: Option<String>,

        /// CSS selector for the username/email input.
        /// Required when using form-based auth (--auth-url).
        #[arg(long, value_name = "SELECTOR")]
        auth_username_selector: Option<String>,

        /// CSS selector for the password input.
        /// Required when using form-based auth (--auth-url).
        #[arg(long, value_name = "SELECTOR")]
        auth_password_selector: Option<String>,

        /// CSS selector for the submit/sign-in button.
        /// Required when using form-based auth (--auth-url).
        #[arg(long, value_name = "SELECTOR")]
        auth_submit_selector: Option<String>,

        /// Path to a JSON cookie file to inject instead of form login.
        /// Export from browser DevTools → Application → Cookies → right-click → Copy as JSON.
        #[arg(long, value_name = "FILE")]
        cookies: Option<PathBuf>,

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
            concurrency,
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
            skip,
            selector,
            profile,
        } => {
            let url = normalize_url(&url);
            let output = resolve_output(output, "report");

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
            let mut auth_url = auth_url;
            let mut auth_username = auth_username;
            let mut auth_password = auth_password;
            let mut auth_username_selector = auth_username_selector;
            let mut auth_password_selector = auth_password_selector;
            let mut auth_submit_selector = auth_submit_selector;
            // Tracks whether auth was loaded from a saved profile (skip the save prompt)
            // and the name of that profile so we can save missing fields back to it.
            let mut profile_used = false;
            let mut loaded_profile_name: Option<String> = None;

            // Apply --profile defaults (explicit flags take precedence)
            if let Some(profile_name) = &profile {
                let store = ProfileStore::load()?;
                if let Some(p) = store.get(profile_name) {
                    if auth_url.is_none() { auth_url = p.login_url.clone(); }
                    if auth_username.is_none() { auth_username = p.username.clone(); }
                    if auth_password.is_none() { auth_password = p.password.clone(); }
                    if auth_username_selector.is_none() { auth_username_selector = p.username_selector.clone(); }
                    if auth_password_selector.is_none() { auth_password_selector = p.password_selector.clone(); }
                    if auth_submit_selector.is_none() { auth_submit_selector = p.submit_selector.clone(); }
                    profile_used = true;
                    loaded_profile_name = Some(p.name.clone());
                    println!("  {}  Using profile: {}", style("→").cyan(), style(profile_name).bold());
                } else {
                    println!("  {}  Profile {:?} not found — continuing without it.", style("⚠").yellow(), profile_name);
                }
            }

            let mut auth_username = auth_username
                .or_else(|| std::env::var("WEBPROBE_USERNAME").ok());
            let mut auth_password = auth_password
                .or_else(|| std::env::var("WEBPROBE_PASSWORD").ok());

            // Only ask about auth if nothing was provided via flags/env.
            if auth_url.is_none() && auth_username.is_none() && auth_password.is_none() && cookies.is_none() {
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
                    println!("  {}  {}", style("◆").cyan().bold(), style("Saved profiles").bold());
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
                            auth_username_selector = auth_username_selector.or_else(|| p.username_selector.clone());
                            auth_password_selector = auth_password_selector.or_else(|| p.password_selector.clone());
                            auth_submit_selector = auth_submit_selector.or_else(|| p.submit_selector.clone());
                            profile_used = true;
                            loaded_profile_name = Some(p.name.clone());
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

                    // Username (required)
                    if auth_username.is_none() {
                        loop {
                            print!(
                                "  {}  {} ",
                                style("├").cyan().dim(),
                                style("Username / email:").dim()
                            );
                            io::stdout().flush().ok();
                            let mut u_input = String::new();
                            io::stdin().read_line(&mut u_input).ok();
                            let trimmed = u_input.trim().to_string();
                            if !trimmed.is_empty() {
                                auth_username = Some(trimmed);
                                break;
                            }
                            eprintln!("  {}  Username is required.", style("⚠").yellow());
                        }
                    }

                    // Password (required)
                    if auth_password.is_none() {
                        loop {
                            let pwd = rpassword::prompt_password(format!(
                                "  {}  {} ",
                                style("├").cyan().dim(),
                                style("Password:").dim()
                            ))
                            .unwrap_or_default();
                            if !pwd.is_empty() {
                                auth_password = Some(pwd);
                                break;
                            }
                            eprintln!("  {}  Password is required.", style("⚠").yellow());
                        }
                    }

                    // CSS selector prompts (required when auth is enabled)
                    println!();
                    println!(
                        "  {}  {}",
                        style("◆").cyan().bold(),
                        style("Form selectors").bold()
                    );
                    println!(
                        "  {}  {}",
                        style("│").cyan().dim(),
                        style("How to find a selector:  open DevTools (F12) → right-click").dim()
                    );
                    println!(
                        "  {}  {}",
                        style("│").cyan().dim(),
                        style("the element → Inspect → right-click the node in the HTML").dim()
                    );
                    println!(
                        "  {}  {}",
                        style("│").cyan().dim(),
                        style("panel → Copy → Copy selector.").dim()
                    );
                    println!();

                    if auth_username_selector.is_none() {
                        print!(
                            "  {}  {} ",
                            style("├").cyan().dim(),
                            style("Username field selector (optional, Enter to auto-detect):").dim()
                        );
                        io::stdout().flush().ok();
                        let mut s = String::new();
                        io::stdin().read_line(&mut s).ok();
                        let trimmed = s.trim().to_string();
                        if !trimmed.is_empty() {
                            auth_username_selector = Some(trimmed);
                        }
                    }

                    if auth_password_selector.is_none() {
                        print!(
                            "  {}  {} ",
                            style("├").cyan().dim(),
                            style("Password field selector (optional, Enter to auto-detect):").dim()
                        );
                        io::stdout().flush().ok();
                        let mut s = String::new();
                        io::stdin().read_line(&mut s).ok();
                        let trimmed = s.trim().to_string();
                        if !trimmed.is_empty() {
                            auth_password_selector = Some(trimmed);
                        }
                    }

                    if auth_submit_selector.is_none() {
                        print!(
                            "  {}  {} ",
                            style("└").cyan().dim(),
                            style("Submit button selector (optional, Enter to auto-detect):").dim()
                        );
                        io::stdout().flush().ok();
                        let mut s = String::new();
                        io::stdin().read_line(&mut s).ok();
                        let trimmed = s.trim().to_string();
                        if !trimmed.is_empty() {
                            auth_submit_selector = Some(trimmed);
                        }
                    }
                }
                } // end if !profile_loaded
            }

            // If credentials supplied via flags/env but no login URL, prompt for it.
            if auth_url.is_none() && (auth_username.is_some() || auth_password.is_some()) {
                loop {
                    print!(
                        "  {}  {} ",
                        style("└").cyan().dim(),
                        style("Login page path or URL (e.g. /login):").dim()
                    );
                    io::stdout().flush().ok();
                    let mut input = String::new();
                    io::stdin().read_line(&mut input).ok();
                    let trimmed = input.trim().to_string();
                    if !trimmed.is_empty() {
                        auth_url = Some(trimmed);
                        break;
                    }
                    eprintln!("  {}  Login URL is required.", style("⚠").yellow());
                }
            }

            // If --auth-url is set but credentials are still missing (not prompted above), ask now.
            if auth_url.is_some() {
                if auth_username.is_none() {
                    loop {
                        print!(
                            "  {}  {} ",
                            style("├").cyan().dim(),
                            style("Username:").dim()
                        );
                        io::stdout().flush().ok();
                        let mut input = String::new();
                        io::stdin().read_line(&mut input).ok();
                        let trimmed = input.trim().to_string();
                        if !trimmed.is_empty() {
                            auth_username = Some(trimmed);
                            break;
                        }
                        eprintln!("  {}  Username is required.", style("⚠").yellow());
                    }
                }
                if auth_password.is_none() {
                    loop {
                        let pwd = rpassword::prompt_password(format!(
                            "  {}  {} ",
                            style("└").cyan().dim(),
                            style("Password:").dim()
                        ))
                        .unwrap_or_default();
                        if !pwd.is_empty() {
                            auth_password = Some(pwd);
                            break;
                        }
                        eprintln!("  {}  Password is required.", style("⚠").yellow());
                    }
                }
                // Require selectors when auth is enabled via flags/env
                let need_selectors = auth_username_selector.is_none()
                    || auth_password_selector.is_none()
                    || auth_submit_selector.is_none();
                if need_selectors {
                    println!();
                    println!(
                        "  {}  {}",
                        style("◆").cyan().bold(),
                        style("Form selectors").bold()
                    );
                    println!(
                        "  {}  {}",
                        style("│").cyan().dim(),
                        style("How to find a selector:  open DevTools (F12) → right-click").dim()
                    );
                    println!(
                        "  {}  {}",
                        style("│").cyan().dim(),
                        style("the element → Inspect → right-click the node in the HTML").dim()
                    );
                    println!(
                        "  {}  {}",
                        style("│").cyan().dim(),
                        style("panel → Copy → Copy selector.").dim()
                    );
                    println!();
                }
                if auth_username_selector.is_none() {
                    print!(
                        "  {}  {} ",
                        style("├").cyan().dim(),
                        style("Username field selector (optional, Enter to auto-detect):").dim()
                    );
                    io::stdout().flush().ok();
                    let mut s = String::new();
                    io::stdin().read_line(&mut s).ok();
                    let trimmed = s.trim().to_string();
                    if !trimmed.is_empty() {
                        auth_username_selector = Some(trimmed);
                    }
                }
                if auth_password_selector.is_none() {
                    print!(
                        "  {}  {} ",
                        style("├").cyan().dim(),
                        style("Password field selector (optional, Enter to auto-detect):").dim()
                    );
                    io::stdout().flush().ok();
                    let mut s = String::new();
                    io::stdin().read_line(&mut s).ok();
                    let trimmed = s.trim().to_string();
                    if !trimmed.is_empty() {
                        auth_password_selector = Some(trimmed);
                    }
                }
                if auth_submit_selector.is_none() {
                    print!(
                        "  {}  {} ",
                        style("└").cyan().dim(),
                        style("Submit button selector (optional, Enter to auto-detect):").dim()
                    );
                    io::stdout().flush().ok();
                    let mut s = String::new();
                    io::stdin().read_line(&mut s).ok();
                    let trimmed = s.trim().to_string();
                    if !trimmed.is_empty() {
                        auth_submit_selector = Some(trimmed);
                    }
                }
            }

            // If a profile was used but it was missing fields that we just collected,
            // save those new fields back to the profile automatically.
            if let Some(ref pname) = loaded_profile_name {
                if let Ok(mut store) = ProfileStore::load() {
                    if let Some(existing) = store.get(pname).cloned() {
                        let orig_login_url = existing.login_url.clone();
                        let orig_user_sel = existing.username_selector.clone();
                        let orig_pass_sel = existing.password_selector.clone();
                        let orig_sub_sel = existing.submit_selector.clone();
                        let updated = AuthProfile {
                            name: existing.name.clone(),
                            login_url: auth_url.clone().or(existing.login_url),
                            username: auth_username.clone().or(existing.username),
                            password: auth_password.clone().or(existing.password),
                            username_selector: auth_username_selector.clone().or(existing.username_selector),
                            password_selector: auth_password_selector.clone().or(existing.password_selector),
                            submit_selector: auth_submit_selector.clone().or(existing.submit_selector),
                        };
                        let changed = updated.login_url != orig_login_url
                            || updated.username_selector != orig_user_sel
                            || updated.password_selector != orig_pass_sel
                            || updated.submit_selector != orig_sub_sel;
                        if changed {
                            store.upsert(updated);
                            if store.save().is_ok() {
                                println!(
                                    "  {}  Profile {:?} updated with new fields.",
                                    style("✓").green(),
                                    pname
                                );
                            }
                        }
                    }
                }
            }

            // Offer to save auth details as a profile
            if !profile_used && auth_url.is_some() {
                println!();
                print!(
                    "  {}  {} ",
                    style("◆").cyan().dim(),
                    style("Save these auth details as a profile? [y/N]:").dim()
                );
                io::stdout().flush().ok();
                let mut save_input = String::new();
                io::stdin().read_line(&mut save_input).ok();
                if save_input.trim().eq_ignore_ascii_case("y") {
                    print!(
                        "  {}  {} ",
                        style("└").cyan().dim(),
                        style("Profile name:").dim()
                    );
                    io::stdout().flush().ok();
                    let mut name_input = String::new();
                    io::stdin().read_line(&mut name_input).ok();
                    let name = name_input.trim().to_string();
                    if !name.is_empty() {
                        let new_profile = AuthProfile {
                            name: name.clone(),
                            login_url: auth_url.clone(),
                            username: auth_username.clone(),
                            password: auth_password.clone(),
                            username_selector: auth_username_selector.clone(),
                            password_selector: auth_password_selector.clone(),
                            submit_selector: auth_submit_selector.clone(),
                        };
                        match ProfileStore::load() {
                            Ok(mut store) => {
                                store.upsert(new_profile);
                                match store.save() {
                                    Ok(()) => println!(
                                        "  {}  Profile {:?} saved.",
                                        style("✓").green(),
                                        name
                                    ),
                                    Err(e) => eprintln!("  {}  Could not save profile: {}", style("⚠").yellow(), e),
                                }
                            }
                            Err(e) => eprintln!("  {}  Could not load profiles: {}", style("⚠").yellow(), e),
                        }
                    }
                }
            }

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
                println!(
                    "  {}  Link scope: {}",
                    style("→").cyan(),
                    style(sel).bold()
                );
            }

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
                concurrency,
                headless: !headed,
                settle_ms: wait_ms,
                auth,
                skip_paths,
                link_selector: selector,
            };

            let crawl_result = crawler::run_crawler(crawler_config).await?;

            let mut report = Report::new(&url);
            report.issues = crawl_result.issues;
            report.perf_metrics = crawl_result.perf_metrics;
            report.network_stats = crawl_result.network_stats;
            report.crawl_stats = crawl_result.stats;

            if !no_load && users > 0 {
                let load_urls = if crawl_result.discovered_urls.is_empty() {
                    vec![url.clone()]
                } else {
                    crawl_result.discovered_urls.clone()
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
                urls: vec![url.clone()],
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
                            let pwd_status = if p.password.is_some() { "(saved)" } else { "(none)" };
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
                    print!(
                        "\n  {}  Profile name: ",
                        style("◆").cyan().bold()
                    );
                    io::stdout().flush().ok();
                    let mut profile_name = String::new();
                    io::BufRead::read_line(&mut io::stdin().lock(), &mut profile_name).ok();
                    let profile_name = profile_name.trim().to_string();
                    if profile_name.is_empty() {
                        println!("\n  {} Profile name cannot be empty.\n", style("⚠").yellow());
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
                            std::io::BufRead::read_line(&mut std::io::stdin().lock(), &mut buf).ok();
                            let v = buf.trim().to_string();
                            if v.is_empty() { None } else { Some(v) }
                        }

                        let login_url         = prompt_new("Login URL         ");
                        let username          = prompt_new("Username          ");

                        print!(
                            "  {}  {}: ",
                            style("├").cyan().dim(),
                            style("Password          ").dim(),
                        );
                        io::stdout().flush().ok();
                        let pwd_input = rpassword::read_password().unwrap_or_default();
                        let password = if pwd_input.is_empty() { None } else { Some(pwd_input) };

                        let username_selector = prompt_new("Username selector ");
                        let password_selector = prompt_new("Password selector ");
                        let submit_selector   = prompt_new("Submit selector   ");

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
                        None => println!("\n  {} No profile named {:?}.\n", style("⚠").yellow(), name),
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

                            fn prompt_optional(label: &str, current: &Option<String>) -> Option<String> {
                                let cur = current.as_deref().unwrap_or("(none)");
                                print!(
                                    "  {}  {} [{}]: ",
                                    console::style("├").cyan().dim(),
                                    console::style(label).dim(),
                                    console::style(cur).yellow()
                                );
                                std::io::Write::flush(&mut std::io::stdout()).ok();
                                let mut buf = String::new();
                                std::io::BufRead::read_line(&mut std::io::stdin().lock(), &mut buf).ok();
                                let v = buf.trim().to_string();
                                if v.is_empty() { current.clone() } else if v == "-" { None } else { Some(v) }
                            }

                            let login_url = prompt_optional("Login URL         ", &p.login_url);
                            let username   = prompt_optional("Username          ", &p.username);

                            // Password — hidden input
                            print!(
                                "  {}  {} [{}]: ",
                                style("├").cyan().dim(),
                                style("Password          ").dim(),
                                style(if p.password.is_some() { "(saved)" } else { "(none)" }).yellow()
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

                            let username_selector = prompt_optional("Username selector ", &p.username_selector);
                            let password_selector = prompt_optional("Password selector ", &p.password_selector);
                            let submit_selector   = prompt_optional("Submit selector   ", &p.submit_selector);

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
