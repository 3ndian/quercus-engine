//! Safe wrapper over the engine's render C ABI (`quercus_render_*`):
//! render a project's viewer frame (CPU, F32 RGBA) and manage the disk
//! cache.
//!
//! [`render_frame_f32`] is the headless frame-grab a functional plugin
//! builds on. OFX effect nodes on the graph need the plugin executor
//! installed first ([`crate::ofx::scan`] +
//! [`crate::ofx::install_render_executor`]).

use std::ffi::c_void;

use crate::codec::string_out;
use crate::node::{NodeId, Project};
use crate::types::Rational;

#[cfg_attr(
	windows,
	link(name = "quercus_engine.dll.lib", kind = "dylib", modifiers = "+verbatim")
)]
#[cfg_attr(not(windows), link(name = "quercus_engine", kind = "dylib"))]
unsafe extern "C" {
	fn quercus_render_frame_f32(
		project: *const c_void,
		viewer: u64,
		time: Rational,
		width: i32,
		height: i32,
		dst: *mut f32,
		dst_capacity: usize,
	) -> i32;
	fn quercus_render_disk_cache_path(buf: *mut u8, buf_len: usize) -> i32;
	fn quercus_render_disk_cache_size() -> i64;
	fn quercus_render_disk_cache_clear() -> bool;
	fn quercus_render_disk_cache_max_bytes() -> i64;
	fn quercus_render_disk_cache_set_max_bytes(max_bytes: i64) -> bool;
	fn quercus_render_disk_cache_set_path(path: *const u8, path_len: usize) -> bool;
	fn quercus_render_disk_cache_prune() -> i64;
	fn quercus_render_manager_init(backend: i32) -> bool;
	fn quercus_render_manager_is_initialized() -> bool;
	fn quercus_render_manager_shutdown();
	fn quercus_render_manager_poll();
	fn quercus_render_manager_set_workspace_size(width: i32, height: i32, fps: u32);
	fn quercus_render_manager_set_inline_project(project: *const c_void) -> bool;
	fn quercus_render_manager_set_graph_snapshot(project: *const c_void, revision: u64) -> bool;
	fn quercus_render_manager_resync_graph_snapshot(project: *const c_void, revision: u64) -> bool;
	fn quercus_render_manager_clear_graph_snapshot();
	fn quercus_render_gpu_install(
		wgpu_device: *const c_void,
		wgpu_queue: *const c_void,
		backend_kind: i32,
	) -> bool;
	fn quercus_render_gpu_kind() -> i32;
	fn quercus_render_gpu_is_adopted() -> bool;
	fn quercus_render_gpu_host_installed() -> bool;
	fn quercus_render_gpu_mark_host() -> bool;
	fn quercus_render_textengine_install();
	fn quercus_render_textengine_add_font_data(bytes: *const u8, len: usize) -> bool;
	fn quercus_render_textengine_add_font_dir(path: *const u8, path_len: usize) -> bool;
	fn quercus_render_textengine_font_family_count() -> i32;
	fn quercus_render_textengine_font_family_at(index: i32, buf: *mut u8, buf_len: usize) -> i32;
	fn quercus_render_swizzle_bgra8_to_rgba8(src: *const u8, dst: *mut u8, pixel_count: usize) -> bool;
}

/// Renders a sequence or video-footage viewer at `time`, returning tight
/// F32 RGBA pixels (`width*height*4` floats). None on failure (unknown
/// viewer, invalid size, render error).
///
/// **Color space**: the returned samples are in the project's *working*
/// color space (ACEScg linear by default), not display/sRGB. Writing them
/// straight to PNG makes the image look dark (a working-space 0.18 can map
/// to a very different display code). Apply
/// [`crate::utils::working_to_display_target`] with the project's working
/// and delivery color spaces before display, or use the same transform the
/// export pipeline uses.
pub fn render_frame_f32(
	project: &Project,
	viewer: NodeId,
	time: Rational,
	size: (u32, u32),
) -> Option<Vec<f32>> {
	let (w, h) = size;
	if w == 0 || h == 0 {
		return None;
	}
	let mut dst = vec![0.0f32; (w * h * 4) as usize];
	let written = unsafe {
		quercus_render_frame_f32(
			project.as_raw(),
			viewer.raw(),
			time,
			w as i32,
			h as i32,
			dst.as_mut_ptr(),
			dst.len(),
		)
	};
	if written < 0 {
		return None;
	}
	dst.truncate(written as usize);
	Some(dst)
}

/// The disk cache directory.
pub fn disk_cache_path() -> String {
	string_out(|buf, len| unsafe { quercus_render_disk_cache_path(buf, len) }).unwrap_or_default()
}

/// The disk cache size in bytes.
pub fn disk_cache_size() -> Option<i64> {
	let size = unsafe { quercus_render_disk_cache_size() };
	(size >= 0).then_some(size)
}

/// Clears the disk cache.
pub fn disk_cache_clear() -> bool {
	unsafe { quercus_render_disk_cache_clear() }
}

/// The configured disk-cache byte budget (`0` = unlimited).
pub fn disk_cache_max_bytes() -> i64 {
	unsafe { quercus_render_disk_cache_max_bytes() }
}

