# OpenFX 真正 GPU 零拷贝实施计划

状态：Linux 第一版已实现并通过本机验收（未发布）
目标平台：第一闭环 Linux/Vulkan + EGL；macOS/Windows 随后
目标版本：0.7.0 前置能力（不发布，等验收通过）

> **阶段 0 实测结论（2026-11-04，RTX 5070 Ti / NVIDIA 615.78.08）**
>
> NVIDIA EGL 的 `EGL_EXT_image_dma_buf_import` 列出 54 个格式，但
> **不包含任何 32F 格式**（只有 `AB4H` 等 16F），因此
> `RGBA32F + DMA-BUF + EGLImage` 在本机直接返回 `EGL_BAD_MATCH`。
>
> 同机验证通过的路径是 **Vulkan `OPAQUE_FD` + GL `GL_EXT_memory_object_fd`**：
>
> - Vulkan 创建 `VK_FORMAT_R32G32B32A32_SFLOAT`、`OPAQUE_FD` exportable image；
> - `vkGetMemoryFdKHR(OPAQUE_FD)`；
> - GL `glImportMemoryFdEXT(..., GL_HANDLE_TYPE_OPAQUE_FD_EXT, fd)`；
> - `glTexStorageMem2DEXT(GL_TEXTURE_2D, 1, GL_RGBA32F, w, h, mem, 0)`；
> - FBO 完整、`glClear` 后 `glReadPixels` 正确；
> - Vulkan `vkCmdCopyImageToBuffer` 读回同一张图，像素值一致。
>
> 因此实现主路径改为 **OPAQUE_FD + GL memory object**；DMA-BUF/EGLImage
> 保留为后续 Linux/AMD/Mesa 的可选路径，不再作为第一版主路径。

## 1. 目标定义

“真正 GPU 零拷贝”在本计划里只有一个含义：

- OFX 插件的输入纹理和输出纹理始终是 **GPU 显存里的同一块图像内存**；
- 宿主不执行 `glReadPixels`，不构造 CPU `Frame`，不做 `upload`/`download`；
- 插件用 GL 纹理名直接读写该内存；
- 输出纹理回到 wgpu 后可直接被后续节点采样/合成；
- 允许 GPU 同步（`vkQueueWaitIdle` + `glFinish` 作为第一版同步手段），
  但同步不是拷贝。

明确不算零拷贝：

- 进程内调用（已经完成）；
- 共享内存/管道传递（已经移除的 worker 路径）；
- `glReadPixels` 后重新上传；
- GPU→GPU blit 到另一张纹理（这是拷贝，不是零拷贝）。

## 2. 当前代码事实（为什么现在还不是零拷贝）

1. `crates/quercus-ofx-plugin/src/node_factory.rs::execute_plugin_job`
   用 `generate_frame` 生成 CPU F32 输出帧，再 `Texture::wrap_frame`。
   插件输出从源头上就是 CPU 帧。
2. `crates/quercus-ofx-plugin/src/render_driver.rs::render_gl_frame`
   为每帧 `create_output_texture` → 插件画进 FBO → `read_pixels_to_image`
   → `write_output_frame`。这是标准的 GL 回读路径。
3. `crates/quercus-ofx-plugin/src/clip.rs::fetch_image`
   对 GPU 输入调用 `texture_get_frame`，即后端下载；随后
   `clip_load_texture` 再用 `create_input_texture` 上传回 GL。
4. `crates/quercus-ofx-plugin/src/gl_bridge.rs`
   的 Linux 实现是 stub；只有 macOS 有 CGL 真实实现。
5. `crates/quercus-core/src/backend.rs::GpuContext::create_texture`
   走普通 `wgpu::Device::create_texture`，创建的 VkImage 没有
   `VK_EXTERNAL_MEMORY_HANDLE_TYPE_DMA_BUF_BIT_EXT`，不能导出 DMA-BUF。
6. `crates/quercus-core/src/backend/external.rs` 只有“导入”DMA-BUF，
   没有“导出”路径。

