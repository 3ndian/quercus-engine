//! SRT import/export ABI for editor sessions.

use super::EditorSessionHandle;
use crate::codec::str_arg;
use crate::editor_commands as ops;
use quercus_node::id::NodeId;

/// Imports an SRT file as Text clips on the current sequence and
/// `track_index`. Returns the cue count, or -1 on a null/invalid session,
/// path or I/O/parse error.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_editor_import_subtitles(
	session: *const EditorSessionHandle,
	path: *const u8,
	path_len: usize,
	track_index: i32,
) -> i32 {
	let (Some(session), Some(path)) = (unsafe { session.as_ref() }, unsafe {
		str_arg(path, path_len)
	}) else {
		return -1;
	};
	if track_index < 0 {
		return -1;
	}
	let (project, sequence) = {
		let state = session.inner.lock().unwrap_or_else(|e| e.into_inner());
		(state.project.clone(), state.current_sequence)
	};
	match ops::import_srt(&project, sequence, track_index as usize, std::path::Path::new(path)) {
		Ok(count) => count as i32,
		Err(_) => -1,
	}
}

/// Exports Text clips on the current sequence's video tracks to an SRT
/// file. Returns the cue count, or -1 on error.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_editor_export_subtitles(
	session: *const EditorSessionHandle,
	path: *const u8,
	path_len: usize,
) -> i32 {
	let (Some(session), Some(path)) = (unsafe { session.as_ref() }, unsafe {
		str_arg(path, path_len)
	}) else {
		return -1;
	};
	let (project, sequence) = {
		let state = session.inner.lock().unwrap_or_else(|e| e.into_inner());
		(state.project.clone(), state.current_sequence)
	};
	match ops::export_srt(&project, sequence, std::path::Path::new(path)) {
		Ok(count) => count as i32,
		Err(_) => -1,
	}
}

/// Sets a clip's audio gain (0..=4, 1 = unity) as one undo entry.
/// Returns false on a null/invalid session, clip or gain.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_editor_set_clip_gain(
	session: *const EditorSessionHandle,
	clip: u64,
	gain: f64,
) -> bool {
	let (Some(session), Some(clip)) =
		(unsafe { session.as_ref() }, NodeId::from_identity(clip))
	else {
		return false;
	};
	let project = session.project();
	crate::editor_commands::set_clip_gain(&project, clip, gain).is_ok()
}
