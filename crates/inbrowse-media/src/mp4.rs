//! MP4 / M4A / MOV reader: progressive files (sample tables in `moov`) and
//! fragmented files (`moof` + `trun`), both into `demux::Demuxed`.
//!
//! Called from `lib.rs::demux`. Sibling: `mkv.rs` does the same for
//! Matroska/WebM. Box parsing is mp4-atom; `mdat` is never decoded as a box
//! (mp4-atom would copy it), samples are sliced out of the input instead.

use bytes::Bytes;
use mp4_atom::{Atom, Codec as Mp4Codec, Decode, FourCC, Moof, Moov, StszSamples, Trak};

use crate::demux::{language, opus_head, Codec, Demuxed, Kind, Sample, TextFormat, Track};
use crate::MediaError;

/// Most samples accepted over all tracks. Each costs a few dozen bytes of index.
const MAX_SAMPLES: usize = 20_000_000;

/// Whether the bytes start like an ISO-BMFF file.
pub(crate) fn sniff(data: &[u8]) -> bool {
    data.len() >= 8
        && matches!(
            &data[4..8],
            b"ftyp" | b"moov" | b"mdat" | b"free" | b"skip" | b"wide" | b"styp" | b"pdin"
        )
}

struct TrackState {
    id: u32,
    timescale: u32,
    /// Index into `Demuxed::tracks`.
    index: usize,
    next_dts: u64,
}

fn malformed(what: impl std::fmt::Display) -> MediaError {
    MediaError::Malformed(format!("MP4: {what}"))
}

/// Returns (kind, total box length including the header).
fn box_header(data: &[u8]) -> Result<([u8; 4], usize), MediaError> {
    let size32 = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);
    let kind = [data[4], data[5], data[6], data[7]];
    let len = match size32 {
        0 => data.len(),
        1 => {
            if data.len() < 16 {
                return Err(malformed("truncated 64-bit box header"));
            }
            let mut b = [0u8; 8];
            b.copy_from_slice(&data[8..16]);
            let size = u64::from_be_bytes(b);
            if size < 16 {
                return Err(malformed("box smaller than its header"));
            }
            usize::try_from(size).map_err(|_| malformed("box too large"))?
        }
        n if n < 8 => return Err(malformed("box smaller than its header")),
        n => n as usize,
    };
    Ok((kind, len))
}

fn to_us(t: i128, timescale: u32) -> i64 {
    let us = t * 1_000_000 / i128::from(timescale.max(1));
    us.clamp(i64::MIN as i128, i64::MAX as i128) as i64
}

