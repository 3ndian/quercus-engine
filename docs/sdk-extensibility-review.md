# SDK 可替换性评审：哪些组件该让调用者换掉

> 审计对象：`quercus-engine` workspace 0.6.0（公开面 = C ABI `quercus-engine` + 安全 SDK `quercus-engine-rs`）。
> 方法：只读源码审计，未运行 `cargo build/test`（本机缺少项目 FFmpeg 工具链）。所有结论给出 `file:line`；标 **推断** 的是静态推理，未做运行验证。
> 明细审计记录见 `review/parts/{storage,boundary,pipeline,runtime}.md`。
> 状态更新（2026-10-31）：**P0-A（字节 ABI）、P0-B（传输 vtable 注册）、P0-C（库/写穿走注册表）均已落地**，
> 实现与验证记录见 [`storage-seam-p0-design.md`](storage-seam-p0-design.md) §8。
> **P1 清单（日志 sink、字体 provider、磁盘缓存配额、媒体 I/O provider、配置持久化后端、
> GPU 设备注入）已全部落地**，见 [`p1-extensibility.md`](p1-extensibility.md)。
> 正文描述的是评审时的状态，符号名以实现为准（`quercus_storage_register_transport`）。

---

## 0. 结论

**是，应该开放；但当前的问题不是"少几个组件"，而是"没有承载替换的模型"。**

三条判断：

1. **公开 ABI 里一个"提供者"型注入点都没有。** header 声明的 684 个函数
   （`crates/quercus-engine/include/quercus_engine.h:14`）里，能由调用方提供实现的只有
   **命令字符串插件**（`quercus_plugin_*`，只贡献菜单命令）和几个
   **通知型回调**（编辑订阅、撤销观察者、OFX 进度、codec 任务提交）。
   配置键（`quercus_core_config_*`）只能在内置枚举里选，不能换实现。
2. **内部其实设计对了，但停在了进程边界内侧。** `StorageBackend`、`Decoder`/`Encoder`、
   `GpuContextLike`、`JobDispatch`、`NodeBehavior`、`TextMeasureBackend` 等 trait
   都在 Rust 内部存在，却没有一个能从 C ABI 实现。根因是 ABI 边界：
   `quercus-engine` 只产出 `cdylib`（`crates/quercus-engine/Cargo.toml:22`），
   SDK 只 `#[link]` 这个动态库并调用 `extern "C"` 符号，
   **Rust trait / `Arc` / `&str` / 泛型过不去这道边界**。
3. **你举的存储正是最典型的例子——而且是最接近能做成的一个。** 它已经有 trait、
   有注册表、有 URI 分派、有三个内置后端；缺的是最后 20%：注册符号、
   库/写穿路径改走注册表、字节级序列化契约、SDK 白名单。
   一个 README 里宣称存在的 `quercusstorage_backend_register` 实际上**从未实现**
   （全仓仅 `crates/quercus-storage/README.md:31` 一处提及）。

一句话：**今天调用者能"选"（配置文件格式、内置 sqlite/pg、GPU 枚举、OFX 插件目录），
不能"换"（存储实现、媒体 IO、字体、日志、GPU 设备、配置持久化）。**

---

## 1. 评审口径：什么组件"应该"可替换

可替换不是越多越好。用五条判据筛：

| # | 判据 | 说明 |
|---|---|---|
| 1 | 调用方已有同职责基础设施 | 存储、日志、配置、字体、GPU 设备、资产/代理库。逼调用方再维护一套必然出错 |
| 2 | 与部署形态强相关 | 桌面编辑器 / 无头渲染农场 / 服务端批处理，对 IO、设备、并发的假设完全不同 |
| 3 | 替换不改变交付物语义 | 换存储不能改变项目格式；换字体不能改变排版结果的定义 |
| 4 | 失败可降级 | 后端不可用时应回退到本地/内存，而不是让整个引擎不可用 |
| 5 | 生命周期可讲清 | 进程级注册 + `userdata` 归属，或会话级句柄；二者必须明确其一 |

反向判据（**不该**开放的典型）：

- **替换会破坏一致性/可复现性**：FFmpeg 编解码集合、OCIO 色彩管线。
  调用方真正需要的是"输入从哪来、输出到哪去"，不是"换解码库"。
- **成本远高于收益**：逐帧跨 C ABI 的自定义解码器、自定义调度器 trait。
- **标准已经给出答案**：OFX 插件。宿主整体替换没有产品意义，
  该开放的是宿主对外**宣称的能力**与**插件白名单**。

