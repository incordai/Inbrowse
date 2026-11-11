
use std::sync::Arc;

use inbrowse::{BrowserContext, Page, WaitUntil};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

#[derive(Debug, Deserialize)]
#[serde(tag = "cmd")]
enum WorkerCommand {
    #[serde(rename = "navigate")]
    Navigate {
        url: String,
        #[serde(default, rename = "waitUntil")]
        wait_until: NavigationWaitUntil,
        /// Ceiling of the adaptive settle after load, as `fetch` runs it
        /// (`Page::settle`); 0 reads the page at load.
        #[serde(default, rename = "settleMs")]
        settle_ms: u64,
        /// `--settle-timers`: also wait for one-shot timers due this soon.
        #[serde(default, rename = "settleTimersMs")]
        settle_timers_ms: u64,
    },
    #[serde(rename = "evaluate")]
    Evaluate { expression: String },
    #[serde(rename = "title")]
    Title,
    #[serde(rename = "dump_html")]
    DumpHtml,
    #[serde(rename = "dump_text")]
    DumpText,
    #[serde(rename = "shutdown")]
    Shutdown,
}

/// Keep the wire protocol strict: WaitUntil::from_str falls back to Load.
#[derive(Debug, Default, Deserialize)]
enum NavigationWaitUntil {
    #[default]
    #[serde(rename = "load")]
    Load,
    #[serde(rename = "domcontentloaded")]
    DomContentLoaded,
    #[serde(rename = "networkidle0")]
    NetworkIdle0,
    #[serde(rename = "networkidle2")]
    NetworkIdle2,
}

impl From<NavigationWaitUntil> for WaitUntil {
    fn from(value: NavigationWaitUntil) -> Self {
        match value {
            NavigationWaitUntil::Load => Self::Load,
            NavigationWaitUntil::DomContentLoaded => Self::DomContentLoaded,
            NavigationWaitUntil::NetworkIdle0 => Self::NetworkIdle0,
            NavigationWaitUntil::NetworkIdle2 => Self::NetworkIdle2,
        }
    }
}

/// Time one `evaluate` command may take, its synchronous run and the wait for
/// a returned promise together. Matches the CDP await timeout; the scrape
/// parent's own `--timeout` still bounds the whole URL.
const EVALUATE_BUDGET: std::time::Duration = std::time::Duration::from_secs(30);

#[derive(Debug, Serialize)]
struct WorkerResponse {
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

impl WorkerResponse {
    fn success(result: serde_json::Value) -> Self {
        WorkerResponse { ok: true, result: Some(result), error: None }
    }
    fn error(msg: String) -> Self {
        WorkerResponse { ok: false, result: None, error: Some(msg) }
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter("warn")
        .with_writer(std::io::stderr)
        .init();

    let proxy = std::env::var("INBROWSE_PROXY")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let stealth = std::env::var("INBROWSE_STEALTH")
        .map(|v| matches!(v.trim(), "1" | "true" | "yes" | "on"))
        .unwrap_or(false);
    let obey_robots = std::env::var("INBROWSE_OBEY_ROBOTS")
        .map(|v| matches!(v.trim(), "1" | "true" | "yes" | "on"))
        .unwrap_or(false);
    let user_agent = std::env::var("INBROWSE_USER_AGENT")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let mut context =
        BrowserContext::with_full_options("worker".to_string(), proxy, stealth, user_agent);
    context.obey_robots = obey_robots;
    let context = Arc::new(context);
    let mut page = Page::new("page-1".to_string(), context);

    let stdin = tokio::io::stdin();
    let mut stdout = tokio::io::stdout();
    let mut reader = BufReader::new(stdin);
    let mut line = String::new();

    loop {
        line.clear();
        match reader.read_line(&mut line).await {
            Ok(0) => break,
            Ok(_) => {}
            Err(e) => {
                eprintln!("Worker stdin error: {}", e);
                break;
            }
        }

        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let cmd: WorkerCommand = match serde_json::from_str(trimmed) {
            Ok(c) => c,
            Err(e) => {
                let resp = WorkerResponse::error(format!("Invalid command: {}", e));
                let mut out = serde_json::to_string(&resp).unwrap();
                out.push('\n');
                let _ = stdout.write_all(out.as_bytes()).await;
                let _ = stdout.flush().await;
                continue;
            }
        };

        let resp = match cmd {
            WorkerCommand::Navigate { url, wait_until, settle_ms, settle_timers_ms } => {
                match page.navigate_with_wait(&url, wait_until.into()).await {
                    Ok(()) => {
                        // Navigation clears the event list and records the main
                        // Document response before scripts and child frames.
                        // Its status is the final response after HTTP redirects.
                        // Zero denotes a blocked request, not an HTTP response.
                        let status = page.network_events.iter()
                            .find(|event| event.resource_type == "Document")
                            .filter(|event| {
                                event.status != 0
                                    && (event.url.starts_with("http://") || event.url.starts_with("https://"))
                            })
                            .map(|event| event.status);
                        // The page `scrape --eval` reads is the settled one
                        // `fetch --eval` reads, not the page at load.
                        page.set_settle_timer_horizon(std::time::Duration::from_millis(settle_timers_ms));
                        page.settle(settle_ms).await;
                        // Scripts a time limit stopped (Page::take_script_warnings);
                        // the scrape parent copies them into the URL's record.
                        WorkerResponse::success(serde_json::json!({
                            "title": page.title,
                            "url": page.url_string(),
                            "status": status,
                            "warnings": page.take_script_warnings(),
                        }))
                    },
                    Err(e) => WorkerResponse::error(e.to_string()),
                }
            }
            WorkerCommand::Evaluate { expression } => {
                // Same evaluation as `fetch --eval` and MCP browser_evaluate
                // (inbrowse Page::evaluate_checked): completion values, a
                // returned promise awaited (issue #693), and one watchdog
                // bounding the run and the wait. A thrown error is an error
                // response, which the scrape parent reports per URL.
                match page.evaluate_checked(&expression, EVALUATE_BUDGET).await {
                    Ok(value) => WorkerResponse::success(value),
                    Err(error) => WorkerResponse::error(error.to_string()),
                }
            }
            WorkerCommand::Title => {
                WorkerResponse::success(serde_json::json!(page.title))
            }
            WorkerCommand::DumpHtml => {
                let html = page.with_dom(|dom| {
                    if let Ok(Some(html_node)) = dom.query_selector("html") {
                        dom.outer_html(html_node)
                    } else {
                        dom.inner_html(dom.document())
                    }
                }).unwrap_or_default();
                WorkerResponse::success(serde_json::json!(html))
            }
            WorkerCommand::DumpText => {
                let text = page.with_dom(|dom| {
                    if let Ok(Some(body)) = dom.query_selector("body") {
                        dom.text_content(body)
                    } else {
                        String::new()
                    }
                }).unwrap_or_default();
                WorkerResponse::success(serde_json::json!(text))
            }
            WorkerCommand::Shutdown => {
                let resp = WorkerResponse::success(serde_json::json!("bye"));
                let mut out = serde_json::to_string(&resp).unwrap();
                out.push('\n');
                let _ = stdout.write_all(out.as_bytes()).await;
                let _ = stdout.flush().await;
                break;
            }
        };

        let mut out = serde_json::to_string(&resp).unwrap();
        out.push('\n');
        let _ = stdout.write_all(out.as_bytes()).await;
        let _ = stdout.flush().await;
    }
}
