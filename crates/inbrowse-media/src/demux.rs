//! What a container reader hands to the decoders: one entry per track with its
//! codec setup and every sample in decode order.
//!
//! Produced by `mp4.rs` (`read`) and `mkv.rs` (`read`); consumed by `video.rs`,
//! `audio.rs` and `captions.rs` through `lib.rs::extract_with`.

use bytes::Bytes;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    Video,
    Audio,
    Subtitle,
}

impl Kind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Kind::Video => "video",
            Kind::Audio => "audio",
            Kind::Subtitle => "subtitle",
        }
    }
}

/// Codec setup a decoder needs, taken from the container's track header.
#[derive(Debug, Clone)]
pub(crate) enum Codec {
    /// `avcc` is the AVCDecoderConfigurationRecord (SPS/PPS out of band).
    H264 {
        avcc: Bytes,
    },
    Vp8,
    Vp9,
    /// `asc` is the AudioSpecificConfig.
    Aac {
        asc: Bytes,
    },
    /// `head` is an OpusHead packet.
    Opus {
        head: Option<Bytes>,
    },
    /// Cue text per sample, in the container's own text encoding.
    Text(TextFormat),
    /// Listed in the report but never decoded.
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TextFormat {
    /// Matroska `S_TEXT/WEBVTT`: each block is the cue payload.
    WebVttBlock,
    /// MP4 `wvtt`: each sample holds `vttc`/`vtte` boxes.
    WebVttMp4,
    /// MP4 `tx3g`: each sample is a 16-bit length then UTF-8 text.
    Tx3g,
    /// Matroska `S_TEXT/UTF8` (SubRip text without timing lines).
    Plain,
}

impl TextFormat {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            TextFormat::WebVttBlock | TextFormat::WebVttMp4 => "webvtt",
            TextFormat::Tx3g => "tx3g",
            TextFormat::Plain => "text",
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct Sample {
    /// Presentation time in microseconds from the start of the file.
    pub pts_us: i64,
    /// Duration in microseconds, when the container states it.
    pub dur_us: Option<i64>,
    pub key: bool,
    pub data: Bytes,
}

#[derive(Debug, Clone)]
pub(crate) struct Track {
    pub kind: Kind,
    pub codec: Codec,
    /// Short codec name for the report, such as `h264`, `vp9`, `aac`.
    pub codec_name: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub sample_rate: Option<u32>,
    pub channels: Option<u32>,
    pub language: Option<String>,
    /// Microseconds before the track's first presented sample: the delay an
    /// MP4 edit list gives it (`mp4.rs`), 0 elsewhere. `audio.rs` writes it as
    /// silence so the WAV shares the file's time axis.
    pub start_us: i64,
    /// Samples in decode order.
    pub samples: Vec<Sample>,
}

impl Track {
    pub(crate) fn new(kind: Kind, codec: Codec, codec_name: impl Into<String>) -> Self {
        Self {
            kind,
            codec,
            codec_name: codec_name.into(),
            width: None,
            height: None,
            sample_rate: None,
            channels: None,
            language: None,
            start_us: 0,
            samples: Vec::new(),
        }
    }

    /// Whether this build can decode the track.
    pub(crate) fn supported(&self) -> bool {
        match &self.codec {
            Codec::H264 { .. } | Codec::Aac { .. } | Codec::Opus { .. } | Codec::Text(_) => true,
            Codec::Vp8 | Codec::Vp9 => cfg!(feature = "vpx"),
            Codec::Other => false,
        }
    }

    /// End of the last sample, in microseconds.
    pub(crate) fn end_us(&self) -> i64 {
        self.samples
            .iter()
            .map(|s| s.pts_us + s.dur_us.unwrap_or(0))
            .max()
            .unwrap_or(0)
    }
}

#[derive(Debug)]
pub(crate) struct Demuxed {
    pub container: &'static str,
    pub duration_secs: Option<f64>,
    pub tracks: Vec<Track>,
}

/// Normalise a container language tag: `und` and empty mean unknown.
pub(crate) fn language(tag: &str) -> Option<String> {
    let tag = tag.trim().trim_end_matches('\0');
    if tag.is_empty() || tag.eq_ignore_ascii_case("und") {
        None
    } else {
        Some(tag.to_string())
    }
}

/// Build the 19-byte OpusHead (RFC 7845 section 5.1, mapping family 0).
pub(crate) fn opus_head(channels: u8, pre_skip: u16, input_rate: u32, gain: i16) -> Bytes {
    let mut head = Vec::with_capacity(19);
    head.extend_from_slice(b"OpusHead");
    head.push(1);
    head.push(channels);
    head.extend_from_slice(&pre_skip.to_le_bytes());
    head.extend_from_slice(&input_rate.to_le_bytes());
    head.extend_from_slice(&gain.to_le_bytes());
    head.push(0);
    Bytes::from(head)
}
