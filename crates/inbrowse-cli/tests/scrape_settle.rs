//! `scrape --eval` reads the page after the same adaptive settle as
//! `fetch --eval`, and `--settle-timers` extends it (#13).

use std::process::Command;

/// Each timer appends its delay to `#a`. The later timer is seconds out, so a
/// slow run of the default settle on a loaded host still ends before it.
const TIMERS_PAGE: &str = "data:text/html,<div id=a></div><script>\
for (const ms of [100, 4000]) setTimeout(() => a.textContent += ms + ' ', ms)\
</script>";

fn scrape_eval(extra: &[&str]) -> serde_json::Value {
    let mut command = Command::new(env!("CARGO_BIN_EXE_inbrowse"));
    for name in ["INBROWSE_PROXY", "HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "http_proxy", "https_proxy", "all_proxy"] {
        command.env_remove(name);
    }
    let output = command
        .env("INBROWSE_STEALTH", "0")
        .args(["scrape", TIMERS_PAGE, "--quiet", "--eval", "a.textContent.trim()"])
        .args(extra)
        .output()
        .expect("run inbrowse scrape");
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    report["results"][0]["eval"].clone()
}

#[test]
fn scrape_evaluates_the_settled_page() {
    assert_eq!(scrape_eval(&[]), serde_json::json!("100"), "a near-term timer has run");
}

#[test]
fn scrape_settle_timers_waits_for_later_timers() {
    assert_eq!(scrape_eval(&["--settle-timers", "4500"]), serde_json::json!("100 4000"));
}
