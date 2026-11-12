//! Decodes one audio track to 16 kHz mono 16-bit WAV, the input most speech
//! recognisers expect.
//!
//! Called from `lib.rs::extract_with`. Decoders are moq-audio's (AAC through
//! symphonia, Opus through unsafe-libopus); resampling is rubato with the same
//! sinc settings moq-audio uses internally (its resampler is private). Audio is
//! streamed to disk packet by packet, so a long file never sits in memory.

use std::fs::File;
use std::io::{BufWriter, Seek, SeekFrom, Write};
use std::path::Path;
use std::time::Instant;

use hang::catalog::{AudioCodec, AudioConfig, AAC};
use moq_audio::decode::{Config as DecoderConfig, Decoder, Kind as DecoderKind};
use moq_audio::Layout;
use rubato::audioadapter_buffers::direct::SequentialSliceOfVecs;
use rubato::{
    Async, FixedAsync, Resampler as _, SincInterpolationParameters, SincInterpolationType,
    WindowFunction,
};

use crate::demux::{Codec, Track};
use crate::AudioReport;

pub(crate) const OUTPUT_RATE: u32 = 16_000;
const CHUNK: usize = 1024;

fn open(track: &Track) -> Result<Decoder, String> {
    let config = match &track.codec {
        Codec::Aac { asc } => {
            // The first five bits of the AudioSpecificConfig are the object type.
            let profile = asc.first().map(|b| b >> 3).unwrap_or(2);
            let mut c = AudioConfig::new(
                AAC { profile },
                track.sample_rate.unwrap_or(48_000),
                track.channels.unwrap_or(2),
            );
            c.description = Some(asc.clone());
            c
        }
        Codec::Opus { head } => {
            let mut c = AudioConfig::new(
                AudioCodec::Opus,
                48_000,
                track.channels.unwrap_or(2).clamp(1, 2),
            );
            c.description = head.clone();
            c
        }
        _ => return Err(format!("{} audio is not supported", track.codec_name)),
    };
    let mut options = DecoderConfig::new();
    options.kind = DecoderKind::Software;
    Decoder::new(&config, &options).map_err(|e| e.to_string())
}

/// Offline mono resampler. Drops the filter's start-up delay and drains its
/// tail at the end, so the output is as long as the input.
struct Mono {
    inner: Option<Async<f32>>,
    ratio: f64,
    pending: Vec<f32>,
    input: Vec<Vec<f32>>,
    output: Vec<Vec<f32>>,
    output_max: usize,
    skip: usize,
    frames_in: u64,
    frames_out: u64,
}

impl Mono {
    fn new(input_rate: u32) -> Result<Self, String> {
        let ratio = f64::from(OUTPUT_RATE) / f64::from(input_rate.max(1));
        if input_rate == OUTPUT_RATE {
            return Ok(Self {
                inner: None,
                ratio,
                pending: Vec::new(),
                input: Vec::new(),
                output: Vec::new(),
                output_max: 0,
                skip: 0,
                frames_in: 0,
                frames_out: 0,
            });
        }
        let params = SincInterpolationParameters {
            sinc_len: 128,
            f_cutoff: Some(0.95),
            interpolation: SincInterpolationType::Linear,
            oversampling_factor: 128,
            window: WindowFunction::BlackmanHarris2,
        };
        let inner = Async::<f32>::new_sinc(ratio, 1.0, &params, CHUNK, 1, FixedAsync::Input)
            .map_err(|e| e.to_string())?;
        let skip = inner.output_delay();
        let output_max = inner.output_frames_max();
        Ok(Self {
            inner: Some(inner),
            ratio,
            pending: Vec::new(),
            input: vec![vec![0.0; CHUNK]],
            output: vec![vec![0.0; output_max]],
            output_max,
            skip,
            frames_in: 0,
            frames_out: 0,
        })
    }

