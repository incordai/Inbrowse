//! Emulation.setTimezoneOverride is per page: the page's Date and Intl follow
//! the override, an empty id restores the process zone, an invalid id is an
//! error, and no other page or CDP connection in the process sees it.
//!
//! Its own test binary: TZ must be set before the first isolate is created.

use inbrowse_cdp::dispatch::{dispatch, CdpContext};
use inbrowse_cdp::types::{CdpRequest, CdpResponse};
use serde_json::{json, Value};

const PROBE: &str = "[Intl.DateTimeFormat().resolvedOptions().timeZone, \
    new Date(2026, 0, 15).getTimezoneOffset(), \
    new Date(0).toString().includes('GMT+0900')]";

async fn cdp(
    ctx: &mut CdpContext,
    id: u64,
    method: &str,
    params: Value,
    session_id: Option<&str>,
) -> CdpResponse {
    dispatch(
        &CdpRequest {
            id,
            method: method.to_string(),
            params,
            session_id: session_id.map(str::to_string),
        },
        ctx,
    )
    .await
}

async fn open_page(ctx: &mut CdpContext, id: u64) -> String {
    let created = cdp(
        ctx,
        id,
        "Target.createTarget",
        json!({ "url": "data:text/html,<p>zone</p>" }),
        None,
    )
    .await;
    let target = created.result.unwrap()["targetId"].as_str().unwrap().to_string();
    let attached = cdp(
        ctx,
        id + 1,
        "Target.attachToTarget",
        json!({ "targetId": target, "flatten": true }),
        None,
    )
    .await;
    attached.result.unwrap()["sessionId"].as_str().unwrap().to_string()
}

async fn probe(ctx: &mut CdpContext, id: u64, session: &str) -> Value {
    let response = cdp(
        ctx,
        id,
        "Runtime.evaluate",
        json!({ "expression": PROBE, "returnByValue": true }),
        Some(session),
    )
    .await;
    assert!(response.error.is_none(), "evaluate failed: {:?}", response.error);
    response.result.unwrap()["result"]["value"].clone()
}

async fn set_zone(ctx: &mut CdpContext, id: u64, session: &str, zone: &str) -> CdpResponse {
    cdp(
        ctx,
        id,
        "Emulation.setTimezoneOverride",
        json!({ "timezoneId": zone }),
        Some(session),
    )
    .await
}

fn use_process_zone() {
    std::env::set_var("TZ", "America/New_York");
}

#[tokio::test(flavor = "current_thread")]
async fn override_is_per_page_and_survives_navigation() {
    use_process_zone();
    let mut ctx = CdpContext::new();
    let tokyo = open_page(&mut ctx, 1).await;
    let plain = open_page(&mut ctx, 10).await;
    assert_eq!(probe(&mut ctx, 20, &plain).await, json!(["America/New_York", 300, false]));

    let set = set_zone(&mut ctx, 21, &tokyo, "Asia/Tokyo").await;
    assert!(set.error.is_none(), "set failed: {:?}", set.error);

    assert_eq!(probe(&mut ctx, 22, &tokyo).await, json!(["Asia/Tokyo", -540, true]));
    // The other page on the same connection keeps the process zone, and the
    // overridden page keeps its own when the pages alternate.
    assert_eq!(probe(&mut ctx, 23, &plain).await, json!(["America/New_York", 300, false]));
    assert_eq!(probe(&mut ctx, 24, &tokyo).await, json!(["Asia/Tokyo", -540, true]));

    let navigated = cdp(
        &mut ctx,
        25,
        "Page.navigate",
        json!({ "url": "data:text/html,<p>again</p>" }),
        Some(&tokyo),
    )
    .await;
    assert!(navigated.error.is_none(), "navigate failed: {:?}", navigated.error);
    assert_eq!(probe(&mut ctx, 26, &tokyo).await, json!(["Asia/Tokyo", -540, true]));
}

#[tokio::test(flavor = "current_thread")]
async fn empty_id_restores_the_process_zone() {
    use_process_zone();
    let mut ctx = CdpContext::new();
    let page = open_page(&mut ctx, 1).await;
    assert!(set_zone(&mut ctx, 10, &page, "Pacific/Auckland").await.error.is_none());
    assert_eq!(probe(&mut ctx, 11, &page).await[0], json!("Pacific/Auckland"));
    assert!(set_zone(&mut ctx, 12, &page, "").await.error.is_none());
    assert_eq!(probe(&mut ctx, 13, &page).await, json!(["America/New_York", 300, false]));
}

#[tokio::test(flavor = "current_thread")]
async fn invalid_id_is_an_error_and_changes_nothing() {
    use_process_zone();
    let mut ctx = CdpContext::new();
    let page = open_page(&mut ctx, 1).await;
    assert!(set_zone(&mut ctx, 10, &page, "Asia/Tokyo").await.error.is_none());
    let rejected = set_zone(&mut ctx, 11, &page, "Foo/Bar").await;
    let message = rejected.error.expect("invalid id must fail").message;
    assert!(message.contains("Invalid timezone ID"), "unexpected error: {message}");
    assert_eq!(probe(&mut ctx, 12, &page).await, json!(["Asia/Tokyo", -540, true]));
}

/// Two connections run on their own threads, as `serve` does. One overrides its
/// page while the other keeps the process zone; neither may ever read the
/// other's zone, however their work interleaves.
#[test]
fn concurrent_connections_never_see_each_others_zone() {
    use_process_zone();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let (done, finished) = std::sync::mpsc::channel();
    let run = |zone: Option<&'static str>, expected: Value| {
        let barrier = barrier.clone();
        let done = done.clone();
        std::thread::spawn(move || {
            let outcome = std::panic::catch_unwind(|| {
                let runtime =
                    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
                runtime.block_on(async {
                    let mut ctx = CdpContext::new();
                    let page = open_page(&mut ctx, 1).await;
                    if let Some(zone) = zone {
                        assert!(set_zone(&mut ctx, 10, &page, zone).await.error.is_none());
                    }
                    barrier.wait();
                    for round in 0..150 {
                        assert_eq!(
                            probe(&mut ctx, 100 + round, &page).await,
                            expected,
                            "round {round}"
                        );
                    }
                });
            });
            let _ = done.send(outcome.is_ok());
        });
    };
    run(Some("Asia/Tokyo"), json!(["Asia/Tokyo", -540, true]));
    run(None, json!(["America/New_York", 300, false]));
    // A lock cycle in the zone gate would hang here; fail instead of waiting.
    for _ in 0..2 {
        let ok = finished
            .recv_timeout(std::time::Duration::from_secs(30))
            .expect("connections deadlocked in the zone gate");
        assert!(ok, "a connection saw the wrong zone");
    }
}
