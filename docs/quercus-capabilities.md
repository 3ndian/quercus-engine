# Quercus 对外能力总览

> 用途：对外介绍 / 宣传材料的能力底稿。只列 **公开 SDK（`quercus-engine-rs`）与稳定 C ABI**
> 暴露的能力，不包含内部实现 crate。
>
> 版本基线：crates.io `quercus-engine-rs 0.5.7`、`quercus-engine 0.5.3`、内部 crate 0.5.2；
> 主线 HEAD 已合并、但可能尚未发布的能力单独标注「主线」。

## 1. Quercus 是什么

Quercus 是从 Oak Video Editor 中抽出的**无头非线性视频剪辑引擎**：工程/时间线模型、
解码与编码、音频、渲染、特效/OpenFX、调色、导出、代理、多机位、OpenTimelineIO，
以及一个可供前台应用、CLI 或渲染 worker 使用的**稳定 C ABI** 和**安全 Rust SDK**。
Oak 是参考实现，其他前端可以用同一套引擎构建自己的编辑器。

## 2. 关键数字

| 指标 | 数值 |
|---|---:|
| 公开 Rust SDK 模块 | 26 |
| C ABI 导出符号（`quercus_engine.h`） | 718 |
| 语义编辑命令（`EditCommand`） | 74 |
| 内置效果节点 | 68 |
| 转场类型 | 4（Cross Dissolve / Fade / Wipe / Slide） |
| 导出容器格式 / 编码器 / 像素格式 | 15 / 19 / 5 |
| 渲染后端 / GPU 后端 | 4 / 4 |
| 系统字体家族（运行时） | 341 |
| 轨道类型 | 3（Video / Audio / Subtitle） |
| 预览器 | 2（Source / Program） |

## 3. 能力总表