pub(crate) fn read(data: &Bytes, warnings: &mut Vec<String>) -> Result<Demuxed, MediaError> {
    let mut moov: Option<Moov> = None;
    let mut fragments: Vec<(usize, Moof)> = Vec::new();
    let mut pos = 0usize;
    while pos + 8 <= data.len() {
        let (kind, len) = box_header(&data[pos..])?;
        let mut end = pos.checked_add(len).ok_or_else(|| malformed("box too large"))?;
        if end > data.len() {
            if &kind == b"mdat" {
                warnings.push("the file is truncated; samples past the end are skipped".into());
                end = data.len();
            } else {
                return Err(malformed(format!(
                    "{} box runs past the end of the file",
                    String::from_utf8_lossy(&kind)
                )));
            }
        }
        match &kind {
            b"moov" => {
                moov = Some(Moov::decode(&mut &data[pos..end]).map_err(malformed)?);
            }
            b"moof" => {
                fragments.push((pos, Moof::decode(&mut &data[pos..end]).map_err(malformed)?));
            }
            _ => {}
        }
        pos = end;
    }
    let moov = moov.ok_or_else(|| malformed("no moov box"))?;

    let mut tracks = Vec::new();
    let mut states = Vec::new();
    let mut total = 0usize;
    for trak in &moov.trak {
        let Some(mut track) = track_from(trak, warnings) else {
            continue;
        };
        let timescale = trak.mdia.mdhd.timescale;
        if timescale == 0 {
            warnings.push(format!("{} track has timescale 0; skipped", track.codec_name));
            continue;
        }
        track.samples = progressive(trak, timescale, track.kind, data, &mut total, warnings)?;
        let next_dts = trak
            .mdia
            .minf
            .stbl
            .stts
            .entries
            .iter()
            .map(|e| u64::from(e.sample_count) * u64::from(e.sample_delta))
            .sum();
        states.push(TrackState {
            id: trak.tkhd.track_id,
            timescale,
            index: tracks.len(),
            next_dts,
        });
        tracks.push(track);
    }

    for (moof_pos, moof) in &fragments {
        for traf in &moof.traf {
            let Some(state) = states.iter_mut().find(|s| s.id == traf.tfhd.track_id) else {
                continue;
            };
            let trex = moov
                .mvex
                .as_ref()
                .and_then(|m| m.trex.iter().find(|t| t.track_id == state.id));
            let track = &mut tracks[state.index];
            let base = match traf.tfhd.base_data_offset {
                Some(b) => usize::try_from(b).map_err(|_| malformed("base_data_offset too large"))?,
                None => *moof_pos,
            };
            if let Some(tfdt) = &traf.tfdt {
                state.next_dts = tfdt.base_media_decode_time;
            }
            let mut data_pos = base;
            for trun in &traf.trun {
                if let Some(offset) = trun.data_offset {
                    let p = base as i64 + i64::from(offset);
                    data_pos = usize::try_from(p).map_err(|_| malformed("negative trun data offset"))?;
                }
                for entry in &trun.entries {
                    let duration = entry
                        .duration
                        .or(traf.tfhd.default_sample_duration)
                        .or(trex.map(|t| t.default_sample_duration))
                        .unwrap_or(0);
                    let size = entry
                        .size
                        .or(traf.tfhd.default_sample_size)
                        .or(trex.map(|t| t.default_sample_size))
                        .unwrap_or(0) as usize;
                    let flags = entry
                        .flags
                        .or(traf.tfhd.default_sample_flags)
                        .or(trex.map(|t| t.default_sample_flags))
                        .unwrap_or(0);
                    let cts = i128::from(entry.cts.unwrap_or(0));
                    let pts = i128::from(state.next_dts) + cts;
                    let end = data_pos.saturating_add(size);
                    if end <= data.len() {
                        total += 1;
                        if total > MAX_SAMPLES {
                            return Err(MediaError::TooLarge(format!("more than {MAX_SAMPLES} samples")));
                        }
                        track.samples.push(Sample {
                            pts_us: to_us(pts, state.timescale),
                            dur_us: Some(to_us(i128::from(duration), state.timescale)),
                            key: track.kind != Kind::Video || is_sync_sample(flags),
                            data: data.slice(data_pos..end),
                        });
                    }
                    data_pos = end;
                    state.next_dts += u64::from(duration);
                }
            }
        }
    }

    let mvhd = &moov.mvhd;
    let duration_secs = if mvhd.timescale > 0 && mvhd.duration > 0 {
        Some(mvhd.duration as f64 / f64::from(mvhd.timescale))
    } else if let Some(mehd) = moov.mvex.as_ref().and_then(|m| m.mehd.as_ref()) {
        (mvhd.timescale > 0 && mehd.fragment_duration > 0)
            .then(|| mehd.fragment_duration as f64 / f64::from(mvhd.timescale))
    } else {
        None
    };

    Ok(Demuxed {
        container: "mp4",
        duration_secs,
        tracks,
    })
}

/// Sample flags: `sample_depends_on == 2` and not `sample_is_non_sync_sample`.
fn is_sync_sample(flags: u32) -> bool {
    let depends_on_none = (flags >> 24) & 0x3 == 0x2;
    let non_sync = (flags >> 16) & 0x1 == 0x1;
    depends_on_none && !non_sync
}