---

## 2. 现状总览

图例：**注入** = 调用方提供实现；**配置** = 只能在内置项里选；
**回调** = 只能接收事件；**无** = 完全写死。

| 组件 | 内部是否已有 trait/缝隙 | 公开 ABI 现状 | 判定 |
|---|---|---|---|
| 项目文件 open/save（`.ove`/`.otio`/`.fcpxml`） | ✅ `StorageBackend` + `Registry` | 只有只读枚举，**无注册符号** | 半可替换（最该补） |
| 项目库 / 写穿 / 自动保存 | ❌ 直连 `DatabaseBackend` 单例 | 配置 `Storage/Backend=sqlite\|pg\|off` | 硬编码 |
| 媒体读取（解码输入） | ❌ FFmpeg 按**文件名**打开 | 无 | 硬编码 |
| 解码器 / 编码器 | ✅ `Decoder` / `Encoder` | 按 id 选 2 个内置（其一为 stub） | 无 |
| GPU 渲染后端 | ✅ `GpuContextLike` + `adopt` | 4 个枚举值 | 配置 |
| 渲染作业调度 | ✅ `JobDispatch` | 4 个枚举值，参数不可传 | 配置 |
| 音频输出 | ❌ cpal 具体类型 | 设备索引；或绕开取 buffer | 部分 |
| 文字/字体 | ✅ `set_text_backends` 函数指针 | 只能装内建 cosmic-text | 无 |
| 配置存储 | ❌ 进程单例 + 固定 TOML | 键值读写 + load/save | 配置 |
| 日志/诊断 | ⚠️ `log` facade 已写但未用 | 无 | 无 |
| 撤销栈 | ❌ 进程级 `OnceLock` | 观察者只增不减 | 回调 |
| 插件 / OFX | ✅ | 命令插件、OFX 扫描/进度 | **注入（已可用）** |

规模参考：header 声明 684 个函数 / 48 个 POD 结构 / 13 个枚举（`quercus_engine.h:14`）；
引擎侧大量导出由宏批量生成（`handle.rs:59 export_handle!`、`codec/frame.rs:67 frame_getter!`），
静态 grep 只能得到 600+ 个 `#[unsafe(no_mangle)]` 注解，精确数以构建产物为准。
关键是**header 从 SDK 的 `extern "C"` 块生成**（`tooling/gen_c_header.py:25`），
因此存在双向漂移：`quercus_undo_push_owned`、`quercus_audio_params_rational_from_double`
有实现但不在 header 中。这决定了新增缝隙的落地流程（见 §6.7）。

---

## 3. 存储专题：用户例子

### 3.1 今天能做什么

```text
quercus_storage_open/save  ──►  Registry::global().resolve(uri)  ──►  后端
   (storage.rs:80/121)              (registry.rs:87)                 ove-xml / otio / quercusdb
```

`open/save`（以及 `quercus_editor_open/save`，`editor.rs:192,360`）确实是走注册表的。
内置后端有三个：`.ove` XML、`.otio`/`.fcpxml`、`quercusdb+sqlite/pg`
（`crates/quercus-storage/src/backends/mod.rs:31`）。

但**没有注册入口**：storage 的 24 个 C 导出（`quercus_engine.h:1244-1267`）全是使用面，
没有 `register`/`unregister`/`set_backend`。`Registry::register` 虽然在内部是 `pub`
（`registry.rs:55`），生产代码零调用，唯一调用点是集成测试
（`crates/quercus-storage/tests/storage_test.rs:576`）。

### 3.2 四个断点

| # | 断点 | 证据 | 后果 |
|---|---|---|---|
| B1 | **没有注册符号**；README 宣称的 `quercusstorage_backend_register` 不存在 | `crates/quercus-storage/README.md:31`；全仓 grep 仅此一处；`src/` 下也没有它引用的 `ffi.rs` | C 消费者无法新增后端 |
| B2 | **库/写穿/自动保存绕过注册表**，直连 `OnceLock<DatabaseBackend>` | `writethrough.rs:118`（单例）；`writethrough.rs:305,474,551`；`engine/src/storage.rs:225` 起的 15 个 `library_*`；`editor.rs:241,335,414` | 即使注册了自定义后端，项目库、自动保存、会话绑定仍写进内置 DB |
| B3 | **契约是"项目句柄"而不是"字节"** | `StorageBackend::load` 返回 `CHandle`（`backend.rs:81`）；`nodeutil::project_arc` 用 unchecked downcast（`nodeutil.rs:51`） | 外部实现无法合法构造句柄；伪造即 UB。且 ABI 只有按**路径**的 `quercus_project_load/save`（`node.rs:2488/2444`），没有 bytes ↔ project |
| B4 | **SDK 按名字白名单拒绝** | `ProjectLibrary::new` 要求 `backend_for_uri(uri) == Some("quercusdb")`（`crates/quercus-engine-rs/src/storage.rs:304`） | 引擎侧将来支持了，SDK 也先拒 |

