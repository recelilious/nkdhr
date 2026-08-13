# nkdhr UI 栈——内部实现

> [English version / 英文版本](INTERNALS.md)

## 范围

本文覆盖 Phase 3 UI-1…UI-6：共享图元渲染器、文字系统、保留式组件工具包、主题
runtime、双宿主集成与类型化交互语言。shell 产品组件属于 Phase 4；gallery/demo
只验证公共 API，不是临时 shell 实现。

## 依赖方向

```text
nkdhr-canvas ─┬─> nkdhr-ui ──> nkdhr-render ──> Smithay GLES
              └─> COMP-7 pinned-node host

nkdhr-settings/files/tasks ──> nkdhr-ui ──> nkdhr-render
nkdhr-ui ──> nkdhr-ipc
```

`nkdhr-render` 不依赖 UI、画布世界、Wayland 组件或 CTRL-5；`nkdhr-ui` 不依赖具体
画布后端，宿主适配器向内依赖工具包。

## crate 布局

`nkdhr-render/src` 分为公共 geometry/color、经校验的 display-list recorder、稳定
texture/revision、Smithay GLES backend、确定性 software oracle 与提交的 shader。
`nkdhr-ui/src` 分为 text、tree、layout、input、animation、widgets、theme/runtime、
双 host 与 action。模块随里程碑落地，不预建空壳。

## UI-1：图元层

### Display list

`DisplayList` 是画家顺序的不可变序列，命令只含 owned、后端无关数据和稳定
`TextureId`。Builder 管理 transform/clip 栈，输入时验证、归一化圆角并把状态拍平到
命令；`finish` 无需再失败。逻辑坐标保持 `f32`，目标 scale 只乘一次；六分量仿射
变换按父→子组合。clip 被交成每命令一个 target-space 交集，空交集丢命令，旋转或
错切 clip 返回 `NonAxisAlignedClip`。

图元只有 Shape、Texture、BackdropBlur。矩形是零圆角 rounded rect；border 与 shadow
复用同一 signed-distance 路径，阴影边界为 spread+3σ。纹理 source 在 prepare 对 CPU
资源规范化。BackdropBlur 是严格画家顺序 barrier：只滤它之前的命令/合成器层，不滤
自己的材料或之后内容。

### GLES 与批处理

每个 Smithay `GlesRenderer` context 对应一个 `GlesBackend`，拥有四个 shader、动态
VBO、共享 index buffer、按目标调整的 blur texture/FBO 与 context-local texture cache。
prepare 在借用 `GlesFrame` 前上传 revision；draw 在 Smithay 已激活的 framebuffer/
projection 内通过 `with_context` 运行。

shape 每 quad 六顶点，连续同 clip shape 合成一个 draw；texture 每 quad 四顶点，
连续同 texture/sampling/clip 合批。任何 pipeline、clip、texture、blur barrier 都不
跨越重排。Blur 只复制 radius 扩展后的依赖区域，执行九采样水平与垂直 pass，通过
变换后的圆角 mask 替换预乘像素，并在抗锯齿边缘混回原 snapshot。

部分合成宿主必须先调用 `PreparedDisplayList::expand_damage`，用其物理矩形重绘下层
并提交同一 damage；重叠 blur 依赖按 fixed-point 传播。后端恢复 framebuffer、viewport、
buffer、program、texture unit/binding、attribute、scissor 与预乘 blend 基线。资源销毁
显式且依赖当前 context；下一 prepare 回收 texture 删除。shader 仅要求 GLES 2.0，
不依赖 UBO/SSBO/desktop GL/instancing。

### Golden 与性能

`software.rs` 是测试 oracle，不是产品路径。它对拍平 display list 实现同一预乘混合、
圆角 SDF、border、shadow、clip、transform、texture sampling 和可分离 blur；每次 blur
读取不可变 painter-order snapshot。固定 scale 后叠加明确背景并编码 binary PPM，
提交字节即 golden 契约。独立测试覆盖几何、变换、clip、圆角、batch barrier、revision
和预乘；离屏 GLES 仅在硬件 edge coverage 处允许小容差。

