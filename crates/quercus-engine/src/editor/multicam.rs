//! Read-only multicam graph queries for editor sessions.

use quercus_node::graph::Graph;
use quercus_node::id::NodeId;
use quercus_node::nodes::multicamnode::{CURRENT_INPUT, MultiCamNode, SEQUENCE_TYPE_INPUT};
use quercus_node::track::TrackType;

use super::EditorSessionHandle;
use crate::codec::copy_str_out;
use crate::coretypes::QuercusRational;
use crate::editor_commands as ops;
use crate::handle::NativeMirror;
use crate::node::QUERCUS_NODE_ID_INVALID;

/// ABI mirror of the semantic multicam state returned by editor commands.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct QuercusEditorMulticamState {
	pub sequence: u64,
	pub multicam: u64,
	pub clip: u64,
	pub source_count: i32,
	pub current_source: i32,
}

impl NativeMirror for QuercusEditorMulticamState {
	type Native = ops::MulticamState;

	fn from_native(native: &Self::Native) -> Self {
		Self {
			sequence: native.sequence.identity(),
			multicam: native.multicam.identity(),
			clip: native.clip.identity(),
			source_count: native.source_count,
			current_source: native.current_source,
		}
	}

	fn to_native(&self) -> Self::Native {
		Self::Native {
			sequence: NodeId::from_identity(self.sequence).unwrap_or(NodeId::INVALID),
			multicam: NodeId::from_identity(self.multicam).unwrap_or(NodeId::INVALID),
			clip: NodeId::from_identity(self.clip).unwrap_or(NodeId::INVALID),
			source_count: self.source_count,
			current_source: self.current_source,
		}
	}
}

/// Resolves a clip's multicam state; false when it is not multicam-enabled.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_editor_multicam_state(
	session: *const EditorSessionHandle,
	clip: u64,
	out: *mut QuercusEditorMulticamState,
) -> bool {
	let (Some(session), Some(clip), Some(out)) = (
		unsafe { session.as_ref() },
		NodeId::from_identity(clip),
		unsafe { out.as_mut() },
	) else {
		return false;
	};
	let project = session.project();
	let Some(state) = ops::multicam_state(&project, clip) else {
		return false;
	};
	*out = QuercusEditorMulticamState::from_native(&state);
	true
}

/// Returns the first sequence feeding a clip, or the invalid-node sentinel.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_editor_multicam_connected_sequence(
	session: *const EditorSessionHandle,
	clip: u64,
) -> u64 {
	let (Some(session), Some(clip)) = (unsafe { session.as_ref() }, NodeId::from_identity(clip))
	else {
		return QUERCUS_NODE_ID_INVALID;
	};
	let project = session.project();
	let guard = project.lock().unwrap_or_else(|e| e.into_inner());
	ops::clip_connected_sequence(&guard.graph, clip)
		.map(|sequence| sequence.identity())
		.unwrap_or(QUERCUS_NODE_ID_INVALID)
}

/// Whether a clip's direct texture source is a multicam node.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_editor_multicam_is_clip(
	session: *const EditorSessionHandle,
	clip: u64,
) -> bool {
	let (Some(session), Some(clip)) = (unsafe { session.as_ref() }, NodeId::from_identity(clip))
	else {
		return false;
	};
	let project = session.project();
	let guard = project.lock().unwrap_or_else(|e| e.into_inner());
	ops::clip_is_multicam(&guard.graph, clip)
}

/// Finds the multicam clip covering `time` on a sequence's video tracks.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_editor_multicam_clip_at(
	session: *const EditorSessionHandle,
	sequence: u64,
	time: QuercusRational,
) -> u64 {
	let (Some(session), Some(sequence)) =
		(unsafe { session.as_ref() }, NodeId::from_identity(sequence))
	else {
		return QUERCUS_NODE_ID_INVALID;
	};
	let project = session.project();
	ops::multicam_clip_at(&project, sequence, time.to_native())
		.map(|clip| clip.identity())
		.unwrap_or(QUERCUS_NODE_ID_INVALID)
}

