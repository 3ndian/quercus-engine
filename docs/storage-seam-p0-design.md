# 存储可替换性 P0 落地设计

> 配套评审：[`sdk-extensibility-review.md`](sdk-extensibility-review.md)。
> 本文把 P0 的三件事拆成可实现的步骤：**字节原语 / 传输 vtable / 库与写穿走注册表**，
> 并给出 C 结构、引擎适配器、生命周期契约、URI 约定、测试与工时。
> 所有路径相对仓库根；行号为审计时刻（2026-10-30）的物理行号。

---

## 0. 先修正依赖关系

上一版评审把 P0 写成 `字节 → vtable → 库/写穿` 的箭头。严格说这是**叙述顺序**，
不是构建依赖。真实依赖图是：

```text
[P0-A 项目字节原语]  ──────────────┐（可独立发布；调用方自管字节时必需）
                                   ├──► [P0-C 库能力 + 写穿走注册表]
[P0-B 传输 vtable + 注册] ─────────┘（P0-C 的前置）
```

- **P0-B 不依赖 P0-A**：vtable 的 `load` 拿回字节、`save` 交出字节，
  引擎侧适配器用已有的 `nodeutil::serializer_load/save`（`crates/quercus-storage/src/nodeutil.rs:71-78`）
  就能把字节转成项目/把项目转成字节。所以"没有字节 ABI 就写不出 vtable"是不成立的。
- **P0-A 的独立价值**：调用方要把项目塞进**自己的容器**（自有文档格式、云同步协议、
  剪贴板、单元测试）时，需要 memory ↔ project；否则只能落临时 `.ove` 文件。
  它也是 C 侧验证一个自定义后端的最简工具。
- **P0-C 依赖 P0-B**：没有可解析的外来后端，"库走注册表"没有对象。

> 一句话：**P0-A 和 P0-B 可以并行；P0-C 必须等 P0-B。**
> 建议的交付顺序仍是先做 P0-B（价值最高），P0-A 并行做，最后 P0-C。

---

## 1. P0-B：传输 vtable（核心）

### 1.1 为什么必须是"字节"而不是"项目句柄"

现状 `StorageBackend::load` 返回 `CHandle`（`crates/quercus-storage/src/backend.rs:81`），
`save` 接收 `CHandle`。若直接把这个形状搬到 C：

1. **内存安全**：引擎把 `CHandle.ctx` 当作 `RefBox<ProjectArc>` 解引用，
   而 `nodeutil::project_arc` 用的是**不做类型校验**的 `get::<ProjectArc>`
   （`nodeutil.rs:51-55` → `crates/quercus-node/src/handle.rs:136`）。
   外部实现返回自己构造的句柄 = 未定义行为。
2. **格式失控**：句柄背后是引擎的 `Project`；外部后端能读到什么、写回什么，
   引擎无法约束版本与 schema。
3. **跨语言不可行**：外部（C/C++/其他语言）没有 `Project` 类型，
   只能通过节点 API 逐节点重建——既昂贵又丢失 `ptr` 身份映射。

字节契约把后端降级为**纯传输**：存什么格式、怎么版本化由引擎决定；
后端只负责"这段字节放在哪"。这同时消除了 `StorageUri`、`Result`、`Arc` 等
非 FFI-safe 类型（当前 trait 全都用了，见 `backend.rs:67-91`）。

### 1.2 C 结构