benchmark 使用约 1,000 稳定图元，warm-up 后分别测 CPU submission 与 fenced GPU，
记录驱动、分辨率、scale、图元/draw 数与 percentile。2026-08-08 Iris Xe 2560×1600
一批次结果为编译+VBO 1.937 ms、GPU median 1.189 ms、p95 1.228 ms；clear+draw+fence
p95 3.032 ms 单列。2026-08-09 加 1160×760、36 logical px blur 后 GPU p95 3.212 ms，
完整 wall p95 5.426 ms；高频离屏 oracle 最大通道差 ≤4、p95 ≤1。

## UI-2：文字

`cosmic-text` 负责 shaping、BiDi 与 fallback。glyph atlas key 含 font identity、glyph ID、
subpixel bin、scale、raster mode；Alpha8 mask 与彩色 glyph 分开。atlas 用 skyline allocator、
有界页数和页级 LRU，本帧引用页在提交完成前 pin。文字布局 LRU 与 raster cache 分离：
改色只 repaint，改宽只重排需要 wrap 的 run，逐出 atlas 不丢 shaped layout。

`TextSystem` 一次加载系统字体库，缓存不可变 `TextLayout`，键包括内容、字体优先级、
weight/slant/metrics/wrap/alignment/width/scale/locale。布局存每行 glyph range，垂直 clip
用二分选择邻近行，因此滚动成本随视口而非全文增长。新 glyph 先尝试 oxitext 的
COLR/CPAL（Swash 0.2 会把已装 Noto COLRv1 画成单色 outline），再分别打包 mask/RGBA。
dirty CPU 页更新稳定 texture revision；逐出页删除其所有 key 并提升 atlas generation。
若所需格式的全部有界页都被当前帧 pin，明确返回 `AtlasFull`。

确定性测试使用提交的 SIL OFL Noto Sans、Noto Sans CJK SC、Noto Color Emoji 子集，
覆盖 Latin/CJK/emoji fallback、BiDi、wrap、golden、彩色页、逐出重建和行 clip。验收
benchmark：5,000 混合行/265,000 glyph，初次 layout 175.999 ms、缓存 1.539 ms；
滚动帧访问 2,055 glyph、记录 1,744 图元，CPU p95 0.262 ms，稳定 atlas 无逐出。

## UI-3：工具包核心

每个组件有带代次 `WidgetId`。保留树存 descriptor 私有状态、children、layout 与 dirty。
同 key/位置及类型才复用；删除先释放 capture、focus、hover、animation 和 reactive。
invalidation 分 layout/paint/semantics；layout 为约束/测量/排列，paint 顺序也成为逆序
hit-test 顺序并共享 clip。

`Element` 带可选 `WidgetKey`、flex 与 children；arena 用 `(slot,generation)`，slot
复用提升 generation。keyed sibling 用临时 map 线性匹配，无 key 按位置。`Constraints`
要求有限且 min≤max，所有派生 Rect/size 再校验。Flex/Padding/Align/Stack/Clip 无主题
默认。未布局不能 paint；完全 clip 或父级故意不画的 subtree 会消费 paint dirty，
避免永远请求 frame。

Reactive 只往 root 队列追加 invalidation；订阅 token 由 retained node 拥有、去重并在
替换/删除时立即解绑。宿主在明确边界处理更新、输入、动画。`Widget::animation` 在下一
layout 前得到 `AnimationCtx`，几何动画可改状态并重新注册；仅绘制动画可在 PaintCtx
采样同一 Clock。生产用 SystemClock，测试用 ManualClock，不内置 duration/easing。

Pointer 默认命中最后绘制节点并向根冒泡，capture 时 hover 仍跟真实 hit path；
PointerCancel 先送 captured target 再强制释放。focus 为受 scope 限制的树路径，Tab 按
semantic 顺序。IME selection 验证 UTF-8 byte boundary。host 传 modifier 与非零 click
count。复合组件可显式 handled-and-continue 并请求直接 child focus，普通 handled 仍
立即停止。剪贴板请求在 `DispatchResult` 聚合，read 带原 `WidgetId`，过期/被删 target
不收返回。overlay hit rect 和只影响绘制的 child translation 支持滚动条与弹性。

