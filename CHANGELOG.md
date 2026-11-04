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

### Fixed

- `inbrowse serve` printed the upstream project's name in its startup banner;
  it now reads Inbrowse. The render test font is renamed to match.
- `inbrowse media` and MCP `media_extract` gave some MP4 frames a time about
  40 hours past the end of the video, and picked the wrong picture for them.
  Negative B-frame offsets stored in a version 0 `ctts` box are now read as
  signed.

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
