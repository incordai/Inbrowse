//! Form submission and `FormData` share one "construct the entry list"
//! implementation (bootstrap.js `_constructEntryList`), and `<select>`
//! selectedness lives in `option.selected` alone. These tests drive the real
//! navigation a form submission queues, so the query string and POST body
//! are what the server would receive, not just what `FormData` reports.

use inbrowse_js::runtime::InbrowseJsRuntime;
use serde_json::{json, Value};

fn runtime(html: &str) -> InbrowseJsRuntime {
    let mut rt = InbrowseJsRuntime::new();
    rt.set_url("http://127.0.0.1:8765/page.html");
    rt.set_dom(inbrowse_dom::parse_html(html));
    rt.run_page_init();
    rt
}

/// Run `script` in a fresh page and return the navigation it queued, as
/// (url, method, body).
fn submitted(html: &str, script: &str) -> (String, String, String) {
    let mut rt = runtime(html);
    rt.evaluate(script).unwrap();
    rt.take_pending_navigation()
        .unwrap_or_else(|| panic!("no navigation queued by {script}"))
}

const SELECT_FORM: &str = "<form action='/submit'><select name='s'>\
    <option>a</option><option selected>b</option><option>c</option></select>\
    <button>Go</button></form>";

/// The three ways to submit a form: the submit() method, requestSubmit(), and
/// a click on the submit button.
const TRIGGERS: [&str; 3] = [
    "document.forms[0].submit()",
    "document.forms[0].requestSubmit()",
    "document.querySelector('button').click()",
];

#[test]
fn every_select_setter_is_submitted_by_every_trigger() {
    let setters = [
        ("document.querySelector('select').selectedIndex = 0", "a"),
        ("document.querySelector('select').selectedIndex = 2", "c"),
        ("document.querySelector('select').value = 'a'", "a"),
        ("document.querySelector('select').options[0].selected = true", "a"),
        ("document.querySelector('select').options[2].selected = true", "c"),
    ];
    for (setter, expected) in setters {
        for trigger in TRIGGERS {
            let (url, method, body) = submitted(SELECT_FORM, &format!("{setter}; {trigger}"));
            assert_eq!(url, format!("http://127.0.0.1:8765/submit?s={expected}"), "{setter} then {trigger}");
            assert_eq!((method.as_str(), body.as_str()), ("GET", ""));
        }
    }
}

#[test]
fn an_untouched_select_submits_its_default_selection() {
    let (url, _, _) = submitted(SELECT_FORM, "document.forms[0].submit()");
    assert_eq!(url, "http://127.0.0.1:8765/submit?s=b");
    let (url, _, _) = submitted(
        "<form action='/submit'><select name='s'><option>x</option><option>y</option></select></form>",
        "document.forms[0].submit()",
    );
    assert_eq!(url, "http://127.0.0.1:8765/submit?s=x", "the first option is the default");
}

#[test]
fn a_post_form_sends_the_chosen_option_in_the_body() {
    let html = "<form action='/submit' method='post'><select name='s'>\
        <option>a</option><option selected>b</option></select></form>";
    let (url, method, body) =
        submitted(html, "document.querySelector('select').selectedIndex = 0; document.forms[0].submit()");
    assert_eq!((url.as_str(), method.as_str(), body.as_str()), ("http://127.0.0.1:8765/submit", "POST", "s=a"));
}

#[test]
fn selected_index_getter_and_setter_follow_the_specification() {
    let mut rt = runtime(
        "<select id=s><option selected>a</option><option>b</option><option>c</option></select>\
         <select id=m multiple><option>x</option><option>y</option></select>\
         <select id=big size=3><option>p</option><option>q</option></select>",
    );
    let value = rt
        .evaluate(
            r#"(() => {
                const s = document.getElementById('s');
                const log = [];
                s.selectedIndex = 2;
                log.push([s.selectedIndex, s.value, s.options[2].selected, s.options[0].selected]);
                s.selectedIndex = '1';
                log.push([s.selectedIndex, s.value]);
                s.selectedIndex = NaN;
                log.push([s.selectedIndex, s.value]);
                s.selectedIndex = 99;
                log.push([s.selectedIndex, s.value, [...s.options].filter(o => o.selected).length]);
                s.selectedIndex = -1;
                log.push([s.selectedIndex, s.value]);
                s.selectedIndex = 1.9;
                log.push([s.selectedIndex]);
                const m = document.getElementById('m');
                log.push([m.selectedIndex, m.value, m.selectedOptions.length]);
                const big = document.getElementById('big');
                log.push([big.selectedIndex, big.value]);
                return log;
            })()"#,
        )
        .unwrap();
    assert_eq!(
        value,
        json!([
            [2, "c", true, false],
            [1, "b"],
            [0, "a"],
            [-1, "", 0],
            [-1, ""],
            [1],
            [-1, "", 0],
            [-1, ""]
        ])
    );
}