| 领域 | 能力 | 对外接口（SDK / C ABI） | 说明 |
|---|---|---|---|
| 工程 | 新建 / 打开 / 保存工程 | `EditorSession::{new,open,save,from_project,close}` | `.ove` 或存储 URI |
| 工程 | 工程库（Library） | `create_in_library`、`open_project_from_library(_uri)`、`storage::library_projects/delete/rename`、`ProjectLibrary` | 需要后端支持 |
| 工程 | 可注册存储后端 | `storage::register_transport`、`backends`、`backend_for_uri`、`storage::open/save` | 自定义 URI / 传输 |
| 工程 | 工程元数据与设置 | `Project::{name,filename,pretty_filename,uuid,modified,setting,set_setting,cache_location,set_cache_location}` | 键值设置随工程保存 |
| 工程 | 快照 DTO | `ProjectSnapshot`、`Entry/Sequence/Track/Clip/Footage/Marker/WorkAreaSnapshot` | 只读、可缓存/跨线程 |
| 工程 | 标记与工作区 | `EditorSession::{markers,workarea_state}` + `MarkerAdd/Remove`、`WorkareaSet` | 工作区可驱动导出范围 |
| 工程 | 撤销 / 重做 | `node::{undo,redo,can_undo,can_redo,count,index,jump,group_begin/end/abort,add_undo_observer}` | 进程级栈，支持分组 |
| 工程 | 节点图快照与连接校验 | `EditorSession::graph_snapshot`、`GraphSnapshot/Node/Port/Edge`、`GraphSnapshot::can_connect` | 完整图视图、端口/边 |
| 工程 | 参数与关键帧查询 | `EditorSession::parameters`、`Project::value_at_time`、`ParameterSnapshot/Parameter/Keyframe` | 静态值 + 关键帧 |
| 工程 | OpenTimelineIO / FCPXML | `otio::{Document,Timelines}::{from_json,to_json,from_fcpxml,to_fcpxml}`、`task::{save_otio,load_otio}` | 交换格式 |
| 工程 | 配置与路径工具 | `state::ConfigStore`、`config_*`、`utils::{default_disk_cache_path,configuration_location,application_path,temp_file_path,auto_recovery_root}` | 可替换 `ConfigSource` |
| 会话 | 会话生命周期 | `EditorSession`（Clone/Drop）、`close`、`storage_bound`、`storage_last_error` | 同一进程可多会话 |
| 会话 | 序列管理 | `create_sequence`、`current_sequence`、`set_current_sequence`、`CreateSequence/SetSequenceParameters/SetSequencePlayhead` | 帧率/分辨率/隔行 |
| 会话 | 编辑通知 | `EditorSession::subscribe_edit` | 每条命令回调 |
| 剪辑 | 74 个语义编辑命令 | `EditorSession::execute(EditCommand) -> EditResult` | 原子、可撤销 |
| 剪辑 | 轨道 | `AddTrack/RemoveTrack/SetTrackHeight/SetTrackMuted/SetTrackLocked` | 视频/音频/字幕轨 |
| 剪辑 | 素材放置 | `PlaceFootageClip`、`PlaceFootageClipsLinked`、`PlaceTextClip`、`PlaceGeneratorClip`、`PlaceNestedSequenceClip`、`CreateAdjustmentLayer` | 含 A/V 链接 |
| 剪辑 | 片段编辑 | `SplitClip(s)`、`TrimClip`、`RippleTrimClip`、`RollEdit`、`SlideClip`、`SlipClip`、`MoveClip(ToTrack/WithLinks)`、`DeleteClip/Area`、`RollClipEdgeTo` | 帧级时间戳 |
| 剪辑 | 片段属性 | `SetBlocksEnabled`、`SetClipsSpeed`、`RenameBlocks`、`DuplicateClips`、`CopyClips`、`PasteClips(Insert)`、`NestClips` | 含剪贴板/嵌套 |
| 剪辑 | 链接 | `SetClipsLinked`、`SplitClipsPreservingLinks` | A/V 同步编辑 |
| 剪辑 | 转场 | `AddTransitionAtSeam/AtEdge`、`AddDefaultTransition`、`SetTransitionLength` | 4 种类型，引擎节点已支持 |
| 剪辑 | 片段增益（主线） | `EditorSession::set_clip_gain` | 0..=4，一次 undo |
| 剪辑 | SRT 字幕工作流（主线） | `EditorSession::{import_subtitles,export_subtitles}`、`set_subtitle_style` | 以 Text3 片段实现；样式支持字体/字号/颜色/描边/对齐 |
| 媒体 | 导入 / 替换 / 重探 | `ImportFootage`、`ReplaceFootage`、`ReprobeUnprobedFootage`、`CreateSequenceFromFootage` | 支持视频/音频/图片/字幕流探测 |
| 媒体 | 工程箱与文件夹 | `CreateFolder`、`DeleteEntry`、`EnsureSequencesMounted`、`Project::folder_*` | 层级浏览 |
| 媒体 | 代理（Proxy） | `EditorSession::{generate_proxy,complete_proxy,proxy_status,proxy_path,delete_proxy,set_proxy_enabled}`、`SetFootageProxyParams/ClearFootageProxyParams` | 4K 剪辑加速 |
| 媒体 | 多机位 | `SetMulticamEnabled/SwitchMulticam/CreateMulticamSequence`、`EditorSession::multicam_*` | 角度/源/向导 |
| 媒体 | 自定义媒体 I/O | `mediaio::register_media_io` | 替换解码/探测实现 |
| 媒体 | 缩略图 / 波形 | `task::render_thumbnail`、`extract_waveform`、`Thumbnail`、`Waveform` | 异步任务 |
| 效果 | 68 个内置效果节点 | `effect::{factory_entries,factory_name,dynamic_factory_entries}` | 13 调色 / 11 变形 / 12 滤镜 / 13 生成器 / 3 键控 / 7 数学 / 转场 / 时间 / Group |
| 效果 | 效果链 | `Project::{effect_chain,effect_chain_insert,effect_chain_remove,effect_chain_move,effect_set_enabled,effect_is_enabled,effect_input_id,plugin_instance_handle}` | closest-to-source 顺序 |
| 效果 | 参数 / 关键帧 | `SetNodeStandardValue`、`UpsertNodeKeyframe`、`RemoveNodeKeyframe`、`SetNodeValueAtTime`、`SetNodeTextValue`、`PushNodeButton` | 支持 Hold/Linear/Bezier |
| 调色 | 一级/风格化节点 | `Color Correct`、`Grade`、`Three-Way Color`、`White Balance`、`Saturation`、`Gamma`、`Color Matrix`、`Invert`、`Clamp` | 饱和度/对比/增益/黑点白点/色温/三通道 |
| 调色 | OCIO / LUT | `OCIO LUT`、`OCIO Color Grading (Linear/Log)`、`Display Transform`；`color::{ColorProcessor,ColorTransform}`、`state::color_*` | 源/目标色彩空间、LUT、ICC |
| 键控/合成 | 键控节点 | `Chroma Key`（上下容差、garbage/core matte、阴影/高光）、`Color Difference Key`、`Despill`、`Mask`、`KeyMix`、`Premultiply/Unpremultiply`、`Merge`、`Group` | 基础合成 |
| 变换/变形 | 几何节点 | `Transform`、`Position`、`Corner Pin`、`Crop`、`Flip`、`Mirror`、`Wave`、`Tile`、`Swirl`、`Ripple`、`Mask`（feather/invert） | 支持关键帧 |
| 生成器/文字 | 生成器与文字 | `Solid`、`Ramp`、`Checkerboard`、`Color Bars`、`Noise`、`Text`/`Text3`/`Text (Legacy)`、`Shape`、`Polygon`、`Stroke`、`Drop Shadow` | 文字支持字体/描边/发光/颜色/对齐 |
| 文字 | 字体/文字引擎 | `render::{textengine_install,textengine_add_font_data,textengine_add_font_dir,font_families}` | 运行时 341 个字体家族 |
| 插件 | OpenFX host | `ofx::{scan,scan_path,plugins,plugin_at,register_plugin_nodes,Instance,set_progress_handler,install_render_executor}` | 扫描/注册/实例/进度 |
| 插件 | OpenFX GPU 零拷贝 | `gl_bridge::{import_memory_texture,delete_memory_texture}`、`GpuContext::{create_exportable_texture,export_gl_memory}` | Linux/Vulkan + EGL：`OPAQUE_FD` + `GL_EXT_memory_object_fd`，输入/输出同一显存，无 `glReadPixels`/CPU frame；Windows 为**实验性、默认关闭**（`QUERCUS_OFX_GPU_ZEROCOPY=1` 启用）；macOS 待接 |
| 插件 | 动态插件注册 | `plugin::{PluginRegistry,plugin_count,invoke_command}` | 命令式扩展 |
| 音频 | 音频参数/格式 | `AudioParams`、`SampleFormat`、`audio::channel_count`、`frames_to_rational/rational_to_samples` | 采样率/声道布局/格式 |
| 音频 | 设备输出 | `audio::{create_instance,destroy_instance,output_device_names,set_output_device,output_levels,seconds,push_to_output,...}` | 电平/时钟/缓冲/underflow |
| 音频 | 音频渲染 | `task::render_audio`、`AudioBlock`、`LevelStats` | 48k/立体声等 |
| 音频 | 波形分析 | `extract_waveform`、`Waveform::{peaks,point}`、`audio_analysis::{place_by_source_time,place_by_waveform_offset,estimate_envelope_offset,estimate_stretch_offset}` | 对齐/包络/拉伸估计 |
| 音频 | 音量/声像 | `Volume`、`Pan` 节点 | 混音效果链为主线推进项 |
| 音频 | 片段增益（主线） | `EditorSession::set_clip_gain` | 0..=4 |
| 音频 | 音频导出 | `EncodingParams`（AAC/PCM/MP3/FLAC/Opus/Vorbis 等） | 随容器而定 |
| 预览 | 实时预览 | `PreviewSession::{source,program}`、`seek/play/pause`、`invalidate`、`start/stop_audio_output`、`audio_output_levels` | Source / Program 两种 |
| 预览 | 预览帧/音频请求 | `request_frame`、`request_audio`、`PreviewFrameRequest/PreviewAudioRequest`、`generation` | 异步、可丢弃过期结果 |
| 渲染 | 单帧离线渲染 | `render::render_frame_f32` | F32 RGBA，工作色彩空间 |
| 渲染 | 渲染任务 | `task::{render_video,render_playback_video,render_track_video,render_audio,render_thumbnail}` | 输出 owned DTO |
| 渲染 | 渲染管理器 | `manager_init/is_initialized/shutdown/poll`、`manager_set_workspace_size`、`manager_set_graph_snapshot` | Default/Threads/Pipeline/Processes |
| 渲染 | GPU | `GpuBackend{Auto,Metal,Vulkan,Gl}`、`gpu_kind/gpu_is_adopted/gpu_host_installed/gpu_mark_host` | Metal/Vulkan/GL/CPU |
| 渲染 | 磁盘缓存 | `disk_cache_{path,size,clear,max_bytes,set_max_bytes,set_path,prune}` | 渲染缓存管理 |
| 渲染 | 帧/音频 DTO | `VideoFrame`（F32 RGBA / BGRA8）、`AudioBlock`、`Thumbnail` | 边界拷贝，无借用 |
| 导出 | 导出参数 | `task::EncodingParams`：格式/编码/码率/宽高/时间基/像素格式/音频/字幕/自定义范围/色彩覆盖 | 15 种容器 |
| 导出 | 任务系统 | `Task::{state,progress,title,error,wait,cancel,cancellation}`、`take_{project,video,audio,thumbnail,waveform}` | 可取消、可轮询 |
| 导出 | 交换格式导出 | `task::{save_otio,load_otio}`、`export_subtitles`（主线） | OTIO/FCPXML/SRT |
| 导出 | 代理任务并发 | `task::proxy_max_concurrent` | 由宿主控制队列 |
| 平台 | 稳定 C ABI | `quercus_engine.h`（1452 行，718 个 `quercus_*` 符号） | C/C++ 可直接调用 |
| 平台 | Rust SDK | 26 个模块、edition 2024、Rust 1.85+ | safe wrapper |
| 平台 | 渲染 worker | `worker::{parse_backend,run,ofx_host_main,main}` | 进程隔离渲染 |
| 平台 | 依赖与许可 | 静态 FFmpeg 8.1、OpenColorIO 2.x；GPL-3.0-or-later | 与 Oak 一致 |

