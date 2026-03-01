# WebProbe

A fast, Rust-powered CLI tool for exhaustive local web auditing. WebProbe crawls every route of your app, collects accessibility issues, performance metrics, network timing stats, and runs a load test — all in one command.

Built for developers who want to catch problems before they reach production.

---

## Features

- **SPA-aware crawling** — discovers routes via rendered DOM, not raw HTML, so Vite/React/Next apps are fully supported
- **Accessibility auditing** — missing alt text, unlabelled inputs, empty buttons, and more
- **Performance metrics** — FCP, LCP, DCL, and Load time per page, colour-coded against Web Vitals thresholds
- **Network statistics** — DNS lookup, TCP connect, TLS handshake, TTFB, download time, resource counts, transfer size, and slowest resource per page
- **Load testing** — concurrent user simulation with HDR histogram latency percentiles (p50 / p90 / p99)
- **Auth support** — inject a cookie file or auto-fill a login form (React-compatible) before crawling
- **Timestamped reports** — every run writes a new `report-YYYYMMDD-HHMMSS.json`, never overwriting previous results
- **Fast** — runs headless Chrome only when needed; blocks images/fonts/video during auditing to stay lean

---

## Install

### Prerequisites

- [Rust](https://rustup.rs) (stable, 1.75+)
- Google Chrome installed at `/Applications/Google Chrome.app` (macOS) or available on `PATH`

### Build & install

```bash
git clone https://github.com/KaiavN/WebProbe.git
cd WebProbe
cargo install --path . --force
```

This installs `webprobe` to `~/.cargo/bin/webprobe`. Make sure `~/.cargo/bin` is in your `PATH` (the Rust installer adds this automatically).

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

# Standalone load test
webprobe load 3000 --users 20 --duration 60
```

That's it. WebProbe accepts a bare port number — no need to type `http://localhost:`.

---

## Commands

### `webprobe crawl <PORT>`

Crawl every route, run an accessibility + performance audit on each page, then run a load test.

```
USAGE:
    webprobe crawl [OPTIONS] <URL>

ARGS:
    <URL>    Port number (3000), host:port (localhost:3000), or full URL
```

| Flag | Default | Description |
|---|---|---|
| `-d`, `--depth <N>` | `5` | Maximum BFS link-follow depth |
| `-o`, `--output <FILE>` | `report-YYYYMMDD-HHMMSS.json` | JSON report output path |
| `-u`, `--users <N>` | `1` | Concurrent virtual users for the load test |
| `--duration <SECS>` | `30` | Load test duration in seconds |
| `--wait-ms <MS>` | `300` | Extra milliseconds to wait for JS to settle after DOM ready |
| `--headed` | off | Show Chrome window (useful for debugging auth) |
| `--no-load` | off | Skip the load test phase entirely |

**Auth flags** (see [Authentication](#authentication)):

| Flag | Description |
|---|---|
| `--auth-url <PATH>` | Login page path (e.g. `/login`) or full URL |
| `--auth-username <USER>` | Username or email to fill into the login form |
| `--auth-password <PASS>` | Password to fill into the login form |
| `--auth-username-selector <SEL>` | CSS selector for the username input (auto-detected if omitted) |
| `--auth-password-selector <SEL>` | CSS selector for the password input (auto-detected if omitted) |
| `--auth-submit-selector <SEL>` | CSS selector for the submit button (auto-detected if omitted) |
| `--cookies <FILE>` | Path to a JSON cookie file to inject before crawling |

### `webprobe load <PORT>`

Standalone load test — no crawling or browser.

```
USAGE:
    webprobe load [OPTIONS] <URL>
```

| Flag | Default | Description |
|---|---|---|
| `-u`, `--users <N>` | `10` | Concurrent virtual users |
| `-d`, `--duration <SECS>` | `30` | Test duration in seconds |
| `-o`, `--output <FILE>` | `load-YYYYMMDD-HHMMSS.json` | JSON report output path |

---

## Output

### Console

```
  webprobe  Crawling http://localhost:5173

  → Checking http://localhost:5173… ok
  → Launching browser… ready  (1.2s)
  ✓ Crawled 4 pages  (3 issues)

  http://localhost:5173
    ◆ WARN    [Accessibility] Image missing alt attribute
       ↳ <img src="/hero.png" class="hero">

  http://localhost:5173/dashboard
    ● ERROR   [Console Error] Uncaught TypeError: Cannot read ...

  ── Performance ───────────────────────────────────────────
  http://localhost:5173
    FCP:  320ms
    LCP:  850ms
    Load: 1240ms
    DCL:  410ms

  ── Network Stats ─────────────────────────────────────────
  http://localhost:5173
    DNS:       0.1ms
    TCP:       0.2ms
    TTFB:      18.3ms
    Download:  2.1ms
    Resources: 24  failed: 0  transferred: 142.6KB
    Slowest:   340ms  …/src/main.tsx

  ── Load Test ─────────────────────────────────────────────
  1 users  ×  30s  →  48.3 req/s
  Requests: 1450 total  1450 ok  0 failed  (0.0% error rate)
  Latency  p50:20ms  p90:35ms  p99:58ms  max:112ms

━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
  Summary  0 critical  1 errors  2 warnings  0 info
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
```

### JSON report

Every run writes a timestamped JSON file (e.g. `report-20260301-191400.json`). Use `--output myfile.json` to specify a fixed path.

```jsonc
{
  "tool": "webprobe",
  "version": "0.1.0",
  "timestamp": "2026-03-01T19:14:00Z",
  "target_url": "http://localhost:5173",

  "issues": [
    {
      "severity": "warning",
      "category": "accessibility",
      "message": "Image missing alt attribute",
      "page_url": "http://localhost:5173",
      "element": "<img src=\"/hero.png\" class=\"hero\">",
      "action_path": []
    }
  ],

  "perf_metrics": [
    {
      "page_url": "http://localhost:5173",
      "fcp_ms": 320.0,
      "lcp_ms": 850.0,
      "dom_content_loaded_ms": 410.0,
      "load_ms": 1240.0
    }
  ],

  "network_stats": [
    {
      "page_url": "http://localhost:5173",
      "dns_ms": 0.1,
      "tcp_connect_ms": 0.2,
      "tls_ms": null,
      "ttfb_ms": 18.3,
      "download_ms": 2.1,
      "resource_count": 24,
      "failed_resource_count": 0,
      "total_transfer_kb": 142.6,
      "slowest_resource_ms": 340.0,
      "slowest_resource_url": "http://localhost:5173/src/main.tsx"
    }
  ],

  "crawl_stats": {
    "pages_visited": 4,
    "states_explored": 4,
    "duration_secs": 12.4
  },

  "load_test": {
    "users": 1,
    "duration_secs": 30,
    "total_requests": 1450,
    "successful_requests": 1450,
    "failed_requests": 0,
    "error_rate_pct": 0.0,
    "throughput_rps": 48.3,
    "latency_p50_ms": 20.0,
    "latency_p90_ms": 35.0,
    "latency_p99_ms": 58.0,
    "latency_min_ms": 5.0,
    "latency_max_ms": 112.0,
    "latency_mean_ms": 22.1
  },

  "summary": {
    "critical": 0,
    "errors": 1,
    "warnings": 2,
    "infos": 0,
    "total": 3
  }
}
```

---

## Authentication

For apps that redirect to a login page, webprobe supports two methods:

### Method 1 — Auto-login (form fill)

```bash
webprobe crawl 5173 \
  --auth-url /login \
  --auth-username admin@example.com \
  --auth-password secret
```

WebProbe navigates to the login page, fills the form fields using React-compatible native input setters, clicks submit, and waits for navigation before starting the crawl.

Use `--headed` to watch it happen in a visible browser window if you need to debug.

Custom selectors (optional — auto-detected by default):

```bash
webprobe crawl 5173 \
  --auth-url /login \
  --auth-username admin@example.com \
  --auth-password secret \
  --auth-username-selector "input[name=email]" \
  --auth-password-selector "input[name=password]" \
  --auth-submit-selector "button[type=submit]"
```

### Method 2 — Cookie injection

Export your session cookies from Chrome DevTools (Application → Cookies → right-click → Export) or an extension like **EditThisCookie**. Save them as a JSON file:

```json
[
  { "name": "session_id", "value": "abc123", "domain": "localhost", "path": "/" },
  { "name": "auth_token",  "value": "eyJhbGc...", "domain": "localhost", "path": "/", "secure": false }
]
```

Then pass it with `--cookies`:

```bash
webprobe crawl 5173 --cookies ~/my-session.json
```

Cookies are injected via CDP before the first navigation, so the app sees you as already logged in.

---

## Performance thresholds

WebProbe colour-codes metrics against these thresholds in the console:

| Metric | Green | Yellow | Red |
|---|---|---|---|
| FCP | < 1800ms | 1800–3000ms | > 3000ms |
| LCP | < 2500ms | 2500–4000ms | > 4000ms |
| Load time | < 2000ms | 2000–4000ms | > 4000ms |
| TTFB | < 200ms | 200–600ms | > 600ms |

A **Performance** issue is automatically raised if page load time exceeds 3s.

---

## Troubleshooting

**`Cannot reach http://localhost:XXXX — is your server running?`**  
Start your dev server first, then run webprobe.

**`Failed to launch Chrome`**  
WebProbe expects Google Chrome at `/Applications/Google Chrome.app/Contents/MacOS/Google Chrome` on macOS. Install Chrome or ensure it's in that location.

**`Crawled 1 page` (SPA that requires login)**  
If your app redirects everything to `/login`, no links are visible without auth. Use `--auth-url` / `--auth-username` / `--auth-password` to log in first.

**`timed out after 20s` on some pages**  
Increase `--wait-ms` (e.g. `--wait-ms 1000`) to give slow pages more time to render, or check for JS errors that prevent the page from loading.

**Pages show 0ms latency in the load test**  
This only happens if requests complete in under 1 microsecond, which shouldn't occur in practice. If you see it, check that your server is actually processing the requests.

---

## Examples

```bash
# Audit a Vite React app with no load test
webprobe crawl 5173 --no-load

# Deep crawl with 5 virtual users, 60s load test
webprobe crawl 3000 --depth 10 --users 5 --duration 60

# Crawl an app that requires login (auto-fill form)
webprobe crawl 5173 --auth-url /login --auth-username dev@test.com --auth-password password --no-load

# Inject an existing browser session from a cookie file
webprobe crawl 5173 --cookies ~/cookies.json

# Standalone load test: 50 users for 2 minutes
webprobe load 8080 --users 50 --duration 120

# Save to a specific report path
webprobe crawl 4000 --no-load --output my-audit.json

# Headed mode (see Chrome window — useful for auth debugging)
webprobe crawl 5173 --auth-url /login --auth-username u --auth-password p --headed --no-load
```

---

## License

MIT