结论：只在 `gl_bridge` 里加 EGL 不够，必须同时改 **输出纹理来源**、
**输入纹理获取方式**、**wgpu 纹理分配方式** 三处。

## 3. 目标数据流（Linux 第一版）

```text
上一节点（wgpu/Vulkan）
  └─ 外部内存 VkImage（R32G32B32A32_SFLOAT, OPTIMAL, OPAQUE_FD exportable）
       ├─ wgpu texture_from_raw（后续节点可采样/渲染）
       └─ vkGetMemoryFdKHR → opaque fd
             └─ GL glImportMemoryFdEXT + glTexStorageMem2DEXT
                   └─ GL 纹理名（同一块显存）
                         └─ OFX clipLoadTexture 返回真实 GL 名

OFX 插件
  └─ 直接渲染到输出 GL 纹理（同一块显存）
       └─ glFinish
             └─ 同一 VkImage 已是插件输出
                   └─ 后续 wgpu 节点直接采样，无回读/无上传
```

### 3.1 格式与布局

第一版固定：

- 像素格式：`wgpu::TextureFormat::Rgba32Float`
- Vulkan 格式：`VK_FORMAT_R32G32B32A32_SFLOAT`
- GL 内部格式：`GL_RGBA32F`
- 布局：`VK_IMAGE_TILING_OPTIMAL`
- 外部句柄：`VK_EXTERNAL_MEMORY_HANDLE_TYPE_OPAQUE_FD_BIT`
- GL 导入：`GL_EXT_memory_object_fd` + `GL_EXT_memory_object`

阶段 0 已验证该组合在 NVIDIA 上像素正确，且 Vulkan/GL 对同一张
OPTIMAL image 的内存布局一致。

DMA-BUF/EGLImage 路径仍可能用于其他驱动（尤其 AMD/Mesa），但本版
不作为主路径；只有在目标平台 OPAQUE_FD 不可用时才评估。

### 3.2 同步与布局（第一版）

不引入外部 semaphore 的原因是 wgpu 公共 API 不暴露 queue submit 的
wait/signal semaphore；第一版采用粗粒度 GPU 同步：

- wgpu 完成输入纹理后：`device.poll(wgpu::Maintain::Wait)`；
- GL 读取前：`glMemoryBarrier(GL_TEXTURE_FETCH_BARRIER_BIT |
  GL_SHADER_IMAGE_ACCESS_BARRIER_BIT)`；
- 插件渲染完成后：`glFinish()`；
- 用 wgpu 的 raw Vulkan queue 提交一个布局/可见性 barrier
  （`oldLayout == newLayout == SHADER_READ_ONLY_OPTIMAL`，
  `srcAccessMask` 覆盖 GL 写，`dstAccessMask` 覆盖 shader read）；
- `vkQueueWaitIdle` 后再允许 wgpu 采样输出。

第一版接受“每帧一次 GPU 全同步”的吞吐代价；正确性优先。
后续阶段再评估 `VK_KHR_external_semaphore_fd` + `GL_EXT_semaphore_fd`。

### 3.3 wgpu 纹理跟踪的一个关键点

`wgpu::Device::create_texture_from_hal` 把外部纹理的初始 tracker 状态设为
`TextureUses::UNINITIALIZED`。第一次被 wgpu 使用时，会从
`VK_IMAGE_LAYOUT_UNDEFINED` 转换，可能丢弃已有内容。

第一版处理：

- **输入纹理**：由 wgpu 自己先渲染/写入，tracker 已有状态，不需要预热；
- **输出纹理**：在 GL 写入前，先用 wgpu 对输出纹理做一次 `clear_texture`
  （一个 render pass，`LoadOp::Clear`），把 tracker 推进到 `COLOR_TARGET`
  且 layout 为 `COLOR_ATTACHMENT_OPTIMAL`；随后 GL 写入时 Vulkan layout
  保持不变，wgpu 后续采样会从 `COLOR_TARGET` 转到 `RESOURCE`，旧内容
  不会被 `UNDEFINED` 丢弃。

