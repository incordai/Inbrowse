//! The client against a stand-in engine that answers like the engine's
//! `POST /v1/ocr` (shapes from the engine's src/ocr.rs).
//!
//! Test images encode each pixel's own position (red = x low byte, green = x
//! high byte, blue = y), so the stand-in can tell from a tile's first pixel
//! where in the whole image that tile was cut.

use std::io::Cursor;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base64::Engine;
use image::{DynamicImage, ImageFormat, RgbImage};
use inbrowse_ocr::{Client, Config, Limits, OcrError};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

#[derive(Clone, Debug)]
struct Seen {
    authorization: Option<String>,
    body: Value,
}

impl Seen {
    fn image(&self) -> RgbImage {
        let b64 = self.body["image_base64"].as_str().unwrap();
        let bytes = base64::engine::general_purpose::STANDARD.decode(b64).unwrap();
        image::load_from_memory(&bytes).unwrap().to_rgb8()
    }

    /// Where this piece starts in the whole image.
    fn origin(&self) -> (u32, u32) {
        let [r, g, b] = self.image().get_pixel(0, 0).0;
        (r as u32 + ((g as u32) << 8), b as u32)
    }
}

struct Reply {
    status: u16,
    headers: Vec<(&'static str, String)>,
    body: String,
}

fn ok(body: Value) -> Reply {
    Reply { status: 200, headers: vec![], body: body.to_string() }
}

type Handler = Arc<dyn Fn(&Seen, usize) -> Reply + Send + Sync>;

/// The bound gives the closure its signature (`&Seen` for any lifetime).
fn answer(f: impl Fn(&Seen, usize) -> Reply + Send + Sync + 'static) -> Handler {
    Arc::new(f)
}

async fn engine(handler: Handler) -> (String, Arc<Mutex<Vec<Seen>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else { return };
            let mut data = Vec::new();
            let mut buf = [0u8; 65536];
            let (head_end, length) = loop {
                let n = stream.read(&mut buf).await.unwrap_or(0);
                if n == 0 {
                    break (None, 0);
                }
                data.extend_from_slice(&buf[..n]);
                if let Some(pos) = data.windows(4).position(|w| w == b"\r\n\r\n") {
                    let head = String::from_utf8_lossy(&data[..pos]).to_lowercase();
                    let length = head
                        .lines()
                        .find_map(|l| l.strip_prefix("content-length:").map(|v| v.trim().parse::<usize>().unwrap()))
                        .unwrap_or(0);
                    break (Some(pos + 4), length);
                }
            };
            let Some(head_end) = head_end else { continue };
            while data.len() < head_end + length {
                let n = stream.read(&mut buf).await.unwrap_or(0);
                if n == 0 {
                    break;
                }
                data.extend_from_slice(&buf[..n]);
            }
            let head = String::from_utf8_lossy(&data[..head_end]).to_string();
            let authorization = head.lines().find_map(|l| {
                let (k, v) = l.split_once(':')?;
                k.eq_ignore_ascii_case("authorization").then(|| v.trim().to_string())
            });
            let body: Value = serde_json::from_slice(&data[head_end..head_end + length]).unwrap_or(Value::Null);
            let request = Seen { authorization, body };
            let index = {
                let mut log = log.lock().unwrap();
                log.push(request.clone());
                log.len() - 1
            };
            let reply = handler(&request, index);
            let mut out = format!(
                "HTTP/1.1 {} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n",
                reply.status,
                reply.body.len()
            );
            for (k, v) in reply.headers {
                out.push_str(&format!("{k}: {v}\r\n"));
            }
            out.push_str("\r\n");
            out.push_str(&reply.body);
            let _ = stream.write_all(out.as_bytes()).await;
            let _ = stream.shutdown().await;
        }
    });
    (url, seen)
}

fn client(url: &str, limits: Limits) -> Client {
    Client::new(Config { url: url.into(), key: Some("test-key".into()), timeout: Duration::from_secs(10) })
        .unwrap()
        .with_limits(limits)
}

fn positions(width: u32, height: u32) -> RgbImage {
    assert!(height <= 256);
    RgbImage::from_fn(width, height, |x, y| image::Rgb([(x & 255) as u8, (x >> 8) as u8, y as u8]))
}

fn encode(image: RgbImage, format: ImageFormat) -> Vec<u8> {
    let mut out = Vec::new();
    DynamicImage::ImageRgb8(image).write_to(&mut Cursor::new(&mut out), format).unwrap();
    out
}

fn small_tiles() -> Limits {
    Limits {
        max_pixels: 100_000,
        tile_pixels: 6_000,
        tile_max_width: 100,
        min_side: 16,
        ..Limits::default()
    }
}

