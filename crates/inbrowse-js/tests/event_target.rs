//! EventTarget is its own base class, as in browsers: Node and the Window
//! global inherit it (js/bootstrap.js `class EventTarget` and the Window
//! prototype wiring). Before this, EventTarget was an alias of Node, so
//! `window` was not an EventTarget and YouTube's Shady DOM polyfill threw on
//! `window.__shady_native_addEventListener`.

use inbrowse_js::runtime::InbrowseJsRuntime;
use serde_json::json;

fn runtime() -> InbrowseJsRuntime {
    let mut rt = InbrowseJsRuntime::new();
    rt.set_dom(inbrowse_dom::parse_html("<body><p id=p>x</p></body>"));
    rt.run_page_init();
    rt
}

#[test]
fn window_and_nodes_inherit_a_separate_event_target() {
    let mut rt = runtime();
    let result = rt
        .evaluate(
            r#"[
        window instanceof EventTarget,
        window instanceof Window,
        Object.getPrototypeOf(window) === Window.prototype,
        Object.getPrototypeOf(Window.prototype) === EventTarget.prototype,
        EventTarget !== Node,
        Object.getPrototypeOf(Node.prototype) === EventTarget.prototype,
        document.getElementById('p') instanceof EventTarget,
        Object.prototype.hasOwnProperty.call(EventTarget.prototype, 'addEventListener'),
        Object.prototype.hasOwnProperty.call(Node.prototype, 'addEventListener'),
        'appendChild' in window,
        'nodeType' in window
    ]"#,
        )
        .unwrap();
    assert_eq!(
        result,
        json!([true, true, true, true, true, true, true, true, false, false, false])
    );
}

#[test]
fn a_constructed_event_target_is_not_a_node() {
    let mut rt = runtime();
    let result = rt
        .evaluate(
            r#"(() => {
        const t = new EventTarget(), seen = [];
        t.addEventListener('ping', e => seen.push(e.type, e.target === t));
        t.dispatchEvent(new Event('ping'));
        return [seen, t instanceof Node, 'nodeType' in t];
    })()"#,
        )
        .unwrap();
    assert_eq!(result, json!([["ping", true], false, false]));
}

#[test]
fn a_method_saved_on_event_target_prototype_reaches_window() {
    // What webcomponents-sd.js does: keep the native method under another
    // name on EventTarget.prototype, then call it on window.
    let mut rt = runtime();
    let result = rt
        .evaluate(
            r#"(() => {
        EventTarget.prototype.__test_native_addEventListener = EventTarget.prototype.addEventListener;
        const seen = [];
        window.__test_native_addEventListener('ping', e => seen.push(e.currentTarget === window));
        window.dispatchEvent(new Event('ping'));
        return seen;
    })()"#,
        )
        .unwrap();
    assert_eq!(result, json!([true]));
}

#[test]
fn bare_add_event_listener_without_a_receiver_targets_window() {
    let mut rt = runtime();
    let result = rt
        .evaluate(
            r#"(function () {
        'use strict';
        const seen = [];
        addEventListener('pong', () => seen.push('pong'));
        dispatchEvent(new Event('pong'));
        return seen;
    })()"#,
        )
        .unwrap();
    assert_eq!(result, json!(["pong"]));
}
