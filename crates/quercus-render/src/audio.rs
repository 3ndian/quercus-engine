// Quercus Video Editor - Non-Linear Video Editor
// Copyright (C) 2026 Quercus Team
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <http://www.gnu.org/licenses/>.

//! Audio loudness analysis and the sequence master-bus effect setting.
//!
//! [`measure_loudness`] approximates EBU R128 / ITU-R BS.1770 integrated
//! loudness from an interleaved f32 buffer: K-weighting (high shelf +
//! high pass), 400 ms blocks with 75 % overlap, the -70 LUFS absolute gate
//! and the -10 dB relative gate, then the gated mean power. It is a
//! measurement approximation, not a certified R128 meter:
//!
//! - channel weights are all 1.0 because the API receives a channel count,
//!   not a channel layout (surround weights are therefore omitted);
//! - the reported peak is a 4x oversampled cubic-interpolation peak, not a
//!   spec-compliant 4x FIR true-peak meter;
//! - a buffer shorter than one 400 ms block is measured as one block
//!   instead of producing no measurement.
//!
//! The module also owns the persisted master-bus chain shape. Bus effects
//! live in the project's settings map under
//! [`AUDIO_BUS_EFFECTS_SETTING`], keyed by sequence identity, so preview,
//! export and the render worker all read the same chain and it survives a
//! project save.

use std::collections::HashMap;

use quercus_node::value::NodeValue;
use serde::{Deserialize, Serialize};

use crate::ticket::MontageAudioEffect;

/// Project-settings key holding the per-sequence master-bus chains.
///
/// The value is a JSON object `{"<sequence identity>": [effect, ...]}`.
/// An absent key (the common case) means no bus processing.
pub const AUDIO_BUS_EFFECTS_SETTING: &str = "quercus.audio.bus_effects";

/// Result of [`measure_loudness`].
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct LoudnessMeasurement {
	/// Gated integrated loudness in LUFS. `f64::NEG_INFINITY` for silence
	/// (or invalid/empty input).
	pub integrated_lufs: f64,
	/// Approximate true peak in dBFS. `f64::NEG_INFINITY` for silence.
	pub peak_dbfs: f64,
}

/// One stored bus-effect parameter (only finite scalar values are
/// persisted; the built-in bus effects are all float-parameter kernels).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
struct StoredBusParam {
	input: String,
	value: f64,
}

/// One stored bus effect. Kept separate from [`MontageAudioEffect`] so the
/// setting has a plain JSON shape independent of the node-value wire enum.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
struct StoredBusEffect {
	#[serde(default)]
	type_id: String,
	#[serde(default = "stored_enabled_default")]
	enabled: bool,
	#[serde(default)]
	params: Vec<StoredBusParam>,
}

fn stored_enabled_default() -> bool {
	true
}

impl StoredBusEffect {
	fn from_effect(effect: &MontageAudioEffect) -> Self {
		StoredBusEffect {
			type_id: effect.type_id.clone(),
			enabled: effect.enabled,
			params: effect
				.params
				.iter()
				.filter_map(|(input, value)| {
					let value = value.to_double();
					value.is_finite().then(|| StoredBusParam {
						input: input.clone(),
						value,
					})
				})
				.collect(),
		}
	}

	fn to_effect(&self) -> MontageAudioEffect {
		MontageAudioEffect {
			type_id: self.type_id.clone(),
			enabled: self.enabled,
			params: self
				.params
				.iter()
				.map(|param| (param.input.clone(), NodeValue::Float(param.value)))
				.collect(),
		}
	}
}

type StoredBuses = HashMap<String, Vec<StoredBusEffect>>;

fn parse_buses(text: &str) -> StoredBuses {
	serde_json::from_str(text).unwrap_or_default()
}

