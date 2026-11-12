//! Child frames for the DOM and Input domains: node ids that name a node in
//! any frame of a page, and the geometry that maps a frame's viewport onto its
//! parent's.

use inbrowse::Page;
use inbrowse_js::frame::FrameRealm;
use serde_json::Value;

/// Frames nest no deeper than this when a point is followed into them.
const MAX_FRAME_DEPTH: usize = 32;

/// A CDP node id is global across a page's frames. The page's own nodes keep
/// their plain index; a child frame's carry its frame number above bit 32,
/// which a JSON number holds exactly.
pub(crate) fn node_id(frame: u32, node: u32) -> u64 {
    (u64::from(frame) << 32) | u64::from(node)
}

/// The frame number and the node index inside that frame's tree.
pub(crate) fn split_node_id(id: u64) -> (u32, u32) {
    ((id >> 32) as u32, id as u32)
}

/// The child frame hosted by iframe element `node` of frame `parent`.
pub(crate) fn hosted_frame(page: &Page, parent: u32, node: u32) -> Option<u32> {
    page.frames
        .iter()
        .map(FrameRealm::frame_id)
        .filter(|frame| page.frame_owner(*frame) == Some((parent, node)))
        .max()
}

/// The two numbers of a probe that answers with an array of numbers.
fn numbers(value: &Value) -> Option<Vec<f64>> {
    value.as_array()?.iter().map(Value::as_f64).collect()
}

/// Where the content box of iframe element `node` begins in its own
/// document's viewport: its border box plus border and padding.
const CONTENT_ORIGIN_JS: &str = "var r = e.getBoundingClientRect(); var s = getComputedStyle(e); \
     var f = function(name) { return parseFloat(s[name]) || 0; }; \
     var left = r.left + f('borderLeftWidth') + f('paddingLeft'); \
     var top = r.top + f('borderTopWidth') + f('paddingTop');";

/// Where frame `frame`'s viewport origin lies in the page's viewport. The page
/// itself is at the origin.
pub(crate) fn frame_origin(page: &mut Page, frame: u32) -> Result<(f64, f64), String> {
    let (mut x, mut y) = (0.0, 0.0);
    let mut current = frame;
    for _ in 0..MAX_FRAME_DEPTH {
        if current == 0 {
            return Ok((x, y));
        }
        let (parent, node) = page.frame_owner(current).ok_or("The frame is detached")?;
        let probe = page.evaluate_in_frame_realm(
            parent,
            &format!(
                "(function() {{ var e = globalThis._wrap({node}); \
                 if (!e || typeof e.getBoundingClientRect !== 'function') return null; \
                 {CONTENT_ORIGIN_JS} return [left, top]; }})()"
            ),
        )?;
        let origin = numbers(&probe)
            .filter(|origin| origin.len() == 2)
            .ok_or("Could not compute the frame's position")?;
        x += origin[0];
        y += origin[1];
        current = parent;
    }
    Err("Frames are nested too deeply".into())
}

/// A point of the page's viewport as the frame under it sees it.
pub(crate) struct FramePoint {
    pub frame: u32,
    pub x: f64,
    pub y: f64,
}

/// Follows the point `(x, y)` of the page's viewport through every iframe
/// under it, to the innermost frame, and returns that frame with the point in
/// its own viewport coordinates.
pub(crate) fn frame_at_point(page: &mut Page, x: f64, y: f64) -> Result<FramePoint, String> {
    let mut point = FramePoint { frame: 0, x, y };
    if page.frames.is_empty() {
        return Ok(point);
    }
    for _ in 0..MAX_FRAME_DEPTH {
        let probe = page.evaluate_in_frame_realm(
            point.frame,
            &format!(
                "(function() {{ var e = document.elementFromPoint({x},{y}); \
                 if (!e || (e.localName !== 'iframe' && e.localName !== 'frame')) return null; \
                 {CONTENT_ORIGIN_JS} return [e._nid, left, top]; }})()",
                x = point.x,
                y = point.y,
            ),
        )?;
        let Some(hit) = numbers(&probe).filter(|hit| hit.len() == 3) else {
            return Ok(point);
        };
        let Some(child) = hosted_frame(page, point.frame, hit[0] as u32) else {
            return Ok(point);
        };
        point = FramePoint {
            frame: child,
            x: point.x - hit[1],
            y: point.y - hit[2],
        };
    }
    Err("Frames are nested too deeply".into())
}