```c
#define QUERCUS_STORAGE_TRANSPORT_VERSION 1u

/* 失败详情：code 用现有 QUERCUSSTORAGE_E_*；message 是 UTF-8、NUL 结尾的
   best-effort 诊断（引擎会复制，不保留指针）。 */
typedef struct QuercusStorageError {
    int32_t code;
    uint8_t message[512];
} QuercusStorageError;

/* 负载格式提示（P0-A 的 codec 选择用；0 = 由 URI 扩展名决定）。 */
typedef enum QuercusStorageFormat {
    QUERCUS_STORAGE_FORMAT_AUTO = 0,
    QUERCUS_STORAGE_FORMAT_OVE_XML = 1,
    QUERCUS_STORAGE_FORMAT_OTIO_JSON = 2,
    QUERCUS_STORAGE_FORMAT_FCPXML = 3,
} QuercusStorageFormat;

typedef struct QuercusStorageTransportVTable {
    uint32_t struct_size;      /* = sizeof(本结构)，由调用方填；未来加字段 */
    uint32_t abi_version;      /* = QUERCUS_STORAGE_TRANSPORT_VERSION */

    /* 引擎复制这两段文本；调用后原指针可失效。name 不得与已注册项重名。 */
    const char* name;
    const char* scheme;        /* 主 scheme，如 "s3"；空 = 纯靠 can_handle */

    /* 写入策略：0 = 引擎默认（外来后端用 coalesced，见 §3.4）
                  1 = immediate（每条命令同步写）
                  2 = coalesced（标脏 + 后台合并写）
                  3 = explicit（只 flush/unbind 时写） */
    uint32_t write_policy;

    /* uri 是原始文本（引擎不做预解析），只在调用期有效。
       1 = 认领；0 = 不认领；<0 = 错误（写入 err）。必须廉价、无 I/O。 */
    int32_t (*can_handle)(void* ud,
                          const uint8_t* uri, size_t uri_len,
                          QuercusStorageError* err);

    /* 读取。成功返回 0，并通过 out_bytes/out_len 把字节所有权交给引擎；
       引擎读完调用 free_bytes 恰好一次（包括出错/取消路径）。
       out_format 可空：写 QuercusStorageFormat。 */
    int32_t (*load)(void* ud,
                    const uint8_t* uri, size_t uri_len,
                    uint8_t** out_bytes, size_t* out_len,
                    int32_t* out_format,
                    QuercusStorageError* err);
    void (*free_bytes)(void* ud, uint8_t* bytes, size_t len);

    /* 写入。bytes 是引擎序列化好的项目数据，只在调用期有效。
       options 为 QUERCUSSTORAGE_SAVE_* 位掩码（保留位必须忽略）。 */
    int32_t (*save)(void* ud,
                    const uint8_t* uri, size_t uri_len,
                    const uint8_t* bytes, size_t bytes_len,
                    uint32_t options,
                    QuercusStorageError* err);

    /* ---- 可选库能力；NULL = 不支持。语义见 §3.3 ---- */
    int32_t (*library_list)(void* ud,
                            const uint8_t* uri, size_t uri_len,
                            QuercusStorageProjectInfo* out, size_t cap,
                            size_t* out_count,
                            QuercusStorageError* err);
    int32_t (*library_delete)(void* ud,
                              const uint8_t* uri, size_t uri_len,
                              const uint8_t* uuid, size_t uuid_len,
                              QuercusStorageError* err);
    int32_t (*library_rename)(void* ud,
                              const uint8_t* uri, size_t uri_len,
                              const uint8_t* uuid, size_t uuid_len,
                              const uint8_t* name, size_t name_len,
                              QuercusStorageError* err);
    /* 仅增量/日志型后端需要；整文档后端可留 NULL。 */
    int32_t (*library_snapshot)(void* ud,
                                const uint8_t* uri, size_t uri_len,
                                const uint8_t* uuid, size_t uuid_len,
                                QuercusStorageError* err);
} QuercusStorageTransportVTable;

int32_t quercus_storage_register_transport(const QuercusStorageTransportVTable* vt,
                                           void* userdata);
bool    quercus_storage_unregister_transport(int32_t slot);
```

库列表的 POD：

```c
typedef struct QuercusStorageProjectInfo {
    uint8_t uuid[40];        /* NUL 结尾 */
    uint8_t name[256];       /* NUL 结尾 */
    int64_t created_at;      /* Unix 秒 */
    int64_t modified_at;     /* Unix 秒 */
    int32_t schema_ver;
    int32_t reserved;
} QuercusStorageProjectInfo;
```

### 1.3 引擎侧适配器（关键实现）

新增 `crates/quercus-engine/src/ext/storage_transport.rs`：把 vtable 包成内部 `StorageBackend`（P1 收尾时从 storage crate 迁入 ABI crate）。
这样**不需要改 `resolve/open/save` 的主流程**，外来回后端就是注册表里的一个普通项。

```rust
struct ForeignBackend {
    vt: OwnedVTable,          // 复制结构体；name/scheme 已转成 String
    ud: *mut c_void,          // 调用方所有
    name: String,
    scheme: String,
    op_lock: Mutex<()>,       // 序列化同一后端的调用（见 §1.5）
}

// Safety: 契约要求 userdata 线程安全；引擎用 op_lock 串行化。
unsafe impl Send for ForeignBackend {}
unsafe impl Sync for ForeignBackend {}

impl StorageBackend for ForeignBackend {
    fn name(&self) -> &str { &self.name }
    fn uri_scheme(&self) -> &str { &self.scheme }

    fn can_handle(&self, uri: &StorageUri) -> bool {
        let s = uri.to_uri_string();               // uri.rs:89
        let mut err = Default::default();
        (self.vt.can_handle)(self.ud, s.as_ptr(), s.len(), &mut err) > 0
    }

    fn load(&self, uri: &StorageUri) -> Result<LoadResult> {
        let s = uri.to_uri_string();
        let mut bytes: *mut u8 = std::ptr::null_mut();
        let mut len = 0usize;
        let mut fmt = 0i32;
        let mut err = Default::default();
        let _g = self.op_lock.lock().unwrap();   // load/save 共用一把锁
        let rc = (self.vt.load)(self.ud, s.as_ptr(), s.len(), &mut bytes, &mut len, &mut fmt, &mut err);
        // 用 guard 保证 free_bytes 恰好一次（含错误/UTF-8 失败路径）
        let _guard = FreeGuard { vt: &self.vt, ud: self.ud, bytes, len };
        if rc != 0 { return Err(map_error(&err)); }
        if bytes.is_null() { return Err(Error::NoMem); }
        let text = std::str::from_utf8(unsafe { std::slice::from_raw_parts(bytes, len) })
            .map_err(|_| Error::Format("backend returned non-UTF-8".into()))?;
        let project = nodeutil::serializer_load(text)?;   // 格式由引擎决定
        Ok(LoadResult::success(nodeutil::make_project_owned(project)))
    }

    fn save(&self, project: CHandle, uri: &StorageUri, options: u32) -> Result<()> {
        let arc = unsafe { nodeutil::project_arc(&project)? };
        let xml = { let g = arc.lock().map_err(|_| Error::State)?;
                    nodeutil::serializer_save(&g)? };
        let _g = self.op_lock.lock().unwrap();
        let s = uri.to_uri_string();
        let mut err = Default::default();
        let rc = (self.vt.save)(self.ud, s.as_ptr(), s.len(),
                                xml.as_ptr(), xml.len(), options, &mut err);
        if rc != 0 { return Err(map_error(&err)); }
        Ok(())
    }
}
```

