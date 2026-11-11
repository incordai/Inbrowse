//! `fetch` adaptive settle and one-shot timers (#13).

use std::process::{Command, Output};
use std::time::{Duration, Instant};

/// Each timer appends its delay to `#a`.
const TIMERS_PAGE: &str = "data:text/html,<div id=a></div><script>\
for (const ms of [100, 1000, 1500]) setTimeout(() => a.textContent += ms + ' ', ms)\
</script>";

fn fetch_text(extra: &[&str]) -> (Output, Duration) {
    let started = Instant::now();
    let output = Command::new(env!("CARGO_BIN_EXE_inbrowse"))
        .args(["fetch", TIMERS_PAGE, "--dump", "text"])
        .args(extra)
        .output()
        .expect("run inbrowse fetch");
    (output, started.elapsed())
}

#[test]
fn the_default_settle_leaves_long_timers_and_says_so() {
    let (output, _) = fetch_text(&[]);
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let text = String::from_utf8_lossy(&output.stdout);
    assert_eq!(text.trim(), "100", "only the near-term timer runs: {text:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("note: 2 timers pending (next in 0.") && stderr.contains("--settle-timers"),
        "the settle names what it left: {stderr}"
    );
}

#[test]
fn quiet_leaves_out_the_pending_timer_note() {
    let (output, _) = fetch_text(&["--quiet"]);
    assert!(output.status.success());
    assert!(!String::from_utf8_lossy(&output.stderr).contains("timers pending"));
}

#[test]
fn settle_timers_waits_for_timers_within_the_horizon() {
    let (output, elapsed) = fetch_text(&["--settle-timers", "2000", "--quiet"]);
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let text = String::from_utf8_lossy(&output.stdout);
    assert_eq!(text.trim(), "100 1000 1500", "{text:?}");
    assert!(elapsed < Duration::from_secs(5), "returns once the last timer ran: {elapsed:?}");
}

#[test]
fn settle_timers_stops_at_its_horizon() {
    let (output, _) = fetch_text(&["--settle-timers", "1200"]);
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "100 1000");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("note: 1 timer pending"), "{stderr}");
}

#[test]
fn settle_timers_and_a_fixed_wait_conflict() {
    let (output, _) = fetch_text(&["--settle-timers", "1000", "--wait", "2"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("cannot be used with"));
}
