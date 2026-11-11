use std::collections::HashMap;

use inbrowse::Page;
use inbrowse_dom::{DomTree, NodeData, NodeId};
use serde_json::{json, Value};

use crate::dispatch::CdpContext;
use crate::domains::frames::{self, node_id, split_node_id};
use crate::domains::page::child_frame_id;
use crate::types::CdpEvent;

pub(crate) struct DomDocument {
    pub page_id: String,
    /// Keyed by CDP node id, which names the frame as well as the node.
    children_sent: HashMap<u64, bool>,
}

impl DomDocument {
    fn remember(&mut self, root: &Value) {
        let mut pending = vec![root];
        while let Some(node) = pending.pop() {
            if let Some(id) = node["nodeId"].as_u64() {
                self.children_sent.insert(id,
                    node["children"].is_array() || node["childNodeCount"] == 0);
            }
            if let Some(children) = node["children"].as_array() {
                pending.extend(children);
            }
        }
    }
}

/// The child frame hosted by an iframe element, as a node reports it.
struct FrameLink {
    id: String,
    document: Value,
}

/// What serializing one frame's tree needs to know about the frames around it:
/// which frame it is, and which of its iframe elements host a child frame.
struct FrameScope {
    frame: u32,
    /// The CDP frame id of this frame; the page's own frame is not named here.
    own_id: Option<String>,
    /// Hosting iframe node index -> the frame it hosts.
    links: HashMap<u32, FrameLink>,
}

impl FrameScope {
    fn new(page: &Page, frame: u32) -> Self {
        Self {
            frame,
            own_id: (frame != 0).then(|| child_frame_id(&page.frame_id, frame)),
            links: HashMap::new(),
        }
    }

    /// The scope of `frame` in `page`, with the child frames it hosts and, for
    /// each, the content document an iframe element reports.
    fn of(page: &Page, frame: u32) -> Self {
        let mut scope = Self::new(page, frame);
        for realm in &page.frames {
            let child = realm.frame_id();
            let Some((parent, node)) = page.frame_owner(child) else { continue };
            if parent != frame {
                continue;
            }
            let child_scope = Self::new(page, child);
            let document = page.with_frame_dom(child, |dom| {
                node_value(dom, dom.document(), &child_scope).map(|(mut document, _)| {
                    document["documentURL"] = json!(realm.url());
                    document["baseURL"] = json!(realm.url());
                    document
                })
            }).flatten();
            if let (Some(document), Some(id)) = (document, child_scope.own_id) {
                scope.links.insert(node, FrameLink { id, document });
            }
        }
        scope
    }
}

fn push_children(dom: &DomTree, scope: &FrameScope, state: &mut DomDocument, node: NodeId,
    depth: u32, session_id: &Option<String>, events: &mut Vec<CdpEvent>) {
    let mut pending = vec![(node, depth.min(MAX_SERIALIZE_DEPTH))];
    while let Some((parent, depth)) = pending.pop() {
        let parent_id = node_id(scope.frame, parent.index() as u32);
        let Some(sent) = state.children_sent.get(&parent_id).copied() else { continue };
        let children = dom.children(parent);
        if !sent {
            let nodes: Vec<_> = children.iter()
                .map(|child| serialize_node(dom, *child, depth - 1, 0, scope)).collect();
            for node in &nodes { state.remember(node); }
            state.children_sent.insert(parent_id, true);
            events.push(CdpEvent {
                method: "DOM.setChildNodes".into(),
                params: json!({"parentId":parent_id, "nodes":nodes}),
                session_id: session_id.clone(),
            });
        } else if depth > 1 {
            pending.extend(children.into_iter().rev().map(|child| (child, depth - 1)));
        }
    }
}

fn push_node(ctx: &mut CdpContext, session_id: &Option<String>, frame: u32, node: NodeId)
    -> Result<u64, String>
{
    let Some(mut state) = ctx.dom_documents.remove(session_id) else { return Ok(0) };
    let mut events = Vec::new();
    let result = ctx.get_session_page(session_id).and_then(|page| {
        let scope = FrameScope::of(page, frame);
        page.with_frame_dom(frame, |dom| {
            if dom.get_node(node).is_none() { return Err("No node with given id found".into()); }
            let id = node_id(frame, node.index() as u32);
            if !state.children_sent.contains_key(&id) {
                let path = dom.ancestors(node);
                if path.last().copied() != Some(dom.document()) {
                    return Err("Node is outside the requested light-DOM document".into());
                }
                for parent in path.into_iter().rev() {
                    push_children(dom, &scope, &mut state, parent, 1, session_id, &mut events);
                }
                if !state.children_sent.contains_key(&id) {
                    // shortcut: refresh inserted children until live DOM mutation events publish them.
                    return Err("Node requires DOM mutation events; request the document again".into());
                }
            }
            Ok(id)
        })
    }).unwrap_or_else(|| Err("No DOM loaded".into()));
    ctx.dom_documents.insert(session_id.clone(), state);
    ctx.pending_events.extend(events);
    result
}

/// Resolve a DOM `nodeId` from CDP params. Honors `nodeId`, `backendNodeId`,
/// and `objectId` in that order. Playwright commonly passes only `objectId`
/// (returned by a prior `DOM.resolveNode`); without this fallback those
/// requests silently default to node 0 and click the wrong element.
///
/// The id names the frame too (see [`frames::node_id`]); a node of the page
/// itself is its plain index.
pub(crate) fn resolve_node_id(page: &mut Page, params: &Value) -> Result<u64, String> {
    if let Some(nid) = params.get("nodeId").and_then(|v| v.as_u64()) {
        return Ok(nid);
    }
    if let Some(nid) = params.get("backendNodeId").and_then(|v| v.as_u64()) {
        return Ok(nid);
    }
    if let Some(oid) = params.get("objectId").and_then(|v| v.as_str()) {
        let (frame, node) = page.node_ref(oid)?;
        return Ok(node_id(frame, node));
    }
    Err("nodeId, backendNodeId, or objectId required".to_string())
}

