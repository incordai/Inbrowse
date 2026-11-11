#![cfg(feature = "render")]

use std::io::{Read, Write};
use std::process::Command;
use std::sync::{Arc, Mutex};

type Requests = Arc<Mutex<Vec<(String, String)>>>;

/// The `file:` URL the file-gate test points a frame image at.
static FILE_URL: Mutex<String> = Mutex::new(String::new());

fn png(color: [u8; 4]) -> Vec<u8> {
    let mut bytes = Vec::new();
    image::RgbaImage::from_pixel(4, 4, image::Rgba(color))
        .write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Png)
        .unwrap();
    bytes
}

/// Serves `route(path)` over loopback and records (path, raw request head).
fn serve(route: fn(&str) -> Option<(&'static str, Vec<u8>)>) -> (String, Requests) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let requests: Requests = Arc::default();
    let seen = requests.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let seen = seen.clone();
            std::thread::spawn(move || {
                let mut head = Vec::new();
                let mut chunk = [0u8; 2048];
                while !head.windows(4).any(|w| w == b"\r\n\r\n") {
                    match stream.read(&mut chunk) {
                        Ok(0) | Err(_) => return,
                        Ok(n) => head.extend_from_slice(&chunk[..n]),
                    }
                }
                let head = String::from_utf8_lossy(&head).into_owned();
                let path = head.split_whitespace().nth(1).unwrap_or("/").to_string();
                seen.lock().unwrap().push((path.clone(), head));
                let (status, kind, body) = match route(&path) {
                    Some((kind, body)) => ("200 OK", kind, body),
                    None => ("404 Not Found", "text/plain", b"missing".to_vec()),
                };
                let _ = write!(
                    stream,
                    "HTTP/1.1 {status}\r\nContent-Type: {kind}\r\nSet-Cookie: sid=abc; Path=/\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(&body);
            });
        }
    });
    (base, requests)
}

fn route(path: &str) -> Option<(&'static str, Vec<u8>)> {
    let page = |html: &str| Some(("text/html", html.as_bytes().to_vec()));
    let css = |css: &str| Some(("text/css", css.as_bytes().to_vec()));
    match path.split('?').next().unwrap() {
        "/index.html" => page(concat!(
            "<html style='margin:0'><body style='margin:0;background:white'>",
            "<iframe src='/frame.html' style='position:absolute;left:10px;top:10px;",
            "width:200px;height:120px;border:0'></iframe></body></html>"
        )),
        "/frame.html" => page(concat!(
            "<html><head><link rel='stylesheet' href='/frame.css'></head><body>",
            "<img id='i' src='/red.png'><div class='bg'></div>",
            "<p class='text'>text</p><div id='nested'></div></body></html>"
        )),
        "/frame.css" => css(concat!(
            "@import url('/imported.css');",
            "html,body{margin:0;background:#e8590c}",
            ".bg{position:absolute;left:60px;top:5px;width:30px;height:30px;background:url(/blue.png);background-size:30px 30px}",
            "@font-face{font-family:Probe;src:url(/probe.woff2)}",
            ".text{position:absolute;left:10px;top:60px;margin:0;font:16px Probe}"
        )),
        "/imported.css" => css("#i{position:absolute;left:10px;top:5px;width:40px;height:30px}"),
        "/red.png" => Some(("image/png", png([255, 0, 0, 255]))),
        "/blue.png" => Some(("image/png", png([0, 0, 255, 255]))),
        "/probe.woff2" => Some(("font/woff2", b"not a font".to_vec())),
        "/outer.html" => page(concat!(
            "<html style='margin:0'><body style='margin:0;background:white'>",
            "<iframe src='/middle.html' style='position:absolute;left:10px;top:10px;",
            "width:150px;height:100px;border:0'></iframe></body></html>"
        )),
        "/middle.html" => page(concat!(
            "<html><head><style>html,body{margin:0;background:#1971c2}</style></head><body>",
            "<iframe src='/inner.html' style='position:absolute;left:20px;top:10px;",
            "width:60px;height:40px;border:0'></iframe></body></html>"
        )),
        "/inner.html" => page(concat!(
            "<html><head><link rel='stylesheet' href='/inner.css'></head><body></body></html>"
        )),
        "/inner.css" => css("html,body{margin:0;background:#7048e8}"),
        "/file.html" => page(
            "<html style='margin:0'><body style='margin:0'><iframe src='/file-frame.html' \
             style='position:absolute;left:0;top:0;width:60px;height:70px;border:0'></iframe></body></html>",
        ),
        "/file-frame.html" => page(&format!(
            "<html><body style='margin:0'><img style='display:block;width:60px;height:40px' src='{}'>\
             <img style='display:block;width:60px;height:30px' src='/red.png'></body></html>",
            FILE_URL.lock().unwrap()
        )),
        _ => None,
    }
}

