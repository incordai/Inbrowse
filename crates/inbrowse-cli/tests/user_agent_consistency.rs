//! A raw download (`fetch --dump original`) must announce the same browser a
//! page sees in `navigator.userAgent`. The plain HTTP client used to default
//! to a Linux Chrome UA while pages reported Chrome on Windows, a mismatch a
//! site can cross-check (inbrowse-net client.rs `DEFAULT_USER_AGENT`).

use std::io::{Read, Write};
use std::net::TcpListener;
use std::process::Command;
use std::sync::mpsc;

fn cli() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_inbrowse"));
    for name in ["INBROWSE_PROXY", "HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "http_proxy", "https_proxy", "all_proxy"] {
        command.env_remove(name);
    }
    command.env("INBROWSE_STEALTH", "0");
    command
}

/// Serve one request and hand back its User-Agent header.
fn serve_once_capturing_user_agent() -> (String, mpsc::Receiver<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let (sent, received) = mpsc::channel();
    std::thread::spawn(move || {
        if let Ok((mut socket, _)) = listener.accept() {
            let mut request = [0u8; 4096];
            let read = socket.read(&mut request).unwrap_or(0);
            let head = String::from_utf8_lossy(&request[..read]).to_string();
            let user_agent = head
                .lines()
                .find_map(|line| line.split_once(':').filter(|(k, _)| k.eq_ignore_ascii_case("user-agent")))
                .map(|(_, v)| v.trim().to_string())
                .unwrap_or_default();
            let _ = sent.send(user_agent);
            let body = b"ok";
            let _ = socket.write_all(
                format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).as_bytes(),
            );
            let _ = socket.write_all(body);
        }
    });
    (format!("http://{addr}/"), received)
}

#[test]
fn raw_download_user_agent_matches_the_page_profile() {
    let page = cli()
        .args(["fetch", "about:blank", "--quiet", "--eval", "navigator.userAgent"])
        .output()
        .unwrap();
    assert!(page.status.success(), "{}", String::from_utf8_lossy(&page.stderr));
    let navigator_ua = String::from_utf8_lossy(&page.stdout).trim().trim_matches('"').to_string();

    let (url, user_agent) = serve_once_capturing_user_agent();
    let raw = cli()
        .args(["--allow-private-network", "fetch", &url, "--dump", "original", "--quiet"])
        .output()
        .unwrap();
    assert!(raw.status.success(), "{}", String::from_utf8_lossy(&raw.stderr));
    let sent_ua = user_agent.recv_timeout(std::time::Duration::from_secs(10)).unwrap();

    assert!(sent_ua.contains("Windows NT 10.0"), "raw download sent {sent_ua:?}");
    assert_eq!(sent_ua, navigator_ua, "the request header and navigator.userAgent disagree");
}

/// `--user-agent` reaches the raw download whether or not `--stealth` swaps in
/// the impersonating client.
#[test]
fn raw_download_honours_user_agent_with_and_without_stealth() {
    for stealth in [false, true] {
        let (url, user_agent) = serve_once_capturing_user_agent();
        let mut command = cli();
        command.args(["--allow-private-network", "--user-agent", "RawDownloadTest/1.0"]);
        if stealth {
            command.arg("--stealth");
        }
        let raw = command.args(["fetch", &url, "--dump", "original", "--quiet"]).output().unwrap();
        assert!(raw.status.success(), "stealth={stealth}: {}", String::from_utf8_lossy(&raw.stderr));
        let sent_ua = user_agent.recv_timeout(std::time::Duration::from_secs(10)).unwrap();
        assert_eq!(sent_ua, "RawDownloadTest/1.0", "stealth={stealth}");
    }
}