/// Sets the disk-cache byte budget (`0` = unlimited) and prunes immediately.
pub fn disk_cache_set_max_bytes(max_bytes: i64) -> bool {
	unsafe { quercus_render_disk_cache_set_max_bytes(max_bytes) }
}

/// Points the disk cache at `path` (absolute; created when missing). Only
/// caches created afterwards use it.
pub fn disk_cache_set_path(path: &str) -> bool {
	unsafe { quercus_render_disk_cache_set_path(path.as_ptr(), path.len()) }
}

/// Prunes the disk cache to the configured budget; returns bytes freed.
pub fn disk_cache_prune() -> Option<i64> {
	let freed = unsafe { quercus_render_disk_cache_prune() };
	(freed >= 0).then_some(freed)
}

/// The render backend for [`manager_init`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RenderBackend {
	/// The `QUERCUS_PIPELINE` environment default (the process pool unless
	/// overridden).
	Default,
	/// The inline backend: jobs run synchronously on the calling
	/// thread — the only backend that works without the quercus-worker
	/// binary (e.g. a plugin process).
	Threads,
	/// The M1 thread pipeline (one render thread + one decode thread,
	/// in-process).
	Pipeline,
	/// The process-isolated quercus-worker pool (needs the quercus-worker
	/// binary).
	Processes,
}

/// Initializes the process-wide render manager. The render/export
/// tasks ([`crate::task::export`]) need a manager — or a worker pool —
/// to render. False when already initialized.
pub fn manager_init(backend: RenderBackend) -> bool {
	let backend = match backend {
		RenderBackend::Default => 0,
		RenderBackend::Threads => 1,
		RenderBackend::Pipeline => 2,
		RenderBackend::Processes => 3,
	};
	unsafe { quercus_render_manager_init(backend) }
}

/// Whether the render manager is initialized.
pub fn manager_is_initialized() -> bool {
	unsafe { quercus_render_manager_is_initialized() }
}

/// Shuts the render manager down (drains pending work).
pub fn manager_shutdown() {
	unsafe { quercus_render_manager_shutdown() }
}

/// Pumps the manager's completion dispatcher; call once per UI tick.
pub fn manager_poll() {
	unsafe { quercus_render_manager_poll() }
}

/// Sizes the worker pool's workspace for the current sequence resolution.
pub fn manager_set_workspace_size(width: i32, height: i32, fps: u32) {
	unsafe { quercus_render_manager_set_workspace_size(width, height, fps) }
}

/// Points the inline backend at the live project (same shared project).
pub fn manager_set_inline_project(project: &Project) -> bool {
	unsafe { quercus_render_manager_set_inline_project(project.as_raw()) }
}

/// Uploads the project snapshot at `revision` to the worker pool.
pub fn manager_set_graph_snapshot(project: &Project, revision: u64) -> bool {
	unsafe { quercus_render_manager_set_graph_snapshot(project.as_raw(), revision) }
}

/// Re-uploads the snapshot although the undo revision did not change.
pub fn manager_resync_graph_snapshot(project: &Project, revision: u64) -> bool {
	unsafe { quercus_render_manager_resync_graph_snapshot(project.as_raw(), revision) }
}

/// Drops the manager's current snapshot (project close).
pub fn manager_clear_graph_snapshot() {
	unsafe { quercus_render_manager_clear_graph_snapshot() }
}

// ---------------------------------------------------------------------------
// GPU device sharing (zero-copy present)
// ---------------------------------------------------------------------------

/// The engine's wgpu backend selection (the C ABI `BackendKind` values).
#[repr(i32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GpuBackend {
	/// Resolve automatically (Metal → Vulkan → GL → CPU).
	Auto = 0,
	/// Metal (macOS primary).
	Metal = 1,
	/// Vulkan.
	Vulkan = 2,
	/// OpenGL (legacy fallback).
	Gl = 3,
	/// CPU fallback (no adapter).
	Cpu = 4,
}

impl GpuBackend {
	/// A `GpuBackend` from its C ABI discriminant; `None` outside 0..=4.
	pub const fn from_i32(value: i32) -> Option<Self> {
		match value {
			0 => Some(Self::Auto),
			1 => Some(Self::Metal),
			2 => Some(Self::Vulkan),
			3 => Some(Self::Gl),
			4 => Some(Self::Cpu),
			_ => None,
		}
	}
}

