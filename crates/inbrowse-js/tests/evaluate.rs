//! `InbrowseJsRuntime::evaluate` / `evaluate_with_timeout` (src/runtime.rs),
//! the evaluation behind `Page::evaluate` and its internal callers: an
//! expression returns its value, other source runs as a block statement and
//! returns its completion value, and a thrown error is an `Err`, never `null`.

use inbrowse_js::runtime::InbrowseJsRuntime;
use serde_json::json;
use std::time::Duration;

fn runtime() -> InbrowseJsRuntime {
    let mut rt = InbrowseJsRuntime::new();
    rt.set_dom(inbrowse_dom::parse_html("<body><p id=p>x</p></body>"));
    rt.run_page_init();
    rt
}

#[test]
fn statements_return_their_completion_value() {
    let mut rt = runtime();
    assert_eq!(rt.evaluate("1; 2").unwrap(), json!(2));
    assert_eq!(rt.evaluate("var a = 1; a + 1").unwrap(), json!(2));
    assert_eq!(rt.evaluate("if (true) { 'yes' } else { 'no' }").unwrap(), json!("yes"));
}

#[test]
fn block_scoped_declarations_do_not_leak_between_evaluations() {
    let mut rt = runtime();
    assert_eq!(rt.evaluate("const x = 1; x").unwrap(), json!(1));
    assert_eq!(rt.evaluate("const x = 2; x").unwrap(), json!(2));
    assert_eq!(rt.evaluate("typeof x").unwrap(), json!("undefined"));
}

#[test]
fn a_function_body_returns_its_value() {
    let mut rt = runtime();
    assert_eq!(rt.evaluate("return 7;").unwrap(), json!(7));
    assert_eq!(rt.evaluate("const p = document.getElementById('p'); return p.textContent;").unwrap(), json!("x"));
    assert_eq!(rt.evaluate("const p = 1; return p;").unwrap(), json!(1), "locals stay local");
    let error = rt.evaluate("return 1 +").unwrap_err();
    assert!(error.contains("SyntaxError"), "{error}");
}

#[test]
fn a_runtime_syntax_error_is_not_retried() {
    // JSON.parse throws SyntaxError while running; the source must run once.
    let mut rt = runtime();
    rt.evaluate("globalThis.runs = 0").unwrap();
    let error = rt.evaluate("runs++; JSON.parse('{'); 'unreached'").unwrap_err();
    assert!(error.contains("SyntaxError"), "{error}");
    assert_eq!(rt.evaluate("runs").unwrap(), json!(1));
}

#[test]
fn an_object_literal_is_an_expression() {
    let mut rt = runtime();
    assert_eq!(rt.evaluate("{a: 1}").unwrap(), json!({"a": 1}));
    assert_eq!(rt.evaluate("(() => 5)();").unwrap(), json!(5));
}

#[test]
fn a_trailing_line_comment_does_not_swallow_the_source() {
    let mut rt = runtime();
    assert_eq!(rt.evaluate("40 + 2 //# sourceURL=x.js").unwrap(), json!(42));
    assert_eq!(rt.evaluate("var y = 2; y //# sourceURL=y.js").unwrap(), json!(2));
}

#[test]
fn a_thrown_error_is_an_error() {
    let mut rt = runtime();
    let error = rt.evaluate("throw new Error('boom')").unwrap_err();
    assert!(error.contains("boom"), "{error}");
    let error = rt.evaluate("null.property").unwrap_err();
    assert!(error.contains("TypeError"), "{error}");
}

#[test]
fn a_syntax_error_is_an_error() {
    let mut rt = runtime();
    let error = rt.evaluate("1 +").unwrap_err();
    assert!(error.contains("SyntaxError"), "{error}");
}

#[test]
fn numbers_keep_integer_form() {
    let mut rt = runtime();
    let value = rt.evaluate("41 + 1").unwrap();
    assert!(value.is_i64(), "{value:?}");
    assert_eq!(value, json!(42));
    assert_eq!(rt.evaluate("-0").unwrap(), json!(0));
    assert_eq!(rt.evaluate("2 ** 53").unwrap(), json!(9007199254740992_i64));
    // A top-level number reads exactly like the same number nested, which goes
    // through JSON.stringify (shortest round-trip digits above 2^53).
    for source in ["2 ** 60", "-(2 ** 62)", "1e20", "1e21", "123.25", "-0", "NaN", "Infinity"] {
        let top = rt.evaluate(source).unwrap();
        let nested = rt.evaluate(&format!("[{source}]")).unwrap();
        assert_eq!(json!([top]), nested, "{source}");
    }
    assert_eq!(rt.evaluate("2 ** 60").unwrap(), json!(1152921504606847000_u64));
    assert_eq!(rt.evaluate("0.5").unwrap(), json!(0.5));
    assert_eq!(rt.evaluate("NaN").unwrap(), json!(null));
}

#[test]
fn the_timeout_path_has_the_same_semantics() {
    let mut rt = runtime();
    let budget = Duration::from_secs(5);
    assert_eq!(rt.evaluate_with_timeout("1; 2", budget).unwrap(), json!(2));
    assert!(rt.evaluate_with_timeout("throw new Error('t')", budget).unwrap_err().contains("t"));
    let error = rt.evaluate_with_timeout("while (true) {}", Duration::from_millis(200)).unwrap_err();
    assert!(error.contains("timed out"), "{error}");
    assert_eq!(rt.evaluate("6 * 7").unwrap(), json!(42), "the runtime stays usable");
}
