use std::cell::RefCell;
use std::time::Duration;

use inbrowse::lifecycle::WaitUntil;
use inbrowse::{InterceptedRequest, Page as InnerPage};
use inbrowse_net::{RequestCallback, ResponseCallback};
use serde_json::Value;

use crate::error::Error;

/// Read a DOM node id from a JS `evaluate` result. Integral numbers come back
/// as JSON integers, so a node id is a `u64`; null and anything else is `None`.
fn nid_from_value(v: &Value) -> Option<u64> {
    v.as_u64()
}

/// A browser tab/page.
///
/// The wrapped [`InnerPage`] lives behind a `RefCell` so that read-style
/// operations — including [`Page::evaluate`] — take `&self`. That lets an
/// [`Element`] borrow the page immutably and drive evaluation through a safe
/// shared reference, instead of the previous lifetime-less raw pointer that
/// could dangle if the page was dropped or moved.
pub struct Page {
    pub(crate) inner: RefCell<InnerPage>,
}

impl Page {
    /// Navigate to URL and wait for load.
    pub async fn goto(&mut self, url: &str) -> Result<(), Error> {
        self.inner
            .get_mut()
            .navigate_with_wait(url, WaitUntil::Load)
            .await
            .map_err(|e| Error::Navigation(e.to_string()))
    }

    /// Get current URL.
    pub fn url(&self) -> String {
        self.inner.borrow().url_string()
    }

    /// Execute JS in the page and return its value as JSON.
    ///
    /// The source is an object literal, a block of statements whose last
    /// value is the result (`var a = 1; a + 1` is `2`), or a function body
    /// when it has a top-level `return`. A thrown or syntax error is
    /// [`Error::JsEval`]. A returned promise is not awaited; use
    /// [`Self::evaluate_awaited`] for that, or [`Self::settle`] afterwards.
    ///
    /// Takes `&self` via interior mutability so element handles that borrow the
    /// page immutably can drive evaluation without any `unsafe` aliasing.
    pub fn evaluate(&self, expression: &str) -> Result<Value, Error> {
        self.inner.borrow_mut().evaluate(expression).map_err(Error::JsEval)
    }

    /// Evaluate JS the way the DevTools console does and await a returned
    /// promise, all within `timeout`. A thrown error, a rejected promise and a
    /// syntax error are [`Error::JsEval`]; running out of time is
    /// [`Error::Timeout`]. Same semantics as `inbrowse fetch --eval` and the
    /// MCP `browser_evaluate` tool (inbrowse `Page::evaluate_checked`).
    pub async fn evaluate_awaited(&mut self, expression: &str, timeout: Duration) -> Result<Value, Error> {
        self.inner
            .get_mut()
            .evaluate_checked(expression, timeout)
            .await
            .map_err(|error| match error {
                inbrowse::EvalError::TimedOut(_) => Error::Timeout(error.to_string()),
                other => Error::JsEval(other.to_string()),
            })
    }

    /// URLs of the page's child frames, in creation order.
    pub fn frame_urls(&self) -> Vec<String> {
        self.inner.borrow().frame_urls()
    }

    /// Execute JS inside one of the page's child frames. Each frame is its own
    /// realm with its own document, so this is the only way to observe one.
    pub fn evaluate_in_frame(&mut self, index: usize, expression: &str) -> Result<Value, String> {
        self.inner.borrow_mut().evaluate_in_frame(index, expression)
    }

    /// Get page HTML content.
    pub fn content(&self) -> Result<String, Error> {
        let val = self.evaluate("document.documentElement ? document.documentElement.outerHTML : ''")?;
        Ok(val.as_str().unwrap_or_default().to_string())
    }

