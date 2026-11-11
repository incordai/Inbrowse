//! `Page.printToPDF` header and footer dates use the page's time zone, the one
//! its own `Date` reports, not UTC.

#![cfg(feature = "render")]

use base64::Engine;
use inbrowse_cdp::dispatch::{dispatch, CdpContext};
use inbrowse_cdp::types::CdpRequest;
use serde_json::{json, Value};

const FORMAT: &str =
    "{year:'2-digit', month:'numeric', day:'numeric', hour:'numeric', minute:'2-digit'}";

async fn cdp(ctx: &mut CdpContext, method: &str, params: Value) -> Value {
    let response = dispatch(
        &CdpRequest {
            id: 1,
            method: method.to_string(),
            params,
            session_id: Some("pdf-session".to_string()),
        },
        ctx,
    )
    .await;
    assert!(response.error.is_none(), "{method}: {:?}", response.error);
    response.result.unwrap()
}

async fn page_date(ctx: &mut CdpContext, zone: Option<&str>) -> String {
    let options = match zone {
        Some(zone) => format!("Object.assign({FORMAT}, {{timeZone: '{zone}'}})"),
        None => FORMAT.to_string(),
    };
    let result = cdp(
        ctx,
        "Runtime.evaluate",
        json!({"expression": format!("new Date().toLocaleString('en-US', {options})"), "returnByValue": true}),
    )
    .await;
    // Newer ICU separates the time and the day period with a narrow no-break space.
    result["result"]["value"].as_str().unwrap().replace('\u{202f}', " ")
}

#[tokio::test(flavor = "current_thread")]
async fn header_date_is_formatted_in_the_process_zone() {
    // UTC-6 all year; set before the first runtime so the pinned ICU zone sees it.
    std::env::set_var("TZ", "Etc/GMT+6");
    let mut ctx = CdpContext::new();
    let page = ctx.create_page();
    ctx.sessions.insert("pdf-session".to_string(), page);
    cdp(&mut ctx, "Page.navigate", json!({"url": "about:blank"})).await;
    cdp(
        &mut ctx,
        "Runtime.evaluate",
        json!({"expression": "document.body.innerHTML = '<h1>Dated</h1>'"}),
    )
    .await;

    let local_before = page_date(&mut ctx, None).await;
    let utc_before = page_date(&mut ctx, Some("UTC")).await;
    assert_ne!(local_before, utc_before, "the test zone must differ from UTC");

    let printed = cdp(
        &mut ctx,
        "Page.printToPDF",
        json!({
            "displayHeaderFooter": true,
            "headerTemplate": "<div style=\"font-size:10px\"><span class=\"date\"></span></div>",
            "footerTemplate": "<div style=\"font-size:10px\"><span class=\"pageNumber\"></span></div>",
        }),
    )
    .await;
    let local_after = page_date(&mut ctx, None).await;
    let utc_after = page_date(&mut ctx, Some("UTC")).await;
    let pdf = base64::engine::general_purpose::STANDARD
        .decode(printed["data"].as_str().expect("base64 pdf"))
        .unwrap();
    let raw = String::from_utf8_lossy(&pdf);

    // The clock may cross a minute while the PDF is produced; either side matches.
    let local = [&local_before, &local_after].iter().any(|date| raw.contains(&format!("({date}) Tj")));
    assert!(local, "header shows neither {local_before:?} nor {local_after:?} (local time)");
    let utc = [&utc_before, &utc_after].iter().any(|date| raw.contains(&format!("({date}) Tj")));
    assert!(!utc, "header must not show the UTC time {utc_before:?}");
}

#[tokio::test(flavor = "current_thread")]
async fn header_date_follows_the_pages_timezone_override() {
    std::env::set_var("TZ", "Etc/GMT+6");
    let mut ctx = CdpContext::new();
    let page = ctx.create_page();
    ctx.sessions.insert("pdf-session".to_string(), page);
    cdp(&mut ctx, "Page.navigate", json!({"url": "about:blank"})).await;
    cdp(&mut ctx, "Runtime.evaluate", json!({"expression": "document.body.innerHTML = '<h1>Dated</h1>'"})).await;

    let process_zone = page_date(&mut ctx, None).await;
    cdp(&mut ctx, "Emulation.setTimezoneOverride", json!({"timezoneId": "Asia/Kolkata"})).await;
    let override_before = page_date(&mut ctx, None).await;
    assert_ne!(override_before, process_zone, "the override must change the page's local time");

    let printed = cdp(
        &mut ctx,
        "Page.printToPDF",
        json!({
            "displayHeaderFooter": true,
            "headerTemplate": "<div style=\"font-size:10px\"><span class=\"date\"></span></div>",
            "footerTemplate": "<div style=\"font-size:10px\"><span class=\"pageNumber\"></span></div>",
        }),
    )
    .await;
    let override_after = page_date(&mut ctx, None).await;
    let pdf = base64::engine::general_purpose::STANDARD
        .decode(printed["data"].as_str().expect("base64 pdf"))
        .unwrap();
    let raw = String::from_utf8_lossy(&pdf);
    assert!(
        [&override_before, &override_after].iter().any(|date| raw.contains(&format!("({date}) Tj"))),
        "header shows neither {override_before:?} nor {override_after:?} (override zone)"
    );
    assert!(!raw.contains(&format!("({process_zone}) Tj")), "header must not show the process zone time {process_zone:?}");
}
