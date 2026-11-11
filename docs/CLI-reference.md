## `inbrowse`

Most top-level flags apply to every subcommand. `--storage-dir` is the
exception: it is declared on `serve` and `fetch`, and the top-level copy is read
only by `mcp`, which exports it to `INBROWSE_STORAGE_DIR`. Pass it after the
subcommand to use it with `serve` or `fetch`.

```
-v, --verbose                Enable info logging
-p, --port <PORT>            CDP port (default 9222)
    --proxy <URL>            HTTP or SOCKS5 proxy
    --stealth                Consistent browser fingerprint + tracker blocking
    --obey-robots            Respect robots.txt
    --user-agent <UA>        Override the User-Agent
    --storage-dir <DIR>      Persistent cookies, localStorage and IndexedDB
    --allow-private-network  Permit loopback / RFC1918 / link-local
    --v8-flags <FLAGS>       Raw V8 flags, applied at startup
-h, --help                   Help
-V, --version                Version
```

## `inbrowse fetch <URL>`

Load a URL and print its content or an evaluated expression.

```
    --dump <FORMAT>          html | text | links | markdown | original | assets | cookies
                             (default html)
    --selector <CSS>         Wait for a CSS selector, then narrow output to its first match
    --wait <SECONDS>         Fixed post-load delay; omitted uses adaptive settle (5s cap)
    --timeout <SECONDS>      Navigation timeout (default 30)
    --script-timeout <SECONDS>  Time one page script may run (default 5)
    --settle-timers <MS>     Also wait for setTimeout timers due within MS (adaptive settle)
    --wait-until <LEVEL>     domcontentloaded | load | networkidle2 | networkidle0
                             (default load)
    --user-agent <UA>        Override the User-Agent
    --proxy <URL>            HTTP or SOCKS5 proxy
    --stealth                Consistent browser fingerprint + tracker blocking (global)
-e, --eval <JS>              Evaluate JS, print the result as JSON
-o, --output <FILE>          Write to a file instead of stdout
-s, --screenshot <FILE>      Capture the settled page as PNG (single URL)
    --font-dir <DIR>         Recursively load fonts for rendering (repeatable; render build)
-q, --quiet                  Suppress info logging
-v, --verbose                Enable verbose logging
```

`--eval` reads its source the way the DevTools console does, the same as
`scrape --eval` and the MCP `browser_evaluate` tool:

- Statements return their last value: `-e "var a = 1; a + 1"` prints `2`.
- Source wrapped in braces is an object literal: `-e "{a: 1}"` prints
  `{"a":1}`.
- A returned promise is awaited: `-e "fetch('/api').then(r => r.status)"`.
- `--timeout` bounds the run and the wait for the promise together.
- Numbers print as `JSON.stringify` writes them (`42`, not `42.0`); a string
  prints without quotes.
- A thrown error, a rejected promise, a syntax error or a timeout prints the
  JavaScript error on stderr and exits non-zero. Nothing is printed on stdout.