    /// Query a single element by CSS selector. `Ok(None)` when nothing
    /// matches; an invalid selector is an error.
    pub fn query_selector(&self, selector: &str) -> Result<Option<Element<'_>>, Error> {
        let escaped = selector.replace('\\', "\\\\").replace('\'', "\\'");
        let js = format!(
            "(function() {{ var el = document.querySelector('{}'); return el ? el._nid : null; }})()",
            escaped
        );
        let val = self.evaluate(&js)?;
        Ok(nid_from_value(&val).map(|nid| Element { node_id: nid, page: self }))
    }

    /// Wait for CSS selector to appear (polls every 100ms).
    pub async fn wait_for_selector(
        &self,
        selector: &str,
        timeout: Duration,
    ) -> Result<Element<'_>, Error> {
        let start = std::time::Instant::now();
        let escaped = selector.replace('\\', "\\\\").replace('\'', "\\'");
        loop {
            let js = format!(
                "(function() {{ var el = document.querySelector('{}'); return el ? el._nid : null; }})()",
                escaped
            );
            let val = self.evaluate(&js)?;
            if let Some(nid) = nid_from_value(&val) {
                return Ok(Element { node_id: nid, page: self });
            }
            if start.elapsed() > timeout {
                return Err(Error::Timeout(format!(
                    "wait_for_selector({}) timed out after {}ms",
                    selector,
                    timeout.as_millis()
                )));
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// Drive the page's JS event loop for up to `max_ms` milliseconds.
    ///
    /// Call this after `evaluate()` kicks off async work (Promises, fetch,
    /// setTimeout, RxJS subscribers) to let the V8 event loop pump and
    /// resolve scheduled microtasks/macrotasks before the next `evaluate()`.
    pub async fn settle(&mut self, max_ms: u64) {
        self.inner.get_mut().settle(max_ms).await
    }

    /// Register a script that runs before any of the page's own `<script>` tags,
    /// equivalent to CDP `Page.addScriptToEvaluateOnNewDocument`. Runs on the next
    /// `goto()` / navigation. Use it to install a fetch()/XHR interceptor or any
    /// other page-init logic before the page's bootstrap runs.
    pub fn add_preload_script(&mut self, script: &str) {
        self.inner.get_mut().add_preload_script(script);
    }

    /// Enable CDP-Fetch-style interception of every JS `fetch()`/XHR. Returns a
    /// receiver yielding each request; resolve it through its `resolver` with
    /// [`inbrowse_sdk::InterceptResolution`] (`Continue`, `Fulfill`, `Fail`) to pass,
    /// mock, or block it. Works in stealth and non-stealth.
    pub fn enable_interception(
        &mut self,
    ) -> tokio::sync::mpsc::UnboundedReceiver<InterceptedRequest> {
        self.inner.get_mut().enable_interception()
    }

    /// Register a passive callback fired for every request the page makes
    /// (navigation and JS `fetch()`/XHR), once its method/headers/body are known
    /// and before it is sent. Non-blocking; use `enable_interception` to mutate
    /// or block. Returns a stable id; pass it to `off_request` to detach.
    pub fn on_request(&mut self, cb: RequestCallback) -> u64 {
        self.inner.get_mut().on_request(cb)
    }

    /// Register a passive callback fired with every response the page receives
    /// (navigation and JS `fetch()`/XHR), including its body. Non-blocking. The
    /// main path for capturing API response payloads from SPAs. Returns a stable
    /// id; pass it to `off_response` to detach.
    pub fn on_response(&mut self, cb: ResponseCallback) -> u64 {
        self.inner.get_mut().on_response(cb)
    }

    /// Detach a request callback previously registered with `on_request`.
    /// Returns true if a callback with that id was removed. Callbacks are
    /// scoped to this page — they never fire for sibling pages and are
    /// dropped with the page (issue #408).
    pub fn off_request(&mut self, id: u64) -> bool {
        self.inner.get_mut().off_request(id)
    }

    /// Detach a response callback previously registered with `on_response`.
    /// Returns true if a callback with that id was removed.
    pub fn off_response(&mut self, id: u64) -> bool {
        self.inner.get_mut().off_response(id)
    }
}

/// Handle to a DOM element.
///
/// Created via [`Page::query_selector`] or [`Page::wait_for_selector`]. The
/// handle borrows the [`Page`] for `'p`, so the borrow checker guarantees the
/// page outlives every element derived from it — a dangling handle is a compile
/// error rather than undefined behaviour.
pub struct Element<'p> {
    node_id: u64,
    page: &'p Page,
}

impl Element<'_> {
    /// Get text content of this element.
    pub fn text(&self) -> Result<String, Error> {
        let val = self.page.evaluate(&format!(
            "(function() {{ var el = globalThis._wrap && globalThis._wrap({}); return el ? el.textContent : ''; }})()",
            self.node_id
        ))?;
        Ok(val.as_str().unwrap_or_default().to_string())
    }

    /// Get an attribute value.
    pub fn attribute(&self, name: &str) -> Result<Option<String>, Error> {
        // Escape the name like query_selector escapes its selector, so a name
        // containing a quote/backslash cannot break out of the JS string literal
        // and inject code into the page.
        let escaped = name.replace('\\', "\\\\").replace('\'', "\\'");
        let val = self.page.evaluate(&format!(
            "(function() {{ var el = globalThis._wrap && globalThis._wrap({}); return el ? el.getAttribute('{}') : null; }})()",
            self.node_id, escaped
        ))?;
        Ok(val.as_str().map(str::to_string))
    }

    /// Click this element.
    pub fn click(&self) -> Result<(), Error> {
        // Scroll into view
        self.page.evaluate(&format!(
            "(function() {{ var el = globalThis._wrap && globalThis._wrap({}); if (el) el.scrollIntoView({{block:'center'}}); }})()",
            self.node_id
        ))?;
        // Click
        let result = self.page.evaluate(&format!(
            "(function() {{ var el = globalThis._wrap && globalThis._wrap({}); if (el) {{ el.click(); return true; }} return false; }})()",
            self.node_id
        ))?;
        if result.as_bool().unwrap_or(false) {
            Ok(())
        } else {
            Err(Error::ElementNotFound("click failed".into()))
        }
    }
}
