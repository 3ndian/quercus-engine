# P1 扩展点：日志 / 字体 / 磁盘缓存 / 媒体 I/O

> 承接 [`sdk-extensibility-review.md`](sdk-extensibility-review.md) 的 P1 清单。
> 本批交付四个调用方可注入的组件；GPU 设备注入与配置持久化仍未开始（见文末）。
> 所有路径相对仓库根。

---

## 0. `unsafe` 边界（P1 收尾）

P0/P1 新增的“调用方可替换组件”统一按两层收拢：

- **业务 crate 的扩展点只暴露安全 Rust trait**（crate 内既有的 handle/FFmpeg/wgpu 内部实现不在此列）：
  `quercus-storage`（`StorageBackend`）、`quercus-core`（`LogSink`/`ConfigSource`）、
  `quercus-codec`（`MediaIoProvider`/`MediaStream`）、`quercus-render`（字体/缓存 API）、
  `quercus-core::backend`（GPU adopt 安全 API）。
- **C vtable / `userdata` / 回调生命周期全部集中在
  `crates/quercus-engine/src/ext/`**：
  `storage_transport.rs`、`config_source.rs`、`log_sink.rs`、`media_io.rs`、`gpu.rs`。
  这里也是对应的 `quercus_*` C 导出所在层；业务 crate 不再持有 C 回调类型或裸指针槽。
- `quercus-engine-rs` 是**调用方侧**的镜像边界（extern 声明 + 安全闭包 trampoline），仍然需要 `unsafe`，这是 SDK 的职责。

不在此策略内的是既有“第三方/平台岛”：FFmpeg（`quercus-codec`）、wgpu（`quercus-core::backend`）、
OFX C API（`quercus-ofx-plugin`）以及 node 的 handle 内部实现——它们本来就需要 `unsafe`，
不属于扩展点适配层。

---

## 1. 日志/诊断 sink

**C ABI**（`quercus_engine.h`，prefix `quercus_core_log_`）：

```c
typedef void (*LogFn)(int32_t level, const uint8_t* message, size_t len, void* userdata);
typedef void (*LogDropFn)(void* userdata);

bool quercus_core_log_set_level(int32_t level);   // 0=DEBUG .. 4=FATAL
int32_t quercus_core_log_get_level(void);
void quercus_core_log_set_sink(LogFn sink, void* userdata, LogDropFn drop_cb);
void quercus_core_log_write(int32_t level, const uint8_t* msg, size_t len);
```

- sink 进程级；替换/清除时旧 `drop_cb` 被调用一次。无 sink 时仍写 stderr（旧行为）。
- 消息到达 sink 前已按当前 level 过滤并格式化为 `"[LEVEL] message"`；FATAL 以前被 `log` facade 过滤的旧问题一并修复。
- 回调在锁外调用，可递归日志；`userdata` 在回调期间由引擎的 `Arc` 快照保活。

**SDK**：`state::log_set_level`、`log_get_level`、`log_write`、`set_log_sink(Option<F>)`（闭包 → C 函数指针，panic 被捕获，替换/清除自动回收 Box）。

**已接入的引擎日志**：worker 的 `log_error`、procpool 的非测试诊断已改走 `debug::log`；`QUERCUS_DEBUG_*` 显式调试开关仍直接写 stderr。

---

## 2. 字体 provider

**C ABI**（prefix `quercus_render_textengine_`）：

```c
bool quercus_render_textengine_add_font_data(const uint8_t* bytes, size_t len);
bool quercus_render_textengine_add_font_dir(const uint8_t* path, size_t path_len);
```

- 可在 `textengine_install` 之前或之后调用；下一次 `font_family_count`/`font_family_at` 可见新字体。
- 空数据 / 非 UTF-8 / 不存在或非目录路径返回 false。
- Rust 侧来源：`quercus_render::textengine::{add_font_data, add_font_dir, clear_font_families_cache}`。

**SDK**：`render::textengine_add_font_data(&[u8])`、`textengine_add_font_dir(&Path)`。

**限制**：
- 非空但畸形的字体数据无法检测（fontdb 静默忽略，仍返回 true）。
- 新增字体只对**显式 family 选择**生效，不进入 `FontSystem` 构造时固化的默认 fallback 链。

---

## 3. 磁盘缓存配额/路径

**C ABI**（prefix `quercus_render_disk_cache_`）：

```c
int64_t quercus_render_disk_cache_max_bytes(void);
bool    quercus_render_disk_cache_set_max_bytes(int64_t max_bytes);
bool    quercus_render_disk_cache_set_path(const uint8_t* path, size_t len);
int64_t quercus_render_disk_cache_prune(void);   // 返回释放字节，-1 失败
```

