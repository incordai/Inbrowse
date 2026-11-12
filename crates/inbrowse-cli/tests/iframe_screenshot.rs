#![cfg(feature = "render")]

use std::process::Command;

fn capture(name: &str, page: &str, width: &str, height: &str) -> image::RgbaImage {
    let path =
        std::env::temp_dir().join(format!("inbrowse-iframe-{name}-{}.png", std::process::id()));
    let output = Command::new(env!("CARGO_BIN_EXE_inbrowse"))
        .args(["fetch", page, "--screenshot"])
        .arg(&path)
        .args(["--wait", "0", "--timeout", "10", "--quiet"])
        .env("INBROWSE_SHOT_W", width)
        .env("INBROWSE_SHOT_H", height)
        .output()
        .expect("run inbrowse fetch screenshot");
    assert!(
        output.status.success(),
        "capture failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let screenshot = image::open(&path)
        .expect("decode screenshot PNG")
        .to_rgba8();
    std::fs::remove_file(path).expect("remove screenshot");
    screenshot
}

const WHITE: [u8; 4] = [255, 255, 255, 255];
const RED: [u8; 4] = [255, 0, 0, 255];
const LIME: [u8; 4] = [0, 255, 0, 255];

#[test]
fn screenshot_paints_srcdoc_iframe_inside_its_content_box() {
    let url = concat!(
        "data:text/html,<html style=\"margin:0\"><body style=\"margin:0;background:white\">",
        "<iframe style=\"position:absolute;left:20px;top:10px;width:60px;height:40px;border:0\" ",
        "srcdoc=\"<body style='margin:0;background:red'></body>\"></iframe>",
        "</body></html>"
    );
    let image = capture("srcdoc", url, "120", "80");
    assert_eq!(image.get_pixel(50, 30).0, RED, "frame center");
    assert_eq!(image.get_pixel(20, 10).0, RED, "frame top-left pixel");
    assert_eq!(image.get_pixel(79, 49).0, RED, "frame bottom-right pixel");
    assert_eq!(image.get_pixel(19, 30).0, WHITE, "left of the frame");
    assert_eq!(image.get_pixel(80, 30).0, WHITE, "right of the frame");
    assert_eq!(image.get_pixel(50, 9).0, WHITE, "above the frame");
    assert_eq!(image.get_pixel(50, 50).0, WHITE, "below the frame");
}

#[test]
fn screenshot_paints_data_url_iframe() {
    let url = concat!(
        "data:text/html,<html style=\"margin:0\"><body style=\"margin:0;background:white\">",
        "<iframe style=\"position:absolute;left:10px;top:10px;width:50px;height:30px;border:0\" ",
        "src=\"data:text/html,<body style='margin:0;background:lime'></body>\"></iframe>",
        "</body></html>"
    );
    let image = capture("data-src", url, "100", "60");
    assert_eq!(image.get_pixel(30, 25).0, LIME);
    assert_eq!(image.get_pixel(70, 25).0, WHITE);
}

#[test]
fn screenshot_insets_iframe_content_by_border_and_padding() {
    let url = concat!(
        "data:text/html,<html style=\"margin:0\"><body style=\"margin:0;background:white\">",
        "<iframe style=\"position:absolute;left:10px;top:10px;width:40px;height:30px;",
        "border:5px solid blue;padding:3px;background:yellow\" ",
        "srcdoc=\"<body style='margin:0;background:lime'></body>\"></iframe>",
        "</body></html>"
    );
    let image = capture("insets", url, "100", "80");
    // Border box starts at 10; border 5 and padding 3 put the content box at 18.
    assert_eq!(image.get_pixel(12, 30).0, [0, 0, 255, 255], "border");
    assert_eq!(
        image.get_pixel(16, 30).0,
        [255, 255, 0, 255],
        "padding shows its own background"
    );
    assert_eq!(image.get_pixel(18, 30).0, LIME, "first content column");
    assert_eq!(image.get_pixel(57, 30).0, LIME, "last content column");
    assert_eq!(
        image.get_pixel(58, 30).0,
        [255, 255, 0, 255],
        "right padding"
    );
    assert_eq!(image.get_pixel(30, 18).0, LIME, "first content row");
    assert_eq!(image.get_pixel(30, 47).0, LIME, "last content row");
    assert_eq!(
        image.get_pixel(30, 48).0,
        [255, 255, 0, 255],
        "bottom padding"
    );
}

#[test]
fn screenshot_clips_iframe_content_to_the_frame_and_lays_it_out_at_its_size() {
    // The child fills 100% of its own viewport. If it were laid out at the
    // parent's width the right half of the ruler would stay visible.
    let url = concat!(
        "data:text/html,<html style=\"margin:0\"><body style=\"margin:0;background:white\">",
        "<iframe style=\"position:absolute;left:10px;top:10px;width:40px;height:30px;border:0\" ",
        "srcdoc=\"<body style='margin:0'><div style='width:100%;height:10px;background:red'></div>",
        "<div style='width:500px;height:10px;background:lime'></div></body>\"></iframe>",
        "</body></html>"
    );
    let image = capture("clip", url, "120", "60");
    assert_eq!(
        image.get_pixel(49, 14).0,
        RED,
        "100% wide row ends at the frame edge"
    );
    assert_eq!(
        image.get_pixel(50, 14).0,
        WHITE,
        "row is not wider than the frame"
    );
    assert_eq!(
        image.get_pixel(49, 24).0,
        LIME,
        "overflowing row is clipped at the edge"
    );
    assert_eq!(
        image.get_pixel(50, 24).0,
        WHITE,
        "overflowing row does not leave the frame"
    );
    assert_eq!(image.get_pixel(30, 41).0, WHITE, "nothing below the frame");
}

#[test]
fn screenshot_paints_nested_iframes() {
    let url = concat!(
        "data:text/html,<html style=\"margin:0\"><body style=\"margin:0;background:white\">",
        "<iframe style=\"position:absolute;left:10px;top:10px;width:80px;height:60px;border:0\" ",
        "srcdoc=\"<body style='margin:0'><iframe style='position:absolute;left:20px;top:10px;",
        "width:30px;height:20px;border:0' srcdoc='<body style=margin:0;background:red></body>'>",
        "</iframe></body>\"></iframe>",
        "</body></html>"
    );
    let image = capture("nested", url, "120", "90");
    // Grandchild at (10 + 20, 10 + 10) inside the child frame.
    assert_eq!(image.get_pixel(40, 30).0, RED, "grandchild content");
    assert_eq!(image.get_pixel(29, 30).0, WHITE, "left of the grandchild");
    assert_eq!(image.get_pixel(60, 30).0, WHITE, "right of the grandchild");
}

#[test]
fn screenshot_without_iframes_is_unchanged_by_frame_painting() {
    let url = concat!(
        "data:text/html,<html style=\"margin:0\"><body style=\"margin:0;background:white\">",
        "<div style=\"width:20px;height:20px;background:lime\"></div></body></html>"
    );
    let image = capture("plain", url, "40", "40");
    assert_eq!(image.get_pixel(10, 10).0, LIME);
    assert_eq!(image.get_pixel(30, 30).0, WHITE);
}