/// Load the master-bus chain for `sequence` from a project settings map.
///
/// Malformed or legacy settings degrade to an empty chain; the function
/// never fails and never mutates the map.
pub fn load_bus_effects(
	settings: &HashMap<String, String>,
	sequence: u64,
) -> Vec<MontageAudioEffect> {
	let Some(text) = settings.get(AUDIO_BUS_EFFECTS_SETTING) else {
		return Vec::new();
	};
	parse_buses(text)
		.get(&sequence.to_string())
		.map(|effects| effects.iter().map(StoredBusEffect::to_effect).collect())
		.unwrap_or_default()
}

/// Store (or clear) the master-bus chain for `sequence` in a project
/// settings map.
///
/// An empty chain removes the sequence entry; when no sequence has a
/// chain left the setting key itself is removed so a project without bus
/// processing keeps its settings map unchanged.
pub fn store_bus_effects(
	settings: &mut HashMap<String, String>,
	sequence: u64,
	effects: &[MontageAudioEffect],
) -> Result<(), String> {
	let mut buses = settings
		.get(AUDIO_BUS_EFFECTS_SETTING)
		.map(|text| parse_buses(text))
		.unwrap_or_default();
	let key = sequence.to_string();
	if effects.is_empty() {
		buses.remove(&key);
	} else {
		buses.insert(
			key,
			effects.iter().map(StoredBusEffect::from_effect).collect(),
		);
	}
	if buses.is_empty() {
		settings.remove(AUDIO_BUS_EFFECTS_SETTING);
	} else {
		let text = serde_json::to_string(&buses)
			.map_err(|e| format!("serialize audio bus effects: {e}"))?;
		settings.insert(AUDIO_BUS_EFFECTS_SETTING.to_string(), text);
	}
	Ok(())
}

/// Approximate true peak of an interleaved buffer: the sample peak and the
/// 4x cubic (Catmull-Rom) interpolated points between neighbouring
/// samples. Returns linear amplitude, `0.0` for empty/silent input.
fn true_peak(samples: &[f32], channels: usize) -> f64 {
	let frames = samples.len() / channels;
	if frames == 0 {
		return 0.0;
	}
	let sample_at = |channel: usize, frame: isize| -> f64 {
		let frame = frame.clamp(0, frames as isize - 1) as usize;
		f64::from(samples[frame * channels + channel])
	};
	let mut peak = 0.0f64;
	for channel in 0..channels {
		for frame in 0..frames {
			peak = peak.max(sample_at(channel, frame as isize).abs());
		}
		// Cubic interpolation between every pair of samples catches most
		// inter-sample overshoot that a sample-peak meter misses.
		for frame in 0..frames.saturating_sub(1) {
			let p0 = sample_at(channel, frame as isize - 1);
			let p1 = sample_at(channel, frame as isize);
			let p2 = sample_at(channel, frame as isize + 1);
			let p3 = sample_at(channel, frame as isize + 2);
			for step in 1..4 {
				let t = step as f64 / 4.0;
				let value = 0.5
					* ((2.0 * p1)
						+ (-p0 + p2) * t + (2.0 * p0 - 5.0 * p1 + 4.0 * p2 - p3) * t * t
						+ (-p0 + 3.0 * p1 - 3.0 * p2 + p3) * t * t * t);
				peak = peak.max(value.abs());
			}
		}
	}
	peak
}

/// One biquad section in transposed direct form II.
#[derive(Clone, Copy, Debug)]
struct Biquad {
	b0: f64,
	b1: f64,
	b2: f64,
	a1: f64,
	a2: f64,
	z1: f64,
	z2: f64,
}

impl Biquad {
	fn new(b0: f64, b1: f64, b2: f64, a1: f64, a2: f64) -> Self {
		Biquad {
			b0,
			b1,
			b2,
			a1,
			a2,
			z1: 0.0,
			z2: 0.0,
		}
	}

	fn process(&mut self, x: f64) -> f64 {
		let y = self.b0 * x + self.z1;
		self.z1 = self.b1 * x - self.a1 * y + self.z2;
		self.z2 = self.b2 * x - self.a2 * y;
		y
	}
}

