//! `fetch --eval` and `scrape --eval` evaluate the way the DevTools console
//! does (inbrowse `Page::evaluate_checked`, shared with MCP `browser_evaluate`
//! and the scrape worker): a lone expression returns its value, statements
//! return their completion value, a returned promise is awaited, and a thrown
//! error fails the command instead of printing `null`.

use std::process::{Command, Output};

fn cli() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_inbrowse"));
    for name in ["INBROWSE_PROXY", "HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "http_proxy", "https_proxy", "all_proxy"] {
        command.env_remove(name);
    }
    command.env("INBROWSE_STEALTH", "0");
    command
}

fn eval(expression: &str) -> Output {
    // `--eval=<source>` so a source starting with `-` is not read as a flag.
    cli()
        .args(["fetch", "about:blank", "--quiet", &format!("--eval={expression}")])
        .output()
        .unwrap()
}

fn printed(expression: &str) -> String {
    let output = eval(expression);
    assert!(
        output.status.success(),
        "{expression:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

#[test]
fn statements_return_their_completion_value() {
    assert_eq!(printed("1; 2"), "2");
    assert_eq!(printed("var a = 1; a + 1"), "2");
    assert_eq!(printed("const x = 3; x * 2"), "6");
    assert_eq!(printed("document.title = 'renamed'; document.title"), "renamed");
}

#[test]
fn an_object_literal_is_read_as_an_expression() {
    assert_eq!(printed("{a: 1}"), r#"{"a":1}"#);
}

#[test]
fn integral_numbers_print_as_integers() {
    assert_eq!(printed("41 + 1"), "42");
    assert_eq!(printed("2 ** 53"), "9007199254740992");
    assert_eq!(printed("-7"), "-7");
    assert_eq!(printed("1.5"), "1.5");
    assert_eq!(printed("[1, 2.5]"), "[1,2.5]");
}

#[test]
fn a_returned_promise_is_awaited() {
    assert_eq!(printed("Promise.resolve(41).then((v) => v + 1)"), "42");
    assert_eq!(printed("new Promise((r) => setTimeout(() => r('late'), 50))"), "late");
}

#[test]
fn a_thrown_error_fails_the_command() {
    let output = eval("throw new Error('boom')");
    assert!(!output.status.success(), "a thrown error must not exit 0");
    assert!(output.stdout.is_empty(), "nothing on stdout: {}", String::from_utf8_lossy(&output.stdout));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("boom"), "stderr names the error: {stderr}");
}

#[test]
fn a_rejected_promise_fails_the_command() {
    let output = eval("Promise.reject(new Error('rejected-here'))");
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("rejected-here"));
}

#[test]
fn a_syntax_error_fails_the_command() {
    let output = eval("1 +");
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("SyntaxError"));
}

#[test]
fn a_runaway_script_is_stopped_by_the_timeout() {
    let started = std::time::Instant::now();
    let output = cli()
        .args(["fetch", "about:blank", "--quiet", "--timeout", "2", "--eval", "while (true) {}"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(started.elapsed() < std::time::Duration::from_secs(15), "took {:?}", started.elapsed());
}

#[test]
fn a_runaway_page_script_is_stopped_and_reported_on_stderr() {
    let started = std::time::Instant::now();
    let output = cli()
        .args([
            "fetch",
            "data:text/html,<script>while(true){}</script><p>ok</p>",
            "--quiet", "--script-timeout", "1", "--dump", "text",
        ])
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert!(started.elapsed() < std::time::Duration::from_secs(15), "took {:?}", started.elapsed());
    assert!(String::from_utf8_lossy(&output.stdout).contains("ok"));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Warning: script") && stderr.contains("1000 ms"), "{stderr}");
}

#[test]
fn eval_side_effects_reach_the_dump() {
    let output = cli()
        .args([
            "fetch", "about:blank", "--quiet", "--dump", "html",
            "--eval", "document.body.append('SIDE_EFFECT'); 1",
        ])
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert!(String::from_utf8_lossy(&output.stdout).contains("SIDE_EFFECT"));
}

#[test]
fn scrape_reports_an_eval_error_per_url() {
    let output = cli()
        .args(["scrape", "about:blank", "--quiet", "--eval", "throw new Error('scrape-boom')"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let result = &report["results"][0];
    assert!(result["eval"].is_null(), "{result}");
    assert!(
        result["error"].as_str().is_some_and(|e| e.contains("scrape-boom")),
        "the eval error is reported: {result}"
    );
}

#[test]
fn scrape_returns_completion_values_and_integers() {
    let output = cli()
        .args(["scrape", "about:blank", "--quiet", "--eval", "var n = 41; n + 1"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let result = &report["results"][0];
    assert_eq!(result["eval"], serde_json::json!(42), "{result}");
    assert!(result["eval"].is_i64(), "{result}");
    assert!(result.get("error").is_none(), "{result}");
}
