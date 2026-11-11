<div align="center">
<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="/assets/Inbrowse-white.svg">
    <source media="(prefers-color-scheme: light)" srcset="/assets/Inbrowse-black.svg">
    <img alt="Inbrowse" src="/assets/Inbrowse-black.svg" width="840">
  </picture>
</p>
</div>
<p align="center">by <strong>Incord AI</strong></p>
<p align="center">
  <a href="https://github.com/incordai/Inbrowse/releases"><img src="https://img.shields.io/badge/Releases-1a1a1a?style=for-the-badge&logo=github&logoColor=white" alt="Releases" /></a>
</p>
<p align="center">
  <strong>The open-source headless browser for AI agents and web scraping.</strong><br>
  Lightweight, stealthy, and built in Rust.
</p>
<h3 align="center">Native rendering is here. No Chromium required. 🎉 </h3>
<p align="center">
  Capture screenshots, screencast live pages, and export PDFs directly with Inbrowse.
</p>

---

Inbrowse is a headless browser engine written in Rust by Incord AI, built for web scraping and AI agent automation. It runs real JavaScript via V8, supports the Chrome DevTools Protocol, and acts as a drop-in replacement for headless Chrome with Puppeteer and Playwright.

### Why Inbrowse over headless Chrome?

| Metric | Inbrowse | Headless Chrome |
|--------|----------|-----------------|
| Memory, small page (example.com) | **30 MB** | 432 MB |
| Memory, real site (pypi.org project page) | **151 MB** (81 MB without rendering) | 541 MB |
| Binary size | **85 MB** (64 MB without rendering) | 487 MB install |
| Load one page, process start to exit | **40 ms** static, **64 ms** React | 1.28 s |
| Anti-detect | **Built-in** | None |
| Puppeteer | **Yes** | Yes |
| Playwright | **Yes** | Yes |

Measured on Windows 11 x86_64 with the v0.3.0 release downloads (the default
build, which renders, and the `no-render` build), headless Chrome 157 with
`--dump-dom`, median of 3 to 7 runs. Memory is peak working set; Chrome's is the
sum over its processes, which counts shared pages more than once. Page loads use
the local obstacle-course fixtures. Other release archives: Linux x86_64 88 MB
(68 MB without rendering), macOS Apple Silicon 76 MB. A local `--release`
build is larger (102 MB on Windows), since release archives use the
size-optimized `release-dist` profile. Memory grows with the page: a 5,000-row DOM build
peaks at 45 MB, and a rendered page keeps its layout and images.

## What's new in Inbrowse

### Web platform

| Feature | What changed |
|---|---|
| **WebSocket** | A real connection instead of a stub that reported OPEN and dropped every message. Sends the page's cookies, `Origin` and user agent; uses the configured HTTP/SOCKS5 proxy; refuses private addresses unless `--allow-private-network`; stealth builds use the Chrome TLS fingerprint. Text, binary, Blob, subprotocols and close codes work. |
| **WebSocket in CDP** | `Network.webSocketCreated`, `webSocketWillSendHandshakeRequest`, `webSocketHandshakeResponseReceived`, `webSocketFrameSent` / `Received`, `webSocketFrameError` and `webSocketClosed`, so Puppeteer and Playwright see socket traffic. |
| **IndexedDB on disk** | With `--storage-dir`, databases are saved per origin under `indexedDB/` and survive a restart. |
| **Touch and drag** | `Touch`, `TouchList`, `TouchEvent`, `DataTransfer` and `DragEvent`; CDP `Input.dispatchTouchEvent`, `Input.dispatchDragEvent` and `Input.setInterceptDrags`. |
| **Timezone on Windows** | The default zone and `INBROWSE_TIMEZONE` also apply on Windows, where ICU ignores `TZ`, so pages never see the host's real zone. |
| **`innerText` whitespace** | Whitespace collapses per the element's `white-space` value, so `pre` and `pre-wrap` text keeps its spaces and line breaks. |

### Rendering

