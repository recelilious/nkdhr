# nkdhr UI 栈——用户与应用开发指南

> [English version / 英文版本](USAGE.md)

nkdhr UI 栈是集成 shell 与项目内系统应用共享的渲染、组件和交互基础：
`nkdhr-render` 记录与后端无关的 2D 图元并通过批处理 OpenGL ES 绘制；
`nkdhr-ui` 在其上提供保留式组件树、布局、输入、动画、主题和类型化 action。
同一套组件代码既可作为 `nkdhr-canvas` 内的场景节点，也可运行在独立 Wayland
客户端中；两种宿主不会使用不同组件集。

## 坐标与色彩

应用与组件使用逻辑 `f32` 坐标，目标 scale 只在绘制时提供，因此像素对齐属于
渲染后端。颜色为规范化 RGBA，通常用 `Color::from_srgba8` 构造；GLES 全程使用
预乘 alpha。非有限几何、负尺寸/圆角或无效透明度在记录阶段即被拒绝。

## 绘图图元（`nkdhr-render`）

一帧由 `DisplayList` 描述，应用通过 `DisplayListBuilder` 按画家顺序记录：

```rust
use nkdhr_render::{Color, CornerRadii, DisplayListBuilder, Rect, Shadow};

let mut list = DisplayListBuilder::new();
list.shadow(
    Rect::new(24.0, 24.0, 320.0, 180.0),
    CornerRadii::all(18.0),
    Shadow::new(0.0, 10.0, 24.0, 0.0, Color::from_srgba8(0, 0, 0, 96)),
)?;
list.backdrop_blur(
    Rect::new(24.0, 24.0, 320.0, 180.0),
    CornerRadii::all(18.0),
    32.0,
)?;
list.rounded_rect(
    Rect::new(24.0, 24.0, 320.0, 180.0),
    CornerRadii::all(18.0),
    Color::from_srgba8(36, 40, 59, 255),
)?;
let display_list = list.finish();
# Ok::<(), nkdhr_render::BuildError>(())
```

公共图元包括矩形/独立圆角、同路径边框、带 offset/spread/blur 的外阴影、按绘制
顺序采样的圆角背景模糊、可裁切/调透明度/采样方式的 RGBA 纹理、嵌套轴对齐 clip
与平移/缩放/旋转仿射变换。clip 入栈时允许平移缩放但拒绝旋转/错切；clip 内内容
仍可任意变换，这使行为确定且无需隐式 stencil 回退。

`TextureStore` 以稳定 `TextureId` 保存带 revision 的 RGBA 资源和单通道 alpha mask。
每个 GLES context 只上传所需 revision；文字颜色不必复制 mask。删除资源会在下一次
prepare 使所有后端缓存失效。

背景模糊先读取图元背后的目标，再让填充、边框与内容清晰绘制在其上。部分重绘的
合成器必须先用 `PreparedDisplayList::expand_damage` 扩展下层 damage，并把同一物理
damage 交给 `GlesBackend::draw`。无法满足该依赖契约时不得宣称支持 blur，材料系统
会选择可读的补偿填充。

## 文字（`nkdhr-ui`，UI-2）

文字样式包含字体族列表、weight/style、逻辑字号、行高、换行与对齐。`cosmic-text`
负责 shaping 与 fallback，同一 run 可混排拉丁、CJK、emoji 和双向文本。布局缓存键
由内容、字体属性、宽度、scale、locale 组成；颜色不在键内。

每个渲染 context 持有一个 `TextSystem`。默认缓存 256 个段落，并把 mask atlas 限制
为四张 1024 页、彩色 atlas 两张。`layout` 后通过 `begin_frame().draw(...)` 写入
display list；提交期间必须保留 frame guard，防止所引用 atlas 页被逐出。可见 clip
会同时记录裁切并跳过远离视口的行。复用旧 glyph 坐标前须比较
`atlas_generation()`，因为页逐出会提升代次。

## 组件与布局（`nkdhr-ui`，UI-3）

应用组合 `Row`、`Column`、`Stack`、`Padding`、`Align`、`Button`、`Toggle`、
`Slider`、`List`、`Scroll`、`Text` 与 `TextInput`。布局固定为 measure→arrange；父级
给出有限约束，组件返回有限尺寸，溢出由拥有者明确 clip。命中测试按最终绘制顺序
逆序进行，视觉层级与输入层级不会分离。

