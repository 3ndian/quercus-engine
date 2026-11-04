# 引擎侧特效/后期能力盘点

说明：参考片的镜头分析、节奏判断是模型的职责（`preview_frame` 已能按需给帧），
所以本文不把“没有镜头检测工具”算作引擎缺口。这里只评估引擎作为 NLE 的
**效果与后期能力**，不重复列出正在修复的导出 bug，也不讨论 MCP 工具暴露。

## 1. 内置效果清单（68 个）

| 类别 | 数量 | 节点 |
|---|---:|---|
| Color | 13 | Display Transform、OCIO Color Grading (Linear/Log)、White Balance、OCIO LUT、Three-Way Color、Color Matrix、Color Correct、Gamma、Saturation、Invert、Clamp、Grade |
| Distort | 11 | Transform、Crop、Flip、Corner Pin、Mask、Wave、Tile、Swirl、Ripple、Position、Mirror |
| Filter | 12 | Volume、Pan、Blur、Stroke、Mosaic、Opacity、Drop Shadow、Dilate、Edge Detect、Erode、Directional Blur、Sharpen |
| Generator | 13 | Polygon、Orthographic Matrix、Solid、Text (Legacy ×2)、Text、Value、Shape、Noise、Time Format、Checkerboard、Color Bars、Ramp |
| Keying | 3 | Color Difference Key、Despill、Chroma Key |
| Math | 7 | Math、Trigonometry、Merge、Dissolve、KeyMix、Premultiply、Unpremultiply |
| Timeline/Transition | 3 | Transition、Transition FX、Multi-Cam |
| Time/Group | 4 | Time、Time Remap、Group、Time Offset |
| Input/Output | 2 | Graph Input、Graph Output |

## 2. 完整性评估

| 能力 | 评价 | 说明 / 缺口 |
|---|---|---|
| 剪辑、时间重映射、多机位、代理、OTIO | ✅ | 完整 |
| OpenFX GPU 零拷贝 | ✅ Linux / ◐ Windows 实验性 / ❌ macOS | Linux/Vulkan + EGL：`OPAQUE_FD` + `GL_EXT_memory_object_fd`，正式支持；Windows 为未验证的实验路径，默认关闭（`QUERCUS_OFX_GPU_ZEROCOPY=1`）；macOS 后续 |
| 转场 | ✅ | `transitions.rs` 支持 4 种：Cross Dissolve、Fade、Wipe、Slide（`type_in` combo）；`transitionfx` 另有 color/direction。缺 dip-to-color、iris 等少数类型 |
| 调色（一级） | ✅ | Color Correct（sat/contrast/gamma/gain/offset）、Grade、Three-Way、White Balance、Saturation、Gamma、OCIO LUT/Grading、Display Transform |
| 调色（二级/分析） | ◐ | 没有 Curves、HSL qualifier、Hue-vs-Sat 等二级校色；没有视频直方图/矢量图/波形示波器；没有 LUT 资产/输入色彩空间自动识别 |
| 文字/标题 | ✅ | Text/Text3 支持字体、大小、颜色、对齐、描边、发光；系统字体 341 个；另有 Shape/Stroke/Drop Shadow |
| 字幕 | ❌ | 只有字幕轨类型、`SubtitleParams` 元数据、SRT 容器和导出开关；**没有字幕片段/渲染/SRT 导入的实现** |
| 键控 | ◐ | Chroma Key（上下容差、garbage/core matte、阴影/高光）、Color Difference Key、Despill；缺 Luma Key 和更高级的 keyer |
| 合成 | ◐ | Merge 只有 alpha-over，没有 multiply/screen/overlay 等混合模式；有 Group/Mask/Premult |
| 变换/变形 | ✅ | Transform、Position、Corner Pin、Crop、Flip、Mirror、Swirl、Ripple、Wave、Tile、Mask（feather/invert） |
| 生成器 | ✅ | Solid、Ramp、Checkerboard、Color Bars、Noise、Text、Shape、Polygon |
| 基础滤镜 | ◐ | Blur、Directional Blur、Sharpen、Mosaic、Edge Detect、Dilate/Erode、Drop Shadow、Stroke、Opacity；缺 Glow、Vignette、Grain、Lens Flare、Chromatic Aberration |
| 音频后期 | ❌ | 只有 Volume/Pan 两个音频节点；混音器**不应用效果链**（见第 3 节），没有 EQ、压缩、限制、ducking、淡入淡出自动化；导出只有固定 gain |
| 跟踪/稳定/降噪 | ❌ | 没有 tracker、stabilizer、降噪节点 |
| OFX | ✅ | 有 OFX host（scan/register/instance），可以补风格化/高级滤镜 |

