use inbrowse_dom::parse_html;
use inbrowse_js::runtime::InbrowseJsRuntime;
use serde_json::json;

fn setup(html: &str) -> InbrowseJsRuntime {
    let dom = parse_html(html);
    let mut rt = InbrowseJsRuntime::new();
    rt.set_dom(dom);
    rt.set_url("http://example.com/test");
    rt.set_title("Test Page");
    rt.run_page_init();
    rt
}

/// A srcdoc document loads in a queued task, a later turn; one short turn can
/// miss it on a loaded host, so pump until `loaded` holds (bounded).
async fn pump_until(rt: &mut InbrowseJsRuntime, loaded: &str) {
    for _ in 0..100 {
        rt.run_event_loop_bounded(30).await.unwrap();
        if rt.evaluate(loaded).unwrap() == json!(true) {
            return;
        }
    }
}

#[tokio::test]
async fn srcdoc_iframe_created_via_innerhtml_does_not_abort() {
    let mut rt = setup("<html><body><div id=host></div></body></html>");
    rt.evaluate(r#"host.innerHTML = '<iframe srcdoc="<p>frame</p>"></iframe>'"#)
        .unwrap();
    pump_until(
        &mut rt,
        "host.querySelector('iframe')?.contentDocument?.body?.innerHTML === '<p>frame</p>'",
    )
    .await;
    assert_eq!(
        rt.evaluate(
            r#"(() => {
                const frame = host.querySelector('iframe');
                return [!!frame, frame.contentDocument.body.innerHTML];
            })()"#
        )
        .unwrap(),
        json!([true, "<p>frame</p>"])
    );
}

#[tokio::test]
async fn srcdoc_iframe_created_via_property_then_append_does_not_abort() {
    let mut rt = setup("<html><body></body></html>");
    rt.evaluate(
        r#"(() => {
            const frame = document.createElement('iframe');
            frame.srcdoc = '<p>prop</p>';
            document.body.appendChild(frame);
        })()"#,
    )
    .unwrap();
    pump_until(
        &mut rt,
        "document.querySelector('iframe')?.contentDocument?.body?.innerHTML === '<p>prop</p>'",
    )
    .await;
    assert_eq!(
        rt.evaluate(
            r#"(() => {
                const frame = document.querySelector('iframe');
                return frame.contentDocument.body.innerHTML;
            })()"#
        )
        .unwrap(),
        json!("<p>prop</p>")
    );
}

#[tokio::test]
async fn srcdoc_iframe_with_synchronous_dom_touch_does_not_abort() {
    let mut rt = setup("<html><body></body></html>");
    rt.evaluate(
        r#"(() => {
            const frame = document.createElement('iframe');
            frame.srcdoc = '<body><span>inner</span></body>';
            document.body.appendChild(frame);
        })()"#,
    )
    .unwrap();
    pump_until(
        &mut rt,
        "!!document.querySelector('iframe')?.contentDocument?.querySelector('span')",
    )
    .await;
    assert_eq!(
        rt.evaluate(
            r#"(() => {
                const frame = document.querySelector('iframe');
                const doc = frame.contentDocument;
                const span = doc.querySelector('span');
                span.textContent = 'touched';
                return doc.body.textContent;
            })()"#
        )
        .unwrap(),
        json!("touched")
    );
}
