// Quercus Video Editor - Non-Linear Video Editor
// Copyright (C) 2026 Quercus Team
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <http://www.gnu.org/licenses/>.

//! The render manager: process-wide singleton owning the process
//! dispatcher, the ticket arena, the auto-cacher, and backend selection
//! (C++ `RenderManager`).
//!
//! The singleton lives behind a `Mutex<Option<Arc<…>>>` so `init` /
//! `shutdown` round-trips (C++ `create_instance` / `destroy_instance`),
//! and consumers share the `Arc`. `global()` returns the `Arc` (the
//! skeleton's `&'static` reference was replaced because a resettable
//! singleton cannot hand out stable references safely).

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use crate::autocacher::PreviewAutoCacher;
use crate::error::{Error, Result};
use crate::eval;
use crate::procpool::{DispatcherConfig, ProcessDispatcher, ShmAudioRef, ShmFrameRef};
use crate::ticket::{TicketArena, TicketId};
use crate::worker::{GraphSnapshotStore, InlineDispatcher, JobDispatch};
use quercus_core::backend::BackendKind;

static MANAGER: Mutex<Option<Arc<RenderManager>>> = Mutex::new(None);

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
	m.lock().unwrap_or_else(|e| e.into_inner())
}

/// The render backend the manager initializes (M15 S2).
pub enum RenderBackendChoice {
	/// In-process thread-free dispatch (**test-only** after M15 S2: the
	/// internal render thread pool was deleted by mandate; this backend
	/// runs jobs synchronously on the calling thread). Kept so manager /
	/// integration tests do not spawn quercus-worker children.
	Threads,
	/// Process-isolated quercus-worker pool (crash isolation + shm frames).
	/// The M15 S2 default.
	Processes(DispatcherConfig),
	/// The M1 thread pipeline ([`crate::pipeline::PipelineBackend`]): one
	/// render thread running the same producer the other backends run,
	/// plus the decode thread footage rendering rendezvouses with.
	/// Selected by `QUERCUS_PIPELINE=threads`; the process pool stays the
	/// default until the M4 acceptance.
	Pipeline,
}

/// The backend `QUERCUS_PIPELINE` asks for: `threads` selects the M1 thread
/// pipeline, anything else (including unset) the default process pool.
fn backend_choice_from_env() -> RenderBackendChoice {
	if std::env::var("QUERCUS_PIPELINE").as_deref() == Ok("threads") {
		RenderBackendChoice::Pipeline
	} else {
		RenderBackendChoice::Processes(DispatcherConfig::default())
	}
}

/// The manager. Created by `quercusrender_manager_init` (C ABI), accessed
/// internally through [`RenderManager::global`].
pub struct RenderManager {
	/// Video job dispatch (the process dispatcher, M15).
	pub dispatch: Arc<dyn JobDispatch>,
	/// Audio job dispatch (M16 S2: deliberately the inline dispatcher —
	/// playback audio renders synchronously on the submitting thread so it
	/// never queues behind worker video batches; the worker-pool audio path
	/// was the M15 S3 default, design §3.7).
	pub audio_dispatch: Arc<dyn JobDispatch>,
	/// Ticket arena.
	pub tickets: Arc<TicketArena>,
	/// Active GPU backend.
	pub backend: BackendKind,
	/// The backend the user requested (C++ `requested_backend`).
	pub requested_backend: BackendKind,
	/// Auto-cacher (None until first access; created lazily by
	/// [`RenderManager::get_cacher`]).
	pub autocacher: Mutex<Option<PreviewAutoCacher>>,
	/// Aggressive decoder GC toggle.
	aggressive_gc: AtomicBool,
	/// Graph snapshot files shared with worker processes (M16 S1).
	snapshots: GraphSnapshotStore,
	/// The snapshot path currently shipped to the worker pool (None until
	/// the app pushes one).
	current_snapshot: Mutex<Option<String>>,
	/// The (project uuid, undo revision) the current snapshot was written
	/// for (M16 S1: dedup key — revisions alone collide across projects,
	/// since every fresh project shares small revision numbers).
	current_key: Mutex<Option<(String, u64)>>,
	/// The live project the current snapshot was written from — the
	/// INLINE (test) backend's graph-mode render source. The process
	/// backend loads the snapshot file in its workers instead; this slot
	/// only exists so `render_produced_frame` can run the same
	/// [`quercus_node::traverser`] graph evaluation without forking a child.
	inline_graph: Mutex<Option<(String, std::sync::Arc<std::sync::Mutex<quercus_node::project::Project>>)>>,	/// Teardown in progress (M16 S1): set first thing in
	/// [`RenderManager::shutdown`]; `set_graph_snapshot` /
	/// `clear_graph_snapshot` become no-ops afterwards so a stale push
	/// from a dying test/app cannot re-arm the worker pool mid-shutdown.
	stopping: AtomicBool,
	/// The process pool, when running the Processes backend (the resize
	/// target of [`RenderManager::set_workspace_size`]). `None` on the
	/// inline (test) backend.
	process_pool: Option<Arc<ProcessDispatcher>>,
	/// The thread pipeline, when running the Pipeline backend (`None` on
	/// the inline and process backends). Holding the `Arc` keeps the
	/// render/decode threads alive for the manager's lifetime.
	pipeline: Option<Arc<crate::pipeline::PipelineBackend>>,
	/// The single OpenFX host client (M3), when the Pipeline backend is
	/// live: the render thread's plugin jobs go through it instead of the
	/// in-process executor (design §3.2).
	ofx_host: Option<Arc<crate::ofxhost::OfxHost>>,
}

