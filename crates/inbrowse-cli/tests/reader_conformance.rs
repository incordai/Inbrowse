//! Page-to-Markdown and page-to-text conversion, checked through
//! `fetch --dump markdown|text` against local pages.
//!
//! Adapted from the end-to-end and unit tests of jina-ai/reader (Apache-2.0,
//! https://github.com/jina-ai/reader; see NOTICE): content-fidelity,
//! gfm-tables, retain-images, retain-links, data-url-images,
//! base-url-resolution, detach-invisibles, selectors, html-input, markdown
//! options and markify. Reader's HTTP-service options (response headers,
//! token budgets, summaries, option headers) and its output style options
//! (setext headings, `*` or `+` bullets, `_` emphasis, reference-style links,
//! raw-HTML tables, link and image retention modes) are not ported; where its
//! output is a style choice, the expectations follow Inbrowse's: ATX headings,
//! `-` bullets, `*emphasis*`, `**strong**`, `---` rules and `|---|` table
//! separators.

mod common;

use common::{fetch, fetch_output, Response, Server};

const SAMPLE: &str = include_str!("fixtures/reader/sample.html");

/// Serves the sample page at `/docs/sample.html` and fetches it with `args`.
fn sample(args: &[&str]) -> (Server, String) {
    let server = Server::start(vec![("/docs/sample.html", Response::html(SAMPLE))]);
    let output = fetch(&server.url("/docs/sample.html"), args);
    (server, output)
}

fn sample_markdown() -> (Server, String) {
    sample(&["--dump", "markdown"])
}

/// Converts each `html` fragment on one page and compares it with its
/// expected Markdown. Fragments are separated by a marker paragraph, so one
/// browser run checks them all and every mismatch is reported together.
fn convert(cases: &[(&str, &str)]) {
    let fragments: String = cases.iter().map(|(html, _)| format!("<div>{html}</div><p>@@@</p>")).collect();
    let page = format!("<!doctype html><html><head><title>cases</title></head><body>{fragments}</body></html>");
    let server = Server::start(vec![("/cases", Response::html(&page))]);
    let output = fetch(&server.url("/cases"), &["--dump", "markdown"]);
    let got: Vec<&str> = output.split("@@@").map(str::trim).collect();
    assert_eq!(got.len(), cases.len() + 1, "one output per case: {output}");
    let failures: Vec<String> = cases
        .iter()
        .zip(&got)
        .filter(|((_, expected), got)| expected != *got)
        .map(|((html, expected), got)| format!("{html}\n  expected: {expected:?}\n  got:      {got:?}"))
        .collect();
    assert!(failures.is_empty(), "{} of {} cases differ:\n{}", failures.len(), cases.len(), failures.join("\n"));
}

/// The Markdown of one body.
fn markdown_of(body: &str, args: &[&str]) -> String {
    let page = format!("<!doctype html><html><head><title>t</title></head><body>{body}</body></html>");
    let server = Server::start(vec![("/page", Response::html(&page))]);
    fetch(&server.url("/page"), &[&["--dump", "markdown"], args].concat())
}

// content-fidelity: headings

#[test]
fn h1_is_a_level_one_heading_and_all_sections_survive() {
    let (_server, md) = sample_markdown();
    assert!(md.lines().any(|line| line == "# Web Crawling Guide"), "{md}");
    for section in ["One", "Two", "Three", "Four", "Five", "Six", "Seven"] {
        assert!(md.contains(&format!("Section {section}")), "Section {section} missing:\n{md}");
    }
}

// content-fidelity: inline, quote, code, list, rule

#[test]
fn sample_inline_quote_code_list_and_rule() {
    let (_server, md) = sample_markdown();
    assert!(md.contains("This is the **main content** of the article"), "{md}");
    assert!(md.contains("involves *fetching pages* from"), "{md}");
    assert!(md.lines().any(|line| line == "> The web is a graph, not a tree."), "{md}");
    assert!(
        md.contains("```\nconst crawler = new Crawler();\ncrawler.start('https://example.com');\n```"),
        "{md}"
    );
    assert!(md.contains("- Fetch HTML pages\n- Parse links\n- Follow links recursively"), "{md}");
    assert!(md.contains("Content before the rule.\n\n---\n\nContent after the rule."), "{md}");
}

// gfm-tables

