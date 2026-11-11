//! Child frames through the protocol the way Puppeteer and Playwright use it:
//! each frame has a default execution context, Runtime commands run in the
//! frame a context belongs to, `DOM.describeNode` on an iframe names its frame,
//! and nodes inside a frame can be resolved, measured and clicked.

use inbrowse_cdp::dispatch::{dispatch, CdpContext};
use inbrowse_cdp::types::{CdpEvent, CdpRequest};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const SRCDOC: &str = "<p id=sp>SRCDOC_CONTENT</p>\
    <button id=sb onclick=\"document.getElementById('sp').textContent='SRCDOC_CLICKED'\">go</button>";

fn page_html() -> String {
    format!(
        "<!doctype html><body style=\"margin:0\"><p style=\"margin:0;height:40px\">top</p>\
         <iframe id=ifr src=\"/frame.html\" style=\"border:0;width:300px;height:100px\"></iframe>\
         <iframe id=sd srcdoc=\"{}\" style=\"border:0;width:300px;height:100px\"></iframe>\
         <iframe id=du src=\"data:text/html,<p id=dp>DATA_CONTENT</p>\" style=\"border:0\"></iframe>\
         <script>window.addEventListener('load', () => {{ \
           window.__atLoad = document.getElementById('ifr').contentDocument.getElementById('fp').textContent; }});</script>",
        SRCDOC.replace('"', "&quot;")
    )
}

const FRAME_HTML: &str = "<!doctype html><body style=\"margin:0\"><title>child</title>\
    <p id=fp style=\"margin:0\">IFRAME_CONTENT</p>\
    <button id=fb style=\"display:block;width:100px;height:30px\" \
     onclick=\"document.getElementById('fp').textContent='CLICKED'\">click</button>\
    <script>document.getElementById('fp').textContent += ' (ran)';</script>";

async fn serve() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut buf = [0u8; 2048];
                let read = socket.read(&mut buf).await.unwrap_or(0);
                let request = String::from_utf8_lossy(&buf[..read]).to_string();
                let body = if request.starts_with("GET /frame.html ") {
                    FRAME_HTML.to_string()
                } else {
                    page_html()
                };
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = socket.write_all(resp.as_bytes()).await;
            });
        }
    });
    format!("http://{addr}/")
}

async fn send(ctx: &mut CdpContext, id: u64, method: &str, params: Value, session: &str) -> Value {
    let resp = dispatch(
        &CdpRequest {
            id,
            method: method.to_string(),
            params,
            session_id: Some(session.to_string()),
        },
        ctx,
    )
    .await;
    assert!(resp.error.is_none(), "CDP {method} failed: {:?}", resp.error);
    resp.result.unwrap_or_else(|| json!({}))
}

async fn attached_session(ctx: &mut CdpContext) -> String {
    let created = dispatch(
        &CdpRequest {
            id: 900,
            method: "Target.createTarget".to_string(),
            params: json!({"url": "about:blank"}),
            session_id: None,
        },
        ctx,
    )
    .await
    .result
    .expect("Target.createTarget produced no result");
    let target_id = created["targetId"].as_str().expect("no targetId").to_string();
    let attached = dispatch(
        &CdpRequest {
            id: 901,
            method: "Target.attachToTarget".to_string(),
            params: json!({"targetId": target_id, "flatten": true}),
            session_id: None,
        },
        ctx,
    )
    .await
    .result
    .expect("Target.attachToTarget produced no result");
    attached["sessionId"].as_str().expect("no sessionId").to_string()
}

struct Fixture {
    ctx: CdpContext,
    session: String,
    next_id: u64,
}

impl Fixture {
    /// Opens the page with every kind of child frame and waits until the three
    /// frames are in the tree, as a client would after `goto`.
    async fn open() -> Self {
        std::env::set_var("INBROWSE_ALLOW_PRIVATE_NETWORK", "1");
        let url = serve().await;
        let mut ctx = CdpContext::new();
        let session = attached_session(&mut ctx).await;
        let mut fixture = Fixture { ctx, session, next_id: 1 };
        fixture.call("Page.enable", json!({})).await;
        fixture.call("Runtime.enable", json!({})).await;
        fixture.call("Page.navigate", json!({"url": url})).await;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let tree = fixture.call("Page.getFrameTree", json!({})).await;
            let built = tree["frameTree"]["childFrames"].as_array().map_or(0, Vec::len) == 3;
            if built || std::time::Instant::now() >= deadline {
                break;
            }
            fixture.eval(None, "1").await;
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        fixture
    }