impl RenderManager {
	/// Initialize the process-wide manager with the backend `QUERCUS_PIPELINE`
	/// asks for — the process-isolated quercus-worker pool (M15 S2 mandate)
	/// unless the variable says `threads` (idempotent; C++ instance()
	/// semantics — only the main GUI process does this).
	pub fn init() -> Result<()> {
		Self::init_with_backend(backend_choice_from_env())
	}

	/// Initialize the process-wide manager with an explicit backend.
	/// `Threads` is the test-only inline backend (no worker threads, no
	/// child processes); `Processes` spawns the quercus-worker pool;
	/// `Pipeline` starts the M1 render/decode threads.
	pub fn init_with_backend(choice: RenderBackendChoice) -> Result<()> {
		let mut guard = lock(&MANAGER);
		if guard.is_some() {
			return Err(Error::State);
		}
		let backend = BackendKind::from_user_config();
		let producer: crate::ticket::Producer = Arc::new(|time, params| {
			// M16 S1 graph mode on the INLINE (test) backend: when the
			// ticket names a viewer of the snapshotted project, evaluate
			// the node graph (the same path the quercus-worker pool runs in
			// the process backend) instead of the montage shortcut — the
			// preview must show the node graph, not the decoded-only
			// montage.
			if params.viewer != 0 && !params.project.is_empty() {
				if let Some(m) = crate::manager::RenderManager::global() {
					if let Some((uuid, project)) = m.inline_graph() {
						if uuid == params.project {
							if let Some(viewer_id) =
								quercus_node::id::NodeId::from_identity(params.viewer)
							{
								if let Ok(texture) = eval::render_graph_frame(
									&project,
									viewer_id,
									time,
									params.render_size(),
									quercus_core::PixelFormat::F32,
								) {
									return Ok(crate::ticket::TicketPayload::Video(
										texture,
									));
								}
							}
						}
					}
				}
			}
			eval::render_produced_frame(time, params)
				.map(crate::ticket::TicketPayload::Video)
		});
		let (dispatch, audio_dispatch, audio_fallback, process_pool, pipeline, ofx_host): (
			Arc<dyn JobDispatch>,
			Arc<dyn JobDispatch>,
			Option<Arc<dyn JobDispatch>>,
			Option<Arc<ProcessDispatcher>>,
			Option<Arc<crate::pipeline::PipelineBackend>>,
			Option<Arc<crate::ofxhost::OfxHost>>,
		) = match choice {
			RenderBackendChoice::Threads => {
				// Test-only inline backend: synchronous execution on the
				// calling thread, shared by video and audio.
				let inline = InlineDispatcher::sync();
				(inline.clone(), inline, None, None, None, None)
			}
			RenderBackendChoice::Processes(config) => {
				let dispatcher = ProcessDispatcher::new(config)?;
				dispatcher.start()?;
				// M16 S2: audio rendering deliberately runs INLINE on the
				// submitting (UI) thread instead of the worker pool. Video
				// batches — especially OFX plugin frames at ~100-500 ms
				// each — can occupy the workers far longer than the cpal
				// output buffer (~4 chunks, ≈100 ms) holds; an audio batch
				// parked behind them underruns and the audio goes silent
				// while the video is merely slow. Playback audio is a short
				// synchronous range pull, so inline mixing keeps audio
				// flowing no matter how stuck the video workers are
				// (design §3.7 prefers inline before S3). Trade: an
				// audio-side plugin crash now takes down the main process,
				// and the mix cost lands on the UI tick.
				let inline = InlineDispatcher::sync();
				(dispatcher.clone(), inline, None, Some(dispatcher), None, None)
			}
			RenderBackendChoice::Pipeline => {
				// M1 thread pipeline: the render thread executes the same
				// producer (so graph mode included), and the decode thread
				// the producer's footage decodes rendezvous with. Audio
				// stays inline, as on the process backend.
				//
				// No OFX host process: the in-process quercusplugin
				// executor installed by the frontend renders plugins on the
				// same render thread. This is what removes the worker
				// process dependency; GPU texture sharing is not implied
				// (the in-process executor's render path is still the
				// CPU/GL-readback path).
				crate::ofxhost::install_client(None);
				let pipeline = crate::pipeline::PipelineBackend::new()?;
				let inline = InlineDispatcher::sync();
				(
					pipeline.clone(),
					inline,
					None,
					None,
					Some(pipeline),
					None,
				)
			}
		};
		let tickets = Arc::new(TicketArena::new_with_audio_fallback(
			dispatch.clone(),
			audio_dispatch.clone(),
			audio_fallback,
			producer,
		));
		*guard = Some(Arc::new(RenderManager {
			dispatch,
			audio_dispatch,
			tickets,
			backend,
			requested_backend: backend,
			autocacher: Mutex::new(None),
			aggressive_gc: AtomicBool::new(false),
			snapshots: GraphSnapshotStore::new(),
			current_snapshot: Mutex::new(None),
			current_key: Mutex::new(None),
			inline_graph: Mutex::new(None),
			stopping: AtomicBool::new(false),
			process_pool,
			pipeline,
			ofx_host,
		}));
		Ok(())
	}