#[test]
fn sample_table_is_a_gfm_pipe_table() {
    let (_server, md) = sample_markdown();
    assert!(
        md.contains("| Method | Speed | Accuracy |\n|---|---|---|\n| Browser | Slow | High |\n| Curl | Fast | Medium |"),
        "{md}"
    );
}

// retain-links (all) and retain-images (all)

#[test]
fn sample_links_and_images_keep_their_text_and_urls() {
    let (_server, md) = sample_markdown();
    assert!(md.contains("[link to crawling docs](https://example.com/crawling)"), "{md}");
    assert!(md.contains("[link about robots.txt](https://example.org/robots)"), "{md}");
    assert!(md.contains("![A spider crawling the web](https://example.com/spider.png)"), "{md}");
    assert!(md.contains("![Network diagram](https://example.com/network.jpg)"), "{md}");
    assert!(md.contains("![](https://example.com/noalt.png)"), "{md}");
}

// data-url-images

#[test]
fn a_data_url_image_keeps_its_alt_text_and_not_its_payload() {
    let (_server, md) = sample_markdown();
    assert!(md.contains("![Tiny pixel](data:image/png)"), "{md}");
    assert!(!md.contains("base64") && !md.contains("iVBOR"), "{md}");
    assert!(md.contains("https://example.com/spider.png"), "other images are untouched: {md}");
}

// base-url-resolution

#[test]
fn relative_urls_resolve_against_the_page_url() {
    let (server, md) = sample(&["--dump", "markdown", "--boilerplate", "keep"]);
    for expected in [
        format!("[Home]({})", server.url("/home")),
        format!("[About]({})", server.url("/about")),
        format!("[Privacy Policy]({})", server.url("/privacy")),
        format!("[advanced guide]({})", server.url("/guide/advanced")),
        format!("[sibling page]({})", server.url("/docs/sibling-page")),
        "[link to crawling docs](https://example.com/crawling)".to_string(),
    ] {
        assert!(md.contains(&expected), "{expected} missing:\n{md}");
    }
}

#[test]
fn a_base_element_sets_the_base_of_links_and_images() {
    let md = markdown_of(
        r#"<base href="http://base.test/dir/"><a href="page">rel</a> <a href="/root">root</a> <img src="pic.png" alt="p">"#,
        &[],
    );
    assert_eq!(md, "[rel](http://base.test/dir/page) [root](http://base.test/root) ![p](http://base.test/dir/pic.png)");
}