#[test]
fn option_selected_keeps_a_single_select_to_one_option() {
    let mut rt = runtime("<select id=s><option>a</option><option selected>b</option><option>c</option></select>");
    let value = rt
        .evaluate(
            r#"(() => {
                const s = document.getElementById('s');
                const [a, b, c] = s.options;
                a.selected = true;
                const afterA = [b.selected, s.selectedIndex];
                c.selected = true;
                const afterC = [a.selected, b.selected, s.selectedIndex];
                // Deselecting the only selected option resets to the first one.
                c.selected = false;
                return [afterA, afterC, s.selectedIndex, a.selected, s.outerHTML];
            })()"#,
        )
        .unwrap();
    assert_eq!(value[0], json!([false, 0]));
    assert_eq!(value[1], json!([false, false, 2]));
    assert_eq!(value[2], json!(0));
    assert_eq!(value[3], json!(true));
    assert_eq!(
        value[4],
        json!("<select id=\"s\"><option selected=\"\">a</option><option>b</option><option>c</option></select>")
    );
}

#[test]
fn a_select_value_matching_nothing_selects_nothing() {
    let mut rt = runtime("<select id=s><option>a</option><option>b</option></select>");
    let value = rt
        .evaluate(
            "(() => { const s = document.getElementById('s'); s.value = 'zzz'; return [s.selectedIndex, s.value]; })()",
        )
        .unwrap();
    assert_eq!(value, json!([-1, ""]));
    let (url, _, _) = submitted(
        "<form action='/submit'><select name='s'><option>a</option></select></form>",
        "document.querySelector('select').value = 'zzz'; document.forms[0].submit()",
    );
    assert_eq!(url, "http://127.0.0.1:8765/submit", "no selected option, no entry");
}

#[test]
fn selected_options_is_a_live_collection_of_the_selected_options() {
    let mut rt = runtime(
        "<select id=m multiple><option>x</option><option>y</option><option>z</option></select>\
         <select id=s><option>a</option><option>b</option></select><div id=d></div>",
    );
    let value = rt
        .evaluate(
            r#"(() => {
                const m = document.getElementById('m');
                const before = [...m.selectedOptions].map(o => o.value);
                m.options[0].selected = true;
                m.options[1].selected = true;
                const both = [...m.selectedOptions].map(o => o.value);
                m.options[0].selected = false;
                const one = [...m.selectedOptions].map(o => o.value);
                const s = document.getElementById('s');
                return {
                    before, both, one,
                    isCollection: m.selectedOptions instanceof HTMLCollection,
                    single: [...s.selectedOptions].map(o => o.value),
                    notSelect: document.getElementById('d').selectedOptions,
                };
            })()"#,
        )
        .unwrap();
    assert_eq!(value["before"], json!([]));
    assert_eq!(value["both"], json!(["x", "y"]));
    assert_eq!(value["one"], json!(["y"]));
    assert_eq!(value["isCollection"], json!(true));
    assert_eq!(value["single"], json!(["a"]));
    assert_eq!(value["notSelect"], Value::Null);
}

