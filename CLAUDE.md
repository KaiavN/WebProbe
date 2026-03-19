# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build & Run

```bash
# Build in debug mode
cargo build

# Build optimized release binary
cargo build --release

# Install locally
cargo install --path . --force

# Run the tool
webprobe crawl 3000              # crawl + audit + load test
webprobe crawl 5173 --no-load    # audit only
webprobe load 3000 --users 20    # standalone load test
webprobe profile list            # manage auth profiles
```

## Tests

```bash
# Run all tests
cargo test

# Run a single test
cargo test test_page_state_fingerprint_deduplication
```

There is currently one test module in `src/types.rs` covering URL normalization and fingerprint-based deduplication of `PageState`.

## Architecture

WebProbe is a CLI tool (`src/main.rs`) that crawls localhost SPAs using browser automation, collects audits, and runs load tests. It uses `clap` for CLI parsing with three subcommands: `crawl`, `load`, and `profile`.

### Module layout

- **`src/main.rs`** — CLI entry point with `clap` derive. Parses args, resolves auth config (inline flags, env vars, interactive prompts, or saved profiles), normalizes URLs to `http://localhost:PORT`, and orchestrates the crawl → report → load-test pipeline.
- **`src/types.rs`** — All shared data types: `Report`, `Issue`, `Severity`, `IssueCategory`, `PerfMetrics`, `NetworkStats`, `LoadTestResult`, `CrawlStats`, `PageState`, `PageInteractions`, `AuthConfig`, `CookieEntry`. Contains URL normalization logic (`normalize_url`) and `PageState::fingerprint()` for deduplication.
- **`src/crawler/mod.rs`** — Core crawler. Uses `fantoccini` (WebDriver client) to automate a browser. Implements BFS crawl: discovers `<a href>` links from the rendered DOM, queues them, and runs audits per page. Contains all JavaScript injection scripts for console error capture, performance metrics collection, network stats, accessibility/SEO checks, and interactive element discovery. Also handles auth (form login via native input events, cookie injection) and multi-driver support (geckodriver, chromedriver, safaridriver).
- **`src/crawler/browser.rs`** — WebDriver process management. `DriverProcess` auto-detects the best available browser (Firefox > Chrome > Safari), spawns the WebDriver subprocess on a free port, and polls until ready. Includes brew-based auto-installation of missing drivers/browsers.
- **`src/crawler/state.rs`** — `StateTracker` using `DashSet` for thread-safe deduplication of visited page fingerprints during BFS.
- **`src/crawler/collectors.rs`** and **`src/crawler/element.rs`** — Placeholder modules; actual collection logic is inline in `crawler/mod.rs`.
- **`src/load/mod.rs`** — HTTP load tester using `reqwest`. Spawns N concurrent tasks that round-robin across target URLs, records per-request latency in local `hdrhistogram` instances, and merges for percentile reporting.
- **`src/profiles.rs`** — `ProfileStore` for persisting auth credentials to `~/.config/webprobe/profiles.json`. Supports CRUD operations on `AuthProfile` entries.
- **`src/reporter/mod.rs`** — Re-exports `console` and `json` submodules.
- **`src/reporter/console.rs`** — Color-coded terminal output using the `console` crate. Groups issues by page, deduplicates site-wide issues (shown once when affecting >= 3 pages), and renders performance/network/load-test sections.
- **`src/reporter/json.rs`** — Writes the `Report` struct to a timestamped JSON file.

### Key design decisions

- **BFS crawl with fingerprint deduplication**: `PageState::fingerprint()` returns only the normalized URL (ignoring action path) to prevent infinite ping-ponging between cross-linked pages. `StateTracker` uses `DashSet` for concurrent access.
- **JavaScript injection**: All audit logic (console error capture, performance metrics via `PerformanceObserver`, network stats via `PerformanceResourceTiming`, accessibility checks, SEO checks) is injected as an async JS script via `fantocini.execute_async_script()`.
- **Multi-browser support**: The crawler automatically detects and prioritizes geckodriver+Firefox > chromedriver+Chrome > safaridriver, with auto-install via Homebrew on macOS.
- **Auth flow**: Supports form-based login (React-compatible native input events with `dispatchEvent`), cookie injection from JSON files, and interactive credential prompting. After login, seeds the post-redirect URL into the BFS queue.
- **No external test dependencies beyond `cargo test`**: Tests are inline `#[cfg(test)]` modules.

## Dependencies

Key crates: `fantoccini` (WebDriver), `reqwest` (HTTP), `clap` (CLI), `tokio` (async runtime), `hdrhistogram` (latency percentiles), `dashmap` (concurrent collections), `console`/`indicatif` (terminal UI).