另外两个正确性缺陷（顺手该修）：

- `quercus_storage_open` 在 `resolve` 失败时返回 null 但 `version_info` 保持 0
  （`engine/src/storage.rs:80-90`）；而 `load` 失败却把**负错误码**写进
  `version_info`（`:93-95`），与"正数信息码"的契约冲突。SDK 侧因此可能拿到
  `StorageError(0)`（`crates/quercus-engine-rs/src/storage.rs:144-153`）。
- 库导出（`database/mod.rs:611`）固定用 `OveXmlBackend` 做 export，
  而旁边的注释声称"按扩展名经 registry 分派"（`editor_commands/media.rs:445`）。

还有一个不显眼但真实的成本：`quercus-storage` **无条件**依赖
`sea-orm` + `sqlx`（postgres & sqlite）+ `tokio`（`crates/quercus-storage/Cargo.toml:31-32`），
没有 feature 开关。只想用 `.ove` 文件格式的消费者，也要为内置数据库后端付出依赖树
（整个 workspace 的 `Cargo.lock` 共 469 个包）与二进制体积。库后端一旦可插拔，
这组依赖应当变成可选 feature。

### 3.3 为什么"直接依赖内部 crate"救不了

- README 明确：除 `quercus-engine`/`quercus-engine-rs` 外都是实现细节，
  "Do not depend on them directly"（`README.md:28`）；storage 自己也在 crate 文档首行重申
  （`crates/quercus-storage/src/lib.rs:1`）。
- 即使无视这条：`quercus-engine` 以 cdylib 形式把 `quercus-storage` **静态链接进 `.so`**。
  消费者再依赖一份 `quercus-storage`，进程里会出现**两份 `Registry`**
  （各自一个 `OnceLock` 静态），消费者注册的是自己那份，引擎那份看不见。
  Rust mangled 符号也不是 cdylib 的导出面。**推断**（基于 crate-type 与 cdylib 导出规则，
  未做链接实验），但这正是"必须走 C ABI"的结构性原因。

### 3.4 目标形状

原则：**后端只负责"把一段字节存到某个 URI / 从某个 URI 取回"，不接触项目图。**

这样一次性解决 B3：序列化、schema 校验、版本判定留在引擎；外部后端不可能伪造项目句柄；
格式型后端继续作为编译期内置。

```c
/* 按 §6 的准入规范设计（本仓库目前还没有 vtable 先例，这会是第一个） */
typedef struct QuercusStorageError {
  int32_t code;            /* QUERCUSSTORAGE_OK / E_* */
  uint8_t message[512];    /* UTF-8, NUL 结尾, best-effort */
} QuercusStorageError;

typedef struct QuercusStorageBackendVTable {
  uint32_t struct_size;    /* sizeof(本结构)：未来加字段的尺寸协商 */
  uint32_t abi_version;    /* = 1 */
  const char* name;        /* 进程生命周期内有效，不得与内置重名 */
  const char* scheme;      /* 如 "s3" / "acme+pg" */

  int32_t (*can_handle)(void* ud, const uint8_t* uri, size_t len);

  /* 成功时把字节所有权交给引擎，引擎读完回调 free_bytes 恰好一次 */
  int32_t (*load)(void* ud, const uint8_t* uri, size_t len,
                  uint8_t** out_bytes, size_t* out_len,
                  int32_t* out_version_info,          /* 可空 */
                  QuercusStorageError* err);
  void    (*free_bytes)(void* ud, uint8_t* bytes, size_t len);

  int32_t (*save)(void* ud, const uint8_t* uri, size_t len,
                  const uint8_t* bytes, size_t bytes_len,
                  uint32_t options, QuercusStorageError* err);

  /* 可选槽位：NULL = 不支持库能力（对应现有 15 个 library_* 导出） */
  int32_t (*library_list)(/* … */);
  int32_t (*library_delete)(/* … */);
  int32_t (*library_rename)(/* … */);
  int32_t (*library_snapshot)(/* … */);
} QuercusStorageBackendVTable;

int32_t quercus_storage_register_backend(const QuercusStorageBackendVTable* vt);
bool    quercus_storage_unregister_backend(int32_t slot);
```

