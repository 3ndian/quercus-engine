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

//! GL 渲染桥端到端：真实 CGL 离屏上下文 + 输出纹理/FBO + 回读。
//!
//! 链路（方案 B，见 [`quercus_ofx_plugin::gl_bridge`] 模块文档）：扫描最小测试
//! 插件的 GL 变体（org.quercus.test-plugin.gl，声明 OpenGLRenderSupported=
//! "true" + F32）→ 用 GL 后端渲染器（fake `GpuContextLike`，kind=Gl）
//! 构造 RenderJob → `render_frame` 的 use_opengl 决策命中 → 桥建 CGL
//! 上下文 + 输出 GL 纹理 + FBO → 插件 render 走 GL 路径（attach/detach、
//! clipLoadTexture 拿真实纹理名、glClear 清成已知颜色）→ 宿主
//! glReadPixels 回读装帧。
//!
//! 断言即"真实 GL 渲染"的证据：
//! 1. 输出像素 = 插件 GL 清屏色 (0.1, 0.2, 0.3, 1.0)（CPU 路径恒填
//!    0.5，二者可区分）；
//! 2. 插件经 message 上报的 gl-source-index / gl-output-index 都是
//!    非零真实 GL 纹理名（CPU 桩恒 0）；
//! 3. gl-attached / gl-detached 动作发生（attach/detach 配对）。
//!
//! 门：macOS（Linux/Windows 的 GL 桥是 stub）+ `QUERCUS_GPU_TESTS`（GL
//! 验收约定，CI 一律跳过）。

mod common;

use std::ffi::{c_char, c_int, c_void, CStr};
use std::sync::Arc;

use quercus_core::backend::{BackendKind, GpuContextLike};
use quercus_core::texture::Frame;
use quercus_core::{PixelFormat, Rational};
use quercus_ofx_plugin::host::Host;
use quercus_ofx_plugin::render::{Renderer, Texture};

const GL_PLUGIN_ID: &str = "org.quercus.test-plugin.gl";

/// GL 后端渲染器（fake `GpuContextLike`；只声明 kind=Gl，让 use_opengl
/// 决策命中。GL 路径不用它的 upload/download/blit——输出是 CPU 帧，
/// 渲染发生在桥自建的 CGL 上下文）。
struct FakeGlRenderer;

impl GpuContextLike for FakeGlRenderer {
	fn kind(&self) -> BackendKind {
		BackendKind::Gl
	}
	fn destroy_texture(&self, _token: u64) {}
	fn upload(&self, _token: u64, _frame: &Frame) -> quercus_render::error::Result<()> {
		Ok(())
	}
	fn download(&self, _token: u64) -> quercus_render::error::Result<Frame> {
		Ok(Frame::new())
	}
	fn blit(
		&self,
		_src: u64,
		_dst: u64,
		_processor: Option<&quercus_core::color::ColorProcessor>,
	) -> quercus_render::error::Result<()> {
		Ok(())
	}
}

/// 捕获插件 message（gl-test 消息流）。
unsafe extern "C" fn capture_msg(
	type_: *const c_char,
	message: *const c_char,
	userdata: *mut c_void,
) -> c_int {
	let v = unsafe { &mut *(userdata as *mut Vec<(String, String)>) };
	v.push((
		unsafe { CStr::from_ptr(type_) }
			.to_string_lossy()
			.into_owned(),
		unsafe { CStr::from_ptr(message) }
			.to_string_lossy()
			.into_owned(),
	));
	1
}

fn first_pixel(texture: &Texture) -> [f32; 4] {
	let Texture::Cpu(frame) = texture else {
		panic!("期望 CPU 帧");
	};
	let mut out = [0f32; 4];
	for i in 0..4 {
		out[i] = f32::from_le_bytes(frame.data[i * 4..i * 4 + 4].try_into().unwrap());
	}
	out
}

/// 扫描 + 注册（幂等；宿主不可用返回 false = skip）。
fn scan_and_register() -> bool {
	let Some(dir) = common::test_plugin_scan_dir() else {
		common::skip("最小测试插件未构建");
		return false;
	};
	if Host::global().cache.scan_path(&dir).is_err() {
		common::skip("测试插件扫描失败");
		return false;
	}
	quercus_ofx_plugin::node_factory::register_plugin_nodes();
	true
}

