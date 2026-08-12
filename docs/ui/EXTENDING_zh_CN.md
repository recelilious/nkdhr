# nkdhr UI 栈——扩展规则

> [English version / 英文版本](EXTENDING.md)

本文定义 Phase 3 支持的扩展接缝。进程内 Rust API 并不是稳定的二进制插件 ABI；
外部加载、版本协商和沙箱只由以后真正负责这些能力的功能实现。

## 设计目标

扩展可以加入可复用组件、主题 token 组和类型化 action，但不能依赖合成器后端、
绕过控制面，或让集成宿主与独立宿主表现不同。

## 可复用组件

可复用组件依赖 `nkdhr-ui` 公共 trait，并通过提供的绘制上下文记录内容。它不能：

- 访问 Smithay、EGL 或原始 GLES 状态；
- 假定自己一定运行在合成器内，而不是独立 Wayland 客户端；
- 检查其他组件的私有保留节点；
- 绕过类型化应用模型直接连接系统服务；
- 在 measure、arrange、paint 或输入分发中执行阻塞工作。

自定义组件暴露语义属性与事件，内部动画使用根时钟；无障碍/语义信息属于组件
契约本身。已实现的扩展契约是 `Widget`：descriptor 创建私有类型擦除状态，在
`update` 比较旧 descriptor，并参与 `measure`、`arrange`、`paint`、`animation`、
`event` 和 `semantics` pass。上下文提供稳定 ID、当前状态、根局部响应式 watch、
失效和动画帧请求。子节点遍历保持显式，因此容器拥有布局与绘制顺序，不读取子
节点私有状态。改变几何的动画属于 `AnimationCtx`；纯绘制时间线可继续通过
`PaintCtx` 采样同一根时钟。

自定义输入组件声明可聚焦性、焦点域、指针和裁剪行为；事件处理器提出焦点/捕获
变更，由根在回调结束后应用。`set_handled_and_continue` 只用于子节点有意把一次
复合事务的一部分交给祖先，不是普通事件冒泡；祖先可用 `request_child_focus`
同步逻辑游标。宿主必须在指针按下/释放时提供 modifier 状态和系统规范化的非零
点击次数，组件不能再发明一套双击阈值。

剪贴板也由宿主持有。可编辑组件调用 `read_clipboard_text`/
`write_clipboard_text`，不阻塞平台 API；宿主把读取结果返回给请求携带的
`WidgetId`，而不是稍后恰好拥有焦点的控件。敏感组件必须明确复制与历史策略，
不得把秘密写入语义值。

嵌套滚动容器只能通过 `EventCtx::handoff_scroll` 上报数学上未消耗的 delta，不能
把部分滚动标记为 handled 后在祖先重放原始 delta。边界反馈放在
`Widget::scroll_boundary`，且仅在余量耗尽冒泡路径后触发。捕获指针的文字、对象
与 thumb 拖拽使用普通 handled 传播，不能加入滚动 handoff。

overlay 控件可用 `PaintCtx::register_pointer_overlay`，但矩形必须对应实际绘制在
子节点之上的控件。进入中的 overlay 可用 `paint_child_clipped` 只给目标子节点
增加 reveal clip；不能为了隐藏一个抽屉给整个面板打开 `clips_children`，否则也
会截断兄弟 material 阴影并改变命中裁剪。整个进入/退出期间，布局矩形、reveal
clip 和 pointer barrier 必须描述同一个可见区域。

公共机制不授权扩展临时改写 nkdhr 标准组件设计。首批经确认的内置组件是
`GlassSurface`、`Button`、`Toggle`、`Slider`、`List`/`ListItem`、`Scroll` 和
`Text`/`TextInput`；语义合适时应组合或包装它们。结构性或应用私有组件仍可直接
实现 `Widget`。新增标准组件族或改变内置外观/状态词汇仍需项目所有者参与设计；
补全已确认组件族中明确记录的高级行为属于现有契约内的实现工作。

生成文字的组件使用 `MeasureCtx::layout_text` 和 `PaintCtx::draw_text`，不能自建
atlas 或 texture store。宿主根使用一个 `TextResources` 构造，renderer 提交必须
使用该根的 texture store，确保 glyph ID 位于同一资源命名空间。

能用现有图元表达的视觉必须记录现有图元。新图元只有在代表通用绘制能力，并有
确定性 software oracle、GLES 实现、golden 覆盖和批处理策略时才可加入。

`BackdropBlurPrimitive` 是排序与 damage 依赖，不是装饰 fill。宣称支持 backdrop
的宿主必须把 UI list 放在被采样层之后，在重绘这些层前调用
`PreparedDisplayList::expand_damage`，并把同一个扩展后的物理 damage 交给
`GlesBackend::draw`。无法同时满足三项时必须关闭能力，让 material resolution
选择可读 fallback。

## 主题 token 组