    async fn call(&mut self, method: &str, params: Value) -> Value {
        self.next_id += 1;
        let (id, session) = (self.next_id, self.session.clone());
        send(&mut self.ctx, id, method, params, &session).await
    }

    /// `Runtime.evaluate` by value, in a context when one is given.
    async fn eval(&mut self, context: Option<i64>, expression: &str) -> Value {
        let mut params = json!({"expression": expression, "returnByValue": true, "awaitPromise": true});
        if let Some(context) = context {
            params["contextId"] = json!(context);
        }
        let reply = self.call("Runtime.evaluate", params).await;
        assert!(reply.get("exceptionDetails").is_none(), "{expression}: {reply}");
        reply["result"]["value"].clone()
    }

    fn events(&self, method: &str) -> Vec<&CdpEvent> {
        self.ctx.pending_events.iter().filter(|event| event.method == method).collect()
    }

    /// The CDP frame id of the child whose URL satisfies `matches`.
    async fn child_frame(&mut self, matches: impl Fn(&str) -> bool) -> String {
        let tree = self.call("Page.getFrameTree", json!({})).await;
        tree["frameTree"]["childFrames"]
            .as_array()
            .unwrap()
            .iter()
            .find(|child| matches(child["frame"]["url"].as_str().unwrap_or_default()))
            .unwrap_or_else(|| panic!("no such child frame in {tree}"))["frame"]["id"]
            .as_str()
            .unwrap()
            .to_string()
    }

    /// The default execution context announced for `frame_id`.
    fn default_context(&self, frame_id: &str) -> i64 {
        self.events("Runtime.executionContextCreated")
            .iter()
            .map(|event| &event.params["context"])
            .find(|context| {
                context["auxData"]["frameId"] == frame_id && context["auxData"]["isDefault"] == true
            })
            .unwrap_or_else(|| panic!("no default execution context for frame {frame_id}"))["id"]
            .as_i64()
            .unwrap()
    }
}

#[tokio::test(flavor = "current_thread")]
async fn every_child_frame_is_attached_and_has_a_default_context() {
    let mut page = Fixture::open().await;
    let tree = page.call("Page.getFrameTree", json!({})).await;
    let children = tree["frameTree"]["childFrames"].as_array().unwrap().clone();
    let urls: Vec<&str> = children
        .iter()
        .map(|child| child["frame"]["url"].as_str().unwrap_or_default())
        .collect();
    assert!(urls.iter().any(|url| url.ends_with("/frame.html")), "src frame missing: {urls:?}");
    assert!(urls.contains(&"about:srcdoc"), "srcdoc frame missing: {urls:?}");
    assert!(urls.iter().any(|url| url.starts_with("data:text/html")), "data frame missing: {urls:?}");

    let main_frame = tree["frameTree"]["frame"]["id"].as_str().unwrap().to_string();
    for child in &children {
        let id = child["frame"]["id"].as_str().unwrap();
        assert_eq!(child["frame"]["parentId"], main_frame.as_str());
        let attached = page
            .ctx
            .pending_events
            .iter()
            .position(|e| e.method == "Page.frameAttached" && e.params["frameId"] == id)
            .unwrap_or_else(|| panic!("frame {id} was never attached"));
        let created = page
            .ctx
            .pending_events
            .iter()
            .position(|e| {
                e.method == "Runtime.executionContextCreated"
                    && e.params["context"]["auxData"]["frameId"] == id
                    && e.params["context"]["auxData"]["isDefault"] == true
                    && e.params["context"]["auxData"]["type"] == "default"
            })
            .unwrap_or_else(|| panic!("frame {id} has no default execution context"));
        assert!(attached < created, "a context must follow the attach of its frame");
    }
}

#[tokio::test(flavor = "current_thread")]
async fn a_frames_scripts_and_load_come_before_the_pages_load_event() {
    let mut page = Fixture::open().await;
    let at_load = page.eval(None, "window.__atLoad").await;
    assert_eq!(at_load, "IFRAME_CONTENT (ran)", "the frame's script had not run when the page's load fired");
}