	/// The inline backend's graph-mode render source: the live project the
	/// current snapshot was written for, with its uuid. `None` when there
	/// is no snapshot (or the process pool answers frames instead).
	pub fn inline_graph(
		&self,
	) -> Option<(String, std::sync::Arc<std::sync::Mutex<quercus_node::project::Project>>)> {
		lock(&self.inline_graph).clone()
	}

	/// The thread pipeline, when the Pipeline backend is live (its queue
	/// stats and decode service). `None` on the inline and process backends.
	pub fn pipeline_backend(&self) -> Option<Arc<crate::pipeline::PipelineBackend>> {
		self.pipeline.clone()
	}

	/// The single OpenFX host client, when the Pipeline backend is live
	/// (tests/observability). `None` on the inline and process backends.
	pub fn ofx_host(&self) -> Option<Arc<crate::ofxhost::OfxHost>> {
		self.ofx_host.clone()
	}

	/// Global access; `None` before init.
	pub fn global() -> Option<Arc<RenderManager>> {
		lock(&MANAGER).clone()
	}

	/// The auto-cacher, creating it on first access (C++ `get_cacher`).
	pub fn get_cacher(&self) -> MutexGuard<'_, Option<PreviewAutoCacher>> {
		let mut guard = lock(&self.autocacher);
		if guard.is_none() {
			*guard = Some(PreviewAutoCacher::new(self.tickets.clone()));
		}
		guard
	}

	/// Shut down: cancel tickets, drain both dispatch backends.
	pub fn shutdown() {
		let manager = lock(&MANAGER).take();
		if let Some(manager) = manager {
			// Mark stopping FIRST: from here on `set_graph_snapshot`,
			// `clear_graph_snapshot` and `poll` become no-ops, so a
			// concurrent app thread racing the teardown cannot re-arm the
			// dispatcher after it is drained.
			manager.stopping.store(true, Ordering::Release);
			manager.tickets.cancel_all();
			// Stop routing plugin jobs to the OFX host before tearing it
			// down (an in-flight submit fails out to a purple frame).
			if let Some(host) = &manager.ofx_host {
				crate::ofxhost::install_client(None);
				host.shutdown();
			}
			// Drain after the cancels so queued completions fire. Both
			// dispatches are idempotent.
			manager.dispatch.shutdown();
			manager.audio_dispatch.shutdown();
			// Release the graph snapshot (the file is retained: a worker
			// may still hold the path for a late load_graph).
			if let Some(path) = lock(&manager.current_snapshot).take() {
				manager.snapshots.release(&path);
			}
			// The store directory is cleared here and only here — no worker
			// can reference a snapshot file once the dispatchers are down.
			manager.snapshots.cleanup();
			drop(manager);
		}
	}

	/// M16 S1 graph mode: snapshot the project to the worker pool. The
	/// snapshot is serialized once per (project, revision) — the undo-stack
	/// position; the key includes the project's uuid because fresh projects
	/// reuse small identity numbers and two projects at the same revision
	/// would otherwise collide on one file (the cross-project snapshot race
	/// that shipped before M16 S1). A new key rewrites the file and
	/// re-sends `load_graph` to every live worker, releasing the previous
	/// snapshot (file retained at zero refs).
	pub fn set_graph_snapshot(
		&self,
		project: &std::sync::Mutex<quercus_node::project::Project>,
		revision: u64,
	) -> Result<()> {
		if self.stopping.load(Ordering::Acquire) {
			return Ok(()); // teardown: no re-arm after the drain
		}
		let uuid = lock(project).uuid.clone();
		if std::env::var_os("QUERCUS_DEBUG_SNAPSHOT").is_some() {
			eprintln!(
				"[snapshot] set_graph_snapshot uuid {uuid} revision {revision} key {:?}",
				*lock(&self.current_key)
			);
		}
		if *lock(&self.current_key) == Some((uuid.clone(), revision)) {
			return Ok(()); // unchanged state: no rewrite, no re-send
		}
		let path = self.snapshots.acquire(project, revision)?;
		if let Some(old) = lock(&self.current_snapshot).replace(path.clone()) {
			self.snapshots.release(&old);
		}
		self.dispatch.set_graph_snapshot(Some(path));
		// Keep a live graph handle for the inline backend's graph mode
		// (the same snapshot content, in-process, no file required).
		// `project` arrives by reference (the snapshot store's lifetime);
		// quercus-app installs the Arc via [`RenderManager::set_inline_project`]
		// — the inline graph slot is only armed there, never from a
		// borrowed reference.
		*lock(&self.current_key) = Some((uuid, revision));
		Ok(())
	}

	/// Installs the live project the inline backend's graph mode renders
	/// from (quercus-app calls this on project adoption; it also arms the uuid
	/// the snapshot dedups against). The process backend ignores it.
	pub fn set_inline_project(
		&self,
		project: std::sync::Arc<std::sync::Mutex<quercus_node::project::Project>>,
	) {
		let uuid = match project.lock() {
			Ok(g) => g.uuid.clone(),
			Err(p) => p.into_inner().uuid.clone(),
		};
		*lock(&self.inline_graph) = Some((uuid, project));
	}

	/// M16 S1 graph mode: force the workers to re-load the current project
	/// snapshot even when the undo-stack revision is unchanged. The
	/// color-settings dialog writes project settings directly (no undo
	/// command), so the revision-based dedup in [`set_graph_snapshot`] would
	/// never re-send the snapshot — workers would keep rendering under the
	/// colors they loaded at project-load time. The snapshot file is
	/// rewritten (see [`GraphSnapshotStore::acquire_rewrite`]) and
	/// `load_graph` re-broadcast to every live worker (the dispatcher's
	/// re-send has no dedup).
	pub fn resync_graph_snapshot(
		&self,
		project: &std::sync::Mutex<quercus_node::project::Project>,
		revision: u64,
	) -> Result<()> {
		if self.stopping.load(Ordering::Acquire) {
			return Ok(()); // teardown: no re-arm after the drain
		}
		let path = self.snapshots.acquire_rewrite(project, revision)?;
		if let Some(old) = lock(&self.current_snapshot).replace(path.clone()) {
			self.snapshots.release(&old);
		}
		self.dispatch.set_graph_snapshot(Some(path));
		let uuid = lock(project).uuid.clone();
		*lock(&self.current_key) = Some((uuid, revision));
		Ok(())
	}

	/// M16 S1 graph mode: drop the current snapshot (project closed). The
	/// protocol has no clear message, so alive workers keep their loaded
	/// graph; new/restarted workers no longer load it and the snapshot file
	/// is retained (removed wholesale at manager shutdown).
	pub fn clear_graph_snapshot(&self) {
		if self.stopping.load(Ordering::Acquire) {
			return;
		}
		if let Some(old) = lock(&self.current_snapshot).take() {
			self.snapshots.release(&old);
		}
		*lock(&self.current_key) = None;
		*lock(&self.inline_graph) = None;
		self.dispatch.set_graph_snapshot(None);
	}

	/// Announce the current frame size to the manager (a sequence
	/// resolution change). The process dispatcher re-evaluates the
	/// GPU-vram-aware worker count for the new size and resizes the pool
	/// (1080p can run the CPU-bound count; 4K only a vram-bounded
	/// fraction — see [`crate::procpool::worker_count_for_size`]).
	/// No-op on backends that hold no process pool.
	pub fn set_workspace_size(&self, width: i32, height: i32, fps: u32) {
		let Some(pool) = &self.process_pool else {
			return;
		};
		let slots = pool.slots_per_worker();
		let slot_bytes = crate::procpool::slot_bytes_for(
			width.max(1),
			height.max(1),
			pool.slot_format(),
		);
		let target = crate::procpool::worker_count_for_size(slots, slot_bytes, (width, height), fps);
		pool.set_target_workers(target);
	}

	/// Pump the video backend's control plane (M15 S2): the process
	/// dispatcher delivers ticket completions from its poll loop, so the
	/// UI tick and any blocking wait must call this regularly. No-op on
	/// backends that deliver inline.
	pub fn poll(&self) {
		self.dispatch.poll();
	}

	/// Release a consumed shm frame's slot back to its worker (M15 S2
	/// zero-copy onscreen path: slot release = cache eviction). No-op on
	/// backends that hold no slots.
	pub fn release_frame(&self, frame: &ShmFrameRef) {
		self.dispatch.release_frame(frame);
	}

	/// Release a consumed shm audio frame's slot (M15 S3; see
	/// [`RenderManager::release_frame`]).
	pub fn release_audio_frame(&self, frame: &ShmAudioRef) {
		self.dispatch.release_audio_frame(frame);
	}

	/// Cancel every pending AND claimed frame of `sequence` (M15 S2
	/// preview-window invalidation); their completions fire with
	/// `Error::State`. No-op on backends that schedule no window.
	pub fn cancel_preview_sequence(&self, sequence: u64) {
		self.dispatch.cancel_preview_sequence(sequence);
	}

	/// Aggressive-GC toggle (C++ `SetAggressiveGarbageCollection`).
	pub fn set_aggressive_gc(&self, on: bool) {
		self.aggressive_gc.store(on, Ordering::Release);
	}

	/// The aggressive-GC toggle.
	pub fn aggressive_gc(&self) -> bool {
		self.aggressive_gc.load(Ordering::Acquire)
	}

	/// Submit a video ticket through the manager's arena (used by the
	/// auto-cacher and the FFI request path).
	pub fn submit_video(
		&self,
		params: crate::ticket::VideoTicketParams,
		done: crate::ticket::Completion,
	) -> TicketId {
		self.tickets.submit_video(params, done)
	}
}

