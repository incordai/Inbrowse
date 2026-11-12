#![cfg(feature = "render")]
//! A page opened from a `file:` URL loads its relative subresources and
//! frames like Chrome does, and a network page never reaches local files.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

const WHITE: [u8; 4] = [255, 255, 255, 255];
const RED: [u8; 4] = [255, 0, 0, 255];
const BLUE: [u8; 4] = [0, 0, 255, 255];
const LIME: [u8; 4] = [0, 255, 0, 255];
const ORANGE: [u8; 4] = [255, 128, 0, 255];

fn png(color: [u8; 4]) -> Vec<u8> {
    let mut bytes = Vec::new();
    image::RgbaImage::from_pixel(4, 4, image::Rgba(color))
        .write_to(
            &mut std::io::Cursor::new(&mut bytes),
            image::ImageFormat::Png,
        )
        .unwrap();
    bytes
}

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("inbrowse-file-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn file_url(path: &Path) -> String {
    url::Url::from_file_path(path).unwrap().to_string()
}

fn capture(page: &str, extra: &[&str]) -> image::RgbaImage {
    let out = std::env::temp_dir().join(format!(
        "inbrowse-file-shot-{}-{:x}.png",
        std::process::id(),
        page.len() ^ extra.len() << 8
    ));
    let output = Command::new(env!("CARGO_BIN_EXE_inbrowse"))
        .args(["fetch", page, "--screenshot"])
        .arg(&out)
        .args(["--wait", "1", "--timeout", "15", "--quiet"])
        .args(extra)
        .env("INBROWSE_SHOT_W", "120")
        .env("INBROWSE_SHOT_H", "90")
        .output()
        .expect("run inbrowse fetch screenshot");
    assert!(
        output.status.success(),
        "capture failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let shot = image::open(&out).expect("decode screenshot").to_rgba8();
    std::fs::remove_file(out).unwrap();
    shot
}

/// The four-tile page used by both the local and the network test.
fn tiles(img: &str, css: &str, frame: &str) -> String {
    format!(
        "<html><head><link rel=stylesheet href='{css}'></head>\
         <body style='margin:0'>\
         <img src='{img}' style='position:absolute;left:0;top:0;width:40px;height:40px'>\
         <div id=bg></div><div id=flat></div>\
         <iframe src='{frame}' style='position:absolute;left:0;top:40px;width:40px;height:40px;border:0'></iframe>\
         </body></html>"
    )
}

fn css(bg: &str) -> String {
    format!(
        "#bg{{position:absolute;left:40px;top:0;width:40px;height:40px;background:url('{bg}');background-size:40px 40px}}\
         #flat{{position:absolute;left:80px;top:0;width:40px;height:40px;background:#00ff00}}"
    )
}

const CHILD: &str = "<html><body style='margin:0;background:#ff8000'></body></html>";

#[test]
fn file_page_loads_its_image_stylesheet_background_and_iframe() {
    let dir = temp_dir("local");
    std::fs::write(dir.join("pic.png"), png([255, 0, 0, 255])).unwrap();
    std::fs::write(dir.join("bg.png"), png([0, 0, 255, 255])).unwrap();
    std::fs::write(dir.join("style.css"), css("bg.png")).unwrap();
    std::fs::write(dir.join("child.html"), CHILD).unwrap();
    std::fs::write(
        dir.join("page.html"),
        tiles("pic.png", "style.css", "child.html"),
    )
    .unwrap();

    let shot = capture(&file_url(&dir.join("page.html")), &[]);
    assert_eq!(shot.get_pixel(20, 20).0, RED, "relative <img>");
    assert_eq!(
        shot.get_pixel(60, 20).0,
        BLUE,
        "stylesheet background image"
    );
    assert_eq!(shot.get_pixel(100, 20).0, LIME, "linked stylesheet");
    assert_eq!(shot.get_pixel(20, 60).0, ORANGE, "file: iframe document");
    assert_eq!(shot.get_pixel(100, 70).0, WHITE, "nothing else painted");
    std::fs::remove_dir_all(dir).unwrap();
}

/// A one-route-table HTTP server on loopback; returns its base URL.
fn serve(files: Vec<(&'static str, &'static str, Vec<u8>)>) -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut head = [0u8; 2048];
            let n = stream.read(&mut head).unwrap_or(0);
            let head = String::from_utf8_lossy(&head[..n]).into_owned();
            let path = head.split_whitespace().nth(1).unwrap_or("/").to_string();
            let found = files.iter().find(|(route, _, _)| *route == path);
            let (status, kind, body) = match found {
                Some((_, kind, body)) => ("200 OK", *kind, body.clone()),
                None => ("404 Not Found", "text/plain", b"missing".to_vec()),
            };
            let _ = write!(
                stream,
                "HTTP/1.1 {status}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(&body);
        }
    });
    base
}

#[test]
fn network_page_never_loads_file_subresources_or_frames() {
    let dir = temp_dir("secret");
    std::fs::write(dir.join("pic.png"), png([255, 0, 0, 255])).unwrap();
    std::fs::write(dir.join("bg.png"), png([0, 0, 255, 255])).unwrap();
    std::fs::write(dir.join("child.html"), CHILD).unwrap();
    // `fetch` allows file access, so only the document origin keeps these out.
    let page = tiles(
        &file_url(&dir.join("pic.png")),
        "/style.css",
        &file_url(&dir.join("child.html")),
    );
    let sheet = css(&file_url(&dir.join("bg.png")));
    let base = serve(vec![
        ("/page.html", "text/html", page.into_bytes()),
        ("/style.css", "text/css", sheet.into_bytes()),
    ]);

    let shot = capture(&format!("{base}/page.html"), &["--allow-private-network"]);
    assert_ne!(
        shot.get_pixel(20, 20).0,
        RED,
        "file: <img> from a network page"
    );
    assert_ne!(
        shot.get_pixel(60, 20).0,
        BLUE,
        "file: css background from a network page"
    );
    assert_eq!(
        shot.get_pixel(100, 20).0,
        LIME,
        "the network stylesheet still applies"
    );
    assert_ne!(
        shot.get_pixel(20, 60).0,
        ORANGE,
        "file: iframe from a network page"
    );
    std::fs::remove_dir_all(dir).unwrap();
}
