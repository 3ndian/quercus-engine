//! Safe multicam state and graph-resolution queries.

use std::ffi::c_void;

use crate::node::NodeId;
use crate::types::Rational;

use super::EditorSession;

#[cfg_attr(
	windows,
	link(name = "quercus_engine.dll.lib", kind = "dylib", modifiers = "+verbatim")
)]
#[cfg_attr(not(windows), link(name = "quercus_engine", kind = "dylib"))]
unsafe extern "C" {
	fn quercus_editor_multicam_state(
		session: *const c_void,
		clip: u64,
		out: *mut RawMulticamState,
	) -> bool;
	fn quercus_editor_multicam_connected_sequence(session: *const c_void, clip: u64) -> u64;
	fn quercus_editor_multicam_is_clip(session: *const c_void, clip: u64) -> bool;
	fn quercus_editor_multicam_clip_at(session: *const c_void, sequence: u64, time: Rational) -> u64;
	fn quercus_editor_multicam_sequence(session: *const c_void, multicam: u64) -> u64;
	fn quercus_editor_multicam_source_track(session: *const c_void, multicam: u64, source: i32) -> u64;
	fn quercus_editor_multicam_source_count(session: *const c_void, multicam: u64) -> i32;
	fn quercus_editor_multicam_current_source(session: *const c_void, multicam: u64) -> i32;
	fn quercus_editor_multicam_source_name(
		session: *const c_void,
		multicam: u64,
		source: i32,
		buf: *mut u8,
		buf_len: usize,
	) -> i32;
	fn quercus_editor_multicam_grid(source_count: i32, rows: *mut i32, cols: *mut i32) -> bool;
	fn quercus_editor_multicam_source_at(source_count: i32, x: f64, y: f64) -> i32;
}
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
struct RawMulticamState {
	sequence: u64,
	multicam: u64,
	clip: u64,
	source_count: i32,
	current_source: i32,
}
/// Resolved multicam node, source sequence and current angle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MulticamState {
	/// The `sequence` field.
	pub sequence: NodeId,
	/// The `multicam` field.
	pub multicam: NodeId,
	/// The `clip` field.
	pub clip: NodeId,
	/// The `source_count` field.
	pub source_count: i32,
	/// The `current_source` field.
	pub current_source: i32,
}
/// One multicam angle: index, source-sequence track, display name and
/// selection state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MulticamSource {
	/// Zero-based angle index in the multicam node's source track list.
	pub index: i32,
	/// The angle's track inside the multicam node's source sequence.
	pub track: NodeId,
	/// Display name (`"Video Track N"`, `"Audio Track N"`, ...).
	pub name: String,
	/// Whether this angle is the multicam node's current source.
	pub current: bool,
}
impl EditorSession {
	/// Resolves a clip's multicam state, or `None` when multicam is not enabled.
	pub fn multicam_state(&self, clip: NodeId) -> Option<MulticamState> {
		let mut raw = RawMulticamState::default();
		unsafe { quercus_editor_multicam_state(self.ptr, clip.raw(), &mut raw) }.then(|| {
			MulticamState {
				sequence: NodeId::from_raw(raw.sequence),
				multicam: NodeId::from_raw(raw.multicam),
				clip: NodeId::from_raw(raw.clip),
				source_count: raw.source_count,
				current_source: raw.current_source,
			}
		})
	}

	/// Returns the first sequence feeding the clip's texture chain.
	pub fn multicam_connected_sequence(&self, clip: NodeId) -> Option<NodeId> {
		let id = NodeId::from_raw(unsafe {
			quercus_editor_multicam_connected_sequence(self.ptr, clip.raw())
		});
		id.is_valid().then_some(id)
	}

	/// Whether a clip's direct texture source is a multicam node.
	pub fn multicam_is_clip(&self, clip: NodeId) -> bool {
		unsafe { quercus_editor_multicam_is_clip(self.ptr, clip.raw()) }
	}

	/// Finds the multicam clip covering `time` on a sequence's video tracks.
	pub fn multicam_clip_at(&self, sequence: NodeId, time: Rational) -> Option<NodeId> {
		let id = NodeId::from_raw(unsafe {
			quercus_editor_multicam_clip_at(self.ptr, sequence.raw(), time)
		});
		id.is_valid().then_some(id)
	}

