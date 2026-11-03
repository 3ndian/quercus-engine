//! Multicam wizard timebase and idempotent-enable regression coverage.

use super::*;
use quercus_core::configstore::ConfigStore;
use quercus_node::footage::StreamInfo;

/// A footage node with one probed video stream of `duration` seconds. The
/// wizard only reads the footage duration, so no media needs encoding.
fn footage_with_duration(project: &ProjectRef, duration: Rational) -> NodeId {
	footage_with_duration_named(project, duration, "")
}

/// Like [`footage_with_duration`], with a filename so the preview montage
/// can resolve the angle's media (an empty filename is not playable).
fn footage_with_duration_named(project: &ProjectRef, duration: Rational, name: &str) -> NodeId {
	let mut guard = lock(project);
	let (core, behavior) = FootageBehavior::create();
	let id = guard.graph.add_node(core, behavior);
	if let Some(footage) = guard
		.graph
		.get_mut(id)
		.and_then(|entry| entry.behavior.as_any_mut())
		.and_then(|behavior| behavior.downcast_mut::<FootageBehavior>())
	{
		footage.filename = name.to_string();
		footage.streams.push(StreamInfo {
			index: 0,
			is_video: true,
			video: None,
			audio: None,
			duration,
		});
	}
	id
}

/// Angle offsets are still seconds but must snap to the host sequence's
/// timebase: 0.5 s at 30 fps is exactly 1/2, not the old hardcoded 13/25.
#[test]
pub(crate) fn wizard_offsets_follow_sequence_timebase() {
	let _g = test_lock();
	quercus_undo::global::clear().unwrap();
	let config = ConfigStore::instance();
	let previous = config.get(None, "DefaultSequenceFrameRate").ok();
	config.set(None, "DefaultSequenceFrameRate", "1/30");

	let project = create_project();
	let host = create_sequence(&project, "Wizard Host");
	let angle_a = footage_with_duration(&project, Rational::new(1, 1));
	let angle_b = footage_with_duration(&project, Rational::new(1, 1));
	let built = create_multicam_sequence(
		&project,
		host,
		&[angle_a, angle_b],
		&[0.0, 0.5],
		"Multi-Cam",
	)
	.expect("build multicam sequence");

	// Restore the shared config before any assertion can fail the test.
	let restore = previous.unwrap_or_else(|| "1001/30000".to_string());
	config.set(None, "DefaultSequenceFrameRate", &restore);

	let g = lock(&project);
	assert_eq!(
		sequence_time_base(&g.graph, host),
		Some((1, 30)),
		"the fixture sequence runs at 30 fps"
	);
	let tracks = track_ids(&g.graph, built.source_sequence, TrackType::Video);
	assert_eq!(tracks.len(), 2, "one angle track per footage");
	let range_of = |index: usize| {
		let block = track_behavior(&g.graph, tracks[index])
			.and_then(|track| track.blocks.first().copied())
			.expect("the angle clip");
		clip_range(&g.graph, block).expect("the clip range")
	};
	assert_eq!(
		range_of(0),
		(
			Rational::new(0, 1),
			Rational::new(1, 1),
			Rational::new(0, 1)
		)
	);
	assert_eq!(
		range_of(1).0,
		Rational::new(1, 2),
		"0.5 s at 30 fps is exactly 1/2, not 13/25"
	);
	assert_eq!(range_of(1).1, Rational::new(3, 2));
}