#[test]
fn a_multiple_select_submits_one_entry_per_selected_option() {
    let html = "<form action='/submit'><select name='m' multiple>\
        <option>x</option><option>y</option><option disabled>z</option></select></form>";
    for trigger in TRIGGERS.iter().take(2) {
        let (url, _, _) = submitted(
            html,
            &format!(
                "const o = document.querySelector('select').options; o[0].selected = true; o[1].selected = true; o[2].selected = true; {trigger}"
            ),
        );
        assert_eq!(url, "http://127.0.0.1:8765/submit?m=x&m=y", "disabled option is skipped: {trigger}");
    }
    let (url, _, _) = submitted(html, "document.forms[0].submit()");
    assert_eq!(url, "http://127.0.0.1:8765/submit", "a multiple select with nothing selected sends nothing");
}

#[test]
fn form_data_and_the_submitted_query_come_from_the_same_entries() {
    let html = "<form action='/submit'>\
        <input name='t' value='a b'>\
        <input type='checkbox' name='c' checked>\
        <input type='checkbox' name='c2' value='v'>\
        <input type='radio' name='r' value='1'><input type='radio' name='r' value='2' checked>\
        <select name='s'><option>a</option><option>b</option></select>\
        <select name='m' multiple><option>x</option><option selected>y</option></select>\
        <textarea name='ta'>hello</textarea>\
        <input name='off' value='no' disabled>\
        <input value='nameless'>\
        <input type='submit' name='go' value='Go'>\
        </form>";
    let script = "const f = document.forms[0]; f.querySelector('select').selectedIndex = 1;";
    let mut rt = runtime(html);
    let entries = rt
        .evaluate(&format!(
            "(() => {{ {script} return [...new FormData(document.forms[0])]; }})()"
        ))
        .unwrap();
    assert_eq!(
        entries,
        json!([
            ["t", "a b"],
            ["c", "on"],
            ["r", "2"],
            ["s", "b"],
            ["m", "y"],
            ["ta", "hello"]
        ])
    );
    let (url, _, _) = submitted(html, &format!("{script} document.forms[0].submit()"));
    assert_eq!(url, "http://127.0.0.1:8765/submit?t=a+b&c=on&r=2&s=b&m=y&ta=hello");
}

#[test]
fn the_submitter_button_contributes_its_name_and_value() {
    let html = "<form action='/submit'><input name='t' value='v'>\
        <button name='act' value='save'>Save</button>\
        <button name='act' value='delete'>Delete</button>\
        <input type='image' name='img' src='x.png'></form>";
    let (url, _, _) = submitted(html, "document.querySelectorAll('button')[1].click()");
    assert_eq!(url, "http://127.0.0.1:8765/submit?t=v&act=delete");
    let (url, _, _) = submitted(html, "document.forms[0].submit()");
    assert_eq!(url, "http://127.0.0.1:8765/submit?t=v", "no submitter, no button entry");
    let (url, _, _) = submitted(html, "document.querySelector('input[type=image]').click()");
    assert_eq!(url, "http://127.0.0.1:8765/submit?t=v&img.x=0&img.y=0");
    let (url, _, _) = submitted(
        "<form action='/submit'><input type='image' src='x.png'></form>",
        "document.querySelector('input[type=image]').click()",
    );
    assert_eq!(url, "http://127.0.0.1:8765/submit?x=0&y=0", "an unnamed image button sends bare coordinates");
    let mut rt = runtime(html);
    let entries = rt
        .evaluate(
            "[...new FormData(document.forms[0], document.querySelectorAll('button')[0])]",
        )
        .unwrap();
    assert_eq!(entries, json!([["t", "v"], ["act", "save"]]));
}

#[test]
fn form_data_rejects_a_submitter_that_is_not_a_submit_button_of_the_form() {
    let mut rt = runtime(
        "<form><input name=t><button type=button id=plain>x</button></form><form id=other><button id=other_b>y</button></form>",
    );
    let value = rt
        .evaluate(
            r#"(() => {
                const f = document.forms[0];
                const name = fn => { try { fn(); return null; } catch (e) { return e.constructor.name + ':' + e.name; } };
                return [
                    name(() => new FormData(f, document.getElementById('plain'))),
                    name(() => new FormData(f, document.getElementById('other_b'))),
                ];
            })()"#,
        )
        .unwrap();
    assert_eq!(value, json!(["TypeError:TypeError", "DOMException:NotFoundError"]));
}

