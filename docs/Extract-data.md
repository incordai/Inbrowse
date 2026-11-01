`--dump` formats the page output without writing JavaScript.

```bash
inbrowse fetch https://example.com --dump html
inbrowse fetch https://example.com --dump text
inbrowse fetch https://example.com --dump markdown
inbrowse fetch https://example.com --dump links
inbrowse fetch https://example.com --dump assets
inbrowse fetch https://example.com --dump original
inbrowse fetch https://example.com --dump cookies
```

## `html`

Rendered HTML after JavaScript runs. Default.

```bash
inbrowse fetch https://news.ycombinator.com --dump html > hn.html
```

## `text`

Plain text. No markup.

```bash
inbrowse fetch https://en.wikipedia.org/wiki/Rust_(programming_language) --dump text
```

## `markdown`

Markdown conversion: headings, lists, links, code blocks, images.

```bash
inbrowse fetch https://docs.example.com/page --dump markdown > page.md
```

## `links`

Every `<a href>` on the page, one per line.

```bash
inbrowse fetch https://example.com --dump links
```

## `assets`

Every external resource (stylesheets, scripts, images, fonts, iframes), plus the URLs the page requested through `fetch()`/XHR, one JSON object per line.

```bash
inbrowse fetch https://example.com --dump assets
```

## `original`

The raw HTML the server sent, before JavaScript ran.

```bash
inbrowse fetch https://my-spa.example --dump original > before.html
inbrowse fetch https://my-spa.example --dump html     > after.html
diff before.html after.html
```

## `cookies`

Every cookie in the jar as a JSON array, including HttpOnly cookies that `document.cookie` cannot see. Useful for capturing session tokens set by anti-bot challenges.

```bash
inbrowse fetch https://example.com --dump cookies
```

## With `--wait-until`

`--dump` runs after the wait condition:

```bash
inbrowse fetch https://my-spa.example --wait-until load --dump markdown
```

## Pipe and redirect

```bash
inbrowse fetch https://example.com --dump markdown > example.md
inbrowse fetch https://example.com --dump text --quiet | wc -w
```

## Reader libraries

After loading a reader library such as [Mozilla Readability](https://github.com/mozilla/readability),
run it on a document clone to leave the page unchanged:

```javascript
const article = new Readability(document.cloneNode(true)).parse();
```

Inbrowse does not bundle Readability. Supply the library through your automation
client before evaluating this expression. Document clones preserve metadata and
live form-control state. Attribute clones retain their value and namespace.
`DOMParser` HTML documents preserve the document structure and root attributes,
inherit the parser's document URL, and do not execute scripts. XML parsing still
uses the existing limited XML implementation.
