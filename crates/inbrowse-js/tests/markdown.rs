//! The shared HTML to markdown converter (src/markdown.rs
//! `HTML_TO_MARKDOWN_JS`), used by `fetch --dump markdown`, MCP
//! `browser_markdown` and CDP `LP.getMarkdown`.

use inbrowse_js::runtime::InbrowseJsRuntime;
use inbrowse_js::HTML_TO_MARKDOWN_JS;

fn markdown(html: &str) -> String {
    let mut rt = InbrowseJsRuntime::new();
    rt.set_dom(inbrowse_dom::parse_html(html));
    rt.run_page_init();
    rt.evaluate(HTML_TO_MARKDOWN_JS)
        .unwrap()
        .as_str()
        .unwrap()
        .to_string()
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
    let md = markdown(
        "<div>\n    <p>First</p>\n    \n    <div>\n      <p>Second</p>\n    </div>\n</div>",
    );
    assert_eq!(md, "First\n\nSecond");
    assert!(!md
        .lines()
        .any(|line| !line.is_empty() && line.trim().is_empty()));
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

/// The converter on a page whose structure `build` makes with script, so the
/// nesting is as deep as the test needs whatever the parser allows. It runs on
/// a thread with the stack the engine's own threads get, because the engine's
/// DOM operations recurse natively on a tree this deep.
fn markdown_of_built_page(build: &str) -> String {
    let build = build.to_string();
    std::thread::Builder::new()
        .stack_size(256 * 1024 * 1024)
        .spawn(move || {
            let mut rt = InbrowseJsRuntime::new();
            rt.set_dom(inbrowse_dom::parse_html("<html><body></body></html>"));
            rt.run_page_init();
            rt.evaluate(&build).unwrap();
            rt.evaluate(HTML_TO_MARKDOWN_JS)
                .unwrap()
                .as_str()
                .unwrap()
                .to_string()
        })
        .unwrap()
        .join()
        .unwrap()
}

#[test]
fn thousands_of_nested_blocks_convert_without_exhausting_the_stack() {
    let md = markdown_of_built_page(
        "(() => { let parent = document.body; \
         for (let i = 0; i < 5000; i++) { const div = document.createElement('div'); \
         parent.appendChild(div); parent = div; } \
         parent.appendChild(document.createTextNode('deep text')); return 1; })()",
    );
    assert_eq!(md, "deep text");
}

#[test]
fn thousands_of_nested_lists_keep_every_level_and_its_indent() {
    let md = markdown_of_built_page(
        "(() => { let parent = document.body; \
         for (let i = 0; i < 2000; i++) { const list = document.createElement('ul'); \
         const item = document.createElement('li'); \
         item.appendChild(document.createTextNode('level ' + i)); \
         list.appendChild(item); parent.appendChild(list); parent = item; } return 1; })()",
    );
    let lines: Vec<&str> = md.lines().collect();
    assert_eq!(lines.len(), 2000);
    assert_eq!(lines[0], "- level 0");
    assert_eq!(lines[1999], format!("{}- level 1999", " ".repeat(2 * 1999)));
}

#[test]
fn a_table_cell_holding_a_deep_tree_is_converted_once_and_in_order() {
    let md = markdown_of_built_page(
        "(() => { const table = document.createElement('table'); \
         table.innerHTML = '<tr><th>A</th><th>B</th></tr><tr><td id=c>x</td><td>y | z</td></tr>'; \
         let parent = table.querySelector('#c'); \
         for (let i = 0; i < 3000; i++) { const span = document.createElement('div'); \
         parent.appendChild(span); parent = span; } \
         parent.appendChild(document.createTextNode('bottom')); \
         document.body.appendChild(table); return 1; })()",
    );
    assert_eq!(md, "| A | B |\n|---|---|\n| x bottom | y \\| z |");
}