注册入口（`crates/quercus-engine/src/storage.rs`）：

```rust
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_storage_register_transport(
    vt: *const QuercusStorageTransportVTable,
    userdata: *mut c_void,
) -> i32 {
    // 1) 校验 vt 非空、struct_size/abi_version 兼容
    // 2) 复制 vtable + name/scheme（CString → String）
    // 3) Registry::global().register(Arc::new(ForeignBackend::new(...)))
    // 4) 返回 slot（内部列表下标）；重名 → E_STATE
}
```

`Registry` 只需增加一个按 `slot` 移除的入口（现有 `unregister(name)` 不够；
`registry.rs` 增加 `register_with_slot` / `unregister_slot`），并把
`list()` 的结果扩展为带 `origin`（native/foreign），供 `backend_count/name_at/scheme_at` 继续工作。

### 1.4 仲裁：内置优先，外来按注册序

`Registry::resolve` 现在是"第一个 `can_handle` 命中"（`registry.rs:87-94`）。
加入外来后端后必须防止"宽泛 `can_handle` 抢走 `file://`"：

- **`file` 与 `quercusdb+*` 为保留 scheme**：外来后端不得认领（注册时校验 + 运行时跳过）。
- 解析顺序：先内置（`backends::builtins()` 的 3 个），再外来（注册顺序）。
- 外来之间同 scheme 允许，但注册时返回冲突警告（或 `E_STATE`，二选一，
  建议：允许并在 `list()` 中暴露顺序，文档写明"先注册先赢"）。
- `can_handle` 可能被高频调用（每次 open/save），契约要求 O(1)、无 I/O。

### 1.5 注册 / 生命周期 / 线程 / 内存契约

| 事项 | 规则 |
|---|---|
| 注册时机 | 任意时刻（注册表是 `Mutex<Vec<_>>`）；但**首次 open/save 之后**注册只影响之后的解析，不影响已开始的调用 |
| vtable 内存 | 引擎复制结构体；`name`/`scheme` 复制为 Rust `String`；函数指针要求进程生命周期有效 |
| `userdata` | 调用方所有；必须活到 `unregister` **返回之后**。要释放它，先 `unregister` 再 free |
| 注销 | 从注册表移除；已解析出的 `Arc<ForeignBackend>` 仍可完成在途调用。**契约：`unregister` 会等待在途调用结束**（用读写锁：调用持读锁，注销取写锁），因此可能阻塞到一次 I/O 完成 |
| 线程 | `load/save/can_handle` 可能来自：调用者线程（`quercus_storage_open/save`）、撤销观察者线程（编辑线程，写穿 **同步**）、`quercus-storage-snapshot` 后台线程（`writethrough.rs:411`）。`userdata` 必须线程安全；引擎对同一后端用 `op_lock` 串行化，后端无需自己防重入 |
| 重入 | 回调内**禁止**再调用 `quercus_storage_*`（会死锁）。可以做网络 I/O、可以开自己的线程 |
| 内存 | 返回给引擎的字节必须用 `free_bytes` 释放（跨 DLL 分配器不互 free，Windows 尤其）；引擎传给后端的字节只在调用期有效，后端要留存必须自己复制 |
| 错误 | `code` 用 `QUERCUSSTORAGE_E_*`；引擎把 `message` 复制进错误上下文。**前置改动**：`quercus-storage` 的 `Error` 目前把 `Failed(String)` 压成 `E_FAILED`（`error.rs:80-91`），需新增 `Error::Foreign { code, message }` 并让 `code()` 透传，否则后端的 `E_NOT_FOUND` 会被吞成 `E_FAILED` |
| panic | 纯 C 实现不会有 Rust panic；若 SDK 用 Rust 闭包适配，trampoline 必须 `catch_unwind`（参考 `crates/quercus-engine/src/editor/notify.rs:92`） |

### 1.6 与内置后端的关系