配套必须先补的两个原语（否则 vtable 只是空壳）：

```c
void*   quercus_project_load_from_bytes(const uint8_t* xml, size_t len);
int32_t quercus_project_save_to_bytes(const void* project, uint8_t* buf, size_t cap);
```

它们直接对应内部已有的 `nodeutil::serializer_load/save`
（`crates/quercus-storage/src/nodeutil.rs:71-78`），实现成本很低，
却让"项目存在 Postgres/S3/HTTP"从不可能变成可能。

SDK 侧按现有先例封装（`quercus_codec_task_set_submit_cb` 已经示范了
trampoline + `Box::into_raw` 的手法，`crates/quercus-engine-rs/src/codec.rs:679-706`）：

```rust
pub trait StorageBackend: Send + Sync + 'static {
    fn name(&self) -> &str;
    fn scheme(&self) -> &str;
    fn can_handle(&self, uri: &str) -> bool;
    fn load(&self, uri: &str) -> Result<(Vec<u8>, i32), StorageError>;
    fn save(&self, uri: &str, bytes: &[u8], options: u32) -> Result<(), StorageError>;
    // 可选：library_* 默认 Unsupported
}

pub struct BackendRegistration { slot: i32 }   // Drop 即 unregister
pub fn register_backend<B: StorageBackend>(b: B) -> Result<BackendRegistration, StorageError>;
```

### 3.5 迁移顺序（每步都可独立发布，不破坏现有行为）

1. **文档纠偏**：删除 `quercusstorage_backend_register` 的宣称，或标注 planned。
   现在 README 还把 database 后端描述成 `todo!()` stub，实际是 1654 行已注册实现
   （`crates/quercus-storage/backends/mod.rs:31`）。
2. **字节原语**：`quercus_project_load_from_bytes` / `save_to_bytes`。
3. **注册符号 + 字节 vtable**：`Registry::global().register` 适配层；
   默认行为不变（`file://` 仍先命中内置后端）。
4. **库/写穿改走注册表**：`writethrough::backend()` 变为 `library_backend(uri)`，
   先 `Registry::resolve(uri)`，未命中再回退内置 `DatabaseBackend`；
   `storage_enabled()` 的白名单（`writethrough.rs:338`）改为"配置的库 URI 可解析即启用"。
5. **SDK 去掉名字白名单**（`engine-rs/src/storage.rs:304`），改为能力查询。
6. **错误通道**：`open/save` 增加 `QuercusStorageError*` out 参数，修复 `version_info` 语义。

---

## 4. 其他候选，按优先级

### P0（不做则"可替换"名不副实）

| 组件 | 为什么 | 最小改动 | 成本 |
|---|---|---|---|
| **项目字节序列化** | 一切自定义存储/同步/云端的公共前提 | 2 个导出（见上） | S |
| **存储后端注册** | 用户例子；内部 80% 已就绪 | vtable + register/unregister | M |
| **库与写穿可替换** | 否则自动保存仍写内置 DB | 去掉 `DatabaseBackend` 单例硬编码 | M |
| **日志/诊断 sink** | 所有消费者都需要；`debug.rs` 已写好等级日志却**没有 ABI 且内核不调用**，实际全走 `eprintln!`（`crates/quercus-engine/src/worker/mod.rs:83`、`quercus-render/src/procpool.rs:1242`） | `quercus_core_log_set_level` + `set_sink(cb, ud)`；把 `eprintln!` 改走 `debug::log` | S |

### P1（高价值、中成本）