Theme/Motion/标准组件是经所有者确认的产品层。Theme 包含密度、间距、圆角、排版、
palette、七层 glass；能力解析在无真实 blur 或降低透明度时使用补偿 opacity。
`MotionProfile` 含曲线、family 时长、全局 policy 和 fluid tuning；ScalarMotion 重定向
前采样可见值，procedural variation 只供 shell 且由稳定 seed 有界决定。全局 speed
同步缩放 control/panel/wallpaper/fluid 时长，不改曲线或幅度。

Button/Toggle/Slider pending 是 layout-stable；Settings 为每项保存最新 opaque token，
并行设置互不阻塞，同一项旧 transport 回包不可覆盖新反馈。List 用稳定 selection/
cursor/anchor，virtual extent 不改隐藏 selection；tree/reorder/typeahead 都发稳定 ID
事务。Scroll 把 exact remainder 冒泡给嵌套容器，生命周期可跨零余量，只有最外边界
拥有弹性；thumb capture 不转移。offset 与 visual elasticity 分开，Reduced 跳过惯性/
snap/stretch。TextResources 统一 TextSystem、atlas、scale、TextureStore，本帧 glyph
统一 pin。TextLayout 保存 source offset、BiDi visual cell 和多片 selection。TextInput
分开 logical anchor/caret，preedit/format/mask 都显式映射；密码无 undo snapshot 且
语义永不暴露。validation 带 generation/debounce/status，旧结果无效，失败不改值。

Appearance & Interaction oracle 使用完整 CJK fixture、真实 icon mask、四宽度模式和
单一外层 blur。专业 panel/drawer 在 retained layout state 持有 ScalarMotion，从当前
几何重定向；窄屏退出期间保留 InputBarrier，Reduced/Off 直接 settle。只 clip 移动
overlay，不切 sibling shadow。2026-08-10 所有者接受静止和导出 transition frame，
UI-3 完成。

## UI-4：主题 runtime

`nkdhr-theme` 把有界 schema-v1 `ThemeProfile` 解析成完整 ThemeData。base 为不可变
Tokyo Night/Nord 或带 frozen palette 的 wallpaper；稀疏 override 只覆盖已知字段，
live base 再生成时原样保留。任何 syntax/version/unknown/type/range/scale/material/motion
错误拒绝整个候选。

`nkdhrd` 用两个独立 scalar 保存 `theme.profile` 与 `theme.library`；namespace 校验
完整 profile、library 全成员、唯一 ID 和边界，原子持久化/Changed。ThemeRuntime 在
锁外 resolve/convert，然后一次交换共享不可变 snapshot+generation；拒绝不动 Arc。
diff 区分 paint/layout，ThemeToken/ThemeReadSet 记录精确 leaf。UiRoot 在自己的活动
边界比较自己上一 snapshot 与最新值，允许跨代；只 dirty 相交 reader。默认 portable
profile 与 UI-3 Theme 结构完全相等。

ThemeProfileEditor 是共享 runtime 上可 clone、宿主无关的 Settings transaction owner。
preview 先校验发布，cancel 重新发布 committed。profile/library commit 返回 opaque token、
dotted key、已校验 JSON，由宿主在线程外写 D-Bus；只接受最新 completion。失败保留
draft，外部 Changed 可确认匹配请求、在干净时采用，或在有本地预览时报告冲突。
library 在替换前整体验证，支持 save/upsert/copy/remove/import/export。

WallpaperImage 校验尺寸、stride、overflow、长度；生成器固定内存最多取样 262,144
像素写入 alpha-weighted 5-bit RGB histogram，以 OKLab/OKLCH 分析、weighted median
选明暗、population/chroma 选 accent，并 gamut-map 全部语义角色。对比与极端输入有
测试，生成器不保留像素/解码。live profile 再生成只替换 frozen base，保留 ID/name/
override；异步 token 保证旧结果和离开 profile 后的结果不能赢。干净结果进入原子保存，
dirty 结果只更新 preview 不暗中写入；失败保留 runtime generation。