/// Run the realm's file-input helper on `node` of frame `frame`. With
/// `validate_only` it only reports whether the node is a file input; otherwise
/// it selects the files described by `specs_json`. The helper lives in the
/// frame's own realm so the check and the selection use that frame's elements.
fn run_file_input_helper(page: &mut Page, frame: u32, node: u32, specs_json: &str,
    validate_only: bool) -> Result<bool, String>
{
    let code = format!(
        "(function() {{ var el = globalThis._wrap({node}); \
         return globalThis.__inbrowse_setInputFiles(el, {specs_json}, {validate_only}) === true; }})()"
    );
    Ok(page.evaluate_in_frame_realm(frame, &code)?.as_bool() == Some(true))
}

/// [`resolve_node_id`] split into the frame and the node inside it.
fn resolve_node(page: &mut Page, params: &Value) -> Result<(u32, NodeId), String> {
    let (frame, node) = split_node_id(resolve_node_id(page, params)?);
    Ok((frame, NodeId::new(node)))
}

/// The frame a node id names, when the page has it.
fn live_frame(page: &Page, frame: u32) -> Result<u32, String> {
    if frame == 0 || page.frames.iter().any(|realm| realm.frame_id() == frame) {
        Ok(frame)
    } else {
        Err("No node with given id found".into())
    }
}

/// The boxes of element `node` of frame `frame` in the page's viewport
/// coordinates: four quads of eight numbers each (content, padding, border,
/// margin), then the border box's width and height. Measured in the frame's
/// realm and shifted by where the frame sits in the page.
fn element_boxes(page: &mut Page, frame: u32, node: NodeId) -> Result<Vec<f64>, String> {
    let code = format!(
        "(function() {{\
            var e = globalThis._wrap && globalThis._wrap({});\
            if (!e || typeof e.getBoundingClientRect !== 'function') return null;\
            if (typeof e.getClientRects === 'function' && e.getClientRects().length === 0) return null;\
            var r = e.getBoundingClientRect();\
            var s = getComputedStyle(e);\
            var f = function(name) {{ return parseFloat(s[name]) || 0; }};\
            var border = [r.left, r.top, r.right, r.bottom];\
            var padding = [r.left + f('borderLeftWidth'), r.top + f('borderTopWidth'),\
                           r.right - f('borderRightWidth'), r.bottom - f('borderBottomWidth')];\
            var content = [padding[0] + f('paddingLeft'), padding[1] + f('paddingTop'),\
                           padding[2] - f('paddingRight'), padding[3] - f('paddingBottom')];\
            var margin = [r.left - f('marginLeft'), r.top - f('marginTop'),\
                          r.right + f('marginRight'), r.bottom + f('marginBottom')];\
            var quad = function(b) {{ return [b[0], b[1], b[2], b[1], b[2], b[3], b[0], b[3]]; }};\
            return quad(content).concat(quad(padding), quad(border), quad(margin), [r.width, r.height]);\
        }})()",
        node.index()
    );
    let mut numbers: Vec<f64> = page
        .evaluate_in_frame_realm(frame, &code)?
        .as_array()
        .map(|values| values.iter().filter_map(Value::as_f64).collect())
        .unwrap_or_default();
    if numbers.len() != 34 {
        return Err("Could not compute box model.".into());
    }
    let (x, y) = frames::frame_origin(page, frame)?;
    for (index, value) in numbers[..32].iter_mut().enumerate() {
        *value += if index % 2 == 0 { x } else { y };
    }
    Ok(numbers)
}

/// Standard base64 (with padding). Used to ferry file bytes to the JS layer for
/// DOM.setFileInputFiles without pulling in a dependency.
fn encode_base64(input: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity((input.len() + 2) / 3 * 4);
    for chunk in input.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(T[((n >> 18) & 63) as usize] as char);
        out.push(T[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 { T[((n >> 6) & 63) as usize] as char } else { '=' });
        out.push(if chunk.len() > 2 { T[(n & 63) as usize] as char } else { '=' });
    }
    out
}

/// Best-effort MIME type from a file extension, for the File objects created by
/// DOM.setFileInputFiles. Defaults to application/octet-stream.
fn guess_mime(path: &str) -> &'static str {
    let ext = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "bmp" => "image/bmp",
        "ico" => "image/x-icon",
        "pdf" => "application/pdf",
        "txt" => "text/plain",
        "html" | "htm" => "text/html",
        "css" => "text/css",
        "js" | "mjs" => "text/javascript",
        "json" => "application/json",
        "xml" => "application/xml",
        "csv" => "text/csv",
        "zip" => "application/zip",
        "gz" => "application/gzip",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "doc" => "application/msword",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xls" => "application/vnd.ms-excel",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        _ => "application/octet-stream",
    }
}

