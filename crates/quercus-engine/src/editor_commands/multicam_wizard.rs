//! Multicam wizard source-graph and host-clip construction.

use super::prelude::*;
use super::{
	moving::set_clips_linked,
	project::{lock, sequence_behavior, track_behavior, track_list_behavior},
	sequence::{
		clip_range, footage_duration_seconds, rational_to_ts, sequence_playhead,
		sequence_time_base, track_ids, track_list_of,
	},
	tracks::{add_track, place_footage_clip},
};
use quercus_node::block::clip_input;
use quercus_node::nodes::multicamnode::{SEQUENCE_INPUT, SOURCES_INPUT};

/// IDs produced by wizard construction. The host remains on its original
/// sequence; only its texture source changes to the new multicam node.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MulticamCreateResult {
	pub source_sequence: NodeId,
	pub host_clip: NodeId,
}

/// Converts seconds to a frame timestamp on the `(num, den)` frame-duration
/// timebase, rounding to the nearest frame. A degenerate pair keeps the
/// historical 25 fps fallback.
fn ts_for_seconds(seconds: f64, time_base: (i64, i64)) -> i64 {
	let fps = if time_base.0 > 0 && time_base.1 > 0 {
		time_base.1 as f64 / time_base.0 as f64
	} else {
		25.0
	};
	(seconds.max(0.0) * fps).round() as i64
}

/// Builds a non-negative range from a second-based offset/duration, snapped
/// to the sequence timebase and at least one frame long.
fn range_for_seconds(offset: f64, duration: f64, time_base: (i64, i64)) -> TimeRange {
	let (num, den) = if time_base.0 > 0 && time_base.1 > 0 {
		time_base
	} else {
		(1, 25)
	};
	let in_ts = ts_for_seconds(offset, time_base).max(0);
	let length_ts = ts_for_seconds(duration, time_base).max(1);
	TimeRange::new(
		Rational::new(in_ts * num, den),
		Rational::new((in_ts + length_ts) * num, den),
	)
}

