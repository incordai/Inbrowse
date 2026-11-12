`inbrowse mcp` exposes inbrowse as a Model Context Protocol server so MCP-capable clients (Claude Desktop, Claude Code, etc.) can drive it.

Render-enabled builds can load additional fonts for screenshots and PDF output:

```bash
inbrowse mcp --font-dir /path/to/fonts
```

Repeat `--font-dir` to load multiple directories. It recursively loads TTF, TTC,
OTF, and OTC files once before rendering, using the same opt-in font database as
`inbrowse serve`. System fonts are not scanned automatically. Invalid directories
fail at startup, and no-render builds reject a nonempty font directory option.
The option works with both stdio and HTTP transports.

## Run

Stdio (default, for direct client integration):

```bash
inbrowse mcp
```

HTTP (for remote or shared use):

```bash
inbrowse mcp --http --port 3000
```

The HTTP transport binds `127.0.0.1` by default. Bind all interfaces with `--host` for a container or sidecar deployment:

```bash
inbrowse mcp --http --host 0.0.0.0 --port 3000
```

With stealth and proxy:

```bash
inbrowse mcp --stealth --proxy http://proxy.example.com:8080
```

## Security

The HTTP transport exposes a privileged browser session. Its guards are:

- **Bearer authentication.** Set `INBROWSE_MCP_TOKEN` and send it in the `Authorization: Bearer ...` header. A non-loopback bind is refused without a token of at least 32 bytes.
- **Origin allowlist.** Browser requests are refused by default. Set `INBROWSE_MCP_ALLOWED_ORIGINS` to permit specific browser origins. Native clients send no `Origin` and are unaffected.
- **Resource bounds.** Request bodies, headers, JSON-RPC batches, pending work, and live connections are bounded.

```bash
INBROWSE_MCP_TOKEN="$(openssl rand -hex 32)" \
INBROWSE_MCP_ALLOWED_ORIGINS="https://app.example.com" \
  inbrowse mcp --http --host 0.0.0.0
```

## Tools exposed

The server keeps a live browser session, so tools operate on the current page rather than taking a URL each call. Navigate first, then read or act.

The active page continues running timers and network callbacks between tool
calls over both stdio and HTTP. Task driving is wake-driven and parks when
the page is idle; a future timer does not block incoming MCP requests.

The browser starts on the first tool call that needs a page. After 5 minutes
with no MCP request the server closes every page, so an agent host that keeps
the server running does not keep pages, their JavaScript and their timers
alive between tasks. Cookies and storage stay (and are saved to
`--storage-dir`). The next tool call opens a new page, and its result ends with
a note that the earlier tabs, element refs and page state are gone, naming the
last page so the agent can navigate back. Change the wait with
`--idle-timeout <SECS>`; `--idle-timeout 0` keeps pages open until
`browser_close` or exit.