这个 clear 不是拷贝，只是初始化新输出纹理；需要测试证明 GL 写入后
wgpu 采样拿到的是 GL 的颜色而不是被清空。

如果预热方案在驱动上仍不可靠，备选：

- 在 `quercus-core` 中给外部纹理增加一个仅本仓库使用的 “initial usage”
  包装层，绕过 `create_texture_from_hal` 的默认 tracker；但这需要改
  wgpu-core 或用 raw HAL 自己维护纹理对象，风险高，不作为第一版。

## 4. 分阶段实施

### 阶段 0：独立可行性探针（先做，不改生产代码）

新增 `crates/quercus-core/examples/ofx_zerocopy_probe.rs`（或
`tooling/ofx-zerocopy-probe`）：

1. 创建 wgpu Vulkan 设备；
2. 用 raw ash 创建 LINEAR、exportable、RGBA32F VkImage；
3. `vkAllocateMemory` + `vkGetMemoryFdKHR`；
4. EGL 初始化、创建桌面 GL 上下文、导入 DMA-BUF、绑定 GL 纹理；
5. GL 清成已知颜色，`glFinish`；
6. 仅探针内用 `glReadPixels` 验证颜色/行序；
7. 打印 fourcc、pitch、offset、EGL/GL 扩展、驱动信息。

验收：探针在目标 Linux/NVIDIA 机器输出正确颜色。
失败则立即停在这个阶段，先确定 B/C 方案，不把半成品接进渲染器。

### 阶段 1：`quercus-core` 可导出纹理

文件：

- `crates/quercus-core/src/backend.rs`
- 新文件 `crates/quercus-core/src/backend/export.rs`（Linux）
- `crates/quercus-core/Cargo.toml`（如需要 `libc`/`ash`，已有 ash）

改动：

1. `GpuTexture` 增加：
   ```rust
   external: Option<Arc<ExternalTexture>>,
   ```
2. 新类型：
   ```rust
   #[cfg(target_os = "linux")]
   pub struct DmaBufExport {
       fd: Arc<OwnedFd>,
       offset: u64,
       pitch: u64,
       width: u32,
       height: u32,
       format: wgpu::TextureFormat,
       keepalive: Arc<wgpu::Texture>,
   }
   ```
   提供 `raw_fd()`、`dup_fd()`、`offset()`、`pitch()` 等只读访问。
3. `GpuContext` 增加能力字段/方法：
   - `ofx_zerocopy_enabled: bool`（默认 Linux/Vulkan 开启，可用
     `QUERCUS_OFX_GPU_ZEROCOPY=0` 关闭）；
   - `supports_dmabuf_export() -> bool`；
   - `create_exportable_texture(width, height, format, usage) -> Result<u64>`；
   - `export_dmabuf(token) -> Result<DmaBufExport>`。
4. `create_texture_format` 在 `zerocopy` 且格式为 `Rgba32Float` 时优先走
   `create_exportable_texture`；任何一步失败回退普通 `create_texture`，
   并记录一次原因（不能因零拷贝失败导致渲染失败）。
5. 外部纹理创建步骤：
   - 创建 dummy `ash::Instance`（仅用于加载 device proc）；
   - `vkCreateImage`：LINEAR、`ExternalMemoryImageCreateInfo`、
     `DMA_BUF_EXT`、usage `SAMPLED|COLOR_ATTACHMENT|TRANSFER_SRC|TRANSFER_DST`；
   - 选 memory type（遍历 `memory_type_bits` 尝试，
     `MemoryDedicatedAllocateInfo` + `ExportMemoryAllocateInfo`）；
   - `vkGetMemoryFdKHR`；
   - `vkGetImageSubresourceLayout` 取 offset/pitch；
   - `hal::vulkan::Device::texture_from_raw(..., TextureMemory::External)`；
   - `wgpu::Device::create_texture_from_hal` 注册；
   - DropCallback 负责 `vkDestroyImage` + `vkFreeMemory`；
     `ExternalTexture` 负责关闭导出的 fd。
