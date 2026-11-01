//! `inbrowse extract` end to end: local files, downloads, and the
//! private-network gate that downloads share with `fetch --dump original`.
#![cfg(feature = "extract")]

use std::io::{Read, Write};
use std::net::TcpListener;
use std::process::Command;

fn cli() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_inbrowse"));
    for name in [
        "INBROWSE_ALLOW_PRIVATE_NETWORK",
        "INBROWSE_PROXY",
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
        "http_proxy",
        "https_proxy",
        "all_proxy",
    ] {
        command.env_remove(name);
    }
    command.env("INBROWSE_STEALTH", "0");
    command
}

fn json_stdout(output: &std::process::Output) -> serde_json::Value {
    assert!(
        output.status.success(),
        "exit {:?}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("stdout is one JSON document")
}

/// Serve `body` once with `content_type`, return the URL.
fn serve_once(body: &'static [u8], content_type: &'static str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        if let Ok((mut socket, _)) = listener.accept() {
            let mut request = [0u8; 2048];
            let _ = socket.read(&mut request);
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = socket.write_all(head.as_bytes());
            let _ = socket.write_all(body);
        }
    });
    format!("http://{addr}/report")
}

#[test]
fn local_file_name_marks_csv() {
    let path = std::env::temp_dir().join(format!("inbrowse-extract-{}.csv", std::process::id()));
    std::fs::write(&path, "a,b\n1,2\n").unwrap();
    let output = cli().arg("extract").arg(&path).output().unwrap();
    std::fs::remove_file(&path).unwrap();
    let doc = json_stdout(&output);
    assert_eq!(doc["format"], "csv");
    assert_eq!(doc["text"], "a,b\n1,2");
}

#[test]
fn download_uses_the_response_content_type() {
    let url = serve_once(b"x,y\n3,4\n", "text/csv; charset=utf-8");
    let output = cli()
        .args(["--allow-private-network", "extract", &url])
        .output()
        .unwrap();
    let doc = json_stdout(&output);
    assert_eq!(doc["format"], "csv");
    assert_eq!(doc["sections"][0]["text"], "x,y\n3,4");
}

#[test]
fn private_network_download_is_refused_without_the_opt_in() {
    let url = serve_once(b"secret", "text/plain");
    let output = cli().args(["extract", &url]).output().unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let error = String::from_utf8(output.stderr).unwrap();
    assert!(error.contains("is not allowed"), "{error}");
}

#[test]
fn missing_file_and_unknown_binary_are_errors() {
    let missing = cli().args(["extract", "no-such-file.pdf"]).output().unwrap();
    assert!(!missing.status.success());
    assert!(String::from_utf8_lossy(&missing.stderr).contains("Failed to read"));

    let path = std::env::temp_dir().join(format!("inbrowse-extract-{}.bin", std::process::id()));
    std::fs::write(&path, [0u8, 159, 146, 150, 0]).unwrap();
    let binary = cli().arg("extract").arg(&path).output().unwrap();
    std::fs::remove_file(&path).unwrap();
    assert!(!binary.status.success());
    assert!(String::from_utf8_lossy(&binary.stderr).contains("unsupported document"));
}
