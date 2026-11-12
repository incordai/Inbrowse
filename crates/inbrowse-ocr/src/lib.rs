//! Text from images through the Incord memory engine's OCR endpoint
//! (`POST {INBROWSE_OCR_URL}/v1/ocr`). Inbrowse ships no OCR model.
//!
//! A big input is never dropped. Anything the engine would refuse (a file over
//! 16 MB, over 64 MP, wider than 43,690 px, or a format other than PNG or
//! JPEG) is decoded here and sent as overlapping tiles. Their lines come back
//! shifted into the full image's coordinates, with the copies read twice along
//! a seam removed. A piece the engine still refuses as too large is split
//! again, down to `Limits::min_side`; only what is left after that becomes a
//! warning naming the region.
//!
//! Engine answers: 200 carries `warnings`, kept per region. 413 carries an
//! optional `reason` and triggers a split. 429 `busy` and 503 `model_loading`
//! are retried after their Retry-After, a bounded number of times. 503
//! `model_unavailable`, a refused key, or an engine without the endpoint stop
//! OCR at once (`OcrError::Unavailable`), so a caller reading many images
//! reports it once instead of retrying each image.

use std::collections::VecDeque;
use std::fmt;
use std::io::Cursor;
use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine;
use image::{DynamicImage, ImageFormat, RgbImage};
use serde::{Deserialize, Serialize};

/// Base address of the Incord memory engine.
pub const ENV_URL: &str = "INBROWSE_OCR_URL";
/// The engine's local address when `INBROWSE_OCR_URL` is unset: where a
/// Watchman install runs it. Nothing else in Inbrowse calls the engine, so
/// there is no other setting to share.
pub const DEFAULT_URL: &str = "http://127.0.0.1:9180";
/// Key sent as `Authorization: Bearer`, when the engine wants one.
pub const ENV_KEY: &str = "INBROWSE_OCR_KEY";
/// Seconds one request may take (default 120).
pub const ENV_TIMEOUT: &str = "INBROWSE_OCR_TIMEOUT";

/// Retries of 429 `busy` and 503 `model_loading`, per request.
const MAX_RETRIES: u32 = 12;

#[derive(Clone)]
pub struct Config {
    /// Engine base address, without `/v1/ocr`.
    pub url: String,
    pub key: Option<String>,
    pub timeout: Duration,
}

impl fmt::Debug for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Config")
            .field("url", &self.url)
            .field("key", &self.key.as_ref().map(|_| "<set>"))
            .field("timeout", &self.timeout)
            .finish()
    }
}

impl Config {
    /// From `INBROWSE_OCR_URL` (default `DEFAULT_URL`), `INBROWSE_OCR_KEY`
    /// and `INBROWSE_OCR_TIMEOUT`.
    pub fn from_env() -> Self {
        let url = std::env::var(ENV_URL)
            .ok()
            .map(|u| u.trim().trim_end_matches('/').to_string())
            .filter(|u| !u.is_empty())
            .unwrap_or_else(|| DEFAULT_URL.to_string());
        let key = std::env::var(ENV_KEY)
            .ok()
            .map(|k| k.trim().to_string())
            .filter(|k| !k.is_empty());
        let timeout = std::env::var(ENV_TIMEOUT)
            .ok()
            .and_then(|t| t.trim().parse::<f64>().ok())
            .filter(|t| t.is_finite() && *t > 0.0)
            .map(Duration::from_secs_f64)
            .unwrap_or(Duration::from_secs(120));
        Self { url, key, timeout }
    }
}

/// The engine's refusal limits, with tiles sized well under them.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// The engine refuses a file over 16 MB.
    pub max_file_bytes: usize,
    /// ... a JPEG or interlaced PNG over 64 MP.
    pub max_pixels: u64,
    /// ... a PNG wider than 43,690 px.
    pub max_width: u32,
    /// Pixels per tile. 4 MP as RGB PNG stays under 16 MB even for noise.
    pub tile_pixels: u64,
    pub tile_max_width: u32,
    /// A too-large piece is not split below this side.
    pub min_side: u32,
    /// Memory the decoder may allocate for one input.
    pub max_decode_bytes: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_file_bytes: 16_000_000,
            max_pixels: 64_000_000,
            max_width: 43_690,
            tile_pixels: 4_000_000,
            tile_max_width: 8_192,
            min_side: 256,
            max_decode_bytes: 1 << 30,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Line {
    pub text: String,
    /// `[x, y, w, h]` in pixels of the whole input image, top-left origin.
    #[serde(rename = "box")]
    pub bbox: [u32; 4],
    /// Mean character probability, 0..1.
    pub confidence: f32,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Recognized {
    /// Lines joined with newlines, in reading order.
    pub text: String,
    pub lines: Vec<Line>,
    pub model: Option<String>,
    pub width: u32,
    pub height: u32,
    /// Pieces sent to the engine and read.
    pub tiles: usize,
    pub elapsed_ms: u64,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum OcrError {
    /// No engine configured or reachable, the key refused, or the model
    /// cannot load. Stop OCR for the rest of the call.
    Unavailable(String),
    /// Not an image the engine or Inbrowse can read.
    Rejected(String),
    /// The deadline passed before any text came back.
    TimedOut,
}

impl fmt::Display for OcrError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            OcrError::Unavailable(d) => write!(f, "ocr_unavailable: {d}"),
            OcrError::Rejected(d) => write!(f, "ocr_rejected: {d}"),
            OcrError::TimedOut => write!(
                f,
                "ocr_timed_out: the deadline passed before any text came back"
            ),
        }
    }
}