#[test]
fn relative_urls_resolve_against_the_final_url_after_a_redirect() {
    let server = Server::start(vec![
        ("/old", Response::redirect("/new/dir/page")),
        ("/new/dir/page", Response::html(r#"<p><a href="next">next</a></p>"#)),
    ]);
    let md = fetch(&server.url("/old"), &["--dump", "markdown"]);
    assert_eq!(md, format!("[next]({})", server.url("/new/dir/next")));
}

#[test]
fn an_error_page_is_still_converted() {
    let server = Server::start(vec![("/gone", Response::html("<h1>Gone</h1>").status(404))]);
    assert_eq!(fetch(&server.url("/gone"), &["--dump", "markdown"]), "# Gone");
}

// detach-invisibles

#[test]
fn content_hidden_by_style_is_left_out() {
    let md = markdown_of(
        r#"<style>.gone{display:none}.ghost{visibility:hidden}</style>
        <p>CrawlerVisibleContent99</p>
        <div style="display:none"><p>CrawlerHiddenSecret42</p></div>
        <p class="gone">ClassHidden</p>
        <p class="ghost">GhostText <span style="visibility:visible">ShownAgain</span></p>
        <ul><li>kept</li><li class="gone">dropped item</li></ul>
        <table><tr><th>H</th></tr><tr class="gone"><td>hidden row</td></tr><tr><td>row</td></tr></table>
        <p>More <span style="display:none">inline hidden </span>visible text below.</p>
        <p hidden>attr hidden</p>
        <img class="gone" src="https://x.test/h.png" alt="hidden image">"#,
        &[],
    );
    assert_eq!(
        md,
        "CrawlerVisibleContent99\n\nShownAgain\n\n- kept\n\n| H |\n|---|\n| row |\n\nMore visible text below."
    );
}

// selectors (targetSelector); removeSelector is not an Inbrowse option

#[test]
fn a_selector_narrows_to_the_element() {
    let (_server, md) = sample(&["--dump", "markdown", "--selector", "#main-content"]);
    assert!(md.contains("# Web Crawling Guide") && md.contains("Section One: Basics"), "{md}");
    for outside in ["Related articles sidebar", "Copyright 2024", "example.com/related", "/privacy", "/home"] {
        assert!(!md.contains(outside), "{outside} is outside #main-content:\n{md}");
    }
}

#[test]
fn a_selector_can_target_a_sidebar() {
    let (_server, md) = sample(&["--dump", "markdown", "--selector", ".sidebar"]);
    assert_eq!(md, "Related articles sidebar content\n\n[Related Article](https://example.com/related)");
}

// html-input

#[test]
fn entities_unicode_and_whitespace_come_out_as_text() {
    convert(&[
        ("<p>Tom &amp; Jerry</p>", "Tom & Jerry"),
        ("<p>1 &lt; 2</p>", "1 < 2"),
        ("<p>2 &gt; 1</p>", "2 > 1"),
        ("<p>日本語テスト</p>", "日本語テスト"),
        ("<p>Hello 🌍 World</p>", "Hello 🌍 World"),
        ("<p>a  b\n   c</p>", "a b c"),
        ("<p>Hello world</p>", "Hello world"),
    ]);
}

#[test]
fn nested_sections_keep_their_headings_and_text() {
    convert(&[(
        "<article><h1>Top Level</h1><section><h2>Sub Section</h2><p>Body text</p></section></article>",
        "# Top Level\n\n## Sub Section\n\nBody text",
    )]);
}

#[test]
fn identical_pages_give_identical_output() {
    let body = "<h1>Deterministic</h1><p>Same every time.</p>";
    assert_eq!(markdown_of(body, &[]), markdown_of(body, &[]));
    assert_eq!(markdown_of(body, &[]), "# Deterministic\n\nSame every time.");
}

// markify: headings

#[test]
fn headings() {
    convert(&[
        ("<h1>Hello</h1>", "# Hello"),
        ("<h2>Section</h2>", "## Section"),
        ("<h3>Sub</h3>", "### Sub"),
        ("<h4>Deep</h4>", "#### Deep"),
        ("<h5>Deeper</h5>", "##### Deeper"),
        ("<h6>Deepest</h6>", "###### Deepest"),
        ("<h1></h1>", ""),
        ("<h1>Title</h1><p>body</p>", "# Title\n\nbody"),
        ("<h2>Section</h2><p>Content here.</p>", "## Section\n\nContent here."),
        ("<h2>Hello <em>world</em></h2>", "## Hello *world*"),
        ("<h2><a href=\"https://x.com\">Linked Heading</a></h2>", "## [Linked Heading](https://x.com/)"),
        ("<h3>Two<br>lines</h3>", "### Two lines"),
        ("<h2>With <code>code</code> and <strong>bold</strong></h2>", "## With `code` and **bold**"),
        ("<h2><span>Split</span> <span>spans</span></h2>", "## Split spans"),
        ("<h2><img src=\"https://x.test/i.png\" alt=\"Icon\"> Title</h2>", "## ![Icon](https://x.test/i.png) Title"),
    ]);
}

// markify: inline formatting

#[test]
fn inline_formatting() {
    convert(&[
        ("<strong>bold</strong>", "**bold**"),
        ("<b>bold</b>", "**bold**"),
        ("<strong></strong>", ""),
        ("<em>italic</em>", "*italic*"),
        ("<i>italic</i>", "*italic*"),
        ("<em></em>", ""),
        ("<p>Say <strong>hi </strong>there</p>", "Say **hi** there"),
        ("<p>Say <em> hi</em> there</p>", "Say *hi* there"),
        ("<del>gone</del>", "~~gone~~"),
        ("<s>struck</s>", "~~struck~~"),
        ("<strike>old</strike>", "~~old~~"),
        ("<del></del>", ""),
        ("<code>const x = 1</code>", "`const x = 1`"),
        ("<code>  trimmed  </code>", "`trimmed`"),
        ("<code>a ` b</code>", "``a ` b``"),
        ("<p>This is <strong>bold</strong> and <em>italic</em>.</p>", "This is **bold** and *italic*."),
        ("<p>Inner <strong>bold <em>both</em></strong></p>", "Inner **bold *both***"),
    ]);
}

// markify: links and images

#[test]
fn links_and_images() {
    convert(&[
        // URLs are written in their normalized form, as `a.href` reads them.
        ("<a href=\"https://example.com\">click</a>", "[click](https://example.com/)"),
        ("<a>anchor</a>", "anchor"),
        ("<a href=\"\">empty</a>", "empty"),
        ("<a href=\"javascript:void(0)\">script</a>", "script"),
        ("<a href=\"https://x.com\">\nclick\nme\n</a>", "[click me](https://x.com/)"),
        ("<a href=\"mailto:a@x.test\">mail</a>", "[mail](mailto:a@x.test)"),
        ("<a href=\"https://x.com\"><img src=\"https://x.com/a.png\" alt=\"A\"></a>", "[![A](https://x.com/a.png)](https://x.com/)"),
        ("<a href=\"https://x.com\">text <img src=\"https://x.com/a.png\" alt=\"A\"></a>", "[text ![A](https://x.com/a.png)](https://x.com/)"),
        ("<img src=\"https://x.test/img.png\" alt=\"a photo\">", "![a photo](https://x.test/img.png)"),
        ("<img src=\"https://x.test/img.png\">", "![](https://x.test/img.png)"),
        ("<img src=\"https://x.test/img.png\" alt=\"\">", "![](https://x.test/img.png)"),
        ("<img alt=\"no source\">", ""),
        ("<p>See <img src=\"https://x.test/chart.png\" alt=\"chart\"> here.</p>", "See ![chart](https://x.test/chart.png) here."),
        ("<img src=\"data:image/png;base64,AAAA\" alt=\"Tiny\">", "![Tiny](data:image/png)"),
        ("<img src=\"data:image/svg+xml;utf8,<svg/>\" alt=\"Icon\">", "![Icon](data:image/svg+xml)"),
    ]);
}

// markify: code

#[test]
fn code_blocks_keep_their_text_and_language() {
    convert(&[
        ("<pre><code>line1\nline2</code></pre>", "```\nline1\nline2\n```"),
        (
            "<pre><code class=\"language-js\">const x = 1\nconst y = 2</code></pre>",
            "```js\nconst x = 1\nconst y = 2\n```",
        ),
        ("<pre><code class=\"lang-python\">x = 1\ny = 2</code></pre>", "```python\nx = 1\ny = 2\n```"),
        ("<pre class=\"language-rust\"><code>fn main() {}</code></pre>", "```rust\nfn main() {}\n```"),
        ("<pre data-lang=\"go\">package main</pre>", "```go\npackage main\n```"),
        ("<pre><code>  line1\n  line2</code></pre>", "```\n  line1\n  line2\n```"),
        ("<pre>a\n\n\nb</pre>", "```\na\n\n\nb\n```"),
        ("<pre>use ``` here</pre>", "````\nuse ``` here\n````"),
        ("<p>intro</p><pre><code>line1\nline2</code></pre>", "intro\n\n```\nline1\nline2\n```"),
        ("<pre><code></code></pre>", ""),
        (
            "<ul><li>Run:<pre><code class=\"language-sh\">make\nmake test</code></pre></li></ul>",
            "- Run:\n  ```sh\n  make\n  make test\n  ```",
        ),
        ("<blockquote><pre>a\n\nb</pre></blockquote>", "> ```\n> a\n>\n> b\n> ```"),
    ]);
}

// markify: lists, quotes, rules

#[test]
fn lists_quotes_and_rules() {
    convert(&[
        ("<ul><li>Item A</li><li>Item B</li></ul>", "- Item A\n- Item B"),
        ("<ol><li>First</li><li>Second</li><li>Third</li></ol>", "1. First\n2. Second\n3. Third"),
        ("<ul><li>Top<ul><li>Sub</li></ul></li></ul>", "- Top\n  - Sub"),
        ("<ol><li>a<ol><li>b</li></ol></li></ol>", "1. a\n   1. b"),
        ("<ol><li>a<ul><li>b</li></ul></li></ol>", "1. a\n   - b"),
        ("<ul><li><p>One</p><p>Two</p></li></ul>", "- One\n\n  Two"),
        ("<ul><li>See <a href=\"https://x.test\">x</a></li></ul>", "- See [x](https://x.test/)"),
        ("<ul></ul>", ""),
        ("<blockquote><p>quoted text</p></blockquote>", "> quoted text"),
        ("<blockquote><p>line one</p><p>line two</p></blockquote>", "> line one\n>\n> line two"),
        (
            "<blockquote><p>outer</p><blockquote><p>inner</p></blockquote></blockquote>",
            "> outer\n>\n> > inner",
        ),
        ("<blockquote><ul><li>a</li><li>b</li></ul></blockquote>", "> - a\n> - b"),
        ("<blockquote></blockquote>", ""),
        ("<p>a</p><hr><p>b</p>", "a\n\n---\n\nb"),
    ]);
}

// markify: tables

#[test]
fn tables() {
    convert(&[
        (
            "<table><thead><tr><th>Name</th><th>Age</th></tr></thead><tbody><tr><td>Alice</td><td>30</td></tr></tbody></table>",
            "| Name | Age |\n|---|---|\n| Alice | 30 |",
        ),
        (
            "<table><tr><th><strong>Name</strong></th><th>Link</th></tr><tr><td><em>Bob</em> <code>x</code></td><td><a href=\"https://x.test\">site</a></td></tr></table>",
            "| **Name** | Link |\n|---|---|\n| *Bob* `x` | [site](https://x.test/) |",
        ),
        (
            "<table><tr><th>a|b</th></tr><tr><td>c|d</td></tr></table>",
            "| a\\|b |\n|---|\n| c\\|d |",
        ),
        (
            "<table><tr><td>1</td><td>2</td></tr><tr><td>3</td><td>4</td></tr></table>",
            "| 1 | 2 |\n|---|---|\n| 3 | 4 |",
        ),
        (
            "<table><tr><th>A</th></tr><tr><td>one<br>two</td></tr></table>",
            "| A |\n|---|\n| one two |",
        ),
        (
            "<table><tr><th>A</th><th>B</th></tr><tr><td colspan=\"2\">wide</td></tr><tr><td>x</td></tr></table>",
            "| A | B |\n|---|---|\n| wide |  |\n| x |  |",
        ),
        (
            "<p>before</p><table><tr><th>A</th></tr><tr><td>1</td></tr></table><p>after</p>",
            "before\n\n| A |\n|---|\n| 1 |\n\nafter",
        ),
        (
            "<table><caption>Prices</caption><tr><th>A</th></tr><tr><td>1</td></tr></table>",
            "Prices\n\n| A |\n|---|\n| 1 |",
        ),
    ]);
}

// markify: mixed content

#[test]
fn mixed_content() {
    convert(&[
        (
            "<script>alert(1)</script><meta name=\"x\"><style>body{}</style><p>visible</p>",
            "visible",
        ),
        (
            "<h1>Guide</h1><p>Introduction to the <strong>topic</strong>.</p><ul><li>Step one</li><li>Step two</li></ul><blockquote><p>A wise quote.</p></blockquote>",
            "# Guide\n\nIntroduction to the **topic**.\n\n- Step one\n- Step two\n\n> A wise quote.",
        ),
    ]);
}

// plain text (respondWith text)

#[test]
fn text_has_the_content_without_markdown_syntax() {
    let (_server, text) = sample(&["--dump", "text"]);
    assert!(text.contains("Web Crawling Guide"), "{text}");
    assert!(text.contains("The web is a graph, not a tree"), "{text}");
    assert!(!text.lines().any(|line| line.starts_with('#')), "{text}");
    assert!(!text.contains("**"), "{text}");
}

/// Text needs the render build's style cascade to see what stylesheets hide.
#[cfg(feature = "render")]
#[test]
fn text_leaves_out_content_hidden_by_style() {
    let page = r#"<style>.gone{display:none}.ghost{visibility:hidden}</style>
        <p>CrawlerVisibleContent99</p>
        <div style="display:none"><p>CrawlerHiddenSecret42</p></div>
        <p class="gone">ClassHidden</p>
        <p class="ghost">GhostText <span style="visibility:visible">ShownAgain</span></p>
        <ul><li>kept</li><li class="gone">dropped item</li></ul>
        <table><tr><th>H</th></tr><tr class="gone"><td>hidden row</td></tr><tr><td>row</td></tr></table>
        <p>More <span style="display:none">inline hidden </span>visible text below.</p>
        <p hidden>attr hidden</p>"#;
    let server = Server::start(vec![("/page", Response::html(page))]);
    let text = fetch(&server.url("/page"), &["--dump", "text"]);
    for shown in ["CrawlerVisibleContent99", "ShownAgain", "kept", "row", "More visible text below."] {
        assert!(text.contains(shown), "{shown} missing:\n{text}");
    }
    for hidden in [
        "CrawlerHiddenSecret42",
        "ClassHidden",
        "GhostText",
        "dropped item",
        "hidden row",
        "inline hidden",
        "attr hidden",
    ] {
        assert!(!text.contains(hidden), "{hidden} shown:\n{text}");
    }
}

#[test]
fn links_resolve_against_a_base_element() {
    let server = Server::start(vec![(
        "/page",
        Response::html(
            r#"<base href="http://base.test/dir/"><a href="page">rel</a><a href="/root">root</a><a href="https://x.test/abs">abs</a>"#,
        ),
    )]);
    assert_eq!(
        fetch(&server.url("/page"), &["--dump", "links"]),
        "http://base.test/dir/page\trel\nhttp://base.test/root\troot\nhttps://x.test/abs\tabs"
    );
}

// main content: boilerplate

/// A page with page-level and article-level headers, footers and sidebars.
const BOILERPLATE_PAGE: &str = r#"<header>SITE HEADER</header>
    <nav>NAV LINKS</nav>
    <div role="navigation">NAV ROLE</div>
    <article><header><h1>Headline</h1><p>By Jane</p></header><p>Body text</p>
      <aside>Pull quote</aside><footer>Filed under news</footer></article>
    <section><header>Section head</header></section>
    <aside>SIDEBAR</aside>
    <div role="contentinfo">CONTENT INFO</div>
    <footer>SITE FOOTER</footer>"#;
const PAGE_BOILERPLATE: [&str; 6] = ["SITE HEADER", "NAV LINKS", "NAV ROLE", "SIDEBAR", "CONTENT INFO", "SITE FOOTER"];
const PAGE_CONTENT: [&str; 6] =
    ["Headline", "By Jane", "Body text", "Pull quote", "Filed under news", "Section head"];

fn boilerplate_page(args: &[&str]) -> String {
    let server = Server::start(vec![("/page", Response::html(BOILERPLATE_PAGE))]);
    fetch(&server.url("/page"), args)
}

#[test]
fn text_and_markdown_drop_the_same_boilerplate_when_asked() {
    for dump in ["text", "markdown"] {
        let output = boilerplate_page(&["--dump", dump, "--boilerplate", "drop"]);
        for kept in PAGE_CONTENT {
            assert!(output.contains(kept), "{dump} dropped '{kept}':\n{output}");
        }
        for dropped in PAGE_BOILERPLATE {
            assert!(!output.contains(dropped), "{dump} kept '{dropped}':\n{output}");
        }
    }
}

#[test]
fn text_and_markdown_keep_everything_when_asked() {
    for dump in ["text", "markdown"] {
        let output = boilerplate_page(&["--dump", dump, "--boilerplate", "keep"]);
        for wanted in PAGE_CONTENT.iter().chain(&PAGE_BOILERPLATE) {
            assert!(output.contains(wanted), "{dump} dropped '{wanted}':\n{output}");
        }
    }
}

#[test]
fn text_and_markdown_drop_boilerplate_by_default() {
    for dump in ["text", "markdown"] {
        let output = boilerplate_page(&["--dump", dump]);
        assert!(output.contains("Body text"), "{dump}: {output}");
        for dropped in PAGE_BOILERPLATE {
            assert!(!output.contains(dropped), "{dump} kept '{dropped}':\n{output}");
        }
    }
}

#[test]
fn the_sample_page_without_boilerplate() {
    let (_server, md) = sample_markdown();
    assert!(md.contains("# Web Crawling Guide") && md.contains("Buy our product!"), "{md}");
    for boilerplate in ["Home", "About", "Related articles sidebar", "Copyright 2024", "Privacy Policy"] {
        assert!(!md.contains(boilerplate), "{boilerplate} kept:\n{md}");
    }
}

#[test]
fn a_selected_element_is_kept_even_when_it_is_boilerplate() {
    let server = Server::start(vec![("/page", Response::html(BOILERPLATE_PAGE))]);
    let url = server.url("/page");
    for dump in ["text", "markdown"] {
        let output = fetch(&url, &["--dump", dump, "--boilerplate", "drop", "--selector", "nav"]);
        assert_eq!(output, "NAV LINKS", "{dump}");
    }
}

#[test]
fn boilerplate_applies_to_text_markdown_and_json_only() {
    let server = Server::start(vec![("/page", Response::html(BOILERPLATE_PAGE))]);
    let output = fetch_output(&server.url("/page"), &["--dump", "html", "--boilerplate", "drop"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("--boilerplate applies to"));
}