#[tokio::test(flavor = "current_thread")]
async fn runtime_commands_run_in_the_frame_their_context_belongs_to() {
    let mut page = Fixture::open().await;
    let src = page.child_frame(|url| url.ends_with("/frame.html")).await;
    let srcdoc = page.child_frame(|url| url == "about:srcdoc").await;
    let data = page.child_frame(|url| url.starts_with("data:")).await;
    let (src_context, srcdoc_context, data_context) =
        (page.default_context(&src), page.default_context(&srcdoc), page.default_context(&data));

    assert_eq!(
        page.eval(Some(src_context), "document.getElementById('fp').textContent").await,
        "IFRAME_CONTENT (ran)"
    );
    assert_eq!(page.eval(Some(src_context), "location.pathname").await, "/frame.html");
    assert_eq!(
        page.eval(Some(srcdoc_context), "document.getElementById('sp').textContent").await,
        "SRCDOC_CONTENT"
    );
    assert_eq!(
        page.eval(Some(data_context), "document.getElementById('dp').textContent").await,
        "DATA_CONTENT"
    );
    assert_eq!(page.eval(None, "document.getElementById('fp')").await, Value::Null);

    // callFunctionOn with only a context runs on that frame's global object.
    let reply = page
        .call(
            "Runtime.callFunctionOn",
            json!({
                "functionDeclaration": "function() { return this.document.title; }",
                "executionContextId": src_context,
                "returnByValue": true,
            }),
        )
        .await;
    assert_eq!(reply["result"]["value"], "child");
}

#[tokio::test(flavor = "current_thread")]
async fn describe_node_on_an_iframe_names_its_frame_and_content_document() {
    let mut page = Fixture::open().await;
    let src = page.child_frame(|url| url.ends_with("/frame.html")).await;
    let handle = page
        .call("Runtime.evaluate", json!({"expression": "document.getElementById('ifr')"}))
        .await;
    let object_id = handle["result"]["objectId"].as_str().unwrap().to_string();

    let described = page.call("DOM.describeNode", json!({"objectId": object_id})).await;
    assert_eq!(described["node"]["frameId"], src.as_str());
    let document = &described["node"]["contentDocument"];
    assert_eq!(document["nodeType"], 9);
    assert_eq!(document["frameId"], src.as_str());
    assert!(document["documentURL"].as_str().unwrap().ends_with("/frame.html"));

    let owner = page.call("DOM.getFrameOwner", json!({"frameId": src})).await;
    assert_eq!(owner["backendNodeId"], described["node"]["backendNodeId"]);

    // The frame's document is reachable as a node of its own.
    let root = document["nodeId"].as_u64().unwrap();
    let found = page
        .call("DOM.querySelector", json!({"nodeId": root, "selector": "#fp"}))
        .await;
    let node = found["nodeId"].as_u64().unwrap();
    assert_ne!(node, 0, "the frame's element was not found");
    let attributes = page.call("DOM.getAttributes", json!({"nodeId": node})).await;
    assert_eq!(attributes["attributes"][0], "id");
}