impl std::error::Error for OcrError {}

#[derive(Deserialize)]
struct EngineResponse {
    #[serde(default)]
    text: String,
    #[serde(default)]
    lines: Vec<Line>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    warnings: Vec<String>,
}

#[derive(Deserialize, Default)]
struct EngineError {
    #[serde(default)]
    error: String,
    #[serde(default)]
    reason: Option<String>,
}

/// What one request came back with, short of a reason to stop.
enum Sent {
    Read(EngineResponse),
    TooLarge(String),
    Refused { code: String, detail: String },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Rect {
    x: u32,
    y: u32,
    w: u32,
    h: u32,
}

/// A line the engine read in one tile, in whole-image coordinates. `piece`
/// numbers the tile reads in order; [`merge`] only compares lines of
/// different pieces.
struct Found {
    piece: usize,
    tile: Rect,
    line: Line,
}

impl fmt::Display for Rect {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}x{} at {},{}", self.w, self.h, self.x, self.y)
    }
}

pub struct Client {
    config: Config,
    /// `config.url` without any user:password, for messages.
    shown_url: String,
    endpoint: String,
    http: reqwest::Client,
    limits: Limits,
}

impl Client {
    pub fn new(config: Config) -> Result<Self, OcrError> {
        let endpoint = format!("{}/v1/ocr", config.url.trim_end_matches('/'));
        let mut shown = reqwest::Url::parse(&config.url)
            .map_err(|_| OcrError::Unavailable(format!("{ENV_URL} is not a valid address")))?;
        let _ = shown.set_username("");
        let _ = shown.set_password(None);
        let shown_url = shown.as_str().trim_end_matches('/').to_string();
        // The engine is the operator's own service: no proxy, no SSRF gate.
        let http = reqwest::Client::builder()
            .no_proxy()
            .connect_timeout(Duration::from_secs(5))
            .build()
            .map_err(|e| OcrError::Unavailable(format!("could not start the OCR client: {e}")))?;
        Ok(Self {
            config,
            shown_url,
            endpoint,
            http,
            limits: Limits::default(),
        })
    }

    /// `Client::new` from the environment (`Config::from_env`).
    pub fn from_env() -> Result<Self, OcrError> {
        Self::new(Config::from_env())
    }

    pub fn with_limits(mut self, limits: Limits) -> Self {
        self.limits = limits;
        self
    }

    /// Read the text in one image (PNG, JPEG, WebP, GIF or BMP). Every error
    /// and warning is scrubbed of the key and the URL's user:password.
    pub async fn recognize(
        &self,
        bytes: &[u8],
        deadline: Option<Instant>,
    ) -> Result<Recognized, OcrError> {
        match self.recognize_unscrubbed(bytes, deadline).await {
            Ok(mut r) => {
                r.warnings = r.warnings.into_iter().map(|w| self.scrub(w)).collect();
                Ok(r)
            }
            Err(OcrError::Unavailable(d)) => Err(OcrError::Unavailable(self.scrub(d))),
            Err(OcrError::Rejected(d)) => Err(OcrError::Rejected(self.scrub(d))),
            Err(e) => Err(e),
        }
    }

    fn scrub(&self, mut text: String) -> String {
        if let Some(key) = self.config.key.as_deref().filter(|k| !k.is_empty()) {
            text = text.replace(key, "<key>");
        }
        if self.shown_url != self.config.url {
            text = text.replace(&self.config.url, &self.shown_url);
        }
        text
    }