| 组件 | 为什么 | 最小改动 | 成本 |
|---|---|---|---|
| **媒体 IO（VFS/AVIO）** | FFmpeg 按纯文件名打开（`quercus-codec/src/ffmpeg.rs:784`），"媒体不在本地"（对象存储/代理缓存/厂商 raw）今天只能先落盘。这是渲染农场最痛的一条 | `quercus_codec_mediaio_install(vtable)` → `avio_alloc_context`；`CodecStream::exists()` 改查 vtable | M |
| **字体注入** | 品牌字体/多语言/农场统一字体目录；改动集中在 `textengine.rs` | `textengine_add_font_dir` / `add_font_data` + 字体列表缓存可失效 | S |
| **配置持久化后端** | 单例 + 固定 `config.toml`，宿主无法接自己的设置系统；两个消费者无法隔离 | `ConfigSource` vtable（load/save）或 `engine_init(options)` 传入 | M |
| **磁盘缓存配额/路径** | 目前**没有容量上限与淘汰**，长会话/农场会把盘写满；`DiskCacheBehind/Ahead` 是无消费者的死配置（`configstore.rs:588`） | `set_max_bytes` / `set_path` + LRU | M |
| **GPU 设备注入** | `adopt`/`install_shared` 存在但**无 ABI 出口**（`corestate.rs:7-9` 明说 "deliberately NOT exported"），零拷贝 present 对任何消费者不可达 | `quercus_render_gpu_install(device*, queue*, kind)`；代价是 ABI 锁 wgpu major（29） | M |

> GPU 那条要诚实：只要跨 ABI 传 wgpu 句柄，ABI 就与 wgpu 大版本绑定。
> 若不接受这个绑定，正确做法是删掉误导性注释、承认"零拷贝 present 仅 in-tree 可用"。

### P2（有明确客户再做）

| 组件 | 判断 |
|---|---|
| 渲染 worker 配置（worker 数/槽位/超时/worker 路径） | 做**配置结构体**（`init_ex`），不要导出 `JobDispatch` trait——调用方实现调度器没有产品价值 |
| OFX 宿主身份 / GL 能力声明 | 是**配置**问题不是替换问题。现在 Linux/Windows 也向插件宣告 `OpenGLRenderSupported="true"`（`quercus-ofx-plugin/src/host.rs:1529-1531`），而 GL 桥在非 macOS 恒 false（`gl_bridge.rs:815`） |
| 音频输出开关 | 先加 `set_output_enabled(false)`，让调用方用 `quercus_task_audio_render` 自己送 NDI/SDI/ASIO；sink vtable 推后 |
| 解码器工厂 | 先修**零 ABI** 的 bug：图渲染取帧硬编码 `Arc::new(FFmpegDecoder::new())`（`quercus-render/src/eval.rs:1293`），注册表与 `set_test_decoders` 在主路径完全失效；第二个内置解码器 OIIO 是 stub（`oiio.rs:73`） |
| 撤销作用域 | 全局栈是**文档化的故意设计**（`quercus-engine-rs/README.md:78`）。单文档应用可接受；多文档才需要 `UndoSession` 句柄，属大改 |

### 明确不建议开放

- **FFmpeg 编解码集合**：静态链接 + 项目专用构建；开放等于让消费者承担
  版本/许可/链接矩阵，ROI 为负。
- **OCIO 色彩管理**：唯一合理的替换是换 config 文件，已有 `OCIO` env。
- **OFX 宿主整体**：OFX 是标准，换宿主等于换掉整个 `quercus-ofx-plugin`。
- **音频 DSP / 混音**：调用方要的是样本，不是替换算法。
- **节点图/特效系统**：已有 OFX 这条标准路径。

---

## 5. 关键的架构判断

### 5.1 没有 EngineContext，所以"没地方放注入"

仓库里的进程级静态（`OnceLock`/`LazyLock`/`static mut`）有 100 余处，其中一批是关键单例：
配置、撤销栈、活动工程、RenderManager、GPU 共享槽、音频管理器、OFX 宿主、
FFmpeg 初始化、帧管理器、解码器注册表。

后果不是"单例不好"，而是**注册顺序由首次使用隐式决定，且无法重置**：

- 先读 config → 后 `ConfigStore::load()`：`load()` 内部 `set_defaults()` 会
  **清掉**此前 `set()` 的值（`configstore.rs:103,536`）。静默丢配置。
- 先 `GpuContext::shared()`（任何一个缩略图任务）→ 再 `install_shared(host_ctx)`：
  接管失败，进程永久用引擎私有 device（`backend.rs:1865-1888`）。
- 第二个消费者 `RenderManager::init()` 直接返回 `Err(State)`（`manager.rs:131-142`）。
- `textengine::install()` 用 `Once`，注释直言 "a second engine instance cannot swap the hooks"
  （`textengine.rs:141`）。