/// Config key naming the disk-cache directory override (read by
/// [`quercus_core::filefunctions::default_disk_cache_path`]).
const DISK_CACHE_PATH_KEY: &str = "DiskCachePath";

/// Config key holding the disk-cache byte budget (`0` = unlimited).
const DISK_CACHE_MAX_BYTES_KEY: &str = "DiskCacheMaxBytes";

/// Minimum wall-clock gap between debounced quota sweeps (see
/// [`enforce_disk_cache_quota`]).
const DISK_CACHE_PRUNE_INTERVAL: Duration = Duration::from_secs(2);

/// A quota sweep is also forced every this many cache writes, so a burst of
/// frame writes cannot outrun the budget by more than this many files.
const DISK_CACHE_PRUNE_WRITES: u64 = 64;

/// Writes observed since the last quota sweep (debounce bookkeeping).
static DISK_CACHE_WRITES: AtomicU64 = AtomicU64::new(0);

/// When the last quota sweep started (`None` = no sweep yet); guarded by a
/// mutex only for the read/update, never held across the directory walk.
static DISK_CACHE_LAST_PRUNE: Mutex<Option<Instant>> = Mutex::new(None);

/// The default disk cache directory (C++ `DiskManager::
/// get_default_disk_cache_path`); shared with quercusnode via quercus_core
/// (single-lib unification).
pub fn disk_cache_path() -> String {
	quercus_core::filefunctions::default_disk_cache_path()
}