## 4. 主线新增（可能尚未发布）

- **SRT 字幕工作流**：`import_subtitles(path, track_index)` 把 SRT 导入为当前序列/轨道的
  Text3 片段；`export_subtitles(path)` 把当前序列的 Text 片段导出为 SRT；
  `set_subtitle_style` 支持字体、字号、填充色、描边色/宽度、对齐。
  说明：目前基于 Text 片段实现；`margin_h/margin_v/line_spacing` 接口保留但 text3 暂未使用。
- **片段增益**：`set_clip_gain`（0..=4，1 = 原始），一条 undo。
- **导出链路修复**：视频内嵌音轨、静音轨、关键帧、短视频补帧等已在主线修复。

## 5. 典型接入（Rust SDK）

```rust
use quercus_engine_rs::editor::{EditorSession, TrackKind};
use quercus_engine_rs::edit::EditCommand;
use quercus_engine_rs::render::{self, RenderBackend};
use quercus_engine_rs::task;

render::manager_init(RenderBackend::Threads);

let session = EditorSession::new()?;
let sequence = session.create_sequence("Demo")?;
let footage = session.project().import_footage("clip.mp4").unwrap();

session.execute(EditCommand::PlaceFootageClip {
    footage,
    kind: TrackKind::Video,
    track_index: 0,
    in_ts: 0,
    out_ts: 125,
    media_in_ts: 0,
})?;

let params = task::EncodingParams { /* 格式/编码/尺寸/时间基 */ ..Default::default() };
let export = task::export(&session.project(), sequence, "out.mp4", &params).unwrap();
export.wait();
```

