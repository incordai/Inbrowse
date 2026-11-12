//! `Performance.*`: Chrome's page metrics, read from counters the engine keeps
//! as the page runs (see `inbrowse_js::metrics`). A call costs the same on a
//! large document as on a small one.
//!
//! Where a value is an approximation of Chrome's:
//! - `Documents` equals `Frames`: the engine keeps no detached documents alive.
//! - `Nodes` counts every live node in every document, detached nodes included.
//! - `JSEventListeners` counts `addEventListener` registrations on nodes, the
//!   window and plain event targets; `on*` handler properties are not counted.
//! - `LayoutCount` and `RecalcStyleCount` count render passes. The cascade and
//!   layout run as one pass, which counts for both. A build without the render
//!   feature has neither and reports zero.
//! - `LayoutDuration` is the time of those full passes. `RecalcStyleDuration` is
//!   the time of passes that resolved styles alone (accessibility queries).
//! - `ScriptDuration` is the time the page's isolate was entered, less the
//!   passes script forced. `TaskDuration` is script plus every pass.
//!
//! Metrics for subsystems the engine does not have (audio handlers, media
//! keys, peer connections, workers, resource fetchers, UA stylesheets,
//! navigation paint timings) are not reported.

use std::sync::OnceLock;
use std::time::Instant;

use serde_json::{json, Value};

use crate::dispatch::CdpContext;

/// Seconds on a monotonic clock. Its origin is arbitrary, as Chrome's is; here
/// it is the wall clock at the first read, so values are comparable and positive.
fn timestamp() -> f64 {
    static ORIGIN: OnceLock<(f64, Instant)> = OnceLock::new();
    let (wall, started) = ORIGIN.get_or_init(|| {
        let wall = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0.0, |since| since.as_secs_f64());
        (wall, Instant::now())
    });
    wall + started.elapsed().as_secs_f64()
}

fn metric(name: &str, value: f64) -> Value {
    json!({"name": name, "value": value})
}

pub async fn handle(
    method: &str,
    params: &Value,
    ctx: &mut CdpContext,
    session_id: &Option<String>,
) -> Result<Value, String> {
    match method {
        "enable" => {
            match params.get("timeDomain") {
                None | Some(Value::Null) => {}
                Some(domain) => match domain.as_str() {
                    Some("timeTicks") => {}
                    Some("threadTicks") => return Err("Thread ticks are not supported".to_string()),
                    _ => return Err(format!("Invalid timeDomain: {domain}")),
                },
            }
            let session = session_id
                .as_ref()
                .ok_or("Performance.enable requires a page session")?;
            ctx.get_session_page(session_id).ok_or("No page")?;
            ctx.performance_sessions.insert(session.clone());
            Ok(json!({}))
        }
        "disable" => {
            let session = session_id
                .as_ref()
                .ok_or("Performance.disable requires a page session")?;
            ctx.performance_sessions.remove(session);
            Ok(json!({}))
        }
        "getMetrics" => {
            let enabled = session_id
                .as_ref()
                .is_some_and(|session| ctx.performance_sessions.contains(session));
            let page = ctx.get_session_page_mut(session_id).ok_or("No page")?;
            if !enabled {
                return Ok(json!({"metrics": []}));
            }
            let timestamp = timestamp();
            let measured = page
                .js
                .as_mut()
                .ok_or("No JavaScript runtime")?
                .page_metrics();
            Ok(json!({"metrics": [
                metric("Timestamp", timestamp),
                metric("Documents", measured.documents as f64),
                metric("Frames", measured.frames as f64),
                metric("JSEventListeners", measured.js_event_listeners as f64),
                metric("Nodes", measured.nodes as f64),
                metric("LayoutCount", measured.layout_count as f64),
                metric("RecalcStyleCount", measured.recalc_style_count as f64),
                metric("LayoutDuration", measured.layout_duration.as_secs_f64()),
                metric("RecalcStyleDuration", measured.recalc_style_duration.as_secs_f64()),
                metric("ScriptDuration", measured.script_duration.as_secs_f64()),
                metric("TaskDuration", measured.task_duration.as_secs_f64()),
                metric("JSHeapUsedSize", measured.js_heap_used_size as f64),
                metric("JSHeapTotalSize", measured.js_heap_total_size as f64),
            ]}))
        }
        _ => Err(format!("'Performance.{method}' wasn't found")),
    }
}