6. `DmaBufExport` 必须持有 `Arc<wgpu::Texture>`，防止插件使用期间纹理被回收。

测试（`QUERCUS_GPU_TESTS=1` 或 `QUERCUS_REQUIRE_GPU=1`）：

- 创建 exportable Rgba32Float 纹理，导出 fd 成功；
- fd 有效（`fcntl(F_GETFD)`）、pitch >= width*16；
- 普通 `create_texture` 在关闭开关后仍是普通纹理；
- 不支持时 `supports_dmabuf_export() == false` 且回退不 panic。

### 阶段 2：Linux EGL / GL 桥

文件：

- `crates/quercus-ofx-plugin/src/gl_bridge.rs`
- `crates/quercus-ofx-plugin/Cargo.toml`（Linux 加 `libloading`、`libc`）

改动：

1. 把 `#[cfg(not(target_os = "macos"))] mod imp` 拆成
   `#[cfg(target_os = "linux")] mod imp` + Windows stub。
2. Linux EGL 上下文：
   - 动态加载 `libEGL.so.1`、`libGL.so.1`（`libloading`）；
   - `eglGetDisplay(EGL_DEFAULT_DISPLAY)` 初始化；
   - `EGL_OPENGL_BIT` + pbuffer surface + desktop GL context；
   - 全局 `Mutex` 串行化 + 线程 current，沿用 macOS 的 `GlGuard` 语义；
   - 失败时 `gl_available() == false`，插件路径回退。
3. 新增 API：
   ```rust
   pub fn import_dmabuf_texture(
       export: &DmaBufExport,
       width: i32,
       height: i32,
       params: &VideoParams,
   ) -> Result<GlTexture>;
   pub struct GlTexture { name: u32, /* EGLImage, display */ }
   ```
   内部：
   - `dup(export.raw_fd())`；
   - `eglCreateImageKHR`，attr：`EGL_WIDTH/HEIGHT`、
     `EGL_LINUX_DRM_FOURCC_EXT = DRM_FORMAT_ABGR32323232F`、
     `EGL_DMA_BUF_PLANE0_FD_EXT/OFFSET/PITCH`；
   - `glEGLImageTargetTexture2DOES(GL_TEXTURE_2D, image)`；
   - 设置 `GL_TEXTURE_MIN/MAG_FILTER`、`CLAMP_TO_EDGE`；
   - 返回 GL 名 + EGLImage（析构时 `glDeleteTextures` +
     `eglDestroyImageKHR`）。
4. `create_output_texture` / `create_input_texture` 保留给 CPU 路径；
   零拷贝路径使用 `import_dmabuf_texture`。
5. 增加 `gl_finish()`、`gl_memory_barrier()`、`gl_has_extension()`。

测试：

- Linux 上创建 EGL 上下文，`GL_VERSION` 非空；
- 导入一张探针产生的 DMA-BUF，GL 纹理名非 0；
- FBO 完整性通过；
- 清理后无 GL error。

### 阶段 3：渲染驱动的零拷贝分支

文件：

- `crates/quercus-ofx-plugin/src/render_driver.rs`
- `crates/quercus-ofx-plugin/src/clip.rs`
- `crates/quercus-ofx-plugin/src/suites/gl_render.rs`

改动：

1. `ClipInstance` 增加：
   ```rust
   input_gl_texture: Mutex<Option<ZeroCopyGlTexture>>,
   pub fn set_input_gl_texture(&self, tex: Option<ZeroCopyGlTexture>);
   ```
2. `clip_load_texture` 输入分支：
   - 若 `input_gl_texture` 存在：跳过 `fetch_image`，直接
     用真实 GL 名构造 `OpenGLTextureIndex`/尺寸/分量属性；
   - 否则维持现有 CPU 上传路径。
