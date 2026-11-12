//! A document's load event waits for its child frames: their scripts run, and
//! their own load events fire, before the parent's `load` handler runs.

use std::sync::Arc;

use inbrowse::{BrowserContext, Page};
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const PARENT: &str = "<!doctype html>\
    <iframe id=src src=\"/child.html\"></iframe>\
    <iframe id=doc srcdoc=\"<p id=t>from srcdoc</p><script>document.getElementById('t').textContent += '!'</script>\"></iframe>\
    <script>\
    window.addEventListener('load', () => {\
      const text = id => document.getElementById(id).contentDocument.getElementById('t').textContent;\
      window.__atLoad = [text('src'), text('doc')].join('|');
      const frame = document.getElementById('src');
      window.__links = [frame.contentWindow.parent === window, frame.contentWindow.top === window,
        frame.contentWindow.frameElement === frame].join();\
    });\
    </script>";

const CHILD: &str = "<!doctype html><p id=t>from src</p>\
    <script>document.getElementById('t').textContent += ' (true)'</script>";

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
                    CHILD
                } else {
                    PARENT
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
async fn frame_scripts_run_before_the_parents_load_event() {
    std::env::set_var("INBROWSE_ALLOW_PRIVATE_NETWORK", "1");
    let url = serve().await;
    let mut page = Page::new(
        "load-order".into(),
        Arc::new(BrowserContext::new("load-order".into())),
    );
    page.navigate(&url).await.unwrap();
    assert_eq!(
        page.evaluate("window.__atLoad").unwrap(),
        json!("from src (true)|from srcdoc!"),
        "the frames' scripts had not run when the parent's load fired"
    );
    // A same-origin frame's window relates to the page by identity.
    assert_eq!(
        page.evaluate("window.__links").unwrap(),
        json!("true,true,true")
    );
}

fn page() -> Page {
    Page::new(
        "blank-frames".into(),
        Arc::new(BrowserContext::new("blank-frames".into())),
    )
}

#[tokio::test(flavor = "current_thread")]
async fn parser_created_blank_iframes_are_frames_without_a_script_touching_them() {
    let mut page = page();
    page.navigate(
        "data:text/html,<iframe></iframe><iframe src='about:blank'></iframe><p>no frame here</p>",
    )
    .await
    .unwrap();
    page.adopt_pending_frames().await;
    assert_eq!(page.frame_urls(), vec!["about:blank", "about:blank"]);
}

#[tokio::test(flavor = "current_thread")]
async fn a_script_created_iframe_is_a_frame_with_a_document_at_once() {
    let mut page = page();
    page.navigate("data:text/html,<p>no frames yet</p>")
        .await
        .unwrap();
    assert!(page.frames.is_empty());
    assert_eq!(
        page.evaluate(
            "const f = document.createElement('iframe'); document.body.appendChild(f); \
             [f.contentDocument !== null, f.contentDocument.URL, f.contentWindow.parent === window].join()"
        )
        .unwrap(),
        json!("true,about:blank,true")
    );
    page.adopt_pending_frames().await;
    assert_eq!(page.frame_urls(), vec!["about:blank"]);
}

#[tokio::test(flavor = "current_thread")]
async fn a_page_without_iframes_has_no_frame_work() {
    let mut page = page();
    page.navigate("data:text/html,<p>plain</p>").await.unwrap();
    page.adopt_pending_frames().await;
    assert!(page.frames.is_empty());
}
