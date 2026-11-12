//! Selection state of text controls (HTML "text selection" members and the
//! `__inbrowse_*` helpers the CDP input domain edits through): the caret sits
//! at the end of the value until moved, offsets are clamped, the members apply
//! only to some `<input>` types, and select() and setSelectionRange() fire
//! `select`.

use inbrowse_js::runtime::InbrowseJsRuntime;
use serde_json::json;

fn runtime(html: &str) -> InbrowseJsRuntime {
    let mut rt = InbrowseJsRuntime::new();
    rt.set_dom(inbrowse_dom::parse_html(html));
    rt.run_page_init();
    rt
}

const FIELDS: &str = "<input id=i value='initial'><textarea id=t>line one\nline two</textarea>\
    <input id=e type=email value='a@b.c'><input id=n type=number value='42'>\
    <input id=c type=checkbox><div id=d></div>";

#[test]
fn the_caret_starts_at_the_end_of_the_value() {
    let mut rt = runtime(FIELDS);
    let value = rt
        .evaluate(
            r#"(() => [i, t].map(f => [f.selectionStart, f.selectionEnd, f.selectionDirection, f.value.length]))()"#,
        )
        .unwrap();
    assert_eq!(value, json!([[7, 7, "none", 7], [17, 17, "none", 17]]));
}

#[test]
fn assigning_a_different_value_moves_the_caret_to_the_end() {
    let mut rt = runtime(FIELDS);
    let value = rt
        .evaluate(
            r#"(() => {
                i.setSelectionRange(1, 3, 'backward');
                i.value = 'initial';
                const same = [i.selectionStart, i.selectionEnd, i.selectionDirection];
                i.value = 'changed value';
                const different = [i.selectionStart, i.selectionEnd, i.selectionDirection];
                return [same, different];
            })()"#,
        )
        .unwrap();
    assert_eq!(value, json!([[1, 3, "backward"], [13, 13, "none"]]));
}

#[test]
fn set_selection_range_clamps_and_orders_its_arguments() {
    let mut rt = runtime(FIELDS);
    let value = rt
        .evaluate(
            r#"(() => {
                const read = () => [i.selectionStart, i.selectionEnd, i.selectionDirection];
                const out = [];
                i.setSelectionRange(2, 4, 'forward'); out.push(read());
                i.setSelectionRange(5, 2); out.push(read());
                i.setSelectionRange(3, 99, 'backward'); out.push(read());
                i.setSelectionRange(50, 60); out.push(read());
                i.setSelectionRange(-1, 4); out.push(read());
                i.setSelectionRange(1, 3, 'bogus'); out.push(read());
                i.setSelectionRange('2', '4.9'); out.push(read());
                return out;
            })()"#,
        )
        .unwrap();
    assert_eq!(
        value,
        json!([
            [2, 4, "forward"],
            [2, 2, "none"],
            [3, 7, "backward"],
            [7, 7, "none"],
            [4, 4, "none"],
            [1, 3, "none"],
            [2, 4, "none"]
        ])
    );
}

#[test]
fn the_individual_selection_setters_adjust_the_range() {
    let mut rt = runtime(FIELDS);
    let value = rt
        .evaluate(
            r#"(() => {
                const read = () => [i.selectionStart, i.selectionEnd, i.selectionDirection];
                const out = [];
                i.setSelectionRange(2, 4);
                i.selectionEnd = 6; out.push(read());
                i.selectionStart = 1; out.push(read());
                i.selectionStart = 6.9; out.push(read());
                i.selectionStart = 7; i.selectionEnd = 3; out.push(read());
                i.setSelectionRange(1, 4);
                i.selectionDirection = 'backward'; out.push(read());
                i.selectionDirection = 'sideways'; out.push(read());
                return out;
            })()"#,
        )
        .unwrap();
    assert_eq!(
        value,
        json!([
            [2, 6, "none"],
            [1, 6, "none"],
            [6, 6, "none"],
            [3, 3, "none"],
            [1, 4, "backward"],
            [1, 4, "none"]
        ])
    );
}

#[test]
fn selection_members_apply_only_to_text_like_input_types() {
    let mut rt = runtime(FIELDS);
    let value = rt
        .evaluate(
            r#"(() => {
                const name = fn => { try { fn(); return 'ok'; } catch (e) { return e.name; } };
                const out = {};
                for (const id of ['e', 'n', 'c']) {
                    const f = document.getElementById(id);
                    out[id] = [f.selectionStart, f.selectionEnd, f.selectionDirection,
                        name(() => { f.selectionStart = 1; }),
                        name(() => f.setSelectionRange(0, 1)),
                        name(() => f.setRangeText('x'))];
                }
                const d = document.getElementById('d');
                out.div = [d.selectionStart, name(() => { d.selectionStart = 5; }), d.selectionStart,
                    name(() => d.setSelectionRange(0, 1))];
                return out;
            })()"#,
        )
        .unwrap();
    let rejected = json!([null, null, null, "InvalidStateError", "InvalidStateError", "InvalidStateError"]);
    assert_eq!(value["e"], rejected);
    assert_eq!(value["n"], rejected);
    assert_eq!(value["c"], rejected);
    // Not a text control: no accessor behaviour, an ordinary expando.
    assert_eq!(value["div"], json!([null, "ok", 5, "TypeError"]));
}

