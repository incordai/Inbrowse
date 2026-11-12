//! Picks still frames from a video track and writes them as PNG files.
//!
//! Called from `lib.rs::extract_with`. Decoders: H.264 through moq-video
//! (OpenH264, software only), VP8/VP9 through `vpx.rs`.
//!
//! The track is never decoded end to end. Interval frames decode one GOP each,
//! from the keyframe before the target time. Scene changes compare keyframes
//! only: encoders place a keyframe at a cut, so two neighbouring keyframes that
//! look different mark one. That keeps a two-hour file to a few thousand
//! decoded pictures.

use std::path::Path;
use std::time::Instant;

use hang::catalog::{VideoConfig, H264};
use moq_net::{Timescale, Timestamp};
use moq_video::decode::{Config as DecoderConfig, Decoder, Kind as DecoderKind};
use moq_video::{Output, Size, Surface, I420};

use crate::demux::{Codec, Sample, Track};
use crate::{FrameReason, FrameReport};

/// A decoded picture: tightly packed I420 at even dimensions.
pub(crate) struct Picture {
    pub pts_us: i64,
    pub width: u32,
    pub height: u32,
    pub i420: Vec<u8>,
}

pub(crate) struct FrameOptions {
    pub every_us: i64,
    pub max_frames: usize,
    pub scene_changes: bool,
    pub max_side: u32,
    pub deadline: Option<Instant>,
}

/// Samples decoded past the target before giving up on a closer picture, to
/// let a reordering decoder emit the frames it holds back.
const REORDER_TAIL: usize = 8;
/// Most samples decoded for one interval target.
const MAX_GOP_SAMPLES: usize = 2_000;
/// Mean difference of the colour grid (0 to 255) that counts as a new scene.
const SCENE_THRESHOLD: f64 = 15.0;
/// A scene change this close to a frame already taken adds nothing.
const MIN_GAP_US: i64 = 1_000_000;

enum Backend {
    Moq(Box<Decoder>),
    #[cfg(feature = "vpx")]
    Vpx(crate::vpx::VpxDecoder),
}

pub(crate) struct VideoDecoder {
    backend: Backend,
}

impl VideoDecoder {
    pub(crate) fn open(track: &Track) -> Result<Self, String> {
        match &track.codec {
            Codec::H264 { avcc } => {
                if avcc.len() < 7 {
                    return Err("H.264 track has a short avcC record".into());
                }
                let mut config = VideoConfig::new(H264 {
                    profile: avcc[1],
                    constraints: avcc[2],
                    level: avcc[3],
                    inline: false,
                });
                config.description = Some(avcc.clone());
                config.coded_width = track.width;
                config.coded_height = track.height;
                let mut options = DecoderConfig::new();
                options.kind = DecoderKind::Software;
                options.output = Output::Cpu;
                let decoder = Decoder::new(&config, &options).map_err(|e| e.to_string())?;
                Ok(Self {
                    backend: Backend::Moq(Box::new(decoder)),
                })
            }
            #[cfg(feature = "vpx")]
            Codec::Vp8 | Codec::Vp9 => Ok(Self {
                backend: Backend::Vpx(crate::vpx::VpxDecoder::new(matches!(
                    track.codec,
                    Codec::Vp9
                ))?),
            }),
            _ => Err(format!(
                "{} video is not supported in this build",
                track.codec_name
            )),
        }
    }

    fn decode(&mut self, sample: &Sample) -> Result<Vec<Picture>, String> {
        match &mut self.backend {
            Backend::Moq(decoder) => {
                let ts = Timestamp::from_micros(sample.pts_us.max(0) as u64)
                    .map_err(|e| e.to_string())?;
                let frames = decoder
                    .decode(&sample.data, ts, sample.key)
                    .map_err(|e| e.to_string())?;
                frames.into_iter().map(moq_picture).collect()
            }
            #[cfg(feature = "vpx")]
            Backend::Vpx(decoder) => decoder.decode(&sample.data, sample.pts_us),
        }
    }

    fn flush(&mut self, last_pts_us: i64) -> Result<Vec<Picture>, String> {
        match &mut self.backend {
            Backend::Moq(decoder) => {
                let frames = decoder.flush().map_err(|e| e.to_string())?;
                frames.into_iter().map(moq_picture).collect()
            }
            #[cfg(feature = "vpx")]
            Backend::Vpx(decoder) => decoder.flush(last_pts_us),
            #[allow(unreachable_patterns)]
            _ => {
                let _ = last_pts_us;
                Ok(Vec::new())
            }
        }
    }
}