    async fn recognize_unscrubbed(
        &self,
        bytes: &[u8],
        deadline: Option<Instant>,
    ) -> Result<Recognized, OcrError> {
        let started = Instant::now();
        let format = image::guess_format(bytes).ok();
        let (width, height) = dimensions(bytes).map_err(OcrError::Rejected)?;
        let mime = match format {
            Some(ImageFormat::Png) => Some("image/png"),
            Some(ImageFormat::Jpeg) => Some("image/jpeg"),
            _ => None,
        };
        if let Some(mime) = mime.filter(|_| self.fits(bytes.len(), width, height)) {
            match self.send(bytes, mime, deadline).await? {
                Sent::Read(r) => {
                    return Ok(Recognized {
                        text: r.text,
                        lines: r.lines,
                        model: r.model,
                        width,
                        height,
                        tiles: 1,
                        elapsed_ms: started.elapsed().as_millis() as u64,
                        warnings: r.warnings,
                    })
                }
                // Decoded and tiled below.
                Sent::TooLarge(_) => {}
                // The engine's decoder may not handle every variant (a CMYK
                // JPEG, say); ours gets a try before giving up.
                Sent::Refused { code, .. } if code == "bad_image" => {}
                Sent::Refused { detail, .. } => return Err(OcrError::Rejected(detail)),
            }
        }
        let image = decode(bytes.to_vec(), self.limits.max_decode_bytes)
            .await
            .map_err(OcrError::Rejected)?;
        self.tiled(image, deadline, started).await
    }

    fn fits(&self, len: usize, width: u32, height: u32) -> bool {
        len <= self.limits.max_file_bytes
            && width as u64 * height as u64 <= self.limits.max_pixels
            && width <= self.limits.max_width
    }

    async fn tiled(
        &self,
        image: DynamicImage,
        deadline: Option<Instant>,
        started: Instant,
    ) -> Result<Recognized, OcrError> {
        let (width, height) = (image.width(), image.height());
        let rgb = Arc::new(
            tokio::task::spawn_blocking(move || flatten(image))
                .await
                .map_err(|e| OcrError::Rejected(format!("could not prepare the image: {e}")))?,
        );
        let mut queue: VecDeque<Rect> = plan(width, height, &self.limits).into();
        let mut found: Vec<Found> = Vec::new();
        let mut warnings = Vec::new();
        let mut model = None;
        let mut read = 0usize;
        let mut piece = 0usize;
        let mut timed_out = false;

        while let Some(rect) = queue.pop_front() {
            if deadline.is_some_and(|d| Instant::now() >= d) {
                timed_out = true;
                warnings.push(format!(
                    "OCR stopped at the deadline; {} regions not read",
                    queue.len() + 1
                ));
                break;
            }
            let png = encode_region(rgb.clone(), rect)
                .await
                .map_err(OcrError::Rejected)?;
            let sent = if png.len() > self.limits.max_file_bytes {
                Sent::TooLarge(format!("tile PNG is {} bytes", png.len()))
            } else {
                match self.send(&png, "image/png", deadline).await {
                    Ok(sent) => sent,
                    Err(OcrError::TimedOut) => {
                        timed_out = true;
                        warnings.push(format!(
                            "OCR stopped at the deadline; {} regions not read",
                            queue.len() + 1
                        ));
                        break;
                    }
                    Err(e) => return Err(e),
                }
            };
            match sent {
                Sent::Read(r) => {
                    read += 1;
                    piece += 1;
                    model = model.or(r.model);
                    for mut line in r.lines {
                        line.bbox[0] = line.bbox[0].saturating_add(rect.x);
                        line.bbox[1] = line.bbox[1].saturating_add(rect.y);
                        found.push(Found {
                            piece,
                            tile: rect,
                            line,
                        });
                    }
                    warnings.extend(
                        r.warnings
                            .into_iter()
                            .map(|w| format!("region {rect}: {w}")),
                    );
                }
                Sent::TooLarge(reason) => match split(rect, self.limits.min_side) {
                    Some(parts) => {
                        for part in parts.into_iter().rev() {
                            queue.push_front(part);
                        }
                    }
                    None => warnings.push(format!(
                        "region {rect} not read: too large for the OCR engine ({reason})"
                    )),
                },
                Sent::Refused { detail, .. } => {
                    warnings.push(format!("region {rect} not read: {detail}"))
                }
            }
        }

        if read == 0 && timed_out {
            return Err(OcrError::TimedOut);
        }
        let lines = merge(found);
        let text = lines
            .iter()
            .map(|l| l.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        Ok(Recognized {
            text,
            lines,
            model,
            width,
            height,
            tiles: read,
            elapsed_ms: started.elapsed().as_millis() as u64,
            warnings,
        })
    }

    /// One request, with the bounded retries for busy and loading.
    async fn send(
        &self,
        bytes: &[u8],
        mime: &str,
        deadline: Option<Instant>,
    ) -> Result<Sent, OcrError> {
        let body = serde_json::to_vec(&serde_json::json!({
            "image_base64": base64::engine::general_purpose::STANDARD.encode(bytes),
            "mime": mime,
            "detail": "lines",
        }))
        .map_err(|e| OcrError::Rejected(e.to_string()))?;
        let mut retries = 0;
        loop {
            let mut timeout = self.config.timeout;
            if let Some(d) = deadline {
                let left = d.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    return Err(OcrError::TimedOut);
                }
                timeout = timeout.min(left);
            }
            let mut req = self
                .http
                .post(&self.endpoint)
                .timeout(timeout)
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .body(body.clone());
            if let Some(key) = &self.config.key {
                req = req.bearer_auth(key);
            }
            let resp = match req.send().await {
                Ok(r) => r,
                Err(e) if e.is_timeout() => return Err(OcrError::TimedOut),
                Err(e) => {
                    return Err(OcrError::Unavailable(format!(
                        "could not reach the OCR engine at {} (start the Incord memory engine, or set {ENV_URL}): {}",
                        self.config.url,
                        error_chain(&e)
                    )))
                }
            };
            let status = resp.status().as_u16();
            let retry_after = resp
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.trim().parse::<u64>().ok())
                .unwrap_or(1);
            let data = match resp.bytes().await {
                Ok(d) => d,
                Err(e) if e.is_timeout() => return Err(OcrError::TimedOut),
                Err(e) => {
                    return Err(OcrError::Unavailable(format!(
                        "the OCR engine's answer broke off: {e}"
                    )))
                }
            };
            let error: EngineError = serde_json::from_slice(&data).unwrap_or_default();
            let detail = || match &error.reason {
                Some(reason) => format!("{}: {reason}", error.error),
                None if error.error.is_empty() => format!("HTTP {status}"),
                None => error.error.clone(),
            };
            match status {
                200 => {
                    return serde_json::from_slice::<EngineResponse>(&data).map(Sent::Read).or_else(|e| {
                        Ok(Sent::Refused {
                            code: "unreadable".into(),
                            detail: format!("unreadable answer from the OCR engine: {e}"),
                        })
                    })
                }
                413 => return Ok(Sent::TooLarge(error.reason.clone().unwrap_or_else(|| "over the engine's size limit".into()))),
                503 if error.error == "model_unavailable" => {
                    return Err(OcrError::Unavailable(format!(
                        "the OCR model could not be loaded on the engine ({}); retry in about {retry_after}s",
                        detail()
                    )))
                }
                429 | 503 if retries < MAX_RETRIES => {
                    let cap = if status == 503 { 10 } else { 5 };
                    let wait = Duration::from_secs(retry_after.clamp(1, cap));
                    if deadline.is_some_and(|d| Instant::now() + wait >= d) {
                        return Err(OcrError::TimedOut);
                    }
                    retries += 1;
                    tokio::time::sleep(wait).await;
                }
                503 => {
                    return Err(OcrError::Unavailable(format!(
                        "the OCR model is still loading after {retries} retries ({})",
                        detail()
                    )))
                }
                401 | 403 => {
                    return Err(OcrError::Unavailable(format!(
                        "the OCR engine refused the request (HTTP {status}); check {ENV_KEY}"
                    )))
                }
                404 | 405 => {
                    return Err(OcrError::Unavailable(format!(
                        "the engine at {} has no OCR endpoint (HTTP {status}); it needs a newer engine",
                        self.config.url
                    )))
                }
                _ => {
                    return Ok(Sent::Refused {
                        code: error.error.clone(),
                        detail: detail(),
                    })
                }
            }
        }
    }
}

