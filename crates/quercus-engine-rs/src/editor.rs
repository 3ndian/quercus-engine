//! Safe, high-level editor sessions and owned read snapshots.

use std::ffi::c_void;

use crate::codec::string_out;
use crate::node::{NodeId, Project};
use crate::storage::StorageError;
use crate::types::{Rational, TimeRange};

/// Owned C-compatible storage project handle used only for same-instance
/// migration. Layout matches `quercus_core::handle::CHandle` exactly.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct StorageProjectHandle {
	/// Opaque refcounted project box.
	pub ctx: *mut c_void,
	/// Atomic increment supplied by the storage backend.
	pub addref: Option<unsafe extern "C" fn(*mut c_void)>,
	/// Atomic decrement supplied by the storage backend.
	pub release: Option<unsafe extern "C" fn(*mut c_void)>,
	/// Storage handle ABI version.
	pub abi_version: u32,
}

#[cfg_attr(
	windows,
	link(name = "quercus_engine.dll.lib", kind = "dylib", modifiers = "+verbatim")
)]
#[cfg_attr(not(windows), link(name = "quercus_engine", kind = "dylib"))]
unsafe extern "C" {
	fn quercus_editor_add_ref(session: *const c_void);
	fn quercus_editor_release(session: *const c_void);
	fn quercus_editor_new() -> *mut c_void;
	fn quercus_editor_open(uri: *const u8, uri_len: usize, version_info: *mut i32) -> *mut c_void;
	fn quercus_editor_open_project_from_library(uuid: *const u8, uuid_len: usize, error: *mut i32) -> *mut c_void;
	fn quercus_editor_open_project_from_library_uri(
		library_uri: *const u8,
		library_uri_len: usize,
		uuid: *const u8,
		uuid_len: usize,
		error: *mut i32,
	) -> *mut c_void;
	fn quercus_editor_adopt_storage_project_handle(project: StorageProjectHandle) -> *mut c_void;
	fn quercus_editor_from_project_handle(project: *mut c_void) -> *mut c_void;
	fn quercus_editor_create_library(name: *const u8, name_len: usize, error: *mut i32) -> *mut c_void;
	fn quercus_editor_project(session: *const c_void) -> *mut c_void;
	fn quercus_editor_save(
		session: *const c_void,
		path: *const u8,
		path_len: usize,
		options: u32,
	) -> i32;
	fn quercus_editor_bind_project_to_library(session: *mut c_void) -> bool;
	fn quercus_editor_unbind(session: *mut c_void) -> bool;
	fn quercus_editor_storage_bound(session: *const c_void) -> bool;
	fn quercus_editor_storage_last_error(session: *const c_void, buf: *mut u8, buf_len: usize) -> i32;
	fn quercus_editor_set_current_sequence(session: *mut c_void, id: u64) -> bool;
	fn quercus_editor_current_sequence(session: *const c_void) -> u64;
	fn quercus_editor_create_sequence(session: *mut c_void, name: *const u8, name_len: usize) -> u64;
	fn quercus_editor_marker_count(session: *const c_void) -> i32;
	fn quercus_editor_marker_at(session: *const c_void, index: i32, out: *mut EditorMarkerRaw) -> bool;
	fn quercus_editor_marker_name_at(
		session: *const c_void,
		index: i32,
		buf: *mut u8,
		buf_len: usize,
	) -> i32;
	fn quercus_editor_workarea(session: *const c_void, out: *mut TimeRange) -> bool;
	fn quercus_editor_import_subtitles(
		session: *const c_void,
		path: *const u8,
		path_len: usize,
		track_index: i32,
	) -> i32;
	fn quercus_editor_export_subtitles(
		session: *const c_void,
		path: *const u8,
		path_len: usize,
	) -> i32;
	fn quercus_editor_set_clip_gain(session: *const c_void, clip: u64, gain: f64) -> bool;
	fn quercus_editor_set_clip_pan(session: *const c_void, clip: u64, pan: f64) -> bool;
	fn quercus_editor_set_clip_fade(
		session: *const c_void,
		clip: u64,
		fade_in_num: i64,
		fade_in_den: i64,
		fade_out_num: i64,
		fade_out_den: i64,
	) -> bool;
	fn quercus_editor_add_clip_audio_effect(
		session: *const c_void,
		clip: u64,
		type_id: *const u8,
		type_id_len: usize,
		index: i32,
	) -> bool;
	fn quercus_editor_remove_clip_audio_effect(session: *const c_void, clip: u64, index: i32) -> bool;
	fn quercus_editor_set_clip_audio_effect_enabled(
		session: *const c_void,
		clip: u64,
		index: i32,
		enabled: bool,
	) -> bool;
	fn quercus_editor_set_clip_audio_effect_param(
		session: *const c_void,
		clip: u64,
		index: i32,
		input_id: *const u8,
		input_id_len: usize,
		value: f64,
	) -> bool;
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
struct EditorMarkerRaw {
	time: TimeRange,
	color: i32,
}

/// One live editor session. Drop releases the session and unbinds its
/// project from write-through storage. `project()` returns an
/// independently refcounted handle to the same live project.
pub struct EditorSession {
	pub(crate) ptr: *mut c_void,
}

// The engine serializes session/project mutation behind mutexes. The opaque
// handle itself is refcounted and may be shared; raw pointer dereferences stay
// inside the engine ABI rather than in SDK client threads.
unsafe impl Send for EditorSession {}
unsafe impl Sync for EditorSession {}

impl EditorSession {
	/// Creates a fresh session around an existing project handle without
	/// reopening or copying the document. The handle is consumed exactly once.
	pub fn from_project(project: Project) -> Result<Self, StorageError> {
		Self::from_raw(unsafe { quercus_editor_from_project_handle(project.into_raw()) })
	}

	/// Adopts an owned storage handle into a session without reopening the project.
	///
	/// This is intended for migration from an existing shared project owner. The
	/// handle is consumed, and the session and original `ProjectRef` point to the
	/// same Arc; it is not a second document or a raw-Arc reconstruction.
	pub fn adopt_storage_project_handle(
		handle: StorageProjectHandle,
	) -> Result<Self, StorageError> {
		Self::from_raw(unsafe { quercus_editor_adopt_storage_project_handle(handle) })
	}

	fn from_raw(ptr: *mut c_void) -> Result<Self, StorageError> {
		if ptr.is_null() {
			Err(StorageError(-1))
		} else {
			Ok(Self { ptr })
		}
	}

	/// Creates a new project and binds it when a default library is configured.
	pub fn new() -> Result<Self, StorageError> {
		let session = Self::from_raw(unsafe { quercus_editor_new() })?;
		let _ = session.bind_project_to_library();
		Ok(session)
	}

	/// Opens any storage URI and binds the loaded project.
	pub fn open(uri: &str) -> Result<Self, StorageError> {
		let mut info = 0;
		let ptr = unsafe { quercus_editor_open(uri.as_ptr(), uri.len(), &mut info) };
		if ptr.is_null() {
			return Err(StorageError(info));
		}
		let session = Self { ptr };
		let _ = session.bind_project_to_library();
		Ok(session)
	}

	/// Opens the project identified by `uuid` in the configured default library.
	pub fn open_project_from_library(uuid: &str) -> Result<Self, StorageError> {
		let mut error = 0;
		let ptr = unsafe {
			quercus_editor_open_project_from_library(uuid.as_ptr(), uuid.len(), &mut error)
		};
		if ptr.is_null() {
			return Err(StorageError(error));
		}
		let session = Self { ptr };
		let _ = session.bind_project_to_library();
		Ok(session)
	}

	/// Opens the project `uuid` from an explicit library URI (any backend
	/// the registry can resolve, not only the configured default library).
	pub fn open_project_from_library_uri(
		library_uri: &str,
		uuid: &str,
	) -> Result<Self, StorageError> {
		let mut error = 0;
		let ptr = unsafe {
			quercus_editor_open_project_from_library_uri(
				library_uri.as_ptr(),
				library_uri.len(),
				uuid.as_ptr(),
				uuid.len(),
				&mut error,
			)
		};
		if ptr.is_null() {
			return Err(StorageError(error));
		}
		let session = Self { ptr };
		// Write-through binding targets the *configured* default library;
		// an explicitly chosen non-default library stays unbound until the
		// caller configures it (or binds explicitly).
		if crate::storage::default_library_uri().as_deref() == Some(library_uri) {
			let _ = session.bind_project_to_library();
		}
		Ok(session)
	}

	/// Creates a blank row in the default library without reloading it.
	pub fn create_in_library(name: &str) -> Result<Self, StorageError> {
		let mut error = 0;
		let ptr = unsafe { quercus_editor_create_library(name.as_ptr(), name.len(), &mut error) };
		if ptr.is_null() {
			return Err(StorageError(error));
		}
		let session = Self { ptr };
		let _ = session.bind_project_to_library();
		Ok(session)
	}

	/// A new safe handle to the same live project used by storage binding.
	pub fn project(&self) -> Project {
		Project::from_raw(unsafe { quercus_editor_project(self.ptr) })
	}

	/// Saves through the storage backend selected by the path extension.
	pub fn save(&self, path: &str) -> Result<(), StorageError> {
		let code = unsafe { quercus_editor_save(self.ptr, path.as_ptr(), path.len(), 0) };
		if code == 0 {
			Ok(())
		} else {
			Err(StorageError(code))
		}
	}

	/// Registers this exact project for write-through persistence in the default
	/// library. Later successful undo-path edits can be written back automatically.
	/// Binding does not itself save or create a library row.
	///
	/// False commonly means that no library is configured, which is a supported
	/// headless/non-persistent mode rather than a fatal error.
	pub fn bind_project_to_library(&self) -> bool {
		unsafe { quercus_editor_bind_project_to_library(self.ptr) }
	}

	/// Explicitly flushes pending writes and removes the library binding.
	/// Automatic persistence stops, but the session and in-memory project remain
	/// usable. This is required before deleting its library row. Drop performs
	/// the same operation as a fallback, and repeated closes are harmless.
	pub fn close(&self) {
		unsafe { quercus_editor_unbind(self.ptr) };
	}

	/// Queries whether the project currently has an active write-through binding.
	/// This neither binds nor saves the project and does not report write errors.
	pub fn storage_bound(&self) -> bool {
		unsafe { quercus_editor_storage_bound(self.ptr) }
	}

	/// Returns the most recent write-through error, if the backend supplied one.
	pub fn storage_last_error(&self) -> Option<String> {
		string_out(|buf, len| unsafe { quercus_editor_storage_last_error(self.ptr, buf, len) })
	}

	/// Returns the sequence targeted by timeline commands without an explicit ID.
	pub fn current_sequence(&self) -> Option<NodeId> {
		let id = NodeId::from_raw(unsafe { quercus_editor_current_sequence(self.ptr) });
		id.is_valid().then_some(id)
	}

	/// Selects the target sequence; false means the ID is not a session sequence.
	pub fn set_current_sequence(&self, sequence: NodeId) -> bool {
		unsafe { quercus_editor_set_current_sequence(self.ptr, sequence.raw()) }
	}

	/// Creates a sequence with the engine's default track lists and selects it.
	pub fn create_sequence(&self, name: &str) -> Result<NodeId, StorageError> {
		let id = NodeId::from_raw(unsafe {
			quercus_editor_create_sequence(self.ptr, name.as_ptr(), name.len())
		});
		if id.is_valid() {
			Ok(id)
		} else {
			Err(StorageError(-1))
		}
	}

	/// Builds an owned snapshot. The revision is the global undo row count
	/// at snapshot start and lets callers detect later edits.
	pub fn snapshot(&self) -> ProjectSnapshot {
		let revision = crate::node::undo_count();
		let project = self.project();
		let root = entry_snapshot(&project, project.root());
		let mut sequences = Vec::new();
		let mut footages = Vec::new();
		collect_media(&project, project.root(), &mut sequences, &mut footages);
		let current_sequence = self.current_sequence();
		let markers = self.markers();
		let workarea = self.workarea();
		ProjectSnapshot {
			revision,
			name: project.name(),
			filename: project.filename(),
			uuid: project.uuid(),
			modified: project.modified(),
			root,
			sequences,
			footages,
			current_sequence,
			markers,
			workarea,
		}
	}

	/// The session's markers (owned DTOs).
	pub fn markers(&self) -> Vec<MarkerSnapshot> {
		let count = unsafe { quercus_editor_marker_count(self.ptr) }.max(0);
		(0..count)
			.filter_map(|index| {
				let mut raw = EditorMarkerRaw::default();
				if !unsafe { quercus_editor_marker_at(self.ptr, index, &mut raw) } {
					return None;
				}
				Some(MarkerSnapshot {
					name: string_out(|buf, len| unsafe {
						quercus_editor_marker_name_at(self.ptr, index, buf, len)
					})
					.unwrap_or_default(),
					time: raw.time,
					color: raw.color,
				})
			})
			.collect()
	}

	fn workarea(&self) -> Option<WorkAreaSnapshot> {
		let (enabled, range) = self.workarea_state();
		enabled.then_some(WorkAreaSnapshot { range })
	}

	/// The session's work area: the enabled flag and its range (the range
	/// is meaningful even while disabled).
	pub fn workarea_state(&self) -> (bool, TimeRange) {
		let mut range = TimeRange::default();
		let enabled = unsafe { quercus_editor_workarea(self.ptr, &mut range) };
		(enabled, range)
	}

	/// Imports an SRT file as Text clips on the current sequence and track.
	/// Returns the cue count.
	pub fn import_subtitles(
		&self,
		path: &std::path::Path,
		track_index: usize,
	) -> Result<usize, i32> {
		let path = path.to_str().ok_or(-1)?;
		let count = unsafe {
			quercus_editor_import_subtitles(
				self.ptr,
				path.as_ptr(),
				path.len(),
				track_index as i32,
			)
		};
		(count >= 0).then_some(count as usize).ok_or(count)
	}

	/// Sets a clip's audio gain (0..=4, 1 = unity) as one undo entry.
	pub fn set_clip_gain(&self, clip: NodeId, gain: f64) -> bool {
		unsafe { quercus_editor_set_clip_gain(self.ptr, clip.raw(), gain) }
	}

	/// Sets a clip's constant-power stereo pan (-1..=1, 0 = centre) as one
	/// undo entry.
	pub fn set_clip_pan(&self, clip: NodeId, pan: f64) -> bool {
		unsafe { quercus_editor_set_clip_pan(self.ptr, clip.raw(), pan) }
	}

	/// Sets a clip's linear fade-in/fade-out durations as one undo entry.
	/// Both rationals are clamped/validated by the engine (negative or NaN
	/// durations are rejected).
	pub fn set_clip_fade(&self, clip: NodeId, fade_in: Rational, fade_out: Rational) -> bool {
		unsafe {
			quercus_editor_set_clip_fade(
				self.ptr,
				clip.raw(),
				fade_in.num,
				fade_in.den,
				fade_out.num,
				fade_out.den,
			)
		}
	}

	/// Attaches an audio effect to a clip's chain at `index` (clamped to
	/// the chain length) as one undo entry. Returns false for an empty
	/// type id.
	pub fn add_clip_audio_effect(&self, clip: NodeId, type_id: &str, index: usize) -> bool {
		let Ok(index) = i32::try_from(index) else {
			return false;
		};
		unsafe {
			quercus_editor_add_clip_audio_effect(
				self.ptr,
				clip.raw(),
				type_id.as_ptr(),
				type_id.len(),
				index,
			)
		}
	}

	/// Removes one entry of a clip's audio effect chain.
	pub fn remove_clip_audio_effect(&self, clip: NodeId, index: usize) -> bool {
		let Ok(index) = i32::try_from(index) else {
			return false;
		};
		unsafe { quercus_editor_remove_clip_audio_effect(self.ptr, clip.raw(), index) }
	}

	/// Enables/disables one entry of a clip's audio effect chain.
	pub fn set_clip_audio_effect_enabled(&self, clip: NodeId, index: usize, enabled: bool) -> bool {
		let Ok(index) = i32::try_from(index) else {
			return false;
		};
		unsafe {
			quercus_editor_set_clip_audio_effect_enabled(self.ptr, clip.raw(), index, enabled)
		}
	}

	/// Sets one static float parameter of a clip's audio effect (Phase 1:
	/// Volume `volume_in`, Pan `panning_in`).
	pub fn set_clip_audio_effect_param(
		&self,
		clip: NodeId,
		index: usize,
		input_id: &str,
		value: f64,
	) -> bool {
		let Ok(index) = i32::try_from(index) else {
			return false;
		};
		unsafe {
			quercus_editor_set_clip_audio_effect_param(
				self.ptr,
				clip.raw(),
				index,
				input_id.as_ptr(),
				input_id.len(),
				value,
			)
		}
	}

	/// Exports the current sequence's Text clips as an SRT file.
	pub fn export_subtitles(&self, path: &std::path::Path) -> Result<usize, i32> {
		let path = path.to_str().ok_or(-1)?;
		let count = unsafe {
			quercus_editor_export_subtitles(self.ptr, path.as_ptr(), path.len())
		};
		(count >= 0).then_some(count as usize).ok_or(count)
	}
}

impl Clone for EditorSession {
	/// Clones the handle, not the project; both values refer to the same session.
	fn clone(&self) -> Self {
		unsafe { quercus_editor_add_ref(self.ptr) };
		Self { ptr: self.ptr }
	}
}

impl Drop for EditorSession {
	fn drop(&mut self) {
		unsafe { quercus_editor_release(self.ptr) };
	}
}

mod snapshot;
mod graph;
mod params;
mod notify;
mod workflow;
mod media;
pub mod proxy;
pub mod sync;
pub mod multicam;
mod subtitles;
pub use snapshot::*;
pub use graph::*;
pub use params::*;
pub use notify::*;
pub use subtitles::*;

#[cfg(test)]
mod tests {
	use super::*;
	use crate::node::NodeKind;
	use crate::state::Config;
	use crate::types::Rational;

	#[test]
	fn file_round_trip_preserves_the_owned_snapshot() {
		let _guard = crate::node::UNDO_SERIAL
			.lock()
			.unwrap_or_else(|e| e.into_inner());
		let session = EditorSession::new().expect("new session");
		let folder = session
			.project()
			.create_folder(session.project().root(), "Nested Bin")
			.expect("nested folder");
		let media = crate::test_support::make_test_clip("snapshot");
		let footage = session.project().import_footage(&media).expect("footage");
		let sequence = session.create_sequence("SDK Sequence").expect("sequence");
		let project = session.project();
		let list = project.sequence_track_list_at(sequence, 0);
		let track = project.tracklist_track_at(list, 0);
		let clip = project.create_clip().expect("clip");
		assert!(project.track_append_block(track, clip));
		assert!(project.block_set_length(clip, Rational::new(2, 1)));

		let enabled = session.snapshot();
		assert!(enabled.sequences[0].tracks[0].clips[0].enabled);
		assert!(crate::timeline::block_set_enabled(&project, clip, false));
		let disabled = session.snapshot();
		assert!(!disabled.sequences[0].tracks[0].clips[0].enabled);
		assert!(disabled.revision > enabled.revision);
		assert!(crate::node::undo());
		assert!(session.snapshot().sequences[0].tracks[0].clips[0].enabled);
		assert!(crate::node::redo());
		assert!(!session.snapshot().sequences[0].tracks[0].clips[0].enabled);

		let before = session.snapshot();
		assert_eq!(before.current_sequence, Some(sequence));
		assert_eq!(before.sequences.len(), 1);
		assert_eq!(before.sequences[0].tracks[0].clips.len(), 1);
		assert_eq!(before.footages.len(), 1);
		assert_eq!(before.footages[0].id, footage);
		assert_eq!(before.footages[0].filename.as_deref(), Some(media.as_str()));
		assert!(before.footages[0].video_streams >= 1);
		assert_eq!(before.footages[0].video.map(|p| p.width), Some(64));
		assert!(before.footages[0].duration > Rational::new(0, 1));
		assert!(before.root.children.iter().any(|entry| entry.id == folder));
		assert!(before.markers.is_empty());
		assert!(before.workarea.is_none());

		let path = std::env::temp_dir()
			.join(format!("quercussdk_editor_{}.ove", std::process::id()))
			.to_string_lossy()
			.into_owned();
		session.save(&path).expect("save");
		let opened = EditorSession::open(&path).expect("open");
		let after = opened.snapshot();
		assert_eq!(after.filename, path);
		assert!(!after.modified);
		assert_eq!(after.current_sequence, Some(sequence));
		assert_eq!(after.sequences, before.sequences);
		assert_eq!(after.footages, before.footages);
		assert_eq!(after.root.children, before.root.children);
		assert_eq!(after.root.kind, Some(NodeKind::Folder));
		assert!(after.revision >= before.revision);
		let _ = std::fs::remove_file(path);
		let _ = std::fs::remove_file(media);
	}

	#[test]
	fn project_settings_and_cache_round_trip() {
		let _guard = crate::node::UNDO_SERIAL
			.lock()
			.unwrap_or_else(|e| e.into_inner());
		let session = EditorSession::new().expect("new session");
		let project = session.project();
		assert_eq!(project.setting("missing"), None);
		assert!(!project.set_setting("", "rejected"));
		assert!(project.set_setting("ocioconfig", "/tmp/test.ocio"));
		assert_eq!(
			project.setting("ocioconfig").as_deref(),
			Some("/tmp/test.ocio")
		);
		assert!(project.set_cache_location(2, "/tmp/quercus-cache"));
		assert_eq!(project.cache_location(), (2, "/tmp/quercus-cache".into()));
		assert!(project.set_cache_location(0, "/ignored"));
		assert_eq!(project.cache_location(), (0, String::new()));
		assert!(project.set_cache_location(-10, "/ignored"));
		assert_eq!(project.cache_location().0, 0);
		assert!(project.set_cache_location(99, "  /tmp/custom  "));
		assert_eq!(project.cache_location(), (2, "/tmp/custom".into()));
	}

	#[test]
	fn invalid_inputs_and_shared_close_semantics() {
		let _guard = crate::node::UNDO_SERIAL
			.lock()
			.unwrap_or_else(|e| e.into_inner());
		Config::global()
			.set("Storage/Backend", "off")
			.expect("disable library");
		assert!(EditorSession::open("not-a-storage-uri").is_err());
		assert!(EditorSession::open_project_from_library("missing").is_err());

		let session = EditorSession::new().expect("new session");
		let first = session.create_sequence("First").expect("first");
		let second = session.create_sequence("Second").expect("second");
		assert_eq!(session.current_sequence(), Some(second));
		assert!(session.set_current_sequence(first));
		assert!(!session.set_current_sequence(NodeId::INVALID));
		assert_eq!(session.current_sequence(), Some(first));
		assert_eq!(session.snapshot().sequences.len(), 2);

		let cloned = session.clone();
		cloned.close();
		assert!(!cloned.storage_bound());
		assert!(!session.storage_bound(), "clones share one session");
		let project = session.project();
		drop(cloned);
		drop(session);
		assert!(
			project.root().is_valid(),
			"the project handle owns its own reference"
		);
	}
}

#[cfg(test)]
mod auxiliary_roundtrip_tests {
	use super::*;
	use crate::edit::EditCommand;
	use crate::types::{Rational, TimeRange};

	#[test]
	fn markers_and_workarea_survive_save_open() {
		let _guard = crate::node::UNDO_SERIAL
			.lock()
			.unwrap_or_else(|e| e.into_inner());
		let session = EditorSession::new().expect("session");
		let _sequence = session.create_sequence("Markers").expect("sequence");
		session
			.execute(EditCommand::MarkerAdd {
				time: Rational::new(1, 1),
				name: "Marker One".into(),
				color: 3,
			})
			.expect("marker");
		session
			.execute(EditCommand::WorkareaSet {
				enabled: true,
				range: TimeRange::new(Rational::new(1, 1), Rational::new(3, 1)),
			})
			.expect("workarea");

		let path = std::env::temp_dir()
			.join(format!("quercus_sdk_marker_roundtrip_{}.ove", std::process::id()))
			.to_string_lossy()
			.into_owned();
		session.save(&path).expect("save");
		let opened = EditorSession::open(&path).expect("open");
		let markers = opened.markers();
		assert_eq!(markers.len(), 1);
		assert_eq!(markers[0].name, "Marker One");
		assert_eq!(
			markers[0].time,
			TimeRange::new(Rational::new(1, 1), Rational::new(1, 1))
		);
		assert_eq!(markers[0].color, 3);
		let (enabled, range) = opened.workarea_state();
		assert!(enabled);
		assert_eq!(range, TimeRange::new(Rational::new(1, 1), Rational::new(3, 1)));
		let _ = std::fs::remove_file(path);
	}
}

#[cfg(test)]
mod length_snapshot_tests {
	use super::*;
	use crate::edit::EditCommand;
	use crate::types::Rational;

	#[test]
	fn snapshot_length_is_computed_from_blocks() {
		let _guard = crate::node::UNDO_SERIAL
			.lock()
			.unwrap_or_else(|e| e.into_inner());
		let session = EditorSession::new().expect("session");
		let sequence = session.create_sequence("Length").expect("sequence");
		let media = crate::test_support::make_test_clip("length_snapshot");
		let footage = session.project().import_footage(&media).expect("footage");
		session
			.execute(EditCommand::PlaceFootageClip {
				footage,
				kind: crate::editor::TrackKind::Video,
				track_index: 0,
				in_ts: 0,
				out_ts: 25,
				media_in_ts: 0,
			})
			.expect("place clip");
		let snapshot = session.snapshot();
		let sequence_snapshot = snapshot
			.sequences
			.iter()
			.find(|s| s.id == sequence)
			.expect("sequence snapshot");
		assert_eq!(
			sequence_snapshot.length,
			Rational::new(1001, 1200),
			"length must reflect the placed clip without app-side verify"
		);
		let _ = std::fs::remove_file(media);
	}
}

#[cfg(test)]
mod edit_result_primary_tests {
	use super::*;
	use crate::edit::EditCommand;
	use crate::editor::TrackKind;

	#[test]
	fn add_track_and_split_return_primary_nodes() {
		let _guard = crate::node::UNDO_SERIAL
			.lock()
			.unwrap_or_else(|e| e.into_inner());
		let session = EditorSession::new().expect("session");
		let sequence = session.create_sequence("Primary").expect("sequence");
		let added = session
			.execute(EditCommand::AddTrack {
				kind: TrackKind::Video,
			})
			.expect("add track");
		let track = added.primary.expect("add track primary");
		let snapshot = session.snapshot();
		let sequence_snapshot = snapshot
			.sequences
			.iter()
			.find(|s| s.id == sequence)
			.expect("sequence snapshot");
		assert!(
			sequence_snapshot.tracks.iter().any(|t| t.id == track),
			"primary must be the new track"
		);

		let media = crate::test_support::make_test_clip("primary_split");
		let footage = session.project().import_footage(&media).expect("footage");
		let clip = session
			.execute(EditCommand::PlaceFootageClip {
				footage,
				kind: TrackKind::Video,
				track_index: 0,
				in_ts: 0,
				out_ts: 25,
				media_in_ts: 0,
			})
			.expect("place")
			.primary
			.expect("clip");
		let split = session
			.execute(EditCommand::SplitClip {
				clip,
				time_ts: 10,
			})
			.expect("split");
		let rear = split.primary.expect("split primary");
		assert_ne!(rear, clip);
		let snapshot = session.snapshot();
		let clips = snapshot
			.sequences
			.iter()
			.find(|s| s.id == sequence)
			.expect("sequence")
			.tracks
			.iter()
			.flat_map(|t| t.clips.iter())
			.collect::<Vec<_>>();
		assert!(clips.iter().any(|c| c.id == rear), "rear half must exist");
		let _ = std::fs::remove_file(media);
	}
}

#[cfg(test)]
mod subtitle_roundtrip_tests {
	use super::*;
	use std::sync::atomic::{AtomicU64, Ordering};

	static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

	fn temp_srt(tag: &str, contents: &str) -> std::path::PathBuf {
		let n = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
		let path = std::env::temp_dir().join(format!(
			"quercus_sdk_subtitle_{tag}_{}_{n}.srt",
			std::process::id()
		));
		std::fs::write(&path, contents).unwrap();
		path
	}

	fn text_source(session: &EditorSession, sequence: NodeId, clip: NodeId) -> NodeId {
		let graph = session
			.graph_snapshot(sequence, Some(clip))
			.expect("clip graph");
		graph
			.edges
			.iter()
			.find(|edge| !edge.synthetic && edge.to == clip && edge.input_id == "tex_in")
			.map(|edge| edge.from)
			.expect("clip has a texture source")
	}

	#[test]
	fn srt_import_export_through_sdk() {
		let _guard = crate::node::UNDO_SERIAL
			.lock()
			.unwrap_or_else(|e| e.into_inner());
		let session = EditorSession::new().expect("session");
		let _sequence = session.create_sequence("Subtitle SDK").expect("sequence");
		let input = temp_srt(
			"roundtrip",
			"1\n00:00:00,500 --> 00:00:01,500\nFirst\n\n2\n00:00:02,000 --> 00:00:03,000\nSecond\n",
		);
		let count = session.import_subtitles(&input, 0).expect("import");
		assert_eq!(count, 2);
		let output = temp_srt("roundtrip_out", "");
		let exported = session.export_subtitles(&output).expect("export");
		assert_eq!(exported, 2);
		let text = std::fs::read_to_string(&output).unwrap();
		assert!(text.contains("First"));
		assert!(text.contains("Second"));
		let _ = std::fs::remove_file(input);
		let _ = std::fs::remove_file(output);
	}

	#[test]
	fn styled_import_and_set_style_are_undoable_through_sdk() {
		let _guard = crate::node::UNDO_SERIAL
			.lock()
			.unwrap_or_else(|e| e.into_inner());
		let session = EditorSession::new().expect("session");
		let sequence = session.create_sequence("Styled SDK").expect("sequence");
		let input = temp_srt(
			"styled",
			"1\n00:00:00,500 --> 00:00:01,500\nFirst\n\n2\n00:00:02,000 --> 00:00:03,000\nSecond\n",
		);
		let style = SubtitleStyle {
			font_family: "DejaVu Sans".to_string(),
			font_size: 48.0,
			color: [1.0, 0.25, 0.0, 1.0],
			outline_color: [0.0, 0.0, 1.0, 1.0],
			outline_width: 3.0,
			align: 2,
			..SubtitleStyle::default()
		};
		let count = session
			.import_subtitles_styled(&input, 0, &style)
			.expect("styled import");
		assert_eq!(count, 2);

		let snapshot = session.snapshot();
		let sequence_snapshot = snapshot
			.sequences
			.iter()
			.find(|s| s.id == sequence)
			.expect("sequence");
		// The track also holds gap blocks before/between cues; only the
		// text clips are subtitles.
		let clips = sequence_snapshot
			.tracks
			.iter()
			.flat_map(|track| track.clips.iter())
			.filter(|clip| clip.kind == Some(crate::node::NodeKind::Clip))
			.collect::<Vec<_>>();
		assert_eq!(clips.len(), 2);
		let mut sources = Vec::new();
		for clip in &clips {
			let source = text_source(&session, sequence, clip.id);
			let graph = session
				.graph_snapshot(sequence, Some(clip.id))
				.expect("clip graph");
			assert_eq!(
				graph
					.nodes
					.iter()
					.find(|node| node.id == source)
					.expect("source node")
					.type_id,
				"org.olivevideoeditor.Olive.text3"
			);
			let params = session.parameters(source).expect("text parameters");
			assert_eq!(
				params
					.get("font_family_in")
					.expect("font family")
					.scalar_value()
					.expect("font value")
					.text
					.as_deref(),
				Some("DejaVu Sans")
			);
			let size = params
				.get("font_size_in")
				.expect("font size")
				.scalar_value()
				.expect("size value")
				.pod
				.expect("size pod");
			assert_eq!(size.f[0], 48.0);
			let color = params
				.get("color_in")
				.expect("color")
				.scalar_value()
				.expect("color value")
				.pod
				.expect("color pod");
			assert_eq!(color.f, [1.0, 0.25, 0.0, 1.0]);
			let outline = params
				.get("outline_enabled_in")
				.expect("outline enabled")
				.scalar_value()
				.expect("outline value")
				.pod
				.expect("outline pod");
			assert_eq!(outline.num, 1);
			let outline_width = params
				.get("outline_width_in")
				.expect("outline width")
				.scalar_value()
				.expect("outline width value")
				.pod
				.expect("outline width pod");
			assert_eq!(outline_width.f[0], 3.0);
			let align = session
				.project()
				.standard_value(source, "valign_in", -1)
				.expect("vertical alignment");
			assert_eq!(align.num, 2);
			sources.push(source);
		}

		// Applying another style is one undo row; undo restores every field.
		let clip = clips[0].id;
		let replacement = SubtitleStyle {
			font_family: "Serif".to_string(),
			font_size: 30.0,
			color: [0.0, 1.0, 0.0, 1.0],
			outline_enabled: false,
			outline_width: 0.0,
			align: 0,
			..SubtitleStyle::default()
		};
		assert!(
			session.set_subtitle_style(clip, &replacement),
			"style application succeeds"
		);
		let params = session.parameters(sources[0]).expect("styled parameters");
		assert_eq!(
			params
				.get("font_family_in")
				.unwrap()
				.scalar_value()
				.unwrap()
				.text
				.as_deref(),
			Some("Serif")
		);
		assert_eq!(
			params
				.get("outline_enabled_in")
				.unwrap()
				.scalar_value()
				.unwrap()
				.pod
				.unwrap()
				.num,
			0
		);
		assert!(crate::node::undo());
		let restored = session.parameters(sources[0]).expect("restored parameters");
		assert_eq!(
			restored
				.get("font_size_in")
				.unwrap()
				.scalar_value()
				.unwrap()
				.pod
				.unwrap()
				.f[0],
			48.0
		);
		assert_eq!(
			restored
				.get("font_family_in")
				.unwrap()
				.scalar_value()
				.unwrap()
				.text
				.as_deref(),
			Some("DejaVu Sans")
		);

		// A non-text clip is rejected without panicking.
		let plain = session.project().create_clip().expect("plain clip");
		assert!(!session.set_subtitle_style(plain, &replacement));

		assert_eq!(
			EditorSession::default_subtitle_style(),
			SubtitleStyle::default()
		);
		let _ = std::fs::remove_file(input);
	}
}

#[cfg(test)]
mod clip_audio_post_tests {
	use super::*;
	use crate::types::Rational;

	/// The Phase-1 clip audio setters are reachable through the C ABI and
	/// reject invalid values without panicking.
	#[test]
	fn clip_audio_post_setters_cross_the_abi() {
		let _guard = crate::node::UNDO_SERIAL
			.lock()
			.unwrap_or_else(|e| e.into_inner());
		let session = EditorSession::new().expect("session");
		let sequence = session.create_sequence("Audio Post").expect("sequence");
		let project = session.project();
		let list = project.sequence_track_list_at(sequence, 0);
		let track = project.tracklist_track_at(list, 0);
		let clip = project.create_clip().expect("clip");
		assert!(project.track_append_block(track, clip));

		assert!(session.set_clip_gain(clip, 0.5));
		assert!(session.set_clip_pan(clip, -0.5));
		assert!(session.set_clip_fade(clip, Rational::new(1, 2), Rational::new(1, 4)));
		assert!(session.add_clip_audio_effect(
			clip,
			"org.olivevideoeditor.Olive.volume",
			0
		));
		assert!(session.set_clip_audio_effect_param(clip, 0, "volume_in", 0.25));
		assert!(session.add_clip_audio_effect(
			clip,
			"org.olivevideoeditor.Olive.pan",
			1
		));
		assert!(session.set_clip_audio_effect_param(clip, 1, "panning_in", 0.5));
		assert!(session.set_clip_audio_effect_enabled(clip, 1, false));
		assert!(session.remove_clip_audio_effect(clip, 1));

		// Invalid values come back as false.
		assert!(!session.set_clip_pan(clip, 2.0));
		assert!(!session.set_clip_fade(clip, Rational::new(-1, 1), Rational::new(0, 1)));
		assert!(!session.add_clip_audio_effect(clip, "", 0));
		assert!(!session.set_clip_audio_effect_param(clip, 99, "volume_in", 1.0));
		assert!(!session.set_clip_audio_effect_enabled(clip, 99, true));
		assert!(!session.remove_clip_audio_effect(clip, 99));
	}
}
