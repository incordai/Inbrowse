`--dump markdown` converts the rendered page to markdown.

```bash
inbrowse fetch https://example.com --dump markdown
```

## What gets converted

- Headings (`<h1>` through `<h6>`)
- Paragraphs, line breaks
- Bold, italic, code spans
- Links (with `href`)
- Images (with `src` and `alt`)
- Ordered and unordered lists
- Block quotes
- Code blocks (`<pre>`, `<code>`)
- Tables

## How it is written

- Headings are ATX (`# Title`), bullets are `- `, emphasis is `*x*`, strong is
  `**x**`, strikethrough is `~~x~~`, rules are `---`.
- Link and image URLs are absolute and normalized as a browser's `a.href` is
  (`https://example.com` is written `https://example.com/`), resolved against
  the page's final URL or its `<base href>`. `javascript:` links and `#`-only links keep their text and lose
  the link.
- A `data:` image keeps its alt text and its type, not its payload:
  `![Logo](data:image/png)`.
- `<pre>` is a fenced block that keeps its text verbatim. A `language-x`,
  `lang-x` or `highlight-source-x` class, or a `data-lang`, becomes the fence's
  info string (```` ```js ````). A fence is longer than any backtick run inside.
- Tables are GFM pipe tables: bold, links and code in cells are kept, `|` is
  escaped, and a table without a header row uses its first row.
- Lists and block quotes nest; code inside either stays indented under it.
- Content that is not shown is left out: the `hidden` attribute,
  `display: none`, and `visibility: hidden` (unless a descendant sets
  `visibility: visible`).

## What gets stripped

- `<script>`, `<style>`, `<noscript>`
- Inline styles
- ARIA attributes
- Tracking pixels and beacons
- Navigation, the page's header and footer, and sidebars (see
  [CLI reference](CLI-reference.md)). `--boilerplate keep` keeps them. A header
  or footer inside an article, main or section stays.

## As JSON

`--dump json` adds the title, description, language, publication time, final
URL, status and the link and image lists around the same Markdown; see the
[CLI reference](CLI-reference.md).

## Save to file

```bash
inbrowse fetch https://docs.example.com/page --dump markdown -o page.md
```

## For RAG / LLM context

```bash
inbrowse fetch https://docs.example.com/page --dump markdown --quiet
```

`--quiet` strips info logging so the output is just markdown.

## Wait for SPA content

For pages that render content client-side:

```bash
inbrowse fetch https://my-spa.example --wait-until load --dump markdown
```

## Narrow to a region

`--selector` restricts the conversion to a CSS selector:

```bash
inbrowse fetch https://example.com --selector "main" --dump markdown
inbrowse fetch https://example.com --selector "article.post" --dump markdown
```

Navigation, sidebars and the page header and footer are already left out by
default (`--boilerplate keep` brings them back); `--selector` is for narrowing
further. An element you select is kept even if it is one of those.
