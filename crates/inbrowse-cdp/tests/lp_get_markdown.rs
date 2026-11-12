use inbrowse_cdp::dispatch::{dispatch, CdpContext};
use inbrowse_cdp::types::CdpRequest;
use serde_json::{json, Value};

async fn page() -> (CdpContext, String) {
    let mut ctx = CdpContext::new();
    let page = ctx.create_page();
    let session = format!("{page}-session");
    ctx.sessions.insert(session.clone(), page);
    inbrowse_cdp::domains::page::handle(
        "navigate",
        &json!({"url": "data:text/html,<nav><a href=https://x.test/n>Menu</a></nav>\
            <article><header><h1>Title</h1></header><p>Body</p></article><footer>Foot</footer>"}),
        &mut ctx,
        &Some(session.clone()),
    )
    .await
    .unwrap();
    (ctx, session)
}

async fn get_markdown(ctx: &mut CdpContext, session: &str, params: Value) -> Result<String, String> {
    let response = dispatch(
        &CdpRequest {
            id: 1,
            method: "LP.getMarkdown".to_string(),
            params,
            session_id: Some(session.to_string()),
        },
        ctx,
    )
    .await;
    match response.error {
        Some(error) => Err(error.message),
        None => Ok(response.result.unwrap()["markdown"].as_str().unwrap().to_string()),
    }
}

#[tokio::test(flavor = "current_thread")]
async fn markdown_leaves_out_page_boilerplate_by_default() {
    let (mut ctx, session) = page().await;
    let expected = "# Title\n\nBody";
    assert_eq!(get_markdown(&mut ctx, &session, json!({})).await.unwrap(), expected);
    assert_eq!(get_markdown(&mut ctx, &session, json!({"boilerplate": "drop"})).await.unwrap(), expected);
}

#[tokio::test(flavor = "current_thread")]
async fn markdown_keeps_the_whole_page_when_asked() {
    let (mut ctx, session) = page().await;
    assert_eq!(
        get_markdown(&mut ctx, &session, json!({"boilerplate": "keep"})).await.unwrap(),
        "[Menu](https://x.test/n)\n\n# Title\n\nBody\n\nFoot"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn markdown_rejects_an_unknown_boilerplate_choice() {
    let (mut ctx, session) = page().await;
    let error = get_markdown(&mut ctx, &session, json!({"boilerplate": "none"})).await.unwrap_err();
    assert!(error.contains("boilerplate must be"), "{error}");
}