ThemeExtensionRegistry 由受信任宿主在 runtime 前装配，reverse-DNS group/token descriptor
声明 type/range/default/paint-layout impact。profile 的 `overrides.extension` 只有稀疏值，
缺省补 default，未知/无效拒绝全部。ResolvedTheme、diff、ThemeSnapshot、动态 ReadSet
和 Settings/library 共享同一 registry。双 root 测试证明各显示器只在本地边界同步，
可 1→3 跨代并忽略 generation 2 中改后还原的 layout 值，同时捕获 generation 3 paint
值。插件加载/声明分发属于以后工作。

## UI-5：两个宿主，一个 runtime

UiHost 拥有 UiRoot、逻辑 size/scale、layout/paint scheduling、最后完整不可变 display
list、texture namespace 与 commit；只有成功记录推进 commit，无改动复用前帧。
UiSurface 是 object-safe 的 render/list/textures/commit/input/focus/frame-demand 边界。
AppearanceSurface 只实现一次；仅 viewport、composition revision 或 theme generation
变化时重建 Element，值型 Reactive 在现有 root 内继续。

集成适配器用 UiPinnedNode 实现 PinnedNode，NkdhrUi payload 借用完整 list/store/commit。
`DisplayList::transformed` 组合外部 viewport translation/zoom 与 clip，root 始终保持
node-local。每 `(node ID, GLES context ID)` 独立持有 GlesBackend 和 PreparedDisplayList，
避免输出/context 代次互相污染。GlesTargetRenderer 统一 nested 直接 frame 与 TTY
multi-GPU 的 GLES render side；element identity/commit signature 含 app commit、target、
placement、zoom、scale，无 CPU 中间图。

指针为 node-local，padding 也可命中，capture 越界继续，focus/leave 明确。局部 UI
键盘焦点在全局绑定前接收 key/text，外部 press 清除。集成 clipboard/IME 桥接仍归
以后 shell，接口已经能接收。独立宿主持有 Wayland winit window、wl_egl_window、EGL
display/context/surface、Smithay renderer/backend；resize/scale 在一帧边界更新。
pointer/keyboard/repeat/focus/text-input-v3 归一化为 UiEvent，剪贴板保留请求 WidgetId。
独立模式全帧绘制可满足 blur 依赖。相同 Appearance root 的 twin test 输出相等，真实
独立和 nested 首帧均已运行，TTY multi-renderer 由 all-feature build 覆盖。

## UI-6：类型化交互语言

`nkdhr-ui::action` 不依赖后端。ActionCatalog 最多 512 个稳定 lowercase ID；descriptor
含说明、instant/continuous、最多 32 个 Boolean/有界 integer/number/string/choice
参数及声明式 required capabilities。ActionRegistry 为每项附一个 Send+Sync 宿主 adapter；
配置永远不能选择函数指针或可执行字符串。

schema-v1 BindingDocument 上限 1 MiB/2,048 项。Trigger 为 key/button/gesture 加 context。
编译解析 action、填 default、校验参数、lowercase key、把 modifier array 变成 bit set，
拒绝客户端保留的两指触控板以及非空白/边缘触屏 ownership，并逐对分析 overlap。手势
冲突比较 kind、device overlap、finger、origin、direction、context；按钮同样比较
device/origin/context。重复 ID、无效 action/参数/冲突是 error；缺能力/设备是 warning，
该行保留但 non-effective。

BindingRuntime 只发布完整编译候选。Arc<BindingSnapshot> 内含同一个 catalog Arc、递增
generation、编译行与 warning；拒绝返回候选诊断但保留精确旧 Arc/代次。canvas watcher
把它与 grid policy 放同一 mutex；`canvas.bindings` 是 CTRL-5 bounded string，空值从
旧叶子合成标准文档，非空 JSON 权威。输入线程每次只 clone 不可变 snapshot。

`ActionDispatcher<App,CanvasActionPayload>` 是可配置 action 唯一 adapter 入口。
`input.rs` 只做宿主归一化/lookup，不按快捷键匹配 action；`actions.rs` 集中实现 action。
instant 用 Invoke；continuous begin 分配 InteractionId，只有同 ID 可 update/end。终止
前先清 ownership，update 失败会 cancel，故最多一个终止 phase。输出/焦点改变、目标
死亡、设备移除、锁屏、绑定代次变化均取消，并抑制已消费物理流的余部。