#[tokio::test(flavor = "current_thread")]
async fn an_element_of_a_frame_resolves_measures_and_clicks_in_page_coordinates() {
    let mut page = Fixture::open().await;
    let src = page.child_frame(|url| url.ends_with("/frame.html")).await;
    let context = page.default_context(&src);
    let handle = page
        .call(
            "Runtime.evaluate",
            json!({"expression": "document.getElementById('fb')", "contextId": context}),
        )
        .await;
    let object_id = handle["result"]["objectId"].as_str().unwrap().to_string();

    // The handle belongs to the frame: a function run on it sees that
    // frame's constructors, which Puppeteer's injected helpers rely on.
    let own_realm = page
        .call(
            "Runtime.callFunctionOn",
            json!({
                "functionDeclaration": "function() { return this instanceof HTMLButtonElement && this.ownerDocument.title; }",
                "objectId": object_id,
                "returnByValue": true,
            }),
        )
        .await;
    assert_eq!(own_realm["result"]["value"], "child");

    let described = page.call("DOM.describeNode", json!({"objectId": object_id})).await;
    assert_eq!(described["node"]["nodeName"], "BUTTON");
    let backend = described["node"]["backendNodeId"].as_u64().unwrap();
    let resolved = page.call("DOM.resolveNode", json!({"backendNodeId": backend})).await;
    assert_eq!(resolved["object"]["subtype"], "node");

    // The frame's button is 100x30, so its quad is the button's place in its
    // own frame shifted by where the iframe's content box starts in the page.
    let iframe = page
        .eval(None, "(() => { const r = document.getElementById('ifr').getBoundingClientRect(); return [r.left, r.top]; })()")
        .await;
    let origin = (iframe[0].as_f64().unwrap(), iframe[1].as_f64().unwrap());
    assert!(origin.1 >= 40.0, "the iframe should sit below the paragraph: {origin:?}");
    let quads = page.call("DOM.getContentQuads", json!({"objectId": object_id})).await;
    let quad: Vec<f64> = quads["quads"][0].as_array().unwrap().iter().map(|n| n.as_f64().unwrap()).collect();
    let local = page
        .eval(Some(context), "(() => { const r = document.getElementById('fb').getBoundingClientRect(); return [r.left, r.top]; })()")
        .await;
    let expected = (origin.0 + local[0].as_f64().unwrap(), origin.1 + local[1].as_f64().unwrap());
    assert!(local[1].as_f64().unwrap() > 0.0, "the button sits below the frame's paragraph");
    assert_eq!((quad[0], quad[1]), expected, "quad is not in page coordinates: {quad:?}");
    assert_eq!((quad[4] - quad[0], quad[5] - quad[1]), (100.0, 30.0));
    let model = page.call("DOM.getBoxModel", json!({"objectId": object_id})).await;
    assert_eq!(model["model"]["width"], 100);

    // Hit testing at that page point reaches the button inside the frame.
    let (x, y) = ((quad[0] + quad[4]) / 2.0, (quad[1] + quad[5]) / 2.0);
    let hit = page.call("DOM.getNodeForLocation", json!({"x": x as i64, "y": y as i64})).await;
    assert_eq!(hit["frameId"], src.as_str());
    assert_eq!(hit["backendNodeId"], backend);
    for kind in ["mousePressed", "mouseReleased"] {
        page.call(
            "Input.dispatchMouseEvent",
            json!({"type": kind, "x": x, "y": y, "button": "left", "clickCount": 1}),
        )
        .await;
    }
    assert_eq!(
        page.eval(Some(context), "document.getElementById('fp').textContent").await,
        "CLICKED"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn accessibility_nodes_of_a_frame_use_the_dom_domains_ids() {
    let mut page = Fixture::open().await;
    let src = page.child_frame(|url| url.ends_with("/frame.html")).await;
    let context = page.default_context(&src);
    let handle = page
        .call(
            "Runtime.evaluate",
            json!({"expression": "document.getElementById('fb')", "contextId": context}),
        )
        .await;
    let object_id = handle["result"]["objectId"].as_str().unwrap().to_string();
    let described = page.call("DOM.describeNode", json!({"objectId": object_id})).await;
    let backend = described["node"]["backendNodeId"].clone();

    let tree = page.call("Accessibility.getFullAXTree", json!({"frameId": src})).await;
    let button = tree["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|node| node["role"]["value"] == "button")
        .unwrap_or_else(|| panic!("no button in the frame's AX tree: {tree}"));
    assert_eq!(button["backendDOMNodeId"], backend);

    // The same id selects the node's tree without naming the frame.
    let partial = page
        .call("Accessibility.getPartialAXTree", json!({"backendNodeId": backend, "fetchRelatives": false}))
        .await;
    assert_eq!(partial["nodes"][0]["backendDOMNodeId"], backend);
}

#[tokio::test(flavor = "current_thread")]
async fn typing_goes_to_the_frame_that_holds_focus() {
    let mut page = Fixture::open().await;
    let src = page.child_frame(|url| url.ends_with("/frame.html")).await;
    let context = page.default_context(&src);
    page.eval(
        Some(context),
        "(() => { const input = document.createElement('input'); input.id = 'q'; document.body.appendChild(input); input.focus(); return true; })()",
    )
    .await;
    page.call("Input.insertText", json!({"text": "hello"})).await;
    assert_eq!(page.eval(Some(context), "document.getElementById('q').value").await, "hello");
}

#[tokio::test(flavor = "current_thread")]
async fn removing_an_iframe_detaches_its_frame_and_destroys_its_context() {
    let mut page = Fixture::open().await;
    let src = page.child_frame(|url| url.ends_with("/frame.html")).await;
    let context = page.default_context(&src);
    // Chrome detaches the frame as soon as the iframe leaves the document, so
    // the events come with the command that removed it.
    page.eval(None, "document.getElementById('ifr').remove()").await;
    assert!(
        page.events("Page.frameDetached").iter().any(|event| event.params["frameId"] == src.as_str()),
        "the removed iframe's frame was never detached"
    );
    assert!(
        page.events("Runtime.executionContextDestroyed")
            .iter()
            .any(|event| event.params["executionContextId"] == context),
        "the frame's context was never destroyed"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn every_way_of_removing_an_iframe_detaches_its_frame_in_the_same_command() {
    for removal in [
        "document.body.removeChild(document.getElementById('ifr'))",
        "document.getElementById('ifr').replaceWith(document.createElement('div'))",
        "document.body.replaceChild(document.createElement('div'), document.getElementById('ifr'))",
        "document.body.innerHTML = ''",
        "document.body.textContent = ''",
        // Moving a connected iframe re-creates its browsing context.
        "document.body.insertBefore(document.getElementById('ifr'), document.body.firstChild)",
    ] {
        let mut page = Fixture::open().await;
        let src = page.child_frame(|url| url.ends_with("/frame.html")).await;
        let before = page.events("Page.frameDetached").len();
        page.eval(None, removal).await;
        let detached = page.events("Page.frameDetached");
        assert!(
            detached[before..].iter().any(|event| event.params["frameId"] == src.as_str()),
            "{removal} did not detach the frame in the same command"
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn a_new_frame_gets_the_isolated_worlds_the_client_created_for_the_page() {
    let mut page = Fixture::open().await;
    let tree = page.call("Page.getFrameTree", json!({})).await;
    let main = tree["frameTree"]["frame"]["id"].as_str().unwrap().to_string();
    page.call(
        "Page.createIsolatedWorld",
        json!({"frameId": main, "worldName": "utility", "grantUniveralAccess": true}),
    )
    .await;
    page.eval(
        None,
        "(() => { const f = document.createElement('iframe'); f.id = 'late'; f.srcdoc = '<p>late</p>'; document.body.appendChild(f); })()",
    )
    .await;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let late = loop {
        page.eval(None, "new Promise(resolve => setTimeout(resolve, 10))").await;
        let tree = page.call("Page.getFrameTree", json!({})).await;
        let found = tree["frameTree"]["childFrames"].as_array().unwrap().iter().find(|child| {
            child["frame"]["url"] == "about:srcdoc"
                && !page.events("Runtime.executionContextCreated").iter().all(|event| {
                    event.params["context"]["auxData"]["frameId"] != child["frame"]["id"]
                        || event.params["context"]["name"] != "utility"
                })
        });
        if let Some(found) = found {
            break found["frame"]["id"].as_str().unwrap().to_string();
        }
        assert!(std::time::Instant::now() < deadline, "no isolated world appeared for the new frame");
    };
    let world = page
        .events("Runtime.executionContextCreated")
        .iter()
        .map(|event| &event.params["context"])
        .find(|context| context["auxData"]["frameId"] == late.as_str() && context["name"] == "utility")
        .map(|context| context["auxData"]["isDefault"].clone());
    assert_eq!(world, Some(json!(false)));
}

#[tokio::test(flavor = "current_thread")]
async fn an_element_listed_by_get_properties_can_be_described_and_passed_on() {
    let mut page = Fixture::open().await;
    let src = page.child_frame(|url| url.ends_with("/frame.html")).await;
    let list = page
        .call("Runtime.evaluate", json!({"expression": "[document.getElementById('ifr')]"}))
        .await;
    let properties = page
        .call(
            "Runtime.getProperties",
            json!({"objectId": list["result"]["objectId"], "ownProperties": true}),
        )
        .await;
    let element = properties["result"][0]["value"]["objectId"].as_str().unwrap().to_string();

    let described = page.call("DOM.describeNode", json!({"objectId": element})).await;
    assert_eq!(described["node"]["frameId"], src.as_str());
    let passed = page
        .call(
            "Runtime.callFunctionOn",
            json!({
                "functionDeclaration": "function(element) { return element.id; }",
                "objectId": list["result"]["objectId"],
                "arguments": [{"objectId": element}],
                "returnByValue": true,
            }),
        )
        .await;
    assert_eq!(passed["result"]["value"], "ifr");
}