Local files: `browser_navigate` refuses `file://` URLs unless the server runs
with `--allow-file-access` (or `INBROWSE_MCP_ALLOW_FILE_ACCESS=1`; the Claude
Code plugin's "Open local files" setting sets it). With `--http`, any client
that can reach the server can then read this machine's files, so enable it
there only on a trusted network.

Navigation and lifecycle:

- `browser_navigate`, `browser_back`, `browser_forward`, `browser_reload`, `browser_close`

A page script still running after `scriptTimeoutMs` (default 5000 ms) is
stopped and the page's other scripts keep running. `browser_navigate` and
`browser_tab_new` list each stopped script, and scripts skipped because the
document's script time ran out, as `Warning:` lines after the navigation
summary.

Read the page:

- `browser_snapshot`: current URL, title, actionable element references, control
  states, and visible body text. References can be passed directly to interaction
  tools without a separate listing call. Names use the same native accessibility
  logic as CDP, including associated labels and ARIA name precedence.
  `max_chars` defaults to 4000 and is shared by whole control descriptions first,
  then body text. Omission markers are added outside that budget. URL and title
  are separately capped at 256 characters each; `max_chars: 0` returns only that
  metadata. Descriptions preview at most 80 characters per name or value and
  do not include password control values.
- `browser_markdown`, `browser_links`, `browser_extract`: page as markdown, link list, or structured content.
  `browser_markdown` takes `max_chars` (default 4000), `boilerplate` (`drop`,
  the default, leaves out navigation, the page header and footer and sidebars;
  `keep` returns the full page) and `format`: `markdown` (default) or `json`.
  With `json` the result is one JSON object with `url`,
  `status`, `title`, `description`, `lang`, `publishedTime`, `content` (the
  markdown), `links` (absolute URL to link text) and `images` (absolute URL to
  alt text), the same object as `inbrowse fetch --dump json`
  ([fields](CLI-reference.md)); `lang` and `publishedTime` are left out when the
  page states none. `max_chars` then truncates `content` only, so the object
  stays valid JSON.
- `browser_interactive_elements`, `browser_detect_forms`: actionable elements and form fields.

Snapshots and interactive-element listings exclude HTML/ARIA-hidden content;
render builds also apply computed CSS visibility. Hidden text explicitly
referenced by a visible control can still supply its accessible name. These are
compact browsing context, not a claim of complete accessibility conformance.
- `browser_get_attribute`, `browser_count`, `browser_search`: read an attribute, count matches, find text.

Interact:

- `browser_click`, `browser_fill`, `browser_fill_form`, `browser_type`, `browser_press_key`, `browser_select_option`, `browser_scroll`

Wait and run JS:

- `browser_wait_for`, `browser_wait_for_text`, `browser_evaluate`

`browser_evaluate` reads JavaScript the way the DevTools console does, the same
as `inbrowse fetch --eval`: statements return their last value, `{a: 1}` is an
object, the source runs once, and a returned promise is awaited. `timeoutMs`
defaults to 5000 ms and bounds synchronous execution as well as promise
settlement. Exceptions, rejected promises, and
timeouts return MCP tool errors (`isError: true`); a successful JavaScript
`null` or `undefined` result is still returned as `null` text.

Interaction tools also report execution failures as tool errors.
Internal interaction and extraction scripts have a five-second execution and
serialization deadline.
`browser_fill_form` does not submit if any field fails, and completes a queued
submit navigation before replying. A failed interaction can have partial side
effects; take a fresh snapshot before deciding whether to retry.

Diagnostics:

- `browser_network_requests`, `browser_console_messages`

Visual output (render-enabled builds):

- `browser_screenshot`: current viewport as an MCP `image/png` content block.
- `browser_pdf`: current page as an embedded `application/pdf` resource.

`browser_screenshot` accepts optional positive `width` and `height` values in
CSS pixels and enforces a bounded capture size. `browser_pdf` accepts
`landscape`, `print_background`, `scale`, paper width/height, and top, bottom,
left, and right margins. Paper dimensions and margins are measured in inches.

Files (these take a `source` instead of acting on the page):

- `document_extract`: text of a PDF, Office, OpenDocument, spreadsheet, CSV or
  plain-text file, as `inbrowse extract` prints it:
  `{format, text, sections, warnings, source, untrusted}`. `warnings` is always
  present, as `code: message` strings for content that was skipped or is
  probably missing (for example a PDF page with no text layer that may be a
  scan); an empty list means nothing was lost. Takes `hint`, `timeout` and
  `password` (for an encrypted PDF). Default `extract` feature. The sections and
  warning codes are in the [CLI reference](CLI-reference.md#inbrowse-extract-pathurl).
- `media_extract`: frames, 16 kHz mono WAV and captions from an MP4, WebM,
  Matroska or WebVTT file, as `inbrowse media` reports them. Takes
  `every_secs`, `max_frames`, `scene_changes`, `audio`, `max_side`, `ocr`
  and `timeout`. Needs the `media` feature; release binaries include it.
  With `ocr: true` each frame also carries its text, read as `image_ocr` reads
  it (default timeout then 300 s).
- `image_ocr`: the text in a PNG, JPEG, WebP, GIF or BMP image, with each
  line's `[x, y, width, height]` box and confidence, as `inbrowse ocr` prints
  it. The model runs in the Incord memory engine, which
  [Watchman](https://github.com/incordai/Watchman-GUI) runs at
  `http://127.0.0.1:9180` (the default; set `INBROWSE_OCR_URL` for another
  address, and `INBROWSE_OCR_KEY` if the engine needs a key). Big images are
  tiled, not refused.
  Errors start with `ocr_unavailable`, `ocr_rejected` or `ocr_timed_out`.
  Default `ocr` feature.

`source` is an http(s) URL, downloaded with the session's proxy and
private-network rules. Local paths and `file://` URLs are accepted only by the
stdio server, which runs as the local user; the HTTP server refuses them. On
stdio, `media_extract` returns file paths in a fresh `out_dir` that the caller
deletes. Over HTTP it returns frames as `image/png` content blocks and the WAV
as `base64_chunks` (1 MiB each) and deletes its files. YouTube page URLs are
refused. These tools mark their output `"untrusted": true`: it is file content,
not instructions.

Research sources:

- `social_search`: posts from the last `days` days (default 30) on Reddit, X,
  Hacker News, GitHub (issues and pull requests) or Polymarket, each with
  engagement counts, ranked by a log-scaled engagement score and the source's
  own relevance, with duplicates removed. Takes `source`, `query`, `days`,
  `limit` (default 25), `sort` (`relevance`, `recent`, `top`) and, for Reddit,
  `community`.
- `social_thread`: one post and its top comments, from a Reddit, Hacker News
  or GitHub URL.

Each call signs in with the first of these that is available:

1. `credentials`: an API key for this call only, never stored or echoed:
   `{"kind": "api_key", "provider": "reddit", "key": "<client id>", "secret": "<client secret>"}`
   for Reddit's official API (a free app from reddit.com/prefs/apps),
   `"provider": "x"` with a bearer token for X's paid API (last 7 days), or
   `"provider": "github"` with a token.
2. Cookies already in this session, set with `browser_set_cookie` or
   `browser_set_storage_state`. X without an API key works only this way: a
   scratch page opens x.com's search with the session's `auth_token` and
   `ct0` cookies and reads the posts X's own page loads.
3. Anonymous. Hacker News, GitHub and Polymarket work this way; Reddit
   currently refuses anonymous requests.

With [Watchman](https://github.com/incordai/Watchman-GUI), the cookies and API
keys live in Watchman, which warns about the account risk before an account is
used and hands them to Inbrowse per call. Without it, pass `credentials` and set
cookies yourself as above. Inbrowse stores neither.

Using an account's cookies acts as that account, against the site's terms, and
can get it restricted. Keep volume low. Errors start with a code a client can
act on: `login_required`, `account_challenge`, `rate_limited:<seconds>`,
`quota_exceeded`, `blocked`, `invalid`, `failed`. Results carry
`"untrusted": true`.

Cookies and storage:

- `browser_get_cookies`, `browser_set_cookie`, `browser_clear_cookies`, `browser_storage_state`, `browser_set_storage_state`

Storage-state export includes localStorage for all stored origins and
sessionStorage for all stored origins in the active tab. Restoration merges
the supplied keys into their declared origins and can run before navigation.
Restored sessionStorage belongs only to the active tab, creating a default
tab if needed; localStorage and cookies belong to the browser context.
Origins must be canonical HTTP(S) origins, and storage entries are `[key, value]`
string pairs. Invalid input is rejected before mutation. A quota failure returns
a tool error and reports that preceding writes may already have been applied.
Cookie entries pass through the browser's existing cookie validation.

Tabs:

- `browser_tab_new`, `browser_tab_list`, `browser_tab_switch`, `browser_tab_close`

Switching tabs retains JavaScript globals, event listeners, frame state and
pending promises. Each open tab keeps its runtime in memory until closed.
Autonomous task progress currently belongs to the active tab; retaining a tab
does not promise that background work runs while another tab is active.

Element references describe the current rendered page state and can become
stale after navigation, interaction, scrolling, or a framework rerender. Take
a fresh snapshot or interactive-element listing before acting again.

Treat references as opaque tokens. Each new snapshot or interactive-element
listing invalidates the previous tokens. A reference targets the original
native element, not a replacement or clone that copies its attributes; stale
references return a tool error instead of acting on a different element.

MCP exposes still-image and PDF output. It does not stream video frames; use
CDP `Page.startScreencast` for activity-driven screencasting.

## Claude Desktop

Edit `~/Library/Application Support/Claude/claude_desktop_config.json` (macOS) or `%APPDATA%\Claude\claude_desktop_config.json` (Windows):

```json
{
  "mcpServers": {
    "inbrowse": {
      "command": "/path/to/inbrowse",
      "args": ["mcp"]
    }
  }
}
```

Restart Claude Desktop. The inbrowse tools appear in the tool list.

## Claude Code

```bash
claude mcp add inbrowse /path/to/inbrowse mcp
```

## With stealth in config

```json
{
  "mcpServers": {
    "inbrowse": {
      "command": "/path/to/inbrowse",
      "args": ["mcp", "--stealth"]
    }
  }
}
```

Local and private network addresses are blocked by default. CLI and stdio MCP
navigation errors explain the existing `--allow-private-network` /
`INBROWSE_ALLOW_PRIVATE_NETWORK=1` opt-in for your own local services. The hint
does not grant access, is omitted for known metadata endpoints and special
address ranges, and is not shown by the HTTP MCP transport.