/// GL 端到端：插件 GL 变体真实走 GL 路径并输出已知颜色。
#[test]
fn gl_plugin_renders_through_real_gl_path() {
	common::with_host(|| {
		if cfg!(target_os = "windows") {
			common::skip("Windows GL 桥仍是 stub");
			return;
		}
		if !common::gpu_available() {
			common::skip("GL 测试需 QUERCUS_GPU_TESTS（本机 GPU 验收；CI 一律跳过）");
			return;
		}
		if !quercus_ofx_plugin::gl_bridge::gl_available() {
			common::skip("本机无可用 GL 上下文");
			return;
		}
		if !scan_and_register() {
			return;
		}

		let mut captured: Vec<(String, String)> = Vec::new();
		quercus_ofx_plugin::suites::message::set_handler(
			Some(capture_msg),
			&mut captured as *mut _ as *mut c_void,
		);

		let inst = Host::global()
			.create_instance(GL_PLUGIN_ID, None)
			.expect("GL 变体实例应可建");
		let reg = quercus_ofx_plugin::node_factory::register_instance(inst.clone());

		let dst =
			quercus_render::eval::generate_frame(Rational::new(0, 1), (4, 4), PixelFormat::F32)
				.unwrap();
		let src =
			quercus_render::eval::generate_frame(Rational::new(0, 1), (4, 4), PixelFormat::F32)
				.unwrap();
		let renderer: Renderer = Arc::new(FakeGlRenderer);
		let job = quercus_ofx_plugin::render_driver::RenderJob {
			time: 0.0,
			dst: Texture::wrap_frame(dst),
			src: Some(Texture::wrap_frame(src)),
			effect_input_id: Some("Source".into()),
			inputs: Vec::new(),
			values: Vec::new(),
			renderer: Some(renderer),
			clear_destination: false,
			interactive: false,
		};
		let (out, _rois) = quercus_ofx_plugin::render_driver::render_frame(&inst.value, &job)
			.expect("GL render_frame 应成功");

		quercus_ofx_plugin::node_factory::unregister_instance(reg);
		quercus_ofx_plugin::suites::message::set_handler(None, std::ptr::null_mut());
		Host::global().shutdown();

		// 1. 输出像素 = 插件 GL 清屏色（CPU 路径恒 0.5，可区分）。
		let px = first_pixel(&out);
		for (i, v) in [0.1, 0.2, 0.3, 1.0].iter().enumerate() {
			assert!(
				(px[i] - v).abs() < 1e-4,
				"GL 回读像素[{i}] = {}，期望 {v}（GL 路径未生效？）",
				px[i]
			);
		}

		// 2. 真实 GL 纹理名（非零）+ 3. attach/detach 配对。
		let msg: Vec<&str> = captured.iter().map(|(_, m)| m.as_str()).collect();
		for need in [
			"gl-attached",
			"gl-detached",
			"gl-source-index=",
			"gl-output-index=",
		] {
			assert!(
				msg.iter().any(|m| m.contains(need)),
				"应捕获到 {need}，实际 {msg:?}"
			);
		}
		for m in msg.iter().filter(|m| m.contains("gl-source-index=")) {
			let v: i32 = m.rsplit('=').next().unwrap().parse().unwrap();
			assert!(
				v > 0,
				"输入 clip 的 OpenGLTextureIndex 应为真实非零 GL 名，got {v}"
			);
		}
		for m in msg.iter().filter(|m| m.contains("gl-output-index=")) {
			let v: i32 = m.rsplit('=').next().unwrap().parse().unwrap();
			assert!(
				v > 0,
				"输出 clip 的 OpenGLTextureIndex 应为真实非零 GL 名，got {v}"
			);
		}
	});
}

