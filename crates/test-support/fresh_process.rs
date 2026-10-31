// Run one test alone in a fresh copy of its test binary.
//
// `cargo test` runs a crate's unit tests as threads of one process, so a test
// that must configure process-wide, set-once state first (V8 flags, the font
// database) only works if no earlier test touched that state. A test calls
// `run_alone` with its own libtest path: in the parent the binary is re-run
// with `--exact <test>` and the call returns `true` once that child ran and
// passed exactly one test; in the child it returns `false` and the test body
// runs as the first thing in the process. Under nextest each test already has
// its own process, and the re-run only adds one exec.
//
// Included with `#[path]` at each crate root (obscura-browser, obscura-cdp,
// obscura-js, obscura-mcp, obscura-net client.rs, obscura-render with paint)
// and by the integration tests that change process state (obscura-cdp
// integration.rs, obscura-mcp http_page_progress.rs, obscura child_frame_scripts.rs
// and stealth_transport.rs). Callers: every test that sets an environment
// variable or other set-once process state.

const CHILD_ENV: &str = "OBSCURA_FRESH_PROCESS_TEST";

/// `test` is the libtest path without the crate name, e.g.
/// `runtime::tests::heap_limit_terminates_script_and_runtime_recovers`.
pub fn run_alone(test: &str) -> bool {
    if std::env::var(CHILD_ENV).as_deref() == Ok(test) {
        return false;
    }
    let exe = std::env::current_exe().expect("test binary path");
    let output = std::process::Command::new(exe)
        .args([test, "--exact", "--test-threads=1", "--nocapture"])
        .env(CHILD_ENV, test)
        .output()
        .expect("re-run the test binary");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    // A misspelled path runs zero tests and still exits 0, so the count is
    // checked as well as the status.
    assert!(
        output.status.success() && stdout.contains("test result: ok. 1 passed"),
        "{test} failed in its fresh process ({}):\n{stdout}\n{stderr}",
        output.status
    );
    true
}
