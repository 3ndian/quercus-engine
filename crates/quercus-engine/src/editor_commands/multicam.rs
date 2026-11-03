//! Multicam graph queries and undoable session operations.
//!
//! Graph construction details and enable/disable/split behavior remain in
//! quercus-timeline. This module resolves session nodes, groups related video/AFV
//! audio edits into one history entry, and exposes only semantic operations to
//! the dispatcher.

use super::prelude::*;
use super::{
	media::{push_command, push_multi_command},
	project::{
		clip_behavior, lock, node_ref, sequence_behavior, track_behavior, track_list_behavior,
	},
	sequence::{clip_track, find_input_footage, track_ids, track_list_of},
};
use quercus_node::block::{
	AUDIO_EFFECT_PAN_TYPE_ID, AUDIO_EFFECT_VOLUME_TYPE_ID, AUDIO_PAN_INPUT, AUDIO_VOLUME_INPUT,
	ClipAudioEffect, clip_input,
};
use quercus_node::nodes::multicamnode::{CURRENT_INPUT, SEQUENCE_INPUT, SEQUENCE_TYPE_INPUT};
use quercus_node::value::NodeValue;

/// Resolved multicam state for UI and independent SDK consumers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MulticamState {
	pub sequence: NodeId,
	pub multicam: NodeId,
	pub clip: NodeId,
	pub source_count: i32,
	pub current_source: i32,
}

/// Finds the first sequence feeding a clip's texture chain.
pub fn clip_connected_sequence(graph: &Graph, clip: NodeId) -> Option<NodeId> {
	fn walk(graph: &Graph, node: NodeId, visited: &mut Vec<NodeId>) -> Option<NodeId> {
		if visited.contains(&node) {
			return None;
		}
		visited.push(node);
		for (from, _, _) in graph.input_connections(node) {
			if sequence_behavior(graph, from).is_some() {
				return Some(from);
			}
			if let Some(found) = walk(graph, from, visited) {
				return Some(found);
			}
		}
		None
	}
	let source = graph.connected_output(clip, clip_input::TEXTURE_INPUT, -1)?;
	if sequence_behavior(graph, source).is_some() {
		return Some(source);
	}
	walk(graph, source, &mut Vec::new())
}

/// Whether a clip's texture input is directly fed by a multicam node.
pub fn clip_is_multicam(graph: &Graph, clip: NodeId) -> bool {
	graph
		.connected_output(clip, clip_input::TEXTURE_INPUT, -1)
		.and_then(|source| graph.get(source))
		.is_some_and(|entry| entry.behavior.type_id() == "org.olivevideoeditor.Olive.multicam")
}

fn sequence_type(graph: &Graph, multicam: NodeId) -> TrackType {
	graph
		.get(multicam)
		.map(|entry| {
			let value = entry
				.core
				.standard_value(SEQUENCE_TYPE_INPUT, -1)
				.to_double() as i32;
			TrackType::from_c(value).unwrap_or(TrackType::Video)
		})
		.unwrap_or(TrackType::Video)
}

fn current_source(graph: &Graph, multicam: NodeId) -> i32 {
	graph
		.get(multicam)
		.map(|entry| entry.core.standard_value(CURRENT_INPUT, -1).to_double() as i32)
		.unwrap_or(-1)
}

fn source_count(graph: &Graph, multicam: NodeId) -> i32 {
	let Some(sequence) = graph.connected_output(multicam, SEQUENCE_INPUT, -1) else {
		return 0;
	};
	track_list_of(graph, sequence, sequence_type(graph, multicam))
		.and_then(|list| track_list_behavior(graph, list))
		.map(|list| list.tracks.len() as i32)
		.unwrap_or(0)
}

/// Returns the source sequence connected to a multicam node.
pub fn multicam_sequence(graph: &Graph, multicam: NodeId) -> Option<NodeId> {
	graph.connected_output(multicam, SEQUENCE_INPUT, -1)
}

