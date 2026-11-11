//! Uncaught exceptions and unhandled promise rejections reach both consumers
//! exactly once: the console buffer an embedder drains with
//! `take_pending_console_messages` (MCP `browser_console_messages`), and the
//! `RuntimeEvent::Exception` queue behind CDP `Runtime.exceptionThrown`.

use inbrowse_js::ops::RuntimeEvent;
use inbrowse_js::runtime::InbrowseJsRuntime;

fn runtime(console: bool, events: bool) -> InbrowseJsRuntime {
    let mut rt = InbrowseJsRuntime::new();
    rt.set_dom(inbrowse_dom::parse_html("<body><p>ok</p></body>"));
    rt.run_page_init();
    rt.set_console_messages_enabled(console);
    rt.set_runtime_events_enabled(events);
    rt
}

/// The exceptions queued for CDP, as (description, in_promise).
fn exception_events(rt: &mut InbrowseJsRuntime) -> Vec<(String, bool)> {
    rt.take_pending_runtime_events()
        .into_iter()
        .filter_map(|event| match event {
            RuntimeEvent::Exception(e) => Some((e.description, e.in_promise)),
            _ => None,
        })
        .collect()
}

#[test]
fn a_thrown_script_error_is_an_error_console_entry() {
    let mut rt = runtime(true, false);
    assert!(rt.execute_script("page.js", "throw new Error('boom')").is_err());
    let messages = rt.take_pending_console_messages();
    assert_eq!(messages.len(), 1, "{messages:?}");
    assert!(messages[0].starts_with("[error] Uncaught Error: boom"), "{messages:?}");
    assert!(messages[0].contains("page.js"), "the entry carries the location: {messages:?}");
}

#[test]
fn a_thrown_script_error_reaches_console_and_cdp_once_each() {
    let mut rt = runtime(true, true);
    assert!(rt.execute_script("page.js", "throw new TypeError('bad')").is_err());
    let messages = rt.take_pending_console_messages();
    assert_eq!(messages.len(), 1, "{messages:?}");
    assert!(messages[0].starts_with("[error] Uncaught TypeError: bad"), "{messages:?}");
    let events = exception_events(&mut rt);
    assert_eq!(events.len(), 1, "{events:?}");
    assert!(events[0].0.contains("TypeError: bad") && !events[0].1);
}

#[test]
fn nothing_is_buffered_when_console_messages_are_off() {
    let mut rt = runtime(false, true);
    assert!(rt.execute_script("page.js", "throw new Error('boom')").is_err());
    assert!(rt.take_pending_console_messages().is_empty());
    assert_eq!(exception_events(&mut rt).len(), 1);
}

#[test]
fn a_syntax_error_in_a_script_is_reported() {
    let mut rt = runtime(true, true);
    assert!(rt.execute_script("page.js", "var = ;").is_err());
    let messages = rt.take_pending_console_messages();
    assert_eq!(messages.len(), 1, "{messages:?}");
    assert!(messages[0].starts_with("[error] Uncaught"), "{messages:?}");
    assert!(messages[0].contains("SyntaxError"), "{messages:?}");
    assert_eq!(exception_events(&mut rt).len(), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn a_callback_error_is_reported_once() {
    let mut rt = runtime(true, true);
    rt.execute_script("page.js", "queueMicrotask(() => { throw new Error('in microtask'); });")
        .unwrap();
    rt.run_event_loop_bounded(50).await.unwrap();
    let messages = rt.take_pending_console_messages();
    assert_eq!(messages.len(), 1, "{messages:?}");
    assert!(messages[0].starts_with("[error] "), "{messages:?}");
    assert!(messages[0].contains("in microtask"), "{messages:?}");
    assert_eq!(exception_events(&mut rt).len(), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn an_unhandled_rejection_is_an_error_console_entry_and_an_exception() {
    let mut rt = runtime(true, true);
    rt.execute_script(
        "page.js",
        "Promise.reject(new Error('rejected')); Promise.reject('plain reason');",
    )
    .unwrap();
    rt.run_event_loop_bounded(50).await.unwrap();
    let messages = rt.take_pending_console_messages();
    assert_eq!(messages.len(), 2, "{messages:?}");
    assert!(messages[0].starts_with("[error] Uncaught (in promise) Error: rejected"), "{messages:?}");
    assert_eq!(messages[1], "[error] Uncaught (in promise) plain reason");
    let events = exception_events(&mut rt);
    assert_eq!(events.len(), 2, "{events:?}");
    assert!(events.iter().all(|(_, in_promise)| *in_promise));
    assert!(events[0].0.contains("Error: rejected"));
}

#[tokio::test(flavor = "current_thread")]
async fn a_handled_or_canceled_rejection_is_not_reported() {
    let mut rt = runtime(true, true);
    rt.execute_script(
        "page.js",
        r#"
        Promise.reject(new Error('caught')).catch(() => {});
        addEventListener('unhandledrejection', event => {
            if (event.reason === 'canceled') event.preventDefault();
        });
        Promise.reject('canceled');
        "#,
    )
    .unwrap();
    rt.run_event_loop_bounded(50).await.unwrap();
    assert!(rt.take_pending_console_messages().is_empty());
    assert!(exception_events(&mut rt).is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn an_onunhandledrejection_property_handler_can_cancel_reporting() {
    let mut rt = runtime(true, true);
    assert_eq!(
        rt.evaluate("window.onunhandledrejection").unwrap(),
        serde_json::Value::Null,
        "like a browser, no default handler is installed"
    );
    rt.execute_script(
        "page.js",
        "onunhandledrejection = event => event.preventDefault(); Promise.reject(new Error('quiet'));",
    )
    .unwrap();
    rt.run_event_loop_bounded(50).await.unwrap();
    assert!(rt.take_pending_console_messages().is_empty());
    assert!(exception_events(&mut rt).is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn a_module_that_throws_is_reported_once() {
    // A module's evaluation promise rejects, so it arrives through the
    // unhandled-rejection path and must not also be recorded by the loader.
    let mut rt = runtime(true, true);
    let result = rt
        .load_inline_module("throw new Error('module boom');", "http://127.0.0.1:8765/page.html", 2_000)
        .await;
    assert!(result.is_err());
    rt.run_event_loop_bounded(50).await.unwrap();
    let messages = rt.take_pending_console_messages();
    assert_eq!(messages.len(), 1, "{messages:?}");
    assert!(messages[0].starts_with("[error] "), "{messages:?}");
    assert!(messages[0].contains("Error: module boom"), "{messages:?}");
    assert_eq!(exception_events(&mut rt).len(), 1);
}
