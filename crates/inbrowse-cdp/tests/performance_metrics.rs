//! `Performance.getMetrics` returns Chrome's metric set with measured values,
//! and only while the domain is enabled for the calling session.

use std::collections::HashMap;

use inbrowse_cdp::dispatch::{dispatch, CdpContext};
use inbrowse_cdp::types::CdpRequest;
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const SESSION: &str = "perf-session";

async fn call(ctx: &mut CdpContext, session: &str, method: &str, params: Value) -> Result<Value, String> {
    let response = dispatch(
        &CdpRequest {
            id: 1,
            method: method.to_string(),
            params,
            session_id: Some(session.to_string()),
        },
        ctx,
    )
    .await;
    match response.error {
        Some(error) => Err(error.message),
        None => Ok(response.result.expect("result")),
    }
}

async fn cdp(ctx: &mut CdpContext, method: &str, params: Value) -> Value {
    call(ctx, SESSION, method, params).await.unwrap_or_else(|error| panic!("{method}: {error}"))
}

async fn setup() -> CdpContext {
    let mut ctx = CdpContext::new();
    let page = ctx.create_page();
    ctx.sessions.insert(SESSION.to_string(), page);
    cdp(&mut ctx, "Page.navigate", json!({"url": "about:blank"})).await;
    ctx
}

async fn eval(ctx: &mut CdpContext, expression: &str) -> Value {
    cdp(ctx, "Runtime.evaluate", json!({"expression": expression, "returnByValue": true})).await
}

async fn metrics(ctx: &mut CdpContext) -> HashMap<String, f64> {
    let result = cdp(ctx, "Performance.getMetrics", json!({})).await;
    result["metrics"]
        .as_array()
        .unwrap_or_else(|| panic!("no metrics array: {result}"))
        .iter()
        .map(|metric| (metric["name"].as_str().unwrap().to_string(), metric["value"].as_f64().unwrap()))
        .collect()
}

#[tokio::test(flavor = "current_thread")]
async fn disabled_domain_returns_no_metrics_until_enabled() {
    let mut ctx = setup().await;
    let before = cdp(&mut ctx, "Performance.getMetrics", json!({})).await;
    assert_eq!(before, json!({"metrics": []}));

    assert_eq!(cdp(&mut ctx, "Performance.enable", json!({})).await, json!({}));
    assert!(!metrics(&mut ctx).await.is_empty());

    assert_eq!(cdp(&mut ctx, "Performance.disable", json!({})).await, json!({}));
    assert_eq!(cdp(&mut ctx, "Performance.getMetrics", json!({})).await, json!({"metrics": []}));
}

#[tokio::test(flavor = "current_thread")]
async fn enabling_is_per_session() {
    let mut ctx = setup().await;
    let page = ctx.sessions.get(SESSION).unwrap().clone();
    ctx.sessions.insert("second-session".to_string(), page);
    cdp(&mut ctx, "Performance.enable", json!({})).await;
    let other = call(&mut ctx, "second-session", "Performance.getMetrics", json!({})).await.unwrap();
    assert_eq!(other, json!({"metrics": []}), "a session that did not enable the domain collects nothing");
}

#[tokio::test(flavor = "current_thread")]
async fn rejects_unsupported_time_domain_and_unknown_methods() {
    let mut ctx = setup().await;
    assert!(call(&mut ctx, SESSION, "Performance.enable", json!({"timeDomain": "timeTicks"})).await.is_ok());
    assert!(call(&mut ctx, SESSION, "Performance.enable", json!({"timeDomain": "bogus"})).await.is_err());
    let error = call(&mut ctx, SESSION, "Performance.bogus", json!({})).await.unwrap_err();
    assert_eq!(error, "'Performance.bogus' wasn't found");
}

#[tokio::test(flavor = "current_thread")]
async fn reports_the_metrics_page_metrics_reads() {
    let mut ctx = setup().await;
    eval(
        &mut ctx,
        "for (let i = 0; i < 40; i++) document.body.appendChild(document.createElement('p'));",
    )
    .await;
    cdp(&mut ctx, "Performance.enable", json!({})).await;
    let reported = metrics(&mut ctx).await;
    for name in [
        "Timestamp",
        "Documents",
        "Frames",
        "JSEventListeners",
        "Nodes",
        "LayoutCount",
        "RecalcStyleCount",
        "LayoutDuration",
        "RecalcStyleDuration",
        "ScriptDuration",
        "TaskDuration",
        "JSHeapUsedSize",
        "JSHeapTotalSize",
    ] {
        assert!(reported.contains_key(name), "{name} missing from {reported:?}");
    }
    assert!(reported["Timestamp"] > 0.0);
    assert_eq!(reported["Documents"], 1.0);
    assert_eq!(reported["Frames"], 1.0);
    assert!(reported["Nodes"] >= 40.0, "Nodes {}", reported["Nodes"]);
    assert!(reported["JSHeapUsedSize"] > 0.0);
    assert!(reported["JSHeapTotalSize"] >= reported["JSHeapUsedSize"]);
    assert!(reported["TaskDuration"] >= reported["ScriptDuration"]);
    for (name, value) in &reported {
        assert!(value.is_finite() && *value >= 0.0, "{name} = {value}");
    }
}

