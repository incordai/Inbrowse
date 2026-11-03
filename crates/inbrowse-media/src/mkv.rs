//! Matroska / WebM reader into `demux::Demuxed`.
//!
//! Called from `lib.rs::demux`. Sibling: `mp4.rs`. Tag parsing is
//! webm-iterable; laced blocks are split with `read_frame_data` (moq-mux's own
//! importer passes laced payloads through whole, so it is not reused here).

use std::collections::HashMap;

use bytes::Bytes;
use webm_iterable::matroska_spec::{Block, Master, MatroskaSpec, SimpleBlock};
use webm_iterable::WebmIterator;

use crate::demux::{language, Codec, Demuxed, Kind, Sample, TextFormat, Track};
use crate::MediaError;

const MAX_SAMPLES: usize = 20_000_000;
/// Largest single element accepted, so a corrupt size cannot ask for gigabytes.
const MAX_TAG_BYTES: usize = 64 * 1024 * 1024;

pub(crate) fn sniff(data: &[u8]) -> bool {
    data.starts_with(&[0x1A, 0x45, 0xDF, 0xA3])
}

fn malformed(what: impl std::fmt::Display) -> MediaError {
    MediaError::Malformed(format!("Matroska: {what}"))
}

pub(crate) fn read(data: &Bytes, warnings: &mut Vec<String>) -> Result<Demuxed, MediaError> {
    let mut iter = WebmIterator::new(
        &data[..],
        &[
            MatroskaSpec::Ebml(Master::Start),
            MatroskaSpec::Info(Master::Start),
            MatroskaSpec::Tracks(Master::Start),
            MatroskaSpec::TrackEntry(Master::Start),
            MatroskaSpec::Video(Master::Start),
            MatroskaSpec::Audio(Master::Start),
            MatroskaSpec::BlockGroup(Master::Start),
        ],
    );
    iter.set_max_allowable_tag_size(Some(MAX_TAG_BYTES));

    let mut container = "matroska";
    let mut scale_ns: u64 = 1_000_000;
    let mut duration_ticks: Option<f64> = None;
    let mut cluster_ts: u64 = 0;
    let mut tracks: Vec<Track> = Vec::new();
    // TrackNumber -> index into `tracks`.
    let mut numbers: HashMap<u64, usize> = HashMap::new();
    let mut total = 0usize;
    let mut tracks_seen = false;

    for tag in iter.by_ref() {
        let tag = match tag {
            Ok(tag) => tag,
            Err(err) => {
                if tracks_seen {
                    warnings.push(format!("stopped reading at a damaged element: {err}"));
                    break;
                }
                return Err(malformed(err));
            }
        };
        match tag {
            MatroskaSpec::Ebml(Master::Full(children)) => {
                for child in children {
                    if let MatroskaSpec::DocType(doc) = child {
                        container = if doc == "webm" { "webm" } else { "matroska" };
                    }
                }
            }
            MatroskaSpec::Info(Master::Full(children)) => {
                for child in children {
                    match child {
                        MatroskaSpec::TimestampScale(s) if s > 0 => scale_ns = s,
                        MatroskaSpec::Duration(d) if d.is_finite() && d > 0.0 => duration_ticks = Some(d),
                        _ => {}
                    }
                }
            }
            MatroskaSpec::Tracks(Master::Full(entries)) if !tracks_seen => {
                tracks_seen = true;
                for entry in entries {
                    if let MatroskaSpec::TrackEntry(Master::Full(fields)) = entry {
                        if let Some((number, track)) = track_entry(fields) {
                            numbers.insert(number, tracks.len());
                            tracks.push(track);
                        }
                    }
                }
            }
            MatroskaSpec::Cluster(Master::Start) => cluster_ts = 0,
            MatroskaSpec::Timestamp(ts) => cluster_ts = ts,
            MatroskaSpec::SimpleBlock(raw) => {
                let Ok(block) = SimpleBlock::try_from(raw.as_slice()) else {
                    warnings.push("skipped an unreadable block".into());
                    continue;
                };
                let frames = match block.read_frame_data() {
                    Ok(frames) => frames.into_iter().map(|f| f.data.to_vec()).collect::<Vec<_>>(),
                    Err(_) => {
                        warnings.push("skipped a block with bad lacing".into());
                        continue;
                    }
                };
                push_frames(
                    &mut tracks,
                    &numbers,
                    block.track,
                    cluster_ts,
                    block.timestamp,
                    scale_ns,
                    block.keyframe,
                    None,
                    frames,
                    &mut total,
                )?;
            }
            MatroskaSpec::BlockGroup(Master::Full(children)) => {
                let mut raw = None;
                let mut referenced = false;
                let mut duration = None;
                for child in children {
                    match child {
                        MatroskaSpec::Block(b) => raw = Some(b),
                        MatroskaSpec::ReferenceBlock(_) => referenced = true,
                        MatroskaSpec::BlockDuration(d) => duration = Some(d),
                        _ => {}
                    }
                }
                let Some(raw) = raw else { continue };
                let Ok(block) = Block::try_from(raw.as_slice()) else {
                    warnings.push("skipped an unreadable block".into());
                    continue;
                };
                let frames = match block.read_frame_data() {
                    Ok(frames) => frames.into_iter().map(|f| f.data.to_vec()).collect::<Vec<_>>(),
                    Err(_) => {
                        warnings.push("skipped a block with bad lacing".into());
                        continue;
                    }
                };
                push_frames(
                    &mut tracks,
                    &numbers,
                    block.track,
                    cluster_ts,
                    block.timestamp,
                    scale_ns,
                    !referenced,
                    duration,
                    frames,
                    &mut total,
                )?;
            }
            _ => {}
        }
    }

    if !tracks_seen {
        return Err(malformed("no Tracks element"));
    }
    let duration_secs = duration_ticks.map(|d| d * scale_ns as f64 / 1e9);
    Ok(Demuxed {
        container,
        duration_secs,
        tracks,
    })
}