pub async fn handle(
    method: &str,
    params: &Value,
    ctx: &mut CdpContext,
    session_id: &Option<String>,
) -> Result<Value, String> {
    match method {
        "enable" => Ok(json!({})),
        "disable" => {
            ctx.dom_documents.remove(session_id);
            Ok(json!({}))
        }
        "getDocument" => {
            let page = ctx.get_session_page(session_id).ok_or("No page")?;
            let depth = params.get("depth").and_then(|v| v.as_i64()).unwrap_or(2);
            let scope = FrameScope::of(page, 0);
            let root = page.with_dom(|dom| serialize_node(dom, dom.document(), depth as u32, 0, &scope))
                .ok_or("No DOM loaded")?;
            let mut state = DomDocument { page_id: page.id.clone(), children_sent: HashMap::new() };
            state.remember(&root);
            ctx.dom_documents.insert(session_id.clone(), state);
            Ok(json!({"root":root}))
        }
        "getAttributes" => {
            let (frame, node_id) = split_node_id(params["nodeId"].as_u64().ok_or("valid nodeId required")?);
            let node_id = NodeId::new(node_id);
            let page = ctx.get_session_page(session_id).ok_or("No page")?;
            let frame = live_frame(page, frame)?;
            let attributes = page.with_frame_dom(frame, |dom| {
                let node = dom.get_node(node_id).ok_or("No node with given id found")?;
                let attrs = node.attrs().ok_or("Node is not an Element")?;
                Ok::<_, &str>(attrs.iter().flat_map(|attr| [attr.qualified_name(), attr.value.clone()]).collect::<Vec<_>>())
            }).ok_or("No DOM loaded")?.map_err(str::to_string)?;
            Ok(json!({"attributes":attributes}))
        }
        "requestChildNodes" => {
            let (frame, node) = split_node_id(params["nodeId"].as_u64().ok_or("valid nodeId required")?);
            let node = NodeId::new(node);
            live_frame(ctx.get_session_page(session_id).ok_or("No page")?, frame)?;
            let depth = match params.get("depth") {
                None => 1,
                Some(value) => match value.as_i64() {
                    Some(-1) => u32::MAX,
                    Some(value) if value > 0 => u32::try_from(value).unwrap_or(u32::MAX),
                    _ => return Err("depth must be positive or -1".into()),
                },
            };
            if params["pierce"].as_bool() == Some(true) {
                let page = ctx.get_session_page(session_id).ok_or("No page")?;
                let needs_piercing = page.with_frame_dom(frame, |dom| {
                    let mut pending = vec![(node, depth.min(MAX_SERIALIZE_DEPTH))];
                    while let Some((id, remaining)) = pending.pop() {
                        if dom.shadow_root(id).is_some() || dom.get_node(id).is_some_and(|node|
                            node.as_element().is_some_and(|name| name.ns.as_ref() == "http://www.w3.org/1999/xhtml"
                                && matches!(name.local.as_ref(), "iframe" | "frame"))) {
                            return true;
                        }
                        if remaining > 0 {
                            pending.extend(dom.children(id).into_iter().map(|child| (child, remaining - 1)));
                        }
                    }
                    false
                }).unwrap_or(false);
                if needs_piercing {
                    return Err("Piercing frame and shadow trees is not supported".into());
                }
            }
            if let Some(mut state) = ctx.dom_documents.remove(session_id) {
                let mut events = Vec::new();
                if let Some(page) = ctx.get_session_page(session_id) {
                    let scope = FrameScope::of(page, frame);
                    page.with_frame_dom(frame, |dom| {
                        push_children(dom, &scope, &mut state, node, depth, session_id, &mut events)
                    });
                }
                ctx.dom_documents.insert(session_id.clone(), state);
                ctx.pending_events.extend(events);
            }
            Ok(json!({}))
        }
        "requestNode" => {
            let oid = params["objectId"].as_str().ok_or("objectId required")?;
            let page = ctx.get_session_page_mut(session_id).ok_or("No page")?;
            let (frame, node) = page.node_ref(oid)
                .map_err(|_| "Object id does not reference a Node in this document".to_string())?;
            Ok(json!({"nodeId":push_node(ctx, session_id, frame, NodeId::new(node))?}))
        }
        "getNodeForLocation" => {
            let x = params["x"].as_i64().ok_or("integer x required")?;
            let y = params["y"].as_i64().ok_or("integer y required")?;
            if params["ignorePointerEventsNone"].as_bool() == Some(true) {
                return Err("Ignoring pointer-events in hit testing is not supported".into());
            }
            let page = ctx.get_session_page_mut(session_id).ok_or("No page")?;
            let point = frames::frame_at_point(page, x as f64, y as f64)?;
            let id = page.evaluate_in_frame_realm(point.frame, &format!(
                "(function() {{ var node = document.elementFromPoint({},{}); return node ? node._nid : -1; }})()",
                point.x, point.y,
            ))?.as_f64().filter(|id| *id >= 0.0 && *id <= u32::MAX as f64)
                .ok_or("No node found at given location")? as u32;
            let frame_id = if point.frame == 0 {
                page.frame_id.clone()
            } else {
                child_frame_id(&page.frame_id, point.frame)
            };
            let mut result = json!({"backendNodeId":node_id(point.frame, id),"frameId":frame_id});
            if ctx.dom_documents.contains_key(session_id) {
                result["nodeId"] = json!(push_node(ctx, session_id, point.frame, NodeId::new(id))?);
            }
            Ok(result)
        }
        "querySelector" => {
            let page = ctx.get_session_page(session_id).ok_or("No page")?;
            let selector = params.get("selector").and_then(|v| v.as_str()).ok_or("selector required")?;
            let (frame, root) = split_node_id(params.get("nodeId").and_then(|v| v.as_u64()).unwrap_or(0));
            let result = page.with_frame_dom(live_frame(page, frame)?, |dom| {
                let root = if root == 0 { dom.document() } else { NodeId::new(root) };
                dom.query_selector_from(root, selector).ok().flatten()
                    .map(|id| node_id(frame, id.index() as u32)).unwrap_or(0)
            }).unwrap_or(0);
            Ok(json!({ "nodeId": result }))
        }
        "querySelectorAll" => {
            let page = ctx.get_session_page(session_id).ok_or("No page")?;
            let selector = params.get("selector").and_then(|v| v.as_str()).ok_or("selector required")?;
            let (frame, root) = split_node_id(params.get("nodeId").and_then(|v| v.as_u64()).unwrap_or(0));
            let ids = page.with_frame_dom(live_frame(page, frame)?, |dom| {
                let root = if root == 0 { dom.document() } else { NodeId::new(root) };
                dom.query_selector_all_from(root, selector).ok()
                    .map(|ids| ids.iter().map(|id| node_id(frame, id.index() as u32)).collect::<Vec<_>>())
                    .unwrap_or_default()
            }).unwrap_or_default();
            Ok(json!({ "nodeIds": ids }))
        }
        "getOuterHTML" => {
            // go-rod asks for the HTML of an element handle it already holds and
            // sends `objectId` only (rod/element.go DOMGetOuterHTML). Chromium
            // accepts all three identifiers here; accepting nodeId alone made
            // every rod page.HTML() fail with "nodeId required".
            let page = ctx.get_session_page_mut(session_id).ok_or("No page")?;
            let (frame, node) = resolve_node(page, params)?;
            let html = page.with_frame_dom(live_frame(page, frame)?, |dom| {
                dom.outer_html(node)
            }).unwrap_or_default();
            Ok(json!({ "outerHTML": html }))
        }
        "describeNode" => {
            let page = ctx.get_session_page_mut(session_id).ok_or("No page")?;
            let depth = params.get("depth").and_then(|v| v.as_i64()).unwrap_or(0);

            if ["nodeId", "backendNodeId", "objectId"].iter().all(|key| params.get(key).is_none()) {
                return Err("nodeId or objectId required".to_string());
            }
            let (frame, node) = resolve_node(page, params)?;
            let frame = live_frame(page, frame)?;
            let scope = FrameScope::of(page, frame);
            let node = page.with_frame_dom(frame, |dom| {
                serialize_node(dom, node, depth as u32, 0, &scope)
            }).unwrap_or(json!(null));
            Ok(json!({ "node": node }))
        }
        "getFrameOwner" => {
            let frame_id = params["frameId"].as_str().ok_or("frameId required")?;
            let page = ctx.get_session_page(session_id).ok_or("No page")?;
            let (parent, node) = crate::domains::page::frame_number(page, frame_id)
                .filter(|frame| *frame != 0)
                .and_then(|frame| page.frame_owner(frame))
                .ok_or("Frame with the given id does not belong to the target.")?;
            Ok(json!({ "backendNodeId": node_id(parent, node) }))
        }
        "resolveNode" => {
            let page = ctx.get_session_page_mut(session_id).ok_or("No page")?;
            let (frame, node) = resolve_node(page, params)?;
            let frame = live_frame(page, frame)?;
            let node_id = node.index();

            let js_code = format!("globalThis._wrap({node_id})");

            // The handle is a wrapper from the node's own frame, so a script
            // run through it sees that frame's globals.
            let info = if let Some(js) = &mut page.js {
                match js.start_cdp_evaluation(&js_code, false, false, frame) {
                    Ok(inbrowse_js::runtime::CdpEvaluation::Complete(info)) => info,
                    Ok(inbrowse_js::runtime::CdpEvaluation::Pending(_)) | Err(_) => {
                        return Ok(json!({
                            "object": {
                                "type": "object",
                                "subtype": "node",
                                "className": "HTMLElement",
                                "objectId": format!("node-{}", node_id),
                            }
                        }));
                    }
                }
            } else {
                return Err("No JS runtime".to_string());
            };

            Ok(json!({
                "object": {
                    "type": "object",
                    "subtype": "node",
                    "className": if info.class_name.is_empty() { "HTMLElement".to_string() } else { info.class_name },
                    "description": info.description,
                    "objectId": info.object_id.unwrap_or_else(|| format!("node-{}", node_id)),
                }
            }))
        }
        "setAttributeValue" => Ok(json!({})),
        "removeNode" => Ok(json!({})),
        "focus" => {
            // No layout engine, but inbrowse's JS focus() sets document.activeElement,
            // which Input.dispatchKeyEvent targets. CDP clients (browser-use) focus an
            // input via DOM.focus before typing; without this their keystrokes land on
            // nothing and the field stays empty.
            let page = ctx.get_session_page_mut(session_id).ok_or("No page")?;
            let node_id = resolve_node_id(page, params)?;
            let (frame, node) = split_node_id(node_id);
            let code = format!(
                "(function() {{ var el = globalThis._wrap && globalThis._wrap({0}); \
                 if (el && typeof el.focus === 'function') {{ el.focus(); return true; }} return false; }})()",
                node
            );
            if page.evaluate_in_frame_realm(live_frame(page, frame)?, &code)?.as_bool() != Some(true) {
                return Err(format!("node {node_id} is not an element that can take focus"));
            }
            Ok(json!({}))
        }
        "scrollIntoViewIfNeeded" => {
            let page = ctx.get_session_page_mut(session_id).ok_or("No page")?;
            let node_id = resolve_node_id(page, params)?;
            let (frame, node) = split_node_id(node_id);
            // Share the DOM shim's nested-scrollport handling. Fully visible
            // boxes stay put; clipped boxes are centered only where needed.
            let code = format!(
                "(function() {{ var el = globalThis._wrap && globalThis._wrap({0}); \
                 if (!el || typeof el.scrollIntoViewIfNeeded !== 'function') return false; \
                 el.scrollIntoViewIfNeeded(); return true; }})()",
                node
            );
            let did_scroll = page
                .evaluate_in_frame_realm(live_frame(page, frame)?, &code)?
                .as_bool()
                .unwrap_or(false);
            if !did_scroll {
                return Err(format!(
                    "node {node_id} could not be resolved to a scrollable element"
                ));
            }
            Ok(json!({}))
        }
        "setFileInputFiles" => {
            // Puppeteer's ElementHandle.uploadFile / Playwright's setInputFiles
            // drive an <input type=file> through this CDP call (issue #359). Read
            // each local file, then hand its bytes (base64) to the JS layer, which
            // builds real File objects and fires input+change like a real
            // selection so page code can read/upload them.
            let page = ctx.get_session_page_mut(session_id).ok_or("No page")?;
            // setFileInputFiles reads local files and hands their bytes to page
            // JS. Anyone who can reach the CDP port (default localhost, but
            // Docker images bind 0.0.0.0) could otherwise read any file the
            // process can read — the same threat as Page.navigate to file://, so
            // it honours the same opt-in and is off by default.
            if !page.context.allow_file_access {
                return Err("DOM.setFileInputFiles is disabled. Restart with `inbrowse serve --allow-file-access` to enable local file uploads.".to_string());
            }
            let files = params.get("files").and_then(Value::as_array)
                .ok_or("DOM.setFileInputFiles requires a files array")?;
            let paths: Vec<&str> = files.iter().map(|file| file.as_str()
                .ok_or_else(|| "DOM.setFileInputFiles requires string file paths".to_string()))
                .collect::<Result<_, _>>()?;
            let node_id = resolve_node_id(page, params)?;
            let (frame, node) = split_node_id(node_id);
            let frame = live_frame(page, frame)?;
            // Validate the target before any path is read: a request aimed at a
            // missing or non-file node must not touch the file system at all.
            let exists = page.with_frame_dom(frame, |dom| dom.get_node(NodeId::new(node)).is_some())
                .unwrap_or(false);
            if !exists {
                return Err("Could not find node with given id".into());
            }
            if !run_file_input_helper(page, frame, node, "[]", true)? {
                return Err("Node is not a file input element".into());
            }

            let mut specs = Vec::with_capacity(paths.len());
            for p in &paths {
                let bytes = std::fs::read(p)
                    .map_err(|e| format!("setFileInputFiles: cannot read '{}': {}", p, e))?;
                let name = std::path::Path::new(p)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("file")
                    .to_string();
                specs.push(json!({ "name": name, "type": guess_mime(p), "b64": encode_base64(&bytes) }));
            }

            let specs_json = serde_json::to_string(&specs).map_err(|error| error.to_string())?;
            if !run_file_input_helper(page, frame, node, &specs_json, false)? {
                return Err("Could not set files on the input element".into());
            }
            Ok(json!({}))
        }
        "getBoxModel" => {
            let page = ctx.get_session_page_mut(session_id).ok_or("No page")?;
            let (frame, node) = resolve_node(page, params)?;
            let frame = live_frame(page, frame)?;
            let boxes = element_boxes(page, frame, node)?;
            let quad = |index: usize| -> Vec<Value> {
                boxes[index * 8..(index + 1) * 8].iter().map(|n| coord_value(*n)).collect()
            };
            Ok(json!({
                "model": {
                    "content": quad(0),
                    "padding": quad(1),
                    "border": quad(2),
                    "margin": quad(3),
                    "width": coord_value(boxes[32]), "height": coord_value(boxes[33]),
                }
            }))
        }
        "getContentQuads" => {
            let page = ctx.get_session_page_mut(session_id).ok_or("No page")?;
            let (frame, node) = resolve_node(page, params)?;
            let frame = live_frame(page, frame)?;
            // The border box, in the page's viewport coordinates whichever
            // frame the node is in.
            let boxes = element_boxes(page, frame, node)?;
            let quad: Vec<Value> = boxes[16..24].iter().map(|n| coord_value(*n)).collect();
            Ok(json!({ "quads": [quad] }))
        }
        _ => Err(format!("Unknown DOM method: {}", method)),
    }
}