fn error_chain(e: &dyn std::error::Error) -> String {
    let mut out = e.to_string();
    let mut source = e.source();
    while let Some(s) = source {
        out.push_str(": ");
        out.push_str(&s.to_string());
        source = s.source();
    }
    out
}

fn dimensions(bytes: &[u8]) -> Result<(u32, u32), String> {
    let unreadable = || "not an image Inbrowse can read (PNG, JPEG, WebP, GIF or BMP)".to_string();
    let reader = image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|_| unreadable())?;
    if reader.format().is_none() {
        return Err(unreadable());
    }
    reader
        .into_dimensions()
        .map_err(|e| format!("could not read the image size: {e}"))
}

async fn decode(bytes: Vec<u8>, max_alloc: u64) -> Result<DynamicImage, String> {
    let work = tokio::task::spawn_blocking(move || {
        std::panic::catch_unwind(|| {
            let mut reader = image::ImageReader::new(Cursor::new(&bytes))
                .with_guessed_format()
                .map_err(|e| e.to_string())?;
            let mut limits = image::Limits::default();
            limits.max_alloc = Some(max_alloc);
            reader.limits(limits);
            reader
                .decode()
                .map_err(|e| format!("could not decode the image: {e}"))
        })
        .unwrap_or_else(|_| Err("the image decoder failed on this file".into()))
    });
    work.await.map_err(|e| format!("decode task failed: {e}"))?
}

/// RGB, with any transparency composited onto white: dark text on a clear
/// background must not turn into dark text on black.
fn flatten(image: DynamicImage) -> RgbImage {
    if !image.color().has_alpha() {
        return image.to_rgb8();
    }
    let rgba = image.to_rgba8();
    RgbImage::from_fn(rgba.width(), rgba.height(), |x, y| {
        let [r, g, b, a] = rgba.get_pixel(x, y).0;
        let over = |c: u8| ((c as u32 * a as u32 + 255 * (255 - a as u32)) / 255) as u8;
        image::Rgb([over(r), over(g), over(b)])
    })
}