/// Returns the angle's track inside the connected source sequence.
pub fn multicam_source_track(
	project: &ProjectRef,
	multicam: NodeId,
	source: i32,
) -> Option<NodeId> {
	if source < 0 {
		return None;
	}
	let guard = lock(project);
	let sequence = multicam_sequence(&guard.graph, multicam)?;
	track_ids(
		&guard.graph,
		sequence,
		sequence_type(&guard.graph, multicam),
	)
	.get(source as usize)
	.copied()
}

/// Resolves a clip's multicam node, source sequence and angle selection.
pub fn multicam_state(project: &ProjectRef, clip: NodeId) -> Option<MulticamState> {
	let multicam = quercus_timeline::multicam::clip_find_multicam(&node_ref(project, clip))?;
	let guard = lock(project);
	let sequence = guard
		.graph
		.connected_output(multicam.id, SEQUENCE_INPUT, -1)?;
	Some(MulticamState {
		sequence,
		multicam: multicam.id,
		clip,
		source_count: source_count(&guard.graph, multicam.id),
		current_source: current_source(&guard.graph, multicam.id),
	})
}

/// Finds the multicam clip covering `time` on a sequence's video tracks.
pub fn multicam_clip_at(project: &ProjectRef, sequence: NodeId, time: Rational) -> Option<NodeId> {
	let candidates: Vec<NodeId> = {
		let guard = lock(project);
		track_ids(&guard.graph, sequence, TrackType::Video)
			.into_iter()
			.flat_map(|track| {
				track_behavior(&guard.graph, track)
					.map(|behavior| behavior.blocks.clone())
					.unwrap_or_default()
			})
			.filter(|&block| {
				clip_behavior(&guard.graph, block)
					.is_some_and(|clip| time >= clip.core.in_() && time < clip.core.out())
			})
			.collect()
	};
	candidates.into_iter().find(|&clip| {
		quercus_timeline::multicam::clip_find_multicam(&node_ref(project, clip)).is_some()
	})
}

/// Enables or disables multicam on every valid selected clip as one undo entry.
pub fn set_multicam_enabled(
	project: &ProjectRef,
	clips: &[NodeId],
	enabled: bool,
) -> Result<(), String> {
	let mut all_clips = Vec::new();
	let mut eligible = Vec::new();
	{
		let guard = lock(project);
		for &clip in clips {
			if clip_behavior(&guard.graph, clip).is_none() {
				continue;
			}
			all_clips.push(clip);
			if let Some(sequence) = clip_connected_sequence(&guard.graph, clip) {
				eligible.push((clip, sequence));
			}
		}
	}
	if enabled {
		// A clip that already has a multicam keeps its existing node; wrapping
		// it again would nest multicams and duplicate the undo entry. Drop
		// those clips up front (the timeline command guards this too).
		eligible.retain(|(clip, _)| {
			quercus_timeline::multicam::clip_find_multicam(&node_ref(project, *clip)).is_none()
		});
		if eligible.is_empty() {
			return Ok(());
		}
		let mut groups: Vec<(NodeId, Vec<NodeRef>)> = Vec::new();
		for (clip, sequence) in eligible.iter().copied() {
			let index = groups
				.iter()
				.position(|(candidate, _)| *candidate == sequence)
				.unwrap_or_else(|| {
					groups.push((sequence, Vec::new()));
					groups.len() - 1
				});
			groups[index].1.push(node_ref(project, clip));
		}
		let children = groups
			.into_iter()
			.map(|(sequence, clips)| {
				quercus_timeline::multicam::multicam_enable(clips, node_ref(project, sequence))
			})
			.collect();
		push_multi_command(
			children,
			&quercus_timeline::multicam::enable_label(eligible.len()),
		)
	} else {
		if all_clips.is_empty() {
			return Ok(());
		}
		let clip_refs = all_clips
			.iter()
			.map(|&clip| node_ref(project, clip))
			.collect();
		push_command(
			quercus_timeline::multicam::multicam_disable(clip_refs),
			&quercus_timeline::multicam::disable_label(all_clips.len()),
		)
	}
}

