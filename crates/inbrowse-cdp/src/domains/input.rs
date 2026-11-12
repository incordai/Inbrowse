use serde_json::{json, Value};

use crate::dispatch::CdpContext;
use crate::domains::frames;

/// A page whose `evaluate` runs in one of its frames: the one an input event
/// is aimed at. Everything else is the page's own.
struct InFrame<'a> {
    page: &'a mut inbrowse::Page,
    frame: u32,
}

impl InFrame<'_> {
    fn evaluate(&mut self, code: &str) -> Result<Value, String> {
        self.page.evaluate_in_frame_realm(self.frame, code)
    }
}

impl std::ops::Deref for InFrame<'_> {
    type Target = inbrowse::Page;
    fn deref(&self) -> &inbrowse::Page {
        self.page
    }
}

impl std::ops::DerefMut for InFrame<'_> {
    fn deref_mut(&mut self) -> &mut inbrowse::Page {
        self.page
    }
}

/// Embed a string as a JS string literal (double-quoted, with backslash,
/// quotes, and control characters escaped) for interpolation into generated
/// KeyboardEvent scripts. A plain `replace('\'', ...)` misses newline / NUL /
/// U+2028-29, which terminate the literal and silently drop the event.
fn js_str(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".to_string())
}

// Insert `text` at the caret, replacing any non-collapsed selection the way a
// real browser does when you type over selected text (for example after
// Ctrl+A or a triple-click). The selection comes from the page's
// `__inbrowse_selectionRange`, which covers every text-entry control, whatever
// its input type; a control whose selection was never moved has its caret at
// the end of the value, so plain typing appends.
//
// The text is embedded as a JSON string literal rather than escaped by hand
// into single quotes. JSON string syntax is a subset of JavaScript's, so this
// covers the quote and the backslash of issue #433 and the control characters
// they left out: a newline inside a single-quoted literal is a syntax error,
// so the whole snippet was dropped and nothing was inserted. inbrowse-mcp
// already builds its typing snippet this way.
fn insert_text_js(text: &str, ime_commit: bool) -> String {
    let literal = serde_json::to_string(text).unwrap_or_else(|_| "\"\"".to_string());
    format!(
        "(function() {{\
            var t = document.activeElement;\
            if (!t || (t.localName !== 'input' && t.localName !== 'textarea')) return;\
            if (globalThis.__inbrowse_isDisabled(t)) return;\
            if (!{ime_commit} && t.hasAttribute('readonly')) return;\
            var ins = {text};\
            var committed = globalThis.__inbrowse_selectionRange(t);\
            if (!committed) return;\
            var commitStart = {ime_commit} ? committed[0] : null;\
            var before = globalThis.__inbrowse_markTrusted(new InputEvent('beforeinput', {{bubbles:true,composed:true,cancelable:true,data:ins,inputType:'insertText'}}));\
            if (!t.dispatchEvent(before)) return;\
            if (t.hasAttribute('readonly') || globalThis.__inbrowse_isDisabled(t)) return;\
            var v = t.value || '';\
            var range = globalThis.__inbrowse_selectionRange(t) || [v.length, v.length];\
            var lo = range[0], hi = range[1];\
            globalThis.__inbrowse_setFieldValue(t, 'value', v.slice(0, lo) + ins + v.slice(hi));\
            var caret = Math.max(0, Math.min((commitStart == null ? lo : commitStart) + ins.length, t.value.length));\
            globalThis.__inbrowse_setSelection(t, caret, caret);\
            t.dispatchEvent(globalThis.__inbrowse_markTrusted(new InputEvent('input', {{bubbles:true,composed:true,data:ins,inputType:'insertText'}})));\
        }})()",
        text = literal,
    )
}

// Backspace deletes the selected range when there is one, so the common
// "select all, then Backspace to clear" pattern works. With a collapsed caret
// it removes the character before the caret; a control whose caret was never
// moved has it at the end of the value.
const BACKSPACE_JS: &str = "(function() {\
    var t = document.activeElement;\
    if (!t || (t.localName !== 'input' && t.localName !== 'textarea')) return;\
    if (t.hasAttribute('readonly')) return;\
    var range = globalThis.__inbrowse_selectionRange(t);\
    if (!range) return;\
    var v = t.value || '';\
    var s = range[0], e = range[1];\
    function backCount(str, p) {\
        if (p >= 2) {\
            var low = str.charCodeAt(p - 1), high = str.charCodeAt(p - 2);\
            if (low >= 0xDC00 && low <= 0xDFFF && high >= 0xD800 && high <= 0xDBFF) return 2;\
        }\
        return 1;\
    }\
    if (s !== e) {\
        globalThis.__inbrowse_setFieldValue(t, 'value', v.slice(0, s) + v.slice(e));\
        globalThis.__inbrowse_setSelection(t, s, s);\
    } else if (s > 0) {\
        var d = backCount(v, s);\
        globalThis.__inbrowse_setFieldValue(t, 'value', v.slice(0, s - d) + v.slice(s));\
        globalThis.__inbrowse_setSelection(t, s - d, s - d);\
    }\
    t.dispatchEvent(globalThis.__inbrowse_markTrusted(new Event('input', {bubbles:true})));\
})()";