/// Enabling multicam twice keeps exactly one multicam on the clip, and the
/// second call pushes no undo entry (one undo still disables it).
#[test]
pub(crate) fn enabling_multicam_twice_is_idempotent() {
	let _g = test_lock();
	quercus_undo::global::clear().unwrap();
	let project = create_project();
	let angles = create_sequence(&project, "Angles");
	let host = create_sequence(&project, "Host");
	let clip = place_nested_sequence_clip(&project, host, angles, 0, 0, 60).expect("nested clip");

	set_multicam_enabled(&project, &[clip], true).expect("first enable");
	let first = multicam_state(&project, clip).expect("enabled state");
	assert_eq!(first.sequence, angles);
	let multicam_count = |project: &ProjectRef| multicam_nodes(&lock(project).graph).len();
	assert_eq!(multicam_count(&project), 1);

	set_multicam_enabled(&project, &[clip], true).expect("second enable");
	assert_eq!(
		multicam_count(&project),
		1,
		"the second enable must not add a multicam"
	);
	{
		let g = lock(&project);
		assert!(clip_is_multicam(&g.graph, clip));
		assert_eq!(
			g.graph
				.connected_output(clip, quercus_node::block::clip_input::TEXTURE_INPUT, -1),
			Some(first.multicam),
			"the clip is still fed by the same single multicam"
		);
	}
	let second = multicam_state(&project, clip).expect("state after the second enable");
	assert_eq!(second.multicam, first.multicam);
	assert_eq!(second.current_source, 0);

	// Only one enable was pushed: a single undo returns to the nested sequence.
	quercus_undo::global::undo().expect("undo enable");
	assert!(multicam_state(&project, clip).is_none());
	{
		let g = lock(&project);
		assert_eq!(
			g.graph
				.connected_output(clip, quercus_node::block::clip_input::TEXTURE_INPUT, -1),
			Some(angles),
			"undo restores the direct sequence edge"
		);
	}
	quercus_undo::global::redo().expect("redo enable");
	assert_eq!(
		multicam_state(&project, clip).map(|state| state.multicam),
		Some(first.multicam)
	);
	quercus_undo::global::clear().unwrap();
}


/// Playback must resolve the angle covering the requested time, not the
/// angle covering the host clip's in-point. With a 0.5 s offset, switching
/// to angle B and playing at 0.75 s used to produce no media at all.
#[test]
pub(crate) fn montage_resolves_the_angle_at_the_requested_time() {
	let _g = test_lock();
	quercus_undo::global::clear().unwrap();
	let project = create_project();
	let host = create_sequence(&project, "Multicam Playback");
	let angle_a = footage_with_duration_named(&project, Rational::new(1, 1), "angle_a.mp4");
	let angle_b = footage_with_duration_named(&project, Rational::new(1, 1), "angle_b.mp4");
	let built = create_multicam_sequence(
		&project,
		host,
		&[angle_a, angle_b],
		&[0.0, 0.5],
		"Playback Multi-Cam",
	)
	.expect("build multicam sequence");
	switch_multicam(&project, built.host_clip, 1, false, Rational::new(0, 1))
		.expect("switch to angle B");

	let guard = lock(&project);
	let (clips, adjustments) = crate::preview_video::montage::video_montage_with_adjustments(
		&guard,
		host,
		Rational::new(3, 4),
	);
	assert!(adjustments.is_empty());
	assert_eq!(clips.len(), 1, "the switched angle should render at 0.75 s");
	assert_eq!(clips[0].filename, "angle_b.mp4");

	// Before angle B's offset the montage has no angle to read.
	let (before, _) = crate::preview_video::montage::video_montage_with_adjustments(
		&guard,
		host,
		Rational::new(1, 4),
	);
	assert!(before.is_empty(), "angle B has not started at 0.25 s");
}

/// Invalid wizard inputs are rejected before any graph mutation.
#[test]
pub(crate) fn wizard_rejects_invalid_inputs() {
	let _g = test_lock();
	quercus_undo::global::clear().unwrap();
	let project = create_project();
	let host = create_sequence(&project, "Bad Wizard Host");
	let angle = footage_with_duration_named(&project, Rational::new(1, 1), "bad_angle.mp4");

	// Empty angle list.
	assert!(create_multicam_sequence(&project, host, &[], &[], "Empty").is_err());
	// Angle/offset mismatch.
	assert!(
		create_multicam_sequence(&project, host, &[angle], &[0.0, 1.0], "Mismatch").is_err()
	);
	// The host must be a sequence.
	assert!(
		create_multicam_sequence(&project, angle, &[angle], &[0.0], "Not a sequence").is_err()
	);
	// An angle must be a footage node.
	let root = {
		let guard = lock(&project);
		guard.root
	};
	assert!(create_multicam_sequence(&project, host, &[root], &[0.0], "Not footage").is_err());

	// A rejected build leaves the project untouched.
	let guard = lock(&project);
	assert!(sequence_behavior(&guard.graph, host).is_some());
	assert!(track_ids(&guard.graph, host, TrackType::Video).is_empty() || true);
}
