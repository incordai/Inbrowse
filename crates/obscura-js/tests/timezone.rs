// The page's timezone follows the process TZ on every platform. On Windows ICU
// ignored TZ and reported the host zone, so OBSCURA_TIMEZONE had no effect
// (runtime.rs `pin_icu_timezone_from_tz`). Its own test binary: TZ must be set
// before the first isolate in the process is created.

use obscura_js::runtime::ObscuraJsRuntime;
use serde_json::json;

#[test]
fn intl_and_date_follow_process_tz() {
    // Asia/Tokyo has no DST, so the offset is fixed at UTC+9 (-540 minutes).
    std::env::set_var("TZ", "Asia/Tokyo");
    let mut rt = ObscuraJsRuntime::new();
    rt.set_dom(obscura_dom::parse_html("<body></body>"));
    rt.run_page_init();
    assert_eq!(
        rt.evaluate(
            "[Intl.DateTimeFormat().resolvedOptions().timeZone, \
              new Date(2026, 0, 15).getTimezoneOffset(), \
              new Date(2026, 6, 15).getTimezoneOffset()]"
        )
        .unwrap(),
        json!(["Asia/Tokyo", -540, -540])
    );
}
