// A runtime's time zone override affects only that runtime. ICU's default zone is
// process-wide, so these tests interleave runtimes on one thread and run them on
// several threads. Its own test binary: TZ must be set before the first isolate
// in the process is created.

use inbrowse_js::runtime::InbrowseJsRuntime;
use inbrowse_js::timezone;
use serde_json::{json, Value};

const PROBE: &str = "[Intl.DateTimeFormat().resolvedOptions().timeZone, \
    new Date(2026, 0, 15).getTimezoneOffset(), \
    new Date(2026, 6, 15).getTimezoneOffset(), \
    new Date(0).toString()]";

fn process_zone() {
    std::env::set_var("TZ", "America/New_York");
}

fn runtime(zone: Option<&str>) -> InbrowseJsRuntime {
    let mut rt = InbrowseJsRuntime::new();
    rt.set_dom(inbrowse_dom::parse_html("<body></body>"));
    rt.run_page_init();
    rt.set_time_zone_override(zone);
    rt
}

fn probe(rt: &mut InbrowseJsRuntime) -> Value {
    rt.evaluate(PROBE).unwrap()
}

fn new_york() -> Value {
    json!([
        "America/New_York",
        300,
        240,
        "Wed Dec 31 1969 19:00:00 GMT-0500 (Eastern Standard Time)"
    ])
}

fn tokyo() -> Value {
    json!([
        "Asia/Tokyo",
        -540,
        -540,
        "Thu Jan 01 1970 09:00:00 GMT+0900 (Japan Standard Time)"
    ])
}

#[test]
fn override_applies_to_date_and_intl() {
    process_zone();
    let mut page = runtime(Some("Asia/Tokyo"));
    assert_eq!(probe(&mut page), tokyo());
}

#[test]
fn runtimes_on_one_thread_keep_their_own_zone() {
    process_zone();
    let mut plain = runtime(None);
    let mut japan = runtime(Some("Asia/Tokyo"));
    let mut auckland = runtime(Some("Pacific/Auckland"));
    for _ in 0..3 {
        assert_eq!(probe(&mut plain), new_york());
        assert_eq!(probe(&mut japan), tokyo());
        let seen = probe(&mut auckland);
        assert_eq!(
            seen.as_array().unwrap()[..3],
            [json!("Pacific/Auckland"), json!(-780), json!(-720)]
        );
    }
}

#[test]
fn date_objects_follow_a_later_override_and_its_removal() {
    process_zone();
    let mut page = runtime(None);
    page.evaluate("globalThis.when = new Date(0); when.getHours()")
        .unwrap();
    page.set_time_zone_override(Some("Asia/Tokyo"));
    assert_eq!(page.evaluate("when.getHours()").unwrap(), json!(9));
    page.set_time_zone_override(None);
    assert_eq!(page.evaluate("when.getHours()").unwrap(), json!(19));
}

#[test]
fn event_loop_callbacks_run_in_the_page_zone() {
    process_zone();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let mut plain = runtime_for_loop(None);
        let mut japan = runtime_for_loop(Some("Asia/Tokyo"));
        let (plain_done, japan_done) = tokio::join!(plain.run_event_loop(), japan.run_event_loop());
        plain_done.unwrap();
        japan_done.unwrap();
        assert_eq!(plain.evaluate("seen").unwrap(), json!(300));
        assert_eq!(japan.evaluate("seen").unwrap(), json!(-540));
    });
}

fn runtime_for_loop(zone: Option<&str>) -> InbrowseJsRuntime {
    let mut rt = runtime(zone);
    rt.execute_script(
        "timer",
        "setTimeout(() => { globalThis.seen = new Date(2026, 0, 15).getTimezoneOffset(); }, 1)",
    )
    .unwrap();
    rt
}

#[test]
fn threads_never_read_each_others_zone() {
    process_zone();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
    let (done, finished) = std::sync::mpsc::channel();
    let spawn = |zone: Option<&'static str>, expected: Value| {
        let barrier = barrier.clone();
        let done = done.clone();
        std::thread::spawn(move || {
            let outcome = std::panic::catch_unwind(|| {
                let mut page = runtime(zone);
                barrier.wait();
                for round in 0..300 {
                    assert_eq!(probe(&mut page), expected, "round {round}");
                }
            });
            let _ = done.send(outcome.is_ok());
        });
    };
    spawn(Some("Asia/Tokyo"), tokyo());
    spawn(None, new_york());
    spawn(Some("Asia/Tokyo"), tokyo());
    // A lock cycle in the zone gate would hang here; fail instead of waiting.
    for _ in 0..3 {
        let ok = finished
            .recv_timeout(std::time::Duration::from_secs(30))
            .expect("threads deadlocked in the zone gate");
        assert!(ok, "a thread saw the wrong zone");
    }
}

#[test]
fn zone_ids_are_validated_by_icu() {
    process_zone();
    assert!(timezone::is_known_zone("Asia/Tokyo"));
    assert!(timezone::is_known_zone("America/Los_Angeles"));
    assert!(!timezone::is_known_zone("Foo/Bar"));
    assert!(!timezone::is_known_zone("Etc/Unknown"));
    assert!(!timezone::is_known_zone(""));
}

#[test]
fn offsets_include_daylight_saving_without_touching_the_default() {
    process_zone();
    let january = 1_768_435_200_000.0; // 2026-01-15T00:00:00Z
    let july = 1_784_073_600_000.0; // 2026-07-15T00:00:00Z
    assert_eq!(
        timezone::utc_offset_ms("America/New_York", january),
        Some(-5 * 3_600_000)
    );
    assert_eq!(
        timezone::utc_offset_ms("America/New_York", july),
        Some(-4 * 3_600_000)
    );
    assert_eq!(
        timezone::utc_offset_ms("Asia/Tokyo", july),
        Some(9 * 3_600_000)
    );
    assert_eq!(timezone::utc_offset_ms("Foo/Bar", july), None);
    assert_eq!(timezone::process_default().as_ref(), "America/New_York");
    let mut plain = runtime(None);
    assert_eq!(probe(&mut plain), new_york());
}