/// Linux 真正 GPU 零拷贝：真实 wgpu/Vulkan 上下文 + GPU 输入/输出，
/// 插件 GL 渲染后输出仍是同一个 GPU token，且渲染期间没有 download。
#[cfg(target_os = "linux")]
#[test]
fn gl_plugin_renders_zero_copy_on_linux() {
	common::with_host(|| {
		if !common::gpu_available() {
			common::skip("零拷贝 GL 测试需 QUERCUS_GPU_TESTS");
			return;
		}
		let Some(ctx) = quercus_core::backend::gpu_or_skip("OFX zero-copy") else {
			return;
		};
		if !ctx.supports_gl_zerocopy() {
			common::skip("当前 Vulkan 上下文不支持 OPAQUE_FD 导出");
			return;
		}
		if !quercus_ofx_plugin::gl_bridge::gl_available() {
			common::skip("本机无可用 EGL/GL 上下文");
			return;
		}
		if !scan_and_register() {
			return;
		}

		let inst = Host::global()
			.create_instance(GL_PLUGIN_ID, None)
			.expect("GL 变体实例应可建");
		let reg = quercus_ofx_plugin::node_factory::register_instance(inst.clone());

		let (w, h) = (64, 64);
		let src_token = ctx.create_texture(w, h).expect("src texture");
		let mut src_frame =
			quercus_render::eval::generate_frame(Rational::new(0, 1), (w, h), PixelFormat::F32)
				.expect("source frame");
		for px in src_frame.data.chunks_exact_mut(16) {
			px[0..4].copy_from_slice(&0.05f32.to_le_bytes());
			px[4..8].copy_from_slice(&0.10f32.to_le_bytes());
			px[8..12].copy_from_slice(&0.15f32.to_le_bytes());
			px[12..16].copy_from_slice(&1.0f32.to_le_bytes());
		}
		ctx.upload(src_token, &src_frame).expect("src upload");
		let dst_token = ctx.create_texture(w, h).expect("dst texture");
		let src = Texture::gpu(ctx.clone(), src_token, w, h, PixelFormat::F32);
		let dst = Texture::gpu(ctx.clone(), dst_token, w, h, PixelFormat::F32);
		let renderer: Renderer = Arc::new(quercus_ofx_plugin::render::GlKindMarker);
		let job = quercus_ofx_plugin::render_driver::RenderJob {
			time: 0.0,
			dst: dst.clone(),
			src: Some(src),
			effect_input_id: Some("Source".into()),
			inputs: Vec::new(),
			values: Vec::new(),
			renderer: Some(renderer),
			clear_destination: false,
			interactive: false,
		};

		let mut captured: Vec<(String, String)> = Vec::new();
		quercus_ofx_plugin::suites::message::set_handler(
			Some(capture_msg),
			&mut captured as *mut _ as *mut c_void,
		);
		let fds_before = std::fs::read_dir("/proc/self/fd")
			.map(|d| d.count())
			.unwrap_or(0);
		let before = quercus_core::backend::gpu_transfer_counters();
		let readbacks_before = quercus_ofx_plugin::gl_bridge::gl_readback_count();
		let mut out = None;
		for _ in 0..25 {
			let (rendered, _rois) =
				quercus_ofx_plugin::render_driver::render_frame(&inst.value, &job)
					.expect("zero-copy render_frame");
			out = Some(rendered);
		}
		let out = out.expect("at least one frame");
		let fds_after = std::fs::read_dir("/proc/self/fd")
			.map(|d| d.count())
			.unwrap_or(0);
		assert!(
			fds_after <= fds_before + 4,
			"zero-copy leaked file descriptors: {fds_before} -> {fds_after}"
		);
		quercus_ofx_plugin::suites::message::set_handler(None, std::ptr::null_mut());
		let after = quercus_core::backend::gpu_transfer_counters();
		assert_eq!(
			quercus_ofx_plugin::gl_bridge::gl_readback_count(),
			readbacks_before,
			"zero-copy must not call glReadPixels"
		);
		assert_eq!(after.0, before.0, "zero-copy must not upload a CPU frame");
		assert_eq!(after.1, before.1, "zero-copy must not download a CPU frame");
		let Texture::Gpu { token, .. } = &out else {
			panic!("zero-copy output must stay a GPU texture");
		};
		assert_eq!(*token, dst_token, "output must be the destination token");

		// The output must be sampleable by the next wgpu graph node (not
		// just copyable). A GPU blit exercises the RESOURCE transition and
		// catches the UNINITIALIZED-discard bug.
		let sampled = ctx.create_texture(w, h).expect("sampled texture");
		ctx.blit(dst_token, sampled, None)
			.expect("downstream GPU sample/blit");
		ctx.wait_idle();
		let sampled_frame = ctx.download(sampled).expect("download sampled output");
		let sampled_px: Vec<f32> = sampled_frame.data[0..16]
			.chunks_exact(4)
			.map(|c| f32::from_le_bytes(c.try_into().unwrap()))
			.collect();
		for (i, v) in [0.1, 0.2, 0.3, 1.0].iter().enumerate() {
			assert!(
				(sampled_px[i] - v).abs() < 1e-4,
				"downstream sample pixel[{i}] = {} expected {v}",
				sampled_px[i]
			);
		}
		ctx.destroy_texture(sampled);

		// Test-only readback to verify the plugin's GL clear color.
		let frame = ctx.download(dst_token).expect("download zero-copy output");
		let px: Vec<f32> = frame.data[0..16]
			.chunks_exact(4)
			.map(|c| f32::from_le_bytes(c.try_into().unwrap()))
			.collect();
		for (i, v) in [0.1, 0.2, 0.3, 1.0].iter().enumerate() {
			assert!(
				(px[i] - v).abs() < 1e-4,
				"zero-copy pixel[{i}] = {} expected {v}",
				px[i]
			);
		}

		quercus_ofx_plugin::node_factory::unregister_instance(reg);
		Host::global().shutdown();
		ctx.destroy_texture(src_token);
		ctx.destroy_texture(dst_token);
	});
}

