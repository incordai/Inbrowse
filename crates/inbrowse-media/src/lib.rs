//! Still frames, speech-ready audio and captions from video and audio files.
//!
//! Entry point: [`extract`]. Callers: `inbrowse-cli` (`inbrowse media`) and
//! `inbrowse-mcp` (`media_extract` tool). Sibling: `inbrowse-extract` reads
//! text from documents with the same limits and error shape.
//!
//! Pipeline: `mp4.rs` / `mkv.rs` demux into `demux.rs` tracks; `video.rs`
//! picks frames (PNG via `image_out.rs`); `audio.rs` writes 16 kHz mono WAV;
//! `captions.rs` reads subtitle tracks and WebVTT files. Decoders come from
//! moq (moq-video, moq-audio) and, with the `vpx` feature, libvpx (`vpx.rs`).
//!
//! Everything read here is untrusted. Parser panics become
//! [`MediaError::Panicked`]; every allocation driven by the input is capped.

mod audio;
mod captions;
mod demux;
mod image_out;
mod mkv;
mod mp4;
mod video;
#[cfg(feature = "vpx")]
mod vpx;

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::time::Instant;

use bytes::Bytes;
use serde::Serialize;

use crate::demux::{Codec, Demuxed, Kind};

/// What to extract and the bounds to stay within.
#[derive(Debug, Clone)]
pub struct Options {
    /// Take a frame at least this often, in seconds.
    pub every_secs: f64,
    /// Most frames written.
    pub max_frames: usize,
    /// Also take a frame where the picture changes sharply.
    pub scene_changes: bool,
    /// Write the first audio track as 16 kHz mono WAV.
    pub audio: bool,
    /// Frames larger than this on their longer side are scaled down.
    pub max_side: u32,
    /// Media past this point is ignored, in seconds.
    pub max_duration_secs: f64,
    /// Largest input accepted, in bytes.
    pub max_input_bytes: usize,
    /// Stop decoding at this instant and return what is done so far.
    pub deadline: Option<Instant>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            every_secs: 10.0,
            max_frames: 20,
            scene_changes: true,
            audio: true,
            max_side: 2048,
            max_duration_secs: 2.0 * 3600.0,
            max_input_bytes: 100 * 1024 * 1024,
            deadline: None,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub container: &'static str,
    pub duration_secs: Option<f64>,
    pub tracks: Vec<TrackReport>,
    pub audio: Option<AudioReport>,
    pub frames: Vec<FrameReport>,
    pub captions: Vec<CaptionTrack>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TrackReport {
    pub kind: &'static str,
    pub codec: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sample_rate: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channels: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    pub supported: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct AudioReport {
    pub format: &'static str,
    pub sample_rate: u32,
    pub channels: u32,
    pub duration_secs: f64,
    pub path: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FrameReason {
    Interval,
    SceneChange,
}

#[derive(Debug, Clone, Serialize)]
pub struct FrameReport {
    pub time_secs: Option<f64>,
    pub reason: FrameReason,
    pub width: u32,
    pub height: u32,
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub embedded_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CaptionTrack {
    pub language: Option<String>,
    pub format: &'static str,
    pub cues: Vec<Cue>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Cue {
    pub start_secs: f64,
    pub end_secs: f64,
    pub text: String,
}

#[derive(Debug)]
pub enum MediaError {
    /// The input exceeds an [`Options`] bound.
    TooLarge(String),
    /// The bytes are not a format this crate reads.
    Unsupported(String),
    /// The format was recognised but the file could not be read.
    Malformed(String),
    /// The output folder could not be written.
    Io(String),
    /// A parser or decoder panicked on this input.
    Panicked,
}

impl std::fmt::Display for MediaError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooLarge(why) => write!(f, "media too large: {why}"),
            Self::Unsupported(why) => write!(f, "unsupported media: {why}"),
            Self::Malformed(why) => write!(f, "could not read media: {why}"),
            Self::Io(why) => write!(f, "could not write output: {why}"),
            Self::Panicked => f.write_str("could not read media: the decoder failed on this input"),
        }
    }
}

impl std::error::Error for MediaError {}

/// Create a fresh, empty folder for one call's PNG and WAV files, under
/// `INBROWSE_MEDIA_DIR` or else the system temp folder. The caller deletes it.
/// Used by `inbrowse-cli` (`run_media`) and `inbrowse-mcp` (`media_extract`).
pub fn new_output_dir() -> std::io::Result<PathBuf> {
    let base = std::env::var_os("INBROWSE_MEDIA_DIR")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    std::fs::create_dir_all(&base)?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    for attempt in 0..100u32 {
        let dir = base.join(format!("inbrowse-media-{}-{stamp}-{attempt}", std::process::id()));
        match std::fs::create_dir(&dir) {
            Ok(()) => return Ok(dir),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::other("could not create a unique output folder"))
}

/// Read `data` and write frames and audio into `out_dir` (created if missing).
///
/// `hint` (file name, extension or MIME type) is not needed for detection; it
/// only makes the error for an unrecognised file more specific.
pub fn extract(data: Bytes, hint: Option<&str>, out_dir: &Path, options: &Options) -> Result<Report, MediaError> {
    if data.len() > options.max_input_bytes {
        return Err(MediaError::TooLarge(format!(
            "{} bytes, limit {}",
            data.len(),
            options.max_input_bytes
        )));
    }
    std::fs::create_dir_all(out_dir).map_err(|e| MediaError::Io(format!("{}: {e}", out_dir.display())))?;
    match catch_unwind(AssertUnwindSafe(|| extract_inner(&data, hint, out_dir, options))) {
        Ok(result) => result,
        Err(_) => Err(MediaError::Panicked),
    }
}

fn extract_inner(data: &Bytes, hint: Option<&str>, out_dir: &Path, options: &Options) -> Result<Report, MediaError> {
    let mut warnings = Vec::new();
    if captions::sniff_webvtt(data) {
        return Ok(Report {
            container: "webvtt",
            duration_secs: None,
            tracks: Vec::new(),
            audio: None,
            frames: Vec::new(),
            captions: vec![captions::parse_webvtt(data)],
            warnings,
        });
    }
    let demuxed = if mp4::sniff(data) {
        mp4::read(data, &mut warnings)?
    } else if mkv::sniff(data) {
        mkv::read(data, &mut warnings)?
    } else if data.len() > 188 && data[0] == 0x47 && data[188] == 0x47 {
        return Err(MediaError::Unsupported("MPEG-TS is not supported yet".into()));
    } else if data.starts_with(b"FLV") {
        return Err(MediaError::Unsupported("FLV is not supported yet".into()));
    } else {
        let what = hint.map(|h| format!(" ({h})")).unwrap_or_default();
        return Err(MediaError::Unsupported(format!(
            "not an MP4, WebM, Matroska or WebVTT file{what}"
        )));
    };
    Ok(extract_with(demuxed, out_dir, options, warnings))
}

fn extract_with(demuxed: Demuxed, out_dir: &Path, options: &Options, mut warnings: Vec<String>) -> Report {
    let tracks: Vec<TrackReport> = demuxed
        .tracks
        .iter()
        .map(|t| TrackReport {
            kind: t.kind.as_str(),
            codec: t.codec_name.clone(),
            width: t.width,
            height: t.height,
            sample_rate: t.sample_rate,
            channels: t.channels,
            language: t.language.clone(),
            supported: t.supported(),
        })
        .collect();
    for t in demuxed.tracks.iter().filter(|t| !t.supported()) {
        let reason = match t.codec {
            Codec::Vp8 | Codec::Vp9 => "this build has no VP8/VP9 decoder".to_string(),
            _ => "only H.264, VP8 and VP9 video and AAC and Opus audio are decoded".to_string(),
        };
        warnings.push(format!("{} {} track not decoded: {reason}", t.codec_name, t.kind.as_str()));
    }

    let max_duration_us = (options.max_duration_secs.max(0.0) * 1e6) as i64;
    let duration_us = demuxed
        .duration_secs
        .map(|d| (d * 1e6) as i64)
        .unwrap_or_else(|| demuxed.tracks.iter().map(|t| t.end_us()).max().unwrap_or(0));
    if duration_us > max_duration_us {
        warnings.push(format!(
            "only the first {:.0} s of {:.0} s are read (duration limit)",
            options.max_duration_secs,
            duration_us as f64 / 1e6
        ));
    }

    let frames = match demuxed.tracks.iter().find(|t| t.kind == Kind::Video && t.supported()) {
        Some(track) => {
            let frame_options = video::FrameOptions {
                every_us: (options.every_secs.max(1.0) * 1e6) as i64,
                max_frames: options.max_frames,
                scene_changes: options.scene_changes,
                max_side: options.max_side,
                deadline: options.deadline,
            };
            video::extract_frames(track, duration_us.min(max_duration_us), &frame_options, out_dir, &mut warnings)
        }
        None => Vec::new(),
    };

    let audio = if options.audio {
        demuxed
            .tracks
            .iter()
            .find(|t| t.kind == Kind::Audio && t.supported())
            .and_then(|track| audio::extract_audio(track, out_dir, max_duration_us, options.deadline, &mut warnings))
    } else {
        None
    };

    let captions = demuxed
        .tracks
        .iter()
        .filter(|t| t.kind == Kind::Subtitle)
        .filter_map(captions::from_track)
        .collect();

    Report {
        container: demuxed.container,
        duration_secs: demuxed.duration_secs.or((duration_us > 0).then(|| duration_us as f64 / 1e6)),
        tracks,
        audio,
        frames,
        captions,
        warnings,
    }
}