/// Hands the host's wgpu device/queue to the engine so the render thread
/// creates its textures on the presenter's device (zero-copy present).
///
/// `device` and `queue` must point to live `wgpu::Device` / `wgpu::Queue`
/// values from the **same wgpu major version (29)** the engine was built
/// with; mixing major versions is undefined behavior. Cast the
/// references, for example:
///
/// ```ignore
/// let device_ptr = (&device as *const wgpu::Device) as *const std::ffi::c_void;
/// let queue_ptr = (&queue as *const wgpu::Queue) as *const std::ffi::c_void;
/// let installed = unsafe { gpu_install(device_ptr, queue_ptr, GpuBackend::Vulkan) };
/// ```
///
/// The engine clones the handles and takes no ownership: the host keeps
/// the device/queue (the engine's clones keep them alive afterwards), and
/// only has to keep the values alive for the duration of this call.
///
/// **Timing**: call before the first render/thumbnail. The engine refuses
/// an install once its shared context has created GPU resources (returns
/// `false`; presentation then falls back to a staging readback), and
/// returns `false` for null pointers. This SDK deliberately does not
/// depend on wgpu, so the pointers cross as `c_void`.
///
/// # Safety
///
/// Both pointers must be non-null and point to live, aligned
/// `wgpu::Device` / `wgpu::Queue` values of the wgpu generation the
/// engine was built with.
pub unsafe fn gpu_install(
	device: *const c_void,
	queue: *const c_void,
	backend: GpuBackend,
) -> bool {
	// SAFETY: the caller upholds the pointer/lifetime contract above.
	unsafe { quercus_render_gpu_install(device, queue, backend as i32) }
}

/// The engine's active GPU backend, or `None` when no context exists.
pub fn gpu_kind() -> Option<GpuBackend> {
	GpuBackend::from_i32(unsafe { quercus_render_gpu_kind() })
}

/// True when the engine renders on a host-adopted device (zero-copy
/// present); false when no context exists or the engine created its own.
pub fn gpu_is_adopted() -> bool {
	unsafe { quercus_render_gpu_is_adopted() }
}

/// True when the host installed or marked the shared render device.
pub fn gpu_host_installed() -> bool {
	unsafe { quercus_render_gpu_host_installed() }
}

/// Declares the engine-created shared context as the host's render device
/// (macOS/Windows, where the windowing layer does not hand out its wgpu
/// device); creates the context on first use. False without an adapter.
pub fn gpu_mark_host() -> bool {
	unsafe { quercus_render_gpu_mark_host() }
}

/// Installs the text measure/render backends (idempotent; text nodes
/// render empty without them).
pub fn textengine_install() {
	unsafe { quercus_render_textengine_install() }
}

/// Adds a font file's bytes to the shared text backend; the next
/// [`font_families`] call includes its families. Empty data is rejected.
pub fn textengine_add_font_data(bytes: &[u8]) -> bool {
	if bytes.is_empty() {
		return false;
	}
	unsafe { quercus_render_textengine_add_font_data(bytes.as_ptr(), bytes.len()) }
}

/// Adds every font file under `path` (recursively) to the shared text
/// backend. False for a missing/non-directory path.
pub fn textengine_add_font_dir(path: &std::path::Path) -> bool {
	let Some(path) = path.to_str() else {
		return false;
	};
	unsafe { quercus_render_textengine_add_font_dir(path.as_ptr(), path.len()) }
}

/// System font families from the same backend used by render workers.
pub fn font_families() -> Vec<String> {
	let count = unsafe { quercus_render_textengine_font_family_count() }.max(0);
	(0..count)
		.filter_map(|index| {
			string_out(|buf, len| unsafe {
				quercus_render_textengine_font_family_at(index, buf, len)
			})
		})
		.collect()
}

/// Swizzles BGRA8 pixels to RGBA8 (buffers may not overlap).
pub fn swizzle_bgra8_to_rgba8(src: &[u8], dst: &mut [u8]) -> bool {
	if src.len() != dst.len() || !src.len().is_multiple_of(4) {
		return false;
	}
	unsafe { quercus_render_swizzle_bgra8_to_rgba8(src.as_ptr(), dst.as_mut_ptr(), src.len() / 4) }
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn render_empty_sequence_frame() {
		let project = Project::new();
		let sequence = project.create_sequence("Seq").expect("sequence");
		let frame = render_frame_f32(&project, sequence, Rational::new(0, 1), (16, 8))
			.expect("an empty sequence renders");
		assert_eq!(frame.len(), 16 * 8 * 4);
		assert!(frame.iter().all(|&v| v == 0.0), "transparent black");
		// An unknown viewer fails closed.
		assert!(
			render_frame_f32(&project, NodeId::INVALID, Rational::new(0, 1), (16, 8)).is_none()
		);
	}

	#[test]
	fn disk_cache_entry_points_are_safe() {
		assert!(!disk_cache_path().is_empty());
		let _ = disk_cache_size();
	}

	#[test]
	fn manager_and_swizzle() {
		let was = manager_is_initialized();
		if !was {
			assert!(manager_init(RenderBackend::Threads));
		}
		assert!(manager_is_initialized());
		assert!(
			!manager_init(RenderBackend::Threads),
			"double init rejected"
		);

		let src = [10u8, 20, 30, 255];
		let mut dst = [0u8; 4];
		assert!(swizzle_bgra8_to_rgba8(&src, &mut dst));
		assert_eq!(dst, [30, 20, 10, 255]);
		assert!(!swizzle_bgra8_to_rgba8(&src, &mut dst[..3]));
	}

	#[test]
	fn text_backend_install_and_font_families() {
		textengine_install();
		let families = font_families();
		assert!(!families.is_empty(), "the host exposes font families");
		assert!(families.iter().all(|name| !name.is_empty()));
		assert_eq!(families, font_families(), "the engine caches the enumeration");
	}
}
