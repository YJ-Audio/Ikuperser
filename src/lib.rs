use std::sync::Arc;

use nih_plug::prelude::*;

pub mod dsp;

use dsp::{AllpassCoefs, Disperser, MAX_STAGES};

pub struct Ikupeser {
	params: Arc<IkupeserParams>,
	sample_rate: f32,
	/// One cascade per main output channel, allocated in `initialize`.
	channels: Vec<Disperser>,
	coefs: AllpassCoefs,
	/// Frequency and Q that `coefs` was computed from. Coefficients are recomputed only when the
	/// smoothed values differ from these, which avoids a `tan` call per sample outside of
	/// parameter transitions.
	coefs_frequency: f32,
	coefs_q: f32,
}

#[derive(Params)]
pub struct IkupeserParams {
	/// Number of cascaded allpass stages.
	#[id = "amount"]
	pub amount: FloatParam,

	/// Center frequency of every allpass stage.
	#[id = "frequency"]
	pub frequency: FloatParam,

	/// Quality factor of every allpass stage. Higher values concentrate the delay in a narrower
	/// band around `frequency`.
	#[id = "pinch"]
	pub pinch: FloatParam,

	#[id = "mix"]
	pub mix: FloatParam,
}

impl Default for IkupeserParams {
	fn default() -> Self {
		Self {
			amount: FloatParam::new(
				"Amount",
				16.0,
				FloatRange::Skewed {
					min: 0.0,
					max: MAX_STAGES as f32,
					factor: FloatRange::skew_factor(-1.0),
				},
			)
			.with_step_size(1.0)
			// Linear smoothing passes through fractional stage counts, which `Disperser::process`
			// crossfades between adjacent stages.
			.with_smoother(SmoothingStyle::Linear(50.0))
			.with_value_to_string(Arc::new(|value| format!("{value:.0}"))),

			frequency: FloatParam::new(
				"Frequency",
				1000.0,
				FloatRange::Skewed {
					min: 20.0,
					max: 20_000.0,
					factor: FloatRange::skew_factor(-2.0),
				},
			)
			.with_smoother(SmoothingStyle::Logarithmic(50.0))
			.with_value_to_string(formatters::v2s_f32_hz_then_khz(0))
			.with_string_to_value(formatters::s2v_f32_hz_then_khz()),

			pinch: FloatParam::new(
				"Pinch",
				1.0,
				FloatRange::Skewed {
					min: 0.1,
					max: 20.0,
					factor: FloatRange::skew_factor(-2.0),
				},
			)
			.with_smoother(SmoothingStyle::Logarithmic(50.0))
			.with_value_to_string(formatters::v2s_f32_rounded(2)),

			mix: FloatParam::new("Mix", 1.0, FloatRange::Linear { min: 0.0, max: 1.0 })
				.with_smoother(SmoothingStyle::Linear(20.0))
				.with_unit("%")
				.with_value_to_string(formatters::v2s_f32_percentage(0))
				.with_string_to_value(formatters::s2v_f32_percentage()),
		}
	}
}

impl Default for Ikupeser {
	fn default() -> Self {
		Self {
			params: Arc::new(IkupeserParams::default()),
			sample_rate: 44_100.0,
			channels: Vec::new(),
			coefs: AllpassCoefs::new(1000.0, 1.0, 44_100.0),
			coefs_frequency: f32::NAN,
			coefs_q: f32::NAN,
		}
	}
}

impl Plugin for Ikupeser {
	const NAME: &'static str = "Ikupeser";
	const VENDOR: &'static str = "potistudio";
	const URL: &'static str = env!("CARGO_PKG_HOMEPAGE");
	const EMAIL: &'static str = "";

	const VERSION: &'static str = env!("CARGO_PKG_VERSION");

	const AUDIO_IO_LAYOUTS: &'static [AudioIOLayout] = &[
		AudioIOLayout {
			main_input_channels: NonZeroU32::new(2),
			main_output_channels: NonZeroU32::new(2),
			..AudioIOLayout::const_default()
		},
		AudioIOLayout {
			main_input_channels: NonZeroU32::new(1),
			main_output_channels: NonZeroU32::new(1),
			..AudioIOLayout::const_default()
		},
	];

	const SAMPLE_ACCURATE_AUTOMATION: bool = true;

	type SysExMessage = ();
	type BackgroundTask = ();

	fn params(&self) -> Arc<dyn Params> {
		self.params.clone()
	}

	fn initialize(
		&mut self,
		audio_io_layout: &AudioIOLayout,
		buffer_config: &BufferConfig,
		_context: &mut impl InitContext<Self>,
	) -> bool {
		self.sample_rate = buffer_config.sample_rate;

		let num_channels = audio_io_layout.main_output_channels.map_or(0, NonZeroU32::get) as usize;
		self.channels.resize_with(num_channels, Disperser::default);

		true
	}

	fn reset(&mut self) {
		self.channels.iter_mut().for_each(Disperser::reset);
		// NaN never compares equal, so the first processed sample recomputes the coefficients for
		// the current sample rate.
		self.coefs_frequency = f32::NAN;
		self.coefs_q = f32::NAN;
	}

	fn process(
		&mut self,
		buffer: &mut Buffer,
		_aux: &mut AuxiliaryBuffers,
		_context: &mut impl ProcessContext<Self>,
	) -> ProcessStatus {
		for mut channel_samples in buffer.iter_samples() {
			let amount = self.params.amount.smoothed.next();
			let frequency = self.params.frequency.smoothed.next();
			let q = self.params.pinch.smoothed.next();
			let mix = self.params.mix.smoothed.next();

			if frequency != self.coefs_frequency || q != self.coefs_q {
				self.coefs = AllpassCoefs::new(frequency, q, self.sample_rate);
				self.coefs_frequency = frequency;
				self.coefs_q = q;
			}

			for (sample, disperser) in channel_samples.iter_mut().zip(&mut self.channels) {
				let dry = *sample;
				let wet = disperser.process(&self.coefs, amount, dry);
				*sample = dry + mix * (wet - dry);
			}
		}

		ProcessStatus::Normal
	}
}

impl ClapPlugin for Ikupeser {
	const CLAP_ID: &'static str = "com.potistudio.ikupeser";
	const CLAP_DESCRIPTION: Option<&'static str> = Some(env!("CARGO_PKG_DESCRIPTION"));
	const CLAP_MANUAL_URL: Option<&'static str> = None;
	const CLAP_SUPPORT_URL: Option<&'static str> = None;
	const CLAP_FEATURES: &'static [ClapFeature] = &[
		ClapFeature::AudioEffect,
		ClapFeature::Stereo,
		ClapFeature::Mono,
		ClapFeature::Filter,
	];
}

impl Vst3Plugin for Ikupeser {
	const VST3_CLASS_ID: [u8; 16] = *b"IkupeserDispersr";
	const VST3_SUBCATEGORIES: &'static [Vst3SubCategory] = &[Vst3SubCategory::Fx, Vst3SubCategory::Filter];
}

nih_export_clap!(Ikupeser);
nih_export_vst3!(Ikupeser);
