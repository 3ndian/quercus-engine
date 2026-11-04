# OpenFX 真正 GPU 零拷贝：Windows 接入计划

状态：计划稿（未实施）
前置：Linux/Vulkan + EGL 第一版已落地（`22b2d3d`）
目标：Windows 上 OFX GL 插件输入/输出同一块 GPU 显存，无
`glReadPixels`、无 CPU `Frame`、无 upload/download。
不包含：Windows 上 Oak 预览器的最终 present 零拷贝（另列）。
明确不做：DX12/D3D12 纹理导出、D3D11↔GL interop（`WGL_NV_DX_interop2`）。
Windows 只接 Vulkan + WGL 这一条路；不可用就回退现有路径。

## 1. Linux 第一版的可复用结论

Linux 实测已经证明：

- 不能依赖 wgpu 普通 `create_texture` 导出：必须自己创建
  external-memory VkImage，再用 `texture_from_raw` 包回 wgpu；
- 输入方向必须 flush `Queue::write_texture` 的 pending staging copy，
  否则 GL 读到全零；
- 输出纹理必须先被 wgpu 触碰一次（`clear_texture`），把 tracker 从
  `UNINITIALIZED` 推到 `COLOR_TARGET`，否则 GL 写入会被
  `UNDEFINED` 转换丢弃；
- 用计数器证明零拷贝：`glReadPixels`、`gpu_transfer_counters` 均不变；
- 同一套 `GlMemoryTexture` / `try_render_gl_zerocopy` 结构可以复用，
  Windows 只需替换“句柄导出”和“GL 上下文/内存对象导入”两层。

## 2. 推荐方案：Vulkan OPAQUE_WIN32 + GL_EXT_memory_object_win32

Windows 第一版采用与 Linux 同构的路径：

```text
wgpu/Vulkan 设备
  └─ Rgba32Float VkImage
       ├─ wgpu texture_from_raw
       └─ VK_EXTERNAL_MEMORY_HANDLE_TYPE_OPAQUE_WIN32_BIT
             └─ vkGetMemoryWin32HandleKHR
                   └─ WGL 隐藏窗口 + OpenGL 3.2 core
                         └─ glImportMemoryWin32HandleEXT
                               + glTexStorageMem2DEXT(GL_RGBA32F)
                                 └─ OFX clipLoadTexture 返回真实 GL 名
```

### 2.1 选型理由

- 与 Linux 的 `OPAQUE_FD` + `GL_EXT_memory_object_fd` 一一对应；
- 不引入 DX12/D3D12 纹理，也不引入 `WGL_NV_DX_interop2` 的
  lock/unlock 生命周期；
- 不经过 DMA-BUF/EGL，Windows 原生路径更短；
- NVIDIA/AMD 的 Windows 驱动通常支持
  `GL_EXT_memory_object_win32`；Intel 需实测。

### 2.2 前置条件

- 引擎在 Windows 必须走 **Vulkan** 后端（`BackendKind::Vulkan` 或
  `Auto` 的 Vulkan fallback），不能走 GL/CPU。
- `wgpu` 设备创建时必须请求
  `wgpu::Features::VULKAN_EXTERNAL_MEMORY_WIN32`；适配器不支持时
  零拷贝关闭，回退现有 GL 回读/CPU。
- OFX 插件必须声明 GL 渲染支持（现有 `use_opengl` 决策不变）。
- GL 上下文必须与 Vulkan 物理设备是同一块 GPU。第一版用
  `GL_RENDERER` 与 `GpuContext::adapter_name()` 做名称比对；不匹配
  就禁用零拷贝。多 GPU 精确匹配（LUID/PCI）放第二阶段。

### 2.3 格式与布局

第一版固定：

- `wgpu::TextureFormat::Rgba32Float`
- Vulkan `VK_FORMAT_R32G32B32A32_SFLOAT`
- GL `GL_RGBA32F`
- `VK_IMAGE_TILING_OPTIMAL`
- `VK_EXTERNAL_MEMORY_HANDLE_TYPE_OPAQUE_WIN32_BIT`
- `GL_HANDLE_TYPE_OPAQUE_WIN32_EXT`（`0x9587`）

如果目标驱动不支持这个组合，直接回退现有 GL 回读/CPU 路径；
**不实现 D3D11/D3D12 interop 作为替代方案**。