#[test]
fn disabled_fieldsets_and_options_are_skipped() {
    let html = "<form action='/submit'>\
        <fieldset disabled><legend><input name='in_legend' value='1'></legend>\
        <input name='in_set' value='2'><select name='sel'><option>q</option></select></fieldset>\
        <fieldset disabled><fieldset><input name='nested' value='3'></fieldset></fieldset>\
        <select name='s'><option disabled selected>d</option><option>e</option>\
        <optgroup disabled><option>f</option></optgroup></select>\
        <input name='ok' value='4'></form>";
    let (url, _, _) = submitted(html, "document.forms[0].submit()");
    assert_eq!(
        url, "http://127.0.0.1:8765/submit?in_legend=1&ok=4",
        "the first legend of a disabled fieldset is exempt; a selected disabled option sends nothing"
    );
}

#[test]
fn textarea_submits_its_current_value_with_crlf_newlines() {
    let html = "<form action='/submit'><textarea name='t'>orig</textarea></form>";
    let (url, _, _) = submitted(html, "document.forms[0].submit()");
    assert_eq!(url, "http://127.0.0.1:8765/submit?t=orig");
    let (url, _, _) = submitted(html, "document.querySelector('textarea').value = ''; document.forms[0].submit()");
    assert_eq!(url, "http://127.0.0.1:8765/submit?t=", "a cleared textarea submits nothing, not the original text");
    let (url, _, _) = submitted(
        html,
        "document.querySelector('textarea').value = 'a\\nb\\r\\nc'; document.forms[0].submit()",
    );
    assert_eq!(url, "http://127.0.0.1:8765/submit?t=a%0D%0Ab%0D%0Ac");
}

#[test]
fn controls_outside_the_form_join_through_the_form_attribute() {
    let html = "<input name='outside' value='o' form='f'><input name='stray' value='x'>\
        <form id='f' action='/submit'><input name='inside' value='i'>\
        <input name='elsewhere' value='no' form='other'></form><form id='other'></form>";
    let (url, _, _) = submitted(html, "document.getElementById('f').submit()");
    assert_eq!(url, "http://127.0.0.1:8765/submit?outside=o&inside=i");
}

#[test]
fn submission_uses_form_urlencoded_escaping() {
    let html = "<form action='/submit'><input name='q' value=\"it's (a) ~test! *ok* \u{e9}\"></form>";
    let (url, _, _) = submitted(html, "document.forms[0].submit()");
    assert_eq!(url, "http://127.0.0.1:8765/submit?q=it%27s+%28a%29+%7Etest%21+*ok*+%C3%A9");
}

/// Two groups named `r` (one per form) and a radio outside any form.
const RADIO_FORMS: &str = "<form action='/submit'>\
    <input type=radio name=r value=r1 checked><input type=radio name=r value=r2>\
    <input type=radio name=other value=o1 checked></form>\
    <form id=second><input type=radio name=r value=s1 checked></form>\
    <input type=radio name=r value=loose checked>";

#[test]
fn checking_a_radio_by_script_unchecks_the_rest_of_its_group() {
    let script = "document.querySelector('[value=r2]').checked = true; document.forms[0].submit()";
    let (url, _, _) = submitted(RADIO_FORMS, script);
    assert_eq!(url, "http://127.0.0.1:8765/submit?r=r2&other=o1", "one radio per group is submitted");
    let mut rt = runtime(RADIO_FORMS);
    let checked = rt
        .evaluate(
            "document.querySelector('[value=r2]').checked = true; \
             [...document.querySelectorAll('input')].map(r => r.value + '=' + r.checked).join(' ')",
        )
        .unwrap();
    assert_eq!(
        checked,
        json!("r1=false r2=true o1=true s1=true loose=true"),
        "another form's group and a radio outside the form keep their state"
    );
}

#[test]
fn unchecking_a_radio_by_script_leaves_its_group_alone() {
    let mut rt = runtime(RADIO_FORMS);
    let checked = rt
        .evaluate(
            "document.querySelector('[value=r2]').checked = false; \
             [document.querySelector('[value=r1]').checked, document.querySelector('[value=r2]').checked]",
        )
        .unwrap();
    assert_eq!(checked, json!([true, false]));
}
