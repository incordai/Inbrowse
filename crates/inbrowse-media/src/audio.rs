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
use rubato::audioadapter_buffers::direct::SequentialSliceOfVecs;
use rubato::{Async, FixedAsync, Resampler as _, SincInterpolationParameters, SincInterpolationType, WindowFunction};

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
            let mut c = AudioConfig::new(AudioCodec::Opus, 48_000, track.channels.unwrap_or(2).clamp(1, 2));
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
        let inner = Async::<f32>::new_sinc(ratio, 1.0, &params, CHUNK, 1, FixedAsync::Input).map_err(|e| e.to_string())?;
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
        let Some(inner) = self.inner.as_mut() else { return Ok(()) };
        while self.pending.len() >= CHUNK {
            self.input[0].copy_from_slice(&self.pending[..CHUNK]);
            let input = SequentialSliceOfVecs::new(&self.input, 1, CHUNK).map_err(|e| e.to_string())?;
            let mut output =
                SequentialSliceOfVecs::new_mut(&mut self.output, 1, self.output_max).map_err(|e| e.to_string())?;
            let (_, produced) = inner.process_into_buffer(&input, &mut output, None).map_err(|e| e.to_string())?;
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
        let bytes = u32::try_from(self.samples * 2).map_err(|_| std::io::Error::other("WAV larger than 4 GiB"))?;
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
            warnings.push(format!("audio: {e}"));
            return None;
        }
    };
    let channels = (decoder.layout().channels() as usize).max(1);
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
        mono.extend(
            decoded
                .samples
                .chunks_exact(channels)
                .map(|frame| frame.iter().sum::<f32>() / channels as f32),
        );
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
        warnings.push(format!("audio cut at {:.0} s (duration limit)", max_duration_us as f64 / 1e6));
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