/// Serialize a CDP box-model coordinate the way Chrome does: an integral double
/// (e.g. `256.0`) becomes a JSON integer (`256`), while a genuinely fractional
/// value (`206.0390625`) stays a float. serde_json always writes an `f64` with a
/// decimal point, so `json!(256.0)` yields `256.0` — which strict CDP clients
/// (Hermes Agent) deserialize as `i64` and reject, breaking every click-by-
/// coordinate flow. Chrome only widens fractional coordinates, so mirroring that
/// keeps those clients working. (issue #576)
fn coord_value(n: f64) -> Value {
    // Collapse to an integer only when the value is exactly integral and fits an
    // i64 losslessly; NaN/inf and out-of-range doubles fall through unchanged.
    if n.is_finite() && n.fract() == 0.0 && n >= i64::MIN as f64 && n <= i64::MAX as f64 {
        json!(n as i64)
    } else {
        json!(n)
    }
}

// Hard cap on how deep a getDocument/describeNode response may nest, independent
// of the requested `depth`. DOM.getDocument{depth:-1} arrives here as u32::MAX,
// which on a pathologically deep DOM (trivially scriptable, unbounded by
// html5ever) produces a Value nested that far. Even built without recursion,
// serde_json's own serialization and the Value's Drop recurse over that nesting
// and overflow the stack — on tokio's ~2 MiB worker stacks especially. Bounding
// the depth keeps the response safe to serialize and drop. Real DOMs are shallow
// (deep React trees are a few hundred), so this only truncates pathological
// nesting, which beats crashing the worker (AGENTS.md: "one page must never
// crash a worker"). Mirrors DOMSnapshot's MAX_NODES guard (issue #341).
//
// The number is sized for the ~2 MiB stack of a tokio worker thread (where the
// CDP processor runs, and where `#[test]` threads also run): each DOM level
// becomes two nested JSON containers (object -> "children" array -> object ...),
// so serde_json's recursive serialization and the Value's recursive Drop descend
// ~2x this depth. 256 keeps that a few hundred frames deep with wide margin,
// while still far exceeding any real page. Clients needing a deeper subtree
// re-request it with DOM.requestChildNodes / describeNode on a specific node.
const MAX_SERIALIZE_DEPTH: u32 = 256;