| Feature | What changed |
|---|---|
| **Tables** | `border-collapse: collapse` draws each shared border once, resolved as CSS specifies. `<caption>` renders above or below the rows per `caption-side`, wrapping to the table width. |
| **List markers** | Bullets and numbers are drawn at the item text's size and on its baseline instead of small and raised. |
| **Inline SVG** | `<svg>` is inline like `<img>`, so `text-align` places it. SVG `font-family` names such as Arial map to the same bundled faces as HTML text. |
| **Web fonts** | `@font-face` rules from linked stylesheets are applied, including cross-origin sheets with CORS such as Google Fonts. Previously only fonts from inline `<style>` worked. |

### PDF export (`Page.printToPDF`, `page.pdf()`)

| Feature | What changed |
|---|---|
| **Selectable text** | Each page has an invisible text layer over the painted text, so PDF text can be selected, searched and extracted (any Unicode text). Previously the pages were images with no text. |
| **Reflow to paper width** | The page is laid out at the paper's printable width with print media, as Chrome does, instead of shrinking the screen layout onto the page. |
| **No split lines** | Page breaks move above a line of text instead of cutting it in half. |
| **Resolution** | Pages render at the device scale factor, so `Emulation.setDeviceMetricsOverride` with `deviceScaleFactor: 2` gives 192 dpi pages. |
| **Headers and footers** | `displayHeaderFooter` with `headerTemplate` / `footerTemplate`, including the `date`, `title`, `url`, `pageNumber` and `totalPages` classes and Chrome's default templates. |
| **Outline and page size** | `generateDocumentOutline` adds bookmarks from the headings; `preferCSSPageSize` uses `@page { size }`. |

### CLI and extraction

| Feature | What changed |
|---|---|
| **`inbrowse extract`** | Text from a PDF, Word, PowerPoint, OpenDocument, spreadsheet, CSV or text file, local or downloaded, as JSON with one section per page, slide or sheet. See the [CLI reference](docs/CLI-reference.md). |
| **`inbrowse media`** | Still frames (PNG, at an interval and at scene changes), 16 kHz mono WAV audio and captions from MP4, M4A, WebM, Matroska and WebVTT files. Decodes H.264, VP8, VP9, AAC and Opus with nothing extra to install. |
| **`inbrowse ocr`** | The text in an image, with each line's box and confidence, read by the OCR model in the Incord memory engine. Big images are split into overlapping tiles, never refused. `inbrowse media --ocr` reads the text in each frame. |
| **`inbrowse social`** | Posts with engagement counts from Reddit, X, Hacker News, GitHub and Polymarket, ranked and de-duplicated, signed in with an API key, your cookies, or anonymously. |
| **`fetch --font-dir`** | One-shot screenshots can load system fonts, as `serve` and `mcp` already could. |
| **`--dump markdown` tables** | Tables are valid GFM: a header row, a `\|---\|` separator, one line per row, and the caption as a paragraph. |

Each of these is also an MCP tool (`document_extract`, `media_extract`,
`image_ocr`, `social_search`, `social_thread`).

## Works with Watchman

