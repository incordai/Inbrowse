//! AAC-LC through symphonia, the pure-Rust fallback for hosts without a
//! platform AAC decoder.
//!
//! Modified for Inbrowse: the layout comes from the channel configuration the decoder
//! resolved rather than from the channel count alone, because AAC configurations 3, 4
//! and 8 channels are not the WAVE defaults `Layout::from_channels` assumes.

use symphonia_core::codecs::audio::AudioDecoder;

use super::Backend;
use crate::decode::Decoded;
use crate::{Activity, Error, Layout, aac};

pub(super) const NAME: &str = "symphonia";

pub(super) struct Symphonia {
	inner: symphonia_codec_aac::AacDecoder,
	sample_rate: u32,
	layout: Layout,
}

impl Symphonia {
	/// AAC-LC in channel configurations 1 to 7 (mono up to 7.1), which covers
	/// what gateways publish and what files carry.
	///
	/// HE-AAC is rejected however its config spells it: leading with SBR or PS
	/// (mp4a.40.5 / .29), or leading with LC and declaring SBR in a sync extension
	/// after the core. Symphonia decodes no SBR either way, so the alternative is
	/// half-rate audio that sounds like a fault rather than an unsupported codec.
	/// A stream that signals SBR only in band is indistinguishable from LC in the
	/// config, and does decode as the core.
	pub(super) fn open(catalog: &hang::catalog::AudioConfig) -> Result<Box<dyn Backend>, Error> {
		use symphonia_core::codecs::audio::well_known::CODEC_ID_AAC;
		use symphonia_core::codecs::audio::{AudioCodecParameters, AudioDecoderOptions};

		let hang::catalog::AudioCodec::AAC(codec) = &catalog.codec else {
			return Err(Error::Unsupported(format!("symphonia cannot decode {}", catalog.codec)));
		};
		let description = aac::description(catalog, codec.profile)?;

		let mut params = AudioCodecParameters::new();
		params
			.for_codec(CODEC_ID_AAC)
			.with_extra_data(description.to_vec().into_boxed_slice());

		let inner = symphonia_codec_aac::AacDecoder::try_new(&params, &AudioDecoderOptions::default())
			.map_err(|err| Error::Unsupported(format!("aac decoder: {err}")))?;

		// Resolved by the decoder from the config, so this is what it will emit
		// even when the catalog's own fields say otherwise.
		let params = inner.codec_params();
		let sample_rate = params
			.sample_rate
			.ok_or_else(|| Error::Unsupported("aac config declares no sample rate".into()))?;
		let channel_count = params
			.channels
			.as_ref()
			.map(|channels| channels.count())
			.ok_or_else(|| Error::Unsupported("aac config declares no channels".into()))?;

		Ok(Box::new(Self {
			inner,
			sample_rate,
			layout: layout(channel_count)?,
		}))
	}
}

/// The layout of the interleaved output of AAC-LC with `channel_count` channels,
/// called by [`Symphonia::open`].
///
/// The decoder writes its planes in the order of the speaker bits, which is the WAVE
/// order [`Layout`] interleaves in, so only the speakers need naming: configurations 3
/// and 4 are 3.0 and 4.0 (centre, then rear centre), not the 2.1 and quad the count
/// alone suggests. Configuration 7 has 8 channels, its two front-of-centre channels
/// taking the side slots of 7.1, which has no wide-front position.
fn layout(channel_count: usize) -> Result<Layout, Error> {
	match channel_count {
		1 => Ok(Layout::Mono),
		2 => Ok(Layout::Stereo),
		3 => Ok(Layout::ThreePointZero),
		4 => Ok(Layout::FourPointZero),
		5 => Ok(Layout::FivePointZero),
		6 => Ok(Layout::FivePointOne),
		8 => Ok(Layout::SevenPointOne),
		count => Err(Error::Unsupported(format!(
			"aac has no channel configuration with {count} channels"
		))),
	}
}

impl Backend for Symphonia {
	fn decode(&mut self, packet: &[u8]) -> Result<Decoded, Error> {
		// The packet is a raw AAC frame, not ADTS, so there is nothing to timestamp
		// it with here: the container carries the timestamp and the decoder only
		// reads the payload.
		let packet = symphonia_core::packet::PacketRef::new(
			0,
			symphonia_core::units::Timestamp::ZERO,
			symphonia_core::units::Duration::ZERO,
			packet,
		);

		let decoded = self
			.inner
			.decode_ref(&packet)
			.map_err(|err| Error::Decode(format!("aac: {err}")))?;

		let mut samples = Vec::new();
		decoded.copy_to_vec_interleaved(&mut samples);
		Ok(Decoded {
			samples,
			activity: Activity::Active,
		})
	}

	fn reset(&mut self) -> Result<(), Error> {
		self.inner.reset();
		Ok(())
	}

	fn sample_rate(&self) -> u32 {
		self.sample_rate
	}

	fn layout(&self) -> Layout {
		self.layout
	}

	fn name(&self) -> &str {
		NAME
	}
}