/// Returns a multicam node's connected source sequence.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_editor_multicam_sequence(
	session: *const EditorSessionHandle,
	multicam: u64,
) -> u64 {
	let (Some(session), Some(multicam)) =
		(unsafe { session.as_ref() }, NodeId::from_identity(multicam))
	else {
		return QUERCUS_NODE_ID_INVALID;
	};
	let project = session.project();
	let guard = project.lock().unwrap_or_else(|e| e.into_inner());
	ops::multicam_sequence(&guard.graph, multicam)
		.map(|sequence| sequence.identity())
		.unwrap_or(QUERCUS_NODE_ID_INVALID)
}

/// Returns one angle's track inside the connected source sequence.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_editor_multicam_source_track(
	session: *const EditorSessionHandle,
	multicam: u64,
	source: i32,
) -> u64 {
	let (Some(session), Some(multicam)) =
		(unsafe { session.as_ref() }, NodeId::from_identity(multicam))
	else {
		return QUERCUS_NODE_ID_INVALID;
	};
	let project = session.project();
	ops::multicam_source_track(&project, multicam, source)
		.map(|track| track.identity())
		.unwrap_or(QUERCUS_NODE_ID_INVALID)
}

/// Reads the multicam node's source track type (`sequence_type_in`, default
/// video), mirroring `editor_commands::multicam`.
fn source_track_type(graph: &Graph, multicam: NodeId) -> TrackType {
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

/// Number of tracks in the multicam node's connected source track list.
fn source_count(graph: &Graph, multicam: NodeId) -> i32 {
	let Some(sequence) = ops::multicam_sequence(graph, multicam) else {
		return 0;
	};
	ops::track_list_of(graph, sequence, source_track_type(graph, multicam))
		.and_then(|list| ops::track_list_behavior(graph, list))
		.map(|list| list.tracks.len() as i32)
		.unwrap_or(0)
}

/// Reads the multicam node's `current_in` standard value.
fn current_source(graph: &Graph, multicam: NodeId) -> i32 {
	graph
		.get(multicam)
		.map(|entry| entry.core.standard_value(CURRENT_INPUT, -1).to_double() as i32)
		.unwrap_or(-1)
}

/// Number of angles for a multicam node, or -1 when the node is invalid.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_editor_multicam_source_count(
	session: *const EditorSessionHandle,
	multicam: u64,
) -> i32 {
	let (Some(session), Some(multicam)) =
		(unsafe { session.as_ref() }, NodeId::from_identity(multicam))
	else {
		return -1;
	};
	let project = session.project();
	let guard = project.lock().unwrap_or_else(|e| e.into_inner());
	if guard.graph.get(multicam).is_none() {
		return -1;
	}
	source_count(&guard.graph, multicam)
}

/// Current angle of a multicam node, or -1 when the node is invalid.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_editor_multicam_current_source(
	session: *const EditorSessionHandle,
	multicam: u64,
) -> i32 {
	let (Some(session), Some(multicam)) =
		(unsafe { session.as_ref() }, NodeId::from_identity(multicam))
	else {
		return -1;
	};
	let project = session.project();
	let guard = project.lock().unwrap_or_else(|e| e.into_inner());
	if guard.graph.get(multicam).is_none() {
		return -1;
	}
	current_source(&guard.graph, multicam)
}

/// Copies one angle's display name (`"Video Track N"` by default), or -1 for
/// an invalid node/source.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_editor_multicam_source_name(
	session: *const EditorSessionHandle,
	multicam: u64,
	source: i32,
	buf: *mut u8,
	buf_len: usize,
) -> i32 {
	let (Some(session), Some(multicam)) =
		(unsafe { session.as_ref() }, NodeId::from_identity(multicam))
	else {
		return -1;
	};
	if source < 0 {
		return -1;
	}
	let project = session.project();
	let Some(track) = ops::multicam_source_track(&project, multicam, source) else {
		return -1;
	};
	let guard = project.lock().unwrap_or_else(|e| e.into_inner());
	let Some(track_behavior) = ops::track_behavior(&guard.graph, track) else {
		return -1;
	};
	let index = if track_behavior.index >= 0 {
		track_behavior.index
	} else {
		source
	};
	let name = match track_behavior.kind {
		TrackType::Video => format!("Video Track {index}"),
		TrackType::Audio => format!("Audio Track {index}"),
		TrackType::Subtitle => format!("Subtitle Track {index}"),
	};
	copy_str_out(&name, buf, buf_len)
}