/// BS.1770 K-weighting stages for `rate` (the pyloudnorm formulation of
/// the ITU high-shelf + high-pass pair, which reduces to the published
/// 48 kHz coefficients).
fn k_weighting(rate: f64) -> (Biquad, Biquad) {
	let rate = rate.max(1.0);
	let pi = std::f64::consts::PI;

	// Stage 1: +4 dB high shelf at ~1.68 kHz.
	let f0 = 1681.974_450_955_533;
	let gain_db = 3.999_843_853_973_347;
	let q = 0.707_175_236_955_419_6;
	let k = (pi * f0 / rate).tan();
	let vh = 10f64.powf(gain_db / 20.0);
	let vb = vh.powf(0.499_666_774_154_541_6);
	let a0 = 1.0 + k / q + k * k;
	let shelf = Biquad::new(
		(vh + vb * k / q + k * k) / a0,
		2.0 * (k * k - vh) / a0,
		(vh - vb * k / q + k * k) / a0,
		2.0 * (k * k - 1.0) / a0,
		(1.0 - k / q + k * k) / a0,
	);

	// Stage 2: 38 Hz high pass.
	let f0 = 38.135_470_876_024_44;
	let q = 0.500_327_037_323_877_3;
	let k = (pi * f0 / rate).tan();
	let a0 = 1.0 + k / q + k * k;
	let high_pass = Biquad::new(
		1.0 / a0,
		-2.0 / a0,
		1.0 / a0,
		2.0 * (k * k - 1.0) / a0,
		(1.0 - k / q + k * k) / a0,
	);

	(shelf, high_pass)
}