/// The configured disk-cache byte budget (`DiskCacheMaxBytes`).
///
/// `0` (the default) means unlimited; negative values are treated as `0`.
/// The value is parsed from the config store's textual form because the key
/// is not registered in `ConfigStore::set_defaults`, so a value loaded from
/// `config.toml` arrives as a string.
pub fn disk_cache_max_bytes() -> i64 {
	let raw = quercus_core::configstore::ConfigStore::instance()
		.get(None, DISK_CACHE_MAX_BYTES_KEY)
		.unwrap_or_default();
	raw.trim().parse::<i64>().unwrap_or(0).max(0)
}

/// Set the disk-cache byte budget (`0` = unlimited; negative clamps to `0`)
/// and immediately enforce it by pruning the default disk cache directory.
///
/// The value is written to the in-memory config store only; persisting it is
/// the job of the existing config-save path (`ConfigStore::save`). Also
/// resets the [`enforce_disk_cache_quota`] debounce so a lowered budget
/// applies on the next enforcement step.
pub fn set_disk_cache_max_bytes(max: i64) -> Result<()> {
	let max = max.max(0);
	quercus_core::configstore::ConfigStore::instance().set_int64(
		None,
		DISK_CACHE_MAX_BYTES_KEY,
		max,
	);
	// A newly applied budget must not wait for the debounce window.
	reset_quota_debounce();
	if max > 0 {
		prune_disk_cache_to(max)?;
	}
	Ok(())
}

/// Point the disk cache at `path` (`DiskCachePath`), creating the directory
/// when missing.
///
/// `path` must be non-empty and absolute; surrounding whitespace is trimmed.
/// Only the config key changes: **existing sessions keep their already
/// resolved paths** — [`crate::cache::PlaybackCache`] copies `disk_dir` at
/// construction, so caches created before this call keep writing to the old
/// location until they are recreated.
///
/// Like [`set_disk_cache_max_bytes`], the value is only written to the
/// in-memory config store; the config-save path persists it.
pub fn set_disk_cache_path(path: &str) -> Result<()> {
	let path = path.trim();
	if path.is_empty() {
		return Err(Error::Invalid);
	}
	let root = std::path::Path::new(path);
	if !root.is_absolute() {
		return Err(Error::Failed(format!(
			"disk cache path must be absolute: {path}"
		)));
	}
	std::fs::create_dir_all(root)
		.map_err(|e| Error::Failed(format!("create disk cache dir {path}: {e}")))?;
	quercus_core::configstore::ConfigStore::instance().set(None, DISK_CACHE_PATH_KEY, path);
	Ok(())
}

/// Bytes consumed by the default disk cache folder (direct filesystem
/// scan; the C++ DiskManager index is replaced by the folder walk).
pub fn disk_cache_size() -> Result<i64> {
	let path = disk_cache_path();
	let root = std::path::Path::new(&path);
	if !root.exists() {
		return Ok(0);
	}
	let mut total: i64 = 0;
	for entry in walk(root) {
		total = total.saturating_add(entry.metadata().map(|m| m.len() as i64).unwrap_or(0));
	}
	Ok(total)
}

/// Clear the default disk cache folder (C++ `DiskManager::
/// clear_disk_cache`).
pub fn disk_cache_clear() -> Result<()> {
	let path = disk_cache_path();
	let root = std::path::Path::new(&path);
	if root.exists() {
		std::fs::remove_dir_all(root)
			.map_err(|e| Error::Failed(format!("clear disk cache: {e}")))?;
	}
	std::fs::create_dir_all(root)
		.map_err(|e| Error::Failed(format!("recreate disk cache: {e}")))?;
	Ok(())
}

/// Prune the default disk cache directory down to `max_bytes`, deleting the
/// oldest files (by mtime) first; returns the bytes freed.
///
/// `max_bytes <= 0` disables pruning and returns `0`. Missing directories are
/// a no-op. See [`prune_dir`] for the eviction rules.
pub fn prune_disk_cache_to(max_bytes: i64) -> Result<i64> {
	prune_dir(std::path::Path::new(&disk_cache_path()), max_bytes)
}

/// Enforce the configured disk-cache quota after a cache write.
///
/// No-op when [`disk_cache_max_bytes`] is `0`. The directory walk is
/// debounced so per-frame writes do not rescan the cache tree every time: a
/// sweep runs at most once every [`DISK_CACHE_PRUNE_INTERVAL`] (2 s), or
/// immediately every [`DISK_CACHE_PRUNE_WRITES`] (64) writes, so a burst of
/// frames may overshoot the budget by at most that many files.
///
/// Best-effort: failures are ignored (the next sweep retries) and only the
/// filesystem plus config are touched — no cache lock is taken, so callers
/// may invoke this while holding one without deadlock risk.
pub fn enforce_disk_cache_quota() {
	let max = disk_cache_max_bytes();
	if max <= 0 {
		return;
	}
	let writes = DISK_CACHE_WRITES
		.fetch_add(1, Ordering::AcqRel)
		.wrapping_add(1);
	{
		let mut last = lock(&DISK_CACHE_LAST_PRUNE);
		let now = Instant::now();
		let due = match *last {
			None => true,
			Some(at) => now.saturating_duration_since(at) >= DISK_CACHE_PRUNE_INTERVAL,
		};
		if !due && writes % DISK_CACHE_PRUNE_WRITES != 0 {
			return;
		}
		// Stamp before the walk and drop the lock: the sweep must not hold
		// the debounce mutex, and stamping first keeps concurrent writers
		// from all starting a sweep.
		*last = Some(now);
	}
	let _ = prune_disk_cache_to(max);
}