/// Computes the square-ish angle grid for `source_count`; false for a
/// non-positive count or null outputs (nothing is written).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_editor_multicam_grid(
	source_count: i32,
	rows: *mut i32,
	cols: *mut i32,
) -> bool {
	let (Some(rows), Some(cols)) = (unsafe { rows.as_mut() }, unsafe { cols.as_mut() }) else {
		return false;
	};
	if source_count <= 0 {
		return false;
	}
	let (row_count, col_count) = MultiCamNode::rows_and_columns(source_count);
	*rows = row_count;
	*cols = col_count;
	true
}

/// Hit-tests normalized display coordinates against the angle grid (C++
/// `MulticamDisplay::DisplayClicked`); -1 outside the grid or an empty cell.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_editor_multicam_source_at(
	source_count: i32,
	x: f64,
	y: f64,
) -> i32 {
	if source_count <= 0 || !(0.0..=1.0).contains(&x) || !(0.0..=1.0).contains(&y) {
		return -1;
	}
	let (rows, cols) = MultiCamNode::rows_and_columns(source_count);
	let multi = rows.max(cols);
	let col = (x * f64::from(multi)).floor() as i32;
	let row = (y * f64::from(multi)).floor() as i32;
	if col >= cols || row >= rows {
		return -1;
	}
	let index = MultiCamNode::rows_cols_to_index(row, col, rows, cols);
	if index >= source_count {
		return -1;
	}
	index
}

#[cfg(test)]
mod tests {
	use super::*;

