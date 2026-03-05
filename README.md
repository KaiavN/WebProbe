# WebProbe

A fast, Rust-powered CLI tool for exhaustive local web auditing. WebProbe crawls every route of your SPA, collects accessibility issues, runtime JS errors, performance metrics, and network stats per page — then runs a load test — all in one command.

Built for developers who want to catch problems before they reach production.

---

## Features

- **SPA-aware crawling** — discovers routes from the rendered DOM, so React / Vite / Next.js / Vue apps are fully supported
- **Post-login crawling** — logs in first, then seeds the post-login URL into the BFS so the dashboard and inner pages are always audited
- **Accessibility auditing** — missing `alt`, unlabelled inputs, empty buttons, unlabelled links
- **SEO checks** — missing `<title>` / `<meta description>` / `<meta viewport>` / `<html lang>`, duplicate `<h1>`, title too long
- **Security checks** — `<a target="_blank">` without `rel="noopener noreferrer"`
- **Runtime JS error capture** — `window.onerror`, `unhandledrejection`, and `console.error` are all intercepted and reported per page
- **Performance metrics** — FCP, LCP, TTI, CLS, DCL, Load per page, colour-coded against Web Vitals thresholds
- **Network statistics** — TTFB, resource counts, total transfer size, slowest resource, and exact URLs of any failed resources
- **Load testing** — concurrent user simulation with HDR histogram latency percentiles (p50 / p90 / p99)
- **Saved auth profiles** — store login credentials once and reuse them across runs
- **Timestamped reports** — every run writes a new `report-YYYYMMDD-HHMMSS.json`, never overwriting previous results


## CLI Demo

https://github.com/user-attachments/assets/feb46009-e9a8-47a7-9296-3e146a5c5f29
Demo using a running vite server on localhost:5173
---

## Install

### Prerequisites

