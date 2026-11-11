#![cfg(feature = "render")]

use std::io::{Read, Write};
use std::sync::Arc;

use inbrowse::{BrowserContext, Page};

fn serve(body: &'static str) -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for incoming in listener.incoming() {
            let Ok(mut stream) = incoming else { continue };
            let mut buf = [0u8; 4096];
            let _ = stream.read(&mut buf);
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    format!("http://{address}/")
}

async fn load(body: &'static str) -> Page {
    std::env::set_var("INBROWSE_ALLOW_PRIVATE_NETWORK", "1");
    let url = serve(body);
    let ctx = Arc::new(BrowserContext::with_storage_and_network(
        "frame-viewport".into(),
        None,
        false,
        None,
        None,
        true,
    ));
    let mut page = Page::new("frame-viewport-page".into(), ctx);
    page.set_viewport((800.0, 600.0));
    page.navigate(&url).await.unwrap();
    page.settle(1_000).await;
    page
}

const SRCDOC_FRAME: &str = concat!(
    "<body style='margin:0'><iframe id='f' style='width:500px;height:200px;border:0' ",
    "srcdoc=\"<body><script>window.resizes=[];",
    "addEventListener('resize',()=>resizes.push([innerWidth,innerHeight]));",
    "</script></body>\"></iframe></body>"
);

#[tokio::test(flavor = "current_thread")]
async fn frame_viewport_follows_the_iframe_content_box() {
    let mut page = load(SRCDOC_FRAME).await;
    let value = page
        .evaluate_script(
            r#"(() => {
                const w = document.getElementById('f').contentWindow;
                return [w.innerWidth, w.innerHeight,
                    w.matchMedia('(min-width:500px)').matches,
                    w.matchMedia('(min-width:501px)').matches,
                    w.document.documentElement.clientWidth, w.resizes.length];
            })()"#,
        )
        .unwrap();
    assert_eq!(value, serde_json::json!([500, 200, true, false, 500, 0]));
}

#[tokio::test(flavor = "current_thread")]
async fn resizing_the_iframe_updates_the_frame_and_fires_resize() {
    let mut page = load(SRCDOC_FRAME).await;
    page.evaluate_script(
        "(() => { const f = document.getElementById('f'); f.style.width='320px'; f.style.height='100px'; return 1; })()",
    )
    .unwrap();
    page.settle(1_000).await;
    let value = page
        .evaluate_script(
            r#"(() => {
                const w = document.getElementById('f').contentWindow;
                return [w.innerWidth, w.innerHeight, w.matchMedia('(max-width:320px)').matches,
                    w.resizes];
            })()"#,
        )
        .unwrap();
    assert_eq!(value, serde_json::json!([320, 100, true, [[320, 100]]]));

    // An unchanged box fires nothing further.
    page.settle(500).await;
    let again = page
        .evaluate_script("document.getElementById('f').contentWindow.resizes.length")
        .unwrap();
    assert_eq!(again, serde_json::json!(1));
}

#[tokio::test(flavor = "current_thread")]
async fn onresize_handler_and_listener_each_run_once_per_resize() {
    let mut page = load(concat!(
        "<body style='margin:0'><iframe id='f' style='width:300px;height:100px;border:0' ",
        "srcdoc=\"<body><script>window.calls=[];window.onresize=()=>calls.push('on');",
        "addEventListener('resize',()=>calls.push('listener'));</script></body>\"></iframe></body>"
    ))
    .await;
    page.evaluate_script("document.getElementById('f').style.width='250px'; 1")
        .unwrap();
    page.settle(1_000).await;
    let calls = page
        .evaluate_script("document.getElementById('f').contentWindow.calls")
        .unwrap();
    assert_eq!(calls, serde_json::json!(["on", "listener"]));
}

#[tokio::test(flavor = "current_thread")]
async fn percentage_sized_frames_follow_the_parent_viewport() {
    let mut page = load(concat!(
        "<body style='margin:0'><iframe id='f' style='width:50%;height:25%;border:0' ",
        "srcdoc=\"<body></body>\"></iframe></body>"
    ))
    .await;
    // The page viewport is 800x600; 25% of an auto-height body is not definite,
    // so only the width is asserted.
    let width = page
        .evaluate_script("document.getElementById('f').contentWindow.innerWidth")
        .unwrap();
    assert_eq!(width, serde_json::json!(400));
}
