//! Clip audio-state queries for editor UIs.

use std::ffi::c_void;

use crate::node::NodeId;
use crate::types::TimeRange;

use super::{EditorSession, Rational};

/// One clip audio effect-chain entry.
#[derive(Clone, Debug, PartialEq)]
pub struct ClipAudioEffect {
	/// Built-in effect type id.
	pub type_id: String,
	/// Whether the effect is enabled.
	pub enabled: bool,
	/// Static Volume value, when the entry carries one.
	pub volume: Option<f64>,
	/// Static Pan value, when the entry carries one.
	pub pan: Option<f64>,
	/// All static scalar parameters the entry carries, keyed by input id.
	pub params: Vec<(String, f64)>,
}

/// The engine's loudness measurement POD. Layout matches
/// `QuercusLoudness` in the engine C ABI.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct QuercusLoudness {
	/// Gated integrated loudness in LUFS (`-inf` for silence).
	pub integrated_lufs: f64,
	/// Approximate true peak in dBFS (`-inf` for silence).
	pub peak_dbfs: f64,
}

/// A loudness measurement in LUFS/dBFS. Alias of the C-compatible POD.
pub type LoudnessMeasurement = QuercusLoudness;

/// One entry of the current sequence's master-bus effect chain.
#[derive(Clone, Debug, PartialEq)]
pub struct AudioBusEffect {
	/// Built-in effect type id.
	pub type_id: String,
	/// Whether the effect is enabled.
	pub enabled: bool,
	/// All static scalar parameters the entry carries, keyed by input id.
	pub params: Vec<(String, f64)>,
}

/// Per-clip audio mix state.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct ClipAudioState {
	/// Gain (1.0 = unity).
	pub gain: f64,
	/// Stereo pan (-1..1).
	pub pan: f64,
	/// Linear fade-in duration.
	pub fade_in: Rational,
	/// Linear fade-out duration.
	pub fade_out: Rational,
	/// Ordered effect chain.
	pub effects: Vec<ClipAudioEffect>,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
struct QuercusClipAudioState {
	gain: f64,
	pan: f64,
	fade_in_num: i64,
	fade_in_den: i64,
	fade_out_num: i64,
	fade_out_den: i64,
	effect_count: i32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
struct QuercusClipAudioEffectInfo {
	type_id: [u8; 128],
	enabled: bool,
}

impl Default for QuercusClipAudioEffectInfo {
	fn default() -> Self {
		Self {
			type_id: [0u8; 128],
			enabled: true,
		}
	}
}

#[cfg_attr(
	windows,
	link(name = "quercus_engine.dll.lib", kind = "dylib", modifiers = "+verbatim")
)]
#[cfg_attr(not(windows), link(name = "quercus_engine", kind = "dylib"))]
unsafe extern "C" {
	fn quercus_editor_clip_audio_state(
		session: *const c_void,
		clip: u64,
		out: *mut QuercusClipAudioState,
	) -> bool;
	fn quercus_editor_clip_audio_effect_info(
		session: *const c_void,
		clip: u64,
		index: i32,
		out: *mut QuercusClipAudioEffectInfo,
	) -> bool;
	fn quercus_editor_clip_audio_effect_param(
		session: *const c_void,
		clip: u64,
		index: i32,
		input: *const u8,
		input_len: usize,
		out: *mut f64,
	) -> bool;
	fn quercus_editor_measure_loudness(
		session: *const c_void,
		range: *const TimeRange,
		out: *mut QuercusLoudness,
	) -> bool;
	fn quercus_editor_audio_bus_effect_count(session: *const c_void) -> i32;
	fn quercus_editor_audio_bus_effect_info(
		session: *const c_void,
		index: i32,
		out: *mut QuercusClipAudioEffectInfo,
	) -> bool;
	fn quercus_editor_audio_bus_effect_param(
		session: *const c_void,
		index: i32,
		input: *const u8,
		input_len: usize,
		out: *mut f64,
	) -> bool;
	fn quercus_editor_add_audio_bus_effect(
		session: *const c_void,
		type_id: *const u8,
		type_id_len: usize,
		index: i32,
	) -> bool;
	fn quercus_editor_remove_audio_bus_effect(session: *const c_void, index: i32) -> bool;
	fn quercus_editor_set_audio_bus_effect_enabled(
		session: *const c_void,
		index: i32,
		enabled: bool,
	) -> bool;
	fn quercus_editor_set_audio_bus_effect_param(
		session: *const c_void,
		index: i32,
		input_id: *const u8,
		input_id_len: usize,
		value: f64,
	) -> bool;
}