Phase 2 Drag 仍是 operational state，但只由 action phase 创建/更新/结束。客户端 xdg
move/resize 在协议 grab 校验后进入同一 dispatcher。三指 swipe 平移；三指 pinch 让
初始世界点留在移动逻辑中心下，并把 zoom 限在工作区间；其他手势经 pointer-gestures
转发。真实 TouchHandle 转发 down/motion/up/frame/cancel；在 recognizer 完成前不把触屏
action 宣称有效。锁屏 VT 是刻意例外：fail-closed path 保留 Linux 固定 Ctrl+Alt+Fn/
XF86 紧急 chord；普通会话 VT 是 capability-gated typed action，nested 明确显示不支持。

BindingSettingsModel 直接接收 BindingSnapshot，从其 catalog 格式化无样式 trigger row；
拒绝 publication 只更新诊断。ActionFeedback 提供统一结果 seam，不提前设计 Phase 4
通知组件。任何绑定都不是代码；未来 CTRL-EXT 命令仍必须是单独授权的类型化 action。

## UI-7A：分段动画曲线基础

可移植 authored data 位于 `nkdhr-theme::motion_curve`，runtime 预计算位于
`nkdhr-ui::motion_curve`，从而保持依赖方向且配置不可执行。`MotionCurveData` 是原子
曲线字段，携带 schema、自动切线算法版本、超调/反向权限与 2–64 个锚点。结构校验
固定端点，要求至少 `1e-6` 的规范化时间间隔，在编译前拒绝非有限值并限制恶意
progress/handle 数据。

automatic 锚点用版本一、保持形状的 PCHIP derivative 解析；continuous 把一个保存的
方向规范化后应用两侧独立长度；broken 保留两个向量；corner 得到零手柄。相邻锚点
编译为 `CompiledSegment`，保存四点及 time/progress 的 f64 power-basis 系数。控制时间
必须在 segment 内有序。不可变编译对象把 boxed segments、解析 range/reverse 结果与
稳定 fingerprint 放在一个 Arc 后；采样不分配、不加锁、不访问配置树。

进度极值由每段二次 derivative 的根解析得到；导数符号区间进一步区分正常范围内的
真实反向与允许越过 1 后回到终点所必需的下降。真实超调/反向必须有 authored 权限，
绝对 progress 安全界限不可绕过。采样先二分 segment，再固定执行 32 次时间反解，因此
输出只由曲线和绝对时间决定，不依赖帧率；0 与 1 使用精确分支。

`split_motion_curve` 编译源曲线、解出目标 segment parameter，再执行 De Casteljau
分割；相邻已解析手柄转换为显式 broken tangent，并重新编译完整结果，密集采样证明
形状不变。旧 `[x1,y1,x2,y2]` 可精确映射；若 `x1>x2`，一次精确 half split 会把合法
CSS 旧曲线转换成两个符合新编辑器更强时间顺序的 segment。UI-7A 不改旧 scalar runtime
或 portable theme schema；UI-7B 才负责原子继承 preset 与持久化迁移。

测试覆盖 portable 边界、固定端点/顺序、全部现有默认曲线、精确新增点、隐藏极值/
反向、overshoot settle 不误报 reverse、自动切线确定性、最大锚点数、绝对时间重复性，
以及 256 条确定生成的合法单调曲线。

## UI-7B：继承与不可变预设快照

`nkdhr-theme::motion_style` 让可执行编译远离 portable data。活动的
`MotionStyleProfileData` 会固定一个内置 revision 或嵌入完整 `MotionStylePresetData`，
再携带稀疏 `MotionStyleTreeData`。树的 root values 下按 semantic family 映射稳定
component ID 与 transition ID。文档最多 4,096 个节点、1 MiB，ID 是有界小写稳定标识。
preset root 必须包含曲线和 duration，后代则可独立只含其中之一。

