//! Clip audio-state queries for editor UIs.

use std::ffi::c_void;

use crate::node::NodeId;

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
			let mut get = |input: &str| {
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
			effects.push(ClipAudioEffect {
				type_id,
				enabled: info.enabled,
				volume: get("volume_in"),
				pan: get("panning_in"),
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
}
