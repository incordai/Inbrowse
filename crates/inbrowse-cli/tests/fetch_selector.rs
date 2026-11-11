//! `fetch --selector` waits for its selector, then narrows the dump to the
//! first match (#12).

use std::process::{Command, Output};

/// A list that a timer adds after load, so the selector has to be waited for.
const PAGE: &str = "data:text/html,<h1>Title</h1><p>Intro <a href=https://example.test/out>out</a></p>\
<div id=slot></div><script>setTimeout(() => { slot.innerHTML = \
'<ul id=dyn><li>One <a href=https://example.test/one>one</a></li><li>Two</li></ul>' }, 400)</script>";

fn fetch(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_inbrowse"))
        .args(["fetch", PAGE, "--quiet"])
        .args(args)
        .output()
        .expect("run inbrowse fetch")
}

fn stdout(output: &Output) -> String {
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

#[test]
fn html_is_the_matched_element() {
    let html = stdout(&fetch(&["--selector", "#dyn"]));
    assert!(html.starts_with("<ul id=\"dyn\">") && html.ends_with("</ul>"), "{html}");
    assert!(!html.contains("Title"), "{html}");
}

#[test]
fn text_markdown_and_links_are_the_matched_subtree() {
    assert_eq!(stdout(&fetch(&["--selector", "#dyn", "--dump", "text"])), "One one\nTwo");
    let markdown = stdout(&fetch(&["--selector", "#dyn", "--dump", "markdown"]));
    assert_eq!(markdown, "- One [one](https://example.test/one)\n- Two", "{markdown}");
    assert_eq!(
        stdout(&fetch(&["--selector", "#dyn", "--dump", "links"])),
        "https://example.test/one\tone"
    );
}

#[test]
fn a_link_selector_lists_that_link() {
    assert_eq!(
        stdout(&fetch(&["--selector", "p > a", "--dump", "links"])),
        "https://example.test/out\tout"
    );
}

#[test]
fn a_selector_that_never_matches_fails() {
    let output = fetch(&["--selector", "#missing", "--wait-until", "load"]);
    assert!(!output.status.success());
    assert!(output.stdout.is_empty(), "nothing is printed for a missing match");
    assert!(String::from_utf8_lossy(&output.stderr).contains("matched nothing"));
}

#[test]
fn an_invalid_selector_fails_at_once() {
    let output = fetch(&["--selector", "a[", "--dump", "text"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("invalid --selector"));
}

#[test]
fn dumps_without_an_element_refuse_a_selector() {
    for dump in ["assets", "cookies", "original"] {
        let output = fetch(&["--selector", "#dyn", "--dump", dump]);
        assert!(!output.status.success(), "{dump}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("has no element to narrow to"),
            "{dump}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
