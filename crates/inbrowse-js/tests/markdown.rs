//! The shared HTML to markdown converter (src/markdown.rs
//! `HTML_TO_MARKDOWN_JS`), used by `fetch --dump markdown`, MCP
//! `browser_markdown` and CDP `LP.getMarkdown`.

use inbrowse_js::runtime::InbrowseJsRuntime;
use inbrowse_js::HTML_TO_MARKDOWN_JS;

fn markdown(html: &str) -> String {
    let mut rt = InbrowseJsRuntime::new();
    rt.set_dom(inbrowse_dom::parse_html(html));
    rt.run_page_init();
    rt.evaluate(HTML_TO_MARKDOWN_JS).unwrap().as_str().unwrap().to_string()
}

#[test]
fn nested_lists_keep_their_depth() {
    assert_eq!(
        markdown("<ul><li>One</li><li>Two<ul><li>Nested</li><li>Also<ol><li>Deep</li></ol></li></ul></li></ul>"),
        "- One\n- Two\n  - Nested\n  - Also\n    1. Deep",
    );
}

#[test]
fn ordered_lists_number_their_items_from_start() {
    assert_eq!(
        markdown("<ol start=3><li>c</li><li>d</li><li value=9>i</li><li>j</li></ol>"),
        "3. c\n4. d\n9. i\n10. j",
    );
}

#[test]
fn source_indentation_leaves_no_whitespace_only_lines() {
    let md = markdown("<div>\n    <p>First</p>\n    \n    <div>\n      <p>Second</p>\n    </div>\n</div>");
    assert_eq!(md, "First\n\nSecond");
    assert!(!md.lines().any(|line| !line.is_empty() && line.trim().is_empty()));
}

#[test]
fn block_children_of_a_link_are_joined_with_a_space() {
    assert_eq!(
        markdown("<a href='/r'><div><span>Name</span></div><div><span>Repository hosting</span></div></a>"),
        "[Name Repository hosting](/r)",
    );
}

#[test]
fn preformatted_code_is_kept_verbatim_in_one_fence() {
    assert_eq!(
        markdown("<p>Run:</p><pre><code>cargo build\n  --release</code></pre>"),
        "Run:\n\n```\ncargo build\n  --release\n```",
    );
}

#[test]
fn a_select_shows_its_selected_option_and_hidden_content_is_skipped() {
    assert_eq!(
        markdown("<p>Size <select><option>S</option><option selected>M</option></select></p><p hidden>secret</p>"),
        "Size M",
    );
}