fn capture(args: &[&str], width: &str, height: &str) -> image::RgbaImage {
    static CAPTURES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let path = std::env::temp_dir().join(format!(
        "inbrowse-frame-res-{}-{}.png",
        std::process::id(),
        CAPTURES.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let output = Command::new(env!("CARGO_BIN_EXE_inbrowse"))
        .arg("fetch")
        .arg(args[0])
        .args(&args[1..])
        .arg("--screenshot")
        .arg(&path)
        .args(["--wait", "1", "--timeout", "20", "--quiet"])
        .env("INBROWSE_SHOT_W", width)
        .env("INBROWSE_SHOT_H", height)
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

fn evaluate(url: &str, expression: &str) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_inbrowse"))
        .args(["fetch", url, "--allow-private-network", "--wait", "1", "--timeout", "20", "--quiet"])
        .args(["--eval", expression])
        .output()
        .expect("run inbrowse fetch --eval");
    assert!(output.status.success(), "eval failed: {}", String::from_utf8_lossy(&output.stderr));
    String::from_utf8_lossy(&output.stdout).replace('\\', "")
}

#[test]
fn frame_computed_style_and_viewport_see_the_linked_sheets_and_the_iframe_box() {
    let (base, _requests) = serve(route);
    let out = evaluate(
        &format!("{base}/index.html"),
        "(() => { const w = document.querySelector('iframe').contentWindow;\
          const image = w.getComputedStyle(w.document.getElementById('i'));\
          return JSON.stringify([image.position, image.width,\
            w.getComputedStyle(w.document.body).backgroundColor,\
            w.innerWidth, w.innerHeight, w.matchMedia('(min-width:200px)').matches]); })()",
    );
    assert!(
        out.contains("[\"absolute\",\"40px\",\"rgb(232, 89, 12)\",200,120,true]"),
        "computed style inside the frame: {out}"
    );
}

fn asked_for(requests: &Requests, path: &str) -> Option<String> {
    requests
        .lock()
        .unwrap()
        .iter()
        .find(|(requested, _)| requested == path)
        .map(|(_, head)| head.clone())
}

#[test]
fn frame_linked_stylesheet_images_and_font_load_through_the_page_pipeline() {
    let (base, requests) = serve(route);
    let url = format!("{base}/index.html");
    let image = capture(&[&url, "--allow-private-network"], "240", "150");

    let seen: Vec<String> = requests.lock().unwrap().iter().map(|(p, _)| p.clone()).collect();
    for path in ["/frame.css", "/imported.css", "/red.png", "/blue.png", "/probe.woff2"] {
        assert!(seen.iter().any(|p| p == path), "{path} was never requested: {seen:?}");
    }
    // The linked sheet paints the frame background and its @import positions
    // the image, so both stylesheets reached the frame's style and layout.
    assert_eq!(image.get_pixel(200, 120).0, [232, 89, 12, 255], "linked sheet background");
    assert_eq!(image.get_pixel(40, 30).0, [255, 0, 0, 255], "img sized by the imported sheet");
    assert_eq!(image.get_pixel(85, 30).0, [0, 0, 255, 255], "css background-image");
    assert_eq!(image.get_pixel(5, 5).0, [255, 255, 255, 255], "outside the frame");
    let head = asked_for(&requests, "/red.png").unwrap().to_ascii_lowercase();
    assert!(
        head.contains("referer: http://") && head.contains("/frame.html"),
        "frame subresource must carry the frame document as referrer: {head}"
    );
    assert!(head.contains("sid=abc"), "frame subresource must carry page cookies: {head}");
    let frames = requests.lock().unwrap().iter().filter(|(p, _)| p == "/red.png").count();
    assert_eq!(frames, 1, "the image is fetched once");
}

#[test]
fn nested_frames_load_their_own_stylesheets() {
    let (base, _requests) = serve(route);
    let url = format!("{base}/outer.html");
    let image = capture(&[&url, "--allow-private-network"], "200", "130");
    assert_eq!(image.get_pixel(15, 15).0, [25, 113, 194, 255], "middle frame inline sheet");
    // Inner frame at (10 + 20, 10 + 10) painted from its own linked sheet.
    assert_eq!(image.get_pixel(50, 40).0, [112, 72, 232, 255], "inner frame linked sheet");
    assert_eq!(image.get_pixel(95, 40).0, [25, 113, 194, 255], "right of the inner frame");
}

#[test]
fn frame_resources_obey_the_page_network_gate() {
    let (base, requests) = serve(route);
    let page = format!(
        "data:text/html,<html style=\"margin:0\"><body style=\"margin:0\">\
         <iframe style=\"position:absolute;left:0;top:0;width:60px;height:40px;border:0\" \
         srcdoc=\"<body style='margin:0'><img style='display:block;width:60px;height:40px' src='{base}/red.png'></body>\">\
         </iframe></body></html>"
    );
    // Loopback is refused without --allow-private-network, exactly as for the
    // page's own images, so the frame image never reaches the server.
    let blocked = capture(&[&page], "80", "60");
    assert_ne!(blocked.get_pixel(30, 20).0, [255, 0, 0, 255]);
    assert!(asked_for(&requests, "/red.png").is_none(), "blocked frame image reached the server");

    let allowed = capture(&[&page, "--allow-private-network"], "80", "60");
    assert_eq!(allowed.get_pixel(30, 20).0, [255, 0, 0, 255]);
    assert!(asked_for(&requests, "/red.png").is_some());
}

#[test]
fn frame_file_resources_stay_blocked_for_a_network_document() {
    let secret = std::env::temp_dir().join(format!("inbrowse-frame-secret-{}.png", std::process::id()));
    std::fs::write(&secret, png([0, 255, 0, 255])).unwrap();
    *FILE_URL.lock().unwrap() = format!("file:///{}", secret.to_string_lossy().replace('\\', "/"));
    let (base, _requests) = serve(route);
    let url = format!("{base}/file.html");
    let image = capture(&[&url, "--allow-private-network"], "80", "70");
    std::fs::remove_file(&secret).ok();
    assert_ne!(image.get_pixel(30, 20).0, [0, 255, 0, 255], "file image must stay blocked");
    // The same markup with an http image does paint, so the frame itself loaded.
    assert_eq!(image.get_pixel(30, 50).0, [255, 0, 0, 255], "http image in the same frame");
}