`Element` 描述下一棵树；同 sibling key 与组件类型匹配时，`UiRoot::reconcile` 保留
带代次的 `WidgetId` 和私有状态。结构组件不选择颜色、圆角、字形、间距或曲线。
宿主显式调用 layout/paint/dispatch/tick；`AnimationCtx` 可在布局前更新几何，仅绘制
动画可在 `PaintCtx` 读取同一宿主时钟。`Reactive<T>` 只为下一边界排队 invalidation，
不会重入借用中的树。

指针事件从目标向根冒泡，hover 独立于 capture；键盘按语义树顺序并受最近 focus
scope 限制。宿主提供修饰键和系统归一化的非零点击次数。IME selection 必须落在
UTF-8 边界。剪贴板读取带请求者 `WidgetId` 返回，读取期间焦点改变不会把文本粘到
别处。所有动画读取宿主单调 `Clock`，框架不私自选择时长/缓动。扁平语义树供未来
accessibility 适配器使用。

标准组件使用统一 `Theme`：密度、间距、圆角、排版、语义色、七层玻璃与 motion。
`GlassSurface` 根据真实 blur 能力、降低透明度和高对比策略选择材料。pending 状态不
改变布局：Button 阻止重复触发；Toggle/Slider 同时保留 requested/effective 值，
后端未确认时显示边缘反馈并暂时不可再次操作。Reduced/Off 使用静态非空间标记。

有文字的 root 用 `UiRoot::with_text` 和单个 `TextResources` 构造，提交时使用该 root
的 texture store。TextInput 的值、密码 reveal/mask、IME preedit、selection 与 caret
使用同一 shaping/显示边界映射；换行和 BiDi selection 可产生多个裁切片段。单拖、
双击 Unicode 单词、三击视觉行以及 grapheme/visual/word/line/document 键盘移动并存。

Ctrl+A/C/X/V 与非敏感 Ctrl+Z/Y/Shift+Z 使用公共剪贴板和有界历史。密码不进入历史，
语义始终脱敏，除非显式 `PasswordCopyPolicy::Allow` 否则禁止复制。Enter/Tab 是明确
form policy；formatter 收发含 selection 的 `TextInputEdit`。校验可在 debounce、blur
或 submit 触发，带代次的旧结果被忽略，后端失败保留输入。缺少文字资源时返回
`UiError::TextResourcesRequired`，不会静默丢标签。

已验收的 Appearance & Interaction 视图位于 `nkdhr-settings`。每项 apply 使用不透明
token，多个设置可并行 pending，同一设置只有最新代次能发布成功/错误。专业 inspector
从当前可见几何重定向，窄屏退出期间保留输入屏障，禁用空间 motion 时立即 settle；
全局速度按比例缩放 control/panel/fluid 时长而不改变曲线。

Scroll 使用不占布局的 inset 比例滚动条；细 thumb 有更宽 hit target，拖动保留精确
抓取点，track 点击分页。滚轮不抢键盘焦点，Shift 为横向；Tab 聚焦后支持方向键、
标准 Vim HJKL、Page、Space、Home、End。精密/触控滚动用显式 phase，惯性从注入
时钟取样且可中断，snap point 使用同一时钟。嵌套区域只冒泡自己不能消费的精确余量，
只有最外抵达边界拥有弹性；thumb capture 永不转移。anchor/reveal 用 caller revision
避免反复对抗用户滚动，virtual list 仍遵守同一事务。

List 支持稳定 ID 单选和 `ListMultiSelection`（独立 cursor/range anchor）。Shift 扩展，
Ctrl 切换离散项或只移动 cursor，Space 作用于真正焦点行；方向/Home/End/Page 和
Unicode/IME typeahead 会移动真实子焦点。树 disclosure、reorder、虚拟前后 extent、
loading 高度、导航行单击、对象行双击/Enter、右键/ContextMenu/Shift+F10 均产生稳定
identity 事务，子控件已处理事件时不会误触发行。

## 主题（`nkdhr-ui`，UI-4）

`nkdhr-theme` 拥有有界、不可执行的 schema-v1 profile。profile 选择 Tokyo Night、
Nord 或壁纸 base，显式 overrides 单独保存。壁纸 base 始终携带完整 frozen fallback，
导出后无需原图；live 再生成只替换 base palette，不碰 override。profile JSON 上限
1 MiB；活动值是 CTRL-5 `theme.profile`。`theme.library` 是独立、最多 256 profile、
4 MiB 的原子集合。