fn afv_audio_switch_commands(
	project: &ProjectRef,
	clip: NodeId,
	source: i32,
) -> Option<Vec<quercus_undo::undocommand::UndoCommand>> {
	let (audio_clip, old_footage) = {
		let guard = lock(project);
		let audio_clip = guard.graph.links_of(clip).into_iter().find(|&linked| {
			clip_track(&guard.graph, linked)
				.and_then(|track| track_behavior(&guard.graph, track))
				.is_some_and(|track| track.kind == TrackType::Audio)
		})?;
		let old_footage = find_input_footage(&guard.graph, audio_clip);
		(audio_clip, old_footage)
	};
	let old_footage = old_footage?;
	let target_footage = {
		let guard = lock(project);
		let multicam = guard
			.graph
			.connected_output(clip, clip_input::TEXTURE_INPUT, -1)?;
		let sequence = guard.graph.connected_output(multicam, SEQUENCE_INPUT, -1)?;
		let list = track_list_of(&guard.graph, sequence, TrackType::Audio)?;
		let audio_track = track_list_behavior(&guard.graph, list)?
			.tracks
			.get(source as usize)?;
		let angle_clip = track_behavior(&guard.graph, *audio_track)?.blocks.first()?;
		find_input_footage(&guard.graph, *angle_clip)?
	};
	if old_footage == target_footage {
		return Some(Vec::new());
	}
	let disconnect_project = project.clone();
	let reconnect_project = project.clone();
	let disconnect = quercus_undo::undocommand::UndoCommand::from_closures(
		move || {
			let mut guard = lock(&disconnect_project);
			for (from, input, element) in guard.graph.input_connections(audio_clip) {
				if input == clip_input::TEXTURE_INPUT {
					guard.graph.disconnect(from, audio_clip, &input, element);
				}
			}
		},
		move || {
			let mut guard = lock(&reconnect_project);
			let _ = guard
				.graph
				.connect(old_footage, audio_clip, clip_input::TEXTURE_INPUT, -1);
		},
	);
	let connect_project = project.clone();
	let restore_project = project.clone();
	let connect = quercus_undo::undocommand::UndoCommand::from_closures(
		move || {
			let mut guard = lock(&connect_project);
			for (from, input, element) in guard.graph.input_connections(audio_clip) {
				if input == clip_input::TEXTURE_INPUT {
					guard.graph.disconnect(from, audio_clip, &input, element);
				}
			}
			let _ = guard
				.graph
				.connect(target_footage, audio_clip, clip_input::TEXTURE_INPUT, -1);
		},
		move || {
			let mut guard = lock(&restore_project);
			for (from, input, element) in guard.graph.input_connections(audio_clip) {
				if input == clip_input::TEXTURE_INPUT {
					guard.graph.disconnect(from, audio_clip, &input, element);
				}
			}
			let _ = guard
				.graph
				.connect(old_footage, audio_clip, clip_input::TEXTURE_INPUT, -1);
		},
	);
	Some(vec![disconnect, connect])
}

/// Switches video and linked AFV audio in one undo group, optionally splitting
/// at `playhead` through quercus-timeline's identity-preserving split command.
pub fn switch_multicam(
	project: &ProjectRef,
	clip: NodeId,
	source: i32,
	split_clip: bool,
	playhead: Rational,
) -> Result<(), String> {
	let Some(state) = multicam_state(project, clip) else {
		return Ok(());
	};
	if source < 0 || source >= state.source_count || source == state.current_source {
		return Ok(());
	}
	let mut commands = vec![quercus_timeline::multicam::multicam_switch(
		node_ref(project, clip),
		source,
		split_clip,
		playhead,
	)];
	if let Some(audio) = afv_audio_switch_commands(project, clip, source) {
		commands.extend(audio);
	}
	push_multi_command(commands, quercus_timeline::multicam::SWITCH_LABEL)
}

