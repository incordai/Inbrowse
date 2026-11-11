#![cfg(feature = "render")]

use std::io::{Read, Write};
use std::process::Command;

const RED: [u8; 4] = [255, 0, 0, 255];
const BLUE: [u8; 4] = [0, 0, 255, 255];
const WHITE: [u8; 4] = [255, 255, 255, 255];

/// A 4x4 tile, left half red and right half blue.
fn tile() -> Vec<u8> {
    let mut bytes = Vec::new();
    image::RgbaImage::from_fn(4, 4, |x, _| image::Rgba(if x < 2 { RED } else { BLUE }))
        .write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Png)
        .unwrap();
    bytes
}

fn serve(page: &'static str) -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let tile = tile();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut head = [0u8; 2048];
            let read = stream.read(&mut head).unwrap_or(0);
            let head = String::from_utf8_lossy(&head[..read]);
            let path = head.split_whitespace().nth(1).unwrap_or("/");
            let (kind, body) = if path.starts_with("/tile.png") {
                ("image/png", tile.clone())
            } else {
                ("text/html", page.as_bytes().to_vec())
            };
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(&body);
        }
    });
    base
}

fn capture(url: &str) -> image::RgbaImage {
    let path = std::env::temp_dir().join(format!("inbrowse-bg-tiling-{}.png", std::process::id()));
    let output = Command::new(env!("CARGO_BIN_EXE_inbrowse"))
        .args(["fetch", url, "--allow-private-network", "--screenshot"])
        .arg(&path)
        .args(["--wait", "1", "--timeout", "20", "--quiet"])
        .env("INBROWSE_SHOT_W", "80")
        .env("INBROWSE_SHOT_H", "40")
        .output()
        .expect("run inbrowse fetch screenshot");
    assert!(
        output.status.success(),
        "capture failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let image = image::open(&path).expect("decode screenshot").to_rgba8();
    std::fs::remove_file(path).ok();
    image
}

#[test]
fn linked_background_image_tiles_without_a_background_size() {
    let base = serve(
        "<html style='margin:0'><body style='margin:0;background:white'>\
         <div style='width:60px;height:20px;background:url(/tile.png)'></div></body></html>",
    );
    let image = capture(&format!("{base}/index.html"));
    assert_eq!(image.get_pixel(0, 0).0, RED);
    assert_eq!(image.get_pixel(2, 0).0, BLUE);
    assert_eq!(image.get_pixel(4, 0).0, RED, "second tile across");
    assert_eq!(image.get_pixel(58, 1).0, BLUE, "last tile across");
    assert_eq!(image.get_pixel(0, 16).0, RED, "fifth tile down");
    assert_eq!(image.get_pixel(60, 0).0, WHITE, "nothing past the box");
    assert_eq!(image.get_pixel(0, 20).0, WHITE);
}

#[test]
fn linked_background_image_honours_no_repeat_and_repeat_x() {
    let base = serve(
        "<html style='margin:0'><body style='margin:0;background:white'>\
         <div style='width:40px;height:12px;background:url(/tile.png) no-repeat'></div>\
         <div style='width:40px;height:12px;background:url(/tile.png) repeat-x'></div></body></html>",
    );
    let image = capture(&format!("{base}/index.html"));
    assert_eq!(image.get_pixel(0, 0).0, RED);
    assert_eq!(image.get_pixel(4, 0).0, WHITE, "no-repeat has no second tile");
    assert_eq!(image.get_pixel(0, 5).0, WHITE);
    assert_eq!(image.get_pixel(4, 12).0, RED, "repeat-x second tile");
    assert_eq!(image.get_pixel(0, 16).0, WHITE, "repeat-x has no second row");
}