`--selector` waits up to the settle ceiling for the selector to match, then
narrows `--dump html` (the element's outer HTML), `text`, `markdown` and
`links` to the first match. A selector that never matches, or is not valid CSS,
exits non-zero with the reason on stderr. It cannot narrow `assets`, `cookies`
or `original`, and is refused with those. With `--screenshot` it only waits.
With `--eval`, the expression runs first and the narrowed page is printed (as
with `--dump`).

`--screenshot` requires a render-enabled build. It uses a 1280×720 viewport by
default and may be combined with `--eval`; the expression runs before capture,
which is useful for scrolling or preparing page state. It is not available in
`--file` batch mode.

A page script still running after `--script-timeout` (or
`INBROWSE_SCRIPT_TIMEOUT_MS`) is stopped and the page's other scripts keep
running. All of a document's scripts together get the shorter of 30 seconds
(`INBROWSE_SCRIPT_DEADLINE_MS`) and `--timeout`. Each stopped script, and any
scripts skipped because that phase ran out, is reported on stderr as a
`Warning:` line, even with `--quiet`, because the page did not finish loading
as written. `scrape` reports the same lines in a URL's `warnings` field.

When `--wait` is omitted, Inbrowse drives timers and async work until the page
becomes quiescent, with a five-second ceiling. Quiescent means 150 ms with no
document change, plus:

- network requests and dynamically added scripts count as work for the first
  second after load, so an app's first fetch and render are waited for;
- one-shot timers (`setTimeout`) count only when due within 300 ms, and only
  during the first 500 ms; intervals never count, since they never finish.

So a page that renders after a longer timer (a 1.5 s `setTimeout`) is read
before it. When that happens, stderr gets a line such as
`note: 2 timers pending (next in 1.0s); ...` (left out with `--quiet`).
`--settle-timers MS` also waits for every one-shot timer due within `MS` of the
settle start, raising the ceiling to cover `MS` when it is longer than five
seconds. Supplying `--wait N` instead requests a fixed `N`-second delay, which
runs every timer due in that time. `--timeout` separately bounds navigation.

`--dump` values:

| Value      | Output                                                    |
| ---------- | --------------------------------------------------------- |
| `html`     | Rendered HTML (default)                                   |
| `text`     | Plain text                                                |
| `markdown` | Markdown conversion                                       |
| `links`    | Every `<a href>`, one URL per line                        |
| `assets`   | Every external resource, one JSON object per line (DOM assets plus `fetch()`/XHR requests) |
| `original` | Raw HTTP response body (binary-safe, bypasses the engine) |
| `cookies`  | All cookies in the jar as a JSON array, including HttpOnly cookies invisible to `document.cookie` |

## `inbrowse serve`

Run the CDP server. Puppeteer and Playwright connect over WebSocket.

```
-p, --port <PORT>            CDP port (default 9222)
    --host <HOST>            Bind host (default 127.0.0.1)
    --proxy <URL>            HTTP or SOCKS5 proxy
    --user-agent <UA>        Override the User-Agent
    --stealth                Consistent browser fingerprint + tracker blocking (global)
    --workers <N>            Worker processes (default 1)
    --worker-max-connections <N>  Recycle a worker after N CDP connections (--workers > 1)
    --worker-max-rss <MB>    Recycle a worker above this resident size (Linux)
    --worker-max-age <SECS>  Recycle a worker after this long
    --worker-drain-timeout <SECS>  Let a replaced worker finish live connections (default 120)
    --font-dir <DIR>         Recursively load fonts once per worker (repeatable; render build)
    --allow-file-access      Permit CDP clients to navigate to file:// URLs
    --storage-dir <DIR>      Persistent cookies, localStorage and IndexedDB
    --allow-private-network  Permit loopback / RFC1918 / link-local
-q, --quiet                  Suppress info logging
-v, --verbose                Enable info logging
```

Default endpoint is `ws://127.0.0.1:9222`.

## `inbrowse scrape [URLS]...`

Run a JS expression across many URLs in parallel.

```
-e, --eval <JS>              JS to run on each page (same reading as fetch --eval)
    --file <PATH|->          Also read URLs from a file, or from stdin with -
    --concurrency <N>        Parallel pages (default 10)
    --format <FORMAT>        Output format (default json)
    --timeout <SECONDS>      Per-URL timeout (default 60)
    --settle-timers <MS>     Also wait for setTimeout timers due within MS
    --proxy <URL>            HTTP or SOCKS5 proxy
    --stealth                Consistent browser fingerprint + tracker blocking (global)
    --allow-private-network  Permit loopback / RFC1918 / link-local
-q, --quiet                  Suppress info logging
-v, --verbose                Enable verbose logging
```

`--stealth`, `--proxy`, and `--allow-private-network` are global flags: they work before or after any subcommand, so each worker in a `scrape` run inherits stealth too.

Read URLs from stdin with `-` (one per line; blank lines and lines starting
with `#` are skipped). Positional URLs and `--file` lists are combined; stdin
can be named only once:

```bash
cat urls.txt | inbrowse scrape - --eval "document.title" --concurrency 20
inbrowse scrape --file urls.txt https://example.com/extra
```

Each page settles after load the way `fetch` without `--wait` does (until
quiet, five-second ceiling, see `fetch`), so `--eval` reads the same page
`fetch --eval` would. `--settle-timers` works as on `fetch`.

A URL whose evaluation throws, rejects or times out gets an `error` field with
the JavaScript error next to its `url` and `title`; the other URLs still run.

Requires `inbrowse-worker` next to `inbrowse` in `PATH`.

## `inbrowse mcp`

Run inbrowse as an MCP server.

```
    --http                   HTTP transport instead of stdio
    --font-dir <DIR>         Load additional fonts before rendering (repeatable; render build)
    --host <HOST>            HTTP bind host (default 127.0.0.1)
    --port <PORT>            HTTP port (default 3000)
    --proxy <URL>            HTTP or SOCKS5 proxy
    --user-agent <UA>        Override the User-Agent
    --idle-timeout <SECS>    Close pages after this long with no request (default 300, 0 = never)
    --stealth                Consistent browser fingerprint + tracker blocking (global)
    --allow-private-network  Permit loopback / RFC1918 / link-local
-v, --verbose                Enable info logging
```

The first tool call that needs a page opens the browser. After
`--idle-timeout` seconds with no MCP request the server closes every page;
cookies and storage are kept, and the next call opens a new page and says the
old one was closed.

`--host` only applies with `--http`. The default `127.0.0.1` keeps the server loopback-only. A non-loopback bind requires `INBROWSE_MCP_TOKEN` (at least 32 bytes); use `INBROWSE_MCP_ALLOWED_ORIGINS` as well when a browser-based client needs access.

Default transport is stdio. See [Use the MCP server](Use-the-MCP-server.md).

Render-enabled builds add `browser_screenshot` and `browser_pdf` to the MCP
tool list. Streaming screencasts are available through CDP rather than MCP.

## `inbrowse extract <PATH|URL>`

Print the text of one document as JSON. Reads PDF, Word (`.docx`),
PowerPoint (`.pptx`), OpenDocument text and presentations (`.odt`, `.odp`),
spreadsheets (`.xlsx`, `.xlsb`, `.xls`, `.ods`), CSV and plain text. The format
is recognised from the file's bytes, not its name. Legacy `.doc` and `.ppt`
files are not supported and are refused by name (`legacy Word .doc file;
convert it to .docx`), as are password-protected Office files.

```
    --hint <NAME|MIME>       Mark plain text as CSV (defaults to the
                             Content-Type or file name)
    --password <PASSWORD>    Password for an encrypted PDF
    --timeout <SECS>         Download plus extraction (default 60)
-o, --output <FILE>          Write the JSON to a file
```

```bash
inbrowse extract report.pdf
inbrowse extract https://example.com/prices.xlsx -o prices.json
```

```json
{"format":"pdf","text":"…","sections":[{"kind":"page","name":"Page 1","text":"…"}],"warnings":[]}
```

`sections` has one entry per page (PDF), slide (PPTX, ODP), sheet
(spreadsheets) or a single `body`. `text` joins every section with a blank
line. Tables (Word, PowerPoint, OpenDocument) and sheets come out as rows on
separate lines with cells separated by tabs; a merged cell is followed by empty
cells so columns stay aligned, and a table inside a cell is folded into that
cell. Spreadsheet dates and times are ISO 8601 (`2026-11-09`,
`2026-11-09T12:00:00`), and elapsed times are `36:00:00`.

Content that hangs off the main text is a separate section after it, so a
caller can keep or drop it by `kind`:

| `kind` | `name` | Source |
| --- | --- | --- |
| `header`, `footer` | `Header`, `Footer` (numbered when several differ) | DOCX headers and footers; identical and empty ones are dropped |
| `footnotes`, `endnotes`, `comments` | `Footnotes`, `Endnotes`, `Comments` | DOCX |
| `notes` | `Slide N notes`, right after `Slide N` | PPTX and ODP speaker notes |

PPTX slides follow the order in the presentation, and slide-number, header,
footer and date placeholders are left out (so are ODP page-number fields).
Slides ODP has not named are `Slide N`.

`warnings` is always present: strings of the form `code: message` for content
that was skipped or is probably missing. An empty list means nothing was lost.

| Code | Meaning |
| --- | --- |
| `no_text_layer` | A PDF page with images and no text; it may be scanned (the command does not run OCR) |
| `page_failed` | A PDF page could not be read; the pages after it still are |
| `no_cached_value` | Formula cells with no stored result, as written by libraries that do not calculate; they print empty |
| `part_missing` | A part the package lists (header, notes page, slide) is not in the file |
| `part_skipped` | A part could not be parsed and was left out |
| `formulas_unreadable` | The formulas of a sheet could not be checked for stored results |
| `password_ignored` | `--password` was given for a file that is not a PDF |

URLs are downloaded like `fetch --dump original`, so `--proxy`, `--stealth`,
`--user-agent` and the private-network block apply. Documents are untrusted:
inputs over 100 MiB, archives that unpack past 256 MiB or list more than 10,000
entries are refused, and a parser failure is reported as an error. The command
is part of the default `extract` cargo feature.

## `inbrowse social`

Research sources from the command line, the same code as the MCP
`social_search` and `social_thread` tools ([Use the MCP server](Use-the-MCP-server.md)).

```
inbrowse social search <SOURCE> <QUERY>   reddit | x | hackernews | github | polymarket
    --days <N>          Window in days, 1 to 365 (default 30)
    --limit <N>         Most items, 1 to 100 (default 25)
    --sort <ORDER>      relevance, recent or top (default relevance)
    --community <SUB>   Subreddit (Reddit only)
    --cookies <FILE>    JSON cookie list for signed-in access
-o, --output <FILE>     Write the JSON to a file

inbrowse social thread <URL>              Reddit, Hacker News or GitHub post
    --comments <N>      Top comments, 0 to 50 (default 10)
    --cookies <FILE>
-o, --output <FILE>
```

```bash
inbrowse social search hackernews "rust borrow checker" --days 7
INBROWSE_REDDIT_CLIENT_ID=... INBROWSE_REDDIT_CLIENT_SECRET=... \
  inbrowse social search reddit "local llm" --community LocalLLaMA --sort top
inbrowse social search x "inbrowse" --cookies x-cookies.json
inbrowse social thread https://news.ycombinator.com/item?id=50069499
```

API keys are read from your own environment and used for that run only:
`INBROWSE_REDDIT_CLIENT_ID` and `INBROWSE_REDDIT_CLIENT_SECRET` (a free app
from reddit.com/prefs/apps), `INBROWSE_X_BEARER_TOKEN` (X's paid API, last 7
days) and `INBROWSE_GITHUB_TOKEN`. Signed-in access uses cookies from
`--cookies` (the same JSON list as a `--storage-dir` `cookies.json`) or from the
`--storage-dir` profile. X without an API key needs the account's `auth_token`
and `ct0` cookies. Using an account's cookies acts as that account and can get
it restricted; keep volume low. Errors start with `login_required`,
`account_challenge`, `rate_limited:<seconds>`, `quota_exceeded`, `blocked`,
`invalid` or `failed`.

## `inbrowse media <PATH|URL>`

Take still frames, speech-ready audio and captions from one video, audio or
WebVTT file, and print a JSON report. Reads MP4, M4A, WebM and Matroska files,
decoding H.264, VP8 and VP9 video and AAC and Opus audio. Other codecs (H.265,
AV1, MP3, FLAC, Vorbis) are listed in `tracks` with `"supported": false` but not
decoded. MPEG-TS and FLV files are not read yet.

```
    --out-dir <DIR>          Folder for the PNG and WAV files (default: a new
                             folder under INBROWSE_MEDIA_DIR or the temp folder)
    --every <SECS>           Take a frame at least this often (default 10)
    --max-frames <N>         Most frames, 0 to 200 (default 20)
    --no-scene-changes       Only take interval frames
    --no-audio               Skip the audio track
    --max-side <PX>          Scale frames so the longer side fits (default 2048)
    --ocr                    Also read the text in each frame (see `inbrowse ocr`)
    --timeout <SECS>         Download, extraction and OCR (default 120)
-o, --output <FILE>          Write the JSON to a file
```

```bash
inbrowse media talk.mp4
inbrowse media https://example.com/clip.webm --every 5 --out-dir clip
```

```json
{"container":"mp4","duration_secs":62.0,
 "tracks":[{"kind":"video","codec":"h264","width":1280,"height":720,"supported":true},
           {"kind":"audio","codec":"aac","sample_rate":48000,"channels":2,"supported":true}],
 "audio":{"format":"wav","sample_rate":16000,"channels":1,"duration_secs":62.0,"path":"…/audio-16k-mono.wav"},
 "frames":[{"time_secs":0.0,"reason":"interval","width":1280,"height":720,"path":"…/frame-0000-0ms.png"}],
 "captions":[],"warnings":[],"out_dir":"…"}
```

Frames are taken every `--every` seconds and where the picture changes sharply
(`"reason": "scene_change"`). If the file is too long for `--max-frames` at that
interval, the interval is widened and a warning says so. Audio is the first
decodable audio track, mixed to mono and resampled to 16 kHz. Captions come from
WebVTT and `tx3g` subtitle tracks, or from a `.vtt` input.

The output folder is not deleted; remove it when done. Inputs over 100 MiB are
refused and only the first two hours are read. URLs are downloaded like
`inbrowse extract`. The command needs the `media` cargo feature, which needs
libvpx at build time; release binaries include it. See
[Build from source](Build-from-source.md#video-and-audio-media-feature).

With `--ocr`, each frame gains `"ocr": {"text": …, "lines": […]}`, read as
`inbrowse ocr` reads an image. Raise `--timeout` for long files: OCR shares it.

## `inbrowse ocr <PATH|URL>`

Read the text in one image (PNG, JPEG, WebP, GIF or BMP) and print it as JSON.
The OCR model runs in the Incord memory engine, not in Inbrowse;
[Watchman](https://github.com/incordai/Watchman-GUI) runs the engine locally:

| Variable | Meaning |
|---|---|
| `INBROWSE_OCR_URL` | The engine's address (default `http://127.0.0.1:9180`, where Watchman runs it) |
| `INBROWSE_OCR_KEY` | Sent as `Authorization: Bearer`, when the engine wants a key |
| `INBROWSE_OCR_TIMEOUT` | Seconds for one engine request (default 120) |

```
    --timeout <SECS>         Download plus OCR (default 120)
-o, --output <FILE>          Write the JSON to a file
```

```bash
inbrowse ocr receipt.jpg
INBROWSE_OCR_URL=http://10.0.0.5:9180 inbrowse ocr https://example.com/scan.png
```

```json
{"source":"receipt.jpg","untrusted":true,"text":"TOTAL 42.00\nThank you",
 "lines":[{"text":"TOTAL 42.00","box":[40,610,220,28],"confidence":0.97}],
 "width":800,"height":1200,"tiles":1,"model":"…","elapsed_ms":310,"warnings":[]}
```

`box` is `[x, y, width, height]` in pixels of the whole image. No image is
refused for its size: one the engine cannot take whole (over 16 MB, over 64
megapixels, wider than 43,690 px, or not PNG or JPEG) is decoded and sent as
overlapping tiles, and lines read twice along a seam are merged. A tile the
engine still refuses is halved again; only a region that stays too large after
that is skipped, with a warning naming it. Inputs up to 256 MiB are read.

Errors start with `ocr_unavailable` (the engine is not running, has no OCR
endpoint, refused the key, or could not load its model),
`ocr_rejected` (not a readable image) or `ocr_timed_out`. A busy or still-loading
engine is retried after the wait it asks for. The command needs the default
`ocr` cargo feature.

## Worker navigation responses

`inbrowse-worker` accepts newline-delimited JSON commands on stdin and emits one
JSON response per command on stdout. For example:

```json
{"cmd":"navigate","url":"https://example.com/"}
```

A completed navigation includes the final document's HTTP status alongside the
existing title and URL:

```json
{"ok":true,"result":{"title":"Example Domain","url":"https://example.com/","status":200}}
```

`status` is the main document's final HTTP response code after redirects, not a
script, image, or child frame's response. HTTP errors such as 404 and 503 still
return `ok: true`: the server responded and the error document can be inspected
with `dump_html`, `dump_text`, or `evaluate`. Navigation without an HTTP response,
such as `about:blank`, a `data:` URL, or a tracker blocked before the request,
returns `status: null`. A transport or
navigation failure retains the existing `{"ok":false,"error":"..."}` envelope
without a `result` object.

The status field is additive. Navigation timing and the other worker commands
are unchanged.

## Worker navigation readiness

`inbrowse-worker` accepts newline-delimited JSON commands on stdin and writes
one response per command to stdout. Select a readiness mode per navigation:

```json
{"cmd":"navigate","url":"https://example.com","waitUntil":"domcontentloaded"}
```

`waitUntil` accepts `load`, `domcontentloaded`, `networkidle0`, or `networkidle2`.
Omitting it preserves the existing `load` default. The option applies only to
that request. Invalid values return an `ok: false` response without navigating;
the worker remains available for subsequent commands.

These modes use the existing `Page::navigate_with_wait` behavior. Parser scripts
run in every mode. Network-idle modes additionally drive the event loop until
active requests remain at or below zero or two, respectively, for 500 ms, with
the engine's existing five-second idle deadline. `domcontentloaded` uses the
engine's DOM readiness boundary; it does not change script execution or resource
warmup semantics. Successful responses retain their existing `title`, `url`, and `status`
fields.
