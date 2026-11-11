#[cfg(not(feature = "render"))]
use std::collections::HashMap;

use inbrowse::accessibility::{build_ax_tree, AccessibilityTree};
use inbrowse::Page;
use inbrowse_dom::NodeId;
use serde_json::{json, Value};

use crate::dispatch::CdpContext;

/// The AX tree of the frame `params.frameId` names, the main frame when absent.
///
/// A child frame's tree is built from its own document without the style
/// cascade, so only attribute-based hiding applies inside it.
///
/// Returns the tree with the number of the frame it belongs to (0 for the page).
fn tree_for(page: &Page, params: &Value) -> Result<(AccessibilityTree, u32), String> {
    let requested = match params.get("frameId") {
        None | Some(Value::Null) => None,
        Some(frame) => Some(frame.as_str().ok_or("frameId must be a string")?),
    };
    let child = match requested.filter(|frame| *frame != page.frame_id) {
        None => None,
        Some(frame) => Some(
            page.frames
                .iter()
                .find(|child| crate::domains::page::child_frame_id(&page.frame_id, child.frame_id()) == frame)
                .map(|child| child.frame_id())
                .ok_or("Frame with the given id was not found.")?,
        ),
    };
    let frame_id = requested.unwrap_or(&page.frame_id);
    match child {
        None => {
            #[cfg(feature = "render")]
            let styles = page.js.as_ref().map(|js| js.accessibility_styles()).unwrap_or_default();
            #[cfg(not(feature = "render"))]
            let styles = HashMap::new();
            page.with_dom(|dom| build_ax_tree(dom, &styles, Some(frame_id)))
                .map(|tree| (tree, 0))
                .ok_or("No DOM loaded".to_string())
        }
        Some(child) => page
            .js
            .as_ref()
            .and_then(|js| js.with_frame_dom(child, |dom| build_ax_tree(dom, &Default::default(), Some(frame_id))))
            .map(|tree| (tree, child))
            .ok_or("No DOM loaded".to_string()),
    }
}

/// Gives the nodes' `backendDOMNodeId` the DOM domain's ids, which name the
/// frame as well as the node. The page's own nodes keep their plain index.
fn qualify(frame: u32, mut nodes: Value) -> Value {
    fn qualify_node(frame: u32, node: &mut Value) {
        if let Some(index) = node["backendDOMNodeId"].as_u64() {
            node["backendDOMNodeId"] = json!(crate::domains::frames::node_id(frame, index as u32));
        }
    }
    if frame != 0 {
        if let Some(list) = nodes.as_array_mut() {
            for node in list {
                qualify_node(frame, node);
            }
        } else {
            qualify_node(frame, &mut nodes);
        }
    }
    nodes
}

/// `depth` limits the levels below the root. Negative means no limit, as the
/// browser's own default does.
fn depth_param(params: &Value) -> Result<Option<u32>, String> {
    match params.get("depth") {
        None | Some(Value::Null) => Ok(None),
        Some(depth) => {
            let depth = depth.as_i64().ok_or("depth must be an integer")?;
            Ok(u32::try_from(depth).ok())
        }
    }
}

/// The DOM node named by `nodeId`, `backendNodeId` or `objectId`.
///
/// Returns the node with the params that select its frame's tree: the caller's
/// own, plus the node's frame when the caller named none.
fn dom_node_param(
    ctx: &mut CdpContext,
    params: &Value,
    session_id: &Option<String>,
) -> Result<(NodeId, Value), String> {
    if ["nodeId", "backendNodeId", "objectId"].iter().all(|key| params.get(key).is_none_or(Value::is_null)) {
        return Err("Either nodeId, backendNodeId or objectId must be specified".to_string());
    }
    let page = ctx.get_session_page_mut(session_id).ok_or("No page")?;
    let id = crate::domains::dom::resolve_node_id(page, params)?;
    let (frame, node) = crate::domains::frames::split_node_id(id);
    let mut params = params.clone();
    if frame != 0 && params.get("frameId").is_none_or(Value::is_null) {
        params["frameId"] = json!(crate::domains::page::child_frame_id(&page.frame_id, frame));
    }
    Ok((NodeId::new(node), params))
}

pub async fn handle(
    method: &str,
    params: &Value,
    ctx: &mut CdpContext,
    session_id: &Option<String>,
) -> Result<Value, String> {
    match method {
        "enable" | "disable" => Ok(json!({})),
        "getFullAXTree" => {
            let depth = depth_param(params)?;
            let page = ctx.get_session_page(session_id).ok_or("No page")?;
            let (tree, frame) = tree_for(page, params)?;
            Ok(json!({"nodes": qualify(frame, json!(tree.nodes(depth)))}))
        }
        "getRootAXNode" => {
            let page = ctx.get_session_page(session_id).ok_or("No page")?;
            let (tree, frame) = tree_for(page, params)?;
            let root = tree.root().ok_or("No accessibility root")?;
            Ok(json!({"node": qualify(frame, json!(root))}))
        }
        "getChildAXNodes" => {
            let id = params.get("id").and_then(Value::as_str).ok_or("id must be a string")?;
            let page = ctx.get_session_page(session_id).ok_or("No page")?;
            let (tree, frame) = tree_for(page, params)?;
            let children = tree.children(id).ok_or("No node with given id found")?;
            Ok(json!({"nodes": qualify(frame, json!(children))}))
        }
        "getPartialAXTree" => {
            let relatives = params.get("fetchRelatives").and_then(Value::as_bool).unwrap_or(true);
            let (dom_node, params) = dom_node_param(ctx, params, session_id)?;
            let page = ctx.get_session_page(session_id).ok_or("No page")?;
            let (tree, frame) = tree_for(page, &params)?;
            let nodes = tree
                .partial(dom_node, relatives)
                .ok_or("No accessibility node for the given DOM node")?;
            Ok(json!({"nodes": qualify(frame, json!(nodes))}))
        }
        "queryAXTree" => {
            let name = params.get("accessibleName").and_then(Value::as_str);
            let role = params.get("role").and_then(Value::as_str);
            let (dom_node, params) = dom_node_param(ctx, params, session_id)?;
            let page = ctx.get_session_page(session_id).ok_or("No page")?;
            let (tree, frame) = tree_for(page, &params)?;
            let nodes = tree
                .query(dom_node, name, role)
                .ok_or("No accessibility node for the given DOM node")?;
            Ok(json!({"nodes": qualify(frame, json!(nodes))}))
        }
        _ => Err(format!("'Accessibility.{method}' wasn't found")),
    }
}
