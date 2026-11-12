//! A page script that runs too long is stopped at the page's per-script limit
//! (`Page::set_script_timeout`), the page's other scripts still run, and the
//! stop is reported through `Page::take_script_warnings`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use inbrowse::{BrowserContext, Page};
use serde_json::json;

fn page() -> Page {
    Page::new("script-limits".into(), Arc::new(BrowserContext::new("script-limits".into())))
}

#[tokio::test(flavor = "current_thread")]
async fn a_runaway_inline_script_is_stopped_and_reported() {
    let mut page = page();
    page.set_script_timeout(Duration::from_millis(200));
    // A first document pays the process's one-time engine start-up, which on
    // a loaded host is seconds; the bound below is for stopping the script.
    page.navigate("data:text/html,<p>warm</p>").await.unwrap();
    let started = Instant::now();
    page.navigate("data:text/html,<script>while(true){}</script><script>document.title='after'</script>")
        .await
        .unwrap();
    assert!(started.elapsed() < Duration::from_secs(5), "took {:?}", started.elapsed());
    assert_eq!(page.evaluate("document.title").unwrap(), json!("after"), "later scripts still run");
    let warnings = page.take_script_warnings();
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(warnings[0].contains("inline script") && warnings[0].contains("200 ms"), "{warnings:?}");
    assert!(page.take_script_warnings().is_empty(), "taking the warnings empties them");
}

#[tokio::test(flavor = "current_thread")]
async fn a_new_document_starts_without_the_previous_warnings() {
    let mut page = page();
    page.set_script_timeout(Duration::from_millis(100));
    page.navigate("data:text/html,<script>while(true){}</script>").await.unwrap();
    page.navigate("data:text/html,<p>quiet</p>").await.unwrap();
    assert!(page.take_script_warnings().is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn a_thrown_error_is_not_a_time_limit_warning() {
    let mut page = page();
    page.navigate("data:text/html,<script>throw new Error('boom')</script>").await.unwrap();
    assert!(page.take_script_warnings().is_empty());
}