/// Set a clip's audio gain (1.0 = unity) as one undoable command.
pub fn set_clip_gain(project: &ProjectRef, clip: NodeId, gain: f64) -> Result<(), String> {
	if !gain.is_finite() || !(0.0..=4.0).contains(&gain) {
		return Err("audio gain must be finite and within 0..=4".into());
	}
	let old = {
		let guard = lock(project);
		guard
			.graph
			.get(clip)
			.and_then(|entry| entry.behavior.as_any())
			.and_then(|behavior| behavior.downcast_ref::<quercus_node::block::ClipBlockBehavior>())
			.map(|clip| clip.core.audio_gain)
			.ok_or_else(|| "the node is not a clip".to_string())?
	};
	if (old - gain).abs() < f64::EPSILON {
		return Ok(());
	}
	let redo_project = project.clone();
	let undo_project = project.clone();
	super::media::push_command(
		quercus_undo::undocommand::UndoCommand::from_closures(
			move || {
				if let Some(clip) = lock(&redo_project)
					.graph
					.get_mut(clip)
					.and_then(|entry| entry.behavior.as_any_mut())
					.and_then(|behavior| behavior.downcast_mut::<quercus_node::block::ClipBlockBehavior>())
				{
					clip.core.audio_gain = gain;
				}
			},
			move || {
				if let Some(clip) = lock(&undo_project)
					.graph
					.get_mut(clip)
					.and_then(|entry| entry.behavior.as_any_mut())
					.and_then(|behavior| behavior.downcast_mut::<quercus_node::block::ClipBlockBehavior>())
				{
					clip.core.audio_gain = old;
				}
			},
		),
		"Set Clip Gain",
	)
}

/// Replace one clip's whole [`BlockCore`] (undo/redo hook for the
/// audio-parameter commands below).
fn set_clip_core(project: &ProjectRef, clip: NodeId, core: &BlockCore) {
	if let Some(clip) = lock(project)
		.graph
		.get_mut(clip)
		.and_then(|entry| entry.behavior.as_any_mut())
		.and_then(|behavior| behavior.downcast_mut::<ClipBlockBehavior>())
	{
		clip.core = core.clone();
	}
}

/// Apply `update` to a cloned clip core and push one undo row. The
/// closure returns whether anything changed; a no-op push is skipped.
///
/// The whole core is captured in both directions, matching the gain
/// command's semantics: undo restores every field, not just the ones the
/// command touched.
fn update_clip_core(
	project: &ProjectRef,
	clip: NodeId,
	label: &'static str,
	update: impl FnOnce(&mut BlockCore) -> Result<bool, String>,
) -> Result<(), String> {
	let (old, new) = {
		let guard = lock(project);
		let old = guard
			.graph
			.get(clip)
			.and_then(|entry| entry.behavior.as_any())
			.and_then(|behavior| behavior.downcast_ref::<ClipBlockBehavior>())
			.map(|clip| clip.core.clone())
			.ok_or_else(|| "the node is not a clip".to_string())?;
		let mut new = old.clone();
		if !update(&mut new)? {
			return Ok(());
		}
		(old, new)
	};
	let redo_project = project.clone();
	let undo_project = project.clone();
	push_command(
		quercus_undo::undocommand::UndoCommand::from_closures(
			move || set_clip_core(&redo_project, clip, &new),
			move || set_clip_core(&undo_project, clip, &old),
		),
		label,
	)
}

/// Validate a fade duration: finite and non-negative. A fade longer than
/// the clip is allowed; the mixer clamps the ramp at the clip's bounds.
fn validate_fade(which: &str, fade: Rational) -> Result<(), String> {
	let seconds = fade.to_f64();
	if !seconds.is_finite() || seconds < 0.0 {
		return Err(format!("audio {which} fade must be finite and >= 0"));
	}
	Ok(())
}

/// Set a clip's constant-power stereo pan (-1 = hard left, 1 = hard right)
/// as one undoable command.
pub fn set_clip_pan(project: &ProjectRef, clip: NodeId, pan: f64) -> Result<(), String> {
	if !pan.is_finite() || !(-1.0..=1.0).contains(&pan) {
		return Err("audio pan must be finite and within -1..=1".into());
	}
	update_clip_core(project, clip, "Set Clip Pan", |core| {
		if core.audio_pan == pan {
			return Ok(false);
		}
		core.audio_pan = pan;
		Ok(true)
	})
}