	/// Returns the source sequence connected to a multicam node.
	pub fn multicam_sequence(&self, multicam: NodeId) -> Option<NodeId> {
		let id =
			NodeId::from_raw(unsafe { quercus_editor_multicam_sequence(self.ptr, multicam.raw()) });
		id.is_valid().then_some(id)
	}

	/// Returns one angle's track inside the connected source sequence.
	pub fn multicam_source_track(&self, multicam: NodeId, source: i32) -> Option<NodeId> {
		let id = NodeId::from_raw(unsafe {
			quercus_editor_multicam_source_track(self.ptr, multicam.raw(), source)
		});
		id.is_valid().then_some(id)
	}

	/// Lists a multicam node's angles, skipping entries that cannot be
	/// resolved.
	pub fn multicam_sources(&self, multicam: NodeId) -> Vec<MulticamSource> {
		let Some(count) = self.multicam_source_count(multicam) else {
			return Vec::new();
		};
		let current = self.multicam_current_source(multicam);
		let mut sources = Vec::new();
		for index in 0..count {
			let Some(track) = self.multicam_source_track(multicam, index) else {
				continue;
			};
			let Some(name) = self.multicam_source_name(multicam, index) else {
				continue;
			};
			sources.push(MulticamSource {
				index,
				track,
				name,
				current: current == Some(index),
			});
		}
		sources
	}

	/// Returns a multicam node's angle count, or `None` when it is invalid.
	pub fn multicam_source_count(&self, multicam: NodeId) -> Option<i32> {
		let count = unsafe { quercus_editor_multicam_source_count(self.ptr, multicam.raw()) };
		(count >= 0).then_some(count)
	}

	/// Returns a multicam node's current angle, or `None` when it is invalid.
	pub fn multicam_current_source(&self, multicam: NodeId) -> Option<i32> {
		let current = unsafe { quercus_editor_multicam_current_source(self.ptr, multicam.raw()) };
		(current >= 0).then_some(current)
	}

	/// Angle grid layout `(rows, cols)` for `sources`, or `None` when the
	/// count is not positive.
	pub fn multicam_grid(sources: i32) -> Option<(i32, i32)> {
		let mut rows = 0;
		let mut cols = 0;
		unsafe { quercus_editor_multicam_grid(sources, &mut rows, &mut cols) }
			.then_some((rows, cols))
	}

	/// Hit-tests normalized display coordinates (`0..=1`) against the angle
	/// grid, returning the source index or `None` outside it / on an empty
	/// cell.
	pub fn multicam_source_at(sources: i32, x: f64, y: f64) -> Option<i32> {
		let index = unsafe { quercus_editor_multicam_source_at(sources, x, y) };
		(index >= 0).then_some(index)
	}

	/// Queries an angle's display name through the C ABI.
	fn multicam_source_name(&self, multicam: NodeId, source: i32) -> Option<String> {
		let len = unsafe {
			quercus_editor_multicam_source_name(
				self.ptr,
				multicam.raw(),
				source,
				std::ptr::null_mut(),
				0,
			)
		};
		if len < 0 {
			return None;
		}
		let mut buf = vec![0u8; len as usize + 1];
		let written = unsafe {
			quercus_editor_multicam_source_name(
				self.ptr,
				multicam.raw(),
				source,
				buf.as_mut_ptr(),
				buf.len(),
			)
		};
		if written < 0 {
			return None;
		}
		buf.truncate(written as usize);
		String::from_utf8(buf).ok()
	}
}

#[cfg(test)]
mod tests {
	use crate::edit::EditCommand;
	use crate::editor::{EditorSession, TrackKind};
	use crate::node::NodeId;
	use crate::types::Rational;