fn build_multicam_sequence(
	project: &ProjectRef,
	angles: &[NodeId],
	offsets: &[f64],
	name: &str,
	time_base: (i64, i64),
) -> Result<(NodeId, NodeId), String> {
	if angles.is_empty() {
		return Err("no angles selected".into());
	}
	if angles.len() != offsets.len() {
		return Err("angle/offset count mismatch".into());
	}
	let mut guard = lock(project);
	let graph = &mut guard.graph;
	let (_core, behavior) = SequenceBehavior::create();
	let mut core = quercus_node::node::NodeCore::new();
	core.label = name.to_string();
	let sequence = graph.add_node(core, behavior);

	let make_list = |graph: &mut Graph, kind: TrackType| {
		let list = if let Some(list) = track_list_of(graph, sequence, kind) {
			list
		} else {
			let (core, mut behavior) = TrackListBehavior::create();
			if let Some(list) = behavior
				.as_any_mut()
				.and_then(|behavior| behavior.downcast_mut::<TrackListBehavior>())
			{
				list.kind = kind;
				list.array_base = sequence_behavior(graph, sequence)
					.map(|sequence| sequence.track_lists.len())
					.unwrap_or(0) as i32;
			}
			let list = graph.add_node(core, behavior);
			if let Some(sequence_behavior) = graph
				.get_mut(sequence)
				.and_then(|entry| entry.behavior.as_any_mut())
				.and_then(|behavior| behavior.downcast_mut::<SequenceBehavior>())
			{
				sequence_behavior.track_lists.push(list);
			}
			list
		};
		let tracks = track_list_behavior(graph, list)
			.map(|list| list.tracks.clone())
			.unwrap_or_default();
		(list, tracks)
	};
	let (video_list, _) = make_list(graph, TrackType::Video);
	let mut video_tracks = Vec::new();
	for index in 0..angles.len() {
		let (core, behavior) = TrackBehavior::create();
		let track = graph.add_node(core, behavior);
		if let Some(track_behavior) = graph
			.get_mut(track)
			.and_then(|entry| entry.behavior.as_any_mut())
			.and_then(|behavior| behavior.downcast_mut::<TrackBehavior>())
		{
			track_behavior.index = index as i32;
		}
		if let Some(list) = graph
			.get_mut(video_list)
			.and_then(|entry| entry.behavior.as_any_mut())
			.and_then(|behavior| behavior.downcast_mut::<TrackListBehavior>())
		{
			list.tracks.push(track);
		}
		video_tracks.push(track);
	}

	let has_audio = angles
		.iter()
		.any(|&footage| project_footage_has_audio(graph, footage));
	let audio_tracks = if has_audio {
		let (audio_list, _) = make_list(graph, TrackType::Audio);
		let mut tracks = Vec::new();
		for index in 0..angles.len() {
			let (core, behavior) = TrackBehavior::create();
			let track = graph.add_node(core, behavior);
			if let Some(track_behavior) = graph
				.get_mut(track)
				.and_then(|entry| entry.behavior.as_any_mut())
				.and_then(|behavior| behavior.downcast_mut::<TrackBehavior>())
			{
				track_behavior.index = index as i32;
			}
			if let Some(list) = graph
				.get_mut(audio_list)
				.and_then(|entry| entry.behavior.as_any_mut())
				.and_then(|behavior| behavior.downcast_mut::<TrackListBehavior>())
			{
				list.tracks.push(track);
			}
			tracks.push(track);
		}
		tracks
	} else {
		Vec::new()
	};

	let mut angle_clips = Vec::new();
	for (index, &footage) in angles.iter().enumerate() {
		let Some(footage_behavior) = graph
			.get(footage)
			.and_then(|entry| entry.behavior.as_any())
			.and_then(|behavior| behavior.downcast_ref::<FootageBehavior>())
		else {
			return Err(format!("angle {index} is not a footage node"));
		};
		let duration = footage_behavior.duration().to_f64();
		let duration = if duration.is_finite() && duration > 0.0 {
			duration
		} else {
			10.0
		};
		let audio = footage_behavior.audio_stream_count() > 0;
		let range = range_for_seconds(offsets[index], duration, time_base);
		let (core, behavior) = quercus_node::block::clip_create();
		let clip = graph.add_node(core, behavior);
		if let Some(clip_behavior) = graph
			.get_mut(clip)
			.and_then(|entry| entry.behavior.as_any_mut())
			.and_then(|behavior| behavior.downcast_mut::<ClipBlockBehavior>())
		{
			clip_behavior.core.range = range;
		}
		if let Some(track) = graph
			.get_mut(video_tracks[index])
			.and_then(|entry| entry.behavior.as_any_mut())
			.and_then(|behavior| behavior.downcast_mut::<TrackBehavior>())
		{
			track.blocks.push(clip);
		}
		graph
			.connect(footage, clip, clip_input::TEXTURE_INPUT, -1)
			.map_err(|e| format!("connect angle {index}: {e:?}"))?;

		if audio {
			let (core, behavior) = quercus_node::block::clip_create();
			let audio_clip = graph.add_node(core, behavior);
			if let Some(clip_behavior) = graph
				.get_mut(audio_clip)
				.and_then(|entry| entry.behavior.as_any_mut())
				.and_then(|behavior| behavior.downcast_mut::<ClipBlockBehavior>())
			{
				clip_behavior.core.range = range;
			}
			if let Some(track) = graph
				.get_mut(audio_tracks[index])
				.and_then(|entry| entry.behavior.as_any_mut())
				.and_then(|behavior| behavior.downcast_mut::<TrackBehavior>())
			{
				track.blocks.push(audio_clip);
			}
			graph
				.connect(footage, audio_clip, clip_input::TEXTURE_INPUT, -1)
				.map_err(|e| format!("connect angle audio {index}: {e:?}"))?;
			if let Some(entry) = graph.get_mut(audio_clip) {
				entry.core.links.push(clip);
			}
			if let Some(entry) = graph.get_mut(clip) {
				entry.core.links.push(audio_clip);
			}
		}
		angle_clips.push(clip);
	}

	let (core, behavior) = quercus_node::nodes::multicamnode::create();
	let multicam = graph.add_node(core, behavior);
	graph
		.connect(sequence, multicam, SEQUENCE_INPUT, -1)
		.map_err(|e| format!("connect multicam sequence: {e:?}"))?;
	for (index, clip) in angle_clips.into_iter().enumerate() {
		graph
			.input_array_insert(multicam, SOURCES_INPUT, index as i32)
			.map_err(|e| format!("multicam source slot {index}: {e:?}"))?;
		graph
			.connect(clip, multicam, SOURCES_INPUT, index as i32)
			.map_err(|e| format!("connect multicam source {index}: {e:?}"))?;
	}
	Ok((sequence, multicam))
}