/// Approximate integrated loudness (LUFS) and true peak (dBFS) of an
/// interleaved f32 buffer.
///
/// `samples` is interleaved by `channels`; `rate` is the sample rate in
/// Hz. The buffer is K-weighted, split into 400 ms blocks with 75 %
/// overlap, absolute-gated at -70 LUFS and relative-gated at -10 dB below
/// the absolute-gated mean, then the surviving block powers are averaged.
/// Silence returns `f64::NEG_INFINITY` for both fields.
pub fn measure_loudness(samples: &[f32], rate: u32, channels: u16) -> LoudnessMeasurement {
	let channels = channels as usize;
	if channels == 0 || rate == 0 || samples.len() < channels {
		return LoudnessMeasurement {
			integrated_lufs: f64::NEG_INFINITY,
			peak_dbfs: f64::NEG_INFINITY,
		};
	}
	let frames = samples.len() / channels;
	let rate_f = f64::from(rate);
	let peak = true_peak(samples, channels);
	let peak_dbfs = if peak > 0.0 {
		20.0 * peak.log10()
	} else {
		f64::NEG_INFINITY
	};

	// K-weight each channel into a planar buffer for block analysis.
	let (shelf_coeffs, high_pass_coeffs) = k_weighting(rate_f);
	let mut planar = vec![vec![0.0f64; frames]; channels];
	for channel in 0..channels {
		let mut shelf = shelf_coeffs;
		let mut high_pass = high_pass_coeffs;
		for frame in 0..frames {
			let x = f64::from(samples[frame * channels + channel]);
			planar[channel][frame] = high_pass.process(shelf.process(x));
		}
	}

	let block_frames = (rate_f * 0.4).round().max(1.0) as usize;
	let hop = (block_frames / 4).max(1);
	// A short buffer is one block, not "no measurement".
	let starts: Vec<usize> = if frames < block_frames {
		vec![0]
	} else {
		(0..=frames - block_frames).step_by(hop).collect()
	};
	let block_len = if frames < block_frames {
		frames
	} else {
		block_frames
	};
	if block_len == 0 {
		return LoudnessMeasurement {
			integrated_lufs: f64::NEG_INFINITY,
			peak_dbfs,
		};
	}

	let mut powers = Vec::with_capacity(starts.len());
	for start in starts {
		let end = (start + block_len).min(frames);
		let mut sum = 0.0f64;
		for channel in 0..channels {
			let mut square_sum = 0.0f64;
			for value in &planar[channel][start..end] {
				square_sum += value * value;
			}
			sum += square_sum / (end - start) as f64;
		}
		powers.push(sum);
	}

	let block_loudness = |power: f64| -> f64 {
		if power <= 0.0 {
			f64::NEG_INFINITY
		} else {
			-0.691 + 10.0 * power.log10()
		}
	};
	// Absolute gate at -70 LUFS.
	let absolute: Vec<f64> = powers
		.iter()
		.copied()
		.filter(|power| block_loudness(*power) > -70.0)
		.collect();
	if absolute.is_empty() {
		return LoudnessMeasurement {
			integrated_lufs: f64::NEG_INFINITY,
			peak_dbfs,
		};
	}
	let mean_absolute = absolute.iter().sum::<f64>() / absolute.len() as f64;
	let relative_gate = block_loudness(mean_absolute) - 10.0;
	let gated: Vec<f64> = absolute
		.into_iter()
		.filter(|power| block_loudness(*power) > relative_gate)
		.collect();
	let integrated = if gated.is_empty() {
		f64::NEG_INFINITY
	} else {
		block_loudness(gated.iter().sum::<f64>() / gated.len() as f64)
	};
	LoudnessMeasurement {
		integrated_lufs: integrated,
		peak_dbfs,
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn sine(frequency: f64, amplitude: f64, rate: u32, seconds: f64) -> Vec<f32> {
		let frames = (f64::from(rate) * seconds) as usize;
		(0..frames)
			.map(|frame| {
				let t = frame as f64 / f64::from(rate);
				(amplitude * (std::f64::consts::TAU * frequency * t).sin()) as f32
			})
			.collect()
	}

	#[test]
	fn known_sine_has_sane_lufs_and_peak() {
		let rate = 48_000;
		let samples = sine(1_000.0, 0.5, rate, 2.0);
		let measured = measure_loudness(&samples, rate, 1);
		assert!(
			measured.integrated_lufs > -20.0 && measured.integrated_lufs < 0.0,
			"1 kHz -6 dBFS sine integrated loudness is sane: {}",
			measured.integrated_lufs
		);
		assert!(
			(measured.peak_dbfs + 6.02).abs() < 0.5,
			"-6 dBFS sine true peak should stay near -6 dBFS, got {}",
			measured.peak_dbfs
		);
	}

	#[test]
	fn silence_is_negative_infinity() {
		let measured = measure_loudness(&vec![0.0f32; 48_000], 48_000, 2);
		assert_eq!(measured.integrated_lufs, f64::NEG_INFINITY);
		assert_eq!(measured.peak_dbfs, f64::NEG_INFINITY);
		// Invalid geometry is also silence, not a panic.
		let measured = measure_loudness(&[], 0, 0);
		assert_eq!(measured.integrated_lufs, f64::NEG_INFINITY);
	}

	#[test]
	fn bus_setting_round_trips_and_clears() {
		let mut settings = HashMap::new();
		let effect = MontageAudioEffect {
			type_id: "org.olivevideoeditor.Olive.limiter".into(),
			enabled: true,
			params: vec![
				("ceiling_db".into(), NodeValue::Float(-1.0)),
				("release_ms".into(), NodeValue::Float(50.0)),
			],
		};
		store_bus_effects(&mut settings, 42, &[effect.clone()]).expect("store");
		assert!(settings.contains_key(AUDIO_BUS_EFFECTS_SETTING));
		let loaded = load_bus_effects(&settings, 42);
		assert_eq!(loaded, vec![effect]);
		assert!(load_bus_effects(&settings, 43).is_empty());

		store_bus_effects(&mut settings, 42, &[]).expect("clear");
		assert!(!settings.contains_key(AUDIO_BUS_EFFECTS_SETTING));
		assert!(load_bus_effects(&settings, 42).is_empty());
	}
}