3. `LIVE_TEXTURES` 条目增加 `gl_owned: bool`：
   - CPU 上传的 GL 纹理：`gl_owned = true`，`clip_free_texture` 删除；
   - 零拷贝借用的 GL 纹理：`gl_owned = false`，由渲染驱动统一清理。
4. `render_driver` 增加：
   ```rust
   fn try_render_gl_zerocopy(...) -> Result<bool>;
   ```
   步骤：
   - `job.dst` 必须是 `Texture::Gpu`，且 `ctx.export_dmabuf(token)` 成功；
   - 输出：`import_dmabuf_texture(dst_export)` → FBO；
   - 输入：对每个 clip 的 `Texture::Gpu` 导出并导入；CPU 输入保留上传路径；
   - 在 GL 写入输出前，对输出纹理执行 wgpu 1×1 预热采样（见 3.3）；
   - `inst.render_gl(..., Some(output_gl_name))`；
   - `glFinish` + cross-API barrier；
   - 不做 `read_pixels_to_image`，不调用 `write_output_frame`；
   - 清理输入/输出 GL 纹理、FBO、EGLImage；
   - 返回 `true` 表示输出已是 `job.dst` 本身。
5. `render_frame` 的 GL 分支：
   - 先尝试 `try_render_gl_zerocopy`；
   - 成功：直接返回 `dst`；
   - 失败：记录原因，回退现有 `render_gl_frame`（回读）或 CPU 路径。
6. 删除/隔离零拷贝路径里的 `read_pixels_to_image` 调用；保留函数给探针/
   回退路径和测试使用。

测试：

- 用测试 OFX 插件（GL 渲染纯色）验证输出颜色；
- `gpu_transfer_counters()` 在零拷贝渲染前后不变（`downloads == 0`，
  且 GPU 输入不产生 `uploads`）；
- 对比 CPU 路径和零拷贝路径的像素误差；
- 连续渲染 100 帧无 GL error、无 Vulkan validation error；
- 关闭零拷贝开关后仍走旧 GL 回读路径并通过。

### 阶段 4：插件输出纹理改为 GPU

文件：

- `crates/quercus-ofx-plugin/src/node_factory.rs`

改动：

`execute_plugin_job` 不再无条件 `generate_frame`：

```rust
let dst = match &req.src {
    Texture::Gpu { ctx, width, height, .. } if zero_copy_available(ctx) => {
        let token = ctx.create_texture(*width, *height)?;
        Texture::gpu(ctx.clone(), token, *width, *height, PixelFormat::F32)
    }
    _ => Texture::wrap_frame(generate_frame(...)),
};
```

要求：

- 只有 `req.src` 是 GPU 且 `ctx` 可导出时才创建 GPU 输出；
- 创建失败回退 CPU 帧，不阻断插件；
- 输出纹理必须由同一个 `GpuContext` 创建，保证后续节点可直接采样；
- 不能复用输入纹理作为输出（插件可能原地写）。

测试：

- GPU 输入 → 输出 `Texture::Gpu`；
- CPU 输入 → 输出仍为 CPU（旧路径）；
- 输出 token 与输入 token 不同；
- 插件失败时仍返回紫色 CPU 帧。

### 阶段 5：可观测性与验收测试

新增/修改：

- `crates/quercus-core/src/backend.rs`：暴露 `gpu_transfer_counters` 已有；
  增加 `gpu_zerocopy_frames` 计数（成功零拷贝帧数）和
  `gpu_zerocopy_fallbacks`（失败原因分类）。
- `crates/quercus-ofx-plugin/tests/gl_render_test.rs`：新增 Linux 零拷贝
  集成测试（`QUERCUS_GPU_TESTS=1` 才跑真实 GPU）。
- `crates/quercus-render/tests/`：graph 级测试，OFX 输出接内置效果。

验收命令：

```bash
FFMPEG_DIR=/home/sunyu/Projects/oak-editor/.cache/ffmpeg \
OCIO_SOURCE_DIR=/home/sunyu/Projects/quercus-engine/target/ocio-probe/src03 \
cargo test -p quercus-core -p quercus-ofx-plugin -p quercus-render
```

