# 多机位（Multicam）

> 状态：创建、启用/停用、切换（含 split 与 AFV 音频）、状态/角度/网格查询、
> 逐角度渲染、序列化均已实现。本文记录数据模型、SDK/C ABI 用法和边界。

## 数据模型

```
source sequence（角度序列）
  ├─ track list (video) ─ track[0] ─ angle 0 clips
  │                       track[1] ─ angle 1 clips
  └─ track list (audio) ─ track[0] ─ angle 0 audio
                          track[1] ─ angle 1 audio
        ▲
        │ sequence_in
   MultiCamNode (current_in = 当前角度, sources_in = 可选显式源)
        │ tex_in
   host clip（目标序列上的多机位片段）
```

- 角度由 **源序列的 track list** 提供：track 顺序就是角度顺序；
  `sequence_type_in` 选择 video/audio/subtitle（0/1/2）。
- `MultiCamNode` 的 `current_in` 是当前角度；`sources_in` 是显式源数组
  （wizard 会填充，便于单独渲染/显示）。
- host clip 的 `tex_in` 接到 multicam 节点；切换角度只改 `current_in`，
  不改变 host clip 的剪辑位置。

## 创建

```rust
// host: 目标序列；angles: 各机位 footage；offsets: 秒；name: 源序列名
let result = session.execute(EditCommand::CreateMulticamSequence {
    host_sequence,
    angles: vec![cam_a, cam_b, cam_c],
    offsets: vec![0.0, 0.5, 1.25],
    name: "Multi-Cam".into(),
})?;
// result.primary = 源序列；result.ids[0] = host clip
```

- 源序列放在项目根目录；host 序列不变。
- 每个角度一 条视频 track；有音频的角度各一条音频 track。
- host 上没有 clip 时，会在 playhead 放一个角度 0 的 clip 作为 host；
  有 clip 时直接改造该 clip（playhead 覆盖的/第一个）。
- 音频：当前源角度（初始 0）的音频会作为 AFV 音频放到 host 音频轨，
  与 host 视频 clip 链接；切换角度时联动换源。
- offsets/durations 按 **host 序列时基** 换算为精确 `Rational`（不再固定 25 fps）。

## 启用/停用（已有序列上的 clip）

```rust
session.execute(EditCommand::SetMulticamEnabled {
    clips: vec![nested_sequence_clip],
    enabled: true,
})?;
```

- 仅对“clip 的纹理链里有 sequence”的 clip 生效（C++ 语义）。
- 启用 = 在 sequence 与 clip 之间插入 `MultiCamNode`，把 sequence 原来
  喂给 clip 的输入改接 multicam，并连接 `sequence_in`。
- 停用 = 反向绕过并移除 multicam 节点。
- 一次命令覆盖多个 clip，一个 undo 条目；已启用的 clip 会被跳过
  （幂等，不会嵌套第二个 multicam）。

## 切换角度

```rust
session.execute(EditCommand::SwitchMulticam {
    clip: host_clip,
    source: 2,
    split_clip: true,
    playhead: Rational::new(1, 2),
})?;
```

- `source` 越界/等于当前角度：无操作。
- `split_clip && playhead` 严格在 clip 内：先做保留链接的 split，
  后半段切到新角度；两半各自拥有独立的 multicam 副本。
- 同时切换 clip 链接的 AFV 音频（若有）。
- 整个操作一个 undo 条目。

## 查询与 UI 数据

SDK：

```rust
let state = session.multicam_state(host_clip)?;      // sequence/multicam/source_count/current
let sources = session.multicam_sources(state.multicam);
for source in &sources {
    // source.index / source.track / source.name / source.current
}
let (rows, cols) = EditorSession::multicam_grid(state.source_count).unwrap();
let hit = EditorSession::multicam_source_at(state.source_count, x_norm, y_norm);
```

C ABI（`quercus_engine.h`）：

```c
bool quercus_editor_multicam_state(const void* session, uint64_t clip,
                                   QuercusEditorMulticamState* out);
int32_t quercus_editor_multicam_source_count(const void* session, uint64_t multicam);
int32_t quercus_editor_multicam_current_source(const void* session, uint64_t multicam);
int32_t quercus_editor_multicam_source_name(const void* session, uint64_t multicam,
                                            int32_t source, uint8_t* buf, size_t len);
uint64_t quercus_editor_multicam_source_track(const void* session, uint64_t multicam,
                                              int32_t source);
bool quercus_editor_multicam_grid(int32_t source_count, int32_t* rows, int32_t* cols);
int32_t quercus_editor_multicam_source_at(int32_t source_count, double x, double y);
```

- `source_name` 返回源序列 track 的显示名（`Video Track N` / `Audio Track N`），
  不是 footage 文件名。
- `source_at` 与 C++ `MulticamDisplay::DisplayClicked` 一致：
  `multi = max(rows, cols)`、`col/row = floor(x/y * multi)`；空白格返回 -1。
- 网格布局：从 1x1 开始增长较小边，直到 `rows*cols >= source_count`。

## 渲染

- **播放/导出**：`preview_video` 的 montage 会识别 host clip 的 multicam，
  取当前角度在源序列对应 track 上的覆盖 clip，按 host 时间换算 media_in；
  序列渲染和 worker 渲染都可用。
- **多角度画面**：对每个角度调用
  `task::render_track_video(project, source_sequence, angle_track, time, cell_size, format)`，
  由宿主按 `rows/cols` 拼成监控墙；`multicam_source_at` 可直接把点击坐标映射
  到角度索引。引擎不强制内置网格合成（不同 UI 的布局/letterbox 策略不同）。

## 已知边界

- `CreateMulticamSequence` 目前在内部直接构图；`folder_add_child` 之外的
  节点/连线不是单个 undo 条目（后续可改为组合命令）。
- 网格画面由宿主按 `render_track_video` 合成；引擎不提供一次性合成 API。
- 直接对启用的 multicam 节点做**裸 node-graph 遍历**时，sequence 角度的
  动态解析依赖 montage 路径；普通播放/导出走该路径，不依赖节点自身解析。
- `source_name` 用 track 名（与 C++ 一致），不是机位/素材自定义名。

## 验证

| 检查 | 结果 |
|---|---|
| `cargo test -p quercus-engine --lib -- --test-threads=1` | 312 passed |
| `cargo test -p quercus-engine-rs --lib` | 125 passed（含 5 个 multicam 测试） |
| `cargo test -p quercus-timeline --lib multicam` | 10 passed |
| `cargo test -p quercus-engine --lib editor_commands::tests::multicam` | 2 passed（时基 + 幂等） |
| `gen_c_header.py --check` + C11/C++17 smoke | 通过（54 structs / 14 enums / 715 functions） |