- `.ove` / `.otio` / `quercusdb+*` 仍是编译期内置，行为不变。
- **v1 只允许引擎 XML**：`out_format` 保留为 0（AUTO）或 1（OVE_XML），
  其他值返回 `E_FORMAT`。原因：OTIO 的"JSON → 项目图"解析目前留在内置
  `.otio` 后端里（`backends/otio.rs`），没有可复用的 bytes→Project 入口；
  把它抽出来是 P1 的事。**不要**在 v1 承诺 `s3://bucket/proj.otio` 可用。
- 版本探测从后端移到引擎（现在在 `ove_xml.rs:58-79` 的 `probe_version`），
  外来后端不需要理解 `TOO_OLD/TOO_NEW`。`StorageBackend::load` 的 `version_info`
  由引擎 codec 填写。

### 1.7 兼容性与回滚

- 不注册任何外来后端时，行为与今天完全一致（内置优先）。
- 新增符号是纯增量；旧客户端不受影响。
- 若需要回滚，移除注册符号与 `ext/storage_transport.rs` 即可，内置路径未改。

### 1.8 测试

- **内存后端**：测试里用 `extern "C"` 函数实现一个 `HashMap<String, Vec<u8>>` 后端，
  注册 → `quercus_storage_save/open` roundtrip → `unregister`。
- **free 计数**：`free_bytes` 计数器，验证错误路径也恰好调用一次。
- **错误透传**：后端返回 `E_NOT_FOUND` + message，验证 C API 返回码与 SDK 异常。
- **并发**：从快照线程与调用者线程同时 save，验证 `op_lock` 串行化。
- **注销阻塞**：`load` 里 sleep，`unregister` 必须等它返回。
- **保留 scheme**：外来后端声明 `file` scheme 必须被拒绝。
- **head 漂移**：`python3 tooling/gen_c_header.py --check`。

---

## 2. P0-A：项目字节原语

### 2.1 现状

| 能力 | 实现 | 证据 |
|---|---|---|
| Project → 文件 | `serializer::save` + `std::fs::write` | `engine/src/node.rs:2444` |
| 文件 → Project | `std::fs::read_to_string` + `serializer::load` | `engine/src/node.rs:2488` |
| 内部已有字节接口 | `serializer::save(&Project) -> Result<String>` / `load(&str)` | `crates/quercus-node/src/serializer.rs:277,495` |
| 版本探测 | 在 `.ove` 后端里私有 | `crates/quercus-storage/src/backends/ove_xml.rs:58-79` |

### 2.2 目标 API

沿用仓库的 snprintf 风格（`copy_str_out`，`crates/quercus-engine/src/codec.rs:16`）：

```c
/* 把项目序列化为 UTF-8 项目文档。返回完整字节数（不含结尾 NUL）；
   buf 为空或 cap==0 时只查询大小；ret > cap 表示被截断。
   负数 = 错误码（QUERCUSSTORAGE_E_*）。 */
int32_t quercus_project_save_to_bytes(const void* project,
                                      uint8_t* buf, size_t cap);

/* 从字节构造项目；成功返回拥有引用（refcount 1）的句柄。
   version_info: 0=OK，1=TOO_OLD，2=TOO_NEW，3=UNKNOWN_VERSION。
   TOO_NEW/UNKNOWN 时返回 NULL 但 version_info 有效（对应现有 LoadResult::info_only 语义）。
   error 非 0 表示真正的失败（与 version_info 互斥）。 */
void* quercus_project_load_from_bytes(const uint8_t* bytes, size_t len,
                                      int32_t* version_info,
                                      QuercusStorageError* error);
```

### 2.3 语义细节

- **编码**：字节就是 `serializer::save` 输出的 UTF-8 XML（根 `<project version="1">`）。
  不接受带 BOM 的输入？`load` 现在用 `XmlReader`，BOM 可能失败——测试补一条，明确行为。
- **filename 字段**：`load_from_bytes` 没有路径可写，`Project::filename` 留空；
  调用方可随后 `quercus_project_set_filename`。路径版 `quercus_project_load` 在成功后设置。
- **版本**：`probe_version` 移到 `quercus-node::serializer`，与 `save` 同处；
  顺手修一个现存漏洞——它现在对 `<project>` 根**不校验 version 属性**
  （`ove_xml.rs:67-70`），`version="999"` 也被当作 Current。
- **共享实现**：路径版改为 `read → load_from_bytes` / `save_to_bytes → write`，
  保证内存版与文件版永远同一语义（现在路径版和 ove 后端各写一套 `fs` 调用）。
- **大项目**：两段式（先查长度再分配）避免引擎替调用方分配；
  文档标注 2 GiB 上限（返回类型 `int32_t`）。
- **空项目**：`Project::new()` 也要能序列化并 roundtrip（现在路径版依赖 filename 非空，
  内存版不依赖）。

### 2.4 落点

- 引擎：`crates/quercus-engine/src/node.rs`（紧邻 2444/2488）。
- SDK：`crates/quercus-engine-rs/src/node.rs` 增加 `Project::to_xml_bytes()` /
  `Project::from_xml_bytes(&[u8])`，并在 `extern "C"` 块声明新符号。