// Delete removes the selected range, or the character after the caret.
const DELETE_JS: &str = "(function() {\
    var t = document.activeElement;\
    if (!t || (t.localName !== 'input' && t.localName !== 'textarea')) return;\
    if (t.hasAttribute('readonly')) return;\
    var range = globalThis.__inbrowse_selectionRange(t);\
    if (!range) return;\
    var v = t.value || '';\
    var lo = range[0], hi = range[1];\
    if (lo === hi) hi = Math.min(v.length, hi + 1);\
    if (lo === hi) return;\
    globalThis.__inbrowse_setFieldValue(t, 'value', v.slice(0, lo) + v.slice(hi));\
    globalThis.__inbrowse_setSelection(t, lo, lo);\
    t.dispatchEvent(globalThis.__inbrowse_markTrusted(new Event('input', {bubbles:true})));\
})()";

fn mouse_button_code(button: &str) -> u8 {
    match button {
        "middle" => 1,
        "right" => 2,
        "back" => 3,
        "forward" => 4,
        _ => 0,
    }
}

fn mouse_button_mask(button: &str) -> u64 {
    match button {
        "right" => 2,
        "middle" => 4,
        "back" => 8,
        "forward" => 16,
        "none" => 0,
        _ => 1,
    }
}

fn modifier_flags(modifiers: u64) -> (bool, bool, bool, bool) {
    // CDP Input.Modifier: Alt=1, Ctrl=2, Meta=4, Shift=8.
    (
        modifiers & 1 != 0,
        modifiers & 2 != 0,
        modifiers & 4 != 0,
        modifiers & 8 != 0,
    )
}