UI-4 的 token runtime、读取跟踪和有界声明式扩展 registry 已实现。它不是插件
加载器：可信宿主在 runtime 构造前组装一个 `ThemeExtensionRegistry`，持久化值的
所有验证者必须接收同一份不可变声明。

扩展可声明 namespaced 类型化 token 组，名称位于
`extension.<owner>.<name>`，使用反向 DNS 所有权。组声明默认值、值类型、验证和
每个 token 对布局或绘制的影响。

扩展不能替换内置 token 的类型或语义。主题缺少扩展组时使用扩展默认值；任一
扩展值无效都会拒绝整个候选，旧的内置与扩展 snapshot 保持活动。profile 只在
`overrides.extension` 中保存稀疏值，不能携带 schema 或代码。支持布尔、有界
整数/数值/字符串、规范化颜色与封闭 choice，并限制组、token、字符串和 choice
数量。

扩展组件在 `ThemeReadSet` 声明动态路径，通过
`Widget::apply_theme_snapshot` 接收完整不可变 generation，并用
`ThemeSnapshot::read_extension` 读取。descriptor 声明的 paint/layout 影响加入
与内置 token 相同的精确 retained-tree 失效。Settings 预览、profile 与 library
导入导出提供 registry-aware 入口。当前静态 daemon 刻意使用空 registry；第三方
持久化值等待可信加载器能向 daemon、Settings 和 shell 分发同一声明集之后启用。

## 类型化 action

公共 `ActionCatalog`/`ActionRegistry<C, P>` 契约已实现。扩展 action 注册：

- 稳定的 namespaced 名称；
- 本地化 description key；
- 封闭参数 schema；
- 控制可用性的声明式宿主 capability；
- instant 或 continuous 类型；
- 返回结构化结果的 invocation handler。

action 名称使用 `extension.<owner>.<action>` 与小写 ASCII 点号/连字符 segment。
重复或无效名称、无效 schema 边界、总数超过 512，或单 action 参数超过 32 都会
使注册失败。参数只能是数据；schema 不能请求执行 shell、Rust、JavaScript 或
配置表达式。绑定编译前把 descriptor 加入完整可信 catalog，然后只附加一个宿主
adapter；未知 action 不能回退到通用 callback。

key/button/gesture 绑定文档同样有界。扩展 trigger 必须经过公共 compiler，不能
绕过 modifier 规范化、context/device/origin 冲突分析、客户端双指触控归属或不
支持设备报告。绑定被拒绝或标记 unavailable 后，扩展不能自行截获客户端流。

continuous adapter 必须接受声明的 phase 词汇。中央 dispatcher 持有 interaction
ID 与唯一 terminal call；adapter 不能再创造第二条 end/cancel 路径、排队 phase，
或在返回后保留 context/payload 借用。cancel 清理必须幂等。异步 cancel 后，宿主
还必须压制同一物理输入流的剩余部分，避免把一个没有 begin/press 的半截序列泄漏
给客户端。

可发现性直接使用实际发布的 `BindingSnapshot`。Settings 不能独立解析文件、重建
catalog，或把已被有效 generation 拒绝的 requested binding 显示为正在生效。

需要特权工作的 action 调用已有 `nkdhrd` 方法。未来 CTRL-EXT 自定义命令仍由
polkit 与 schema 控制；注册 UI action 本身不会授予权限。

## 画布集成

pinned 组件沿用 COMP-7 的稳定宿主概念：身份、world rectangle、显式层级、与
renderer 无关的 render payload 和 node-local 输入。扩展通过 `nkdhr-ui` canvas
adapter 工作，不能仅为接触原始 GLES 而实现后端专用 `PinnedNode`。

UI-5 的具体 adapter 是包裹 object-safe `UiSurface` 的 `UiPinnedNode`。可复用应用
实现或组合 surface 边界，让宿主持有 placement、scale、输入规范化和绑定 GLES
context 的资源。display-list command 或引用 texture revision 改变时必须推进
surface commit，否则宿主可复用上一 prepared frame。扩展不能跨 GLES context
缓存 `PreparedDisplayList`，也不能在 retained tree 内重复应用画布 world transform。

后续第三方 pinned-widget loader 会把版本化边界适配到进程内 registry；它有意
推迟到首批 SHELL-5 组件明确真实生命周期和权限需求之后。

## 兼容性

Rust crate 在一个发布系列内遵守源码兼容，但不承诺稳定 Rust ABI。持久化扩展
配置与 action 名称是版本化数据契约；删除或变更它们必须有迁移路径和弃用期。

每项面向扩展的新增能力必须包含：

1. 公共 API 文档和一个最小示例；
2. 确定性布局/输入测试；
3. 新视觉的 golden 覆盖；
4. 适用时在 canvas 与 standalone 两种宿主中验证；
5. 资源限制与 teardown 行为说明。