真实 GPU：

```bash
QUERCUS_GPU_TESTS=1 QUERCUS_REQUIRE_GPU=1 \
FFMPEG_DIR=... OCIO_SOURCE_DIR=... \
cargo test -p quercus-ofx-plugin --test gl_render_test -- --nocapture
```

硬性断言：

- 零拷贝路径 `glReadPixels` 调用次数为 0；
- `GpuContext::download` 调用次数为 0；
- 输出与期望像素误差 < 1e-5（F32）；
- 帧循环 100 次无 validation error；
- 无 EGL/GL 上下文泄漏（重复 acquire/release 不增长）。

### 阶段 6：Oak 验证

文件（Oak 仓库）：

- `crates/oak-app/src/panels/ofx.rs`（路径以 Oak 当前树为准）
- `crates/oak-app/src/oakui/renderops.rs`
- GPUI 外部纹理合成路径

改动：

- 确认 Oak 的 GPUI 设备与 Quercus `GpuContext` 共用同一个 wgpu device
  （`GpuContext::adopt` 或 `install_shared`）；
- OFX 输出纹理通过 `texture_handle` 直接交给 GPUI，不经过 CPU；
- 如果 Oak 仍使用独立设备，先只验证引擎侧零拷贝，Oak 的跨设备合成
  另开一项，不混入本次验收。

Oak 验收：

- 时间线放一个 GL OFX 插件，播放/暂停/拖动；
- 用 RenderDoc/NSight 或计数器确认无 `glReadPixels`、无 frame download；
- 预览颜色与 CPU 路径一致；
- 关闭零拷贝开关后行为不变。

### 阶段 7：macOS / Windows（后续）

macOS：

- Metal 纹理用 IOSurface 后备（`newTextureWithDescriptor:iosurface:`）；
- CGL 用 `CGLTexImageIOSurface2D` 绑定同一 IOSurface；
- 复用阶段 1 的“可导出纹理”抽象，增加 `IOSurfaceExport`。

Windows：

- wgpu Vulkan 的 `VK_KHR_external_memory_win32` 已由 wgpu-hal 启用；
- 导出 D3D11/D3D12 shared handle，GL 侧用 `WGL_EXT_memory_object_win32`
  或 EGL/ANGLE 导入；
- 不保证第一阶段覆盖。

## 4.1 实现记录（Linux）

已实现并通过本机验收：

- `quercus-core`：
  - `GpuContext::create_exportable_texture` 创建 `OPAQUE_FD` exportable
    `Rgba32Float` VkImage；
  - `GpuContext::export_gl_memory` 导出 fd；
  - `GpuContext::wait_idle` 会先 flush `Queue::write_texture` 的 pending
    staging copy（实测这是“wgpu 上传 → GL 读不到”的根因）。
- `quercus-ofx-plugin`：
  - Linux EGL 上下文（NVIDIA device platform 可用）；
  - `GL_EXT_memory_object_fd` 导入 + `glTexStorageMem2DEXT`；
  - `render_driver::try_render_gl_zerocopy`：输出先 `clear_texture`
    推进 tracker，插件直接写输出 GL 纹理，输入 clip 直接绑定导入
    GL 纹理；无 `glReadPixels`、无 `download/upload`；
  - `node_factory::execute_plugin_job` 在 GPU 输入时创建同 context
    的 GPU 输出纹理。
- 验收：`gl_plugin_renders_zero_copy_on_linux` 连续 25 帧：
  - `glReadPixels` 计数不变；
  - `gpu_transfer_counters` 的 upload/download 均不变；
  - 输出 token 与目标 token 相同；
  - 插件输出颜色正确；
  - 输出再经 wgpu `blit` 采样后颜色仍正确。
- 旧路径：`gl_plugin_renders_through_real_gl_path` 在 Linux 也通过
  （GL 回读 fallback 保留）。

已知限制：