## 3. 分阶段实施

### 阶段 0：Windows 独立探针（先做）

目标：在真实 Windows GPU 上证明 Vulkan OPAQUE_WIN32 ↔ GL 内存对象
可用，再改生产代码。

新增 `tooling/ofx-zerocopy-probe-win32`（或 `quercus-core` 的 example）：

1. 创建 Vulkan 设备，启用 `VK_KHR_external_memory_win32`；
2. 创建 `VK_FORMAT_R32G32B32A32_SFLOAT`、OPAQUE_WIN32 exportable image；
3. `vkGetMemoryWin32HandleKHR` 取 NT handle；
4. 创建隐藏 Win32 窗口 + WGL 3.2 core 上下文；
5. `glImportMemoryWin32HandleEXT` + `glTexStorageMem2DEXT`；
6. GL 清成已知颜色，`glFinish`；
7. 探针内用 `glReadPixels` 验证颜色；
8. Vulkan `vkCmdCopyImageToBuffer` 验证反向可见性；
9. 打印 GL_RENDERER、Vulkan 设备名、扩展、handle 类型。

验收：NVIDIA 与 AMD 至少各一台真实 Windows 机器颜色正确。
失败则停止，记录该驱动不支持；不接生产渲染器，也不转 D3D interop。

### 阶段 1：`quercus-core` Windows 可导出纹理

文件：

- `crates/quercus-core/src/backend.rs`
- `crates/quercus-core/src/backend/external.rs`
- `crates/quercus-core/Cargo.toml`

改动：

1. `GpuContext` 在 Windows 请求
   `VULKAN_EXTERNAL_MEMORY_WIN32`（适配器支持时）；
2. 新增 Windows `ExternalTexture`：
   - 持有 `HANDLE`（`isize`/`windows-sys::HANDLE`）；
   - `size`、宽高、格式；
   - Drop 时 `CloseHandle`；
3. 新增 `GlMemoryExport`（Windows）：
   - `try_clone_handle() -> HANDLE`（`DuplicateHandle`，
     `DUPLICATE_SAME_ACCESS`）；
   - 保留 `Arc<wgpu::Texture>` 防止 GL 使用期间被回收；
4. `create_exportable_texture` Windows 分支：
   - `VkExternalMemoryImageCreateInfo.handleTypes =
     OPAQUE_WIN32`；
   - `VkExportMemoryAllocateInfo` + dedicated allocation；
   - `vkGetMemoryWin32HandleKHR`；
   - `hal::vulkan::Device::texture_from_raw`；
   - `wgpu::Device::create_texture_from_hal`；
5. `GpuContext::supports_gl_zerocopy` Windows：
   - `ofx_zerocopy && kind == Vulkan &&
     device.features().contains(VULKAN_EXTERNAL_MEMORY_WIN32)`；
6. `create_texture_format` 的外部分配分支扩展到 Windows；
7. 导出失败逐纹理回退普通 wgpu 纹理，不阻断渲染。

Windows 依赖：

- `quercus-core`：`windows-sys`（`Win32_Foundation`）或最小
  `CloseHandle`/`DuplicateHandle` extern；
- 继续复用 `ash` 的 `khr::external_memory_win32`。

测试：

- `create_exportable_texture` 返回有效 NT handle；
- 关闭开关后普通纹理路径不变；
- 无 `VULKAN_EXTERNAL_MEMORY_WIN32` 时 `supports_gl_zerocopy()==false`。

### 阶段 2：WGL / GL 桥

文件：

- `crates/quercus-ofx-plugin/src/gl_bridge.rs`
- `crates/quercus-ofx-plugin/Cargo.toml`

改动：

1. 把 Windows stub 换成真实 `imp`：
   - 隐藏窗口：`RegisterClassW` + `CreateWindowExW`（不显示）；
   - `GetDC` + `ChoosePixelFormat`/`SetPixelFormat`；
   - 旧 `wglCreateContext` 取入口，再
     `wglCreateContextAttribsARB` 建 core（优先 4.5，失败回退 3.2）；
   - `wglMakeCurrent`；全局 Mutex 串行化；
   - Drop：`wglMakeCurrent(NULL,NULL)`、`wglDeleteContext`、
     `ReleaseDC`、`DestroyWindow`。
