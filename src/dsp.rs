use std::f32::consts::PI;

/// Upper bound of the `Amount` parameter. Stage state is preallocated to this size so that the
/// audio thread does not allocate.
pub const MAX_STAGES: usize = 200;

/// Second-order allpass coefficients for the trapezoidal state variable filter topology
/// described by Andrew Simper (Cytomic). This topology is used instead of a direct form biquad
/// because the coefficients are modulated per sample while parameters are smoothed.
#[derive(Debug, Clone, Copy)]
pub struct AllpassCoefs {
	a1: f32,
	a2: f32,
	a3: f32,
	/// Band-pass output weight. The allpass response is `x - 2k * bandpass`.
	k2: f32,
}

impl AllpassCoefs {
	pub fn new(frequency: f32, q: f32, sample_rate: f32) -> Self {
		// `tan` diverges at Nyquist, so the cutoff is kept strictly below it.
		let frequency = frequency.clamp(1.0, sample_rate * 0.49);
		let g = (PI * frequency / sample_rate).tan();
		let k = q.max(1.0e-3).recip();
		let a1 = (1.0 + g * (g + k)).recip();
		let a2 = g * a1;
		let a3 = g * a2;

		Self {
			a1,
			a2,
			a3,
			k2: 2.0 * k,
		}
	}
}

#[derive(Debug, Clone, Copy, Default)]
struct AllpassStage {
	ic1eq: f32,
	ic2eq: f32,
}

impl AllpassStage {
	#[inline(always)]
	fn process(&mut self, c: &AllpassCoefs, v0: f32) -> f32 {
		let v3 = v0 - self.ic2eq;
		let v1 = c.a1 * self.ic1eq + c.a2 * v3;
		let v2 = self.ic2eq + c.a2 * self.ic1eq + c.a3 * v3;
		self.ic1eq = 2.0 * v1 - self.ic1eq;
		self.ic2eq = 2.0 * v2 - self.ic2eq;
		v0 - c.k2 * v1
	}

	fn reset(&mut self) {
		*self = Self::default();
	}
}

/// A cascade of identical allpass stages for a single channel.
pub struct Disperser {
	stages: Box<[AllpassStage; MAX_STAGES]>,
	/// Number of stages that were processed for the previous sample. Stages at or above this index
	/// hold stale state and are cleared before they are processed again.
	active_stages: usize,
}

impl Default for Disperser {
	fn default() -> Self {
		Self {
			stages: Box::new([AllpassStage::default(); MAX_STAGES]),
			active_stages: 0,
		}
	}
}

impl Disperser {
	pub fn reset(&mut self) {
		self.stages.iter_mut().for_each(AllpassStage::reset);
		self.active_stages = 0;
	}

	/// Processes one sample through `amount` stages.
	///
	/// A fractional `amount` linearly interpolates between the outputs of stage `floor(amount)` and
	/// stage `ceil(amount)`. This keeps the output continuous while the stage count is smoothed.
	#[inline]
	pub fn process(&mut self, coefs: &AllpassCoefs, amount: f32, input: f32) -> f32 {
		let amount = amount.clamp(0.0, MAX_STAGES as f32);
		let whole = amount as usize;
		let frac = amount - whole as f32;
		let needed = if frac > 0.0 { whole + 1 } else { whole };

		if needed > self.active_stages {
			self.stages[self.active_stages..needed]
				.iter_mut()
				.for_each(AllpassStage::reset);
		}
		self.active_stages = needed;

		let mut output = input;
		for stage in &mut self.stages[..whole] {
			output = stage.process(coefs, output);
		}

		if needed > whole {
			let next = self.stages[whole].process(coefs, output);
			output += frac * (next - output);
		}

		output
	}
}