Appearance Settings 通过共享 `ThemeRuntime` 即时预览完整合法 profile，cancel 恢复
committed baseline；save 产生带代次的异步 `theme.profile` 请求。失败保留预览重试，
确认推进 baseline；干净状态接纳外部更新，有本地工作时显示冲突并保留本地内容。
保存/复制/导入导出 library 使用同一完整校验边界。

宿主在 UI 线程外解码壁纸并传入借用 RGBA8 view。生成器最多均匀取样 262,144 像素，
写入固定 5-bit/channel histogram，拒绝透明/畸形输入，只返回完整语义 `PaletteData`。
Auto/Dark/Light、colorfulness、contrast 均为有界输入；最终对比保证主文字 7:1、次文字
4.5:1、muted 3:1、on-accent 4.5:1。异步提取只有最新 token 可发布；干净 live 结果
预览并原子持久化 frozen fallback，本地已有修改时仅更新 base 并保持未保存状态。

有效主题作为不可变代次发布；无效更新保留最后有效值。UiRoot 只在自己的活动边界
同步，所以不同显示器可暂时处于不同代次并安全跨过中间代次。只有实际读取且发生
变化的 token 才触发 paint/layout。受信任宿主可在 runtime 前注册 reverse-DNS
`extension.*` 类型化 token；profile 只携带稀疏值，未知/无效扩展拒绝整个候选。
当前静态 daemon 不加载插件，未来 loader 必须给 daemon、Settings、shell 同一声明集。

## 集成与独立宿主（`nkdhr-ui`，UI-5）

`AppearanceSurface` 是唯一真实 Settings 应用边界，拥有已验收模型、资源、主题代次和
一个 `UiHost`。集成模式用 `UiPinnedNode` 把 display list 在合成器世界变换下直接提交
到当前 GLES frame；输入为节点局部坐标，capture 可越界，局部键盘焦点优先于客户端/
全局绑定，外部按下会清除。可用 `NKDHR_CANVAS_DEMO_UI=1 nkdhr-canvas --nested`
测试，默认关闭。

`nkdhr-settings` 二进制是独立 Wayland/winit/EGL 宿主，直接提交同一 display list。
configure、fractional scale、指针、多击、键盘/repeat/text、focus、IME preedit/commit
都归一化为同一 `UiEvent`；目标定向纯文字剪贴板由平台边缘的 `wl-copy`/`wl-paste`
完成。集成 fixture 尚未扩展下层 scene damage，因此诚实声明无 backdrop blur 并使用
补偿材料；独立全帧宿主可安全启用 blur。

## 绑定与类型化 action（`nkdhr-ui`，UI-6）

`ActionCatalog` 公开稳定 ID、说明、instant/continuous 类型、封闭标量参数 schema 与
宿主能力要求。配置只能携带 action ID 与数据，不能携带 shell、Rust、JavaScript 或
待求值表达式。CTRL-5 在 `canvas.bindings` 保存上限 1 MiB 的 schema-v1 文档；空值
是迁移/默认 sentinel，会从三个旧 key 叶子构造同一完整结构化文档。

```json
{
  "version": 1,
  "bindings": [{
    "id": "window-close",
    "context": "window",
    "trigger": {
      "type": "key", "key": "Escape",
      "modifiers": ["logo"], "phase": "press"
    },
    "invocation": { "action": "canvas.window.close", "arguments": {} }
  }]
}
```

编译会规范化 key 与 modifier，手势 identity 包含设备、指头数、起点、可选方向、
activation 和 context。未知 action、参数错误、畸形 trigger、重复 ID 或重叠冲突是
error，拒绝整个候选并保留原 Arc/代次；缺少设备/能力是 warning，该行仍可发现但不
匹配输入。嵌套模式因此明确显示 TTY VT/触控板默认不支持。

默认映射包含 Phase 2 操作及已确认的标准 Vim 方向变体：Super+Escape 关闭、Alt+Tab
循环、Super+O 总览、Super+方向/HJKL 平移、Super+Shift+方向移动聚焦窗口、
Super+Ctrl+方向调整右/下边、Super+数字跳 mark、Super+Shift+数字设 mark。指针移动/
缩放/空白平移，以及 TTY 三指滑动平移和三指中心锚定捏合缩放也使用同一文档。
两指滚动始终属于客户端；当前触屏 action 显示不支持，完整触屏序列会转发给客户端。
四指类别保留给 workspace/overview，但在对应 action 经所有者确认前不猜方向默认值。