#[tokio::test]
async fn small_image_goes_as_is_with_warnings_kept() {
    let handler = answer(|_, _| {
        ok(json!({"text": "hi", "lines": [{"text": "hi", "box": [1, 2, 3, 4], "confidence": 0.9}],
                  "lang": null, "model": "m1", "elapsed_ms": 5, "warnings": ["broken PNG row"]}))
    });
    let (url, seen) = engine(handler).await;
    let png = encode(positions(64, 32), ImageFormat::Png);
    let result = client(&url, Limits::default()).recognize(&png, None).await.unwrap();
    assert_eq!(result.text, "hi");
    assert_eq!(result.lines[0].bbox, [1, 2, 3, 4]);
    assert_eq!((result.width, result.height, result.tiles), (64, 32, 1));
    assert_eq!(result.model.as_deref(), Some("m1"));
    assert_eq!(result.warnings, vec!["broken PNG row"]);
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].authorization.as_deref(), Some("Bearer test-key"));
    assert_eq!(seen[0].body["mime"], "image/png");
    assert_eq!(seen[0].body["detail"], "lines");
}

/// A line inside two overlapping tiles comes back once, in whole-image
/// coordinates; each tile's own line is shifted by that tile's offset.
#[tokio::test]
async fn big_image_is_tiled_and_seams_merged() {
    let handler = answer(|seen, _| {
        let (gx, gy) = seen.origin();
        let tile = seen.image();
        let mut lines = vec![json!({"text": format!("tile {gx}"), "box": [2, 40, 10, 10], "confidence": 0.8})];
        let seam = [140u32, 10, 20, 12];
        if seam[0] >= gx && seam[0] + seam[2] <= gx + tile.width() {
            lines.push(json!({"text": "SEAM", "box": [seam[0] - gx, seam[1] - gy, seam[2], seam[3]], "confidence": 0.9}));
        }
        ok(json!({"text": "", "lines": lines, "model": "m1", "elapsed_ms": 1, "warnings": []}))
    });
    let (url, seen) = engine(handler).await;
    // 300x60 = 18,000 px, over a 10,000 px limit: never sent whole.
    let limits = Limits { max_pixels: 10_000, ..small_tiles() };
    let png = encode(positions(300, 60), ImageFormat::Png);
    let result = client(&url, limits).recognize(&png, None).await.unwrap();

    assert_eq!(result.tiles, 4);
    assert_eq!(seen.lock().unwrap().len(), 4);
    let seams: Vec<_> = result.lines.iter().filter(|l| l.text == "SEAM").collect();
    assert_eq!(seams.len(), 1, "{:?}", result.lines);
    assert_eq!(seams[0].bbox, [140, 10, 20, 12]);
    for x in [0u32, 68, 136, 200] {
        let own = result.lines.iter().find(|l| l.text == format!("tile {x}")).expect("tile line");
        assert_eq!(own.bbox, [x + 2, 40, 10, 10]);
    }
    assert!(result.text.contains("SEAM"));
}

/// 413 with a reason on the whole image: decode, tile, retry.
#[tokio::test]
async fn too_large_reply_leads_to_tiles() {
    let handler = answer(|seen, _| {
        if seen.image().width() == 300 {
            return Reply {
                status: 413,
                headers: vec![],
                body: json!({"error": "too_large", "reason": "decoded JPEG over 64 MP; split it into tiles before sending"}).to_string(),
            };
        }
        ok(json!({"text": "", "lines": [], "model": "m1", "elapsed_ms": 1, "warnings": []}))
    });
    let (url, seen) = engine(handler).await;
    let png = encode(positions(300, 60), ImageFormat::Png);
    let result = client(&url, small_tiles()).recognize(&png, None).await.unwrap();
    assert_eq!(result.tiles, 4);
    assert_eq!(seen.lock().unwrap().len(), 5);
    assert!(result.warnings.is_empty(), "{:?}", result.warnings);
}

/// A tile refused as too large is halved until it fits; below the floor it
/// becomes a warning naming the region instead of an error.
#[tokio::test]
async fn refused_tiles_split_down_to_the_floor() {
    let handler = answer(|seen, _| {
        if seen.image().width() > 50 {
            return Reply { status: 413, headers: vec![], body: json!({"error": "too_large"}).to_string() };
        }
        let (gx, _) = seen.origin();
        ok(json!({"text": "", "lines": [{"text": format!("at {gx}"), "box": [0, 0, 5, 5], "confidence": 0.5}],
                  "model": "m1", "elapsed_ms": 1, "warnings": []}))
    });
    let (url, _) = engine(handler).await;
    let limits = Limits { max_pixels: 1_000, tile_pixels: 4_800, tile_max_width: 120, min_side: 16, ..Limits::default() };
    let png = encode(positions(120, 40), ImageFormat::Png);
    let result = client(&url, limits).recognize(&png, None).await.unwrap();
    assert!(result.tiles >= 2, "{result:?}");
    assert!(result.warnings.is_empty(), "{:?}", result.warnings);
    // Every column was read by some piece.
    let starts: Vec<u32> = result.lines.iter().map(|l| l.bbox[0]).collect();
    assert_eq!(*starts.iter().min().unwrap(), 0);

    let (url, _) = engine(answer(|_, _| Reply {
        status: 413,
        headers: vec![],
        body: json!({"error": "too_large"}).to_string(),
    }))
    .await;
    let limits = Limits { min_side: 40, ..limits };
    let result = client(&url, limits).recognize(&png, None).await.unwrap();
    assert_eq!(result.tiles, 0);
    assert!(!result.warnings.is_empty());
    assert!(result.warnings.iter().all(|w| w.contains("not read") && w.contains(" at ")), "{:?}", result.warnings);
}