Inbrowse works on its own, and it also works with
[Watchman](https://github.com/incordai/Watchman-GUI), the Incord AI desktop app
that runs the services Inbrowse leans on:

| Service | What Watchman provides |
|---|---|
| **OCR** | The Incord memory engine Watchman runs holds the OCR model. `inbrowse ocr`, `image_ocr` and `media --ocr` call it at `http://127.0.0.1:9180` by default, so with Watchman running there is nothing to set up. |
| **Cookies** | Watchman keeps the sign-in cookies for Reddit, X, LinkedIn and other sources, and warns you about the account risk before you use one. |
| **API keys** | Watchman keeps the API keys for the research sources (Reddit app id and secret, X bearer token, GitHub token) and the engine key. |

Inbrowse never stores cookies or keys itself: Watchman hands them over for each
call, and Inbrowse never logs or echoes them.

Without Watchman, every feature still works from the CLI or MCP. Point
`INBROWSE_OCR_URL` at any running Incord memory engine, pass cookies with
`--cookies`, and set API keys in environment variables
(`INBROWSE_REDDIT_CLIENT_ID`, `INBROWSE_REDDIT_CLIENT_SECRET`,
`INBROWSE_X_BEARER_TOKEN`, `INBROWSE_GITHUB_TOKEN`, `INBROWSE_OCR_KEY`) or pass
them per call as the MCP tools' `credentials`. See the
[CLI reference](docs/CLI-reference.md).

## Install

### Download

Grab the latest binary from [Releases](https://github.com/incordai/Inbrowse/releases):

```bash
# Linux x86_64
curl -LO https://github.com/incordai/Inbrowse/releases/latest/download/inbrowse-x86_64-linux.tar.gz
tar xzf inbrowse-x86_64-linux.tar.gz
./inbrowse fetch https://example.com --eval "document.title"

# Linux ARM64 (aarch64)
curl -LO https://github.com/incordai/Inbrowse/releases/latest/download/inbrowse-aarch64-linux.tar.gz
tar xzf inbrowse-aarch64-linux.tar.gz

# macOS Apple Silicon
curl -LO https://github.com/incordai/Inbrowse/releases/latest/download/inbrowse-aarch64-macos.tar.gz
tar xzf inbrowse-aarch64-macos.tar.gz

# macOS Intel
curl -LO https://github.com/incordai/Inbrowse/releases/latest/download/inbrowse-x86_64-macos.tar.gz
tar xzf inbrowse-x86_64-macos.tar.gz

# Windows
Download the `.zip` from the releases page and extract it manually.
```

No Chrome, no Node.js, no dependencies. Release archives include both
`inbrowse` and `inbrowse-worker`; keep them in the same directory for the
parallel `scrape` command.

| Archive suffix | Rendering | Stealth transport |
|----------------|-----------|-------------------|
| none | Yes | No |
| `-stealth` | Yes | Yes |
| `-no-render` | No | No |
| `-no-render-stealth` | No | Yes |

Linux release builds target Ubuntu 22.04 so the downloaded binary remains
usable on common LTS servers with glibc 2.35+.

### Docker

```bash
docker build -t inbrowse .
docker run -d --name inbrowse -p 127.0.0.1:9222:9222 \
  -e INBROWSE_CDP_TOKEN="$(openssl rand -hex 32)" \
  inbrowse
```

Built from the repository's `Dockerfile`. Multi-stage build on `distroless/cc:nonroot` — no shell, no package manager, runs as uid 65532, ~57 MB compressed. A mounted `--storage-dir` must be writable by uid 65532. Publish to host loopback as above; `-p 9222:9222` exposes the port on every interface.

### Build from source

```bash
git clone https://github.com/incordai/Inbrowse.git
cd inbrowse

# Rendering
cargo build --release -p inbrowse-cli --bins --features render

# Rendering and stealth
cargo build --release -p inbrowse-cli --bins --features render,stealth

# No rendering
cargo build --release -p inbrowse-cli --bins --no-default-features

# No rendering, with stealth
cargo build --release -p inbrowse-cli --bins --no-default-features --features stealth
```

Requires Rust 1.75+ ([rustup.rs](https://rustup.rs)). First build takes ~5 min (V8 compiles from source, cached after).
The stealth build also compiles BoringSSL and generates bindings, so it needs
CMake, Clang, and the libclang/LLVM development libraries. On Ubuntu/Debian:

```bash
sudo apt-get install build-essential cmake clang libclang-dev llvm-dev
```

The rendering build uses rustls. The rendering-and-stealth build uses
wreq/BoringSSL and therefore needs the additional build tools above.

## Quick Start

### Fetch a page

```bash
# Get the page title
inbrowse fetch https://example.com --eval "document.title"

# Extract all links
inbrowse fetch https://example.com --dump links

# Render JavaScript and dump HTML
inbrowse fetch https://news.ycombinator.com --dump html

# Write dump or eval output to a file
inbrowse fetch https://example.com --dump text --output page.txt

# Stream the raw response body verbatim (binary-safe; bypasses the JS/DOM layer).
# Use this for images, JSON, JS, CSS, or any non-HTML resource.
inbrowse fetch https://picsum.photos/200/300 --dump original > photo.jpg

# List every sub-resource URL the page would fetch (NDJSON; one record per asset)
inbrowse fetch https://example.com --dump assets

# Fetch through an HTTP or SOCKS proxy
inbrowse --proxy socks5://127.0.0.1:1080 fetch https://example.com --dump text

# Wait for dynamic content
inbrowse fetch https://example.com --wait-until networkidle0

# Bound navigation time for slow or broken pages
inbrowse fetch https://example.com --timeout 10

# Capture the settled page as PNG
inbrowse fetch https://example.com --screenshot page.png

# The screenshot flag also has a short form
inbrowse fetch https://example.com -s page.png

### Testing against localhost / LAN dev servers

Inbrowse blocks fetches to private/internal IPs by default (SSRF protection).
To point it at a local dev server, pass `--allow-private-network` (or set
`INBROWSE_ALLOW_PRIVATE_NETWORK=1`):

```bash
inbrowse fetch http://127.0.0.1:3000 --allow-private-network --dump text

# Works on any subcommand, e.g. the CDP server for local Puppeteer/Playwright:
inbrowse serve --port 9222 --allow-private-network
```

See [docs/Environment-variables.md](docs/Environment-variables.md) for the
full allow/deny rules (DNS-resolution-time checks included).
```

## Rendering

Official release archives and the Docker image include the rendering engine.
It provides CSS layout and paint, viewport and full-page screenshots,
scroll-aware fixed and sticky geometry, activity-driven CDP screencasting, and
PDF export with selectable text, without starting Chromium.

```javascript
await page.setViewport({ width: 1440, height: 1000 });
await page.goto('https://example.com', { waitUntil: 'load' });
await page.screenshot({ path: 'page.png', fullPage: true });
await page.pdf({ path: 'page.pdf', format: 'A4', printBackground: true });
```

The current implementation covers block, inline, flex, grid, table, float,
positioning, overflow, transform, text, image, SVG, canvas, background, border,
and animation paths. It remains an evolving independent
engine: long-tail CSS, some Web APIs, media playback, compositor effects, and
platform font rasterization may differ from Chromium. The existing
[Puppeteer](docs/Use-with-Puppeteer.md),
[Playwright](docs/Use-with-Playwright.md), and
[MCP](docs/Use-the-MCP-server.md) guides cover their capture APIs and limits.

### Start the CDP server

```bash
inbrowse serve --port 9222

# With stealth mode (anti-detection + tracker blocking)
inbrowse serve --port 9222 --stealth
```

### Scrape in parallel

```bash
inbrowse scrape url1 url2 url3 ... \
  --concurrency 25 \
  --eval "document.querySelector('h1').textContent" \
  --format json

# Suppress scrape progress on stderr for script-friendly output
inbrowse scrape https://example.com --quiet --format json

# Scrape workers inherit the global proxy
inbrowse --proxy http://127.0.0.1:8080 scrape https://example.com https://news.ycombinator.com
```

## Puppeteer / Playwright

[Media preference emulation](docs/CDP-media.md) covers color-scheme and reduced-motion overrides.

### Puppeteer

```bash
npm install puppeteer-core
```

```javascript
import puppeteer from 'puppeteer-core';

const browser = await puppeteer.connect({
  browserWSEndpoint: 'ws://127.0.0.1:9222/devtools/browser',
});

const page = await browser.newPage();
await page.goto('https://news.ycombinator.com');

const stories = await page.evaluate(() =>
  Array.from(document.querySelectorAll('.titleline > a'))
    .map(a => ({ title: a.textContent, url: a.href }))
);
console.log(stories);

await browser.disconnect();
```

### Playwright

```bash
npm install playwright-core
```

```javascript
import { chromium } from 'playwright-core';

const browser = await chromium.connectOverCDP({
  endpointURL: 'ws://127.0.0.1:9222',
});

const page = await browser.newContext().then(ctx => ctx.newPage());
await page.goto('https://en.wikipedia.org/wiki/Web_scraping');
console.log(await page.title());

await browser.close();
```

### Form submission & login

```javascript
await page.goto('https://quotes.toscrape.com/login');
await page.evaluate(() => {
  document.querySelector('#username').value = 'admin';
  document.querySelector('#password').value = 'admin';
  document.querySelector('form').submit();
});
// Inbrowse handles the POST, follows the 302 redirect, maintains cookies
```

## Benchmarks

Page load:

| Page | Inbrowse | Chrome |
|------|---------|--------|
| Static HTML | **51 ms** | ~500 ms |
| JS + XHR + fetch | **84 ms** | ~800 ms |
| Dynamic scripts | **78 ms** | ~700 ms |

The full benchmark suite (WPT conformance, obstacle course, real-world corpus, and vs-Chrome speed) lives in a separate repo, with each track's results published as it runs: https://github.com/incordai/Inbrowse-benchmark

## Host privacy

An agent browsing with Inbrowse does not reveal the machine it runs on. Pages
see a fixed browser profile rather than the host's own details, with or
without `--stealth`:

| A page reads | It gets | Not the host's |
|---|---|---|
| Timezone (`Intl`, `Date`) | `Europe/Berlin`, or `INBROWSE_TIMEZONE` | real zone, on Linux, macOS and Windows |
| Language and locale | `en-US` | OS language and region |
| `navigator.platform`, user agent | a Chrome-on-Windows profile | real OS and browser |
| CPU cores, device memory, screen | values from a fixed set | real hardware |
| Fonts | fonts bundled with Inbrowse | installed fonts, unless `--font-dir` is passed |
| WebRTC | no network candidates | local IP addresses |

The one thing the profile cannot hide is the **public IP address**: sites see
the address the request leaves from. Route through a proxy to hide it, for
example a VPN client's local SOCKS5 proxy mode:

```bash
inbrowse --proxy socks5://127.0.0.1:40000 fetch https://example.com --dump text
```

## Stealth Mode

Build with `--features render,stealth`, then enable stealth at runtime with the
global `--stealth` flag. The stealth build includes the complete rendering
engine; enabling stealth does not remove screenshot, screencast, PDF, CDP, or
MCP functionality.

### Anti-fingerprinting
- Per-session fingerprint randomization (GPU, screen, canvas, audio, battery)
- Realistic `navigator.userAgentData` (Chrome 145, high-entropy values)
- `event.isTrusted = true` for dispatched events
- Hidden internal properties (`Object.keys(window)` safe)
- Native function masking (`Function.prototype.toString()` → `[native code]`)
- `navigator.webdriver = undefined` (matches real Chrome)

### Tracker Blocking
- 3,520 domains blocked
- Blocks analytics, ads, telemetry, and fingerprinting scripts
- Prevents trackers from loading entirely
- Enabled automatically with `--stealth`

## CDP API

Inbrowse implements the Chrome DevTools Protocol for Puppeteer/Playwright compatibility.

| Domain | Methods |
|--------|---------|
| **Target** | createTarget, closeTarget, attachToTarget, createBrowserContext, disposeBrowserContext |
| **Page** | navigate, getFrameTree, lifecycleEvents, captureScreenshot, start/stopScreencast, printToPDF (selectable text, headers/footers, outline, `@page` size) |
| **Runtime** | evaluate, callFunctionOn, getProperties, addBinding |
| **DOM** | getDocument, querySelector, querySelectorAll, getOuterHTML, resolveNode |
| **Network** | enable, setCookies, getCookies, setExtraHTTPHeaders, setUserAgentOverride; webSocketCreated, webSocketWillSendHandshakeRequest, webSocketHandshakeResponseReceived, webSocketFrameSent/Received, webSocketFrameError, webSocketClosed events |
| **Fetch** | enable, continueRequest, fulfillRequest, failRequest (live interception), takeResponseBodyAsStream |
| **IO** | read, close (stream a large response body in chunks) |
| **Storage** | getCookies, setCookies, deleteCookies |
| **Input** | dispatchMouseEvent, dispatchKeyEvent, dispatchTouchEvent, dispatchDragEvent, setInterceptDrags |
| **LP** | getMarkdown (DOM-to-Markdown conversion) |

To download a large resource without one giant `Network.getResponseBody` blob, call `Fetch.takeResponseBodyAsStream` then read it in chunks with `IO.read` / `IO.close`. Response bodies over the cache limit (`INBROWSE_NETWORK_BODY_BUFFER_BYTES`, default 2 MiB) are not retained, so raise that limit when you intend to stream large downloads.
## CLI Reference

### Tuning V8

Inbrowse embeds V8 directly. Use `--v8-flags` to pass raw flags through to V8, same syntax as Chromium's `--js-flags` and Node's command-line flags. Most common use is raising the heap cap to fix `JavaScript heap out of memory` on JS-heavy pages:

```bash
inbrowse --v8-flags "--max-old-space-size=4096" fetch <url>
```

### Heavy SPAs (script execution budget)

Inbrowse caps the page's script-execution phase so one slow or hung page cannot stall a worker. The default budget is 30s; pages that finish sooner return immediately, so the cap only affects pages that keep running. A very heavy React/Vue/Angular SPA on a slow network can need more time to boot before it fires its data requests. Raise the budget with `INBROWSE_SCRIPT_DEADLINE_MS` (milliseconds), and pair it with a matching navigation timeout in your CDP client:

```bash
INBROWSE_SCRIPT_DEADLINE_MS=60000 inbrowse serve --port 9222
```

Modules that enhance an already-rendered page have a separate 3s per-module budget so one non-essential module cannot hold navigation open. Raise it for legitimate long-running modules such as a Vite HMR client:

```bash
INBROWSE_MODULE_BUDGET_MS=10000 inbrowse serve --port 9222
```

An unmounted SPA shell already gives its app modules the full `INBROWSE_SCRIPT_DEADLINE_MS` budget. `INBROWSE_FETCH_TIMEOUT_MS` controls the module's network request, not its evaluation time. See [Environment variables](docs/Environment-variables.md) for the complete timeout model.

### `inbrowse serve`

Start a CDP WebSocket server.

| Flag | Default | Description |
|------|---------|-------------|
| `--port` | `9222` | WebSocket port |
| `--proxy` | — | HTTP/SOCKS5 proxy URL |
| `--stealth` | off | Enable anti-detection + tracker blocking |
| `--workers` | `1` | Number of parallel worker processes |
| `--worker-max-connections` | off | Recycle a worker after N CDP connections (needs `--workers` > 1) |
| `--worker-max-rss` | off | Recycle a worker above this many MB resident (Linux) |
| `--worker-max-age` | off | Recycle a worker after this many seconds |
| `--worker-drain-timeout` | `120` | Seconds a replaced worker may finish its live connections before it is killed |
| `--font-dir` | — | Recursively load fonts once per worker (repeatable; render build) |
| `--obey-robots` | off | Respect robots.txt |

With multiple workers, exited children are reaped and restarted. New connections
use ready workers only; if none are ready, the balancer returns HTTP 503. A crash
still closes that worker's existing sessions, which clients must reconnect.

The `--worker-max-*` options recycle workers gracefully: a replacement starts
first, new connections go only to it, and the old worker keeps serving the
connections it already has until they close (or the drain timeout passes), then
exits. Each recycle is logged with its reason.
Worker errors remain visible on stderr.

### `inbrowse fetch <URL>`

Fetch and render a single page.

| Flag | Default | Description |
|------|---------|-------------|
| `--dump` | `html` | Output: `html`, `text`, `links`, `markdown`, `assets` (NDJSON of every sub-resource URL the page references), or `original` (raw response body) |
| `--eval` | — | JavaScript expression to evaluate |
| `--wait-until` | `load` | Wait: `load`, `domcontentloaded`, `networkidle0` |
| `--timeout` | `30` | Maximum navigation time in seconds |
| `--wait` | adaptive, up to `5` | Post-load settling; an explicit value is a fixed delay in seconds |
| `--selector` | — | Wait for CSS selector |
| `-s`, `--screenshot` | — | Write a PNG screenshot (single URL; render-enabled build) |
| `--font-dir` | — | Recursively load fonts for rendering, e.g. system fonts (repeatable; render build) |
| `--stealth` | off | Anti-detection mode |
| `--output` | — | Write dump or eval output to a file |
| `--quiet` | off | Suppress banner |
| `--proxy` | — | Inherited global HTTP/SOCKS5 proxy URL |

### `inbrowse scrape <URL...>`

Scrape multiple URLs in parallel with worker processes.

`scrape` and `fetch --file ... --dump original` keep at most `--concurrency`
jobs active, starting the next URL as soon as a slot becomes available. Results
remain in input order. URL input and completed results are still held in memory.

| Flag | Default | Description |
|------|---------|-------------|
| `--concurrency` | `10` | Parallel workers |
| `--eval` | — | JS expression per page |
| `--format` | `json` | Output: `json` or `text` |
| `--quiet` | off | Suppress scrape progress on stderr |
| `--proxy` | — | Inherited global HTTP/SOCKS5 proxy URL for all workers |

## MCP (Model Context Protocol)

Inbrowse ships an MCP server that exposes browser automation tools to AI agents (Claude Desktop, Cursor, etc.).

### Start

**stdio** (default) — for Claude Desktop and MCP clients that launch a subprocess:

```bash
inbrowse mcp
```

**HTTP** — for clients that connect over the network:

```bash
inbrowse mcp --http --port 8080
# endpoint: http://127.0.0.1:8080/mcp
```

Optional flags (both transports):

| Flag | Description |
|------|-------------|
| `--proxy <URL>` | HTTP/SOCKS5 proxy |
| `--user-agent <UA>` | Custom User-Agent string |
| `--font-dir <DIR>` | Load additional fonts before rendering (repeatable; render build) |
| `--stealth` | Enable anti-detection mode |

### Claude Code plugin

The `inbrowse` plugin adds the MCP server to Claude Code with skills for
browsing, reading files and research, and answers Claude Code's WebFetch
through Inbrowse. Put the `inbrowse` binary on your `PATH` first (or set the
plugin's **Inbrowse binary** option to its path; the plugin ships no
executable), then:

```bash
claude plugin marketplace add incordai/Inbrowse
claude plugin install inbrowse@inbrowse
```

`/mcp` then lists `inbrowse` with its tools. Options and details are in
[plugin/inbrowse/README.md](plugin/inbrowse/README.md).

### Claude Desktop config

```json
{
  "mcpServers": {
    "inbrowse": {
      "command": "inbrowse",
      "args": ["mcp"]
    }
  }
}
```

### Tools

| Tool | Description |
|------|-------------|
| `browser_navigate` | Navigate to a URL (`url`, optional `waitUntil`: `load` / `domcontentloaded` / `networkidle0`) |
| `browser_snapshot` | Return the current page URL, title, readable body text, and element references |
| `browser_screenshot` | Return the current page as an MCP PNG image (render-enabled build) |
| `browser_pdf` | Return the current page as an embedded PDF resource (render-enabled build) |
| `browser_click` | Click by current snapshot reference or CSS selector |
| `browser_fill` | Set an input value by reference or selector (triggers `input` + `change`) |
| `browser_type` | Append text to an input |
| `browser_press_key` | Dispatch a keyboard event (`key`, optional `selector`) |
| `browser_select_option` | Select an `<option>` by value or text |
| `browser_evaluate` | Evaluate a JavaScript expression and return the result |
| `browser_wait_for` | Wait for a CSS selector to appear (`selector`, optional `timeout` in seconds) |
| `browser_network_requests` | List network requests made by the current page |
| `browser_console_messages` | Return console messages logged by the page |
| `browser_close` | Close the page and reset browser state |
| `document_extract` | Text of a PDF, Office, OpenDocument, spreadsheet, CSV or text file (`source` URL, or a local path on stdio) |
| `media_extract` | Frames, 16 kHz mono WAV and captions from a video, audio or WebVTT file; `ocr: true` adds each frame's text (`media` feature) |
| `image_ocr` | Text in an image, with line boxes and confidence, from the OCR model in the Incord memory engine ([Watchman](https://github.com/incordai/Watchman-GUI) runs it) |
| `social_search` | Recent posts with engagement counts from Reddit, X, Hacker News, GitHub or Polymarket |
| `social_thread` | One Reddit, Hacker News or GitHub post with its top comments |

The MCP server exposes still-image and PDF output. Use CDP when you need the
streaming `Page.startScreencast` protocol.

## License

Apache-2.0. Copyright 2026 Incord AI. See [LICENSE](LICENSE) and [NOTICE](NOTICE).

Inbrowse is based on [obscura](https://github.com/h4ckf0r0day/obscura) (Apache-2.0).

---