fn track_from(trak: &Trak, warnings: &mut Vec<String>) -> Option<Track> {
    let handler = trak.mdia.hdlr.handler;
    let kind = if handler == FourCC::new(b"vide") {
        Kind::Video
    } else if handler == FourCC::new(b"soun") {
        Kind::Audio
    } else if handler == FourCC::new(b"text")
        || handler == FourCC::new(b"sbtl")
        || handler == FourCC::new(b"subt")
    {
        Kind::Subtitle
    } else {
        return None;
    };
    let codecs = &trak.mdia.minf.stbl.stsd.codecs;
    if codecs.len() > 1 {
        warnings.push(format!(
            "track {} has {} sample descriptions; only the first is used",
            trak.tkhd.track_id,
            codecs.len()
        ));
    }
    let mut track = match codecs.first() {
        Some(Mp4Codec::Avc1(avc1)) => {
            let mut avcc = Vec::new();
            if avc1.avcc.encode_body(&mut avcc).is_err() {
                warnings.push("H.264 track has an unreadable avcC; skipped".into());
                Track::new(kind, Codec::Other, "h264")
            } else {
                let mut t = Track::new(kind, Codec::H264 { avcc: Bytes::from(avcc) }, "h264");
                t.width = Some(u32::from(avc1.visual.width));
                t.height = Some(u32::from(avc1.visual.height));
                t
            }
        }
        Some(Mp4Codec::Hev1(v)) => sized(Track::new(kind, Codec::Other, "h265"), v.visual.width, v.visual.height),
        Some(Mp4Codec::Hvc1(v)) => sized(Track::new(kind, Codec::Other, "h265"), v.visual.width, v.visual.height),
        Some(Mp4Codec::Vp08(v)) => sized(Track::new(kind, Codec::Vp8, "vp8"), v.visual.width, v.visual.height),
        Some(Mp4Codec::Vp09(v)) => sized(Track::new(kind, Codec::Vp9, "vp9"), v.visual.width, v.visual.height),
        Some(Mp4Codec::Av01(v)) => sized(Track::new(kind, Codec::Other, "av1"), v.visual.width, v.visual.height),
        Some(Mp4Codec::Mp4a(mp4a)) => {
            let config = &mp4a.esds.es_desc.dec_config;
            let mut t = match (&config.dec_specific, config.object_type_indication) {
                (Some(specific), 0x40) => Track::new(
                    kind,
                    Codec::Aac {
                        asc: Bytes::copy_from_slice(&specific.raw),
                    },
                    "aac",
                ),
                (_, 0x69 | 0x6B) => Track::new(kind, Codec::Other, "mp3"),
                _ => Track::new(kind, Codec::Other, "mp4a"),
            };
            t.sample_rate = Some(u32::from(mp4a.audio.sample_rate.integer()));
            t.channels = Some(u32::from(mp4a.audio.channel_count));
            t
        }
        Some(Mp4Codec::Opus(opus)) => {
            let d = &opus.dops;
            let mut t = Track::new(
                kind,
                Codec::Opus {
                    head: Some(opus_head(d.output_channel_count, d.pre_skip, d.input_sample_rate, d.output_gain)),
                },
                "opus",
            );
            t.sample_rate = Some(48_000);
            t.channels = Some(u32::from(d.output_channel_count));
            t
        }
        Some(Mp4Codec::Wvtt(_)) => Track::new(Kind::Subtitle, Codec::Text(TextFormat::WebVttMp4), "webvtt"),
        Some(Mp4Codec::Tx3g(_)) => Track::new(Kind::Subtitle, Codec::Text(TextFormat::Tx3g), "tx3g"),
        Some(Mp4Codec::Flac(_)) => Track::new(kind, Codec::Other, "flac"),
        Some(Mp4Codec::Unknown(fourcc)) => Track::new(kind, Codec::Other, fourcc.to_string()),
        Some(_) => Track::new(kind, Codec::Other, "other"),
        None => return None,
    };
    track.language = language(&trak.mdia.mdhd.language);
    Some(track)
}

fn sized(mut track: Track, width: u16, height: u16) -> Track {
    track.width = Some(u32::from(width));
    track.height = Some(u32::from(height));
    track
}