连续 action 接收 Begin、零或多个 Update，以及恰好一个 End 或 Cancel。焦点/输出
变化、目标销毁、设备移除、锁屏或新绑定代次都会中央取消；取消后消费过的物理序列
余部会被抑制，客户端不会收到缺失 begin/press 的 update/release。

`BindingSnapshot` 携带精确 catalog Arc、代次、编译行和诊断。
`BindingSettingsModel` 直接消费它生成无样式的可发现行；拒绝候选只替换诊断，不替换
有效行。`ActionFeedback` 为以后 shell 提供 invoked/began/updated/ended/cancelled
统一反馈，本阶段没有擅自设计通知视觉。

## 分段动画曲线（`nkdhr-theme`、`nkdhr-ui`，UI-7A）

UI-7A 已实现可移植曲线值与 runtime compiler；经确认的专业 Settings 编辑器刻意尚未
开始组合。`MotionCurveData` 保存 2–64 个严格按时间排列的锚点，端点固定为 `(0,0)`、
`(1,1)`。切线显式使用 automatic、同方向但两侧独立长度的 continuous、两侧独立的
broken 或收回手柄的 corner。duration 不属于曲线，因此同一规范化形状可复用在不同
时长。

`CompiledMotionCurve::compile` 校验完整数据，解析带版本且保持形状的自动切线，拒绝
时间回头的控制多边形，解析求出真实进度极值，并拒绝未授权的超调或反向。采样只依赖
绝对规范化时间，通过 segment 二分查找与固定次数的单调时间反解完成；无 allocation、
无锁，端点精确返回。后续 editor/runtime 可读取 `analysis`、`velocity` 与稳定内容
fingerprint。

`split_motion_curve` 使用精确 De Casteljau 分割，因此新增锚点本身不会改变动画，只有
随后移动点或手柄才会。现有 `CubicBezier` API 保持有效，并提供
`to_motion_curve_data`/`compile_motion_curve` 无损迁移入口。旧 CSS cubic 若时间控制
多边形不满足新编辑器的严格顺序，会在内存中精确分成两个合法 segment。UI-7A 不自动
改写现有主题 JSON；版本化 preset 持久化与继承 runtime snapshot 属于 UI-7B。

## 动画风格继承（`nkdhr-theme`、`nkdhr-ui`，UI-7B）

UI-7B 把 `MotionStyleProfileData` 作为主题 motion 数据中可选的 `motion.style`。缺省本身
有明确含义：现有四条 cubic 与各 family duration 只在内存中精确迁移，因此旧 profile
JSON 不会被改写，所有现有 widget 也继续使用已经验收的 `MotionProfile` 路径。显式风格
会固定一个内置 `(style, revision)`，或嵌入一份完整不可变 preset snapshot，再保存稀疏
override tree。

继承顺序固定为：配置、语义族、稳定组件标识、具体过渡。每层都可分别提供完整
`MotionCurveData` 和 duration。曲线始终是一个原子字段，不会按 anchor/tangent JSON
局部继承；因此组件曲线可同时继承语义族时长。`resolve_scope` 分别报告两个字段的精确
来源。删除当前层某字段就是 reset，并会重新显露准确的父级值。

`ThemeSnapshot::motion_style` 暴露在主题 generation 发布前完整编译的不可变
`CompiledMotionStyle`。base 与 override tree 中的每条曲线都会编译，即使当前被另一值
遮住；无效候选保留原样的最后有效主题 snapshot。`resolve`/`resolve_family` 直接返回
Arc-backed 编译曲线、时长和字段来源，不重新解析配置或编译曲线。

Balanced revision 1 是已验收旧默认值的精确快照。Lively、Calm、Direct 已保留稳定
身份，但在与所有者共同校准前刻意没有数值 revision；引用不存在的版本会明确失败，不会
偷偷 fallback。`snapshot_as_preset` 可把同层有效覆盖冻结成不再依赖原内置 base 的便携
用户 revision。

`MotionPresetLibraryData` 最多保存 256 个不可变 `(id, revision)` 快照；重复导入相同
内容是 no-op，同一身份的不同内容则冲突而非覆盖。Settings 的
`MotionPresetLibraryEditor` 会在返回单个 `theme.motion_library` CTRL-5 写请求前完整解析
并编译导入；只有宿主确认整次写入后，durable local library 才变化。UI-7B 没有组合任何
编辑器界面；语义 fluid 参数属于 UI-7C，style-neutral 图形编辑状态属于 UI-7D。