async fn encode_region(rgb: Arc<RgbImage>, rect: Rect) -> Result<Vec<u8>, String> {
    tokio::task::spawn_blocking(move || {
        let tile = image::imageops::crop_imm(&*rgb, rect.x, rect.y, rect.w, rect.h).to_image();
        let mut out = Vec::new();
        DynamicImage::ImageRgb8(tile)
            .write_to(&mut Cursor::new(&mut out), ImageFormat::Png)
            .map_err(|e| format!("could not encode a tile: {e}"))?;
        Ok::<Vec<u8>, String>(out)
    })
    .await
    .map_err(|e| format!("tile task failed: {e}"))?
}

/// The least overlap between neighbouring tiles: a line of large text (about
/// 160 px tall) then lies whole inside one of the two, so a seam never leaves
/// only fragments of it.
const MIN_OVERLAP: u32 = 160;

/// How far two neighbouring tiles overlap along a side of `len` px: a tenth of
/// it, at least [`MIN_OVERLAP`], but never more than a quarter of a small
/// tile. Used by [`side_for`] for [`plan`] and by [`split`].
fn overlap(len: u32) -> u32 {
    (len / 10).max(MIN_OVERLAP.min(len / 4))
}

/// The side of the equal tiles that cover `total` px with `n` windows, each
/// neighbouring pair overlapping by [`overlap`] of that side. Called by
/// [`plan`] for both axes.
fn side_for(total: u32, n: u32) -> u32 {
    let mut side = total.div_ceil(n);
    loop {
        let need = (total as u64 + (n as u64 - 1) * overlap(side) as u64).div_ceil(n as u64) as u32;
        if need <= side {
            return side;
        }
        side = need;
    }
}

/// Start offsets of `n` windows of `side` px spread evenly over `total`, the
/// last one ending exactly at `total`. `side` comes from [`side_for`].
fn offsets(total: u32, n: u32, side: u32) -> Vec<u32> {
    if n <= 1 {
        return vec![0];
    }
    (0..n)
        .map(|i| ((total - side) as u64 * i as u64 / (n - 1) as u64) as u32)
        .collect()
}

/// The fewest windows of at most `max_side` px covering `total`, with their
/// side; `(1, total)` when it fits whole. Called by [`plan`] for the rows.
fn windows(total: u32, max_side: u32) -> (u32, u32) {
    if total <= max_side {
        return (1, total);
    }
    let mut n = total.div_ceil(max_side);
    loop {
        let side = side_for(total, n);
        if side <= max_side {
            return (n, side);
        }
        n += 1;
    }
}

/// Tiles covering the image: a grid of equal tiles, each at most
/// `tile_max_width` wide and `tile_pixels` in area, neighbours overlapping by
/// [`overlap`]. Of the column counts that fit it takes the grid with the
/// fewest tiles (every tile is a request and every seam a chance to cut a
/// line), then the one with the shortest tile sides, which is the most
/// square. Called by [`Client::tiled`].
fn plan(width: u32, height: u32, limits: &Limits) -> Vec<Rect> {
    let (width, height) = (width.max(1), height.max(1));
    let max_w = limits.tile_max_width.max(1);
    let mut best: Option<((u32, u32), Vec<u32>, Vec<u32>, u32, u32)> = None;
    let mut cols = width.div_ceil(max_w);
    loop {
        let w = side_for(width, cols);
        if w <= max_w {
            let rows_max = (limits.tile_pixels / w as u64).min(u32::MAX as u64) as u32;
            let (rows, h) = windows(height, rows_max.max(limits.min_side).max(1));
            let key = (cols * rows, w + h);
            if best.as_ref().is_none_or(|b| key < b.0) {
                best = Some((key, offsets(width, cols, w), offsets(height, rows, h), w, h));
            }
        }
        if cols >= width || (w <= limits.min_side && w <= max_w) {
            break;
        }
        cols += 1;
    }
    let Some((_, xs, ys, w, h)) = best else {
        return Vec::new();
    };
    ys.iter()
        .flat_map(|&y| xs.iter().map(move |&x| Rect { x, y, w, h }))
        .collect()
}

/// Halve the longer side, overlapping; `None` below `min_side`.
fn split(rect: Rect, min_side: u32) -> Option<[Rect; 2]> {
    let horizontal = rect.w >= rect.h;
    let len = if horizontal { rect.w } else { rect.h };
    let half = (len + overlap(len)).div_ceil(2);
    if half < min_side || half >= len {
        return None;
    }
    let second = len - half;
    Some(if horizontal {
        [
            Rect { w: half, ..rect },
            Rect {
                x: rect.x + second,
                w: half,
                ..rect
            },
        ]
    } else {
        [
            Rect { h: half, ..rect },
            Rect {
                y: rect.y + second,
                h: half,
                ..rect
            },
        ]
    })
}