pub async fn handle(
    method: &str,
    params: &Value,
    ctx: &mut CdpContext,
    session_id: &Option<String>,
) -> Result<Value, String> {
    match method {
        "dispatchMouseEvent" => {
            let event_type = params.get("type").and_then(|v| v.as_str()).unwrap_or("");
            let x = params.get("x").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let y = params.get("y").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let button = params
                .get("button")
                .and_then(|v| v.as_str())
                .unwrap_or("left");
            let button_code = mouse_button_code(button);
            let buttons = params
                .get("buttons")
                .and_then(|v| v.as_u64())
                .unwrap_or_else(|| mouse_button_mask(button));
            let click_count = params
                .get("clickCount")
                .and_then(|v| v.as_u64())
                .unwrap_or(1);
            let modifiers = params
                .get("modifiers")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let (alt_key, ctrl_key, meta_key, shift_key) = modifier_flags(modifiers);
            // Set when a touch tap drives this press/release: the touch path
            // already fired the touch-type pointer events.
            let from_touch = params
                .get("__inbrowseFromTouch")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);

            if event_type == "mouseMoved" {
                if let Some(page) = ctx.get_session_page_mut(session_id) {
                    let point = frames::frame_at_point(page, x, y)?;
                    let (x, y) = (point.x, point.y);
                    let mut page = InFrame {
                        page,
                        frame: point.frame,
                    };
                    let code = format!(
                        "(function() {{\
                            var target = (document.elementFromPoint && document.elementFromPoint({x},{y})) || document.body;\
                            if (!target) return;\
                            var old = globalThis.__inbrowse_hover_target || null;\
                            globalThis.__inbrowse_hover_target = target;\
                            globalThis.__inbrowse_setHovered(target);\
                            var init = {{bubbles:true,cancelable:true,view:globalThis,clientX:{x},clientY:{y},button:{button_code},buttons:{buttons},detail:0,altKey:{alt_key},ctrlKey:{ctrl_key},metaKey:{meta_key},shiftKey:{shift_key}}};\
                            if (old !== target) {{\
                                if (old) old.dispatchEvent(globalThis.__inbrowse_markTrusted(new MouseEvent('mouseout', Object.assign({{}}, init, {{relatedTarget:target}}))));\
                                target.dispatchEvent(globalThis.__inbrowse_markTrusted(new MouseEvent('mouseover', Object.assign({{}}, init, {{relatedTarget:old}}))));\
                                target.dispatchEvent(globalThis.__inbrowse_markTrusted(new MouseEvent('mouseenter', Object.assign({{}}, init, {{bubbles:false,relatedTarget:old}}))));\
                            }}\
                            target.dispatchEvent(globalThis.__inbrowse_markTrusted(new MouseEvent('mousemove', init)));\
                        }})()",
                        x = x,
                        y = y,
                        button_code = button_code,
                        buttons = buttons,
                        alt_key = alt_key,
                        ctrl_key = ctrl_key,
                        meta_key = meta_key,
                        shift_key = shift_key,
                    );
                    page.evaluate(&code)?;
                }
            } else if event_type == "mousePressed" {
                if let Some(page) = ctx.get_session_page_mut(session_id) {
                    let point = frames::frame_at_point(page, x, y)?;
                    let (x, y) = (point.x, point.y);
                    let mut page = InFrame {
                        page,
                        frame: point.frame,
                    };
                    let code = format!(
                        "(function() {{\
                            var target = (document.elementFromPoint && document.elementFromPoint({x},{y})) || globalThis.__inbrowse_click_target || document.activeElement || document.body;\
                            if (!target) return;\
                            globalThis.__inbrowse_click_target = target;\
                            globalThis.__inbrowse_mouse_down = {{target:target,button:{button_code},clickCount:{click_count}}};\
                            var previousTarget = globalThis.__inbrowse_mouse_over_target || null;\
                            if (previousTarget !== target) {{\
                                function ancestry(node) {{ var path=[]; while (node) {{ path.push(node); node=node.parentNode || null; }} return path; }}\
                                function pointerEvent(node,type,bubbles,related) {{ if ({from_touch}) return; node.dispatchEvent(globalThis.__inbrowse_markTrusted(new PointerEvent(type, {{bubbles:bubbles,cancelable:bubbles,composed:bubbles,view:globalThis,clientX:{x},clientY:{y},button:{button_code},buttons:{buttons},detail:0,pointerId:1,pointerType:'mouse',isPrimary:true,pressure:{pointer_pressure},relatedTarget:related,altKey:{alt_key},ctrlKey:{ctrl_key},metaKey:{meta_key},shiftKey:{shift_key}}}))); }}\
                                function mouseEvent(node,type,bubbles,related) {{ node.dispatchEvent(globalThis.__inbrowse_markTrusted(new MouseEvent(type, {{bubbles:bubbles,cancelable:bubbles,composed:bubbles,view:globalThis,clientX:{x},clientY:{y},button:{button_code},buttons:{buttons},detail:0,relatedTarget:related,altKey:{alt_key},ctrlKey:{ctrl_key},metaKey:{meta_key},shiftKey:{shift_key}}}))); }}\
                                var oldPath = previousTarget && previousTarget.isConnected ? ancestry(previousTarget) : [];\
                                var newPath = ancestry(target);\
                                var common = newPath.find(function(node) {{ return oldPath.includes(node); }}) || null;\
                                var exited = common ? oldPath.slice(0, oldPath.indexOf(common)) : oldPath;\
                                var entered = common ? newPath.slice(0, newPath.indexOf(common)) : newPath;\
                                if (previousTarget && previousTarget.isConnected) pointerEvent(previousTarget,'pointerout',true,target);\
                                for (var pi=0; pi<exited.length; pi++) pointerEvent(exited[pi],'pointerleave',false,target);\
                                pointerEvent(target,'pointerover',true,previousTarget);\
                                for (var pe=entered.length-1; pe>=0; pe--) pointerEvent(entered[pe],'pointerenter',false,previousTarget);\
                                if (previousTarget && previousTarget.isConnected) mouseEvent(previousTarget,'mouseout',true,target);\
                                for (var mi=0; mi<exited.length; mi++) mouseEvent(exited[mi],'mouseleave',false,target);\
                                mouseEvent(target,'mouseover',true,previousTarget);\
                                for (var me=entered.length-1; me>=0; me--) mouseEvent(entered[me],'mouseenter',false,previousTarget);\
                                globalThis.__inbrowse_mouse_over_target = target;\
                            }}\
                            var focusTarget = target.closest && target.closest('input,select,textarea,button,a[href],[tabindex],[contenteditable]');\
                            var pointer = globalThis.__inbrowse_markTrusted(new PointerEvent('pointerdown', {{bubbles:true,cancelable:true,composed:true,view:globalThis,clientX:{x},clientY:{y},button:{button_code},buttons:{buttons},detail:0,pointerId:1,pointerType:'mouse',isPrimary:true,pressure:{pointer_pressure},altKey:{alt_key},ctrlKey:{ctrl_key},metaKey:{meta_key},shiftKey:{shift_key}}}));\
                            if (!{from_touch}) target.dispatchEvent(pointer);\
                            var evt = globalThis.__inbrowse_markTrusted(new MouseEvent('mousedown', {{bubbles:true,cancelable:true,composed:true,view:globalThis,clientX:{x},clientY:{y},button:{button_code},buttons:{buttons},detail:{click_count},altKey:{alt_key},ctrlKey:{ctrl_key},metaKey:{meta_key},shiftKey:{shift_key}}}));\
                            target.dispatchEvent(evt);\
                            if (!evt.defaultPrevented) {{\
                                var focusTarget = globalThis.__inbrowse_interactiveHost(target) || (target.closest && target.closest('[tabindex]'));\
                                if (focusTarget && !globalThis.__inbrowse_isDisabled(focusTarget)) focusTarget.focus();\
                                else if (document.activeElement && document.activeElement !== document.body) document.activeElement.blur();\
                            }}\
                        }})()",
                        x = x,
                        y = y,
                        button_code = button_code,
                        buttons = buttons,
                        click_count = click_count,
                        alt_key = alt_key,
                        ctrl_key = ctrl_key,
                        meta_key = meta_key,
                        shift_key = shift_key,
                        pointer_pressure = if buttons == 0 { 0.0 } else { 0.5 },
                        from_touch = from_touch,
                    );
                    page.evaluate(&code)?;
                }
            } else if event_type == "mouseReleased" {
                let moved_frame = if let Some(page) = ctx.get_session_page_mut(session_id) {
                    let point = frames::frame_at_point(page, x, y)?;
                    let (x, y) = (point.x, point.y);
                    let mut page = InFrame {
                        page,
                        frame: point.frame,
                    };
                    let code = format!(
                        "(function() {{\
                            var target = (document.elementFromPoint && document.elementFromPoint({x},{y})) || globalThis.__inbrowse_click_target || document.activeElement || document.body;\
                            if (!target) return;\
                            var down = globalThis.__inbrowse_mouse_down;\
                            globalThis.__inbrowse_mouse_down = null;\
                            var pointer = globalThis.__inbrowse_markTrusted(new PointerEvent('pointerup', {{bubbles:true,cancelable:true,composed:true,view:globalThis,clientX:{x},clientY:{y},button:{button_code},buttons:0,detail:0,pointerId:1,pointerType:'mouse',isPrimary:true,pressure:0,altKey:{alt_key},ctrlKey:{ctrl_key},metaKey:{meta_key},shiftKey:{shift_key}}}));\
                            if (!{from_touch}) target.dispatchEvent(pointer);\
                            var evt = globalThis.__inbrowse_markTrusted(new MouseEvent('mouseup', {{bubbles:true,cancelable:true,composed:true,view:globalThis,clientX:{x},clientY:{y},button:{button_code},buttons:0,detail:{click_count},altKey:{alt_key},ctrlKey:{ctrl_key},metaKey:{meta_key},shiftKey:{shift_key}}}));\
                            target.dispatchEvent(evt);\
                            if (!down || down.button !== {button_code} || {button_code} !== 0) return;\
                            var clickTarget = down.target;\
                            while (clickTarget && clickTarget !== target && !(clickTarget.contains && clickTarget.contains(target))) {{\
                                clickTarget = clickTarget.parentElement;\
                            }}\
                            if (!clickTarget) return;\
                            var tag = clickTarget.tagName;\
                            var type = (clickTarget.getAttribute && clickTarget.getAttribute('type') || '').toLowerCase();\
                            if (globalThis.__inbrowse_isDisabled(clickTarget)) return;\
                            var checkable = tag === 'INPUT' && (type === 'checkbox' || type === 'radio');\
                            var oldChecked = checkable ? globalThis.__inbrowse_inputChecked(clickTarget) : false;\
                            var oldIndeterminate = checkable ? globalThis.__inbrowse_inputIndeterminate(clickTarget) : false;\
                            var radioStates = null;\
                            if (checkable && type === 'radio') {{\
                                var radioName = clickTarget.getAttribute('name') || '';\
                                if (radioName) {{\
                                    var candidates = document.querySelectorAll('input');\
                                    radioStates = [];\
                                    for (var ri = 0; ri < candidates.length; ri++) {{\
                                        var radio = candidates[ri];\
                                        if ((radio.getAttribute('type') || '').toLowerCase() !== 'radio' || (radio.getAttribute('name') || '') !== radioName || radio.form !== clickTarget.form) continue;\
                                        radioStates.push([radio, globalThis.__inbrowse_inputChecked(radio)]);\
                                        if (radio !== clickTarget) globalThis.__inbrowse_setInputChecked(radio, false);\
                                    }}\
                                }}\
                                globalThis.__inbrowse_setInputChecked(clickTarget, true);\
                            }} else if (checkable) {{\
                                globalThis.__inbrowse_setInputChecked(clickTarget, !oldChecked);\
                                globalThis.__inbrowse_setInputIndeterminate(clickTarget, false);\
                            }}\
                            var click = globalThis.__inbrowse_markTrusted(new MouseEvent('click', {{bubbles:true,cancelable:true,composed:true,view:globalThis,clientX:{x},clientY:{y},button:0,buttons:0,detail:{click_count},altKey:{alt_key},ctrlKey:{ctrl_key},metaKey:{meta_key},shiftKey:{shift_key}}}));\
                            var cancelled = !clickTarget.dispatchEvent(click);\
                            if (cancelled) {{\
                                if (radioStates) {{\
                                    for (var rr = 0; rr < radioStates.length; rr++) globalThis.__inbrowse_setInputChecked(radioStates[rr][0], radioStates[rr][1]);\
                                }} else if (checkable) {{ globalThis.__inbrowse_setInputChecked(clickTarget, oldChecked); globalThis.__inbrowse_setInputIndeterminate(clickTarget, oldIndeterminate); }}\
                                return;\
                            }}\
                            if ({click_count} === 2) {{\
                                var doubleClick = globalThis.__inbrowse_markTrusted(new MouseEvent('dblclick', {{bubbles:true,cancelable:true,view:globalThis,clientX:{x},clientY:{y},button:0,buttons:0,detail:2,altKey:{alt_key},ctrlKey:{ctrl_key},metaKey:{meta_key},shiftKey:{shift_key}}}));\
                                clickTarget.dispatchEvent(doubleClick);\
                            }}\
                            if (globalThis.__inbrowse_selectionRange(clickTarget) !== null) {{\
                                if ({click_count} >= 3) globalThis.__inbrowse_selectAll(clickTarget);\
                                else if ({click_count} === 2) globalThis.__inbrowse_selectWord(clickTarget);\
                                else {{ var caret = String(clickTarget.value).length; globalThis.__inbrowse_setSelection(clickTarget, caret, caret); }}\
                                return;\
                            }}\
                            if (checkable && globalThis.__inbrowse_inputChecked(clickTarget) !== oldChecked) {{\
                                try {{ clickTarget.dispatchEvent(globalThis.__inbrowse_markTrusted(new Event('input', {{bubbles:true}}))); }} catch(e) {{}}\
                                try {{ clickTarget.dispatchEvent(globalThis.__inbrowse_markTrusted(new Event('change', {{bubbles:true}}))); }} catch(e) {{}}\
                                return;\
                            }}\
                            var labelHost = tag === 'LABEL' ? clickTarget : (clickTarget.closest ? clickTarget.closest('label') : null);\
                            var interactiveHost = globalThis.__inbrowse_interactiveHost(clickTarget);\
                            if (labelHost && !(interactiveHost && labelHost.contains(interactiveHost))) {{\
                                var ctl = globalThis.__inbrowse_labeledControl(labelHost);\
                                if (ctl && ctl !== clickTarget && globalThis.__inbrowse_activateLabel(labelHost, ctl, true)) {{ return; }}\
                            }}\
                            if (globalThis.__inbrowse_activateSummary(clickTarget)) return;\
                            var link = clickTarget.closest ? clickTarget.closest('a[href]') : null;\
                            if (!link && tag === 'A' && clickTarget.getAttribute('href')) link = clickTarget;\
                            var submitButton = tag === 'BUTTON' ? clickTarget : (clickTarget.closest ? clickTarget.closest('button') : null);\
                            var submitButtonType = submitButton ? ((submitButton.getAttribute('type') || '').toLowerCase()) : '';\
                            if (link) {{\
                                var href = link.getAttribute('href');\
                                if (href && !href.startsWith('javascript:')) {{\
                                    if (href.startsWith('#')) history.pushState(null, '', href);\
                                    else location.assign(href);\
                                }}\
                            }} else if (submitButton && submitButtonType !== 'button' && submitButtonType !== 'reset' && !globalThis.__inbrowse_isDisabled(submitButton)) {{\
                                var form = submitButton.closest ? submitButton.closest('form') : null;\
                                if (form) {{ try {{ if (typeof form.requestSubmit === 'function') {{ form.requestSubmit(submitButton); }} else {{ form.submit(submitButton); }} }} catch(e) {{}} }}\
                            }} else if (tag === 'INPUT' && (type === 'submit' || type === 'image')) {{\
                                var form2 = clickTarget.closest ? clickTarget.closest('form') : null;\
                                if (form2) {{ try {{ if (typeof form2.requestSubmit === 'function') {{ form2.requestSubmit(clickTarget); }} else {{ form2.submit(clickTarget); }} }} catch(e) {{}} }}\
                            }}\
                        }})()",
                        x = x,
                        y = y,
                        button_code = button_code,
                        click_count = click_count,
                        alt_key = alt_key,
                        ctrl_key = ctrl_key,
                        meta_key = meta_key,
                        shift_key = shift_key,
                        from_touch = from_touch,
                    );
                    page.evaluate(&code)?;
                    // Document navigation runs after the input acknowledgement.
                    // Same-document routing still needs a frame notification.
                    if !page.has_pending_navigation() && page.sync_virtual_url() {
                        Some((page.frame_id.clone(), page.url_string()))
                    } else {
                        None
                    }
                } else {
                    None
                };
                if let Some((frame_id, url)) = moved_frame {
                    crate::domains::page::emit_same_document_navigation(
                        ctx, session_id, &frame_id, &url,
                    );
                }
            } else if event_type == "mouseWheel" {
                let delta_x = params.get("deltaX").and_then(|v| v.as_f64()).unwrap_or(0.0);
                let delta_y = params.get("deltaY").and_then(|v| v.as_f64()).unwrap_or(0.0);
                if let Some(page) = ctx.get_session_page_mut(session_id) {
                    let point = frames::frame_at_point(page, x, y)?;
                    let (x, y) = (point.x, point.y);
                    let mut page = InFrame {
                        page,
                        frame: point.frame,
                    };
                    let code = format!(
                        "(function() {{\
                            var target = (document.elementFromPoint && document.elementFromPoint({x},{y})) || document.body || document.documentElement;\
                            if (!target) return;\
                            var wheel = globalThis.__inbrowse_markTrusted(new WheelEvent('wheel', {{bubbles:true,cancelable:true,view:globalThis,clientX:{x},clientY:{y},deltaX:{delta_x},deltaY:{delta_y},deltaMode:0,altKey:{alt_key},ctrlKey:{ctrl_key},metaKey:{meta_key},shiftKey:{shift_key}}}));\
                            if (!target.dispatchEvent(wheel)) return;\
                            var dx = {delta_x}, dy = {delta_y};\
                            var root = document.scrollingElement || document.documentElement || document.body;\
                            var scrollTarget = null;\
                            var el = target;\
                            while (el && el.nodeType === 1 && el !== root && el !== document.body && el !== document.documentElement) {{\
                                var maxX = Math.max(0, (el.scrollWidth || 0) - (el.clientWidth || 0));\
                                var maxY = Math.max(0, (el.scrollHeight || 0) - (el.clientHeight || 0));\
                                var style = null;\
                                try {{ style = getComputedStyle(el); }} catch (_e) {{}}\
                                var ox = style ? (style.overflowX || style.overflow || '') : '';\
                                var oy = style ? (style.overflowY || style.overflow || '') : '';\
                                var allowX = ox === 'auto' || ox === 'scroll' || ox === 'overlay';\
                                var allowY = oy === 'auto' || oy === 'scroll' || oy === 'overlay';\
                                var consumesX = allowX && ((dx > 0 && el.scrollLeft < maxX) || (dx < 0 && el.scrollLeft > 0));\
                                var consumesY = allowY && ((dy > 0 && el.scrollTop < maxY) || (dy < 0 && el.scrollTop > 0));\
                                if (consumesX || consumesY) {{ scrollTarget = el; break; }}\
                                el = el.parentElement;\
                            }}\
                            if (!scrollTarget) scrollTarget = root;\
                            if (scrollTarget === root && root && typeof root.scrollBy === 'function') {{\
                                var beforeX = root.scrollLeft, beforeY = root.scrollTop;\
                                root.scrollBy(dx, dy);\
                                if (root.scrollLeft !== beforeX || root.scrollTop !== beforeY) setTimeout(function() {{\
                                    try {{ document.dispatchEvent(new Event('scroll', {{bubbles:false}})); }} catch (_e) {{}}\
                                    try {{ globalThis.dispatchEvent(new Event('scroll', {{bubbles:false}})); }} catch (_e) {{}}\
                                }}, 0);\
                            }} else if (scrollTarget && typeof scrollTarget.scrollBy === 'function') scrollTarget.scrollBy(dx, dy);\
                        }})()",
                        x = x,
                        y = y,
                        delta_x = delta_x,
                        delta_y = delta_y,
                        alt_key = alt_key,
                        ctrl_key = ctrl_key,
                        meta_key = meta_key,
                        shift_key = shift_key,
                    );
                    page.evaluate(&code)?;
                }
            }

            Ok(json!({}))
        }
        // Chrome's Input.insertText: Playwright's fill() focuses the field in
        // page and then types the whole value through this one call (#577).
        "insertText" => {
            let text = params.get("text").and_then(|v| v.as_str()).unwrap_or("");
            if let Some(page) = ctx.get_session_page_mut(session_id) {
                let frame = page.focused_frame();
                let mut page = InFrame { page, frame };
                // IME commits snapshot the final caret before beforeinput.
                // Keyboard insertion instead follows the handler's selection.
                page.evaluate(&insert_text_js(text, true))?;
            }
            Ok(json!({}))
        }
        "dispatchKeyEvent" => {
            let event_type = params.get("type").and_then(|v| v.as_str()).unwrap_or("");
            let modifiers = params
                .get("modifiers")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let (alt_key, ctrl_key, meta_key, shift_key) = modifier_flags(modifiers);
            let key = params.get("key").and_then(|v| v.as_str()).unwrap_or("");
            let code = params.get("code").and_then(|v| v.as_str()).unwrap_or("");
            let text = params.get("text").and_then(|v| v.as_str()).unwrap_or("");
            let virtual_key_code = params
                .get("windowsVirtualKeyCode")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);

            if let Some(page) = ctx.get_session_page_mut(session_id) {
                let frame = page.focused_frame();
                let mut page = InFrame { page, frame };
                match event_type {
                    "keyDown" | "rawKeyDown" => {
                        let js = format!(
                            "(function() {{\
                                var target = document.activeElement || document.body;\
                                var evt = globalThis.__inbrowse_markTrusted(new KeyboardEvent('keydown', {{bubbles:true,cancelable:true,key:{key},code:{code},keyCode:{virtual_key_code},which:{virtual_key_code},altKey:{alt_key},ctrlKey:{ctrl_key},metaKey:{meta_key},shiftKey:{shift_key}}}));\
                                return target.dispatchEvent(evt);\
                            }})()",
                            // Escape backslash BEFORE single-quote (as the text
                            // path below does) so a key like "\" — Chrome's
                            // backslash key — doesn't escape the closing quote
                            // and produce a syntax error that drops the event.
                            key = js_str(key),
                            code = js_str(code),
                            virtual_key_code = virtual_key_code,
                        );
                        if page.evaluate(&js)?.as_bool() == Some(false) {
                            return Ok(json!({}));
                        }

                        // Select all: the CDP `commands` list names it on macOS,
                        // and Ctrl/Cmd+A is the shortcut everywhere else. A
                        // focused text control selects its value, anything else
                        // selects the document.
                        let selects_all = params
                            .get("commands")
                            .and_then(|v| v.as_array())
                            .is_some_and(|commands| {
                                commands.iter().any(|c| c.as_str() == Some("selectAll"))
                            })
                            || ((ctrl_key || meta_key)
                                && !alt_key
                                && !shift_key
                                && (key.eq_ignore_ascii_case("a") || code == "KeyA"));
                        if selects_all {
                            page.evaluate(
                                "globalThis.__inbrowse_selectAll(document.activeElement)",
                            )?;
                        }

                        // A Ctrl/Cmd shortcut types nothing, even if the client
                        // sent the key's text alongside it.
                        let shortcut = (ctrl_key || meta_key) && !alt_key;
                        if !text.is_empty() && text != "\r" && text != "\n" && !shortcut {
                            page.evaluate(&insert_text_js(text, false))?;
                        }

                        if key == "Enter" {
                            // In a textarea Enter inserts a newline; in input fields
                            // it submits the containing form. Real Chrome distinguishes
                            // these two and we should too: previously every Enter tried
                            // to submit the nearest form even from a textarea.
                            let js = "(function() {\
                                var target = document.activeElement;\
                                if (!target) return;\
                                target.dispatchEvent(globalThis.__inbrowse_markTrusted(new KeyboardEvent('keypress', {bubbles:true,key:'Enter',code:'Enter',keyCode:$VK,which:$VK,charCode:$VK,$MODIFIERS})));\
                                if (target.localName === 'textarea') {\
                                    if (target.hasAttribute('readonly')) return;\
                                    var value = target.value || '';\
                                    var range = globalThis.__inbrowse_selectionRange(target);\
                                    var lower = range[0], upper = range[1];\
                                    globalThis.__inbrowse_setFieldValue(target, 'value', value.slice(0, lower) + '\\n' + value.slice(upper));\
                                    globalThis.__inbrowse_setSelection(target, lower + 1, lower + 1);\
                                    target.dispatchEvent(globalThis.__inbrowse_markTrusted(new Event('input', {bubbles:true})));\
                                } else {\
                                    var form = target.form || (target.closest && target.closest('form'));\
                                    if (form) {{ try {{ if (typeof form.requestSubmit === 'function') {{ form.requestSubmit(); }} else {{ form.submit(); }} }} catch(e) {{}} }}\
                                }\
                            })()".replace("$VK", &virtual_key_code.to_string())
                                .replace("$MODIFIERS", &format!("altKey:{alt_key},ctrlKey:{ctrl_key},metaKey:{meta_key},shiftKey:{shift_key}"));
                            page.evaluate(&js)?;
                        }

                        if key == "Backspace" {
                            page.evaluate(BACKSPACE_JS)?;
                        }

                        if key == "Delete" {
                            page.evaluate(DELETE_JS)?;
                        }
                    }
                    "keyUp" => {
                        let js = format!(
                            "(function() {{\
                                var target = document.activeElement || document.body;\
                                var evt = globalThis.__inbrowse_markTrusted(new KeyboardEvent('keyup', {{bubbles:true,key:{key},code:{code},keyCode:{virtual_key_code},which:{virtual_key_code},altKey:{alt_key},ctrlKey:{ctrl_key},metaKey:{meta_key},shiftKey:{shift_key}}}));\
                                target.dispatchEvent(evt);\
                            }})()",
                            key = js_str(key),
                            code = js_str(code),
                            virtual_key_code = virtual_key_code,
                        );
                        page.evaluate(&js)?;
                    }
                    "char" => {
                        if !text.is_empty() {
                            page.evaluate(&insert_text_js(text, false))?;
                            // Pump event loop so Angular change detection picks up the input
                            page.settle(50).await;
                        }
                    }
                    _ => {}
                }
            }

            Ok(json!({}))
        }
        "dispatchTouchEvent" => {
            let event_type = params.get("type").and_then(|v| v.as_str()).unwrap_or("");
            let points = params
                .get("touchPoints")
                .cloned()
                .unwrap_or_else(|| json!([]));
            let modifiers = params
                .get("modifiers")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let taps = match ctx.get_session_page_mut(session_id) {
                Some(page) => page.evaluate(&format!(
                    "JSON.stringify(globalThis.__inbrowse_dispatchTouch({}, {}, {}))",
                    json!(event_type),
                    points,
                    modifiers
                ))?,
                None => return Ok(json!({})),
            };
            let taps: Vec<Value> = taps
                .as_str()
                .and_then(|s| serde_json::from_str(s).ok())
                .unwrap_or_default();
            // An uncancelled tap yields the compatibility mouse sequence, so
            // click and activation behavior (links, submit, checkboxes) run
            // through the same path as a real mouse click.
            for tap in taps {
                let (x, y) = (tap.get("x").cloned(), tap.get("y").cloned());
                for (phase, buttons) in [("mousePressed", 1), ("mouseReleased", 0)] {
                    let mouse = json!({
                        "type": phase, "x": x, "y": y, "button": "left",
                        "buttons": buttons, "clickCount": 1, "modifiers": modifiers,
                        "__inbrowseFromTouch": true,
                    });
                    Box::pin(handle("dispatchMouseEvent", &mouse, ctx, session_id)).await?;
                }
            }
            Ok(json!({}))
        }
        "dispatchDragEvent" => {
            let event_type = params.get("type").and_then(|v| v.as_str()).unwrap_or("");
            let x = params.get("x").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let y = params.get("y").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let data = params.get("data").cloned().unwrap_or_else(|| json!({}));
            let modifiers = params
                .get("modifiers")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            if let Some(page) = ctx.get_session_page_mut(session_id) {
                page.evaluate(&format!(
                    "globalThis.__inbrowse_dispatchDrag({}, {}, {}, {}, {})",
                    json!(event_type),
                    x,
                    y,
                    data,
                    modifiers
                ))?;
            }
            Ok(json!({}))
        }
        "setInterceptDrags" => Ok(json!({})),
        "setIgnoreInputEvents" => Ok(json!({})),
        _ => Err(format!("Unknown Input method: {}", method)),
    }
}

#[cfg(test)]
mod tests {
    use super::js_str;

    // SEC-501 / #819 — key/code are embedded via js_str; it must escape control
    // characters (newline/CR/tab/NUL/U+2028-29), not just backslash and quote,
    // so a control char cannot terminate the literal and drop the event.
    #[test]
    fn js_str_escapes_control_characters() {
        let lit = js_str("a\nb\r\t'c\\d\"e");
        assert!(
            !lit.contains('\n') && !lit.contains('\r') && !lit.contains('\t'),
            "control characters must be escaped, not left raw: {lit:?}"
        );
        // The result must be a valid JS/JSON string literal that round-trips.
        let decoded: String = serde_json::from_str(&lit).expect("the literal must be valid JSON");
        assert_eq!(decoded, "a\nb\r\t'c\\d\"e");
    }
}