- [Rust](https://rustup.rs) (stable, 1.75+)
- [geckodriver](https://github.com/mozilla/geckodriver/releases) on your `PATH` (WebProbe uses Firefox)
- Firefox installed

```bash
# macOS (homebrew)
brew install geckodriver
```

### Build & install

```bash
git clone https://github.com/KaiavN/WebProbe.git
cd WebProbe
cargo install --path . --force
```

This installs `webprobe` to `~/.cargo/bin/webprobe`. Make sure `~/.cargo/bin` is in your `PATH`.

### Verify

```bash
webprobe --version
webprobe --help
```

---

## Quick start

```bash
# Start your dev server, then:
webprobe crawl 5173

# No load test (audit only)
webprobe crawl 3000 --no-load

# With login (auto-fills the form)
webprobe crawl 5173 --auth-url /login --auth-username me@example.com --auth-password secret

# Standalone load test
webprobe load 3000 --users 20 --duration 60
```

WebProbe accepts a bare port number — no need to type `http://localhost:`.

---

## Commands

### `webprobe crawl <PORT>`

Crawl every route, run an accessibility + performance audit on each page, then run a load test.

| Flag | Default | Description |
|---|---|---|
| `-d`, `--depth <N>` | `5` | Maximum BFS link-follow depth |
| `-o`, `--output <FILE>` | `report-YYYYMMDD-HHMMSS.json` | JSON report output path |
| `-u`, `--users <N>` | `1` | Concurrent virtual users for the load test |
| `--duration <SECS>` | `30` | Load test duration in seconds |
| `--wait-ms <MS>` | `300` | Extra ms to wait for JS to settle after DOM ready |
| `--skip <PATHS>` | — | Comma-separated paths to never crawl (e.g. `/map,/logout`) |
| `--headed` | off | Show Firefox window (useful for debugging auth) |
| `--no-load` | off | Skip the load test phase |

**Auth flags:**

| Flag | Description |
|---|---|
| `--auth-url <PATH>` | Login page path (e.g. `/login`) or full URL |
| `--auth-username <USER>` | Username / email |
| `--auth-password <PASS>` | Password |
| `--auth-username-selector <SEL>` | CSS selector for username input (auto-detected if omitted) |
| `--auth-password-selector <SEL>` | CSS selector for password input (auto-detected if omitted) |
| `--auth-submit-selector <SEL>` | CSS selector for submit button (auto-detected if omitted) |
| `--cookies <FILE>` | JSON cookie file to inject before crawling |

### `webprobe load <PORT>`

Standalone load test — no crawling or browser.

| Flag | Default | Description |
|---|---|---|
| `-u`, `--users <N>` | `10` | Concurrent virtual users |
| `-d`, `--duration <SECS>` | `30` | Test duration |
| `-o`, `--output <FILE>` | `load-YYYYMMDD-HHMMSS.json` | JSON report output path |

### `webprobe profile`

Manage saved auth profiles so you don't retype credentials every run.

```bash
webprobe profile list   # list saved profiles
webprobe profile add    # add a new profile interactively
webprobe profile delete # delete a saved profile
```

When you run `webprobe crawl`, you'll be prompted to load a saved profile.

---

## Output

### Console

```
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
  webprobe v0.1.0
  Target : http://localhost:5173
  Crawled: 3 pages  |  6.4s
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

  http://localhost:5173
    ● ERROR    [Console Error] Uncaught TypeError: Cannot read properties of undefined (reading 'map') @ http://localhost:5173/src/App.tsx:42

  http://localhost:5173/dashboard
    ◆ WARN     [Accessibility] Image missing alt attribute
       ↳ <img src="/logo.svg" class="logo">
    ◆ WARN     [SEO] 3 <h1> elements found on this page (should be exactly one)
    · INFO     [SEO] <title> is 72 characters (recommended max: 60)

  http://localhost:5173/settings
    ◆ WARN     [Accessibility] Input missing label / aria-label
       ↳ <input type="text" name="displayName" class="input">

  ── Performance ──────────────────────────────────────────
  http://localhost:5173
    FCP:  410ms
    LCP:  880ms
    TTI:  24ms
    Load: 915ms
    DCL:  890ms

  ── Network Stats ─────────────────────────────────────────
  http://localhost:5173
    TTFB:     3.0ms
    Resources: 118  failed: 1  transferred: 1.8MB
    Failed:    http://localhost:5173/missing-icon.svg
    Slowest:   290ms  …supabase.co/rest/v1/profiles
    
    *Note: Network Stats measures the initial page load execution up to DOM settle time. API calls made after user interaction (like auth submission) will naturally not be recorded here unless they occur immediately on load.*

  ── Load Test ─────────────────────────────────────────────
  1 users  ×  30s  →  2044.2 req/s
  Requests: 61326 total  61326 ok  0 failed  (0.0% error rate)
  Latency  mean:0ms  p50:0ms  p90:1ms  p95:1ms  p99:3ms  max:84ms

  ── Discovered URLs ───────────────────────────────────────
    http://localhost:5173/dashboard
    http://localhost:5173/settings

━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
  Summary  0 critical  1 errors  4 warnings  1 info
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
```

### JSON report

Every run writes a timestamped JSON file. Fields that are always zero or unavailable on localhost (DNS, TCP, TLS, download time) are omitted to keep the report clean.

```json
{
  "tool": "webprobe",
  "version": "0.1.0",
  "timestamp": "2026-03-04T18:20:11Z",
  "target_url": "http://localhost:5173",

  "issues": [
    {
      "severity": "error",
      "category": "console_error",
      "message": "Uncaught TypeError: Cannot read properties of undefined (reading 'map') @ http://localhost:5173/src/App.tsx:42",
      "page_url": "http://localhost:5173",
      "element": null,
      "action_path": []
    },
    {
      "severity": "warning",
      "category": "accessibility",
      "message": "Image missing alt attribute",
      "page_url": "http://localhost:5173/dashboard",
      "element": "<img src=\"/logo.svg\" class=\"logo\">",
      "action_path": []
    },
    {
      "severity": "warning",
      "category": "seo",
      "message": "3 <h1> elements found on this page (should be exactly one)",
      "page_url": "http://localhost:5173/dashboard",
      "element": null,
      "action_path": []
    },
    {
      "severity": "info",
      "category": "seo",
      "message": "<title> is 72 characters (recommended max: 60)",
      "page_url": "http://localhost:5173/dashboard",
      "element": null,
      "action_path": []
    },
    {
      "severity": "error",
      "category": "failed_resource",
      "message": "1 resource failed to load — http://localhost:5173/missing-icon.svg",
      "page_url": "http://localhost:5173",
      "element": null,
      "action_path": []
    }
  ],

  "perf_metrics": [
    {
      "page_url": "http://localhost:5173",
      "fcp_ms": 410.0,
      "lcp_ms": 880.0,
      "tti_ms": 24.0,
      "dom_content_loaded_ms": 890.0,
      "load_ms": 915.0
    },
    {
      "page_url": "http://localhost:5173/dashboard",
      "fcp_ms": 195.0,
      "lcp_ms": 340.0,
      "tti_ms": 18.0,
      "dom_content_loaded_ms": 330.0,
      "load_ms": 355.0
    }
  ],

  "network_stats": [
    {
      "page_url": "http://localhost:5173",
      "ttfb_ms": 3.0,
      "resource_count": 118,
      "failed_resource_count": 1,
      "failed_resource_urls": ["http://localhost:5173/missing-icon.svg"],
      "total_transfer_kb": 1843.2,
      "slowest_resource_ms": 290.0,
      "slowest_resource_url": "https://xyz.supabase.co/rest/v1/profiles"
    },
    {
      "page_url": "http://localhost:5173/dashboard",
      "ttfb_ms": 2.0,
      "resource_count": 42,
      "failed_resource_count": 0,
      "total_transfer_kb": 210.5,
      "slowest_resource_ms": 88.0,
      "slowest_resource_url": "https://xyz.supabase.co/rest/v1/nodes"
    }
  ],

  "crawl_stats": {
    "pages_visited": 3,
    "duration_secs": 6.403,
    "elements_interacted": 15,
    "crawled_urls": [
      "http://localhost:5173",
      "http://localhost:5173/dashboard",
      "http://localhost:5173/settings"
    ]
  },

  "interactions": [
    {
      "page_url": "http://localhost:5173/dashboard",
      "elements_found": 8,
      "elements": [
        { "kind": "button", "label": "New Map" },
        { "kind": "link", "label": "Settings", "href": "/settings" },
        { "kind": "input", "label": "Search", "input_type": "text" }
      ]
    }
  ],

  "discovered_urls": [
    "http://localhost:5173",
    "http://localhost:5173/dashboard",
    "http://localhost:5173/settings"
  ],

  "load_test": {
    "url": "http://localhost:5173",
    "users": 1,
    "duration_secs": 30,
    "total_requests": 61326,
    "successful_requests": 61326,
    "failed_requests": 0,
    "error_rate_pct": 0.0,
    "throughput_rps": 2044.2,
    "latency_p50_ms": 0.302,
    "latency_p90_ms": 0.961,
    "latency_p95_ms": 1.257,
    "latency_p99_ms": 3.405,
    "latency_min_ms": 0.246,
    "latency_max_ms": 84.0,
    "latency_mean_ms": 0.489
  },

  "summary": {
    "critical": 0,
    "errors": 2,
    "warnings": 2,
    "infos": 1,
    "total": 5
  }
}
```

> **Note on omitted fields:** `dns_ms`, `tcp_connect_ms`, `download_ms`, and `tls_ms` are omitted from the report when their values are zero or unavailable. On localhost these are always zero / null (no DNS resolution, no TLS), so they'd only add noise. `cls_score` is omitted when there are no layout shifts (or when Firefox reports none).

---

## Authentication

For apps that require login before crawling inner pages:

### Method 1 — Save a profile (recommended)

```bash
webprobe profile add
# prompts for name, login URL, username, password, and optional CSS selectors
```

Then the next time you run `webprobe crawl`, you'll be offered to load the profile interactively.

### Method 2 — Inline flags

```bash
webprobe crawl 5173 \
  --auth-url /login \
  --auth-username admin@example.com \
  --auth-password secret
```

WebProbe navigates to the login page, fills the form with React-compatible native input events, clicks submit, waits for the redirect, and then starts crawling from the landing page.

If the post-login URL differs from your start URL (e.g. `/` → `/dashboard`), WebProbe automatically seeds both into the crawl queue so neither is missed.

Use `--headed` to watch it happen in a visible Firefox window for debugging.

### Method 3 — Cookie injection

Export session cookies from DevTools (`Application → Cookies → Export`) and pass them with `--cookies`:

```bash
webprobe crawl 5173 --cookies ~/my-session.json
```

---

## Issue checks

| Category | Check | Severity |
|---|---|---|
| Console Error | Runtime JS errors, unhandled promise rejections, `console.error` calls | Error |
| Failed Resource | Sub-resources that returned an error or failed to load (with exact URLs) | Error |
| Network | TTFB > 600ms | Error |
| Network | TTFB > 200ms | Warning |
| Performance | FCP > 3s | Error |
| Performance | FCP / LCP / Load time over threshold | Warning |
| Accessibility | `<img>` missing `alt` | Warning |
| Accessibility | Input without `<label>` or `aria-label` | Warning |
| Accessibility | Button with no accessible text | Warning |
| Accessibility | Link with no accessible text | Warning |
| Accessibility | `<img>` without `width`/`height` (CLS risk) | Info |
| Accessibility | `<html>` missing `lang` attribute | Warning |
| SEO | Missing `<title>` | Warning |
| SEO | `<title>` longer than 60 characters | Info |
| SEO | Missing `<meta name="description">` | Info |
| SEO | Missing `<meta name="viewport">` | Info |
| SEO | More than one `<h1>` on a page | Warning |
| Security | `<a target="_blank">` without `rel="noopener noreferrer"` | Warning |

---

## Performance thresholds

| Metric | Green | Yellow | Red |
|---|---|---|---|
| FCP | < 1800ms | 1800–3000ms | > 3000ms |
| LCP | < 2500ms | 2500–4000ms | > 4000ms |
| Load time | < 2000ms | 2000–4000ms | > 4000ms |
| TTFB | < 200ms | 200–600ms | > 600ms |

---

## Troubleshooting

**`Cannot reach http://localhost:XXXX`**
Start your dev server first, then run webprobe.

**`Login may have failed — still on login page`**
Run with `--headed` to see what the browser is doing. If selectors aren't being found, add them via `--auth-username-selector` / `--auth-password-selector`.

**`Crawled 1 page` on a multi-page SPA**
Your app likely uses programmatic navigation (`navigate('/dashboard')`) instead of `<a href>` links. WebProbe follows `<a href>` tags. If the post-login page is different from your start URL, WebProbe will now automatically seed it too — but pages only reachable via JS navigation won't be discovered.

**`timed out after 15s` on some pages**
Increase `--wait-ms` (e.g. `--wait-ms 1000`) to give slow pages more time to render, or check for JS errors that block hydration.

---

## Examples

```bash
# Audit a Vite React app with no load test
webprobe crawl 5173 --no-load

# Deep crawl with 5 virtual users, 60s load test
webprobe crawl 3000 --depth 10 --users 5 --duration 60

# Skip the map and logout routes
webprobe crawl 5173 --skip /map,/logout

# Headed mode — watch the browser (useful for auth debugging)
webprobe crawl 5173 --auth-url /login --auth-username u --auth-password p --headed --no-load

# Inject an existing session from a cookie file
webprobe crawl 5173 --cookies ~/cookies.json

# Standalone load test: 50 users for 2 minutes
webprobe load 8080 --users 50 --duration 120

# Save to a specific report path
webprobe crawl 4000 --no-load --output my-audit.json
```

---

## License

MIT
