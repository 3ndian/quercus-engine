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


/// C mirror of the loudness measurement result.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct QuercusLoudness {
	/// Gated integrated loudness in LUFS (`-inf` for silence).
	pub integrated_lufs: f64,
	/// Approximate true peak in dBFS (`-inf` for silence).
	pub peak_dbfs: f64,
}

/// The session's current sequence, or `None` when none is selected.
fn current_sequence(session: &EditorSessionHandle) -> Option<NodeId> {
	let state = session.inner.lock().unwrap_or_else(|e| e.into_inner());
	state.current_sequence.valid().then_some(state.current_sequence)
}

/// Measure the current sequence's integrated loudness over `range`.
///
/// The range is mixed through the normal preview-audio path (the same
/// montage, clip effects and master-bus chain playback/export use) and
/// then evaluated with [`quercus_render::audio::measure_loudness`].
/// `range` is a pointer to a [`QuercusTimeRange`]; `out` receives the
/// [`QuercusLoudness`] POD. Returns false for a null/invalid session,
/// range or output, no current sequence, or a failed audio render.
///
/// [`QuercusTimeRange`]: crate::coretypes::QuercusTimeRange
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_editor_measure_loudness(
	session: *const EditorSessionHandle,
	range: *const crate::coretypes::QuercusTimeRange,
	out: *mut QuercusLoudness,
) -> bool {
	use crate::handle::NativeMirror;
	let (Some(session), Some(range), Some(out)) = (
		unsafe { session.as_ref() },
		unsafe { range.as_ref() },
		unsafe { out.as_mut() },
	) else {
		return false;
	};
	let Some(sequence) = current_sequence(session) else {
		return false;
	};
	let range = range.to_native();
	let project = session.project();
	let Ok(audio) = crate::preview_audio::render_sequence_audio(project, sequence, range) else {
		return false;
	};
	let measured = quercus_render::audio::measure_loudness(
		&audio.samples,
		audio.sample_rate.max(1) as u32,
		audio.channel_count.max(1) as u16,
	);
	*out = QuercusLoudness {
		integrated_lufs: measured.integrated_lufs,
		peak_dbfs: measured.peak_dbfs,
	};
	true
}

/// Number of entries in the current sequence's master-bus effect chain.
/// Returns 0 for a null session or no current sequence.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_editor_audio_bus_effect_count(
	session: *const EditorSessionHandle,
) -> i32 {
	let Some(session) = (unsafe { session.as_ref() }) else {
		return 0;
	};
	let Some(sequence) = current_sequence(session) else {
		return 0;
	};
	crate::editor_commands::audio_bus_effects(&session.project(), sequence).len() as i32
}

/// Reads one master-bus effect entry's type id and enabled flag.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_editor_audio_bus_effect_info(
	session: *const EditorSessionHandle,
	index: i32,
	out: *mut QuercusClipAudioEffectInfo,
) -> bool {
	let (Some(session), Some(out)) = (unsafe { session.as_ref() }, unsafe { out.as_mut() }) else {
		return false;
	};
	if index < 0 {
		return false;
	}
	let Some(sequence) = current_sequence(session) else {
		return false;
	};
	let effects = crate::editor_commands::audio_bus_effects(&session.project(), sequence);
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

/// Reads one static scalar parameter of a master-bus effect entry.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_editor_audio_bus_effect_param(
	session: *const EditorSessionHandle,
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
	let Some(sequence) = current_sequence(session) else {
		return false;
	};
	let Some(value) = crate::editor_commands::audio_bus_effect_param(
		&session.project(),
		sequence,
		index as usize,
		input,
	) else {
		return false;
	};
	*out = value;
	true
}

/// Appends an effect to the current sequence's master-bus chain at `index`
/// (clamped to the chain length) as one undo entry.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_editor_add_audio_bus_effect(
	session: *const EditorSessionHandle,
	type_id: *const u8,
	type_id_len: usize,
	index: i32,
) -> bool {
	let Some(session) = (unsafe { session.as_ref() }) else {
		return false;
	};
	let Some(type_id) = (unsafe { str_arg(type_id, type_id_len) }) else {
		return false;
	};
	let Ok(index) = usize::try_from(index) else {
		return false;
	};
	let Some(sequence) = current_sequence(session) else {
		return false;
	};
	crate::editor_commands::add_audio_bus_effect(
		&session.project(),
		sequence,
		type_id,
		index,
	)
	.is_ok()
}

/// Removes one entry of the current sequence's master-bus chain.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_editor_remove_audio_bus_effect(
	session: *const EditorSessionHandle,
	index: i32,
) -> bool {
	let Some(session) = (unsafe { session.as_ref() }) else {
		return false;
	};
	let Ok(index) = usize::try_from(index) else {
		return false;
	};
	let Some(sequence) = current_sequence(session) else {
		return false;
	};
	crate::editor_commands::remove_audio_bus_effect(&session.project(), sequence, index).is_ok()
}

/// Enables/disables one entry of the current sequence's master-bus chain.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_editor_set_audio_bus_effect_enabled(
	session: *const EditorSessionHandle,
	index: i32,
	enabled: bool,
) -> bool {
	let Some(session) = (unsafe { session.as_ref() }) else {
		return false;
	};
	let Ok(index) = usize::try_from(index) else {
		return false;
	};
	let Some(sequence) = current_sequence(session) else {
		return false;
	};
	crate::editor_commands::set_audio_bus_effect_enabled(
		&session.project(),
		sequence,
		index,
		enabled,
	)
	.is_ok()
}

/// Sets one static float parameter of a master-bus effect entry.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_editor_set_audio_bus_effect_param(
	session: *const EditorSessionHandle,
	index: i32,
	input_id: *const u8,
	input_id_len: usize,
	value: f64,
) -> bool {
	let Some(session) = (unsafe { session.as_ref() }) else {
		return false;
	};
	let Some(input_id) = (unsafe { str_arg(input_id, input_id_len) }) else {
		return false;
	};
	let Ok(index) = usize::try_from(index) else {
		return false;
	};
	let Some(sequence) = current_sequence(session) else {
		return false;
	};
	crate::editor_commands::set_audio_bus_effect_param(
		&session.project(),
		sequence,
		index,
		input_id,
		value,
	)
	.is_ok()
}