#[tokio::test]
async fn model_unavailable_stops_without_retrying() {
    let (url, seen) = engine(answer(|_, _| Reply {
        status: 503,
        headers: vec![("retry-after", "60".into())],
        body: json!({"error": "model_unavailable"}).to_string(),
    }))
    .await;
    let png = encode(positions(300, 60), ImageFormat::Png);
    // Tiled, so a retry storm would show up as one request per tile.
    let limits = Limits { max_pixels: 10_000, ..small_tiles() };
    let err = client(&url, limits).recognize(&png, None).await.unwrap_err();
    assert!(matches!(err, OcrError::Unavailable(_)), "{err:?}");
    assert!(err.to_string().starts_with("ocr_unavailable:"));
    assert_eq!(seen.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn busy_is_retried_after_retry_after() {
    let (url, seen) = engine(answer(|_, index| {
        if index == 0 {
            return Reply { status: 429, headers: vec![("retry-after", "1".into())], body: json!({"error": "busy"}).to_string() };
        }
        ok(json!({"text": "later", "lines": [], "model": "m1", "elapsed_ms": 1, "warnings": []}))
    }))
    .await;
    let png = encode(positions(16, 16), ImageFormat::Png);
    let started = Instant::now();
    let result = client(&url, Limits::default()).recognize(&png, None).await.unwrap();
    assert_eq!(result.text, "later");
    assert_eq!(seen.lock().unwrap().len(), 2);
    assert!(started.elapsed() >= Duration::from_millis(900));
}

#[tokio::test]
async fn old_engine_and_bad_input_are_named() {
    let (url, _) = engine(answer(|_, _| Reply { status: 404, headers: vec![], body: String::new() })).await;
    let png = encode(positions(16, 16), ImageFormat::Png);
    let err = client(&url, Limits::default()).recognize(&png, None).await.unwrap_err();
    assert!(matches!(&err, OcrError::Unavailable(d) if d.contains("newer engine")), "{err:?}");

    let (url, seen) = engine(answer(|_, _| ok(json!({})))).await;
    let err = client(&url, Limits::default()).recognize(b"plain text, not an image", None).await.unwrap_err();
    assert!(matches!(err, OcrError::Rejected(_)), "{err:?}");
    assert!(seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn unreachable_engine_is_unavailable() {
    // Bind and drop to get a port nothing listens on.
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let png = encode(positions(16, 16), ImageFormat::Png);
    let err = client(&format!("http://127.0.0.1:{port}"), Limits::default())
        .recognize(&png, None)
        .await
        .unwrap_err();
    assert!(matches!(err, OcrError::Unavailable(_)), "{err:?}");
}

/// Neither the key nor a password in the URL appears in what comes back.
#[tokio::test]
async fn secrets_never_reach_errors() {
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let config = Config {
        url: format!("http://ocr-user:url-secret-1@127.0.0.1:{port}"),
        key: Some("key-secret-2".into()),
        timeout: Duration::from_secs(5),
    };
    assert!(!format!("{config:?}").contains("key-secret-2"));
    let png = encode(positions(16, 16), ImageFormat::Png);
    let err = Client::new(config).unwrap().recognize(&png, None).await.unwrap_err().to_string();
    assert!(err.starts_with("ocr_unavailable:"), "{err}");
    assert!(err.contains(&format!("127.0.0.1:{port}")), "{err}");
    assert!(!err.contains("url-secret-1") && !err.contains("key-secret-2"), "{err}");
}

/// Formats the engine does not take (here BMP) are converted to PNG.
#[tokio::test]
async fn other_formats_are_sent_as_png() {
    let (url, seen) = engine(answer(|_, _| ok(json!({"text": "", "lines": [], "model": "m1", "elapsed_ms": 1, "warnings": []})))).await;
    let bmp = encode(positions(40, 20), ImageFormat::Bmp);
    let result = client(&url, Limits::default()).recognize(&bmp, None).await.unwrap();
    assert_eq!(result.tiles, 1);
    let seen = seen.lock().unwrap();
    assert_eq!(seen[0].body["mime"], "image/png");
    assert_eq!(seen[0].origin(), (0, 0));
}