	#[test]
	fn multicam_enable_switch_split_afv_and_undo_roundtrip() {
		let _guard = crate::node::UNDO_SERIAL
			.lock()
			.unwrap_or_else(|e| e.into_inner());
		let session = EditorSession::new().expect("session");
		let angles = session.create_sequence("Angles").expect("angles");
		for kind in [TrackKind::Video, TrackKind::Audio] {
			session
				.execute(EditCommand::AddTrack { kind })
				.expect("second angle track");
		}
		let media_a = crate::test_support::make_test_clip_with_audio("multicam_a");
		let media_b = crate::test_support::make_test_clip_with_audio("multicam_b");
		let footage_a = session.project().import_footage(&media_a).expect("angle A");
		let footage_b = session.project().import_footage(&media_b).expect("angle B");
		for (track_index, footage) in [footage_a, footage_b].into_iter().enumerate() {
			for kind in [TrackKind::Video, TrackKind::Audio] {
				session
					.execute(EditCommand::PlaceFootageClip {
						footage,
						kind,
						track_index,
						in_ts: 0,
						out_ts: 25,
						media_in_ts: 0,
					})
					.expect("place angle");
			}
		}

		let host = session.create_sequence("Host").expect("host");
		let host_video = session
			.execute(EditCommand::PlaceNestedSequenceClip {
				source: angles,
				track_index: 0,
				in_ts: 0,
				out_ts: 25,
			})
			.expect("host video")
			.primary
			.expect("host video id");
		let host_audio = session
			.execute(EditCommand::PlaceFootageClip {
				footage: footage_a,
				kind: TrackKind::Audio,
				track_index: 0,
				in_ts: 0,
				out_ts: 25,
				media_in_ts: 0,
			})
			.expect("host audio")
			.primary
			.expect("host audio id");
		session
			.execute(EditCommand::SetClipsLinked {
				clips: vec![host_video, host_audio],
				linked: true,
			})
			.expect("link host audio");
		session
			.execute(EditCommand::SetMulticamEnabled {
				clips: vec![host_video],
				enabled: true,
			})
			.expect("enable multicam");
		let state = session.multicam_state(host_video).expect("multicam state");
		assert_eq!(state.sequence, angles);
		assert_eq!(session.multicam_sequence(state.multicam), Some(angles));
		assert!(session.multicam_source_track(state.multicam, 1).is_some());
		assert!(session.multicam_source_track(state.multicam, 99).is_none());
		assert!(state.source_count >= 2);
		assert_eq!(state.current_source, 0);
		assert_eq!(
			session.multicam_connected_sequence(host_video),
			Some(angles)
		);
		assert!(session.multicam_is_clip(host_video));
		assert_eq!(
			session.multicam_clip_at(host, Rational::new(0, 1)),
			Some(host_video)
		);
		assert_eq!(
			session.clip_media_filename(host_audio).as_deref(),
			Some(media_a.as_str())
		);

		session
			.execute(EditCommand::SwitchMulticam {
				clip: host_video,
				source: 1,
				split_clip: false,
				playhead: Rational::new(0, 1),
			})
			.expect("switch to angle B");
		assert_eq!(
			session.multicam_state(host_video).unwrap().current_source,
			1
		);
		assert_eq!(
			session.clip_media_filename(host_audio).as_deref(),
			Some(media_b.as_str())
		);
		assert!(crate::node::undo());
		assert_eq!(
			session.multicam_state(host_video).unwrap().current_source,
			0
		);
		assert_eq!(
			session.clip_media_filename(host_audio).as_deref(),
			Some(media_a.as_str())
		);
		assert!(crate::node::redo());
		assert_eq!(
			session.multicam_state(host_video).unwrap().current_source,
			1
		);
		assert_eq!(
			session.clip_media_filename(host_audio).as_deref(),
			Some(media_b.as_str())
		);

		session
			.execute(EditCommand::SwitchMulticam {
				clip: host_video,
				source: 0,
				split_clip: true,
				playhead: Rational::new(1, 2),
			})
			.expect("split switch to angle A");
		let host_snapshot = session.snapshot();
		let host_clips: Vec<_> = host_snapshot
			.sequences
			.iter()
			.find(|sequence| sequence.id == host)
			.expect("host snapshot")
			.tracks
			.iter()
			.flat_map(|track| track.clips.iter())
			.collect();
		assert!(host_clips.len() >= 3, "split creates a new video block");
		assert!(host_clips.iter().any(|clip| {
			session
				.multicam_state(clip.id)
				.is_some_and(|state| state.current_source == 0)
		}));
		assert!(crate::node::undo());
		assert_eq!(
			session.multicam_state(host_video).unwrap().current_source,
			1
		);

		session
			.execute(EditCommand::SetMulticamEnabled {
				clips: vec![host_video],
				enabled: false,
			})
			.expect("disable multicam");
		assert!(session.multicam_state(host_video).is_none());
		assert!(crate::node::undo());
		assert!(session.multicam_state(host_video).is_some());

		let _ = std::fs::remove_file(media_a);
		let _ = std::fs::remove_file(media_b);
	}