impl EditorSession {
	/// Reads the selected clip's audio mix and effect chain. Returns `None`
	/// for a non-clip or invalid node.
	pub fn clip_audio(&self, clip: NodeId) -> Option<ClipAudioState> {
		let mut raw = QuercusClipAudioState::default();
		unsafe {
			quercus_editor_clip_audio_state(self.ptr, clip.raw(), &mut raw)
		}
		.then_some(())?;
		let mut effects = Vec::new();
		for index in 0..raw.effect_count.max(0) {
			let mut info = QuercusClipAudioEffectInfo::default();
			if !unsafe {
				quercus_editor_clip_audio_effect_info(self.ptr, clip.raw(), index, &mut info)
			} {
				continue;
			}
			let end = info
				.type_id
				.iter()
				.position(|&byte| byte == 0)
				.unwrap_or(info.type_id.len());
			let type_id = String::from_utf8_lossy(&info.type_id[..end]).into_owned();
			let get = |input: &str| {
				let mut value = 0.0f64;
				unsafe {
					quercus_editor_clip_audio_effect_param(
						self.ptr,
						clip.raw(),
						index,
						input.as_ptr(),
						input.len(),
						&mut value,
					)
				}
				.then_some(value)
			};
			let inputs: &[&str] = match type_id.as_str() {
				"org.olivevideoeditor.Olive.volume" => &["volume_in"],
				"org.olivevideoeditor.Olive.pan" => &["panning_in"],
				"org.olivevideoeditor.Olive.eq" => {
					&["low_gain_db", "mid_gain_db", "high_gain_db"]
				}
				"org.olivevideoeditor.Olive.compressor" => &[
					"threshold_db",
					"ratio",
					"attack_ms",
					"release_ms",
					"makeup_db",
				],
				"org.olivevideoeditor.Olive.limiter" => &["ceiling_db", "release_ms"],
				"org.olivevideoeditor.Olive.gate" => {
					&["threshold_db", "attack_ms", "release_ms", "hold_ms"]
				}
				"org.olivevideoeditor.Olive.ducking" => &[
					"threshold_db",
					"amount_db",
					"attack_ms",
					"release_ms",
				],
				_ => &[],
			};
			let mut params = Vec::new();
			for input in inputs {
				if let Some(value) = get(input) {
					params.push((input.to_string(), value));
				}
			}
			let volume = params
				.iter()
				.find(|(input, _)| input == "volume_in")
				.map(|(_, value)| *value);
			let pan = params
				.iter()
				.find(|(input, _)| input == "panning_in")
				.map(|(_, value)| *value);
			effects.push(ClipAudioEffect {
				type_id,
				enabled: info.enabled,
				volume,
				pan,
				params,
			});
		}
		Some(ClipAudioState {
			gain: raw.gain,
			pan: raw.pan,
			fade_in: Rational::new(raw.fade_in_num, raw.fade_in_den),
			fade_out: Rational::new(raw.fade_out_num, raw.fade_out_den),
			effects,
		})
	}

	/// Mixes `range` through the normal preview-audio path and measures
	/// its approximate integrated LUFS and true peak. Returns `None` when
	/// the session has no current sequence or the audio render fails.
	pub fn measure_loudness(&self, range: TimeRange) -> Option<LoudnessMeasurement> {
		let mut out = QuercusLoudness::default();
		unsafe { quercus_editor_measure_loudness(self.ptr, &range, &mut out) }.then_some(out)
	}