fn normalized(text: &str) -> String {
    text.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn area(b: &[u32; 4]) -> u64 {
    b[2] as u64 * b[3] as u64
}

fn intersection(a: &[u32; 4], b: &[u32; 4]) -> u64 {
    let x0 = a[0].max(b[0]);
    let y0 = a[1].max(b[1]);
    let x1 = (a[0] + a[2]).min(b[0] + b[2]);
    let y1 = (a[1] + a[3]).min(b[1] + b[3]);
    if x1 <= x0 || y1 <= y0 {
        0
    } else {
        (x1 - x0) as u64 * (y1 - y0) as u64
    }
}

/// The same text read twice where two tiles overlap: the boxes mostly
/// coincide and one text holds the other (a tile edge can cut a line short).
fn same_line(a: &Line, b: &Line) -> bool {
    let smaller = area(&a.bbox).min(area(&b.bbox));
    if smaller == 0 || (intersection(&a.bbox, &b.bbox) as f64) < 0.6 * smaller as f64 {
        return false;
    }
    let (ta, tb) = (normalized(&a.text), normalized(&b.text));
    let (short, long) = if ta.len() <= tb.len() {
        (&ta, &tb)
    } else {
        (&tb, &ta)
    };
    short == long || (short.chars().count() >= 3 && long.contains(short.as_str()))
}

/// How close to the edge of its tile a line's box must come to count as
/// cut by it: engines pad boxes by a pixel or two.
const EDGE_SLACK: u32 = 4;

/// Whether `line`, found in `tile`, reaches one of the tile's edges, where
/// the tile may have cut it.
fn touches_edge(line: &Line, tile: &Rect) -> bool {
    let [x, y, w, h] = line.bbox;
    x <= tile.x.saturating_add(EDGE_SLACK)
        || y <= tile.y.saturating_add(EDGE_SLACK)
        || x.saturating_add(w) >= (tile.x + tile.w).saturating_sub(EDGE_SLACK)
        || y.saturating_add(h) >= (tile.y + tile.h).saturating_sub(EDGE_SLACK)
}

/// A fragment of a line that another tile read whole: `line` reaches the edge
/// of its own tile and its box lies almost entirely inside the larger box of
/// a line from a different tile, whatever either text says (a cut line is
/// often misread, so the texts do not match). Called by [`merge`].
fn is_seam_fragment(found: &Found, others: &[&Found]) -> bool {
    touches_edge(&found.line, &found.tile)
        && others.iter().any(|o| {
            o.piece != found.piece
                && area(&o.line.bbox) > area(&found.line.bbox)
                && intersection(&o.line.bbox, &found.line.bbox) as f64
                    >= 0.8 * area(&found.line.bbox) as f64
        })
}

/// Drop seam duplicates and fragments (only between different tiles; longer,
/// then more confident readings win) and put the rest in reading order.
/// Called by [`Client::tiled`].
fn merge(mut found: Vec<Found>) -> Vec<Line> {
    found.sort_by(|a, b| {
        b.line
            .text
            .chars()
            .count()
            .cmp(&a.line.text.chars().count())
            .then(b.line.confidence.total_cmp(&a.line.confidence))
    });
    let mut kept: Vec<&Found> = Vec::new();
    for f in &found {
        if !kept
            .iter()
            .any(|k| k.piece != f.piece && same_line(&k.line, &f.line))
        {
            kept.push(f);
        }
    }
    let survivors: Vec<&Found> = kept
        .iter()
        .copied()
        .filter(|f| !is_seam_fragment(f, &kept))
        .collect();
    let mut lines: Vec<Line> = survivors.into_iter().map(|f| f.line.clone()).collect();
    lines.sort_by_key(|l| (l.bbox[1] as u64 * 2 + l.bbox[3] as u64, l.bbox[0]));
    // Rows: a line whose vertical middle falls inside the row's first line.
    let mut ordered = Vec::with_capacity(lines.len());
    let mut row: Vec<Line> = Vec::new();
    for line in lines {
        let middle = line.bbox[1] as u64 * 2 + line.bbox[3] as u64;
        let joins = row.first().is_some_and(|first| {
            let top = first.bbox[1] as u64 * 2;
            let bottom = (first.bbox[1] + first.bbox[3]) as u64 * 2;
            middle >= top && middle <= bottom
        });
        if !joins {
            row.sort_by_key(|l| l.bbox[0]);
            ordered.append(&mut row);
        }
        row.push(line);
    }
    row.sort_by_key(|l| l.bbox[0]);
    ordered.append(&mut row);
    ordered
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(text: &str, bbox: [u32; 4], confidence: f32) -> Line {
        Line {
            text: text.into(),
            bbox,
            confidence,
        }
    }

    #[test]
    fn plan_covers_the_image_with_overlap() {
        let limits = Limits {
            tile_pixels: 6_000,
            tile_max_width: 100,
            min_side: 16,
            ..Limits::default()
        };
        let tiles = plan(300, 60, &limits);
        // Four equal columns, evenly spread, the last ending at the edge.
        let xs: Vec<u32> = tiles.iter().map(|t| t.x).collect();
        assert_eq!(xs, vec![0, 69, 138, 207]);
        assert!(tiles.iter().all(|t| t.w == 93 && t.h == 60 && t.y == 0));
        // Every column is inside at least one tile.
        for x in 0..300 {
            assert!(
                tiles.iter().any(|t| x >= t.x && x < t.x + t.w),
                "column {x}"
            );
        }
        assert_eq!(
            plan(50, 40, &limits),
            vec![Rect {
                x: 0,
                y: 0,
                w: 50,
                h: 40
            }]
        );
    }

    /// Asserts `tiles` cover `width` x `height` as a grid whose neighbours
    /// overlap by at least `floor` on each axis, and returns the grid's
    /// (columns, rows).
    fn assert_grid(tiles: &[Rect], width: u32, height: u32, floor: u32) -> (usize, usize) {
        let mut xs: Vec<u32> = tiles.iter().map(|t| t.x).collect();
        let mut ys: Vec<u32> = tiles.iter().map(|t| t.y).collect();
        xs.sort_unstable();
        xs.dedup();
        ys.sort_unstable();
        ys.dedup();
        assert_eq!(tiles.len(), xs.len() * ys.len(), "not a full grid");
        let (w, h) = (tiles[0].w, tiles[0].h);
        assert!(
            tiles.iter().all(|t| t.w == w && t.h == h),
            "tiles differ in size"
        );
        assert_eq!((xs[0], ys[0]), (0, 0));
        assert_eq!(
            (xs[xs.len() - 1] + w, ys[ys.len() - 1] + h),
            (width, height)
        );
        for pair in xs.windows(2) {
            assert!(
                pair[0] + w >= pair[1] + floor,
                "columns {pair:?} overlap less than {floor}"
            );
        }
        for pair in ys.windows(2) {
            assert!(
                pair[0] + h >= pair[1] + floor,
                "rows {pair:?} overlap less than {floor}"
            );
        }
        (xs.len(), ys.len())
    }

    /// An image just over the width limit used to get two max-width columns
    /// 808 px apart (every row read twice) and strips only 48 px overlapped.
    #[test]
    fn plan_just_over_the_width_limit_has_equal_columns_and_real_overlaps() {
        let limits = Limits::default();
        let tiles = plan(9000, 8000, &limits);
        assert!(tiles.iter().all(|t| t.w <= limits.tile_max_width));
        assert!(tiles
            .iter()
            .all(|t| t.w as u64 * t.h as u64 <= limits.tile_pixels));
        let (cols, rows) = assert_grid(&tiles, 9000, 8000, MIN_OVERLAP);
        // No column is redundant: removing any would leave a gap, so no tile
        // sits mostly inside its neighbour.
        let w = tiles[0].w;
        let mut xs: Vec<u32> = tiles.iter().map(|t| t.x).collect();
        xs.sort_unstable();
        xs.dedup();
        for pair in xs.windows(2) {
            assert!(
                pair[1] - pair[0] >= w / 2,
                "columns {pair:?} read the same pixels twice"
            );
        }
        // Far fewer requests than the 38 (2 x 19 strips) of before.
        assert!(cols * rows <= 24, "{cols} x {rows}");
        // Tall text: every band of `MIN_OVERLAP` rows lies whole inside some tile row.
        let h = tiles[0].h;
        let mut ys: Vec<u32> = tiles.iter().map(|t| t.y).collect();
        ys.sort_unstable();
        ys.dedup();
        for y in (0..8000 - MIN_OVERLAP).step_by(37) {
            assert!(
                ys.iter().any(|&t| y >= t && y + MIN_OVERLAP <= t + h),
                "band at {y} is cut"
            );
        }
    }

    #[test]
    fn plan_prefers_the_grid_with_fewer_tiles_and_shorter_seams() {
        let limits = Limits::default();
        // Narrow and tall: full-width strips (one column), as a document.
        let tiles = plan(1200, 30_000, &limits);
        assert!(tiles.iter().all(|t| t.x == 0 && t.w == 1200));
        assert_grid(&tiles, 1200, 30_000, MIN_OVERLAP);
        // Wide and short fits one row of near-square-or-wider tiles.
        let tiles = plan(30_000, 600, &limits);
        assert!(tiles
            .iter()
            .all(|t| t.h == 600 && t.w <= limits.tile_max_width));
        assert_grid(&tiles, 30_000, 600, MIN_OVERLAP);
        // The largest image the engine accepts: every limit holds.
        let tiles = plan(8000, 8000, &limits);
        assert!(tiles
            .iter()
            .all(|t| t.w as u64 * t.h as u64 <= limits.tile_pixels));
        assert_grid(&tiles, 8000, 8000, MIN_OVERLAP);
    }

    #[test]
    fn overlap_has_a_floor_that_small_tiles_cannot_exceed() {
        assert_eq!(overlap(8192), 819);
        assert_eq!(overlap(1000), MIN_OVERLAP);
        assert_eq!(overlap(488), 122);
        assert_eq!(overlap(100), 25);
        assert_eq!(overlap(3), 0);
    }

    #[test]
    fn split_halves_with_overlap_and_stops_at_the_floor() {
        let [a, b] = split(
            Rect {
                x: 10,
                y: 0,
                w: 120,
                h: 40,
            },
            16,
        )
        .unwrap();
        assert_eq!((a.x, a.w), (10, 75));
        assert_eq!((b.x, b.w), (55, 75));
        // 40 halves to 25 with overlap: allowed at a floor of 25, not 26.
        assert!(split(
            Rect {
                x: 0,
                y: 0,
                w: 40,
                h: 40
            },
            25
        )
        .is_some());
        assert!(split(
            Rect {
                x: 0,
                y: 0,
                w: 40,
                h: 40
            },
            26
        )
        .is_none());
    }

    fn found(piece: usize, tile: Rect, text: &str, bbox: [u32; 4], confidence: f32) -> Found {
        Found {
            piece,
            tile,
            line: line(text, bbox, confidence),
        }
    }

    #[test]
    fn merge_drops_seam_copies_and_orders_rows() {
        let tile = Rect {
            x: 0,
            y: 0,
            w: 200,
            h: 100,
        };
        let found = vec![
            found(1, tile, "world", [60, 10, 30, 10], 0.9),
            found(1, tile, "Hello wor", [0, 10, 55, 10], 0.8),
            found(2, tile, "Hello world", [0, 10, 90, 10], 0.7),
            found(2, tile, "second", [5, 40, 40, 10], 0.9),
            // Same tile: never merged even when it looks the same.
            found(2, tile, "second", [6, 41, 40, 10], 0.5),
        ];
        let lines = merge(found);
        let texts: Vec<&str> = lines.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(texts, vec!["Hello world", "second", "second"]);
    }

    /// A tile that starts mid-line reads the line's tail as something else
    /// ("FE TEXT" for "HUGE IMAGE TEXT"), so the texts share nothing; the box
    /// gives it away.
    #[test]
    fn merge_drops_misread_seam_fragments_by_box() {
        let left = Rect {
            x: 0,
            y: 0,
            w: 5200,
            h: 900,
        };
        let right = Rect {
            x: 4000,
            y: 0,
            w: 5000,
            h: 900,
        };
        let lines = merge(vec![
            found(1, left, "HUGE IMAGE TEXT", [3600, 100, 1500, 160], 0.9),
            // Right tile starts at x = 4000 inside the line: its read of the
            // line's tail is cut at the tile's left edge.
            found(2, right, "FE TEXT", [4000, 100, 1100, 160], 0.4),
        ]);
        let texts: Vec<&str> = lines.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(texts, vec!["HUGE IMAGE TEXT"]);

        // The same across a horizontal seam: the upper tile ends at y = 900,
        // cutting a tall line, and reads a longer garbage string off the slice.
        let upper = Rect {
            x: 4000,
            y: 0,
            w: 5000,
            h: 900,
        };
        let lower = Rect {
            x: 4000,
            y: 700,
            w: 5000,
            h: 900,
        };
        let lines = merge(vec![
            found(
                1,
                upper,
                "an mye ER RL = -_, ee ee CO eee",
                [4300, 780, 1500, 120],
                0.2,
            ),
            found(2, lower, "CORNER TEXT", [4300, 780, 1500, 160], 0.9),
        ]);
        let texts: Vec<&str> = lines.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(texts, vec!["CORNER TEXT"]);
    }

    #[test]
    fn merge_keeps_a_contained_line_that_no_tile_edge_cut() {
        let left = Rect {
            x: 0,
            y: 0,
            w: 1000,
            h: 900,
        };
        let right = Rect {
            x: 800,
            y: 0,
            w: 1000,
            h: 900,
        };
        // A small caption inside a big line, well inside its own tile: real text.
        let lines = merge(vec![
            found(1, left, "BANNER", [850, 300, 100, 200], 0.9),
            found(2, right, "note", [900, 400, 40, 20], 0.9),
        ]);
        assert_eq!(lines.len(), 2);
        // A box touching its tile's edge but only inside a line of its own
        // tile (never compared with it): also kept.
        let lines = merge(vec![
            found(1, left, "BANNER", [0, 300, 100, 200], 0.9),
            found(1, left, "ed", [2, 400, 40, 20], 0.9),
        ]);
        assert_eq!(lines.len(), 2);
    }
}