## 策略动效运行时（`nkdhr-ui`，UI-7C）

组件执行应使用 `ThemeSnapshot::motion_runtime`，而不是只供创作检查的
`motion_style`。用稳定 `MotionScopeData` 与 `MotionPropertyDomain` 解析，返回的不透明
`MotionExecutionSpec` 已经执行 Expressive/Standard/Reduced/Off 最终策略。Reduced
让空间运动立即完成、只保留短暂非空间反馈；Off 全部立即完成。两者都不会禁止直接键鼠
操控。调用方必须消费 `KineticMotion` 或 `SelectionMassMotion` 的 begin/terminal 结果；
它们只保留最新目标，从不排队 clip。

流体字段可在任意 style scope 独立覆盖。调用 `resolve_fluid` 后直接采样返回的
`ResolvedSemanticFluid`，调用方不能传入或覆盖其策略模式。事件变化需要稳定 event
seed；常动水面需要稳定 component seed 与绝对时间。显式为零的振荡保持静止；非零振荡
会在 Standard/Expressive 中持续活动，并在 Reduced/Off 中被强制精确归零。

本阶段只提供框架与执行行为。现有组件继续保持已验收外观，直到每个组件的视觉接入与
数值调校逐项和所有者确认；这里没有开始组合 Settings 编辑器或流体组件造型。

## 无样式动画编辑器（`nkdhr-ui`，UI-7D）

`MotionCurveEditor` 统一拥有有效曲线、继承父值、独立 duration、字段来源、选择集、
viewport、playhead 和有界 undo/redo。构造时传入准确继承值与
`MotionCurveConsumerSet`。consumer set 会取所有登记属性能力的交集：只有全部 consumer
都是 spatial 或 shape 时才允许 overshoot/reverse；opacity、color 和 bounded scalar 会
拒绝这些权限。空集合刻意采用最保守策略。

编辑继承曲线或时长会生成显式 override；`reset_curve`/`reset_duration` 只删除该覆盖，
重新显露此刻准确的父值。双击调用 UI-7A 同一套保持形状的 De Casteljau 分割。锚点、
automatic/continuous/broken/corner 切线及直接手柄坐标都支持数值编辑。多选拖动使用统一
delta，受未选择邻点限制，并可分别吸附 time/progress。每个候选在发布前都完整编译，
所以错误数值、权限或剪贴板内容不会改变最后有效文档与历史。

拖动应包在 `begin_transaction`/`commit_transaction` 中，任意多中间帧只产生一个 undo
步骤；cancel 会恢复开始时的曲线、选择、playhead、viewport 与 playback。复制粘贴使用
带版本且最大 64 KiB 的 JSON 关键帧数据；能适应新邻点时保留显式手柄，脱离原 segment
后无法合法放置时才解析并约束手柄，再编译整个候选。重复时间或不安全进度仍原子失败。

图形内部始终保存规范化时间；`MotionEditorAxis` 只负责映射到可独立修改的真实 duration，
不会改变曲线几何。viewport 支持有界 pan/zoom，播放只根据宿主绝对时钟推进；
`take_preview` 把同一宿主帧前任意次编辑/playhead 更新合并为最新不可变预览。

`MotionEditorInputController` 是无样式 adapter 契约，不是 widget。它接收已命中图形的
鼠标、笔和单指直接编辑，图形内双指触摸/精密触控板 viewport 手势，以及键盘选择/编辑；
不会注册 compositor-global 手势。方向键和标准 Vim `H/J/K/L` 分别表示左/下/上/右，
Shift 使用粗调步长，剪贴板读写以显式请求返回。UI-7E 生产接线已经把鼠标直接编辑、图形
局部鼠标/精密触控板 viewport 手势、键盘/历史/剪贴板命令和宿主时钟预览播放接入逐项确认
的 Settings 组合；持久化仍是后续切片。

## 验证工具

`nkdhr-render` 含确定性图元 gallery；软件参考渲染器生成提交的 PPM golden，测试逐字
节比较，仅在有意视觉改动时提示更新命令。GLES gallery 离屏绘制相同 display list；
约 1,000 混合图元的目标是在参考 Iris Xe 上低于 2 ms，软件数据单独报告，不冒充 GPU
指标。文字还有混合脚本 golden 和滚动裁切 benchmark。Settings 生产视图 golden、
两个 UiHost 的相同 frame，以及真实独立/嵌套首帧共同验证两种宿主。