- 头文件：`gen_c_header.py --write`。

### 2.5 它解决了谁的问题

1. 调用方把项目存进**自己的容器**（自有工程文件、云同步、剪贴板、diff/审计）。
2. C 侧测试自定义存储后端（不需要临时文件）。
3. 通用化库导出的 import/export：现在 `export_to_file` 固定 `OveXmlBackend`
   （`database/mod.rs:611-623`），改成"引擎出字节 → 目标后端写"。
4. 渲染 worker 的 graph snapshot 现在写临时 XML 文件（`quercus-render/src/worker.rs:346`），
   虽然仍需要文件给子进程，但序列化可统一走字节 API。

### 2.6 不在本步做

- 不开放"自定义项目格式"（codec 不公开）。字节始终是引擎 XML。
- 不改变 `quercus_project_save/load` 的签名与语义。

---

## 3. P0-C：库与写穿走注册表

### 3.1 现状与两个隐藏问题

调用路径（全部跳过注册表）：

```text
quercus_storage_library_* (15 个) ─► writethrough::backend() ─► OnceLock<DatabaseBackend>
editor_open_project_from_library   ─► writethrough::backend().load
editor_create_library              ─► writethrough::backend().save_project
editor_bind_project_to_library     ─► writethrough::bind_project
undo observer (每次成功命令)        ─► write_through ─► backend().save_project
snapshot 线程 (默认 600s)           ─► backend().snapshot
quercus_storage_flush (退出)        ─► backend().save_project + snapshot
```

问题一：**两个 `DatabaseBackend` 实例**。`builtins()` 里 `Arc::new(DatabaseBackend::new())`
（`backends/mod.rs:35`），`writethrough::backend()` 又 `OnceLock(DatabaseBackend::new)`
（`writethrough.rs:118-121`）。各自持有独立 tokio runtime、连接池与 `op_lock`；
SQLite 池上限 1 连接（`database/mod.rs:323-338`）。于是同一个库文件有两个写者：
`quercus_storage_save("quercusdb+sqlite://…")` 走注册表实例，
自动保存走 writethrough 实例，可能并发写、触发 `database is locked` 或后写覆盖前写。
**修法**：`DatabaseBackend::shared()` 静态，`builtins()` 与 `writethrough` 都用它。

问题二：**写穿在编辑线程同步执行**。`note_command` 是撤销栈的 command-success observer，
在调用线程上运行（`quercus-undo/src/global.rs:79-90` 派发，push/group/jump 处 `:139/:183/:243` 调用），
每条成功命令都同步 `save_project`。SQLite 本地尚可；换成 HTTP/S3 后**每次编辑一个网络请求**，
直接卡住 UI。这是 P0-C 必须同时解决的时序问题。

### 3.2 目标：单一解析点

```text
所有库/写穿入口 ─► library_backend(uri) ─► Registry::resolve(uri) ─► StorageBackend（含外来）
                                              └─ 未命中且是 quercusdb+* ─► 内置 DatabaseBackend
```

- `writethrough::backend()` 删除，改为 `fn library_backend(uri: &StorageUri) -> Result<Arc<dyn StorageBackend>>`。
- `storage_enabled()` 不再看 `Storage/Backend` 白名单；改为"`Storage/LibraryUri`（或旧键推导出的 URI）可被解析"。
- 解析失败 = 未配置库（保持"无库则降级为不绑定"的现状），而不是静默写错地方。

### 3.3 能力模型：哪些方法必须上 trait

`StorageBackend`（内部 trait）增加默认返回 `Error::Unsupported` 的库方法（**需新增该变体**，映射到 `E_NO_BACKEND` 或新增 `-100009`；建议新增 `QUERCUSSTORAGE_E_UNSUPPORTED`），`DatabaseBackend` 覆写：

```rust
trait StorageBackend {
    // …现有 5 个方法…

    fn library_list(&self, _uri: &StorageUri) -> Result<Vec<ProjectInfo>> { Err(Error::Unsupported) }
    fn library_delete(&self, _uri: &StorageUri, _uuid: &str) -> Result<()> { Err(Error::Unsupported) }
    fn library_rename(&self, _uri: &StorageUri, _uuid: &str, _name: &str) -> Result<()> { Err(Error::Unsupported) }
    fn library_snapshot(&self, _uri: &StorageUri, _uuid: &str) -> Result<()> { Ok(()) } // 默认无操作
}
```

映射到 C 侧：就是 §1.2 里那 4 个可空函数指针。

**哪些能力不需要 slot**（引擎侧组合即可）：

