# Changelog

All notable changes to Inbrowse are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Claude Code plugin `inbrowse` (`plugin/inbrowse`) and a marketplace at the
  repository root: `claude plugin marketplace add incordai/Inbrowse`, then
  `claude plugin install inbrowse@inbrowse`. It runs the MCP server, adds the
  `browse-web`, `read-files` and `social-research` skills, and answers WebFetch
  through Inbrowse. The binary path, OCR engine address and key are plugin
  options.
- SDK `Page::evaluate_awaited(js, timeout)`: the `fetch --eval` reading, with a
  returned promise awaited and one time bound for the run and the wait.
- `fetch --script-timeout <SECONDS>`, MCP `browser_navigate` `scriptTimeoutMs`
  and `INBROWSE_SCRIPT_TIMEOUT_MS`: the time one page script may run (default
  5 s) before it is stopped. Stopped and skipped scripts are reported:
  `Warning:` lines on `fetch` stderr, a `warnings` field per URL in `scrape`,
  and warning lines in MCP `browser_navigate` and `browser_tab_new` (#27).
- `scrape --file <PATH|->`, and a `-` URL reads the list from stdin (#11).
- `extract` output has an always-present `warnings` array (CLI and MCP
  `document_extract`) naming content that was skipped or is probably missing
  (#32).
- `extract --password` (CLI) and `password` (MCP) open encrypted PDFs; a missing
  or wrong password is a clear error (#24).
- `new FormData(form, submitter)` and `select.selectedOptions` (#1, #2).
- `fetch --dump json` and MCP `browser_markdown` with `format: "json"`: the
  page as one JSON object (`url`, `status`, `title`, `description`, `lang`,
  `publishedTime`, `content`, `links`, `images`), modelled on Jina Reader's
  response.
- `fetch --boilerplate keep|drop`, MCP and CDP `boilerplate` options.
- Markdown: code fences carry the language, `~~strikethrough~~`, and a `data:`
  image keeps its alt text without its payload. Conversion is checked against
  test cases adapted from jina-ai/reader.
- On-demand `Response` streams, `window.event`, the `DOMImplementation` global,
  distinct HTML interface brands, worker-scope built-ins, custom element
  reactions, retained render invalidation with shadow roots, and lifecycle
  replay for late `Page.setLifecycleEventsEnabled` subscribers (from upstream
  obscura).

### Changed

- **Breaking:** `fetch --eval` now fails (stderr message, non-zero exit) when
  the script throws, a returned promise rejects, the source does not parse, or
  it runs out of time. It used to print `null` and exit 0, so a failure looked
  like a result (#31).
- **Breaking (SDK):** `Page::evaluate`, `Page::content`, `Page::query_selector`,
  `Element::text` and `Element::attribute` return `Result`. A JavaScript failure
  is `Error::JsEval` instead of `null`, an empty string or `None`.
- `scrape --eval` reports a failed evaluation in the URL's `error` field and
  keeps its title; the text format prints the error.
- CDP DOM and Input commands whose page-side step throws now answer with that
  error instead of success, and `DOM.focus` / `DOM.setFileInputFiles` on a node
  that is not an element is an error.
- `--dump markdown`, `--dump json` and `--dump text`, MCP `browser_markdown`
  and CDP `LP.getMarkdown` leave out navigation, the page header and footer,
  and sidebars by default; `--boilerplate keep` (MCP and CDP
  `boilerplate: "keep"`) gives the full page. A `<header>` or `<footer>`
  inside an article, main or section is kept.
- Markdown link and image URLs are absolute, resolved against the final URL or
  `<base href>`.
- A document's script phase is capped at the navigation timeout as well as
  30 s, so `fetch --timeout 5` no longer spends 30 s in a looping page script
  (#27).
- `--dump text` follows the `innerText` rules: a tab between table cells and a
  line per row, empty blocks add no blank lines, a `<select>` shows its
  selected option, `<pre>` keeps its whitespace and `hidden` content is
  skipped (#14).
- Markdown output (`--dump markdown`, MCP `browser_markdown`, CDP
  `LP.getMarkdown`) indents nested lists, numbers ordered lists from `start`,
  drops whitespace-only lines, joins block content inside a link with a space
  and keeps `<pre>` code verbatim (#15).
- `<canvas>`, `<video>`, `<audio>`, `<iframe>`, `<embed>` and `<object>` are
  inline-level by default like `<img>`, so they share a line (#17).
- Form submission url-encodes as `application/x-www-form-urlencoded`
  (`'`, `(`, `)` and `~` are percent-encoded) and includes file inputs.
- `window.onunhandledrejection` is `null` by default instead of a handler
  that cancelled every rejection event.
- `extract`: tables in DOCX, PPTX, ODT and ODP are tab-separated rows with
  merged cells kept aligned (#20); DOCX headers, footers, footnotes, endnotes
  and comments and PPTX speaker notes are separate sections (#19, #22).
- Network-idle waits use the navigation timeout, count stealth requests, and
  fail at the timeout instead of reporting a false idle (from upstream
  obscura).
- Windows builds link the static C runtime, the one V8's prebuilt library is
  built against, so one process no longer mixes two C runtimes (linker
  warning LNK4098). libvpx for Windows media builds now uses the vcpkg
  `x64-windows-static` triplet.

### Fixed

- Markdown leaves out content hidden by `display: none` or
  `visibility: hidden`, keeps blank lines in code blocks, and no longer leaves
  stray markers for empty headings or emphasis or runs a heading into the text
  before it. `--dump text` leaves out style-hidden content in render builds,
  and `--dump links` resolves against `<base href>`.
- The MCP server no longer keeps its pages open forever. After
  `--idle-timeout` seconds (default 300, `0` turns it off) with no request it
  closes every page and keeps cookies and storage; the next call opens a new
  page and its result says the old one was closed (#33). The stdio transport
  no longer drops a partly read request when a page task finishes first.
- README: the memory, size and speed table now holds our own measurements of
  the release downloads next to headless Chrome on the same machine, with the
  conditions for each figure, instead of the upstream project's figures (#29).
- CDP: iframes are real frames. Each child frame gets a default execution
  context and the page's isolated worlds; `Runtime.evaluate` and
  `callFunctionOn` run in the frame of the context or object; `DOM.describeNode`
  on an `<iframe>` returns `frameId` and `contentDocument`, and
  `DOM.getFrameOwner` is new; DOM, box-model and Input commands reach nodes
  inside frames. Puppeteer `page.frames()`, `contentFrame()`,
  `frame.$eval`/`evaluate`/`type` and Playwright `frameLocator` work (#3).
- Removing or moving an iframe detaches its frame (`Page.frameDetached`,
  `Runtime.executionContextDestroyed`) in the same command, as in Chrome (#3).
- A page's `load` event waits for its child frames' scripts and load events,
  `data:` iframes load their document, and a same-origin frame's
  `window.parent`, `top` and `frameElement` are the real objects (#3).
- `Runtime.getProperties` handles can be described and passed to
  `callFunctionOn` (#3).
- `DOM.getBoxModel` and `DOM.getContentQuads` return the element's real boxes,
  or Chrome's "Could not compute box model." error for an element without
  one, instead of a made-up box. Callers that relied on the made-up box for
  hidden elements now get the error.
- Screenshots (`fetch --screenshot`, `Page.captureScreenshot` with clip and
  full page, `browser_screenshot`) and PDFs (`Page.printToPDF`, `browser_pdf`)
  paint iframe content. Same-origin, `srcdoc` and `data:` frames, nested ones
  included, render inside the frame's content box, clipped to it (#4).
- Iframe documents load their subresources through the page's resource
  pipeline: linked and `@import`ed stylesheets, images, CSS backgrounds and
  web fonts, under the page's network policy, private-network gate, block
  list, cookies and referrer rules. The sheets apply to the frame's style,
  layout and `getComputedStyle` (#4).
- A frame's viewport, `innerWidth`/`innerHeight` and `matchMedia` follow the
  iframe's content box (`srcdoc` frames were fixed at 300x150), and `resize`
  fires in the frame when that box changes (#4).
- `Accessibility.getFullAXTree` returns one connected tree rooted at a
  focusable, titled `RootWebArea` with `frameId`, so Puppeteer and Playwright
  accessibility snapshots no longer collapse to a single node. Nodes hidden
  from assistive technology stay in the tree as ignored nodes with Chrome's
  reasons, and `depth` and `frameId` are honoured. `getRootAXNode`,
  `getChildAXNodes`, `getPartialAXTree` and `queryAXTree` are implemented, and
  unknown `Accessibility` methods are errors instead of empty successes (#16).
- `Performance.getMetrics` returns Chrome's page metrics (`Nodes`,
  `JSEventListeners`, `LayoutCount`, `ScriptDuration`, `TaskDuration`, heap
  sizes and the rest) with measured values while `Performance.enable` is on
  (#28).
- `Page.printToPDF` header and footer dates use the page's time zone, including
  an `Emulation.setTimezoneOverride`, instead of UTC (#18).
- `Emulation.setTimezoneOverride` now changes the page's `Date`,
  `Date.prototype.toString` and `Intl.DateTimeFormat` zone, keeps it across
  navigation, restores the default for an empty id and rejects unknown ids
  with `Invalid timezone ID`. Other pages and CDP connections keep their own
  zone (#6).
- Checking a radio button from script (`radio.checked = true`) unchecks the
  rest of its group, so a form no longer submits two values for one group.
- `fetch --selector` narrows `--dump html|text|markdown|links` to the first
  match, as documented, after waiting for it. A selector that never matches or
  is not valid CSS exits non-zero; `assets`, `cookies` and `original` refuse
  it (#12).
- `fetch` says on stderr when its adaptive settle returned before timers due
  within its ceiling (`note: 2 timers pending (next in 1.0s)`), and
  `--settle-timers <MS>` waits for one-shot timers due within `MS`. The settle
  rule is documented in the CLI reference (#13).
- `scrape --eval` runs on the page after the same adaptive settle as
  `fetch --eval`, instead of at the load event, and takes `--settle-timers`
  too (#13).
- Form submission sends the current `<select>` choice, not the markup default,
  for `selectedIndex`, `value`, `option.selected` and multiple selects; MCP
  `browser_fill_form` with a select submits the chosen option. `FormData` and
  native submission share one entry list (disabled fieldsets and options, the
  submit button, image-button coordinates, `form=` owners, textarea newlines)
  (#1).
- `select.selectedIndex` follows the specification (coercion, out of range or
  -1 deselects, one selected option in a single select) (#2).
- Text inputs and textareas have selection state: the caret starts at the end,
  offsets clamp, `select()` and `setSelectionRange()` fire `select`, and
  `setRangeText` works on an empty value. Over CDP, Ctrl/Cmd+A and the
  `selectAll` command select all, a triple click selects the value, a double
  click selects a word, and typing replaces the selection (#5).
- Uncaught script errors and unhandled promise rejections appear as `[error]`
  entries in MCP `browser_console_messages` and as CDP
  `Runtime.exceptionThrown`, once each (#26).
- OCR splits a wide image into equal columns with a 160 px overlap floor, so
  tall lines are not cut at seams (a 9000x8000 image takes 24 requests, not
  38), and drops misread seam fragments of lines another tile read whole (#9).
- `media` extracts audio from AAC with more than two channels (5.1 and the
  other AAC-LC channel configurations) instead of returning `audio: null`,
  downmixed to mono with ITU-R BS.775 weights (centre and surrounds at 0.7071,
  LFE dropped) and normalised so it cannot clip; mono and stereo output is
  unchanged. Coupling-channel, program-config and HE-AAC tracks are still
  skipped with a warning. The channel limits are lifted in patched copies of
  `moq-audio` and `symphonia-codec-aac` under `vendor/` (#25).
- `media` applies MP4 edit lists to video, audio and captions, so times start
  at the edit start and audio stays aligned; a malformed edit list is ignored
  with a warning instead of failing the file (#10).
- `extract` refuses legacy `.doc`, `.ppt` and password-protected Office files
  by name instead of as a broken spreadsheet (#23), prints spreadsheet dates
  as ISO 8601 and reports formula cells with no stored result (#21), flags PDF
  pages with images and no text as possibly scanned and keeps reading after a
  page that fails (#24), follows PPTX presentation order and keeps ODP
  page-number and placeholder fields out of the text (#22).
- `fetch --eval` evaluates like the DevTools console, the same as MCP
  `browser_evaluate` and `scrape`: statements return their last value
  (`"var a = 1; a + 1"` is `2`), `{a: 1}` is an object, and a returned promise
  is awaited. Anything containing a statement separator used to fail to parse
  and print `null` without running (#7).
- Integral numbers print as integers everywhere (`42`, not `42.0`), matching
  `JSON.stringify` and Chrome, in `fetch --eval`, `scrape`, MCP
  `browser_evaluate` and CDP by-value results (#8).
- `scrape --eval` had no limit on synchronous script time; it now shares the
  evaluation watchdog.
- `inbrowse serve` printed the upstream project's name in its startup banner;
  it now reads Inbrowse. The render test font is renamed to match.
- `inbrowse media` and MCP `media_extract` gave some MP4 frames a time about
  40 hours past the end of the video, and picked the wrong picture for them.
  Negative B-frame offsets stored in a version 0 `ctts` box are now read as
  signed.
- `EventTarget` is now its own base class, as in browsers, instead of an alias
  of `Node`. `Node` and the `window` global inherit from it, so
  `window instanceof EventTarget` holds and a method added to
  `EventTarget.prototype` reaches `window`. Web-component polyfills rely on
  that: calling a saved `EventTarget` method on `window` threw "is not a
  function", so the page's components never rendered.
- Page JavaScript could reach the engine's internal op table through
  `__bootstrap` (from upstream obscura).
- A CSS `@import` with a multibyte character, an SVG with a short `viewBox` and
  a grid `span 0` placement no longer abort the process, and a stalled grid
  track-sizing loop ends (from upstream obscura).
- `confirm()` returns false in non-interactive runs instead of accepting (from
  upstream obscura).
- Deeply nested documents no longer overflow the CDP connection stack (from
  upstream obscura).
- Serialized HTML escapes `textarea` and `title` text, attribute values and
  `nbsp`, and writes void elements and `xml:` attributes correctly (from
  upstream obscura).
- Web Storage keeps working after a page replaces the `Storage` constructor;
  robots.txt groups and wildcard matching; DOMParser no longer depends on a
  page's `startsWith` (from upstream obscura).
- Each CDP connection gets private `localStorage` and IndexedDB, both starting
  from the profile and merged back into it, and onto disk with
  `--storage-dir`, when the connection closes (from upstream obscura).
- PBKDF2 runs on bounded blocking workers and `window.origin` is replaceable;
  `DOM.setFileInputFiles` validates the target and files before reading any
  path and honours `multiple`; stealth requests, `--dump original` downloads
  and `navigator.userAgent` honour `--user-agent` (from upstream obscura).
- Markdown extraction has no recursion limit on deeply nested pages (from
  upstream obscura).
- A page opened from a `file:` URL loads its relative images, stylesheets,
  CSS background images and iframes; a network page still never reaches local
  files.
- A `GET` or `HEAD` whose connection is reset, or closed before any response,
  is sent once more on a fresh connection instead of failing the load. Other
  methods are never replayed.
- A script that has not arrived before the navigation deadline is reported by
  name (`Warning: script ... did not load`) and the page loads without it,
  instead of the whole navigation failing. Inline scripts always run.
- `DOM.requestNode` rejects an object that is not a DOM node again, and a
  fetched iframe document starts with its iframe's content box as viewport.
- An IndexedDB transaction with no requests counts as finished once its task
  ends, so using it later throws `InvalidStateError` as in Chrome, whichever
  task runs first.
- The `background` shorthand sets `background-position`, so
  `background: url(...) center` and sprite offsets such as `-24px 0` take
  effect instead of drawing from the top left.
- A background whose tile grid is shifted by its position is clipped at the
  box's right and bottom edges instead of painting up to one tile past them.
- An SVG image with `width` and `height` but no `viewBox` scales to the size
  it is drawn at, as in Chrome (for example under `background-size: cover`).

### Security

- `zstd`, `zstd-safe` and `zstd-sys` updated to patched versions.
- `calamine` updated to 0.38.0 (RUSTSEC-2026-0436) and `quinn-proto` to
  0.11.19.
- `rustix` updated to 1.1.6 (RUSTSEC-2026-0486) and `lru` to 0.18.5
  (RUSTSEC-2026-0253).

## [0.3.0] - 2026-11-03

### Changed

- The project is now **Inbrowse** by Incord AI. The binaries are `inbrowse` and
  `inbrowse-worker`; the crates are `inbrowse` (page and navigation),
  `inbrowse-cdp`, `inbrowse-cli`, `inbrowse-dom`, `inbrowse-js`, `inbrowse-mcp`,
  `inbrowse-net`, `inbrowse-render`, `inbrowse-ssrf` and `inbrowse-sdk` (the
  embeddable Rust API).
- Environment variables are named `INBROWSE_*`. The previous names are no
  longer read, so scripts and deployments must be updated.
- One default browser identity, Chrome 145 on Windows, everywhere: pages,
  raw downloads (`fetch --dump original`), script `fetch()`/XHR, CDP version
  info and the stealth TLS fingerprint. Raw downloads and CDP used to report
  Chrome on Linux while pages reported Chrome 143 on Windows.

### Added

- `inbrowse extract <PATH|URL>`: prints the text of a PDF, Word, PowerPoint,
  OpenDocument, spreadsheet, CSV or plain-text file as JSON, with one section
  per page, slide or sheet. Inputs over 100 MiB and archives that unpack past
  256 MiB or list more than 10,000 entries are refused. Provided by the new
  `inbrowse-extract` crate and the default `extract` cargo feature.
- MCP `document_extract`: the `inbrowse extract` reader as a tool. Takes an
  http(s) URL, or a local path on the stdio server only.
- `inbrowse media <PATH|URL>` and MCP `media_extract`: still frames (PNG, at an
  interval and at scene changes), 16 kHz mono WAV audio and captions from MP4,
  M4A, WebM, Matroska and WebVTT files. Decodes H.264, VP8, VP9, AAC and Opus.
  New `inbrowse-media` crate and `media` cargo feature; VP8/VP9 need libvpx at
  build time, linked statically.
- MCP `social_search` and `social_thread`: posts with engagement counts from
  Reddit, X, Hacker News, GitHub and Polymarket, ranked and de-duplicated,
  signing in with a per-call API key, the session's cookies, or anonymously.
  New `inbrowse-social` crate and default `social` feature of `inbrowse-mcp`.
- `inbrowse ocr <PATH|URL>`, MCP `image_ocr`, and `--ocr` / `ocr: true` on
  media frames: text from images through the OCR model in the Incord memory
  engine (`INBROWSE_OCR_URL`). Images too large for the engine are sent as
  overlapping tiles and merged. New `inbrowse-ocr` crate and default `ocr`
  feature; no model or native library ships with Inbrowse.
- README "Works with Watchman": Watchman runs the engine that serves OCR and
  keeps the cookies and API keys the research tools use; Inbrowse stores
  neither, and everything also works from the CLI or MCP without it.
- `NOTICE`, `CODE_OF_CONDUCT.md` and this changelog.

### Fixed

- On Windows, the page timezone now follows the default zone and
  `INBROWSE_TIMEZONE`. Previously pages there saw the host's real zone.
- Several tests that waited a fixed time for a child frame or event no longer
  fail when the machine is busy.

The `v0.1.0` to `v0.2.5` tags in this repository are releases of the upstream
project credited in the README.

[Unreleased]: https://github.com/incordai/Inbrowse/compare/v0.3.0...main
[0.3.0]: https://github.com/incordai/Inbrowse/releases/tag/v0.3.0