- 配置键 `DiskCacheMaxBytes`（0/负数 = 不限）与 `DiskCachePath`（必须是绝对路径，缺失时创建）。
- 淘汰：递归统计默认缓存目录，按 mtime 从旧到新删，直到 ≤ 预算；不删目录；最新文件即使单个超预算也保留。
- 写入后触发、带防抖：每 2 秒最多一次，或每 64 次缓存写入强制一次；`set_max_bytes` 立即 prune。
- 现有 `PlaybackCache` 在构造时拷贝 `disk_dir`，所以改路径只影响之后创建的缓存。

**SDK**：`render::disk_cache_max_bytes`、`disk_cache_set_max_bytes`、`disk_cache_set_path`、`disk_cache_prune`。

**限制**：mtime 粒度粗时同时间戳文件可能误删较新的帧；活动播放的缓存被淘汰后会重新渲染；配额只写入内存 ConfigStore，持久化仍走既有 config save。

---

## 4. 媒体 I/O provider（VFS）

**C ABI**（prefix `quercus_codec_mediaio_`）：

```c
typedef struct QuercusMediaIoError { int32_t code; uint8_t message[512]; } QuercusMediaIoError;
typedef struct QuercusMediaIoVTable {
  uint32_t struct_size;   // 完整 v1
  uint32_t abi_version;   // 1
  const char* name;
  int32_t (*can_open)(void* ud, const uint8_t* path, size_t len, QuercusMediaIoError* err);
  int32_t (*open)(void* ud, const uint8_t* path, size_t len, void** out_stream, QuercusMediaIoError* err);
  int64_t (*read)(void* ud, void* stream, uint8_t* buf, size_t cap, QuercusMediaIoError* err);
  int64_t (*seek)(void* ud, void* stream, int64_t offset, int32_t whence, QuercusMediaIoError* err);
  void    (*close)(void* ud, void* stream);
} QuercusMediaIoVTable;

int32_t quercus_codec_mediaio_register(const QuercusMediaIoVTable* vt, void* userdata); // 0/-1/-2
bool    quercus_codec_mediaio_unregister(void);
```

- 进程级**单 provider**；`register` 校验完整 vtable、`abi_version=1`、非空 name、5 个回调，已安装返回 `-2`。
- 解码器与 probe 打开路径已改走 `quercus_codec::mediaio::open_input`：provider `can_open` 命中时用 `ffmpeg-next` 的 `StreamIo::from_read_seek` + `input_from_stream`，否则保持本地文件行为。`CodecStream::exists` 同样先问 provider。
- `unregister` 先从 codec registry 摘除（新 open 不再路由），关闭 activity gate，再用 Condvar 等待已打开的流全部关闭；返回后调用方可释放 `userdata`。可能阻塞一次在途解码。
- 单流回调由 FFmpeg 串行调用；provider 必须 `Send + Sync`，`can_open` 必须廉价无 I/O。

**SDK**（`quercus_engine_rs::mediaio`）：

```rust
pub trait MediaStream: Send + 'static {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize>;
    fn seek(&mut self, pos: std::io::SeekFrom) -> std::io::Result<u64>;
}
pub trait MediaIoProvider: Send + Sync + 'static {
    fn name(&self) -> &str;
    fn can_open(&self, path: &str) -> bool;
    fn open(&self, path: &str) -> Result<Box<dyn MediaStream>, MediaIoError>;
}
pub fn register_media_io(provider: Box<dyn MediaIoProvider>)
    -> Result<MediaIoRegistration, MediaIoError>;   // Drop = unregister + 释放 userdata
```

**限制**：
- 单 provider（多后端需在 `can_open` 里分流）。
- 源必须可 seek；不可 seek 的流在 FFmpeg 探测/长度查询阶段可能失败。
- provider 错误统一映射为 FFmpeg `EIO`，原始诊断在 `QuercusMediaIoError.message` / Rust 错误里保留。

---

## 5. 配置持久化后端

**C ABI**（prefix `quercus_core_config_`）：

```c
typedef struct QuercusConfigSourceVTable {
  uint32_t struct_size;   // 完整 v1
  uint32_t abi_version;   // 1
  int32_t (*load)(void* ud, uint8_t* buf, size_t cap);  // snprintf 语义；负数错误
  bool    (*save)(void* ud, const uint8_t* data, size_t len);
} QuercusConfigSourceVTable;

bool quercus_core_config_set_source(const QuercusConfigSourceVTable* vt, void* userdata,
                                    QuercusConfigDropFn drop_cb);
bool quercus_core_config_clear_source(void);
bool quercus_core_config_has_source(void);
```

