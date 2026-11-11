//! A local HTTP server and a `fetch` runner for the page-extraction tests.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::process::{Command, Output};
use std::sync::Arc;
use std::thread;

/// What the server answers for one path.
pub struct Response {
    status: u16,
    content_type: &'static str,
    location: Option<String>,
    body: String,
}

impl Response {
    pub fn html(body: &str) -> Self {
        Self { status: 200, content_type: "text/html; charset=utf-8", location: None, body: body.to_string() }
    }

    pub fn status(mut self, status: u16) -> Self {
        self.status = status;
        self
    }

    /// A 302 to `location`.
    pub fn redirect(location: &str) -> Self {
        Self { status: 302, content_type: "text/plain", location: Some(location.to_string()), body: String::new() }
    }
}

/// Serves `routes` on a free local port for the rest of the test process;
/// any other path is a 404.
pub struct Server {
    address: String,
}

impl Server {
    pub fn start(routes: Vec<(&str, Response)>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind fixture server");
        let address = listener.local_addr().expect("fixture address").to_string();
        let routes: Arc<HashMap<String, Response>> =
            Arc::new(routes.into_iter().map(|(path, response)| (path.to_string(), response)).collect());
        thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let routes = Arc::clone(&routes);
                thread::spawn(move || serve(stream, &routes));
            }
        });
        Self { address }
    }

    pub fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.address)
    }
}

fn serve(mut stream: std::net::TcpStream, routes: &HashMap<String, Response>) {
    let mut request = Vec::new();
    let mut chunk = [0_u8; 1024];
    while !request.windows(4).any(|window| window == b"\r\n\r\n") {
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => return,
            Ok(count) => request.extend_from_slice(&chunk[..count]),
        }
    }
    let request = String::from_utf8_lossy(&request);
    let target = request.split_whitespace().nth(1).unwrap_or("/");
    let path = target.split('?').next().unwrap_or("/");
    let response = routes.get(path);
    let (status, content_type, location, body) = match response {
        Some(response) => (response.status, response.content_type, response.location.as_deref(), response.body.as_str()),
        None => (404, "text/plain", None, "not found"),
    };
    let location = location.map(|location| format!("Location: {location}\r\n")).unwrap_or_default();
    let head = format!(
        "HTTP/1.1 {status} X\r\nContent-Type: {content_type}\r\n{location}Content-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(body.as_bytes());
}

/// Runs `inbrowse fetch <url> --quiet --allow-private-network <args>`.
pub fn fetch_output(url: &str, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_inbrowse"))
        .args(["fetch", url, "--quiet", "--allow-private-network"])
        .args(args)
        .output()
        .expect("run inbrowse fetch")
}

/// The stdout of a fetch that must succeed.
pub fn fetch(url: &str, args: &[&str]) -> String {
    let output = fetch_output(url, args);
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    String::from_utf8_lossy(&output.stdout).trim_end().to_string()
}