fn moq_picture(frame: moq_video::Frame) -> Result<Picture, String> {
    let pts_us = frame
        .timestamp
        .as_scale(Timescale::MICRO)
        .min(i64::MAX as u128) as i64;
    let i420 = frame.surface.into_i420().map_err(|e| e.to_string())?;
    Ok(Picture {
        pts_us,
        width: i420.width(),
        height: i420.height(),
        i420: i420.into_data(),
    })
}

/// Colour layout signature: 16x16 luma cells and 8x8 cells of each chroma plane.
fn signature(p: &Picture) -> Vec<f64> {
    let (w, h) = (p.width as usize, p.height as usize);
    let (cw, ch) = (w / 2, h / 2);
    let y = &p.i420[..w * h];
    let u = &p.i420[w * h..w * h + cw * ch];
    let v = &p.i420[w * h + cw * ch..];
    let mut sig = grid(y, w, h, 16);
    sig.extend(grid(u, cw, ch, 8));
    sig.extend(grid(v, cw, ch, 8));
    sig
}

fn grid(plane: &[u8], w: usize, h: usize, cells: usize) -> Vec<f64> {
    let mut sums = vec![0f64; cells * cells];
    let mut counts = vec![0u32; cells * cells];
    // Sample every other pixel in both directions; plenty for a coarse grid.
    for row in (0..h).step_by(2) {
        let cy = row * cells / h.max(1);
        for col in (0..w).step_by(2) {
            let cx = col * cells / w.max(1);
            let i = cy * cells + cx;
            sums[i] += f64::from(plane[row * w + col]);
            counts[i] += 1;
        }
    }
    sums.iter()
        .zip(&counts)
        .map(|(s, &c)| if c == 0 { 0.0 } else { s / f64::from(c) })
        .collect()
}

fn distance(a: &[f64], b: &[f64]) -> f64 {
    if a.len() != b.len() || a.is_empty() {
        return f64::MAX;
    }
    a.iter().zip(b).map(|(x, y)| (x - y).abs()).sum::<f64>() / a.len() as f64
}

fn past(deadline: Option<Instant>) -> bool {
    deadline.is_some_and(|d| Instant::now() >= d)
}

/// Decode from the keyframe at or before `target_us` and return the picture
/// shown at that time (the first one at or after it, else the last before it).
fn picture_at(
    decoder: &mut VideoDecoder,
    samples: &[Sample],
    keyframes: &[usize],
    target_us: i64,
    errors: &mut usize,
) -> Option<Picture> {
    let start_at = keyframes
        .iter()
        .rev()
        .find(|&&k| samples[k].pts_us <= target_us)
        .or(keyframes.first())
        .copied()?;
    let mut pictures = Vec::new();
    let mut tail = None;
    let mut last_pts = samples[start_at].pts_us;
    for (n, sample) in samples[start_at..].iter().enumerate() {
        if n > 0 && sample.key && tail.is_some() {
            break;
        }
        if n >= MAX_GOP_SAMPLES {
            break;
        }
        match decoder.decode(sample) {
            Ok(mut p) => pictures.append(&mut p),
            Err(_) => *errors += 1,
        }
        last_pts = sample.pts_us;
        if tail.is_none() && sample.pts_us >= target_us {
            tail = Some(n + REORDER_TAIL);
        }
        if tail.is_some_and(|t| n >= t) {
            break;
        }
    }
    match decoder.flush(last_pts) {
        Ok(mut p) => pictures.append(&mut p),
        Err(_) => *errors += 1,
    }
    let after = pictures
        .iter()
        .enumerate()
        .filter(|(_, p)| p.pts_us >= target_us)
        .min_by_key(|(_, p)| p.pts_us)
        .map(|(i, _)| i);
    let index = after.or_else(|| {
        pictures
            .iter()
            .enumerate()
            .max_by_key(|(_, p)| p.pts_us)
            .map(|(i, _)| i)
    })?;
    Some(pictures.swap_remove(index))
}

