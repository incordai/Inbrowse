//! Fixtures (tests/fixtures/README.md) are two seconds long: one second of red,
//! then one second of blue, over a 440 Hz tone. The cut at 1.0 s is the scene
//! change every video test expects to find.

use std::path::{Path, PathBuf};

use bytes::Bytes;
use inbrowse_media::{extract, FrameReason, MediaError, Options, Report};

fn fixture(name: &str) -> Bytes {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name);
    Bytes::from(std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display())))
}

fn out_dir(test: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("inbrowse-media-test-{}-{test}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn run(name: &str, test: &str) -> (Report, PathBuf) {
    let dir = out_dir(test);
    let report = extract(fixture(name), Some(name), &dir, &Options::default()).expect(name);
    (report, dir)
}

/// Centre pixel of a written PNG.
fn centre(path: &str) -> [u8; 4] {
    let img = image::open(path).expect("png").to_rgba8();
    img.get_pixel(img.width() / 2, img.height() / 2).0
}

fn assert_red_then_blue(report: &Report) {
    assert!(report.frames.len() >= 2, "frames: {:#?}", report.frames);
    let first = &report.frames[0];
    assert_eq!(first.reason, FrameReason::Interval);
    assert!(first.time_secs.unwrap() < 0.3, "{first:?}");
    let [r, _, b, _] = centre(&first.path);
    assert!(r > 150 && b < 100, "first frame should be red, got r={r} b={b}");

    let cut = report
        .frames
        .iter()
        .find(|f| f.reason == FrameReason::SceneChange)
        .expect("a scene change frame");
    let t = cut.time_secs.unwrap();
    assert!((0.9..=1.2).contains(&t), "scene change at {t}");
    let [r, _, b, _] = centre(&cut.path);
    assert!(b > 150 && r < 100, "cut frame should be blue, got r={r} b={b}");
    assert_eq!((first.width, first.height), (128, 96));
}

fn assert_wav(report: &Report) {
    let audio = report.audio.as_ref().expect("audio");
    assert_eq!((audio.sample_rate, audio.channels), (16_000, 1));
    assert!((audio.duration_secs - 2.0).abs() < 0.1, "audio {} s", audio.duration_secs);
    let wav = std::fs::read(&audio.path).unwrap();
    assert_eq!(&wav[0..4], b"RIFF");
    assert_eq!(u32::from_le_bytes(wav[24..28].try_into().unwrap()), 16_000);
    assert_eq!(wav.len() as u64, 44 + (audio.duration_secs * 16_000.0).round() as u64 * 2);
    // A 440 Hz tone, not silence.
    let peak = wav[44..]
        .chunks_exact(2)
        .map(|s| i16::from_le_bytes([s[0], s[1]]).unsigned_abs())
        .max()
        .unwrap();
    assert!(peak > 3000, "peak {peak}");
}

#[test]
fn h264_aac_progressive_mp4() {
    let (report, dir) = run("h264_aac.mp4", "progressive");
    assert_eq!(report.container, "mp4");
    let kinds: Vec<_> = report.tracks.iter().map(|t| (t.kind, t.codec.as_str(), t.supported)).collect();
    assert_eq!(kinds, [("video", "h264", true), ("audio", "aac", true)]);
    assert!((report.duration_secs.unwrap() - 2.0).abs() < 0.1);
    assert_red_then_blue(&report);
    assert_wav(&report);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn h264_bframes_fragmented_mp4() {
    let (report, dir) = run("h264_aac_fragmented.mp4", "fragmented");
    assert_red_then_blue(&report);
    assert_wav(&report);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn aac_only_m4a() {
    let (report, dir) = run("aac_only.m4a", "m4a");
    assert!(report.frames.is_empty());
    assert_wav(&report);
    let _ = std::fs::remove_dir_all(dir);
}

/// The fixture plays a 0.5 amplitude tone on one channel of a 5.1 AAC-LC stream
/// at a time, 0.25 s each, in the order L, R, C, LFE, Ls, Rs. In the mono
/// downmix of ITU-R BS.775 (L = R = 1, C and the surrounds 0.7071, LFE dropped,
/// divided by 1 + 0.7071 + 0.7071) the tone must come out at the level of its
/// channel: it shows each element reached its own channel.
#[test]
fn aac_five_point_one_downmixes_each_channel_at_its_level() {
    let (report, dir) = run("aac_5p1_tones.m4a", "five-point-one");
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let audio = report.audio.as_ref().expect("audio");
    assert_eq!((audio.format, audio.sample_rate, audio.channels), ("wav", 16_000, 1));
    assert!((audio.duration_secs - 1.5).abs() < 0.1, "audio {} s", audio.duration_secs);
    let wav = std::fs::read(&audio.path).unwrap();
    assert_eq!(&wav[0..4], b"RIFF");
    assert_eq!(u16::from_le_bytes(wav[22..24].try_into().unwrap()), 1, "mono");
    assert_eq!(u32::from_le_bytes(wav[24..28].try_into().unwrap()), 16_000);
    let samples: Vec<f64> = wav[44..]
        .chunks_exact(2)
        .map(|s| f64::from(i16::from_le_bytes([s[0], s[1]])))
        .collect();

    // RMS over the middle of each 4000-sample segment, clear of the cuts.
    let rms = |segment: usize| {
        let window = &samples[segment * 4000 + 1000..segment * 4000 + 3000];
        (window.iter().map(|s| s * s).sum::<f64>() / window.len() as f64).sqrt()
    };
    let norm = 1.0 + 2.0 * std::f64::consts::FRAC_1_SQRT_2;
    let side = std::f64::consts::FRAC_1_SQRT_2;
    // Peak of the tone is 0.5 of full scale; the RMS of a sine is its peak / sqrt 2.
    let level = |weight: f64| 0.5 * 32_767.0 * weight * std::f64::consts::FRAC_1_SQRT_2;
    let expected = [
        ("left", level(0.5 / norm)),
        ("right", level(0.5 / norm)),
        ("centre", level(side / norm)),
        ("lfe", 0.0),
        ("left surround", level(side * 0.5 / norm)),
        ("right surround", level(side * 0.5 / norm)),
    ];
    for (segment, (name, want)) in expected.into_iter().enumerate() {
        let got = rms(segment);
        assert!((got - want).abs() <= 0.1 * level(side / norm), "{name}: rms {got:.0}, expected {want:.0}");
    }
    assert!(rms(2) > 1000.0, "the centre is not silent");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn webm_opus_audio_and_vpx_video() {
    for (name, codec) in [("vp8_opus.webm", "vp8"), ("vp9_opus.webm", "vp9")] {
        let (report, dir) = run(name, codec);
        assert_eq!(report.container, "webm");
        assert_eq!(report.tracks[0].codec, codec);
        assert_wav(&report);
        if cfg!(feature = "vpx") {
            assert!(report.tracks[0].supported);
            assert_red_then_blue(&report);
        } else {
            assert!(!report.tracks[0].supported);
            assert!(report.frames.is_empty());
            assert!(report.warnings.iter().any(|w| w.contains("VP8/VP9")), "{:?}", report.warnings);
        }
        let _ = std::fs::remove_dir_all(dir);
    }
}

#[test]
fn frame_budget_and_no_audio() {
    let dir = out_dir("budget");
    let options = Options {
        max_frames: 1,
        audio: false,
        max_side: 64,
        ..Options::default()
    };
    let report = extract(fixture("h264_aac.mp4"), None, &dir, &options).unwrap();
    assert_eq!(report.frames.len(), 1);
    assert_eq!((report.frames[0].width, report.frames[0].height), (64, 48));
    assert!(report.audio.is_none());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn webvtt_file() {
    let dir = out_dir("vtt");
    let vtt = Bytes::from_static(b"WEBVTT\n\n00:00.500 --> 00:01.000\nhello\n");
    let report = extract(vtt, Some("a.vtt"), &dir, &Options::default()).unwrap();
    assert_eq!(report.container, "webvtt");
    assert_eq!(report.captions[0].cues[0].text, "hello");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn refuses_unknown_and_oversized_input() {
    let dir = out_dir("refuse");
    let err = extract(Bytes::from_static(b"just some text"), Some("x.bin"), &dir, &Options::default()).unwrap_err();
    assert!(matches!(err, MediaError::Unsupported(_)), "{err}");
    let options = Options {
        max_input_bytes: 100,
        ..Options::default()
    };
    let err = extract(fixture("h264_aac.mp4"), None, &dir, &options).unwrap_err();
    assert!(matches!(err, MediaError::TooLarge(_)), "{err}");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn damaged_input_never_panics() {
    let dir = out_dir("damaged");
    for name in ["h264_aac.mp4", "h264_aac_fragmented.mp4", "vp9_opus.webm", "aac_only.m4a"] {
        let data = fixture(name);
        for cut in [16, 200, data.len() / 3, data.len() / 2, data.len() - 7] {
            // Truncated, then with bytes flipped in the middle.
            let _ = extract(data.slice(..cut), None, &dir, &Options::default());
            let mut flipped = data.to_vec();
            for b in flipped.iter_mut().skip(cut).step_by(97).take(40) {
                *b ^= 0x5A;
            }
            let _ = extract(Bytes::from(flipped), None, &dir, &Options::default());
        }
    }
    let _ = std::fs::remove_dir_all(dir);
}