| 库操作 | 实现方式 |
|---|---|
| `library_create(name)` | 新建空 `Project`（带新 uuid）→ `backend.save(uri?project=新uuid)` |
| `library_duplicate(uuid)` | `backend.load(old)` → 改 uuid → `backend.save(new)` |
| `library_import(file_uri)` | `resolve(file_uri).load` → `backend.save(library)` |
| `library_export(uuid, file_uri)` | `backend.load` → `resolve(file_uri).save` |
| `library_count/uuid_at/name_at/created/modified` | `library_list` 的结果缓存 |
| `duration_ms/track_count/clip_count/footage_count` | 引擎 `load` 后走图统计（现有 `project_stats`，`database/mod.rs:715`）；按 `(uri,uuid,modified_at)` 缓存。外来后端不必实现 |
| `quercus_editor_open_project_from_library` | `library_backend(uri).load(uri?project=uuid)` |
| `quercus_editor_create_library` | 同 `library_create` |

这样外来后端**最少只需实现 `library_list` + `load` + `save`** 就能被完整库流程使用；
`delete/rename` 可选。现有 15 个 `library_*` 导出全部保留，实现换成组合。

### 3.4 写穿时序：必须区分"本地即时"与"远端延迟"

注册时让后端声明写入策略（vtable 里加一个字段，或复用 `options`）：

```c
/* 见 §1.2：0=引擎默认，1=immediate，2=coalesced，3=explicit。
   内置 DatabaseBackend 默认 1；外来后端默认 2。 */
uint32_t write_policy;
```

引擎行为：

- **immediate**（内置 DB 默认）：保持今天"每条命令后写"的语义（崩溃安全性最好）。
- **coalesced**（外来后端默认）：`note_command` 只标脏 + 通知后台；后台线程每
  `Storage/WriteDebounceMs`（默认 500）写一次；`unbind`/`flush`/退出强制写。
  复用现有 snapshot 线程，不新增线程。
- **explicit**：只在 `quercus_storage_flush` 或 `unbind` 写；适合调用方自己管同步。

**语义差异要写进文档**：内置 DB 是"日志/增量"（`save_project` 做 diff，`save_tx` 记 journal），
外来后端是"整文档覆盖"（引擎每次给完整 XML）。因此：
- 外来后端的 `library_snapshot` 可留 NULL（每次 save 已是快照；
  需要版本历史就自己存多版本）。
- 内置 DB 保留 `library_snapshot`，行为不变。
- `Storage/SnapshotIntervalSec` 只对"支持 snapshot 的后端"生效。

### 3.5 URI 约定（写进头文件）

- **库根**：`Storage/LibraryUri`，例如 `s3://bucket/projects`。
- **行**：引擎在库根后追加 `?project=<uuid>`；后端**必须**在 `load/save` 中识别
  `project` 查询参数；`library_list` 收到的是库根（无该参数）。
- 非库场景（`quercus_storage_open/save`、`editor_save`）传入原始 URI，不含 `project`。
- 兼容：旧配置 `Storage/Backend=sqlite|pg` 继续有效，引擎内部推导
  `LibraryUri = quercusdb+sqlite://<abs path>` / `quercusdb+pg://<conn>`（沿用
  `writethrough::library_uri`，`writethrough.rs:392`）。

### 3.6 SDK 修改

- 新增 `storage::register_transport` + `TransportRegistration`（Drop 即 unregister），
  内部 trampoline 照 `codec::set_task_submit_cb`（`engine-rs/src/codec.rs:679-706`），
  但要补上"注销时释放闭包"——现有先例是泄漏（`codec.rs:679` 注释自认）。
- `ProjectLibrary::new` 去掉 `backend_for_uri == "quercusdb"` 硬编码
  （`engine-rs/src/storage.rs:304`），改为调用新增的
  `quercus_storage_library_supported(uri)`（返回 0/1），或直接信任解析结果。
- `ProjectLibrary::open` 去掉"只允许默认库"的检查（`:394`）。

### 3.7 失败与降级

- 库未配置 / 后端未注册 → `bind_project` 返回 false，不绑定（现状）。
- 写穿失败 → 记 `last_error`（`quercus_editor_storage_last_error` 已导出），
  不打断编辑（现状）。
- `library_list` 失败 → 相关查询返回 0/-1（现状），但**不能**回落到内置 DB
  （否则会"看似成功写错库"）。
- 后端声明 `coalesced` 时，崩溃可能丢失最后 <500ms 的编辑；文档写明，调用方可选 immediate。

---

## 4. 端到端时序（外来后端）

```text
注册：  caller ──register_transport(vt, ud)──► Registry[foreign#0]

打开：  quercus_storage_open("s3://b/proj.ove")
          └─ resolve: native(file/ove) 不认领 → foreign#0.can_handle → 1
             └─ foreign#0.load(uri) ──► caller 后端读 S3 ──► (bytes, free_bytes)
                └─ engine: probe/parse XML → ProjectHandle

保存：  quercus_editor_save(session, "s3://b/proj.ove")
          └─ resolve → foreign#0
             └─ engine: Project → XML bytes
                └─ foreign#0.save(uri, bytes) ──► caller 后端写 S3

自动保存（coalesced）：
  edit → undo push → note_command → mark dirty + notify
        background: 500ms 合并 → foreign#0.save(uri?project=uuid, xml)
        unbind/exit: flush_all → 强制 save（+ 可选的 snapshot）
```

