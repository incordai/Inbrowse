//! Caption cues from subtitle tracks inside a container, and from standalone
//! WebVTT files.
//!
//! Called from `lib.rs::extract_with` (tracks) and `lib.rs::extract_inner`
//! (`.vtt` input). Markup tags inside cue text are removed, so agents get
//! plain text.

use crate::demux::{Codec, TextFormat, Track};
use crate::{CaptionTrack, Cue};

/// Default length of a cue whose container gives no duration.
const DEFAULT_CUE_US: i64 = 2_000_000;

pub(crate) fn from_track(track: &Track) -> Option<CaptionTrack> {
    let Codec::Text(format) = track.codec else {
        return None;
    };
    let mut cues: Vec<Cue> = Vec::new();
    for (i, sample) in track.samples.iter().enumerate() {
        let text = match format {
            TextFormat::WebVttBlock | TextFormat::Plain => {
                String::from_utf8_lossy(&sample.data).into_owned()
            }
            TextFormat::WebVttMp4 => vtt_mp4_text(&sample.data),
            TextFormat::Tx3g => tx3g_text(&sample.data),
        };
        let text = clean(&text);
        if text.is_empty() {
            continue;
        }
        let end = match sample.dur_us {
            Some(d) if d > 0 => sample.pts_us + d,
            _ => track
                .samples
                .get(i + 1)
                .map(|n| n.pts_us)
                .filter(|&n| n > sample.pts_us)
                .unwrap_or(sample.pts_us + DEFAULT_CUE_US),
        };
        // Over before the presentation starts (an MP4 edit list can cut the
        // start off): nobody sees it.
        if end <= 0 {
            continue;
        }
        cues.push(Cue {
            start_secs: sample.pts_us.max(0) as f64 / 1e6,
            end_secs: end.max(0) as f64 / 1e6,
            text,
        });
    }
    Some(CaptionTrack {
        language: track.language.clone(),
        format: format.as_str(),
        cues,
    })
}

/// Text of the `payl` boxes inside the `vttc` boxes of an MP4 WebVTT sample.
fn vtt_mp4_text(data: &[u8]) -> String {
    let mut out = String::new();
    walk_boxes(data, &mut |kind, body| {
        if kind == b"vttc" {
            walk_boxes(body, &mut |inner, payload| {
                if inner == b"payl" {
                    if !out.is_empty() {
                        out.push('\n');
                    }
                    out.push_str(&String::from_utf8_lossy(payload));
                }
            });
        }
    });
    out
}

fn walk_boxes(mut data: &[u8], f: &mut dyn FnMut(&[u8], &[u8])) {
    while data.len() >= 8 {
        let size = u32::from_be_bytes([data[0], data[1], data[2], data[3]]) as usize;
        if size < 8 || size > data.len() {
            return;
        }
        f(&data[4..8], &data[8..size]);
        data = &data[size..];
    }
}

/// A tx3g sample is a 16-bit big-endian length, then UTF-8 text.
fn tx3g_text(data: &[u8]) -> String {
    if data.len() < 2 {
        return String::new();
    }
    let len = usize::from(u16::from_be_bytes([data[0], data[1]]));
    let end = (2 + len).min(data.len());
    String::from_utf8_lossy(&data[2..end]).into_owned()
}