#[test]
fn set_range_text_replaces_and_places_the_selection_by_mode() {
    let mut rt = runtime("<input id=i value='abcdef'><input id=empty>");
    let value = rt
        .evaluate(
            r#"(() => {
                const out = [];
                const state = () => [i.value, i.selectionStart, i.selectionEnd];
                i.setSelectionRange(1, 3);
                i.setRangeText('XYZ'); out.push(state());
                i.setSelectionRange(0, 6); i.value = 'abcdef'; i.setSelectionRange(4, 6);
                i.setRangeText('-', 1, 3); out.push(state());
                i.value = 'abcdef'; i.setSelectionRange(1, 3);
                i.setRangeText('12345', 1, 3, 'select'); out.push(state());
                i.value = 'abcdef'; i.setRangeText('12', 1, 3, 'start'); out.push(state());
                i.value = 'abcdef'; i.setRangeText('12', 1, 3, 'end'); out.push(state());
                // An empty control accepts text, which the old implementation dropped.
                empty.setRangeText('typed'); out.push([empty.value, empty.selectionStart, empty.selectionEnd]);
                let error = null;
                try { i.setRangeText('x', 4, 2); } catch (e) { error = e.name; }
                out.push(error);
                return out;
            })()"#,
        )
        .unwrap();
    assert_eq!(
        value,
        json!([
            ["aXYZdef", 1, 1],
            ["a-def", 3, 5],
            ["a12345def", 1, 6],
            ["a12def", 1, 1],
            ["a12def", 3, 3],
            ["typed", 0, 0],
            "IndexSizeError"
        ])
    );
}

#[tokio::test(flavor = "current_thread")]
async fn select_and_set_selection_range_fire_select_when_the_range_changes() {
    let mut rt = runtime(FIELDS);
    rt.evaluate(
        r#"(() => {
            globalThis.fired = [];
            for (const id of ['i', 'e', 'c']) {
                document.getElementById(id).addEventListener('select', e => fired.push([e.target.id, e.bubbles, e.isTrusted]));
            }
            i.select();
            e.select();
            c.select();
            i.setSelectionRange(0, 7);
            i.setSelectionRange(1, 2);
        })()"#,
    )
    .unwrap();
    // Each select event is a queued task; one short turn can miss the last
    // on a loaded host, so pump until three arrived (bounded), then one more
    // turn so an unexpected fourth would still be seen.
    for _ in 0..100 {
        rt.run_event_loop_bounded(30).await.unwrap();
        if rt.evaluate("fired.length >= 3").unwrap() == json!(true) {
            break;
        }
    }
    rt.run_event_loop_bounded(50).await.unwrap();
    assert_eq!(
        rt.evaluate("[fired, i.selectionStart, i.selectionEnd, e.value.length]").unwrap(),
        json!([[["i", true, true], ["e", true, true], ["i", true, true]], 1, 2, 5])
    );
}

#[test]
fn select_selects_the_whole_value_of_every_input_it_applies_to() {
    let mut rt = runtime(FIELDS);
    let value = rt
        .evaluate(
            r#"(() => {
                i.select(); t.select();
                return [i.selectionStart, i.selectionEnd, t.selectionStart, t.selectionEnd,
                    __inbrowse_selectionRange(e), __inbrowse_selectionRange(n), __inbrowse_selectionRange(c)];
            })()"#,
        )
        .unwrap();
    assert_eq!(value, json!([0, 7, 0, 17, [5, 5], [2, 2], null]));
}

#[test]
fn internal_helpers_edit_the_selection_of_any_text_entry_control() {
    let mut rt = runtime(FIELDS);
    let value = rt
        .evaluate(
            r#"(() => {
                const out = [];
                __inbrowse_selectAll(e);
                out.push(__inbrowse_selectionRange(e));
                __inbrowse_setSelection(e, 1, 3);
                out.push(__inbrowse_selectionRange(e));
                // The IDL members of an email input still report null.
                out.push(e.selectionStart);
                i.value = 'hello big world';
                i.setSelectionRange(8, 8);
                __inbrowse_selectWord(i);
                out.push([i.selectionStart, i.selectionEnd]);
                i.setSelectionRange(5, 5);
                __inbrowse_selectWord(i);
                out.push([i.selectionStart, i.selectionEnd]);
                i.setSelectionRange(15, 15);
                __inbrowse_selectWord(i);
                out.push([i.selectionStart, i.selectionEnd]);
                // Between punctuation and a space the space run is selected.
                i.value = 'a, b';
                i.setSelectionRange(2, 2);
                __inbrowse_selectWord(i);
                out.push([i.selectionStart, i.selectionEnd]);
                return out;
            })()"#,
        )
        .unwrap();
    assert_eq!(
        value,
        json!([[0, 5], [1, 3], null, [6, 9], [0, 5], [10, 15], [2, 3]])
    );
}

#[test]
fn select_all_outside_a_text_control_selects_the_document() {
    let mut rt = runtime("<body><p id=p>hello</p><button id=b>x</button></body>");
    let value = rt
        .evaluate(
            r#"(() => {
                const result = __inbrowse_selectAll(b);
                return [result, getSelection().toString().includes('hello'), getSelection().isCollapsed];
            })()"#,
        )
        .unwrap();
    assert_eq!(value, json!([true, true, false]));
}