## 6. 适合的场景

- 自定义 NLE 前端（Rust 或 C/C++，通过 SDK/C ABI）。
- 无头批处理：转码、套 LUT、加字幕、批量导出。
- 渲染农场 / worker 进程（进程隔离渲染）。
- AI Agent 工具层：把剪辑能力封装成 MCP/HTTP/CLI（例如 `example-mcp`）。
- 素材分析/代理生成/多机位同步等流水线。

## 7. 当前边界（写介绍时建议保留的诚实说明）

- 视频**示波器/直方图/矢量图**尚未提供（调色可做，量化监看需外部工具）。
- **跟踪/稳定/降噪/粒子/3D** 暂无内置节点；风格化/高级滤镜可通过 OFX 扩展。
- `Merge` 目前是 alpha-over，没有 multiply/screen 等混合模式。
- 调色缺 **Curves / HSL 二级校色 / qualifier**；一级调色与 OCIO/LUT 完整。
- 字幕目前以 **Text3 片段**实现，不是独立字幕块；样式部分字段受限。
- 音频效果链/混音自动化仍在主线推进；已有 per-clip gain、Volume/Pan 节点与音频渲染。
- `render_frame_f32` 返回工作色彩空间像素，调用方需做显示变换（`utils::working_to_display_target`）。
- 撤销栈是进程级全局的。

## 8. 版本与许可

- 发布版：`quercus-engine-rs 0.5.7`（crates.io）、`quercus-engine 0.5.3`。
- 主线 HEAD：字幕/音频增益/导出修复等，发布前请以对应版本的 API 文档为准。
- 许可：GPL-3.0-or-later，与 Oak Video Editor 一致。