/// Reset the [`enforce_disk_cache_quota`] debounce (used after the budget is
/// changed so it takes effect immediately).
fn reset_quota_debounce() {
	DISK_CACHE_WRITES.store(0, Ordering::Release);
	*lock(&DISK_CACHE_LAST_PRUNE) = None;
}

/// Prune `dir` down to `max_bytes`, deleting the oldest files (by mtime)
/// first; returns the bytes freed.
///
/// `max_bytes <= 0` disables pruning and returns `0`; a missing directory is
/// a no-op. Only files are deleted — directories are never removed and files
/// whose metadata cannot be read (or that cannot be deleted) are skipped and
/// left in place. A file with an unreadable mtime is treated as oldest. The
/// newest file is always kept even when it alone exceeds the budget, so the
/// function can return with the directory still over `max_bytes`.
fn prune_dir(dir: &std::path::Path, max_bytes: i64) -> Result<i64> {
	if max_bytes <= 0 || !dir.exists() {
		return Ok(0);
	}

	// (mtime, path, size), oldest first. `walk` returns directory order,
	// which is filesystem-dependent, so sort explicitly before evicting.
	let mut entries: Vec<(std::time::SystemTime, std::path::PathBuf, i64)> = Vec::new();
	let mut total: i64 = 0;
	for path in walk(dir) {
		let meta = match std::fs::metadata(&path) {
			Ok(m) => m,
			Err(_) => continue, // unstattable: never delete blind
		};
		if !meta.is_file() {
			continue;
		}
		let size = meta.len().min(i64::MAX as u64) as i64;
		let mtime = meta.modified().unwrap_or(std::time::UNIX_EPOCH);
		total = total.saturating_add(size);
		entries.push((mtime, path, size));
	}
	if total <= max_bytes {
		return Ok(0);
	}
	entries.sort_by_key(|(mtime, _, _)| *mtime);

	// Keep the newest entry unconditionally: `entries` is non-empty here
	// (total > max_bytes >= 0), so `take(len - 1)` leaves at least one file.
	let mut remaining = total;
	let mut freed: i64 = 0;
	for (_, path, size) in entries.iter().take(entries.len() - 1) {
		if remaining <= max_bytes {
			break;
		}
		if std::fs::remove_file(path).is_err() {
			continue; // in use / permission denied: leave it for next time
		}
		freed = freed.saturating_add(*size);
		remaining = remaining.saturating_sub(*size);
	}
	Ok(freed)
}