    fn push(&mut self, samples: &[f32], out: &mut Vec<f32>) -> Result<(), String> {
        self.frames_in += samples.len() as u64;
        if self.inner.is_none() {
            out.extend_from_slice(samples);
            self.frames_out += samples.len() as u64;
            return Ok(());
        }
        self.pending.extend_from_slice(samples);
        self.run(out)
    }

    fn run(&mut self, out: &mut Vec<f32>) -> Result<(), String> {
        let Some(inner) = self.inner.as_mut() else {
            return Ok(());
        };
        while self.pending.len() >= CHUNK {
            self.input[0].copy_from_slice(&self.pending[..CHUNK]);
            let input =
                SequentialSliceOfVecs::new(&self.input, 1, CHUNK).map_err(|e| e.to_string())?;
            let mut output = SequentialSliceOfVecs::new_mut(&mut self.output, 1, self.output_max)
                .map_err(|e| e.to_string())?;
            let (_, produced) = inner
                .process_into_buffer(&input, &mut output, None)
                .map_err(|e| e.to_string())?;
            self.pending.drain(..CHUNK);
            let produced = &self.output[0][..produced];
            let dropped = self.skip.min(produced.len());
            self.skip -= dropped;
            out.extend_from_slice(&produced[dropped..]);
            self.frames_out += (produced.len() - dropped) as u64;
        }
        Ok(())
    }

    fn finish(mut self, out: &mut Vec<f32>) -> Result<(), String> {
        if self.inner.is_none() {
            return Ok(());
        }
        let wanted = (self.frames_in as f64 * self.ratio).round() as u64;
        let mut rounds = 0;
        while self.frames_out < wanted && rounds < 64 {
            let fill = CHUNK - (self.pending.len() % CHUNK);
            self.pending.extend(std::iter::repeat_n(0.0, fill));
            self.run(out)?;
            rounds += 1;
        }
        if self.frames_out > wanted {
            let extra = (self.frames_out - wanted) as usize;
            out.truncate(out.len().saturating_sub(extra));
        }
        Ok(())
    }
}

struct Wav {
    file: BufWriter<File>,
    samples: u64,
}

impl Wav {
    fn create(path: &Path) -> std::io::Result<Self> {
        let mut file = BufWriter::new(File::create(path)?);
        file.write_all(&header(0))?;
        Ok(Self { file, samples: 0 })
    }

    fn write(&mut self, samples: &[f32]) -> std::io::Result<()> {
        for &s in samples {
            let v = (s.clamp(-1.0, 1.0) * 32767.0).round() as i16;
            self.file.write_all(&v.to_le_bytes())?;
        }
        self.samples += samples.len() as u64;
        Ok(())
    }

    fn finish(mut self) -> std::io::Result<u64> {
        let bytes = u32::try_from(self.samples * 2)
            .map_err(|_| std::io::Error::other("WAV larger than 4 GiB"))?;
        self.file.seek(SeekFrom::Start(0))?;
        self.file.write_all(&header(bytes))?;
        self.file.flush()?;
        Ok(self.samples)
    }
}

/// 44-byte RIFF header for 16-bit PCM, mono, 16 kHz.
fn header(data_bytes: u32) -> [u8; 44] {
    let mut h = [0u8; 44];
    h[0..4].copy_from_slice(b"RIFF");
    h[4..8].copy_from_slice(&(36 + data_bytes).to_le_bytes());
    h[8..12].copy_from_slice(b"WAVE");
    h[12..16].copy_from_slice(b"fmt ");
    h[16..20].copy_from_slice(&16u32.to_le_bytes());
    h[20..22].copy_from_slice(&1u16.to_le_bytes());
    h[22..24].copy_from_slice(&1u16.to_le_bytes());
    h[24..28].copy_from_slice(&OUTPUT_RATE.to_le_bytes());
    h[28..32].copy_from_slice(&(OUTPUT_RATE * 2).to_le_bytes());
    h[32..34].copy_from_slice(&2u16.to_le_bytes());
    h[34..36].copy_from_slice(&16u16.to_le_bytes());
    h[36..40].copy_from_slice(b"data");
    h[40..44].copy_from_slice(&data_bytes.to_le_bytes());
    h
}

