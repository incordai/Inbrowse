//! "Select all, then type to replace" through the CDP input domain: Ctrl/Cmd+A
//! and the `selectAll` editing command, double and triple clicks, and
//! insertion, Backspace and Delete acting on the selected range.

use inbrowse_cdp::dispatch::{dispatch, CdpContext};
use inbrowse_cdp::types::CdpRequest;
use serde_json::{json, Value};

async fn cdp(ctx: &mut CdpContext, method: &str, params: Value) -> Value {
    let response = dispatch(
        &CdpRequest {
            id: 1,
            method: method.to_string(),
            params,
            session_id: Some("selection".to_string()),
        },
        ctx,
    )
    .await;
    assert!(response.error.is_none(), "{method}: {:?}", response.error);
    response.result.unwrap_or_else(|| json!({}))
}

async fn evaluate(ctx: &mut CdpContext, expression: &str) -> Value {
    let result = cdp(
        ctx,
        "Runtime.evaluate",
        json!({"expression": expression, "returnByValue": true}),
    )
    .await;
    assert!(result.get("exceptionDetails").is_none(), "{result}");
    result["result"]["value"].clone()
}

const PAGE: &str = "data:text/html,<input id='q' value='initial'><textarea id='t'>one two</textarea>\
    <input id='e' type='email' value='a@b.c'><a href='%23x'><input id='linked' value='in link'></a><p id='p'>plain text</p>";

async fn setup(focus: &str) -> CdpContext {
    let mut ctx = CdpContext::new();
    let page_id = ctx.create_page();
    ctx.sessions.insert("selection".to_string(), page_id);
    cdp(&mut ctx, "Page.navigate", json!({"url": PAGE, "waitUntil": "load"})).await;
    evaluate(&mut ctx, &format!("document.getElementById('{focus}').focus()")).await;
    ctx
}

async fn key(ctx: &mut CdpContext, kind: &str, key: &str, code: &str, modifiers: u64, text: &str) {
    cdp(
        ctx,
        "Input.dispatchKeyEvent",
        json!({"type": kind, "key": key, "code": code, "modifiers": modifiers, "text": text}),
    )
    .await;
}

/// Type `text` the way Puppeteer and Playwright do: one keyDown carrying the text.
async fn type_text(ctx: &mut CdpContext, text: &str) {
    for ch in text.chars() {
        let s = ch.to_string();
        key(ctx, "keyDown", &s, &format!("Key{}", s.to_uppercase()), 0, &s).await;
        key(ctx, "keyUp", &s, &format!("Key{}", s.to_uppercase()), 0, "").await;
    }
}

async fn value(ctx: &mut CdpContext, id: &str) -> Value {
    evaluate(ctx, &format!("document.getElementById('{id}').value")).await
}