- 仅 Linux/Vulkan/NVIDIA 实测；AMD/Mesa 的 EGL/内存对象路径未验证；
- 第一版用 `glFinish + queue wait idle`，尚未接 external semaphore；
- 多 GPU 时 EGL device 选择目前优先非 Mesa 设备，未做 Vulkan UUID
  精确匹配；
- macOS/Windows 未接；
- Oak 已更新 submodule 并通过 `cargo check`，引擎侧 OFX 边界零拷贝；
  Oak 预览器目前仍把最终显示样本上传为 RGBA16F（`oakui/gpu.rs`），
  还没有把引擎纹理直接注册给 GPUI——这是下一步独立的 present 路径
  工作，不影响 OFX 插件输入/输出零拷贝的结论。

## 5. 代码改动清单

| 文件 | 改动 |
|---|---|
| `crates/quercus-core/src/backend.rs` | 外部纹理字段、能力开关、`export_dmabuf`、`create_exportable_texture` |
| `crates/quercus-core/src/backend/export.rs`（新） | Linux Vulkan DMA-BUF 导出 |
| `crates/quercus-core/src/backend/external.rs` | 复用导入/注册工具；补测试 |
| `crates/quercus-core/Cargo.toml` | 必要时补 Linux 依赖 |
| `crates/quercus-ofx-plugin/src/gl_bridge.rs` | Linux EGL 实现、DMA-BUF 导入、GL 清理 |
| `crates/quercus-ofx-plugin/src/clip.rs` | 输入 GL 纹理槽、零拷贝 `clipLoadTexture` |
| `crates/quercus-ofx-plugin/src/suites/gl_render.rs` | 真实 GL 名/所有权标记 |
| `crates/quercus-ofx-plugin/src/render_driver.rs` | 零拷贝渲染分支、回退 |
| `crates/quercus-ofx-plugin/src/node_factory.rs` | GPU 输出纹理 |
| `crates/quercus-ofx-plugin/Cargo.toml` | Linux `libloading`/`libc` |
| `docs/engine-capabilities.md` | 零拷贝能力/平台矩阵更新 |
| `docs/quercus-capabilities.md` | API/能力更新 |

## 6. 风险与回退

| 风险 | 处理 |
|---|---|
| EGL 无法导入 RGBA32F DMA-BUF | 阶段 0 探针先验证；失败转 B/C 方案或回退 |
| 线性纹理性能差 | 第一版接受；后续做 DRM modifier 协商 |
| wgpu tracker 初始 `UNINITIALIZED` 丢内容 | 输出纹理预热采样；测试覆盖 |
| 跨 API 同步不完整 | 第一版 `queue wait idle + glFinish + barrier`；后续 semaphore |
| 外部内存分配失败 | 逐纹理回退普通 wgpu 纹理，插件路径回退旧回读 |
| EGL 上下文与 wgpu 设备不是同一 GPU | 启动时比对 device UUID / EGL device；不同则禁用零拷贝 |
| 驱动崩溃/黑屏 | 开关 `QUERCUS_OFX_GPU_ZEROCOPY=0` 一键回退；默认在验证通过前保持关闭 |

默认策略：

- 实现期间默认关闭，只有显式 `QUERCUS_OFX_GPU_ZEROCOPY=1` 才启用；
- 阶段 5 验收通过后，再改为 Linux/Vulkan 默认开启、失败自动回退；
- 不在验收前发布。

## 7. 完成标准

必须同时满足：

1. Linux/Vulkan + EGL 的真实 OFX GL 插件渲染成功；
2. 输入/输出均为外部 GPU 纹理，`glReadPixels` 和 `download/upload`
   计数为 0；
3. 输出可被后续 wgpu 节点直接采样，颜色正确；
4. 关闭开关后旧路径全部测试通过；
5. 全工作区 `cargo test` / `cargo check` 通过；
6. Oak 预览路径验证无 CPU 回读；
7. 文档记录平台矩阵与已知限制。

以上未全部满足前，不把该能力标记为“完成”，也不发布。