fn project_footage_has_audio(graph: &Graph, footage: NodeId) -> bool {
	graph
		.get(footage)
		.and_then(|entry| entry.behavior.as_any())
		.and_then(|behavior| behavior.downcast_ref::<FootageBehavior>())
		.is_some_and(|footage| footage.audio_stream_count() > 0)
}

fn host_clip_at_or_first(graph: &Graph, sequence: NodeId, playhead: Rational) -> Option<NodeId> {
	let tracks = track_ids(graph, sequence, TrackType::Video);
	for track in tracks.iter().rev() {
		for &block in &track_behavior(graph, *track)?.blocks {
			if let Some(clip) = clip_range(graph, block) {
				if playhead >= clip.0 && playhead < clip.1 {
					return Some(block);
				}
			}
		}
	}
	tracks.into_iter().find_map(|track| {
		track_behavior(graph, track).and_then(|track| track.blocks.first().copied())
	})
}

/// Builds the source sequence, mounts it, resolves/places a host clip, rewires
/// it to the multicam node, and places linked AFV audio when angle 0 has audio.
pub fn create_multicam_sequence(
	project: &ProjectRef,
	host_sequence: NodeId,
	angles: &[NodeId],
	offsets: &[f64],
	name: &str,
) -> Result<MulticamCreateResult, String> {
	if sequence_behavior(&lock(project).graph, host_sequence).is_none() {
		return Err("host sequence is not in the project".into());
	}
	let name = if name.trim().is_empty() {
		"Multi-Cam"
	} else {
		name.trim()
	};
	let time_base = sequence_time_base(&lock(project).graph, host_sequence).unwrap_or((1, 25));
	let (source_sequence, multicam) =
		build_multicam_sequence(project, angles, offsets, name, time_base)?;
	let root = lock(project).root;
	quercus_task::nodeops::folder_add_child_command(
		(project.clone(), root),
		(project.clone(), source_sequence),
	)
	.redo_now();

	let playhead = sequence_playhead(&lock(project).graph, host_sequence);
	let existing_host = {
		let guard = lock(project);
		host_clip_at_or_first(&guard.graph, host_sequence, playhead)
	};
	let host_clip = match existing_host {
		Some(clip) => clip,
		None => {
			let footage = angles[0];
			let duration = footage_duration_seconds(&lock(project).graph, footage).unwrap_or(10.0);
			if track_ids(&lock(project).graph, host_sequence, TrackType::Video).is_empty() {
				add_track(project, host_sequence, TrackType::Video)?;
			}
			let in_ts = rational_to_ts(playhead, time_base).max(0);
			let length = ts_for_seconds(duration, time_base).max(1);
			place_footage_clip(
				project,
				host_sequence,
				footage,
				TrackType::Video,
				0,
				in_ts,
				in_ts + length,
				0,
			)?
		}
	};
	{
		let mut guard = lock(project);
		for (from, input, element) in guard.graph.input_connections(host_clip) {
			if input == clip_input::TEXTURE_INPUT {
				guard.graph.disconnect(from, host_clip, &input, element);
			}
		}
		guard
			.graph
			.connect(multicam, host_clip, clip_input::TEXTURE_INPUT, -1)
			.map_err(|e| format!("connect multicam to host clip: {e:?}"))?;
	}

	if project_footage_has_audio(&lock(project).graph, angles[0]) {
		if track_ids(&lock(project).graph, host_sequence, TrackType::Audio).is_empty() {
			add_track(project, host_sequence, TrackType::Audio)?;
		}
		let (in_ts, out_ts) = {
			let guard = lock(project);
			let range = clip_range(&guard.graph, host_clip).ok_or("host clip has no range")?;
			let time_base = sequence_time_base(&guard.graph, host_sequence).unwrap_or((1, 25));
			(
				rational_to_ts(range.0, time_base),
				rational_to_ts(range.1, time_base),
			)
		};
		let audio_clip = place_footage_clip(
			project,
			host_sequence,
			angles[0],
			TrackType::Audio,
			0,
			in_ts,
			out_ts,
			0,
		)?;
		set_clips_linked(project, &[host_clip, audio_clip], true)?;
	}
	Ok(MulticamCreateResult {
		source_sequence,
		host_clip,
	})
}