/// Frames at `rate` that a packet starting at `pts_us` has before time 0:
/// none for a packet at or after it. Used by [`extract_audio`] to drop what an
/// MP4 edit list (see `mp4.rs`) cuts off the start, usually the AAC encoder's
/// 1024 priming frames.
fn frames_before_zero(pts_us: i64, rate: u32) -> usize {
    if pts_us >= 0 {
        return 0;
    }
    let frames = (i128::from(pts_us).abs() * i128::from(rate) + 500_000) / 1_000_000;
    usize::try_from(frames).unwrap_or(usize::MAX)
}

/// Write `us` microseconds of silence at [`OUTPUT_RATE`]. Used by
/// [`extract_audio`] for a track that starts late.
fn write_silence(wav: &mut Wav, us: i64) -> std::io::Result<()> {
    let mut left = usize::try_from(us.max(0) as i128 * i128::from(OUTPUT_RATE) / 1_000_000)
        .unwrap_or(usize::MAX);
    let zeros = [0.0f32; CHUNK];
    while left > 0 {
        let n = left.min(CHUNK);
        wav.write(&zeros[..n])?;
        left -= n;
    }
    Ok(())
}

/// Where one channel of a multichannel layout goes in a stereo downmix.
#[derive(Clone, Copy)]
enum Feed {
    /// Into the left output only, at this gain.
    Left(f32),
    /// Into the right output only, at this gain.
    Right(f32),
    /// Into both outputs, at this gain.
    Both(f32),
    /// Not used: the low-frequency effects channel.
    Dropped,
}

/// The weight each channel of an interleaved frame of `layout` has in the mono
/// downmix. Called by [`extract_audio`] once per track.
///
/// This is the stereo downmix of ITU-R BS.775 followed by the average of its two
/// outputs: Lo = L + 0.7071 C + 0.7071 Ls and Ro = R + 0.7071 C + 0.7071 Rs, with
/// the LFE dropped, and mono = (Lo + Ro) / 2. BS.775 has no rule for the rear
/// centre or for a front-of-centre pair (AAC's seventh configuration, which
/// `moq-audio` places in the side slots of 7.1), so they get the surround gain of
/// 0.7071 too: both outputs for the rear centre, their own side for the pair. The
/// weights are divided by the sum of the gains feeding one output, so a full-scale
/// signal on every channel cannot clip: 1 + 0.7071 + 0.7071 for 5.1.
///
/// Mono is passed through and stereo is (L + R) / 2, as an average of the channels
/// gives. A layout without a rule here, `Discrete` among them, is averaged the same way.
fn downmix_weights(layout: Layout) -> Vec<f32> {
    const MINUS_3_DB: f32 = std::f32::consts::FRAC_1_SQRT_2;
    use Feed::{Both, Dropped, Left, Right};

    let feeds = match layout {
        Layout::Mono => vec![Both(1.0)],
        Layout::Stereo => vec![Left(1.0), Right(1.0)],
        Layout::TwoPointOne => vec![Left(1.0), Right(1.0), Dropped],
        Layout::ThreePointZero => vec![Left(1.0), Right(1.0), Both(MINUS_3_DB)],
        Layout::Quad => vec![Left(1.0), Right(1.0), Left(MINUS_3_DB), Right(MINUS_3_DB)],
        Layout::FourPointZero => vec![Left(1.0), Right(1.0), Both(MINUS_3_DB), Both(MINUS_3_DB)],
        Layout::FivePointZero => {
            vec![
                Left(1.0),
                Right(1.0),
                Both(MINUS_3_DB),
                Left(MINUS_3_DB),
                Right(MINUS_3_DB),
            ]
        }
        Layout::FivePointOne => {
            vec![
                Left(1.0),
                Right(1.0),
                Both(MINUS_3_DB),
                Dropped,
                Left(MINUS_3_DB),
                Right(MINUS_3_DB),
            ]
        }
        Layout::SixPointOne => vec![
            Left(1.0),
            Right(1.0),
            Both(MINUS_3_DB),
            Dropped,
            Both(MINUS_3_DB),
            Left(MINUS_3_DB),
            Right(MINUS_3_DB),
        ],
        Layout::SevenPointOne => vec![
            Left(1.0),
            Right(1.0),
            Both(MINUS_3_DB),
            Dropped,
            Left(MINUS_3_DB),
            Right(MINUS_3_DB),
            Left(MINUS_3_DB),
            Right(MINUS_3_DB),
        ],
        other => vec![Both(1.0); other.channels().max(1) as usize],
    };

    // Every layout above is symmetric, so one output's total stands for both.
    let norm: f32 = feeds
        .iter()
        .map(|feed| match *feed {
            Left(gain) | Both(gain) => gain,
            Right(_) | Dropped => 0.0,
        })
        .sum();
    feeds
        .iter()
        .map(|feed| match *feed {
            Left(gain) | Right(gain) => gain / (2.0 * norm),
            Both(gain) => gain / norm,
            Dropped => 0.0,
        })
        .collect()
}