/// Build the CDP Node object for a single node (without its `children` array),
/// returning it together with that node's child ids. `None` for a missing node.
fn node_value(dom: &DomTree, id: NodeId, scope: &FrameScope) -> Option<(Value, Vec<NodeId>)> {
    let node = dom.get_node(id)?;
    let children_ids = dom.children(id);
    let child_count = children_ids.len();
    let cdp_id = node_id(scope.frame, id.index() as u32);
    let mut result = json!({ "nodeId": cdp_id, "backendNodeId": cdp_id, "childNodeCount": child_count });

    match &node.data {
        NodeData::Document => {
            result["nodeType"] = json!(9); result["nodeName"] = json!("#document");
            result["localName"] = json!(""); result["nodeValue"] = json!("");
            result["documentURL"] = json!(""); result["baseURL"] = json!(""); result["xmlVersion"] = json!("");
            if let Some(frame_id) = &scope.own_id {
                result["frameId"] = json!(frame_id);
            }
        }
        NodeData::Doctype { name, public_id, system_id } => {
            result["nodeType"] = json!(10); result["nodeName"] = json!(name);
            result["localName"] = json!(""); result["nodeValue"] = json!("");
            result["publicId"] = json!(public_id); result["systemId"] = json!(system_id);
        }
        NodeData::Element { name, attrs, .. } => {
            result["nodeType"] = json!(1);
            result["nodeName"] = json!(name.local.as_ref().to_ascii_uppercase());
            result["localName"] = json!(name.local.as_ref());
            result["nodeValue"] = json!("");
            let cdp_attrs: Vec<String> = attrs.iter()
                .flat_map(|a| [a.qualified_name(), a.value.clone()]).collect();
            result["attributes"] = json!(cdp_attrs);
            if let Some(link) = scope.links.get(&(id.index() as u32)) {
                result["frameId"] = json!(link.id);
                result["contentDocument"] = link.document.clone();
            }
        }
        NodeData::Text { contents } => {
            result["nodeType"] = json!(3); result["nodeName"] = json!("#text");
            result["localName"] = json!(""); result["nodeValue"] = json!(contents);
        }
        NodeData::Comment { contents } => {
            result["nodeType"] = json!(8); result["nodeName"] = json!("#comment");
            result["localName"] = json!(""); result["nodeValue"] = json!(contents);
        }
        NodeData::ProcessingInstruction { target, data } => {
            result["nodeType"] = json!(7); result["nodeName"] = json!(target);
            result["localName"] = json!(""); result["nodeValue"] = json!(data);
        }
    }

    Some((result, children_ids))
}

