# Inbrowse plugin for Claude Code

Puts [Inbrowse](https://github.com/incordai/Inbrowse), the headless browser in
Rust by Incord AI, into Claude Code.

- **MCP server** (`inbrowse mcp`): a browser that runs JavaScript, with forms,
  clicks, tabs, screenshots and PDF; `document_extract` for PDF, Office,
  OpenDocument and spreadsheet files; `image_ocr`; `media_extract` for video
  frames, audio and captions; `social_search` and `social_thread` for Reddit,
  X, Hacker News, GitHub and Polymarket.
- **Skills**: `browse-web`, `read-files` and `social-research` tell Claude which
  tool fits a task.
- **WebFetch through Inbrowse**: Claude Code's WebFetch is answered with the page
  as Inbrowse renders it, in a tab of its own, the request applied by a small
  model as WebFetch does. If Inbrowse cannot read the page, the built-in
  WebFetch runs. Turn it off with the `route_webfetch` option.

## Install

1. Install the `inbrowse` binary, 0.3.0 or later, from the
   [releases](https://github.com/incordai/Inbrowse/releases) (or build it), and
   put it on your `PATH`, or set the plugin's **Inbrowse binary** option to its
   full path. The plugin ships no executable.
2. In Claude Code:

   ```
   claude plugin marketplace add incordai/Inbrowse
   claude plugin install inbrowse@inbrowse
   ```

3. Run `/mcp`: `inbrowse` is listed with its tools.

## Options

| Option | Default | Meaning |
|---|---|---|
| `inbrowse_path` | `inbrowse` | The `inbrowse` executable; the default means it is on `PATH` |
| `ocr_url` | `http://127.0.0.1:9180` | The Incord memory engine that reads text in images ([Watchman](https://github.com/incordai/Watchman-GUI) runs it there) |
| `ocr_key` | none | The engine's key, only if it asks for one; kept in the system's secure storage |
| `route_webfetch` | on | Answer WebFetch through Inbrowse |
| `prefer_inbrowse` | on | Tell the model which Inbrowse tool fits pages, files and research |

Inbrowse never stores credentials. With Watchman, the cookies and API keys the
research tools use live in Watchman.