解析会在每个 specificity 层交错 base/profile：base root、override root、base family、
override family，component 与 transition 同理。因此 specificity 高于 origin，而相同
scope 的显式 profile value 会替换 preset。曲线在层边界是一个完整 `Option`，只能整条
替换。曲线和 duration 分别带 `MotionValueProvenanceData`；reset 是删除一个 option，
不是复制父值。`snapshot_as_preset` 把同 scope 字段覆盖到固定 base 上，生成新的完整
不可变 revision。

当前只有 Balanced revision 1 可解析。它由旧四条 cubic 与全部 23 个 family duration
生成，密集测试逐 family 比较编译结果和旧 evaluator。Lively、Calm、Direct 是稳定 enum
身份，但不会凭空制造 revision payload；不可用版本会失败关闭。`MotionData.style` 缺省
时，`CompiledMotionStyle` 在内存中嵌入同一份精确旧数据迁移。可选 serde 字段会跳过，
因此用户明确编写 style data 前，旧主题 profile 不会被改写。

`CompiledMotionStyle` 用预编译 Arc-backed curve 镜像两棵树；编译会访问包括被遮住
节点在内的每条源曲线。`ThemeRuntime` 在取得 publication mutex 前与 `Theme` 一起构建
它，`ThemeSnapshot` 在同一 generation 下携带两者；任何 data/curve 错误都无法替换旧
Arc。查找只走四个有界 map 层并 clone 被选曲线的 Arc，不解析 JSON 或重新编译。UI-7B
中现有 widget 仍执行旧 `Theme::motion` 路径，从而在 UI-7C policy/runtime 前保持视觉
完全不变。

`MotionPresetLibraryData` 是以不可变 `(id, revision)` 为键的 4 MiB/256 preset 集合；
不同 payload 不能覆盖同一身份。`nkdhrd` 将其作为标量 `theme.motion_library` 叶校验，
并给旧 theme 文件补空默认值。Settings 侧 `MotionPresetLibraryEditor` 再执行更强的 runtime
校验：每次 import 必须先在隔离状态完整编译，之后才能产生 opaque persistence request；
只有匹配的 host/CTRL-5 确认后 durable model 才会变化。

## UI-7C：策略管控的中断与语义流体

UI-7C 将 authored Motion Style 与最终 Motion Policy 分开。`MotionRuntimeProfile`
在 publication 前构建，并与编译 style 一同放入同一代 `ThemeSnapshot`。Standard 与
Expressive 解析指定 scope 的曲线/时长，并只应用一次全局速度倍率；Reduced 把全部空间
过渡替换为立即完成，只给非空间反馈保留固定且不超过 100 ms 的短过渡；Off 对所有域都
立即完成。直接操控在每种模式下都可用，而 Reduced/Off 会禁止空间路径、流体拓扑、拖尾、
振荡、程序变化、惯性与静止态水面。`MotionExecutionSpec` 字段不公开，因此组件在策略
替换后不能从 spec 中重新拼回 authored 值。单独暴露的 compiled style 只供创作/检查，
不是执行入口。

稀疏 style value 现在还包含九个可独立继承的语义流体字段：粘度、表面张力、吸引力、
颈部、拖尾、路径灵动度、振荡、阻尼与变化幅度。它们采用相同的配置/语义族/组件/具体
过渡优先级，每个字段单独保留来源。Balanced revision 1 刻意不写入这些新值；解析只把
实际存在的字段覆盖到与旧实现精确兼容的流体基线上，因此迁移不会凭空增加静止振荡，也
不会改变已验收输出。当前范围是 portable 安全边界，不代表已经与所有者确认视觉数值。

`ResolvedSemanticFluid` 私有保存最终策略模式。瞬态 envelope 对 `(progress, seed)`
确定、受校验参数限制，并在进度 0 与 1 精确归零，所以程序变化无法改变声明的终点或
时长。常动水面用绝对时间与稳定 seed 采样：显式启用非零振荡后会持续活动，同时不会
积累帧历史误差；Reduced 与 Off 的两种采样都精确静止。

`KineticMotion` 只拥有一个活动 segment 与一个最新目标。改目标时先采样屏幕当前值和
速度，把旧 run 以 Interrupted 结束，再围绕编译曲线加入端点约束 Hermite 修正；新段
会以原切线开始，并在目标处以零速度结束。策略要求立即完成时仍会产生一次 begin 和一次
Completed。完成/取消最多报告一次，从不排动画队列。