async fn click(ctx: &mut CdpContext, id: &str, count: u64) {
    let rect = evaluate(
        ctx,
        &format!("(() => {{ const r = document.getElementById('{id}').getBoundingClientRect(); return [r.left + 2, r.top + 2]; }})()"),
    )
    .await;
    let (x, y) = (rect[0].as_f64().unwrap_or(1.0), rect[1].as_f64().unwrap_or(1.0));
    for n in 1..=count {
        for kind in ["mousePressed", "mouseReleased"] {
            cdp(
                ctx,
                "Input.dispatchMouseEvent",
                json!({"type": kind, "x": x, "y": y, "button": "left", "clickCount": n,
                       "buttons": if kind == "mousePressed" { 1 } else { 0 }}),
            )
            .await;
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn control_a_then_typing_replaces_the_value() {
    let mut ctx = setup("q").await;
    key(&mut ctx, "keyDown", "Control", "ControlLeft", 2, "").await;
    key(&mut ctx, "keyDown", "a", "KeyA", 2, "").await;
    assert_eq!(
        evaluate(&mut ctx, "[q.selectionStart, q.selectionEnd]").await,
        json!([0, 7])
    );
    key(&mut ctx, "keyUp", "a", "KeyA", 2, "").await;
    key(&mut ctx, "keyUp", "Control", "ControlLeft", 0, "").await;
    type_text(&mut ctx, "replaced").await;
    assert_eq!(value(&mut ctx, "q").await, "replaced");
}

#[tokio::test(flavor = "current_thread")]
async fn a_ctrl_shortcut_types_nothing_even_when_the_client_sends_text() {
    let mut ctx = setup("q").await;
    key(&mut ctx, "keyDown", "a", "KeyA", 2, "a").await;
    assert_eq!(value(&mut ctx, "q").await, "initial");
    assert_eq!(
        evaluate(&mut ctx, "[q.selectionStart, q.selectionEnd]").await,
        json!([0, 7])
    );
}

#[tokio::test(flavor = "current_thread")]
async fn the_select_all_editing_command_and_the_meta_key_select_all() {
    let mut ctx = setup("q").await;
    cdp(
        &mut ctx,
        "Input.dispatchKeyEvent",
        json!({"type": "keyDown", "key": "a", "code": "KeyA", "modifiers": 4, "commands": ["selectAll"]}),
    )
    .await;
    assert_eq!(evaluate(&mut ctx, "[q.selectionStart, q.selectionEnd]").await, json!([0, 7]));
    type_text(&mut ctx, "x").await;
    assert_eq!(value(&mut ctx, "q").await, "x");

    evaluate(&mut ctx, "q.setSelectionRange(1, 1)").await;
    cdp(
        &mut ctx,
        "Input.dispatchKeyEvent",
        json!({"type": "rawKeyDown", "key": "Unidentified", "commands": ["selectAll"]}),
    )
    .await;
    assert_eq!(evaluate(&mut ctx, "[q.selectionStart, q.selectionEnd]").await, json!([0, 1]));
}

#[tokio::test(flavor = "current_thread")]
async fn a_canceled_keydown_does_not_select_all() {
    let mut ctx = setup("q").await;
    evaluate(&mut ctx, "q.addEventListener('keydown', e => e.preventDefault()); q.setSelectionRange(2, 2)").await;
    key(&mut ctx, "keyDown", "a", "KeyA", 2, "").await;
    assert_eq!(evaluate(&mut ctx, "[q.selectionStart, q.selectionEnd]").await, json!([2, 2]));
}

#[tokio::test(flavor = "current_thread")]
async fn select_all_works_in_a_textarea_and_in_inputs_without_selection_members() {
    let mut ctx = setup("t").await;
    key(&mut ctx, "keyDown", "a", "KeyA", 2, "").await;
    type_text(&mut ctx, "new").await;
    assert_eq!(value(&mut ctx, "t").await, "new");

    evaluate(&mut ctx, "e.focus()").await;
    key(&mut ctx, "keyDown", "a", "KeyA", 2, "").await;
    assert_eq!(evaluate(&mut ctx, "e.selectionStart").await, Value::Null);
    type_text(&mut ctx, "z").await;
    assert_eq!(value(&mut ctx, "e").await, "z");
}

#[tokio::test(flavor = "current_thread")]
async fn control_a_outside_a_text_control_selects_the_document() {
    let mut ctx = setup("q").await;
    evaluate(&mut ctx, "q.blur()").await;
    key(&mut ctx, "keyDown", "a", "KeyA", 2, "").await;
    assert_eq!(
        evaluate(&mut ctx, "[getSelection().isCollapsed, getSelection().toString().includes('plain text')]").await,
        json!([false, true])
    );
}

#[tokio::test(flavor = "current_thread")]
async fn a_triple_click_selects_everything_and_typing_replaces_it() {
    let mut ctx = setup("p").await;
    click(&mut ctx, "q", 3).await;
    assert_eq!(evaluate(&mut ctx, "[q.selectionStart, q.selectionEnd]").await, json!([0, 7]));
    type_text(&mut ctx, "typed").await;
    assert_eq!(value(&mut ctx, "q").await, "typed");
}

#[tokio::test(flavor = "current_thread")]
async fn a_triple_click_works_on_a_control_inside_a_link_and_on_a_textarea() {
    let mut ctx = setup("p").await;
    click(&mut ctx, "linked", 3).await;
    assert_eq!(
        evaluate(&mut ctx, "[linked.selectionStart, linked.selectionEnd, location.hash]").await,
        json!([0, 7, ""])
    );
    click(&mut ctx, "t", 3).await;
    assert_eq!(evaluate(&mut ctx, "[t.selectionStart, t.selectionEnd]").await, json!([0, 7]));
}

#[tokio::test(flavor = "current_thread")]
async fn a_double_click_selects_the_word_at_the_caret() {
    let mut ctx = setup("p").await;
    evaluate(&mut ctx, "q.value = 'hello big world'; q.setSelectionRange(8, 8)").await;
    click(&mut ctx, "q", 2).await;
    // The first click of the pair collapses the caret; without text-offset
    // geometry for control values it lands at the end, so the word is the last.
    assert_eq!(evaluate(&mut ctx, "[q.selectionStart, q.selectionEnd]").await, json!([10, 15]));
}

#[tokio::test(flavor = "current_thread")]
async fn a_single_click_collapses_a_selection_and_typing_appends() {
    let mut ctx = setup("p").await;
    evaluate(&mut ctx, "q.select()").await;
    click(&mut ctx, "q", 1).await;
    assert_eq!(evaluate(&mut ctx, "[q.selectionStart, q.selectionEnd]").await, json!([7, 7]));
    type_text(&mut ctx, "!").await;
    assert_eq!(value(&mut ctx, "q").await, "initial!");
}

#[tokio::test(flavor = "current_thread")]
async fn insert_text_replaces_a_set_selection_range() {
    let mut ctx = setup("q").await;
    evaluate(&mut ctx, "q.setSelectionRange(0, 3)").await;
    cdp(&mut ctx, "Input.insertText", json!({"text": "XY"})).await;
    assert_eq!(value(&mut ctx, "q").await, "XYtial");
    assert_eq!(evaluate(&mut ctx, "[q.selectionStart, q.selectionEnd]").await, json!([2, 2]));
}

#[tokio::test(flavor = "current_thread")]
async fn insert_text_after_select_replaces_a_prefilled_value() {
    // The Playwright fill() sequence: focus, select, insertText.
    let mut ctx = setup("q").await;
    evaluate(&mut ctx, "q.select()").await;
    cdp(&mut ctx, "Input.insertText", json!({"text": "filled"})).await;
    assert_eq!(value(&mut ctx, "q").await, "filled");
}

#[tokio::test(flavor = "current_thread")]
async fn backspace_and_delete_remove_the_selected_range() {
    let mut ctx = setup("q").await;
    key(&mut ctx, "keyDown", "a", "KeyA", 2, "").await;
    key(&mut ctx, "keyDown", "Backspace", "Backspace", 0, "").await;
    assert_eq!(value(&mut ctx, "q").await, "");

    evaluate(&mut ctx, "q.value = 'abcdef'; q.setSelectionRange(1, 4)").await;
    key(&mut ctx, "keyDown", "Delete", "Delete", 0, "").await;
    assert_eq!(value(&mut ctx, "q").await, "aef");
    assert_eq!(evaluate(&mut ctx, "[q.selectionStart, q.selectionEnd]").await, json!([1, 1]));
    key(&mut ctx, "keyDown", "Delete", "Delete", 0, "").await;
    assert_eq!(value(&mut ctx, "q").await, "af");
    key(&mut ctx, "keyDown", "Backspace", "Backspace", 0, "").await;
    assert_eq!(value(&mut ctx, "q").await, "f");
}

#[tokio::test(flavor = "current_thread")]
async fn typing_without_a_selection_still_appends() {
    let mut ctx = setup("q").await;
    type_text(&mut ctx, "xy").await;
    assert_eq!(value(&mut ctx, "q").await, "initialxy");
    assert_eq!(evaluate(&mut ctx, "[q.selectionStart, q.selectionEnd]").await, json!([9, 9]));
}
