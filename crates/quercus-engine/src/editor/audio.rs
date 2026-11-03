//! Clip audio-state queries used by the editor UI.

use super::EditorSessionHandle;
use crate::codec::str_arg;
use quercus_core::Rational;
use quercus_node::block::ClipBlockBehavior;
use quercus_node::id::NodeId;

/// C mirror of one clip's audio mix settings.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct QuercusClipAudioState {
	/// Gain (1.0 = unity).
	pub gain: f64,
	/// Stereo pan (-1..1).
	pub pan: f64,
	/// Fade-in numerator.
	pub fade_in_num: i64,
	/// Fade-in denominator.
	pub fade_in_den: i64,
	/// Fade-out numerator.
	pub fade_out_num: i64,
	/// Fade-out denominator.
	pub fade_out_den: i64,
	/// Number of entries in the audio effect chain.
	pub effect_count: i32,
}

/// C mirror of one audio effect-chain entry.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct QuercusClipAudioEffectInfo {
	/// NUL-terminated effect type id.
	pub type_id: [u8; 128],
	/// Whether the effect is enabled.
	pub enabled: bool,
}

impl Default for QuercusClipAudioEffectInfo {
	fn default() -> Self {
		Self {
			type_id: [0u8; 128],
			enabled: true,
		}
	}
}

fn clip_core(
	session: &EditorSessionHandle,
	clip: u64,
) -> Option<(f64, f64, Rational, Rational, Vec<quercus_node::block::ClipAudioEffect>)> {
	let clip = NodeId::from_identity(clip)?;
	let project = session.project();
	let guard = project.lock().unwrap_or_else(|e| e.into_inner());
	let entry = guard.graph.get(clip)?;
	let behavior = entry
		.behavior
		.as_any()
		.and_then(|a| a.downcast_ref::<ClipBlockBehavior>())?;
	Some((
		behavior.core.audio_gain,
		behavior.core.audio_pan,
		behavior.core.audio_fade_in,
		behavior.core.audio_fade_out,
		behavior.core.audio_effects.clone(),
	))
}

/// Reads a clip's gain/pan/fade fields and effect count.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_editor_clip_audio_state(
	session: *const EditorSessionHandle,
	clip: u64,
	out: *mut QuercusClipAudioState,
) -> bool {
	let (Some(session), Some(out)) = (unsafe { session.as_ref() }, unsafe { out.as_mut() }) else {
		return false;
	};
	let Some((gain, pan, fade_in, fade_out, effects)) = clip_core(session, clip) else {
		return false;
	};
	*out = QuercusClipAudioState {
		gain,
		pan,
		fade_in_num: fade_in.numerator(),
		fade_in_den: fade_in.denominator(),
		fade_out_num: fade_out.numerator(),
		fade_out_den: fade_out.denominator(),
		effect_count: effects.len() as i32,
	};
	true
}

/// Reads one audio effect-chain entry's type id and enabled flag.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_editor_clip_audio_effect_info(
	session: *const EditorSessionHandle,
	clip: u64,
	index: i32,
	out: *mut QuercusClipAudioEffectInfo,
) -> bool {
	let (Some(session), Some(out)) = (unsafe { session.as_ref() }, unsafe { out.as_mut() }) else {
		return false;
	};
	if index < 0 {
		return false;
	}
	let Some((_, _, _, _, effects)) = clip_core(session, clip) else {
		return false;
	};
	let Some(effect) = effects.get(index as usize) else {
		return false;
	};
	let bytes = effect.type_id.as_bytes();
	if bytes.len() >= out.type_id.len() {
		return false;
	}
	out.type_id.fill(0);
	out.type_id[..bytes.len()].copy_from_slice(bytes);
	out.enabled = effect.enabled;
	true
}

/// Reads a static scalar parameter of one audio effect-chain entry.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_editor_clip_audio_effect_param(
	session: *const EditorSessionHandle,
	clip: u64,
	index: i32,
	input: *const u8,
	input_len: usize,
	out: *mut f64,
) -> bool {
	let (Some(session), Some(input), Some(out)) = (
		unsafe { session.as_ref() },
		unsafe { str_arg(input, input_len) },
		unsafe { out.as_mut() },
	) else {
		return false;
	};
	if index < 0 {
		return false;
	}
	let Some((_, _, _, _, effects)) = clip_core(session, clip) else {
		return false;
	};
	let Some(effect) = effects.get(index as usize) else {
		return false;
	};
	let Some(value) = effect.param(input) else {
		return false;
	};
	*out = value.to_double();
	true
}

/// Convenience used by tests / UI for a clip's typed effect chain.
pub fn clip_audio_effects(
	project: &crate::editor_commands::ProjectRef,
	clip: NodeId,
) -> Option<Vec<quercus_node::block::ClipAudioEffect>> {
	let guard = project.lock().unwrap_or_else(|e| e.into_inner());
	let entry = guard.graph.get(clip)?;
	let behavior = entry
		.behavior
		.as_any()
		.and_then(|a| a.downcast_ref::<ClipBlockBehavior>())?;
	Some(behavior.core.audio_effects.clone())
}