---

## 5. 验收清单

**P0-B**
- [ ] 外来内存后端经 C ABI 完成 save→open roundtrip
- [ ] `free_bytes` 在成功/失败/解析失败路径都恰好一次
- [ ] 外来后端用 `file`/`quercusdb+*` scheme 被拒
- [ ] `unregister` 等待在途调用；之后解析不再命中
- [ ] 错误码与 message 透传（含 `E_NOT_FOUND`）
- [ ] 未注册时全部现有测试通过；`.ove/.otio/quercusdb` 行为不变
- [ ] header `--check` 通过

**P0-A**
- [ ] 空项目/大项目 roundtrip；`save_to_bytes` 截断语义正确
- [ ] `TOO_NEW`/`UNKNOWN_VERSION` 时返回 NULL 且 `version_info` 正确
- [ ] 路径版与内存版输出字节完全一致（同一 serializer）
- [ ] 非 UTF-8/非 XML 输入返回 `E_FORMAT` 且不崩溃

**P0-C**
- [ ] 配置 `Storage/LibraryUri=s3://b/projects` 后，`library_create/list/rename/delete` 走外来后端
- [ ] 写穿按 `write_policy` 行为正确；immediate 与今天一致
- [ ] 只有一个 `DatabaseBackend` 实例（修掉双写者）
- [ ] `ProjectLibrary::new` 接受非 `quercusdb` 后端
- [ ] 无库配置时 headless 行为不变（不写用户目录）
- [ ] 退出 `flush_all` 不会因远端阻塞无限挂起（超时/可取消）

---

## 6. 工时与风险

| 项 | 内容 | 估时 |
|---|---|---|
| P0-B | vtable、适配器、注册/注销、仲裁、错误透传、测试、文档 | 4–6 人日 |
| P0-A | 2 个导出 + probe 迁移 + 路径版复用 + SDK/header + 测试 | 2–3 人日 |
| P0-C | trait 能力、写穿策略、URI 约定、15 个导出改造、双实例修复、SDK | 8–12 人日 |
| 合计 | 一人串行约 3 周；P0-A/B 并行可压到 2 周 | |

主要风险：

1. **注销阻塞**：`unregister` 等 I/O 可能很久。缓解：文档 + 可选的 `unregister_forget`
   （立即摘除，调用方保证 userdata 活到进程末）。
2. **远端写穿卡退出**：`flush_all` join 快照线程。缓解：给 `save` 加超时；
   `flush` 记录失败后强制返回。
3. **格式选择歧义**：无扩展名的远端 URI 默认 XML；需要 OTIO 时靠 `out_format` 或
   后缀。文档写清优先级。
4. **ABI 冻结**：`struct_size` 只能加字段；`library_*` 一旦发布不能改签名。
   建议 v1 就把 `write_policy`、`format` 放进去。
5. **错误码透传**需要改 `Error` 枚举（新增 `Foreign`），会影响现有 `code()` 测试
   （`error.rs:96-130`），要同步更新。
6. **双实例修复**会改变 `DatabaseBackend` 的构造方式；SQLite 并发写测试要覆盖。

---

## 7. 与评审报告的关系

- 评审报告给出"为什么、优先级、ABI 规范"；本文给出"怎么落"。
- 两份文档里对 P0 顺序的表述以本文 §0 为准：
  **P0-A 与 P0-B 可并行，P0-C 依赖 P0-B。**

---

## 8. 实施状态（2026-10-31）

**P0-A、P0-B、P0-C 均已落地。**

### 8.1 已交付

