//! `scrape` takes its URL list from positional arguments, from stdin when an
//! argument is `-`, and from `--file <PATH|->` (inbrowse-cli main.rs
//! `scrape_url_list`), with the same blank-line and `#` comment rules as
//! `fetch --file`.

use std::io::Write;
use std::process::{Command, Stdio};

fn cli() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_inbrowse"));
    for name in [
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

fn scrape_with_stdin(args: &[&str], stdin: &str) -> serde_json::Value {
    let mut child = cli()
        .arg("scrape")
        .args(args)
        .args(["--quiet", "--eval", "location.href"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

fn urls(report: &serde_json::Value) -> Vec<String> {
    let mut urls: Vec<String> = report["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|result| result["url"].as_str().unwrap().to_string())
        .collect();
    urls.sort();
    urls
}

#[test]
fn a_dash_reads_the_url_list_from_stdin() {
    let report = scrape_with_stdin(
        &["-"],
        "about:blank#one\n\n  # a comment\n about:blank#two \n",
    );
    assert_eq!(report["total_urls"], 2, "{report}");
    assert_eq!(urls(&report), ["about:blank#one", "about:blank#two"]);
}

#[test]
fn file_dash_reads_stdin_and_combines_with_positional_urls() {
    let report = scrape_with_stdin(&["about:blank#arg", "--file", "-"], "about:blank#stdin\n");
    assert_eq!(urls(&report), ["about:blank#arg", "about:blank#stdin"]);
}

#[test]
fn file_reads_a_url_list() {
    let path =
        std::env::temp_dir().join(format!("inbrowse_scrape_urls_{}.txt", std::process::id()));
    std::fs::write(&path, "# list\nabout:blank#file\n").unwrap();
    let report = scrape_with_stdin(&["--file", path.to_str().unwrap()], "");
    std::fs::remove_file(&path).ok();
    assert_eq!(urls(&report), ["about:blank#file"]);
}

#[test]
fn stdin_cannot_be_read_twice() {
    let output = cli()
        .args(["scrape", "-", "--file", "-", "--quiet"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("stdin"));
}