#[allow(clippy::too_many_arguments)]
fn push_frames(
    tracks: &mut [Track],
    numbers: &HashMap<u64, usize>,
    number: u64,
    cluster_ts: u64,
    relative: i16,
    scale_ns: u64,
    key: bool,
    duration_ticks: Option<u64>,
    frames: Vec<Vec<u8>>,
    total: &mut usize,
) -> Result<(), MediaError> {
    let Some(&index) = numbers.get(&number) else {
        return Ok(());
    };
    let track = &mut tracks[index];
    let ticks = i128::from(cluster_ts) + i128::from(relative);
    let pts_us = (ticks * i128::from(scale_ns) / 1000).clamp(0, i64::MAX as i128) as i64;
    let dur_us = duration_ticks.map(|d| (i128::from(d) * i128::from(scale_ns) / 1000).min(i64::MAX as i128) as i64);
    for frame in frames {
        *total += 1;
        if *total > MAX_SAMPLES {
            return Err(MediaError::TooLarge(format!("more than {MAX_SAMPLES} blocks")));
        }
        track.samples.push(Sample {
            pts_us,
            dur_us,
            key: track.kind != Kind::Video || key,
            data: Bytes::from(frame),
        });
    }
    Ok(())
}

/// Returns the track number and the track, or None for track types this crate
/// does not list (buttons, logos, complex).
fn track_entry(fields: Vec<MatroskaSpec>) -> Option<(u64, Track)> {
    let mut number = None;
    let mut track_type = 0u64;
    let mut codec_id = String::new();
    let mut private: Option<Vec<u8>> = None;
    let mut lang: Option<String> = None;
    let mut width = None;
    let mut height = None;
    let mut rate = None;
    let mut channels = None;
    for field in fields {
        match field {
            MatroskaSpec::TrackNumber(n) => number = Some(n),
            MatroskaSpec::TrackType(t) => track_type = t,
            MatroskaSpec::CodecID(id) => codec_id = id,
            MatroskaSpec::CodecPrivate(p) => private = Some(p),
            MatroskaSpec::Language(l) => lang = Some(l),
            MatroskaSpec::Video(Master::Full(video)) => {
                for v in video {
                    match v {
                        MatroskaSpec::PixelWidth(w) => width = u32::try_from(w).ok(),
                        MatroskaSpec::PixelHeight(h) => height = u32::try_from(h).ok(),
                        _ => {}
                    }
                }
            }
            MatroskaSpec::Audio(Master::Full(audio)) => {
                for a in audio {
                    match a {
                        MatroskaSpec::SamplingFrequency(f) if f.is_finite() && f > 0.0 => rate = Some(f as u32),
                        MatroskaSpec::Channels(c) => channels = u32::try_from(c).ok(),
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    let kind = match track_type {
        1 => Kind::Video,
        2 => Kind::Audio,
        17 => Kind::Subtitle,
        _ => return None,
    };
    let private = private.map(Bytes::from);
    let (codec, name) = match codec_id.as_str() {
        "V_MPEG4/ISO/AVC" => match private {
            Some(avcc) => (Codec::H264 { avcc }, "h264".to_string()),
            None => (Codec::Other, "h264".to_string()),
        },
        "V_VP8" => (Codec::Vp8, "vp8".into()),
        "V_VP9" => (Codec::Vp9, "vp9".into()),
        "V_MPEGH/ISO/HEVC" => (Codec::Other, "h265".into()),
        "V_AV1" => (Codec::Other, "av1".into()),
        "A_AAC" => match private {
            Some(asc) => (Codec::Aac { asc }, "aac".into()),
            None => (Codec::Other, "aac".into()),
        },
        "A_OPUS" => (Codec::Opus { head: private }, "opus".into()),
        "A_VORBIS" => (Codec::Other, "vorbis".into()),
        "A_FLAC" => (Codec::Other, "flac".into()),
        "A_MPEG/L3" => (Codec::Other, "mp3".into()),
        "S_TEXT/WEBVTT" | "D_WEBVTT/SUBTITLES" | "D_WEBVTT/CAPTIONS" => {
            (Codec::Text(TextFormat::WebVttBlock), "webvtt".into())
        }
        "S_TEXT/UTF8" => (Codec::Text(TextFormat::Plain), "text".into()),
        other => (Codec::Other, other.to_ascii_lowercase()),
    };
    let mut track = Track::new(kind, codec, name);
    track.width = width;
    track.height = height;
    track.sample_rate = rate;
    track.channels = channels;
    // Matroska's default language is English when the element is absent.
    track.language = language(lang.as_deref().unwrap_or("eng"));
    Some((number?, track))
}