/// Drop markup tags (`<b>`, `<c.yellow>`, `<00:01.000>`), decode the few
/// entities WebVTT defines, and trim each line.
fn clean(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_tag = false;
    for c in text.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    let out = out
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&nbsp;", " ")
        .replace("&lrm;", "")
        .replace("&rlm;", "")
        .replace("&amp;", "&");
    out.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Whether the bytes are a WebVTT file.
pub(crate) fn sniff_webvtt(data: &[u8]) -> bool {
    let data = data.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(data);
    data.starts_with(b"WEBVTT")
        && data
            .get(6)
            .is_none_or(|c| matches!(c, b' ' | b'\t' | b'\n' | b'\r'))
}

/// Parse a standalone WebVTT file.
pub(crate) fn parse_webvtt(data: &[u8]) -> CaptionTrack {
    let text = String::from_utf8_lossy(data)
        .replace("\r\n", "\n")
        .replace('\r', "\n");
    let mut cues = Vec::new();
    for block in text.split("\n\n") {
        let mut lines = block.lines().skip_while(|l| l.trim().is_empty());
        let mut timing = None;
        let mut body = Vec::new();
        for line in lines.by_ref() {
            if timing.is_none() {
                if line.contains("-->") {
                    timing = parse_timing(line);
                    if timing.is_none() {
                        break;
                    }
                }
                // A line before the timing is the cue id (or a header/NOTE line).
                continue;
            }
            body.push(line);
        }
        let Some((start, end)) = timing else { continue };
        let text = clean(&body.join("\n"));
        if text.is_empty() {
            continue;
        }
        cues.push(Cue {
            start_secs: start,
            end_secs: end,
            text,
        });
    }
    CaptionTrack {
        language: None,
        format: "webvtt",
        cues,
    }
}

fn parse_timing(line: &str) -> Option<(f64, f64)> {
    let (a, b) = line.split_once("-->")?;
    let start = parse_time(a.trim())?;
    let end = parse_time(b.split_whitespace().next()?)?;
    Some((start, end))
}

/// `hh:mm:ss.ttt` or `mm:ss.ttt`.
fn parse_time(t: &str) -> Option<f64> {
    let parts: Vec<&str> = t.split(':').collect();
    let (h, m, s) = match parts.as_slice() {
        [h, m, s] => (
            h.parse::<f64>().ok()?,
            m.parse::<f64>().ok()?,
            s.parse::<f64>().ok()?,
        ),
        [m, s] => (0.0, m.parse::<f64>().ok()?, s.parse::<f64>().ok()?),
        _ => return None,
    };
    let v = h * 3600.0 + m * 60.0 + s;
    v.is_finite().then_some(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn webvtt_file_cues_and_tags() {
        let vtt = b"WEBVTT\n\nNOTE a comment\n\n1\n00:00:01.000 --> 00:00:02.500 align:start\n<v Bob>Hello &amp; welcome</v>\n\n00:03.000 --> 00:04.000\nsecond\nline\n";
        assert!(sniff_webvtt(vtt));
        let track = parse_webvtt(vtt);
        assert_eq!(track.cues.len(), 2);
        assert_eq!(track.cues[0].start_secs, 1.0);
        assert_eq!(track.cues[0].end_secs, 2.5);
        assert_eq!(track.cues[0].text, "Hello & welcome");
        assert_eq!(track.cues[1].text, "second\nline");
    }

    #[test]
    fn mp4_cue_boxes() {
        let mut payl = Vec::new();
        payl.extend_from_slice(&(8u32 + 2).to_be_bytes());
        payl.extend_from_slice(b"payl");
        payl.extend_from_slice(b"hi");
        let mut vttc = Vec::new();
        vttc.extend_from_slice(&(8u32 + payl.len() as u32).to_be_bytes());
        vttc.extend_from_slice(b"vttc");
        vttc.extend_from_slice(&payl);
        assert_eq!(vtt_mp4_text(&vttc), "hi");
        assert_eq!(tx3g_text(&[0, 3, b'a', b'b', b'c']), "abc");
    }

    /// A cue an MP4 edit list leaves wholly before time 0 is dropped; one
    /// that straddles 0 starts at 0.
    #[test]
    fn cues_before_the_presentation_start_are_dropped() {
        use crate::demux::{Kind, Sample, Track};
        let mut track = Track::new(Kind::Subtitle, Codec::Text(TextFormat::Plain), "text");
        for (pts_us, dur_us, text) in [
            (-3_000_000, 1_000_000, "gone"),
            (-500_000, 2_000_000, "cut"),
            (1_000_000, 1_000_000, "kept"),
        ] {
            track.samples.push(Sample {
                pts_us,
                dur_us: Some(dur_us),
                key: true,
                data: text.as_bytes().to_vec().into(),
            });
        }
        let cues = from_track(&track).unwrap().cues;
        let shown: Vec<(&str, f64, f64)> = cues
            .iter()
            .map(|c| (c.text.as_str(), c.start_secs, c.end_secs))
            .collect();
        assert_eq!(shown, vec![("cut", 0.0, 1.5), ("kept", 1.0, 2.0)]);
    }
}