## 3. 关键证据（源码位置）

- **转场类型**：`crates/quercus-node/src/nodes/transitions.rs:58`
  `TYPE_NAMES = ["Cross Dissolve", "Fade", "Wipe", "Slide"]`，输入为 `type_in`；
  `transitionfx.rs` 额外有 `color_in`、`direction_in`。能力在引擎，SDK 的
  `AddTransitionAtSeam` 没有 type 参数。
- **音频效果未接入混音**：`quercus-render/src/ticket.rs` 的 `MontageClip` 有
  `gain` 和 `effects` 字段；但 `eval.rs:2657` 的 `mix_audio_montage` 只用
  `buf[i] * clip.gain`，从不读取 `clip.effects`。导出侧
  `quercus-task/src/render.rs` 也直接写 `effects: Vec::new()`，注释为
  “Audio clips carry no effect stack”。因此 Volume/Pan 节点在导出/渲染音频里
  **不生效**，只有每片段恒定 gain=1.0。
- **没有字幕块**：`crates/quercus-node/src/nodes/` 下没有 subtitle/字幕节点；
  只有 `SubtitleParams` 元数据、字幕轨类型和 SRT 容器/导出开关。没有“放字幕片段/
  导入 SRT/烧录字幕”的实现。
- **没有二级校色/示波器**：`nodes/` 中没有 curves/HSL/hue 节点；引擎里也没有
  视频 histogram/vectorscope/waveform（只有音频 waveform）。
- **Merge 无混合模式**：`merge.rs` 的 shader 是 alpha-over，没有 multiply/screen 等。
- **文字能力可用**：`render::font_families()` 返回 341 个系统字体；Text3 有
  `font_family_in`、`font_size_in`、`outline_*`、`glow_*`、`color_in` 等输入。

## 4. 对参考片的适用性

| 参考片需求 | 引擎是否够用 |
|---|---|
| 多镜头剪辑/节奏 | ✅（模型负责选镜头，引擎负责落时间线） |
| 一级调色（对比/饱和/色温/三通道） | ✅ 节点齐全，可近似；没有示波器，只能靠模型看帧对比 |
| Log→709 输入变换 | ✅ OCIO LUT / Display Transform 可做；但没有自动识别/LUT 资产管理 |
| 二级校色（局部肤色/天空） | ◐ 缺 HSL qualifier/Curves，只能靠 Mask + 一级调色近似 |
| 烧录中文字幕 | ❌ 没有字幕渲染；只能用 Text 片段手工做，无 SRT 工作流 |
| 音乐/旁白/ducking | ❌ 能导入/放置，但没有混音效果，也没有 ducking/自动化 |
| 标题/角标 | ✅ Text + Shape + Stroke/Drop Shadow |
| 转场 | ✅ 4 种基础类型足够（原片以硬切为主） |
| 风格化滤镜（辉光/暗角/颗粒） | ◐ 缺，需 OFX 补 |

## 5. 建议的引擎补全优先级

**P0**
1. 音频后期：把 Volume/Pan 接入混音效果链；补 EQ、压缩、限制、ducking、
   淡入淡出自动化（关键帧）；每片段 gain/pan 生效。
2. 字幕：字幕块 + SRT 导入/导出 + 渲染/烧录路径；导出 `subtitles_enabled` 真正生效。

**P1**
3. 调色：Curves、HSL qualifier/二级校色；视频示波器（histogram/waveform/vectorscope）
   或至少直方图/饱和度统计 API；LUT 资产/输入色彩空间识别。
4. 合成：Merge 混合模式（multiply/screen/overlay 等）；Luma Key。
5. 转场：把已存在的 `type_in` 暴露到 SDK 命令（Cross Dissolve/Fade/Wipe/Slide）。

**P2**
6. 跟踪/稳定/降噪节点。
7. 风格化滤镜（Glow/Vignette/Grain/Lens）——也可以继续靠 OFX。

## 6. 结论

- 68 个内置效果覆盖了 **剪辑、一级调色、变换、键控、生成器、文字、基础滤镜**，
  对一个纪录片式短片（原片主要靠硬切 + 一级调色 + 字幕 + 音乐）来说，
  **调色和标题够用**。
- 真正缺的是 **音频后期** 和 **字幕工作流**；另外 **二级校色/示波器**、
  **跟踪/降噪**、**混合模式** 属于专业能力缺口。
- 风格化/高级滤镜可以走 OFX；但 OFX 补不了音频混音和字幕工作流。
