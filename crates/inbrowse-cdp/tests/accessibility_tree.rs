//! The Accessibility domain must return one connected tree that tree-building
//! clients (Puppeteer and Playwright snapshots) can walk from its first node.

use inbrowse_cdp::dispatch::{dispatch, CdpContext};
use inbrowse_cdp::types::CdpRequest;
use serde_json::{json, Value};

const PAGE: &str = r#"<h1>Static Heading</h1>
    <a href="/x">Link</a>
    <ul><li>One</li><li>Two</li></ul>
    <form><label for="q">Query</label><input id="q" type="text"><button id="go">Go</button></form>
    <div id="hid" aria-hidden="true"><button>Secret</button></div>
    <div id="pres" role="presentation"><span>decor</span></div>
    <p hidden>not rendered</p>"#;

async fn call(ctx: &mut CdpContext, method: &str, params: Value) -> Result<Value, String> {
    let response = dispatch(
        &CdpRequest {
            id: 1,
            method: method.to_string(),
            params,
            session_id: Some("ax-session".to_string()),
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
    call(ctx, method, params).await.unwrap_or_else(|error| panic!("{method}: {error}"))
}

async fn setup() -> (CdpContext, String) {
    let mut ctx = CdpContext::new();
    let page = ctx.create_page();
    ctx.sessions.insert("ax-session".to_string(), page);
    cdp(&mut ctx, "Page.navigate", json!({"url": "about:blank"})).await;
    cdp(
        &mut ctx,
        "Runtime.evaluate",
        json!({
            "expression": format!(
                "document.title = 'Tree Page'; document.body.innerHTML = {}",
                serde_json::to_string(PAGE).unwrap()
            ),
            "returnByValue": true
        }),
    )
    .await;
    let tree = cdp(&mut ctx, "Page.getFrameTree", json!({})).await;
    let frame = tree["frameTree"]["frame"]["id"].as_str().unwrap().to_string();
    (ctx, frame)
}

fn by_id<'a>(nodes: &'a [Value], id: &Value) -> &'a Value {
    nodes
        .iter()
        .find(|node| node["nodeId"] == *id)
        .unwrap_or_else(|| panic!("no node {id}"))
}

fn by_backend<'a>(nodes: &'a [Value], backend: u64) -> &'a Value {
    nodes
        .iter()
        .find(|node| node["backendDOMNodeId"] == backend)
        .unwrap_or_else(|| panic!("no node for backend id {backend}"))
}

async fn backend_of(ctx: &mut CdpContext, selector: &str) -> u64 {
    let found = cdp(ctx, "DOM.querySelector", json!({"nodeId": 0, "selector": selector})).await;
    found["nodeId"].as_u64().filter(|id| *id != 0).unwrap_or_else(|| panic!("{selector} not found"))
}

fn property<'a>(node: &'a Value, name: &str) -> Option<&'a Value> {
    node["properties"]
        .as_array()?
        .iter()
        .find(|property| property["name"] == name)
        .map(|property| &property["value"]["value"])
}

fn reasons(node: &Value) -> Vec<String> {
    node["ignoredReasons"]
        .as_array()
        .map(|reasons| {
            reasons.iter().map(|reason| reason["name"].as_str().unwrap().to_string()).collect()
        })
        .unwrap_or_default()
}