/// Samples listed in the `stbl` sample tables (empty for a fragmented file,
/// whose samples live in `moof` boxes).
fn progressive(
    trak: &Trak,
    timescale: u32,
    kind: Kind,
    data: &Bytes,
    total: &mut usize,
    warnings: &mut Vec<String>,
) -> Result<Vec<Sample>, MediaError> {
    let stbl = &trak.mdia.minf.stbl;
    let (count, fixed_size, sizes): (usize, u32, &[u32]) = match &stbl.stsz.samples {
        StszSamples::Identical { count, size } => (*count as usize, *size, &[]),
        StszSamples::Different { sizes } => (sizes.len(), 0, sizes.as_slice()),
    };
    if count == 0 {
        return Ok(Vec::new());
    }
    *total += count;
    if *total > MAX_SAMPLES {
        return Err(MediaError::TooLarge(format!("more than {MAX_SAMPLES} samples")));
    }
    let size_of = |i: usize| -> usize {
        if sizes.is_empty() {
            fixed_size as usize
        } else {
            sizes[i] as usize
        }
    };

    let chunk_offsets: Vec<u64> = match (&stbl.stco, &stbl.co64) {
        (Some(stco), _) => stco.entries.iter().map(|&o| u64::from(o)).collect(),
        (None, Some(co64)) => co64.entries.clone(),
        (None, None) => return Err(malformed("sample table without chunk offsets")),
    };

    // Byte ranges, walking chunks with the stsc run-length table.
    let mut ranges: Vec<(usize, usize)> = Vec::with_capacity(count);
    let stsc = &stbl.stsc.entries;
    let mut entry = 0usize;
    'chunks: for (index, &chunk_offset) in chunk_offsets.iter().enumerate() {
        let chunk_number = index as u32 + 1;
        while entry + 1 < stsc.len() && stsc[entry + 1].first_chunk <= chunk_number {
            entry += 1;
        }
        let per_chunk = stsc.get(entry).map(|e| e.samples_per_chunk).unwrap_or(0);
        let mut offset = usize::try_from(chunk_offset).map_err(|_| malformed("chunk offset too large"))?;
        for _ in 0..per_chunk {
            if ranges.len() >= count {
                break 'chunks;
            }
            let size = size_of(ranges.len());
            ranges.push((offset, size));
            offset = offset.saturating_add(size);
        }
    }

    let mut deltas = stbl
        .stts
        .entries
        .iter()
        .flat_map(|e| std::iter::repeat_n(e.sample_delta, e.sample_count as usize));
    // Composition offsets are read as signed 32-bit whatever the box version.
    // Version 0 is unsigned by the spec, but encoders store B-frame offsets
    // like -1000 there too; read unsigned, each one put its sample about 2^32
    // ticks (39.8 hours at 30000/s) late. A real offset past 2^31 ticks would
    // be longer than any file this reads.
    let mut offsets = stbl
        .ctts
        .iter()
        .flat_map(|c| c.entries.iter())
        .flat_map(|e| std::iter::repeat_n(e.sample_offset as i32, e.sample_count as usize));
    let sync = stbl.stss.as_ref().map(|s| s.entries.as_slice());
    let mut sync_at = 0usize;

    let mut samples = Vec::with_capacity(ranges.len());
    let mut dts: u64 = 0;
    let mut truncated = false;
    for (i, &(offset, size)) in ranges.iter().enumerate() {
        let delta = deltas.next().unwrap_or(0);
        let cts = offsets.next().unwrap_or(0);
        let key = match sync {
            None => true,
            Some(list) => {
                let number = i as u32 + 1;
                while sync_at < list.len() && list[sync_at] < number {
                    sync_at += 1;
                }
                sync_at < list.len() && list[sync_at] == number
            }
        };
        let end = offset.saturating_add(size);
        if end <= data.len() {
            samples.push(Sample {
                pts_us: to_us(i128::from(dts) + i128::from(cts), timescale),
                dur_us: Some(to_us(i128::from(delta), timescale)),
                key: kind != Kind::Video || key,
                data: data.slice(offset..end),
            });
        } else {
            truncated = true;
        }
        dts += u64::from(delta);
    }
    if truncated {
        warnings.push(format!(
            "{} track: some samples lie past the end of the file and were skipped",
            kind.as_str()
        ));
    }
    Ok(samples)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 2 s, 10 fps H.264 file with B-frames whose ctts is version 0 but holds
    /// negative offsets (-1024 at a 10240 timescale); recipe in
    /// tests/fixtures/README.md.
    #[test]
    fn negative_offsets_in_a_version_0_ctts_are_signed() {
        let data = Bytes::from_static(include_bytes!("../tests/fixtures/h264_ctts_v0_negative.mp4"));
        let mut warnings = Vec::new();
        let demuxed = read(&data, &mut warnings).unwrap();
        let mut pts: Vec<i64> = demuxed.tracks[0].samples.iter().map(|s| s.pts_us).collect();
        assert_eq!(pts.len(), 20);
        pts.sort_unstable();
        let expected: Vec<i64> = (0..20).map(|n| n * 100_000).collect();
        assert_eq!(pts, expected, "{warnings:?}");
    }
}