pub(crate) fn extract_frames(
    track: &Track,
    duration_us: i64,
    opts: &FrameOptions,
    out_dir: &Path,
    warnings: &mut Vec<String>,
) -> Vec<FrameReport> {
    let samples = &track.samples;
    let keyframes: Vec<usize> = samples
        .iter()
        .enumerate()
        .filter(|(_, s)| s.key)
        .map(|(i, _)| i)
        .collect();
    if samples.is_empty() || keyframes.is_empty() || opts.max_frames == 0 {
        return Vec::new();
    }
    let mut decoder = match VideoDecoder::open(track) {
        Ok(d) => d,
        Err(e) => {
            warnings.push(e);
            return Vec::new();
        }
    };
    let duration_us = duration_us.max(1);

    // Interval frames get most of the budget; scene changes the rest.
    let interval_budget = if opts.scene_changes {
        (opts.max_frames - opts.max_frames / 4).max(1)
    } else {
        opts.max_frames
    };
    let mut every = opts.every_us.max(1);
    let wanted = (duration_us + every - 1) / every;
    if wanted > interval_budget as i64 {
        every = (duration_us + interval_budget as i64 - 1) / interval_budget as i64;
        warnings.push(format!(
            "frame interval widened to {:.1} s to stay within {} frames",
            every as f64 / 1e6,
            opts.max_frames
        ));
    }

    let mut errors = 0usize;
    let mut taken: Vec<(Picture, FrameReason)> = Vec::new();
    let mut timed_out = false;
    let mut target = 0i64;
    while target < duration_us && taken.len() < interval_budget {
        if past(opts.deadline) {
            timed_out = true;
            break;
        }
        if let Some(p) = picture_at(&mut decoder, samples, &keyframes, target, &mut errors) {
            if !taken.iter().any(|(t, _)| t.pts_us == p.pts_us) {
                taken.push((p, FrameReason::Interval));
            }
        }
        target += every;
    }

    if opts.scene_changes && !timed_out {
        let mut previous: Option<Vec<f64>> = None;
        for &k in &keyframes {
            if taken.len() >= opts.max_frames {
                break;
            }
            if past(opts.deadline) {
                timed_out = true;
                break;
            }
            let mut pictures = match decoder.decode(&samples[k]) {
                Ok(p) => p,
                Err(_) => {
                    errors += 1;
                    Vec::new()
                }
            };
            match decoder.flush(samples[k].pts_us) {
                Ok(mut p) => pictures.append(&mut p),
                Err(_) => errors += 1,
            }
            let Some(p) = pictures.into_iter().min_by_key(|p| p.pts_us) else {
                continue;
            };
            let sig = signature(&p);
            let changed = previous
                .as_ref()
                .is_some_and(|prev| distance(prev, &sig) > SCENE_THRESHOLD);
            previous = Some(sig);
            if changed
                && !taken
                    .iter()
                    .any(|(t, _)| (t.pts_us - p.pts_us).abs() < MIN_GAP_US)
            {
                taken.push((p, FrameReason::SceneChange));
            }
        }
    }

    if timed_out {
        warnings.push("time limit reached; frame extraction stopped early".into());
    }
    if errors > 0 {
        warnings.push(format!("{errors} video packets could not be decoded"));
    }

    taken.sort_by_key(|(p, _)| p.pts_us);
    let mut reports = Vec::new();
    for (index, (picture, reason)) in taken.into_iter().enumerate() {
        match write_png(&picture, opts.max_side, out_dir, index) {
            Ok((path, width, height)) => reports.push(FrameReport {
                time_secs: Some(picture.pts_us.max(0) as f64 / 1e6),
                reason,
                width,
                height,
                path,
                embedded_name: None,
                page: None,
            }),
            Err(e) => warnings.push(format!("could not write frame {index}: {e}")),
        }
    }
    reports
}

/// Convert to RGBA with moq-video's colour conversion, scale so the longer
/// side is at most `max_side`, and save as PNG.
fn write_png(
    picture: &Picture,
    max_side: u32,
    out_dir: &Path,
    index: usize,
) -> Result<(String, u32, u32), String> {
    let i420 = I420::new(
        Size::new(picture.width, picture.height),
        picture.i420.clone(),
    )
    .map_err(|e| e.to_string())?;
    let rgba = Surface::I420(i420)
        .to_rgba(&moq_video::convert::Config::new())
        .map_err(|e| e.to_string())?;
    let (w, h) = (rgba.width(), rgba.height());
    let image =
        image::RgbaImage::from_raw(w, h, rgba.into_data()).ok_or("pixel buffer size mismatch")?;
    let path = out_dir.join(format!(
        "frame-{:04}-{}ms.png",
        index,
        picture.pts_us.max(0) / 1000
    ));
    crate::image_out::save_png(image, max_side, &path)
}