/// Group delay in milliseconds of `amount` cascaded stages at `frequency`.
///
/// The digital filter is the bilinear transform of the analog prototype
/// `(s^2 - (w0/Q)s + w0^2) / (s^2 + (w0/Q)s + w0^2)` with a prewarped cutoff, so the analog group
/// delay is evaluated at the warped frequency and scaled by the derivative of the warping.
pub fn group_delay_ms(frequency: f32, center: f32, q: f32, amount: f32, sample_rate: f32) -> f32 {
	let nyquist_limit = sample_rate * 0.49;
	let w = (PI * frequency.clamp(1.0, nyquist_limit) / sample_rate).tan();
	let w0 = (PI * center.clamp(1.0, nyquist_limit) / sample_rate).tan();
	let bw = w0 / q.max(1.0e-3);

	let diff = w0 * w0 - w * w;
	let analog = 2.0 * bw * (w0 * w0 + w * w) / (diff * diff + bw * bw * w * w);
	let samples = analog * 0.5 * (1.0 + w * w);

	amount * samples / sample_rate * 1000.0
}

#[cfg(test)]
mod tests {
	use super::*;

	const SAMPLE_RATE: f32 = 48_000.0;

	fn impulse_response(frequency: f32, q: f32, amount: f32, len: usize) -> Vec<f32> {
		let coefs = AllpassCoefs::new(frequency, q, SAMPLE_RATE);
		let mut disperser = Disperser::default();
		(0..len)
			.map(|n| disperser.process(&coefs, amount, if n == 0 { 1.0 } else { 0.0 }))
			.collect()
	}

	#[test]
	fn zero_amount_is_identity() {
		let ir = impulse_response(1000.0, 1.0, 0.0, 16);
		assert_eq!(ir[0], 1.0);
		assert!(ir[1..].iter().all(|&s| s == 0.0));
	}

	#[test]
	fn cascade_preserves_energy() {
		for &(frequency, q, amount) in &[(200.0, 0.7, 1.0), (1000.0, 2.0, 50.0), (5000.0, 8.0, 200.0)] {
			let ir = impulse_response(frequency, q, amount, SAMPLE_RATE as usize * 4);
			let energy: f32 = ir.iter().map(|s| s * s).sum();
			assert!(
				(energy - 1.0).abs() < 1.0e-3,
				"energy {energy} for {frequency} Hz, Q {q}, {amount} stages"
			);
		}
	}

	/// Phase of the DTFT of `ir` at `frequency`.
	fn phase_at(ir: &[f32], frequency: f64) -> f64 {
		let omega = std::f64::consts::TAU * frequency / SAMPLE_RATE as f64;
		let (re, im) = ir.iter().enumerate().fold((0.0, 0.0), |(re, im), (n, &h)| {
			let (sin, cos) = (omega * n as f64).sin_cos();
			(re + h as f64 * cos, im - h as f64 * sin)
		});
		im.atan2(re)
	}

	#[test]
	fn measured_group_delay_matches_analytic() {
		let (center, q, amount) = (1000.0, 4.0, 32.0);
		let ir = impulse_response(center, q, amount, SAMPLE_RATE as usize * 2);

		for &frequency in &[300.0, 1000.0, 1200.0, 5000.0] {
			// The step is small enough that the phase difference stays within (-pi, pi).
			let delta = 0.1;
			let mut dphi = phase_at(&ir, frequency + delta) - phase_at(&ir, frequency - delta);
			dphi = (dphi + std::f64::consts::PI).rem_euclid(std::f64::consts::TAU) - std::f64::consts::PI;
			let domega = std::f64::consts::TAU * 2.0 * delta / SAMPLE_RATE as f64;
			let measured = (-dphi / domega) as f32;

			let expected = group_delay_ms(frequency as f32, center, q, amount, SAMPLE_RATE) * SAMPLE_RATE / 1000.0;
			assert!(
				(measured - expected).abs() <= expected * 0.01 + 0.5,
				"{frequency} Hz: measured {measured}, expected {expected}"
			);
		}
	}
}