2. GL 函数加载：
   - `opengl32.dll` 取 core 1.1；
   - `wglGetProcAddress` 取 `glCreateMemoryObjectsEXT`、
     `glDeleteMemoryObjectsEXT`、`glImportMemoryWin32HandleEXT`、
     `glTexStorageMem2DEXT`、`glMemoryBarrier` 等；
   - 检查 `GL_EXT_memory_object_win32` 扩展字符串。
3. `import_memory_texture` Windows：
   - `glCreateMemoryObjectsEXT`；
   - `export.try_clone_handle()` 后
     `glImportMemoryWin32HandleEXT(mem, size,
     GL_HANDLE_TYPE_OPAQUE_WIN32_EXT, handle)`；
   - `glTexStorageMem2DEXT(GL_TEXTURE_2D, 1, GL_RGBA32F, w, h, mem, 0)`；
   - 失败时 `CloseHandle` + `glDeleteMemoryObjectsEXT`；
   - 成功时句柄所有权交给 GL 内存对象（按扩展规范；探针验证）。
4. CPU fallback 的 `create_output_texture` / `create_input_texture` /
   FBO / `read_pixels_to_image` 复用 Linux 逻辑。
5. 测试插件构建：
   - `build.rs` 的 Windows 分支不再直接返回；
   - 用 `cl`/`cc` 编 `quercus_test_plugin.dll`，链接 `opengl32.lib`；
   - 或支持 `QUERCUS_TEST_PLUGIN_DIR` 预构建 bundle，CI 用后者。

Windows 依赖：

- `libloading`（加载 opengl32）；
- `windows-sys`（窗口/GDI/Foundation，`Win32_Graphics_Gdi`）。
- 不引入 `winit`/`glutin`；隐藏窗口足够。

测试：

- WGL 上下文 `GL_VERSION` 非空；
- `GL_EXT_memory_object_win32` 存在；
- 导入 NT handle 后 GL 纹理名非 0、FBO 完整；
- 清理后无 GL error、无 handle 泄漏。

### 阶段 3：渲染驱动 / clip / node 接线

文件：

- `crates/quercus-ofx-plugin/src/render_driver.rs`
- `crates/quercus-ofx-plugin/src/clip.rs`
- `crates/quercus-ofx-plugin/src/suites/gl_render.rs`
- `crates/quercus-ofx-plugin/src/node_factory.rs`
- `crates/quercus-core/src/backend.rs`（cfg 扩展）

改动：

1. `#[cfg(target_os = "linux")]` 的零拷贝分支扩展到
   `any(target_os = "linux", target_os = "windows")`；
2. `clip.rs` 的 `input_gl_texture` 字段/方法同样扩展；
3. `try_render_gl_zerocopy` 里 `export_for`、`import_memory_texture`
   保持平台无关签名，Windows 用 handle 版本；
4. `node_factory::execute_plugin_job` 的 GPU 输出逻辑不变
   （`supports_gl_zerocopy` 在 Windows 分支返回 true 时才创建）；
5. `GpuContext::create_texture_format` 的外部纹理分支扩展到 Windows；
6. `use_opengl` 决策不变：WGL 可用 + 插件声明 GL + F32 协商。

测试：

- `gl_plugin_renders_zero_copy_on_windows`：25 帧循环；
  - `glReadPixels` 计数不变；
  - `gpu_transfer_counters` upload/download 不变；
  - 输出 token 不变；
  - 插件输出颜色正确；
  - 下游 wgpu `blit` 采样颜色正确；
- 旧 GL 回读路径在 Windows 也通过；
- `QUERCUS_OFX_GPU_ZEROCOPY=0` 时回退。

### 阶段 4：Oak Windows 验证

Oak 当前在 Windows 不注册 GPUI GPU context（`app.rs` 只在
Linux/FreeBSD 注册），因此引擎会自己创建 Vulkan 设备，正好符合第一版
前置条件。需要验证：

- `cargo check -p oak-app` on Windows；
- OFX 插件预览/导出路径在 Windows 真实 GPU 上无 `glReadPixels`；
- 如果以后 Oak 在 Windows 注册 DX12 context，`supports_gl_zerocopy`
  必须为 false，并自动回退；不实现 DX12 导出。