| 项 | 内容 |
|---|---|
| P0-A 探测上移 | `quercus_node::serializer::{VersionProbe, probe_version}`；`.ove` 后端改为调用它，版本信息码结果不变 |
| P0-A 字节 ABI | `quercus_project_save_to_bytes(project, buf, cap) -> i32`；`quercus_project_load_from_bytes(bytes, len, version_info, error) -> *mut ProjectHandle` |
| P0-A 复用 | `quercus_project_save/load` 改为走字节原语；SDK 新增 `Project::to_xml_bytes` / `Project::from_xml_bytes` |
| P0-B 适配器 | `quercus_engine::ext::storage_transport`：`QuercusStorageError`、`QuercusStorageTransportVTable`、`ForeignBackend`（gate + op lock + `free_bytes` guard）、`is_reserved_scheme` |
| P0-B 注册 | `quercus_storage_register_transport` / `quercus_storage_unregister_transport`，进程级 slot 表 + `Registry` 仲裁 |
| P0-B SDK | `TransportBackend` trait、`register_transport`、`TransportRegistration`、全部 trampoline `catch_unwind` |
| P0-C 能力层 | `StorageBackend` 新增 `LibraryProjectInfo` / `LibraryProjectStats` / `supports_library` / `library_list/delete/rename/stats/snapshot` / `write_policy`；新增 `Error::Unsupported`（`-100009`） |
| P0-C 数据库 | `DatabaseBackend::shared()`（进程内唯一实例，`builtins()` 复用）；trait 库操作委托既有 SQL 实现 |
| P0-C 外来库 | vtable 追加 `QuercusStorageProjectInfo` + `library_list/delete/rename/snapshot`；旧前缀 `struct_size` 仍可注册（库槽为 NULL）；两阶段 list |
| P0-C 写穿 | `writethrough::library_backend()`（registry 解析）取代 `backend()`；`LibraryUri` 配置；策略：0/1 立即、2 合并（`WriteDebounceMs` 默认 500）、3 仅显式 flush；编辑线程不再做远端 I/O |
| P0-C 引擎库层 | 15 个 `library_*` 全部走 trait；create/duplicate/import/export 改为通用组合；统计优先 `library_stats`，`Unsupported` 回退 load+图推导；新增 `quercus_storage_library_supported`、`quercus_editor_open_project_from_library_uri` |
| P0-C SDK | `ProjectLibrary::new` 不再按 `"quercusdb"` 名字拒绝；`library_supported`；显式库打开；`TransportBackend` 可选库能力（POD 镜像 + 4 个 trampoline） |
| 深拷贝修复 | `clone_entry` 对拒绝 `duplicate` 的节点（图端点等）按 `type_id` 从 factory 重建，不再落成空 `EmptyBehavior`；`SequenceBehavior::duplicate` 保留 track_lists/参数/播放头（否则副本丢轨道） |
| 头文件 | `gen_c_header.py` 修复函数指针返回类型、`->` 的 `>` 破坏字段切分、跨行空白；header 重新生成（当前 54 structs / 14 enums / 715 functions，含 P1 扩展点） |

### 8.2 与草案的偏差

- **v1 只接受引擎 XML**：`out_format` 只接受 0（AUTO）/1（OVE_XML），其他值 `E_FORMAT`。OTIO 解析仍在内置 `.otio` 后端（P1）。
- **写穿策略已生效**：内置 `DatabaseBackend` 立即写；外来传输 `write_policy=0` 映射为合并写（vtable 里显式 1..=3 原样使用）。SDK 默认传 0，可通过 `TransportBackend::write_policy` 覆盖。
- **通用 duplicate 无 journal 历史**：deep copy + fresh uuid + `"<name> (copy)"`；数据库后端的行级历史复制仍保留在其自身路径（当前未使用）。已写入函数文档。
- **显式库打开只在“就是默认库”时自动绑定写穿**：非默认库会话保持未绑定，避免把编辑写进另一个库。
- **vtable 前缀兼容**：新增 4 个槽位在 `save` 之后；`struct_size` 达到旧前缀即接受，旧调用者库能力为 NULL。SDK 只在 `supports_library()` 为真时填 4 个函数指针。
- **SDK 库行字符串长度**：uuid ≥ 40 或 name ≥ 256 字节直接报错（不静默截断）。
- **`free_bytes` 必须容忍 NULL**：错误且无 buffer 的路径也会调用一次。

### 8.3 验证

| 命令 | 结果 |
|---|---|
| `cargo test -p quercus-node --lib` | 684 passed |
| `cargo test -p quercus-storage` | 全绿（22 lib + 13 pg + 18 database + 6 otio + 11 storage + 10 writethrough；transport 测试随适配器迁入 `engine::ext`） |
| `cargo test -p quercus-engine --lib -- --test-threads=1` | 312 passed（P1 + multicam 后） |
| `cargo test -p quercus-engine-rs --lib`（`OCIO_SOURCE_DIR`=可写 0.3 源码副本） | 121 passed；0 failed（含 color 与新增跨 ABI transport-library 测试） |
| `cargo check -p quercus-engine --tests` | 通过（仅既有 warning） |
| `python3 tooling/gen_c_header.py --check` | 通过 |
| `gcc -std=c11` / `g++ -std=c++17`（含新 vtable 槽位/库 POD/注册调用的 smoke 文件） | 通过 |
| `git diff --check` | 通过 |

> **环境说明（重要）**：本沙箱把 Cargo registry 挂成只读，`ocio-sys` bundled 构建的 ExternalProject `PATCH_COMMAND`（把 `Buildpystring.cmake` 复制成 pystring 源码目录里的 `CMakeLists.txt`）因此失败，`ocio-sys 0.3` 报 `pystring_install-patch Error 1`。把 `vendor/OpenColorIO` 复制到可写目录并设置 `OCIO_SOURCE_DIR` 后，**0.3 完整构建成功**，SDK 121 个测试全绿（含 color）。`ocio-sys 0.2.1` 另有独立问题：其 vendored yaml-cpp 缺 `#include <cstdint>`，GCC 16 下编译失败；临时补上该 include 后 0.2.1 也能构建。结论与测试细节见本轮回复。
>
> 另：11 小时间隔内仓库已有用户侧改动（`ocio-rs` 0.3、`quercus-engine`/`quercus-render` 依赖版本、`Cargo.lock` 索引状态、`.cache/ffmpeg` 构建产物）；本报告未回滚这些改动。