	#[test]
	fn multicam_wizard_builds_offsets_afv_and_survives_reload() {
		let _guard = crate::node::UNDO_SERIAL
			.lock()
			.unwrap_or_else(|e| e.into_inner());
		let session = EditorSession::new().expect("session");
		let media_a = crate::test_support::make_test_clip_with_audio("wizard_a");
		let media_b = crate::test_support::make_test_clip_with_audio("wizard_b");
		let angle_a = session.project().import_footage(&media_a).expect("angle A");
		let angle_b = session.project().import_footage(&media_b).expect("angle B");
		let host = session.create_sequence("Host").expect("host");
		assert!(
			session
				.execute(EditCommand::CreateMulticamSequence {
					host_sequence: host,
					angles: vec![angle_a],
					offsets: vec![0.0, 1.0],
					name: "Bad".into(),
				})
				.is_err()
		);
		let created = session
			.execute(EditCommand::CreateMulticamSequence {
				host_sequence: host,
				angles: vec![angle_a, angle_b],
				offsets: vec![0.0, 0.5],
				name: "Wizard".into(),
			})
			.expect("create wizard");
		let source = created.primary.expect("source sequence");
		assert_eq!(created.ids.len(), 1, "host clip is returned in ids");
		assert_eq!(session.current_sequence(), Some(host));
		let host_clip = session
			.multicam_clip_at(host, Rational::new(0, 1))
			.expect("host multicam clip");
		assert_eq!(created.ids[0], host_clip);
		let state = session.multicam_state(host_clip).expect("wizard state");
		assert_eq!(state.sequence, source);
		assert_eq!(state.source_count, 2);
		let snapshot = session.snapshot();
		let source_snapshot = snapshot
			.sequences
			.iter()
			.find(|sequence| sequence.id == source)
			.expect("source snapshot");
		let offset_clip = source_snapshot
			.tracks
			.iter()
			.filter(|track| track.kind == Some(TrackKind::Video))
			.flat_map(|track| track.clips.iter())
			.find(|clip| clip.in_time == Rational::new(1001, 2000))
			.expect("angle B offset");
		assert_eq!(offset_clip.out_time, Rational::new(3003, 2000));
		let host_audio = snapshot
			.sequences
			.iter()
			.find(|sequence| sequence.id == host)
			.expect("host snapshot")
			.tracks
			.iter()
			.find(|track| track.kind == Some(TrackKind::Audio))
			.and_then(|track| track.clips.first())
			.expect("host AFV audio")
			.id;
		assert_eq!(
			session.clip_media_filename(host_audio).as_deref(),
			Some(media_a.as_str())
		);

		let path = std::env::temp_dir()
			.join(format!("quercus_sdk_wizard_{}.ove", std::process::id()))
			.to_string_lossy()
			.into_owned();
		session.save(&path).expect("save wizard");
		let opened = EditorSession::open(&path).expect("reload wizard");
		let reloaded_clip = opened
			.multicam_clip_at(host, Rational::new(0, 1))
			.expect("reloaded host clip");
		let reloaded = opened
			.multicam_state(reloaded_clip)
			.expect("reloaded state");
		assert_eq!(reloaded.sequence, source);
		assert_eq!(reloaded.source_count, 2);
		assert_eq!(
			opened.clip_media_filename(host_audio).as_deref(),
			Some(media_a.as_str())
		);

		let _ = std::fs::remove_file(path);
		let _ = std::fs::remove_file(media_a);
		let _ = std::fs::remove_file(media_b);
	}

	#[test]
	fn multicam_grid_matches_square_growth() {
		let expected = [
			(1, 1),
			(1, 2),
			(2, 2),
			(2, 2),
			(2, 3),
			(2, 3),
			(3, 3),
			(3, 3),
			(3, 3),
		];
		for (index, (rows, cols)) in expected.into_iter().enumerate() {
			let sources = index as i32 + 1;
			assert_eq!(EditorSession::multicam_grid(sources), Some((rows, cols)));
			assert_eq!(super::rows_and_columns(sources), (rows, cols));
		}
		assert_eq!(EditorSession::multicam_grid(0), None);
		assert_eq!(EditorSession::multicam_grid(-3), None);
	}