/// Serialize a node and its descendants into the CDP Node tree, iteratively.
/// The requested `max_depth` is clamped to `current_depth + MAX_SERIALIZE_DEPTH`
/// so a `depth:-1` (u32::MAX) request on a very deep DOM cannot produce a Value
/// that overflows the stack when serde_json later serializes or drops it. An
/// explicit heap worklist keeps the builder itself off the call stack.
fn serialize_node(
    dom: &DomTree,
    root: NodeId,
    max_depth: u32,
    current_depth: u32,
    scope: &FrameScope,
) -> Value {
    let max_depth = max_depth.min(current_depth.saturating_add(MAX_SERIALIZE_DEPTH));

    struct Frame {
        value: Value,
        children: Vec<NodeId>,
        next: usize,
        built: Vec<Value>,
        depth: u32,
        expand: bool,
    }

    let (root_value, root_children) = match node_value(dom, root, scope) {
        Some(v) => v,
        None => return json!(null),
    };
    let root_expand = current_depth < max_depth && !root_children.is_empty();
    let mut stack = vec![Frame {
        value: root_value,
        children: root_children,
        next: 0,
        built: Vec::new(),
        depth: current_depth,
        expand: root_expand,
    }];

    loop {
        // Decide the next step without holding a borrow across a push.
        let next_child = {
            let top = stack.last_mut().expect("stack is non-empty in loop");
            if top.expand && top.next < top.children.len() {
                let cid = top.children[top.next];
                top.next += 1;
                Some(cid)
            } else {
                None
            }
        };

        match next_child {
            Some(cid) => {
                let child_depth = stack.last().unwrap().depth + 1;
                match node_value(dom, cid, scope) {
                    Some((cval, cchildren)) => {
                        let cexpand = child_depth < max_depth && !cchildren.is_empty();
                        stack.push(Frame {
                            value: cval,
                            children: cchildren,
                            next: 0,
                            built: Vec::new(),
                            depth: child_depth,
                            expand: cexpand,
                        });
                    }
                    // Missing child: match the old recursive behavior of emitting null.
                    None => stack.last_mut().unwrap().built.push(json!(null)),
                }
            }
            None => {
                // This node's children are all built; finalize and fold into parent.
                let mut frame = stack.pop().unwrap();
                if !frame.built.is_empty() {
                    frame.value["children"] = json!(frame.built);
                }
                match stack.last_mut() {
                    Some(parent) => parent.built.push(frame.value),
                    None => return frame.value,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatch::CdpContext;

    #[tokio::test(flavor = "current_thread")]
    async fn dom_node_tracking_is_released_on_navigation_detach_and_close() {
        let mut ctx = CdpContext::new();
        let page = ctx.create_page();
        let session = Some(format!("{page}-session"));
        ctx.sessions.insert(session.clone().unwrap(), page.clone());
        crate::domains::page::handle("navigate", &json!({"url":"data:text/html,<p>first</p>"}),
            &mut ctx, &session).await.unwrap();
        handle("getDocument", &json!({"depth":-1}), &mut ctx, &session).await.unwrap();
        ctx.pending_events.clear();
        crate::domains::page::handle("navigate", &json!({"url":"data:text/html,<p>second</p>"}),
            &mut ctx, &session).await.unwrap();
        assert!(ctx.dom_documents.is_empty(), "navigation must release the old node cache");
        assert!(ctx.pending_events.iter().any(|event| event.method == "DOM.documentUpdated"
            && event.session_id == session));
        handle("getDocument", &json!({}), &mut ctx, &session).await.unwrap();
        crate::domains::target::handle("detachFromTarget", &json!({"sessionId":session}),
            &mut ctx, &None).await.unwrap();
        assert!(ctx.dom_documents.is_empty(), "detach must release session-owned nodes");
        let closing_session = Some("closing".to_string());
        ctx.sessions.insert(closing_session.clone().unwrap(), page.clone());
        handle("getDocument", &json!({}), &mut ctx, &closing_session).await.unwrap();
        assert_eq!(ctx.dom_documents.len(), 1);
        ctx.remove_page(&page);
        assert!(ctx.dom_documents.is_empty(), "close must release the target's node cache");
    }

    // The escaping this file used to own now lives in `util::object_id_literal`,
    // which is where its tests went with it: the three lookup sites here embed
    // the id as a JSON literal rather than splicing it into a single-quoted
    // string, so there is no per-domain escaping left to assert on.

    // A stale/invalid objectId resolves to -1 in JS. describeNode/resolveNode
    // must surface an error, not cast -1 to nodeId 0 (the document root) and
    // return the wrong node. See #917.
    #[tokio::test]
    async fn describe_node_errors_on_unresolvable_object_id() {
        let mut ctx = CdpContext::new();
        let page_id = ctx.create_page();
        let session = Some(format!("{page_id}-session"));
        ctx.sessions.insert(session.clone().unwrap(), page_id.clone());

        crate::domains::page::handle(
            "navigate",
            &json!({ "url": "data:text/html,<p>hi</p>", "waitUntil": "load" }),
            &mut ctx,
            &session,
        )
        .await
        .expect("navigate should succeed");

        let res = handle(
            "describeNode",
            &json!({ "objectId": "no-such-object" }),
            &mut ctx,
            &session,
        )
        .await;
        assert!(
            res.is_err(),
            "describeNode must error on an unresolvable objectId, got: {res:?}"
        );
    }

    #[tokio::test]
    async fn resolve_node_errors_on_unresolvable_object_id() {
        let mut ctx = CdpContext::new();
        let page_id = ctx.create_page();
        let session = Some(format!("{page_id}-session"));
        ctx.sessions.insert(session.clone().unwrap(), page_id.clone());

        crate::domains::page::handle(
            "navigate",
            &json!({ "url": "data:text/html,<p>hi</p>", "waitUntil": "load" }),
            &mut ctx,
            &session,
        )
        .await
        .expect("navigate should succeed");

        let res = handle(
            "resolveNode",
            &json!({ "objectId": "no-such-object" }),
            &mut ctx,
            &session,
        )
        .await;
        assert!(
            res.is_err(),
            "resolveNode must error on an unresolvable objectId, got: {res:?}"
        );
    }

    #[tokio::test]
    async fn dom_focus_sets_active_element() {
        // CDP clients (browser-use) focus an input via DOM.focus before typing;
        // dispatchKeyEvent then targets document.activeElement. DOM.focus must
        // actually move focus or keystrokes land on nothing.
        let mut ctx = CdpContext::new();
        let page_id = ctx.create_page();
        let session = Some(format!("{page_id}-session"));
        ctx.sessions.insert(session.clone().unwrap(), page_id.clone());

        crate::domains::page::handle(
            "navigate",
            &json!({ "url": "data:text/html,<input id=q>", "waitUntil": "load" }),
            &mut ctx,
            &session,
        )
        .await
        .expect("navigate should succeed");

        let qs = handle("querySelector", &json!({ "selector": "input" }), &mut ctx, &session)
            .await
            .expect("querySelector should succeed");
        let nid = qs["nodeId"].as_u64().expect("input nodeId");
        assert!(nid > 0, "the input element should be found");

        handle("focus", &json!({ "nodeId": nid }), &mut ctx, &session)
            .await
            .expect("DOM.focus should succeed");

        let active = ctx
            .get_session_page_mut(&session)
            .unwrap()
            .evaluate("(function(){return document.activeElement?document.activeElement.tagName:'NONE';})()")
            .unwrap();
        assert_eq!(
            active,
            json!("INPUT"),
            "DOM.focus must set document.activeElement to the focused input"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn resolve_node_preserves_identity_and_specialized_wrappers() {
        let mut ctx = CdpContext::new();
        let page_id = ctx.create_page();
        let session = Some(format!("{page_id}-session"));
        ctx.sessions.insert(session.clone().unwrap(), page_id);
        crate::domains::page::handle("navigate", &json!({
            "url": "data:text/html,<html><body><textarea id=field></textarea><a id=link href=https://example.com>Next</a></body></html>"
        }), &mut ctx, &session).await.unwrap();
        for (id, constructor) in [
            ("field", "HTMLTextAreaElement"),
            ("link", "HTMLAnchorElement"),
        ] {
            let selector = format!("#{id}");
            let query = handle(
                "querySelector",
                &json!({"selector": selector}),
                &mut ctx,
                &session,
            )
            .await
            .unwrap();
            let resolved = handle(
                "resolveNode",
                &json!({"backendNodeId": query["nodeId"]}),
                &mut ctx,
                &session,
            )
            .await
            .unwrap();
            let object_id = resolved["object"]["objectId"].as_str().unwrap();
            let expression = format!(
                "(() => {{ const node = globalThis.__inbrowse_objects[{}]; return node === document.getElementById({}) && node instanceof {}; }})()",
                serde_json::to_string(object_id).unwrap(),
                serde_json::to_string(id).unwrap(),
                constructor
            );
            assert_eq!(
                ctx.get_session_page_mut(&session)
                    .unwrap()
                    .evaluate(&expression)
                    .unwrap(),
                json!(true)
            );
        }

        let document = handle("getDocument", &json!({}), &mut ctx, &session)
            .await
            .unwrap();
        let resolved = handle(
            "resolveNode",
            &json!({"nodeId": document["root"]["nodeId"]}),
            &mut ctx,
            &session,
        )
        .await
        .unwrap();
        let object_id = resolved["object"]["objectId"].as_str().unwrap();
        let expression = format!(
            "globalThis.__inbrowse_objects[{}] === document",
            serde_json::to_string(object_id).unwrap()
        );
        assert_eq!(
            ctx.get_session_page_mut(&session)
                .unwrap()
                .evaluate(&expression)
                .unwrap(),
            json!(true)
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn scroll_into_view_if_needed_resolves_all_node_identifiers() {
        let mut ctx = CdpContext::new();
        let page_id = ctx.create_page();
        let session = Some(format!("{page_id}-session"));
        ctx.sessions.insert(session.clone().unwrap(), page_id);

        crate::domains::page::handle(
            "navigate",
            &json!({
                "url": "data:text/html,<main><button id=target>Go</button></main>",
                "waitUntil": "load"
            }),
            &mut ctx,
            &session,
        )
        .await
        .expect("navigate should succeed");

        let query = handle(
            "querySelector",
            &json!({ "selector": "#target" }),
            &mut ctx,
            &session,
        )
        .await
        .expect("querySelector should succeed");
        let node_id = query["nodeId"].as_u64().expect("button nodeId");

        let resolved = handle(
            "resolveNode",
            &json!({ "nodeId": node_id }),
            &mut ctx,
            &session,
        )
        .await
        .expect("resolveNode should succeed");
        let object_id = resolved["object"]["objectId"]
            .as_str()
            .expect("button objectId")
            .to_string();

        for params in [
            json!({ "nodeId": node_id }),
            json!({ "backendNodeId": node_id }),
            json!({ "objectId": object_id }),
        ] {
            ctx.get_session_page_mut(&session)
                .unwrap()
                .evaluate("globalThis.__inbrowse_click_target = null")
                .unwrap();

            handle("scrollIntoViewIfNeeded", &params, &mut ctx, &session)
                .await
                .expect("scrollIntoViewIfNeeded should succeed");

            let target_id = ctx
                .get_session_page_mut(&session)
                .unwrap()
                .evaluate("globalThis.__inbrowse_click_target && globalThis.__inbrowse_click_target.id")
                .unwrap();
            assert_eq!(target_id, json!("target"));
        }
    }

    #[tokio::test(flavor = "current_thread")]
    #[cfg(feature = "render")]
    async fn scroll_into_view_if_needed_preserves_visible_positions() {
        let mut ctx = CdpContext::new();
        let page_id = ctx.create_page();
        let session = Some(format!("{page_id}-session"));
        ctx.sessions.insert(session.clone().unwrap(), page_id);
        crate::domains::page::handle("navigate", &json!({
            "url": "data:text/html,<body style='margin:0;height:1800px'><div style='height:200px'></div><button id=target style='height:40px'>Go</button></body>",
            "waitUntil": "load"
        }), &mut ctx, &session).await.unwrap();
        let node = handle("querySelector", &json!({"selector":"#target"}), &mut ctx, &session).await.unwrap();
        handle("scrollIntoViewIfNeeded", &json!({"nodeId":node["nodeId"]}), &mut ctx, &session).await.unwrap();
        let position = ctx.get_session_page_mut(&session).unwrap()
            .evaluate("[scrollY, document.getElementById('target').getBoundingClientRect().top]")
            .unwrap();
        assert_eq!(position, json!([0, 200]));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn scroll_into_view_if_needed_requires_a_node_identifier() {
        let mut ctx = CdpContext::new();
        let page_id = ctx.create_page();
        let session = Some(format!("{page_id}-session"));
        ctx.sessions.insert(session.clone().unwrap(), page_id);

        let error = handle(
            "scrollIntoViewIfNeeded",
            &json!({}),
            &mut ctx,
            &session,
        )
        .await
        .expect_err("missing node identifier should fail");

        assert_eq!(error, "nodeId, backendNodeId, or objectId required");

        let error = handle(
            "scrollIntoViewIfNeeded",
            &json!({ "nodeId": 999_999 }),
            &mut ctx,
            &session,
        )
        .await
        .expect_err("an unknown node should fail");

        assert_eq!(
            error,
            "node 999999 could not be resolved to a scrollable element"
        );
    }

    // A hostile or heavy page can nest nodes tens of thousands deep (trivially
    // scriptable, and html5ever puts no cap on generic nesting). DOM.getDocument
    // with the standard depth:-1 (which becomes u32::MAX here) must handle such a
    // tree without crashing the worker (AGENTS.md: "one page must never crash a
    // worker"). Two failure modes: (1) a recursive serialize_node overflows the
    // stack building the Value, and (2) even an iterative builder would emit a
    // Value so deeply nested that serde_json's own recursive serialize/Drop
    // overflow. The fix derecurses the builder AND bounds the depth, so the
    // response is always safe to serialize and drop. With the old recursive
    // serialize_node this test aborts with SIGABRT.
    #[test]
    fn get_document_deep_tree_does_not_overflow() {
        use inbrowse_dom::{DomTree, NodeData};

        // Build the deep chain directly (no parser) so setup is O(n) and fast.
        let dom = DomTree::new();
        let mut parent = dom.document();
        let depth = 50_000usize;
        for _ in 0..depth {
            let n = dom.new_node(NodeData::Text { contents: String::new() });
            dom.append_child(parent, n);
            parent = n;
        }

        // Mirror getDocument {"depth": -1}: as_i64() == -1, then `depth as u32`.
        let scope = FrameScope { frame: 0, own_id: None, links: HashMap::new() };
        let node = serialize_node(&dom, dom.document(), (-1i64) as u32, 0, &scope);

        // serde_json's own serialization recurses over the Value nesting; this
        // must not overflow either. That is why the fix bounds depth, not just
        // the builder.
        let s = serde_json::to_string(&node).expect("serialize");
        assert!(!s.is_empty());

        // Output nesting is bounded well below the tree's true depth (truncated,
        // not crashed) yet still serializes a meaningful prefix.
        let mut cur = &node;
        let mut levels = 0usize;
        while let Some(children) = cur.get("children").and_then(|c| c.as_array()) {
            let Some(first) = children.first() else { break };
            cur = first;
            levels += 1;
        }
        assert!(levels >= 100, "should serialize a deep prefix, got {levels}");
        assert!(
            levels < depth,
            "nesting must be bounded below the tree's true depth, got {levels}"
        );
    }

    /// SEC-003 / #579 — DOM.setFileInputFiles reads local files and hands
    /// their bytes to page JS. Without a gate, any CDP client (e.g. against a
    /// Docker image that binds the port to 0.0.0.0) gets an arbitrary
    /// file-read primitive. It must honour the same `allow_file_access` opt-in
    /// as Page.navigate to `file://`, which defaults to off.
    #[tokio::test(flavor = "current_thread")]
    async fn set_file_input_files_refuses_without_allow_file_access() {
        let mut ctx = CdpContext::new();
        let page_id = ctx.create_page();
        let session = Some(format!("{page_id}-session"));
        ctx.sessions.insert(session.clone().unwrap(), page_id);

        crate::domains::page::handle(
            "navigate",
            &json!({ "url": "data:text/html,<input type=file id=f>", "waitUntil": "load" }),
            &mut ctx,
            &session,
        )
        .await
        .expect("navigate should succeed");

        let qs = handle("querySelector", &json!({ "selector": "input" }), &mut ctx, &session)
            .await
            .expect("querySelector should succeed");
        let nid = qs["nodeId"].as_u64().expect("input nodeId");

        // A real, readable file. Without the gate the handler slurps it and
        // returns Ok; with the gate it must refuse before touching the disk.
        let existing = format!("{}/Cargo.toml", env!("CARGO_MANIFEST_DIR"));
        let err = handle(
            "setFileInputFiles",
            &json!({ "nodeId": nid, "files": [existing] }),
            &mut ctx,
            &session,
        )
        .await
        .expect_err("setFileInputFiles must be gated behind --allow-file-access");
        assert!(
            err.contains("allow-file-access"),
            "error must point at the allow-file-access flag: {err}"
        );
    }

    // issue #576: DOM.getBoxModel / getContentQuads build their quad from f64
    // pixel values, so serde_json emits integral coordinates as `256.0`. Strict
    // CDP clients (Hermes Agent) deserialize the quad as i64 and reject the
    // float. coord_value must serialize integral coordinates as integers, the way
    // Chrome does, while leaving genuinely fractional ones as floats.
    #[test]
    fn box_model_integral_coordinates_serialize_as_integers() {
        assert_eq!(serde_json::to_string(&coord_value(256.0)).unwrap(), "256");
        assert_eq!(serde_json::to_string(&coord_value(0.0)).unwrap(), "0");
        // fractional coordinates stay floats
        assert_eq!(serde_json::to_string(&coord_value(206.0390625)).unwrap(), "206.0390625");
        // a full quad mixes both, exactly as DOM.getBoxModel returns it
        let quad: Vec<Value> = [
            256.0, 206.0390625, 347.25, 206.0390625, 347.25, 225.0390625, 256.0, 225.0390625,
        ]
        .iter()
        .map(|n| coord_value(*n))
        .collect();
        assert_eq!(
            serde_json::to_string(&quad).unwrap(),
            "[256,206.0390625,347.25,206.0390625,347.25,225.0390625,256,225.0390625]"
        );
    }
}