	/// Reads the current sequence's master-bus effect chain.
	pub fn audio_bus_effects(&self) -> Vec<AudioBusEffect> {
		let count = unsafe { quercus_editor_audio_bus_effect_count(self.ptr) }.max(0);
		let mut effects = Vec::new();
		for index in 0..count {
			let mut info = QuercusClipAudioEffectInfo::default();
			if !unsafe { quercus_editor_audio_bus_effect_info(self.ptr, index, &mut info) } {
				continue;
			}
			let end = info
				.type_id
				.iter()
				.position(|&byte| byte == 0)
				.unwrap_or(info.type_id.len());
			let type_id = String::from_utf8_lossy(&info.type_id[..end]).into_owned();
			let get = |input: &str| {
				let mut value = 0.0f64;
				unsafe {
					quercus_editor_audio_bus_effect_param(
						self.ptr,
						index,
						input.as_ptr(),
						input.len(),
						&mut value,
					)
				}
				.then_some(value)
			};
			let inputs: &[&str] = match type_id.as_str() {
				"org.olivevideoeditor.Olive.volume" => &["volume_in"],
				"org.olivevideoeditor.Olive.pan" => &["panning_in"],
				"org.olivevideoeditor.Olive.eq" => {
					&["low_gain_db", "mid_gain_db", "high_gain_db"]
				}
				"org.olivevideoeditor.Olive.compressor" => &[
					"threshold_db",
					"ratio",
					"attack_ms",
					"release_ms",
					"makeup_db",
				],
				"org.olivevideoeditor.Olive.limiter" => &["ceiling_db", "release_ms"],
				"org.olivevideoeditor.Olive.gate" => {
					&["threshold_db", "attack_ms", "release_ms", "hold_ms"]
				}
				"org.olivevideoeditor.Olive.ducking" => &[
					"threshold_db",
					"amount_db",
					"attack_ms",
					"release_ms",
				],
				_ => &[],
			};
			let mut params = Vec::new();
			for input in inputs {
				if let Some(value) = get(input) {
					params.push((input.to_string(), value));
				}
			}
			effects.push(AudioBusEffect {
				type_id,
				enabled: info.enabled,
				params,
			});
		}
		effects
	}

	/// Appends an effect to the current sequence's master-bus chain at
	/// `index` (clamped to the chain length) as one undo entry.
	pub fn add_audio_bus_effect(&self, type_id: &str, index: usize) -> bool {
		let Ok(index) = i32::try_from(index) else {
			return false;
		};
		unsafe {
			quercus_editor_add_audio_bus_effect(
				self.ptr,
				type_id.as_ptr(),
				type_id.len(),
				index,
			)
		}
	}

	/// Removes one entry of the current sequence's master-bus chain.
	pub fn remove_audio_bus_effect(&self, index: usize) -> bool {
		let Ok(index) = i32::try_from(index) else {
			return false;
		};
		unsafe { quercus_editor_remove_audio_bus_effect(self.ptr, index) }
	}

	/// Enables/disables one entry of the current sequence's master-bus chain.
	pub fn set_audio_bus_effect_enabled(&self, index: usize, enabled: bool) -> bool {
		let Ok(index) = i32::try_from(index) else {
			return false;
		};
		unsafe { quercus_editor_set_audio_bus_effect_enabled(self.ptr, index, enabled) }
	}

	/// Sets one static float parameter of a master-bus effect entry.
	pub fn set_audio_bus_effect_param(
		&self,
		index: usize,
		input_id: &str,
		value: f64,
	) -> bool {
		let Ok(index) = i32::try_from(index) else {
			return false;
		};
		unsafe {
			quercus_editor_set_audio_bus_effect_param(
				self.ptr,
				index,
				input_id.as_ptr(),
				input_id.len(),
				value,
			)
		}
	}
}