	#[test]
	fn multicam_source_at_hit_tests_grid() {
		assert_eq!(EditorSession::multicam_source_at(1, 0.0, 0.0), Some(0));
		assert_eq!(EditorSession::multicam_source_at(1, 0.999, 0.999), Some(0));
		// 3 sources -> 2x2; index 2 is the bottom-left cell.
		assert_eq!(EditorSession::multicam_source_at(3, 0.25, 0.75), Some(2));
		// 5 sources -> 2 rows x 3 cols; index 4 is the last filled cell.
		assert_eq!(EditorSession::multicam_source_at(5, 0.5, 0.6), Some(4));
		// The trailing cell of that non-square grid is empty.
		assert_eq!(EditorSession::multicam_source_at(5, 0.9, 0.6), None);
		// Outside the normalized range (1.0 maps past the last row/column).
		assert_eq!(EditorSession::multicam_source_at(2, 1.0, 0.0), None);
		assert_eq!(EditorSession::multicam_source_at(2, 2.0, 0.0), None);
		assert_eq!(EditorSession::multicam_source_at(2, 0.0, 1.0), None);
		assert_eq!(EditorSession::multicam_source_at(2, -0.1, 0.0), None);
		// Invalid counts.
		assert_eq!(EditorSession::multicam_source_at(0, 0.5, 0.5), None);
		assert_eq!(EditorSession::multicam_source_at(-1, 0.5, 0.5), None);
	}

	#[test]
	fn multicam_sources_expose_track_names_and_current() {
		let _guard = crate::node::UNDO_SERIAL
			.lock()
			.unwrap_or_else(|e| e.into_inner());
		let session = EditorSession::new().expect("session");
		let media_a = crate::test_support::make_test_clip_with_audio("sources_a");
		let media_b = crate::test_support::make_test_clip_with_audio("sources_b");
		let angle_a = session.project().import_footage(&media_a).expect("angle A");
		let angle_b = session.project().import_footage(&media_b).expect("angle B");
		let host = session.create_sequence("Host").expect("host");
		let source = session
			.execute(EditCommand::CreateMulticamSequence {
				host_sequence: host,
				angles: vec![angle_a, angle_b],
				offsets: vec![0.0, 0.5],
				name: "Sources".into(),
			})
			.expect("create multicam")
			.primary
			.expect("source sequence");
		let host_clip = session
			.multicam_clip_at(host, Rational::new(0, 1))
			.expect("host multicam clip");
		let state = session.multicam_state(host_clip).expect("multicam state");
		assert_eq!(state.sequence, source);
		assert_eq!(
			session.multicam_source_count(state.multicam),
			Some(state.source_count)
		);
		assert_eq!(
			session.multicam_current_source(state.multicam),
			Some(state.current_source)
		);

		let sources = session.multicam_sources(state.multicam);
		assert_eq!(sources.len() as i32, state.source_count);
		assert!(sources.len() >= 2);
		assert!(sources.iter().all(|source| !source.name.is_empty()));
		assert_eq!(sources[0].name, "Video Track 0");
		assert_eq!(sources[1].name, "Video Track 1");
		assert_eq!(sources.iter().filter(|source| source.current).count(), 1);
		for (position, source) in sources.iter().enumerate() {
			assert_eq!(source.index, position as i32);
			assert_eq!(source.current, source.index == state.current_source);
			assert_eq!(
				session.multicam_source_track(state.multicam, source.index),
				Some(source.track)
			);
		}

		assert!(session.multicam_sources(NodeId::INVALID).is_empty());
		assert_eq!(session.multicam_source_count(NodeId::INVALID), None);

		let _ = std::fs::remove_file(media_a);
		let _ = std::fs::remove_file(media_b);
	}
}
/// Square-ish grid layout for `sources` source angles.
pub fn rows_and_columns(sources: i32) -> (i32, i32) {
	let mut rows = 1;
	let mut cols = 1;
	while rows * cols < sources {
		if rows < cols {
			rows += 1;
		} else {
			cols += 1;
		}
	}
	(rows, cols)
}

/// Converts a grid row/column to a source index.
pub fn rows_cols_to_index(row: i32, col: i32, total_rows: i32, total_cols: i32) -> i32 {
	let _ = total_rows;
	col + row * total_cols
}