/// Set a clip's linear fade-in/fade-out durations as one undoable
/// command. Both are anchored at the clip's timeline in/out points.
pub fn set_clip_fade(
	project: &ProjectRef,
	clip: NodeId,
	fade_in: Rational,
	fade_out: Rational,
) -> Result<(), String> {
	validate_fade("in", fade_in)?;
	validate_fade("out", fade_out)?;
	update_clip_core(project, clip, "Set Clip Fade", |core| {
		if core.audio_fade_in == fade_in && core.audio_fade_out == fade_out {
			return Ok(false);
		}
		core.audio_fade_in = fade_in;
		core.audio_fade_out = fade_out;
		Ok(true)
	})
}

/// Attach an audio effect to a clip at `index` (clamped to the chain
/// length) as one undoable command.
///
/// Phase 1 evaluates the built-in `Volume` and `Pan` kernels; any other
/// type id is carried in the chain and bypassed with a one-shot render
/// warning, so attaching a not-yet-supported effect is non-destructive.
pub fn add_clip_audio_effect(
	project: &ProjectRef,
	clip: NodeId,
	type_id: &str,
	index: usize,
) -> Result<(), String> {
	let type_id = type_id.trim();
	if type_id.is_empty() {
		return Err("audio effect type id must not be empty".into());
	}
	let effect = ClipAudioEffect::new(type_id);
	update_clip_core(project, clip, "Add Clip Audio Effect", |core| {
		let index = index.min(core.audio_effects.len());
		core.audio_effects.insert(index, effect);
		Ok(true)
	})
}

/// Remove one entry of a clip's audio effect chain as one undoable
/// command.
pub fn remove_clip_audio_effect(
	project: &ProjectRef,
	clip: NodeId,
	index: usize,
) -> Result<(), String> {
	update_clip_core(project, clip, "Remove Clip Audio Effect", |core| {
		if index >= core.audio_effects.len() {
			return Err("audio effect index out of range".into());
		}
		core.audio_effects.remove(index);
		Ok(true)
	})
}

/// Enable/disable one entry of a clip's audio effect chain (disabled
/// effects are bypassed by the mixer).
pub fn set_clip_audio_effect_enabled(
	project: &ProjectRef,
	clip: NodeId,
	index: usize,
	enabled: bool,
) -> Result<(), String> {
	update_clip_core(project, clip, "Set Clip Audio Effect Enabled", |core| {
		let effect = core
			.audio_effects
			.get_mut(index)
			.ok_or_else(|| "audio effect index out of range".to_string())?;
		if effect.enabled == enabled {
			return Ok(false);
		}
		effect.enabled = enabled;
		Ok(true)
	})
}

/// Set one static float parameter of a clip's audio effect.
///
/// Phase 1 boundary: only float parameters are exposed here (Volume
/// `volume_in`, Pan `panning_in`); keyframed/dynamic parameters and
/// non-float inputs remain a Phase-2 follow-up.
pub fn set_clip_audio_effect_param(
	project: &ProjectRef,
	clip: NodeId,
	index: usize,
	input_id: &str,
	value: f64,
) -> Result<(), String> {
	let input_id = input_id.trim();
	if input_id.is_empty() {
		return Err("audio effect input id must not be empty".into());
	}
	if !value.is_finite() {
		return Err("audio effect parameter must be finite".into());
	}
	let input_id = input_id.to_string();
	update_clip_core(project, clip, "Set Clip Audio Effect Param", |core| {
		let effect = core
			.audio_effects
			.get_mut(index)
			.ok_or_else(|| "audio effect index out of range".to_string())?;
		if effect.param(&input_id) == Some(&NodeValue::Float(value)) {
			return Ok(false);
		}
		effect.set_param(&input_id, NodeValue::Float(value));
		Ok(true)
	})
}

/// The Volume effect type id (re-exported for the dispatcher's validation
/// and for SDK-side convenience constants).
pub const VOLUME_AUDIO_EFFECT: &str = AUDIO_EFFECT_VOLUME_TYPE_ID;

/// The Pan effect type id.
pub const PAN_AUDIO_EFFECT: &str = AUDIO_EFFECT_PAN_TYPE_ID;

/// The Volume effect's parameter input id.
pub const VOLUME_AUDIO_EFFECT_INPUT: &str = AUDIO_VOLUME_INPUT;

/// The Pan effect's parameter input id.
pub const PAN_AUDIO_EFFECT_INPUT: &str = AUDIO_PAN_INPUT;