- Oak 预览器目前仍是 RGBA16F 上传；Windows OFX 边界零拷贝不依赖它。

## 4. 明确不做的范围

- 不做 DX12 / D3D12 纹理导出；
- 不做 `WGL_NV_DX_interop2` / D3D11 `OpenSharedResource1`；
- 不做 D3D11VA 解码纹理到 GL 的直接零拷贝；这类输入回退现有
  GL 回读/CPU 路径；
- 不为了 Windows 改变 Linux 的默认渲染路径或 Vulkan 纹理分配策略。

## 5. 代码改动清单

| 文件 | 改动 |
|---|---|
| `crates/quercus-core/src/backend.rs` | Windows 请求 Win32 external memory feature；`wait_idle` 已有 |
| `crates/quercus-core/src/backend/external.rs` | Windows `ExternalTexture`/`GlMemoryExport`/`create_exportable_texture` |
| `crates/quercus-core/Cargo.toml` | Windows `windows-sys` 最小依赖 |
| `crates/quercus-ofx-plugin/src/gl_bridge.rs` | WGL 上下文 + `GL_EXT_memory_object_win32` |
| `crates/quercus-ofx-plugin/src/clip.rs` | `input_gl_texture` cfg 扩展 |
| `crates/quercus-ofx-plugin/src/render_driver.rs` | 零拷贝分支 cfg 扩展 |
| `crates/quercus-ofx-plugin/src/suites/gl_render.rs` | 零拷贝输入 props（已有） |
| `crates/quercus-ofx-plugin/src/node_factory.rs` | `supports_gl_zerocopy` Windows |
| `crates/quercus-ofx-plugin/build.rs` | Windows 测试插件 DLL/链接 opengl32 |
| `crates/quercus-ofx-plugin/cbits/quercus_test_plugin.c` | Windows GL include/清理 |
| `crates/quercus-ofx-plugin/tests/gl_render_test.rs` | Windows 端到端零拷贝测试 |
| `docs/engine-capabilities.md` | 平台矩阵更新 |

## 6. 验收标准

必须同时满足：

1. Windows 真实 NVIDIA/AMD GPU，Vulkan 后端；
2. 探针证明 Vulkan OPAQUE_WIN32 → WGL → Vulkan 双向可见；
3. OFX 插件输入/输出同一显存；
4. 零拷贝路径 `glReadPixels == 0`、`upload == 0`、`download == 0`；
5. 输出 token 与目标 token 相同，下游 wgpu 采样正确；
6. 25 帧循环无 handle/GL 对象泄漏、无 validation error；
7. `QUERCUS_OFX_GPU_ZEROCOPY=0` 回退旧路径通过；
8. Oak Windows `cargo check` 通过，OFX 预览无回读；
9. 文档写明 Vulkan-only 与多 GPU 限制。

## 7. 风险与回退

| 风险 | 处理 |
|---|---|
| 驱动不支持 `GL_EXT_memory_object_win32` | 探测后禁用；回退现有路径，不做 D3D interop |
| GL 与 Vulkan 不是同一 GPU | `GL_RENDERER` 与 adapter name 比对，不匹配禁用 |
| 多 GPU 笔记本默认 GL 走 iGPU | 第二阶段用 LUID/PCI 精确选择；第一版直接回退 |
| 句柄所有权/泄漏 | 探针验证；`DuplicateHandle` + Drop `CloseHandle`，测试计数 |
| Vulkan 设备未启用 Win32 扩展 | 创建时请求 feature；不支持则 `supports_gl_zerocopy=false` |
| Oak 将来用 DX12 context | 严格 Vulkan 检查，DX12 自动回退；不实现 DX12 零拷贝 |
| D3D11VA 解码纹理不可导出 | 该输入回退旧 GL 回读；不做 DX interop |
| 无 GPU 的 Windows CI | 只编译/单测；真实 GPU 测试用自托管 runner |

## 8. 建议实施顺序

1. Windows 探针（真实 GPU）；
2. `quercus-core` Windows 导出；
3. WGL 桥 + 探针复用；
4. render/clip/node 接线；
5. 测试插件 Windows 构建 + 端到端测试；
6. Oak Windows 验证。

未完成上述 1–6 前，不把 Windows 能力标记为“完成”，不发布。