fn assert_connected(nodes: &[Value]) {
    assert_eq!(
        nodes.iter().filter(|node| node.get("parentId").is_none()).count(),
        1,
        "exactly one root: {nodes:?}"
    );
    let mut seen = std::collections::HashSet::new();
    for node in nodes {
        assert!(seen.insert(node["nodeId"].to_string()), "duplicate nodeId {}", node["nodeId"]);
        assert!(node["ignored"].is_boolean(), "ignored is always present: {node}");
        if let Some(parent) = node.get("parentId") {
            assert!(
                by_id(nodes, parent)["childIds"].as_array().unwrap().contains(&node["nodeId"]),
                "parent does not list {}",
                node["nodeId"]
            );
        }
        for child in node["childIds"].as_array().into_iter().flatten() {
            assert_eq!(by_id(nodes, child)["parentId"], node["nodeId"]);
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn full_tree_starts_at_a_focusable_document_root_with_the_frame_id() {
    let (mut ctx, frame) = setup().await;
    let tree = cdp(&mut ctx, "Accessibility.getFullAXTree", json!({})).await;
    let nodes = tree["nodes"].as_array().unwrap();
    let root = &nodes[0];
    assert_eq!(root["role"]["value"], "RootWebArea", "{root}");
    assert_eq!(root["ignored"], false);
    assert!(root.get("parentId").is_none());
    assert_eq!(root["frameId"], frame);
    assert_eq!(root["name"]["value"], "Tree Page");
    assert_eq!(property(root, "focusable"), Some(&json!(true)), "snapshot clients keep a root only when it is focusable: {root}");
    assert!(!root["childIds"].as_array().unwrap().is_empty());
    assert_connected(nodes);
    assert!(nodes.iter().any(|node| node["role"]["value"] == "heading" && node["name"]["value"] == "Static Heading"));

    let explicit = cdp(&mut ctx, "Accessibility.getFullAXTree", json!({"frameId": frame})).await;
    assert_eq!(explicit["nodes"], tree["nodes"], "the main frame id selects the same tree");
}

#[tokio::test(flavor = "current_thread")]
async fn every_node_is_reachable_from_the_first_node() {
    let (mut ctx, _) = setup().await;
    let tree = cdp(&mut ctx, "Accessibility.getFullAXTree", json!({})).await;
    let nodes = tree["nodes"].as_array().unwrap();
    let mut reached = std::collections::HashSet::new();
    let mut pending = vec![nodes[0]["nodeId"].clone()];
    while let Some(id) = pending.pop() {
        assert!(reached.insert(id.to_string()));
        pending.extend(by_id(nodes, &id)["childIds"].as_array().cloned().unwrap_or_default());
    }
    assert_eq!(reached.len(), nodes.len(), "a client walking childIds from the root must see every node");
}

#[tokio::test(flavor = "current_thread")]
async fn ignored_nodes_carry_chrome_reasons() {
    let (mut ctx, _) = setup().await;
    let hidden = backend_of(&mut ctx, "#hid").await;
    let secret = backend_of(&mut ctx, "#hid button").await;
    let presentation = backend_of(&mut ctx, "#pres").await;
    let tree = cdp(&mut ctx, "Accessibility.getFullAXTree", json!({})).await;
    let nodes = tree["nodes"].as_array().unwrap();

    let node = by_backend(nodes, hidden);
    assert_eq!(node["ignored"], true, "{node}");
    assert_eq!(node["role"]["value"], "none");
    assert_eq!(reasons(node), ["ariaHiddenElement"]);

    let node = by_backend(nodes, secret);
    assert_eq!(node["ignored"], true, "{node}");
    assert_eq!(reasons(node), ["ariaHiddenSubtree"]);
    assert!(node.get("name").is_none(), "ignored nodes expose no name: {node}");

    let node = by_backend(nodes, presentation);
    assert_eq!(node["ignored"], true, "{node}");
    assert_eq!(reasons(node), ["presentationalRole"]);

    assert!(
        !nodes.iter().any(|node| node["name"]["value"] == "not rendered"),
        "a hidden element is not in the tree"
    );
    let heading = nodes.iter().find(|node| node["role"]["value"] == "heading").unwrap();
    assert_eq!(heading["ignored"], false);
    assert!(heading.get("ignoredReasons").is_none());
}

#[tokio::test(flavor = "current_thread")]
async fn depth_limits_the_returned_nodes() {
    let (mut ctx, _) = setup().await;
    let full = cdp(&mut ctx, "Accessibility.getFullAXTree", json!({})).await;
    let full = full["nodes"].as_array().unwrap();

    let only_root = cdp(&mut ctx, "Accessibility.getFullAXTree", json!({"depth": 0})).await;
    let only_root = only_root["nodes"].as_array().unwrap();
    assert_eq!(only_root.len(), 1);
    assert_eq!(only_root[0]["nodeId"], full[0]["nodeId"]);
    assert_eq!(only_root[0]["childIds"], full[0]["childIds"], "a boundary node still lists its children");

    let two = cdp(&mut ctx, "Accessibility.getFullAXTree", json!({"depth": 1})).await;
    let two = two["nodes"].as_array().unwrap();
    assert_eq!(two.len(), 1 + full[0]["childIds"].as_array().unwrap().len());
    assert!(two.len() < full.len());
}

#[tokio::test(flavor = "current_thread")]
async fn root_and_child_queries_match_the_full_tree() {
    let (mut ctx, frame) = setup().await;
    let full = cdp(&mut ctx, "Accessibility.getFullAXTree", json!({})).await;
    let full = full["nodes"].as_array().unwrap();

    let root = cdp(&mut ctx, "Accessibility.getRootAXNode", json!({"frameId": frame})).await;
    assert_eq!(root["node"], full[0]);

    let children = cdp(
        &mut ctx,
        "Accessibility.getChildAXNodes",
        json!({"id": full[0]["nodeId"], "frameId": frame}),
    )
    .await;
    let children = children["nodes"].as_array().unwrap();
    let expected: Vec<&Value> =
        full[0]["childIds"].as_array().unwrap().iter().map(|id| by_id(full, id)).collect();
    assert_eq!(children.iter().collect::<Vec<_>>(), expected);

    let missing = call(&mut ctx, "Accessibility.getChildAXNodes", json!({"id": "999999"})).await;
    assert!(missing.is_err(), "unknown AX node id");
    let no_id = call(&mut ctx, "Accessibility.getChildAXNodes", json!({})).await;
    assert!(no_id.is_err(), "id is required");
}

#[tokio::test(flavor = "current_thread")]
async fn partial_tree_returns_the_node_first_with_relatives() {
    let (mut ctx, _) = setup().await;
    let button = backend_of(&mut ctx, "#go").await;
    let full = cdp(&mut ctx, "Accessibility.getFullAXTree", json!({})).await;
    let full = full["nodes"].as_array().unwrap();

    let alone = cdp(
        &mut ctx,
        "Accessibility.getPartialAXTree",
        json!({"backendNodeId": button, "fetchRelatives": false}),
    )
    .await;
    let alone = alone["nodes"].as_array().unwrap();
    assert_eq!(alone.len(), 1);
    assert_eq!(alone[0], *by_backend(full, button));

    let with = cdp(&mut ctx, "Accessibility.getPartialAXTree", json!({"nodeId": button})).await;
    let with = with["nodes"].as_array().unwrap();
    assert_eq!(with[0], *by_backend(full, button), "the requested node is first");
    let mut ancestor = by_backend(full, button)["parentId"].clone();
    while !ancestor.is_null() {
        let node = by_id(full, &ancestor);
        assert!(with.contains(node), "ancestor {ancestor} missing");
        ancestor = node["parentId"].clone();
    }
    assert!(with.iter().any(|node| node["role"]["value"] == "RootWebArea"));

    let object = cdp(&mut ctx, "Runtime.evaluate", json!({"expression": "document.getElementById('go')"})).await;
    let by_object = cdp(
        &mut ctx,
        "Accessibility.getPartialAXTree",
        json!({"objectId": object["result"]["objectId"], "fetchRelatives": false}),
    )
    .await;
    assert_eq!(by_object["nodes"][0], *by_backend(full, button));

    assert!(call(&mut ctx, "Accessibility.getPartialAXTree", json!({})).await.is_err());
    assert!(call(&mut ctx, "Accessibility.getPartialAXTree", json!({"backendNodeId": 99_999_999})).await.is_err());
}

#[tokio::test(flavor = "current_thread")]
async fn query_filters_a_subtree_by_name_and_role() {
    let (mut ctx, _) = setup().await;
    let form = backend_of(&mut ctx, "form").await;

    let buttons = cdp(&mut ctx, "Accessibility.queryAXTree", json!({"backendNodeId": form, "role": "button"})).await;
    let buttons = buttons["nodes"].as_array().unwrap();
    assert_eq!(buttons.len(), 1, "{buttons:?}");
    assert_eq!(buttons[0]["name"]["value"], "Go");

    let named = cdp(&mut ctx, "Accessibility.queryAXTree", json!({"nodeId": form, "accessibleName": "Query"})).await;
    assert!(
        named["nodes"].as_array().unwrap().iter().any(|node| node["role"]["value"] == "textbox"),
        "{named}"
    );

    let everything = cdp(&mut ctx, "Accessibility.queryAXTree", json!({"nodeId": form})).await;
    assert!(everything["nodes"].as_array().unwrap().len() > 3);

    let none = cdp(&mut ctx, "Accessibility.queryAXTree", json!({"nodeId": form, "role": "tab"})).await;
    assert!(none["nodes"].as_array().unwrap().is_empty());
    assert!(call(&mut ctx, "Accessibility.queryAXTree", json!({"role": "button"})).await.is_err());
}

#[tokio::test(flavor = "current_thread")]
async fn unknown_frame_and_unknown_method_are_errors() {
    let (mut ctx, _) = setup().await;
    let error = call(&mut ctx, "Accessibility.getFullAXTree", json!({"frameId": "no-such-frame"})).await.unwrap_err();
    assert!(error.contains("not found"), "{error}");
    let error = call(&mut ctx, "Accessibility.bogus", json!({})).await.unwrap_err();
    assert_eq!(error, "'Accessibility.bogus' wasn't found");
    assert!(call(&mut ctx, "Accessibility.disable", json!({})).await.is_ok());
    assert!(call(&mut ctx, "Accessibility.enable", json!({})).await.is_ok());
}

async fn serve() -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut buf = [0u8; 2048];
                let read = socket.read(&mut buf).await.unwrap_or(0);
                let request = String::from_utf8_lossy(&buf[..read]).to_string();
                let body = if request.starts_with("GET /child.html ") {
                    "<html><head><title>Child</title></head><body><button>Inside frame</button></body></html>"
                } else {
                    "<html><body><h1>Outer</h1><iframe src=\"/child.html\"></iframe></body></html>"
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
async fn a_child_frame_id_selects_that_frames_document() {
    std::env::set_var("INBROWSE_ALLOW_PRIVATE_NETWORK", "1");
    let url = serve().await;
    let mut ctx = CdpContext::new();
    let page = ctx.create_page();
    ctx.sessions.insert("ax-session".to_string(), page);
    cdp(&mut ctx, "Page.navigate", json!({"url": url})).await;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    let child = loop {
        cdp(&mut ctx, "Runtime.evaluate", json!({"expression": "1"})).await;
        let tree = cdp(&mut ctx, "Page.getFrameTree", json!({})).await;
        if let Some(id) = tree["frameTree"]["childFrames"][0]["frame"]["id"].as_str() {
            break id.to_string();
        }
        assert!(std::time::Instant::now() < deadline, "child frame never appeared");
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    };
    let tree = cdp(&mut ctx, "Accessibility.getFullAXTree", json!({"frameId": child})).await;
    let nodes = tree["nodes"].as_array().unwrap();
    assert_eq!(nodes[0]["role"]["value"], "RootWebArea");
    assert_eq!(nodes[0]["frameId"], child);
    assert_eq!(nodes[0]["name"]["value"], "Child");
    assert!(nodes.iter().any(|node| node["role"]["value"] == "button" && node["name"]["value"] == "Inside frame"));
    assert!(!nodes.iter().any(|node| node["role"]["value"] == "heading"), "the parent's content is not in the child tree");
    assert_connected(nodes);
}