**建议**：短期不强行重构单例，而是新增一个显式的
`quercus_engine_init(const QuercusEngineOptions*)` 把"注册顺序"收口，
并在头文件写死一张 **configure-before-first-use 清单**。
中期若"一进程多实例"是产品需求，再把上述单例搬进 `EngineContext` 句柄——
那是一次独立的大改，不应混在"加几个 seam"里做。

### 5.2 现有注入先例及其缺陷

能携带 `userdata` 的注入点共四个，它们证明模式可行，但都停在"单回调"层面，
没有一个能替换**多方法实现**：

- **命令插件**：descriptor + 一个 `CommandFn`，只能贡献菜单命令、不参与引擎行为
  （`plugin.rs:68`）。可注销，且回调在注册表锁外调用（`plugin.rs:239-252`）——这是正面范例。
- **codec task submit**：真正的函数指针注入，但重复注册会**泄漏**上一个闭包
  （`crates/quercus-engine-rs/src/codec.rs:679`）。
- **OFX 进度**：`ProgressFn` + `ProgressEndFn` + userdata，可替换/清空
  （`ofx/host.rs:351-380`）。
- **编辑订阅**：唯一带 `drop_callback`（`quercus_engine.h:928`），且两侧都
  `catch_unwind`（`editor/notify.rs:92`）。

共同的缺陷：

- **panic containment 只覆盖 edit subscription**（`editor/notify.rs:92` 两侧都有
  `catch_unwind`）；plugin / undo observer / OFX progress 都是裸调用。
  Rust 1.85 下 panic 穿过 `extern "C"` 会 abort（**推断**，未运行验证）。
- **undo observer 只增不减、无析构**（`undo.rs:75` "there is no removal"），
  且派发时**持锁**调用回调（`undo.rs:82-85`）→ 回调内再触发 undo 即死锁。
- SDK README 承诺 "the SDK serializes Rust callbacks and catches panics"
  （`README.md:110`），与实际不符。

---

## 6. 新增缝隙的 ABI 准入规范

今后每个 seam 都应满足以下 9 条（否则会在下一个版本变成包袱）：

1. **不跨 dylib 传 Rust ABI**。`Arc<dyn Trait>`、`&str`、泛型、闭包一律不行。
   只用 `(ptr, len)`、`repr(C)`、`u64` 句柄、`extern "C"` 回调。
2. **注入 = (vtable 或 fn ptr, userdata) 对**。不要新增"裸函数指针"入口——
   现有 48 个 POD 结构里只有 `StorageProjectHandle` 有 `abi_version`，
   裸回调无法在不破坏 ABI 的前提下加参数。
3. **新结构必须有 `struct_size` + `abi_version`**，由调用方填写，引擎校验。
4. **`userdata` 生命周期二选一并写进头文件**：注册即永久（进程级）或
   带 `drop_callback`（现有 `quercus_editor_edit_subscribe` 是唯一范例）。
5. **每个回调调用点 `catch_unwind`**，且**不得持锁调用**。
6. **跨 DLL 内存不互 free**：返回给引擎的字节必须配一个 `free_*` 回调。
7. **落地流程是三处**：引擎 `#[unsafe(no_mangle)]` → SDK `unsafe extern "C"` 声明
   → `python3 tooling/gen_c_header.py --write`。只加引擎导出不会进 header。
8. **`docs-only` feature 门控**：`quercus-engine` 的每个模块都 `cfg(not(feature="docs-only"))`，
   SDK 的 docs.rs 用 `--no-default-features`；新代码必须保持可文档化构建。
9. **线程契约写清楚**：回调可能来自调用者线程、渲染 worker 子进程、或
   `quercus-storage-snapshot` 后台线程（`writethrough.rs:411`）。
   重入规则（回调内禁止再进同一注册表）也要写。

---

## 7. 风险