#[tokio::test(flavor = "current_thread")]
async fn timestamp_advances_and_node_count_follows_the_dom() {
    let mut ctx = setup().await;
    cdp(&mut ctx, "Performance.enable", json!({})).await;
    let first = metrics(&mut ctx).await;
    eval(&mut ctx, "for (let i = 0; i < 25; i++) document.body.appendChild(document.createElement('div'));").await;
    let second = metrics(&mut ctx).await;
    assert!(second["Timestamp"] > first["Timestamp"]);
    assert_eq!(second["Nodes"] - first["Nodes"], 25.0);
}

#[tokio::test(flavor = "current_thread")]
async fn event_listeners_are_counted_as_added_and_removed() {
    let mut ctx = setup().await;
    cdp(&mut ctx, "Performance.enable", json!({})).await;
    let base = metrics(&mut ctx).await["JSEventListeners"];
    eval(
        &mut ctx,
        "globalThis.fn = () => {}; globalThis.b = document.createElement('button'); \
         b.addEventListener('click', fn); b.addEventListener('focus', fn); \
         b.addEventListener('focus', fn); window.addEventListener('resize', fn);",
    )
    .await;
    assert_eq!(metrics(&mut ctx).await["JSEventListeners"] - base, 3.0, "a repeated registration is one listener");
    eval(&mut ctx, "b.removeEventListener('click', fn); b.removeEventListener('click', fn);").await;
    assert_eq!(metrics(&mut ctx).await["JSEventListeners"] - base, 2.0);
}

#[tokio::test(flavor = "current_thread")]
async fn script_and_task_duration_grow_with_script_work() {
    let mut ctx = setup().await;
    cdp(&mut ctx, "Performance.enable", json!({})).await;
    let before = metrics(&mut ctx).await;
    eval(&mut ctx, "const end = Date.now() + 60; while (Date.now() < end) {}").await;
    let after = metrics(&mut ctx).await;
    let script = after["ScriptDuration"] - before["ScriptDuration"];
    assert!((0.05..5.0).contains(&script), "ScriptDuration grew by {script}s");
    assert!(after["TaskDuration"] - before["TaskDuration"] >= script);
}

#[cfg(feature = "render")]
#[tokio::test(flavor = "current_thread")]
async fn layout_and_style_counters_follow_forced_layout() {
    let mut ctx = setup().await;
    eval(&mut ctx, "document.body.innerHTML = '<div id=a style=\"width:10px\">x</div>'").await;
    cdp(&mut ctx, "Performance.enable", json!({})).await;
    eval(&mut ctx, "document.getElementById('a').getBoundingClientRect()").await;
    let before = metrics(&mut ctx).await;
    eval(&mut ctx, "document.getElementById('a').style.width = '30px'; document.getElementById('a').getBoundingClientRect()").await;
    let after = metrics(&mut ctx).await;
    assert!(after["LayoutCount"] > before["LayoutCount"], "{before:?} -> {after:?}");
    assert!(after["RecalcStyleCount"] > before["RecalcStyleCount"]);
    assert!(after["LayoutDuration"] > before["LayoutDuration"]);
}

async fn serve() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut buf = [0u8; 2048];
                let read = socket.read(&mut buf).await.unwrap_or(0);
                let request = String::from_utf8_lossy(&buf[..read]).to_string();
                let body = if request.starts_with("GET /child.html ") {
                    "<html><body><p>one</p><p>two</p></body></html>"
                } else {
                    "<html><body><iframe src=\"/child.html\"></iframe></body></html>"
                };
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = socket.write_all(response.as_bytes()).await;
            });
        }
    });
    format!("http://{addr}/")
}

#[tokio::test(flavor = "current_thread")]
async fn child_frames_count_as_frames_documents_and_nodes() {
    std::env::set_var("INBROWSE_ALLOW_PRIVATE_NETWORK", "1");
    let url = serve().await;
    let mut ctx = CdpContext::new();
    let page = ctx.create_page();
    ctx.sessions.insert(SESSION.to_string(), page);
    cdp(&mut ctx, "Page.navigate", json!({"url": url})).await;
    cdp(&mut ctx, "Performance.enable", json!({})).await;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    let reported = loop {
        eval(&mut ctx, "1").await;
        let reported = metrics(&mut ctx).await;
        if reported["Frames"] == 2.0 || std::time::Instant::now() >= deadline {
            break reported;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    };
    assert_eq!(reported["Frames"], 2.0);
    assert_eq!(reported["Documents"], 2.0);
    eval(&mut ctx, "document.querySelector('iframe').remove()").await;
    // A removed frame is torn down by the page's own event loop turns.
    for _ in 0..3 {
        ctx.get_session_page_mut(&Some(SESSION.to_string()))
            .unwrap()
            .run_autonomous_event_loop_turn()
            .await
            .unwrap();
    }
    let removed = metrics(&mut ctx).await;
    assert_eq!(removed["Frames"], 1.0);
    assert_eq!(removed["Documents"], 1.0);
    assert!(
        reported["Nodes"] - removed["Nodes"] >= 5.0,
        "the child document's nodes are counted while it lives: {} -> {}",
        reported["Nodes"],
        removed["Nodes"]
    );
}