pub(crate) fn extract_audio(
    track: &Track,
    out_dir: &Path,
    max_duration_us: i64,
    deadline: Option<Instant>,
    warnings: &mut Vec<String>,
) -> Option<AudioReport> {
    let mut decoder = match open(track) {
        Ok(d) => d,
        Err(e) => {
            warnings.push(format!(
                "audio: the {} track is skipped and no audio is written: {e}",
                track.codec_name
            ));
            return None;
        }
    };
    let weights = downmix_weights(decoder.layout());
    let mut resampler = match Mono::new(decoder.sample_rate()) {
        Ok(r) => r,
        Err(e) => {
            warnings.push(format!("audio: {e}"));
            return None;
        }
    };
    let path = out_dir.join("audio-16k-mono.wav");
    let mut wav = match Wav::create(&path) {
        Ok(w) => w,
        Err(e) => {
            warnings.push(format!("audio: could not create {}: {e}", path.display()));
            return None;
        }
    };

    // A track whose edit list starts it after 0 is preceded by silence, so the
    // WAV's time 0 is the file's, as the frames' times are.
    if let Err(e) = write_silence(&mut wav, track.start_us.clamp(0, max_duration_us.max(0))) {
        warnings.push(format!("audio: {e}"));
        return None;
    }

    let mut errors = 0usize;
    let mut mono = Vec::new();
    let mut out = Vec::new();
    let mut cut = false;
    for sample in &track.samples {
        if sample.pts_us > max_duration_us {
            cut = true;
            break;
        }
        if deadline.is_some_and(|d| Instant::now() >= d) {
            warnings.push("time limit reached; audio stops early".into());
            break;
        }
        let decoded = match decoder.decode(&sample.data) {
            Ok(d) => d,
            Err(_) => {
                errors += 1;
                continue;
            }
        };
        mono.clear();
        mono.extend(decoded.samples.chunks_exact(weights.len()).map(|frame| {
            frame
                .iter()
                .zip(&weights)
                .map(|(sample, weight)| sample * weight)
                .sum::<f32>()
        }));
        // Audio an edit list puts before 0 (encoder priming) is not played.
        mono.drain(..frames_before_zero(sample.pts_us, decoder.sample_rate()).min(mono.len()));
        out.clear();
        if let Err(e) = resampler.push(&mono, &mut out) {
            warnings.push(format!("audio: {e}"));
            return None;
        }
        if let Err(e) = wav.write(&out) {
            warnings.push(format!("audio: {e}"));
            return None;
        }
    }
    out.clear();
    if let Err(e) = resampler.finish(&mut out) {
        warnings.push(format!("audio: {e}"));
    }
    if let Err(e) = wav.write(&out) {
        warnings.push(format!("audio: {e}"));
        return None;
    }
    if cut {
        warnings.push(format!(
            "audio cut at {:.0} s (duration limit)",
            max_duration_us as f64 / 1e6
        ));
    }
    if errors > 0 {
        warnings.push(format!("{errors} audio packets could not be decoded"));
    }
    match wav.finish() {
        Ok(samples) => Some(AudioReport {
            format: "wav",
            sample_rate: OUTPUT_RATE,
            channels: 1,
            duration_secs: samples as f64 / f64::from(OUTPUT_RATE),
            path: path.to_string_lossy().into_owned(),
        }),
        Err(e) => {
            warnings.push(format!("audio: {e}"));
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;
    use mp4_atom::{Edts, Elst, ElstEntry};

    use super::*;
    use crate::demux::Kind;
    use crate::mp4::{self, tests::rewrite_moov};

    const H264_AAC: &[u8] = include_bytes!("../tests/fixtures/h264_aac.mp4");

    /// The 16 kHz samples `extract_audio` writes for the audio track of `data`.
    fn decode(data: &Bytes, tag: &str) -> Vec<i16> {
        let demuxed = mp4::read(data, &mut Vec::new()).unwrap();
        let track = demuxed
            .tracks
            .iter()
            .find(|t| t.kind == Kind::Audio)
            .unwrap();
        let dir =
            std::env::temp_dir().join(format!("inbrowse-audio-test-{}-{tag}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut warnings = Vec::new();
        let report = extract_audio(track, &dir, i64::MAX, None, &mut warnings).expect("audio");
        assert!(warnings.is_empty(), "{warnings:?}");
        let wav = std::fs::read(&report.path).unwrap();
        let _ = std::fs::remove_dir_all(dir);
        wav[44..]
            .chunks_exact(2)
            .map(|s| i16::from_le_bytes([s[0], s[1]]))
            .collect()
    }

    #[test]
    fn frames_before_zero_rounds_to_whole_frames() {
        assert_eq!(frames_before_zero(-21_333, 48_000), 1024);
        assert_eq!(frames_before_zero(0, 48_000), 0);
        assert_eq!(frames_before_zero(5, 48_000), 0);
        // The most negative time does not overflow.
        assert!(frames_before_zero(i64::MIN, u32::MAX) > 0);
    }

    /// The fixture's audio edit list skips AAC's 1024 priming frames: 95
    /// packets of 1024 frames at 48 kHz, less those, at 16 kHz.
    #[test]
    fn priming_frames_named_by_the_edit_list_are_not_written() {
        let samples = decode(&Bytes::from_static(H264_AAC), "priming");
        let expected = (95 * 1024 - 1024) / 3;
        assert!(
            samples.len().abs_diff(expected) <= 8,
            "{} samples, expected {expected}",
            samples.len()
        );
    }

    /// A leading empty edit puts that much silence before the audio so the
    /// WAV and the frames share a time axis.
    #[test]
    fn leading_empty_edit_becomes_silence() {
        let data = rewrite_moov(H264_AAC, |moov| {
            let entry = |segment_duration, media_time| ElstEntry {
                segment_duration,
                media_time,
                media_rate: 1.into(),
            };
            moov.trak[1].edts = Some(Edts {
                elst: Some(Elst {
                    entries: vec![entry(500, None), entry(2000, Some(1024))],
                }),
            });
        });
        let samples = decode(&data, "silence");
        let expected = 8_000 + (95 * 1024 - 1024) / 3;
        assert!(
            samples.len().abs_diff(expected) <= 8,
            "{} samples, expected {expected}",
            samples.len()
        );
        assert!(samples[..8_000].iter().all(|&s| s == 0));
        let peak = samples[8_000..]
            .iter()
            .map(|s| s.unsigned_abs())
            .max()
            .unwrap();
        assert!(peak > 3000, "peak {peak}");
    }

    /// An AAC track the decoder refuses gives no audio and a warning that says
    /// so and why: HE-AAC (SBR is not decoded) and a channel layout carried in a
    /// program config element (channel configuration 0) are refused, though
    /// the channel count alone no longer is.
    #[test]
    fn refused_aac_track_is_reported_as_skipped() {
        let refused: [(&str, &[u8], &str); 2] = [
            // HE-AAC (object type 5), 48 kHz, stereo.
            ("he-aac", &[0x29, 0x90], "AAC-LC"),
            // AAC-LC, 48 kHz, channel configuration 0, then a program config
            // element naming a centre and a pair of front channels.
            (
                "pce",
                &[0x11, 0x80, 0x04, 0xC8, 0x00, 0x00, 0x01, 0x10, 0x00],
                "program config",
            ),
        ];
        for (tag, asc, reason) in refused {
            let mut track = Track::new(
                Kind::Audio,
                Codec::Aac {
                    asc: Bytes::from_static(asc),
                },
                "aac",
            );
            track.sample_rate = Some(48_000);
            track.channels = Some(3);
            let dir = std::env::temp_dir()
                .join(format!("inbrowse-audio-test-{}-{tag}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            let mut warnings = Vec::new();
            assert!(
                extract_audio(&track, &dir, i64::MAX, None, &mut warnings).is_none(),
                "{tag}"
            );
            let _ = std::fs::remove_dir_all(dir);
            assert_eq!(warnings.len(), 1, "{tag}: {warnings:?}");
            assert!(
                warnings[0].contains("aac track is skipped")
                    && warnings[0].contains("no audio is written"),
                "{tag}: {warnings:?}"
            );
            assert!(
                warnings[0].contains(reason),
                "{tag}: the decoder's reason is kept: {warnings:?}"
            );
        }
    }

    /// Mono and stereo mix down exactly as the plain average of the channels.
    #[test]
    fn mono_and_stereo_downmix_is_the_average() {
        assert_eq!(downmix_weights(Layout::Mono), [1.0]);
        assert_eq!(downmix_weights(Layout::Stereo), [0.5, 0.5]);
    }

    /// Every layout with a rule sums to 1 over the channels that are not LFE
    /// when all are at full scale, so a loud mix cannot clip; the LFE has no weight.
    #[test]
    fn surround_downmix_cannot_clip_and_drops_the_lfe() {
        let layouts = [
            Layout::TwoPointOne,
            Layout::ThreePointZero,
            Layout::Quad,
            Layout::FourPointZero,
            Layout::FivePointZero,
            Layout::FivePointOne,
            Layout::SixPointOne,
            Layout::SevenPointOne,
        ];
        for layout in layouts {
            let weights = downmix_weights(layout);
            assert_eq!(weights.len(), layout.channels() as usize, "{layout:?}");
            assert!(
                (weights.iter().sum::<f32>() - 1.0).abs() < 1e-6,
                "{layout:?}: {weights:?}"
            );
        }
        let five_one = downmix_weights(Layout::FivePointOne);
        let norm = 1.0 + 2.0 * std::f32::consts::FRAC_1_SQRT_2;
        // L, R, C, LFE, Ls, Rs.
        let expected = [
            0.5 / norm,
            0.5 / norm,
            std::f32::consts::FRAC_1_SQRT_2 / norm,
            0.0,
        ];
        for (got, want) in five_one.iter().zip(expected) {
            assert!((got - want).abs() < 1e-6, "{five_one:?}");
        }
        assert_eq!(five_one[3], 0.0);
        assert!(
            (five_one[4] - five_one[2] / 2.0).abs() < 1e-6,
            "{five_one:?}"
        );
        // Without a rule, channels are averaged.
        assert_eq!(downmix_weights(Layout::Discrete(4)), [0.25; 4]);
    }
}