| 风险 | 说明 | 缓解 |
|---|---|---|
| **伪造句柄 → UB** | 现状 `load` 返回 `CHandle`，引擎用 unchecked downcast 解引用（`nodeutil.rs:51`）。若 vtable 允许外部返回句柄，就是内存安全问题 | 改用字节流契约（§3.4），句柄只留在引擎内 |
| **双份静态链接** | 消费者依赖内部 crate 会得到自己的 `Registry` 静态，与 cdylib 里那份互不可见（推断） | 明确"只走 C ABI"，把内部 crate 标成 `publish = false` 或加编译期警告 |
| **远程后端阻塞** | 写穿失败被吞（`writethrough.rs:314`），退出 `flush_all` 会 join 快照线程；网络后端可能卡死退出 | 给 load/save 加取消标志与超时；失败上报到 `storage_last_error` |
| **格式漂移** | 若后端自己决定 framing/版本，不同后端可写出互不兼容格式 | 字节化后由引擎统一序列化，格式型后端只能编译期内置 |
| **仲裁被劫持** | `resolve` 取第一个 `can_handle`（`registry.rs:87`），外部后端可用宽泛匹配抢走 `file://` | 注册时检查 scheme 冲突 + 显式 priority；内置 scheme 保留 |
| **ABI 版本锁** | GPU 句柄绑定 wgpu 29；存储 vtable 绑定项目 XML schema | 只开放稳定抽象（字节流），不开放内部类型 |

---

## 8. 路线图建议

| 阶段 | 内容 | 预估 |
|---|---|---|
| **P0（1–2 周）** | 文档纠偏；`project_load_from_bytes`/`save_to_bytes`；存储注册 vtable；错误通道修复；日志 sink | 让"换存储"第一次成为可能 |
| **P1（2–4 周）** | 库/写穿走注册表；SDK 去掉 `quercusdb` 白名单；字体注入；媒体 IO vtable；磁盘缓存配额 | 覆盖农场与桌面两类真实场景 |
| **P2（按客户）** | 配置持久化后端；GPU 设备注入；worker 配置结构体；OFX 身份/GL 开关；音频输出开关 | 有明确消费者再做 |
| **独立路线** | EngineContext（多实例隔离：config/undo/render/active project） | 大改，单独立项 |

判定门槛建议：**每加一个 seam，先写出"调用方用它做什么"的具体场景**；
写不出场景的（如 `JobDispatch` trait、解码器工厂）就留到 P2 或不做。

---

## 9. 证据索引（关键结论）

| 结论 | 证据 |
|---|---|
| header 684 函数，48 结构，13 枚举 | `quercus_engine.h:14` |
| storage 无注册符号 | `quercus_engine.h:1244-1267`（24 个导出全为使用面） |
| README 宣称的注册 vtable 不存在 | `crates/quercus-storage/README.md:31`；`src/` 无 `ffi.rs` |
| 写穿/库直连 `DatabaseBackend` | `writethrough.rs:118,305`；`engine/src/storage.rs:225`；`editor.rs:241,335,414` |
| 内部注册表只在测试里用过 | `registry.rs:55`；`tests/storage_test.rs:576` |
| 项目 ABI 只有路径版 | `engine/src/node.rs:2444,2488` |
| SDK 按名字拒绝自定义库 | `crates/quercus-engine-rs/src/storage.rs:304` |
| 解码主路径绕过注册表 | `quercus-render/src/eval.rs:1293` |
| GPU 接管无 ABI 出口 | `engine/src/corestate.rs:7-9`；`backend.rs:1865` |
| 无日志 sink | `corestate.rs`（无 log 导出）；`worker/mod.rs:83`；`procpool.rs:1242` |
| panic containment 仅 edit 订阅 | `editor/notify.rs:92`；对比 `undo.rs:82-85`、`plugin.rs:252` |
| 无引擎级 init | header 无 `quercus_engine_init`；仅子系统初始化 |
| 配置 `load()` 清表 | `configstore.rs:103,536` |
| header 由 SDK 生成 | `tooling/gen_c_header.py:25` |
| 内部 crate 不承诺稳定 | `README.md:28`；`quercus-storage/src/lib.rs:1` |

---

## 10. 方法与未验证项

- 全程只读源码；未运行 `cargo build/test`——本机没有项目 FFmpeg 8.1 工具链，
  README 已说明系统 FFmpeg 会产生链接错误。
- 以下为**推断**，未做运行/链接验证：
  - 消费者依赖内部 crate 会得到第二份 `Registry`（§3.3）；
  - panic 穿过 `extern "C"` 会 abort（Rust 1.85 + edition 2024）；
  - 远程后端阻塞会让退出 flush 卡在 join。
- 未覆盖：OFX 91 个导出的实现细节、codec 89 个导出的实现细节、
  渲染进程池的重启/扩缩容实现；`docs/zh/plans/riir/M10-*.md` 等设计文档在仓库中不存在，
  无法核对原始 vtable 约定——§3.4 的草案是按现有代码风格与
  `codec/task.rs:61`、`plugin.rs` 的注册先例设计的。