- 装载 source 后，`ConfigStore::load()` 从宿主取 TOML 文本（两段式 buffer），`save()` 把 TOML 交给宿主；**不再读写 `<config>/config.toml`，也不做 legacy INI 迁移**。
- 未装载 source 时，文件路径、目录校验、INI 迁移、temp+rename 行为完全不变。
- 回调用 `Arc` 快照保证替换期间在途 load/save 的 `userdata` 有效；替换/清除时旧 drop 恰好一次（最后一个引用释放时执行）。回调都在 config 锁外调用。
- 进程级、v1 固定；`struct_size < v1` 或 `abi_version != 1` 拒绝。

**SDK**：`state::ConfigSource { load() -> Option<String>; save(&str) -> bool }`、
`state::set_config_source(Box<dyn ConfigSource>) -> Result<ConfigSourceRegistration, i32>`（RAII，Drop = clear）、`config_source_installed()`。

**限制**：单 source；load 返回坏 TOML 会走既有 `report_error` 并失败；guard 并发替换按“后装者生效”语义，旧 guard drop 不会误清新 source。

---

## 6. GPU 设备注入

**C ABI**（prefix `quercus_render_gpu_`）：

```c
bool    quercus_render_gpu_install(const void* wgpu_device, const void* wgpu_queue,
                                   int32_t backend_kind);
int32_t quercus_render_gpu_kind(void);          // 0..4, -1 无上下文
bool    quercus_render_gpu_is_adopted(void);
bool    quercus_render_gpu_host_installed(void);
bool    quercus_render_gpu_mark_host(void);
```

- `install` 的指针必须指向同 **wgpu 29 major** 的 `wgpu::Device` / `wgpu::Queue`；引擎只 `clone` handle，**不接管所有权**，host 保留所有权且只需调用期间存活。
- 必须在首次 render/thumbnail 之前调用：`install_shared` 在已有 adopted 上下文或 incumbent 已创建 GPU 资源时返回 false（调用方回退 staging readback）。
- `mark_host` 用于 macOS/Windows 上 gpui 不暴露 device 的场景：把引擎自建共享上下文声明为 host 上下文（影响 M5 硬件解码导入的 host gate）。
- `kind` 返回值对应 `BackendKind`：0 Auto / 1 Metal / 2 Vulkan / 3 GL / 4 CPU。

**SDK**：`render::GpuBackend`（repr(i32)）与
`unsafe fn gpu_install(device: *const c_void, queue: *const c_void, backend: GpuBackend) -> bool`、
`gpu_kind() -> Option<GpuBackend>`、`gpu_is_adopted()`、`gpu_host_installed()`、`gpu_mark_host()`。
SDK 不依赖 wgpu；Rust 调用方把 `&wgpu::Device`/`&wgpu::Queue` 转成裸指针传入。

**限制**：ABI 与 wgpu 29 绑定（混用 major 是 UB）；真实设备安装路径需要 GPU；CPU-only 环境只能验证拒绝/查询语义。

---

## 7. 验证

| 检查 | 结果 |
|---|---|
| `cargo test -p quercus-core --lib debug` | 12 passed |
| `cargo test -p quercus-render --lib` | 324 passed |
| `cargo test -p quercus-engine --lib -- --test-threads=1` | 312 passed（含 `ext::*` 适配器与 multicam 测试） |
| `cargo test -p quercus-codec --lib` | 195 passed（含内存 WAV 的 provider 端到端） |
| `cargo test -p quercus-node --lib` | 684 passed |
| `cargo test -p quercus-engine-rs --lib` | 125 passed |
| `cargo test -p quercus-storage` | 80 passed（transport 测试随适配器迁入 `engine::ext`） |
| `cargo test -p quercus-core --lib configstore` | 45 passed |
| `cargo test -p quercus-engine --lib corestate` | 7 passed |
| `python3 tooling/gen_c_header.py --check` | 通过（54 structs / 14 enums / 715 functions） |
| `gcc -std=c11` / `g++ -std=c++17` ABI smoke（新 vtable/日志/字体/缓存调用） | 通过 |

> 环境：SDK/引擎测试使用 `OCIO_SOURCE_DIR=$PWD/target/ocio-probe/src03`（可写的 OCIO 0.3 源码副本）绕过本沙箱只读 registry 的 pystring patch 问题。

---

## 8. P1 状态

P1 清单已全部落地：日志 sink、字体 provider、磁盘缓存配额、媒体 I/O、配置持久化后端、GPU 设备注入。

可选后续（不属于 P1 验收）：
- 媒体 provider 多实例 / 不可 seek 流的流式支持；
- 字体 fallback 链注册；
- GPU 设备注入的 C 侧可用性（当前实际上只适用于 Rust/wgpu 宿主）；
- 配置 source 的多实例/按会话隔离。