`SelectionMassMotion` 把同一契约应用于稳定节点 ID 向量。负的数值质量会被截断，随后
向量解析归一化；归一化速度导数使速度总和保持为零，同时把总质量精确修正。中断会保留
当前每个节点的可见质量/切线，并只把全部质量重定向到最新节点；完成时收束到该节点，
取消则冻结当前分布且速度归零。这是已经确认的左右分裂、收回和粘液式选中传递的运行
基础，但 UI-7C 刻意没有组合或重绘任何现有组件。视觉数值校准与组件接入仍会由所有者
逐步控制。

所有者确认的未来左侧导航消费者，明确不能实现成在各行之间平移的普通圆角选中框。选中
态必须是一份连续守恒的流体质量：经过中间项目时产生几何形变，局部折射/扭曲其下方采样
内容；每次收到新输入都从屏幕当前形状和速度立即重定向，不排队也不重播固定过渡。因此
连续导航必须保持相位与速度连续。渲染器需要把背景采样、流体几何和选中 envelope 分离，
使主题/材质变化不会影响命中区域。Reduced/Off 仍给出同样直接的导航结果，但按上文契约
移除折射、拓扑变化和程序扰动。

## UI-7D：无样式编辑状态与统一定向输入

`motion_editor::model` 是不负责渲染的 authored 状态机。durable `DocumentState` 只含
可选 curve/duration override；继承值和已登记 consumer 属于宿主上下文。因此编辑继承
字段会保存完整覆盖，reset 则删除 option 并立即跟随更新后的父值。替换父值或 consumer
前会先校验继承与有效曲线，并清除在新上下文中可能不再合法的旧历史。

`MotionCurveConsumerSet` 对最多 256 个稳定 consumer 排序去重，再保守求出 overshoot/
reverse 能力交集。这个 authored domain 刻意比 UI-7C 的 spatial/non-spatial runtime
policy 更细：shape 可允许二者，opacity、color、bounded scalar 则不允许。editor 会先
经过该 gate，再调用既有解析 compiler；不存在 consumer 可忽略的纯显示权限。

每次修改都先构造并编译完整候选，成功后才替换最后有效文档和 compiled curve。活动
transaction 保存准确的 document 与 transient baseline；中间帧可以更新 preview，但
commit 只压入一个有界 undo entry。cancel 会重新编译/恢复 baseline 及其 selection、
primary anchor、playhead、viewport、playback。undo/redo 只存 document state；宿主上下文
变化会清空两栈，避免载入新能力集合不再接受的旧条目。

插点直接使用 `split_motion_curve`。直接操作支持单选/多选锚点、时间顺序 clamp、进度安全
范围、可选 snapping、点/手柄数值编辑及显式切线模式转换。
`resolve_motion_curve_handles` 会把 compiler 解析后的 automatic/continuous/corner 几何
物化成 broken handles，且不改变形状；剪贴板关键帧也使用这些显式手柄。若脱离原 segment
的 copied handle 违反新 segment 的时间顺序或进度方向，fallback 会物化当前曲线并只在
完整候选内按比例约束，再重新编译；恶意数据、重复时间和不安全 anchor 顺序仍会失败。

规范化坐标是权威状态；`MotionEditorAxis` 只把 x 映射到独立 duration 供真实时间显示。
`MotionGraphViewport` 把 pan/zoom 限制在规范化时间及正常进度或绝对 overshoot 安全范围。
playback 保存 `(absolute_started_time, normalized_origin)`，不会积累帧率误差。preview 与
document generation 分离；`take_preview` 每个消费帧最多暴露一次最新 curve/compiled pair、
duration 与 playhead。

`motion_editor::input` 同时只拥有一个 `MotionEditorEditId`，并用 model transaction 包住
direct/viewport 手势。Begin 失败会回滚 ownership，不匹配 ID/device 不能修改活动编辑，
End 消费最终 sample，Cancel 恢复 baseline。鼠标、笔、图形内单指执行直接编辑；图形内
双指触摸或双指精密触控板只操作 viewport，另有 pen barrel 和 mouse viewport 路径。
0 指、超过 2 指及不支持的 device/contact 组合 fail closed。adapter 从不声明 compositor-
global 手势，因此 shell workspace 手势完全在本模块之外。

