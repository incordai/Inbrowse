# Changelog

All notable changes to Inbrowse are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed

- The project is now **Inbrowse** by Incord AI. The binaries are `inbrowse` and
  `inbrowse-worker`; the crates are `inbrowse` (page and navigation),
  `inbrowse-cdp`, `inbrowse-cli`, `inbrowse-dom`, `inbrowse-js`, `inbrowse-mcp`,
  `inbrowse-net`, `inbrowse-render`, `inbrowse-ssrf` and `inbrowse-sdk` (the
  embeddable Rust API).
- Environment variables are named `INBROWSE_*`. The old `OBSCURA_*` names are
  no longer read, so scripts and deployments must be updated.

### Added

- `inbrowse extract <PATH|URL>`: prints the text of a PDF, Word, PowerPoint,
  OpenDocument, spreadsheet, CSV or plain-text file as JSON, with one section
  per page, slide or sheet. Inputs over 100 MiB and archives that unpack past
  256 MiB or list more than 10,000 entries are refused. Provided by the new
  `inbrowse-extract` crate and the default `extract` cargo feature.
- `NOTICE`, `CODE_OF_CONDUCT.md` and this changelog.

### Fixed

- On Windows, the page timezone now follows the default zone and
  `INBROWSE_TIMEZONE`. Previously pages there saw the host's real zone.
- Several tests that waited a fixed time for a child frame or event no longer
  fail when the machine is busy.

Inbrowse is based on obscura; the `v0.1.0` to `v0.2.5` tags in this repository
are obscura releases.

[Unreleased]: https://github.com/incordai/Inbrowse/commits/main