/// Recursively walk a directory (files only).
fn walk(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
	let mut out = Vec::new();
	if let Ok(rd) = std::fs::read_dir(dir) {
		for entry in rd.flatten() {
			let path = entry.path();
			if path.is_dir() {
				out.extend(walk(&path));
			} else {
				out.push(path);
			}
		}
	}
	out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, MutexGuard};

    /// Serializes the manager-singleton tests (the singleton is global).
	static MANAGER_TEST_LOCK: Mutex<()> = Mutex::new(());

	fn manager_lock() -> MutexGuard<'static, ()> {
		MANAGER_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner())
	}

	#[test]
	fn init_shutdown_roundtrip() {
		let _lock = manager_lock();
		// Ensure a clean slate. The manager tests use the test-only inline
		// backend (the process backend spawns real quercus-worker children).
		RenderManager::shutdown();
		RenderManager::init_with_backend(RenderBackendChoice::Threads).unwrap();
		assert!(RenderManager::global().is_some());
		// Idempotence: second init is a state error.
		assert_eq!(
			RenderManager::init_with_backend(RenderBackendChoice::Threads)
				.unwrap_err()
				.code(),
			Error::State.code()
		);
		RenderManager::shutdown();
		assert!(RenderManager::global().is_none());
		// Re-init works after shutdown (C++ destroy_instance semantics).
		RenderManager::init_with_backend(RenderBackendChoice::Threads).unwrap();
		RenderManager::shutdown();
	}

	#[test]
	fn aggressive_gc_toggle() {
		let _lock = manager_lock();
		RenderManager::shutdown();
		RenderManager::init_with_backend(RenderBackendChoice::Threads).unwrap();
		let m = RenderManager::global().unwrap();
		assert!(!m.aggressive_gc());
		m.set_aggressive_gc(true);
		assert!(m.aggressive_gc());
		RenderManager::shutdown();
	}

	#[test]
	fn cacher_is_lazily_created() {
		let _lock = manager_lock();
		RenderManager::shutdown();
		RenderManager::init_with_backend(RenderBackendChoice::Threads).unwrap();
		let m = RenderManager::global().unwrap();
		{
			let g = m.get_cacher();
			assert!(g.is_some());
		}
		RenderManager::shutdown();
	}

	#[test]
	fn disk_cache_size_and_clear() {
		let _guard = quercus_core::commonutil::ENV_TEST_LOCK
			.lock()
			.unwrap_or_else(|e| e.into_inner());
		let dir = std::env::temp_dir().join("quercusrender-diskcache-test");
		std::env::set_var("QUERCUS_CONFIG_DIR", &dir);
		std::fs::create_dir_all(dir.join("mediacache").join("sub")).unwrap();
		std::fs::write(dir.join("mediacache").join("sub").join("a.bin"), [1u8; 100]).unwrap();
		assert_eq!(disk_cache_size().unwrap(), 100);
		disk_cache_clear().unwrap();
		assert_eq!(disk_cache_size().unwrap(), 0);
		assert!(dir.join("mediacache").exists());
		std::fs::remove_dir_all(&dir).ok();
		std::env::remove_var("QUERCUS_CONFIG_DIR");
	}

	#[test]
	fn disk_cache_size_missing_dir_is_zero() {
		let _guard = quercus_core::commonutil::ENV_TEST_LOCK
			.lock()
			.unwrap_or_else(|e| e.into_inner());
		let dir = std::env::temp_dir().join("quercusrender-diskcache-missing");
		std::env::set_var("QUERCUS_CONFIG_DIR", &dir);
		let _ = std::fs::remove_dir_all(&dir);
		assert_eq!(disk_cache_size().unwrap(), 0);
		std::env::remove_var("QUERCUS_CONFIG_DIR");
	}

	#[test]
	fn backend_choice_env_selects_pipeline() {
		let _guard = quercus_core::commonutil::ENV_TEST_LOCK
			.lock()
			.unwrap_or_else(|e| e.into_inner());
		std::env::remove_var("QUERCUS_PIPELINE");
		assert!(matches!(
			backend_choice_from_env(),
			RenderBackendChoice::Processes(_)
		));
		std::env::set_var("QUERCUS_PIPELINE", "threads");
		assert!(matches!(
			backend_choice_from_env(),
			RenderBackendChoice::Pipeline
		));
		// Any other value keeps the default process pool.
		std::env::set_var("QUERCUS_PIPELINE", "processes");
		assert!(matches!(
			backend_choice_from_env(),
			RenderBackendChoice::Processes(_)
		));
		std::env::remove_var("QUERCUS_PIPELINE");
	}

	// ---- disk-cache quota ---------------------------------------------------

	/// Serializes the quota tests, which mutate the process-wide
	/// `DiskCachePath` / `DiskCacheMaxBytes` config entries. They also hold
	/// [`quercus_core::commonutil::ENV_TEST_LOCK`] because the config path
	/// depends on `QUERCUS_CONFIG_DIR`.
	static DISK_CACHE_TEST_LOCK: Mutex<()> = Mutex::new(());

	fn disk_cache_test_lock() -> MutexGuard<'static, ()> {
		DISK_CACHE_TEST_LOCK
			.lock()
			.unwrap_or_else(|e| e.into_inner())
	}

	/// A unique, empty temp directory for one quota test.
	fn quota_temp_dir(tag: &str) -> std::path::PathBuf {
		let dir =
			std::env::temp_dir().join(format!("quercusrender-quota-{}-{tag}", std::process::id()));
		let _ = std::fs::remove_dir_all(&dir);
		std::fs::create_dir_all(&dir).unwrap();
		dir
	}

	/// Write `size` zero bytes and sleep briefly so the next write gets a
	/// strictly newer mtime (`std` has no portable "set mtime").
	fn write_with_mtime_gap(path: &std::path::Path, size: usize) {
		std::fs::write(path, vec![0u8; size]).unwrap();
		std::thread::sleep(std::time::Duration::from_millis(10));
	}

	/// Sum the regular files under `dir` (mirrors [`disk_cache_size`] but
	/// for an explicit directory).
	fn dir_file_size(dir: &std::path::Path) -> i64 {
		walk(dir)
			.into_iter()
			.filter_map(|p| std::fs::metadata(&p).ok())
			.filter(|m| m.is_file())
			.map(|m| m.len() as i64)
			.sum()
	}

	fn save_disk_cache_config() -> (Option<String>, i64) {
		let store = quercus_core::configstore::ConfigStore::instance();
		(
			store.get(None, DISK_CACHE_PATH_KEY).ok(),
			disk_cache_max_bytes(),
		)
	}

	fn restore_disk_cache_config(saved: (Option<String>, i64)) {
		let store = quercus_core::configstore::ConfigStore::instance();
		store.set(None, DISK_CACHE_PATH_KEY, saved.0.as_deref().unwrap_or(""));
		store.set_int64(None, DISK_CACHE_MAX_BYTES_KEY, saved.1);
		reset_quota_debounce();
	}

	#[test]
	fn prune_dir_removes_oldest_files_first() {
		let dir = quota_temp_dir("oldest");
		for name in ["a", "b", "c", "d"] {
			write_with_mtime_gap(&dir.join(name), 100);
		}
		let freed = prune_dir(&dir, 250).unwrap();
		// 400 bytes -> evict a and b (200 bytes) -> 200 <= 250.
		assert_eq!(freed, 200);
		assert!(!dir.join("a").exists(), "oldest file evicted");
		assert!(!dir.join("b").exists(), "second-oldest evicted");
		assert!(dir.join("c").exists());
		assert!(dir.join("d").exists(), "newest file kept");
		assert_eq!(dir_file_size(&dir), 200);
		std::fs::remove_dir_all(&dir).ok();
	}

	#[test]
	fn prune_dir_keeps_newest_file_over_budget() {
		let dir = quota_temp_dir("newest");
		write_with_mtime_gap(&dir.join("old"), 100);
		write_with_mtime_gap(&dir.join("new"), 100);
		// Budget below a single file: evict everything but the newest.
		assert_eq!(prune_dir(&dir, 10).unwrap(), 100);
		assert!(!dir.join("old").exists());
		assert!(dir.join("new").exists());
		assert_eq!(dir_file_size(&dir), 100);
		// With only the newest left it is retained even though over budget.
		assert_eq!(prune_dir(&dir, 10).unwrap(), 0);
		assert!(dir.join("new").exists());
		std::fs::remove_dir_all(&dir).ok();
	}

	#[test]
	fn prune_dir_zero_or_negative_budget_is_unlimited() {
		let dir = quota_temp_dir("unlimited");
		write_with_mtime_gap(&dir.join("a"), 100);
		write_with_mtime_gap(&dir.join("b"), 100);
		assert_eq!(prune_dir(&dir, 0).unwrap(), 0);
		assert_eq!(prune_dir(&dir, -5).unwrap(), 0);
		assert!(dir.join("a").exists());
		assert!(dir.join("b").exists());
		std::fs::remove_dir_all(&dir).ok();
	}

	#[test]
	fn prune_dir_missing_directory_is_zero() {
		let dir = quota_temp_dir("missing").join("not-there");
		assert_eq!(prune_dir(&dir, 100).unwrap(), 0);
		assert!(!dir.exists());
	}

	#[test]
	fn prune_disk_cache_to_missing_configured_dir_is_zero() {
		let _local = disk_cache_test_lock();
		let _env = quercus_core::commonutil::ENV_TEST_LOCK
			.lock()
			.unwrap_or_else(|e| e.into_inner());
		let saved = save_disk_cache_config();
		let dir = quota_temp_dir("configured-missing").join("not-there");
		quercus_core::configstore::ConfigStore::instance().set(
			None,
			DISK_CACHE_PATH_KEY,
			&dir.to_string_lossy(),
		);
		assert_eq!(prune_disk_cache_to(100).unwrap(), 0);
		restore_disk_cache_config(saved);
	}

	#[test]
	fn set_max_bytes_writes_config_and_enforces() {
		let _local = disk_cache_test_lock();
		let _env = quercus_core::commonutil::ENV_TEST_LOCK
			.lock()
			.unwrap_or_else(|e| e.into_inner());
		let saved = save_disk_cache_config();
		let dir = quota_temp_dir("config");
		for name in ["a", "b", "c", "d"] {
			write_with_mtime_gap(&dir.join(name), 100);
		}

		set_disk_cache_path(&dir.to_string_lossy()).unwrap();
		assert_eq!(
			disk_cache_path(),
			dir.to_string_lossy().into_owned(),
			"DiskCachePath points at the temp dir"
		);
		assert_eq!(
			disk_cache_size().unwrap(),
			400,
			"budget disabled before set"
		);

		// A zero budget makes enforcement a no-op even after a write event.
		enforce_disk_cache_quota();
		assert_eq!(disk_cache_size().unwrap(), 400);

		set_disk_cache_max_bytes(250).unwrap();
		assert_eq!(disk_cache_max_bytes(), 250);
		assert_eq!(
			quercus_core::configstore::ConfigStore::instance()
				.get(None, DISK_CACHE_MAX_BYTES_KEY)
				.unwrap(),
			"250",
			"config key written"
		);
		assert!(
			disk_cache_size().unwrap() <= 250,
			"setter enforced the quota immediately"
		);
		assert!(!dir.join("a").exists());
		assert!(!dir.join("b").exists());
		assert!(dir.join("c").exists());
		assert!(dir.join("d").exists());

		// Negative values clamp to 0 (unlimited) and disable pruning.
		set_disk_cache_max_bytes(-7).unwrap();
		assert_eq!(disk_cache_max_bytes(), 0);
		assert_eq!(
			quercus_core::configstore::ConfigStore::instance().get_int64(
				None,
				DISK_CACHE_MAX_BYTES_KEY,
				-1
			),
			0
		);

		restore_disk_cache_config(saved);
		std::fs::remove_dir_all(&dir).ok();
	}

	#[test]
	fn set_disk_cache_path_validates_and_creates() {
		let _local = disk_cache_test_lock();
		let _env = quercus_core::commonutil::ENV_TEST_LOCK
			.lock()
			.unwrap_or_else(|e| e.into_inner());
		let saved = save_disk_cache_config();
		let base = quota_temp_dir("path");
		let target = base.join("nested").join("cache");

		assert_eq!(
			set_disk_cache_path("   ").unwrap_err().code(),
			Error::Invalid.code()
		);
		assert_eq!(
			set_disk_cache_path("relative/cache").unwrap_err().code(),
			Error::Failed("".into()).code()
		);
		set_disk_cache_path(&target.to_string_lossy()).unwrap();
		assert!(target.is_dir(), "setter creates the directory");
		assert_eq!(disk_cache_path(), target.to_string_lossy().into_owned());

		restore_disk_cache_config(saved);
		std::fs::remove_dir_all(&base).ok();
	}
}