键盘与 direct input 调用同一 editor 方法：方向键和标准 Vim H/J/K/L 按左/下/上/右微调，
Shift 使用配置的粗调倍率，Tab 循环锚点，Delete 删除可编辑点，Space/Home/End 控制预览，
Ctrl/Logo A/C/V/Z/Y 产生选择/历史操作或显式剪贴板请求。测试覆盖精确插点几何、继承、
原子拒绝、有界/合并历史、cancel、切线模式、剪贴板边界/fallback、宿主时钟 playback、
确定性编辑序列、手势 identity 和全部支持设备类别。UI-7D 没有选择任何 layout、paint
token、组件组合或用户可见风格数值。

## UI-7E：逐项确认后的生产动画工作区

`AppearanceSettings` 持有一个可克隆的 `MotionEditorSession`，而不是把它放进随时会替换的
widget descriptor。因尺寸、主题发布或检查器变化产生的 reconcile 会重建已确认组合，但
不会丢失曲线文档、选择、历史、playhead 或播放模式。文档编辑递增 Settings composition
revision，让文字和检查器 descriptor 从权威 snapshot 重建；另一个 reactive visual
revision 只使曲线图与预览 sibling 在选择、scrub 和宿主时钟播放时重绘，不会每帧重建树。

`MotionCurvePlot` 在已确认的 clay/glass 槽中绘制继承曲线、有效 compiled 曲线、动态
playhead、锚点及选中锚点的 broken handle。命中顺序先 handle、再锚点、playhead 和曲线；
pointer capture 包住直接编辑，双击曲线执行保持形状的精确插点。获得焦点的曲线图转发
UI-7D 键盘契约，包括标准 Vim 方向、undo/redo 与显式剪贴板请求。预览采样同一 compiled
曲线，可以先越过终点再回到稳定最终帧；产品默认明确为单次播放，时间只来自宿主时钟。

绘制与命中测试通过 editor 的稳定 viewport 转换坐标，而不是从每个曲线中间态重新推导
范围，因此拖动越界峰值时坐标系不会在指针下跳动。图形局部滚轮执行平移，Ctrl+滚轮以
指针为中心缩放，Shift+滚轮平移时间轴；精密触控板 scroll lifecycle 从 begin 到
end/cancel 使用一个捕获的 UI-7D viewport transaction。这些操作只更新瞬态视图，不改变
曲线文档或 undo 历史。`100%` 恢复标准化基准视图，`适应`包住 compiled 曲线。

这一切片刻意保持已确认的布局分配、Tokyo Night 配色、Maple Mono 字体和 renderer-native
材质实现。图形数值属性绑定、保存/导出持久化，以及未来守恒液态导航选中块仍属于后续
UI-7E 工作。

clay/glass 深度遵循明确的“边缘—内容分离”约束：外部斜向阴影继续表达部件悬浮高度；
跟随主题的内高光与内暗部只占据一条窄边缘带，其模糊半径不能再复用外部悬浮阴影的
模糊半径。交互内容必须在这条边缘带之外另留组件自己的安全内距。按钮、文本输入框、
开关滑块、导航单元、曲线绘图区以及检查器/抽屉内容因此不会盖住用于表达立体感的明暗
过渡。内阴影透明度可以保持足够强，让材质仍然圆润立体，但增强阴影不能同时扩大它向
内容区侵入的范围。

## 错误与安全策略

- 公共 geometry/style 构造器拒绝非有限值，allocation 在 texture/atlas 前检查；
- shader/GL 错误传播给宿主并跳过整帧，不显示半更新 UI；
- theme 与 binding 使用最后有效 snapshot；
- unsafe 只隔离在 GLES，记录 context/buffer invariant，并由 software oracle 与真实
  离屏渲染覆盖；
- renderer/widget/action callback 不直接执行特权系统工作，系统操作统一经过 `nkdhrd`。
