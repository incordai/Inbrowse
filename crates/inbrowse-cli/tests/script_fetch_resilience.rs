//! A script that cannot be fetched, or does not arrive within the page
//! budget, is reported on stderr by name instead of silently producing a page
//! that never ran it. A connection dropped before any response is retried, as
//! it is on a loaded machine.

use std::io::{Read, Write};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

const PAGE: &str = "<html><body>\
    <script src='/fast.js'></script>\
    <script src='/flaky.js'></script>\
    <script src='/dead.js'></script>\
    <script src='/slow.js'></script>\
    </body></html>";

/// Serves PAGE and four scripts on loopback: `flaky.js` drops its first
/// connection, `dead.js` drops every connection, `slow.js` answers after
/// `slow_secs`.
fn serve(slow_secs: u64) -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let flaky_hits = Arc::new(AtomicUsize::new(0));
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let flaky_hits = flaky_hits.clone();
            std::thread::spawn(move || {
                let mut head = [0u8; 2048];
                let n = stream.read(&mut head).unwrap_or(0);
                let head = String::from_utf8_lossy(&head[..n]).into_owned();
                let path = head.split_whitespace().nth(1).unwrap_or("/").to_string();
                let (kind, body) = match path.as_str() {
                    "/page.html" => ("text/html", PAGE),
                    "/fast.js" => ("application/javascript", "window.fast = 1;"),
                    "/flaky.js" => {
                        if flaky_hits.fetch_add(1, Ordering::SeqCst) == 0 {
                            return;
                        }
                        ("application/javascript", "window.flaky = 1;")
                    }
                    "/dead.js" => return,
                    "/slow.js" => {
                        std::thread::sleep(Duration::from_secs(slow_secs));
                        ("application/javascript", "window.slow = 1;")
                    }
                    _ => ("text/plain", ""),
                };
                let _ = write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
            });
        }
    });
    base
}

fn fetch(base: &str, timeout: &str) -> (String, String, Duration) {
    let started = Instant::now();
    let output = Command::new(env!("CARGO_BIN_EXE_inbrowse"))
        .args(["fetch", &format!("{base}/page.html"), "--allow-private-network", "--quiet"])
        .args(["--timeout", timeout, "--wait", "0", "--eval"])
        .arg("JSON.stringify([window.fast, window.flaky, window.slow])")
        .output()
        .expect("run inbrowse fetch");
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    (
        String::from_utf8_lossy(&output.stdout).trim().to_string(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
        started.elapsed(),
    )
}

#[test]
fn a_dropped_script_connection_is_retried_and_a_dead_script_is_reported() {
    let base = serve(0);
    let (stdout, stderr, _) = fetch(&base, "20");
    assert_eq!(stdout, "[1,1,null]", "fast and retried flaky scripts run; stderr: {stderr}");
    assert!(
        stderr.contains("Warning:") && stderr.contains("/dead.js"),
        "the script that never loaded must be reported: {stderr}"
    );
    assert!(!stderr.contains("/flaky.js"), "the retried script is not a failure: {stderr}");
}

#[test]
fn a_script_that_misses_the_page_budget_is_reported_by_name() {
    let base = serve(30);
    let (_, stderr, elapsed) = fetch(&base, "4");
    assert!(
        stderr.contains("Warning:") && stderr.contains("/slow.js"),
        "the script that missed the budget must be reported: {stderr}"
    );
    assert!(!stderr.contains("/fast.js"), "scripts that arrived are not blamed: {stderr}");
    assert!(elapsed < Duration::from_secs(15), "the budget bounds the wait: {elapsed:?}");
}