/// CPU 回退对照：无 GL 渲染器（renderer=None）时，GL 变体插件走 CPU
/// 路径（actionRenderGL 的 !gl_enabled 回退 actionRender），输出恒
/// 0.5——与 [`gl_plugin_renders_through_real_gl_path`] 的 0.1,0.2,0.3
/// 区分，证明 GL 分支只在 renderer 为 GL 时启用。
#[test]
fn gl_plugin_falls_back_to_cpu_without_gl_renderer() {
	common::with_host(|| {
		if !scan_and_register() {
			return;
		}
		let mut captured: Vec<(String, String)> = Vec::new();
		quercus_ofx_plugin::suites::message::set_handler(
			Some(capture_msg),
			&mut captured as *mut _ as *mut c_void,
		);

		let inst = Host::global()
			.create_instance(GL_PLUGIN_ID, None)
			.expect("GL 变体实例应可建");
		let reg = quercus_ofx_plugin::node_factory::register_instance(inst.clone());

		let dst =
			quercus_render::eval::generate_frame(Rational::new(0, 1), (4, 4), PixelFormat::F32)
				.unwrap();
		let src =
			quercus_render::eval::generate_frame(Rational::new(0, 1), (4, 4), PixelFormat::F32)
				.unwrap();
		let job = quercus_ofx_plugin::render_driver::RenderJob {
			time: 0.0,
			dst: Texture::wrap_frame(dst),
			src: Some(Texture::wrap_frame(src)),
			effect_input_id: Some("Source".into()),
			inputs: Vec::new(),
			values: Vec::new(),
			renderer: None,
			clear_destination: false,
			interactive: false,
		};
		let (out, _rois) = quercus_ofx_plugin::render_driver::render_frame(&inst.value, &job)
			.expect("CPU 回退 render_frame 应成功");

		quercus_ofx_plugin::node_factory::unregister_instance(reg);
		quercus_ofx_plugin::suites::message::set_handler(None, std::ptr::null_mut());
		Host::global().shutdown();

		let px = first_pixel(&out);
		assert_eq!(px, [0.5, 0.5, 0.5, 1.0], "无 GL 渲染器应走 CPU 恒 0.5");
		// 未走 GL：不应有 gl-* 消息流。
		assert!(
			!captured.iter().any(|(_, m)| m.contains("gl-output-index")),
			"CPU 回退不应产生 GL suite 消息"
		);
	});
}