	/// The C entry points must reject null/invalid handles before touching the
	/// project; these are the defensive branches every UI caller relies on.
	#[test]
	fn null_and_invalid_inputs_are_rejected() {
		let session = crate::editor::quercus_editor_new();
		assert!(!session.is_null());
		let mut state = QuercusEditorMulticamState::default();
		unsafe {
			assert!(!quercus_editor_multicam_state(
				std::ptr::null(),
				NodeId::INVALID.identity(),
				&mut state
			));
			assert!(!quercus_editor_multicam_state(session, u64::MAX, &mut state));
			assert!(!quercus_editor_multicam_state(
				session,
				u64::MAX,
				std::ptr::null_mut()
			));
			assert_eq!(
				quercus_editor_multicam_connected_sequence(std::ptr::null(), 0),
				QUERCUS_NODE_ID_INVALID
			);
			assert_eq!(
				quercus_editor_multicam_connected_sequence(session, u64::MAX),
				QUERCUS_NODE_ID_INVALID
			);
			assert!(!quercus_editor_multicam_is_clip(std::ptr::null(), 0));
			assert!(!quercus_editor_multicam_is_clip(session, u64::MAX));
			assert_eq!(
				quercus_editor_multicam_clip_at(std::ptr::null(), 0, QuercusRational::default()),
				QUERCUS_NODE_ID_INVALID
			);
			assert_eq!(
				quercus_editor_multicam_clip_at(session, u64::MAX, QuercusRational::default()),
				QUERCUS_NODE_ID_INVALID
			);
			assert_eq!(
				quercus_editor_multicam_sequence(std::ptr::null(), 0),
				QUERCUS_NODE_ID_INVALID
			);
			assert_eq!(
				quercus_editor_multicam_sequence(session, u64::MAX),
				QUERCUS_NODE_ID_INVALID
			);
			assert_eq!(
				quercus_editor_multicam_source_track(std::ptr::null(), 0, 0),
				QUERCUS_NODE_ID_INVALID
			);
			assert_eq!(
				quercus_editor_multicam_source_track(session, u64::MAX, 0),
				QUERCUS_NODE_ID_INVALID
			);
			assert_eq!(quercus_editor_multicam_source_count(std::ptr::null(), 0), -1);
			assert_eq!(quercus_editor_multicam_source_count(session, u64::MAX), -1);
			assert_eq!(
				quercus_editor_multicam_current_source(std::ptr::null(), 0),
				-1
			);
			assert_eq!(quercus_editor_multicam_current_source(session, u64::MAX), -1);
			assert_eq!(
				quercus_editor_multicam_source_name(std::ptr::null(), 0, 0, std::ptr::null_mut(), 0),
				-1
			);
			assert_eq!(
				quercus_editor_multicam_source_name(session, u64::MAX, -1, std::ptr::null_mut(), 0),
				-1
			);
			assert_eq!(
				quercus_editor_multicam_source_name(session, u64::MAX, 0, std::ptr::null_mut(), 0),
				-1
			);
			let mut rows = 0;
			let mut cols = 0;
			assert!(!quercus_editor_multicam_grid(0, &mut rows, &mut cols));
			assert!(!quercus_editor_multicam_grid(3, std::ptr::null_mut(), &mut cols));
			assert!(!quercus_editor_multicam_grid(3, &mut rows, std::ptr::null_mut()));
			assert!(!quercus_editor_multicam_grid(3, std::ptr::null_mut(), std::ptr::null_mut()));
			assert_eq!(quercus_editor_multicam_source_at(0, 0.0, 0.0), -1);
			assert_eq!(quercus_editor_multicam_source_at(3, -0.1, 0.0), -1);
			assert_eq!(quercus_editor_multicam_source_at(3, 0.0, 1.1), -1);
			assert_eq!(quercus_editor_multicam_source_at(3, 0.99, 0.99), -1);
			assert_eq!(quercus_editor_multicam_source_at(3, f64::NAN, 0.0), -1);
			crate::editor::quercus_editor_release(session);
		}
	}
	/// Exercise the valid-session early returns: a non-multicam node, a
	/// multicam node without a sequence, and a valid id absent from the graph.
	#[test]
	fn valid_session_invalid_graph_paths() {
		let session = crate::editor::quercus_editor_new();
		assert!(!session.is_null());
		let project = unsafe { &*session }.project();
		let (multicam, plain_clip, missing) = {
			let mut guard = project.lock().unwrap();
			let (core, behavior) = quercus_node::nodes::multicamnode::create();
			let multicam = guard.graph.add_node(core, behavior);
			let (core, behavior) = quercus_node::block::clip_create();
			let plain_clip = guard.graph.add_node(core, behavior);
			let missing = NodeId::from_identity(999_999).unwrap();
			(multicam, plain_clip, missing)
		};
		let mut state = QuercusEditorMulticamState::default();
		unsafe {
			// Valid session + non-multicam clip: multicam_state returns false.
			assert!(!quercus_editor_multicam_state(
				session,
				plain_clip.identity(),
				&mut state
			));
			// Valid session + valid clip but null output: false before touching it.
			assert!(!quercus_editor_multicam_state(
				session,
				plain_clip.identity(),
				std::ptr::null_mut()
			));
			// Valid multicam without sequence: count 0, current 0, name -1.
			assert_eq!(
				quercus_editor_multicam_source_count(session, multicam.identity()),
				0
			);
			assert_eq!(
				quercus_editor_multicam_current_source(session, multicam.identity()),
				0
			);
			assert_eq!(
				quercus_editor_multicam_source_name(
					session,
					multicam.identity(),
					0,
					std::ptr::null_mut(),
					0
				),
				-1
			);
			// Valid identity that is no longer in the graph: early -1 path.
			assert_eq!(
				quercus_editor_multicam_source_count(session, missing.identity()),
				-1
			);
			assert_eq!(
				quercus_editor_multicam_current_source(session, missing.identity()),
				-1
			);
			assert_eq!(
				quercus_editor_multicam_source_name(
					session,
					missing.identity(),
					0,
					std::ptr::null_mut(),
					0
				),
				-1
			);
			// 1x2 grid: a point below the first row is outside the grid.
			assert_eq!(quercus_editor_multicam_source_at(2, 0.25, 0.75), -1);
			// Grid outputs for a valid count.
			let mut rows = 0;
			let mut cols = 0;
			assert!(quercus_editor_multicam_grid(2, &mut rows, &mut cols));
			assert_eq!((rows, cols), (1, 2));
			crate::editor::quercus_editor_release(session);
		}
	}

}
