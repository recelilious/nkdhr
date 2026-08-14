# Canvas — 内部实现

> [English version / 英文版本](INTERNALS.md)

本文面向参与 `nkdhr-canvas` 开发的贡献者，覆盖 2026-08-08 验收通过的
Phase 2 实现。文中引用 API 的事实依据是直接检查 Smithay 0.7.0 源码，尤其是
`examples/minimal.rs`，而不是凭借对旧版 Smithay 的记忆；这些 API 随版本发生过
显著变化。

## 本文范围与非目标

本文覆盖合成器本身的架构（COMP-1 … COMP-8），不深入介绍二维图元层
（`nkdhr-render`，UI-1）或组件工具包（`nkdhr-ui`，UI-3）。它们属于 Phase 3，
按照 ROADMAP 的顺序在 Phase 2 验收之后构建。COMP-1 在它们出现前需要输出像素时，
直接使用 Smithay 的 `GlesRenderer`（清屏并交换缓冲）；这不是日后要删除的临时实现，
而是 COMP-1 为验证嵌套窗口生命周期、缩放与无泄漏所需的恰当范围。COMP-7 则建立
画布的场景节点宿主接口，供 Phase 3 出现后的 `nkdhr-ui` 接入。

## crate 布局

```
crates/nkdhr-canvas/src/
  main.rs           入口：选择 winit（嵌套）或 TTY 后端，运行事件循环
  backends/
    winit.rs         COMP-1/2：嵌套后端（smithay::backend::winit）
    tty.rs           COMP-5：DRM/KMS + GBM + libinput + libseat 后端
  canvas/
    world.rs         COMP-3：世界坐标窗口模型
    viewport.rs      COMP-4：平移/缩放、总览模式、位置标记
    output_group.rs  COMP-4/5：输出与画布绑定（ROADMAP §2.3）
  input.rs           COMP-1..4：输入分发与交互几何
  settings.rs        COMP-3：可热重载按键和网格策略
  protocols/         COMP-6：剪贴板、DnD、截图、会话锁、XWayland 等
  widget_host.rs     COMP-7：供 nkdhr-ui 渲染的场景节点接口
```

两个后端（`winit`、`tty`）实现同一个小型内部 trait。渲染循环、损伤追踪和输入传递
属于后端，`backends/` 以上的世界模型、协议处理器和输入分发则完全共用且不感知后端。
所以 COMP-5（TTY）是在 COMP-1（嵌套）之上的增量，而非重写；`canvas/`、
`protocols/` 和面向 shell 的代码无需知道当前后端。

## COMP-1：嵌套骨架

`smithay::backend::winit` 提供嵌套后端：合成器作为客户端窗口运行在开发者现有的
X11 或 Wayland 桌面中，由 winit 自动检测并选择。`smithay` 依赖设置
`default-features = false, features = ["backend_winit"]` 时，只引入 winit、
`backend_egl`、`wayland-client`、`wayland-cursor`、`wayland-egl` 和
`renderer_gl`，没有 `wayland_frontend`、`desktop`、DRM/GBM/libinput/libseat。
COMP-1 没有自己的 Wayland 客户端（那是 COMP-2）且不运行在裸 TTY（那是 COMP-5），
因此还无需 `Display` 或 seat。

初始化与渲染循环依据 Smithay 0.7.0 的 `examples/minimal.rs` 核对，并只保留窗口与
渲染部分；示例中的 xdg-shell/seat 属于 COMP-2：

```rust
let (mut backend, mut winit) = smithay::backend::winit::init_from_attributes::<GlesRenderer>(
    WinitWindow::default_attributes().with_title("nkdhr-canvas"),
)?;

'main: loop {
    let mut should_exit = false;
    let status = winit.dispatch_new_events(|event| match event {
        WinitEvent::CloseRequested => should_exit = true,
        WinitEvent::Resized { size, .. } => { /* 记录；下一帧按新大小重新 bind */ }
        WinitEvent::Input(event) => { /* 记录 */ }
        WinitEvent::Focus(_) | WinitEvent::Redraw => {}
    });
    if should_exit || matches!(status, PumpStatus::Exit(_)) {
        break 'main;
    }

    let size = backend.window_size();
    let damage = Rectangle::from_size(size);
    let (renderer, mut framebuffer) = backend.bind()?;
    let mut frame = renderer.render(&mut framebuffer, size, Transform::Flipped180)?;
    frame.clear(CANVAS_BACKGROUND, &[damage])?;
    frame.finish()?;
    backend.submit(Some(&[damage]))?;
}
```

仅看示例不容易发现的要点：

- **`WinitEvent::CloseRequested` 不会自行结束循环。** Smithay 的 `minimal.rs`
  完全没有处理它；窗口关闭后进程会继续运行。nkdhr-canvas 显式设置 `should_exit`
  并跳出自身循环。`PumpStatus::Exit` 是 winit 事件循环结束的另一种信号，不会因为
  普通关窗自动发生。这里实现 COMP-1 的“嵌套模式干净退出”标准。
- **缩放除记录外无需显式处理。** `backend.bind()` 每帧重新读取 `window_size()`，
  并在变化时自行缩放 `EGLSurface`；COMP-1 无需保存大小或手动缩放。世界视口对大小
  的响应属于 COMP-4。
- **`Transform::Flipped180`** 与 `minimal.rs` 一致，用于匹配 GL 左下原点 framebuffer
  与 Smithay 左上原点逻辑坐标。
- COMP-1 尚无 `Seat`/`SeatState` 或键盘焦点，因为没有客户端。输入直接从
  `WinitEvent::Input` 的 `InputEvent<WinitInput>` payload 记录；COMP-2 有客户端后
  才引入真实 `Seat`。
- 十分钟泄漏验证在关闭 `RUST_LOG` 后运行上述循环，并从外部定期采样
  `/proc/<pid>/status` 的 `VmRSS`，不涉及 Smithay API。
- 此循环没有 frame pacing：默认 `GlAttributes` 的 `vsync: false`，也不等待
  `wl_surface.frame()` callback，因此以宿主允许的速度渲染和 swap，在嵌套模式下表现为
  持续高 CPU。这符合 COMP-1 只验证窗口/渲染/输入生命周期的狭窄范围；COMP-4/5
  在出现有意义的真实内容后用正式策略替代它。底层实现无需删除，只需叠加 pacing 策略。

**实机验证记录**：在临时 Weston `--backend=headless --renderer=pixman` 宿主中
正常启动、通过 Wayland 连接、创建 1280×800 窗口，并在整段测试内无错误逐帧渲染。
十分钟内每 30 秒采样的 `VmRSS` 保持不变（数值见 PROGRESS.md 的 COMP-1 条目）。
该无显示头环境无法真实拖拽窗口边缘或点击关闭，因此未触发交互式 resize 和
`CloseRequested`。不过首次配置 resize 使用相同 `WinitEvent::Resized` 路径并已记录，
`bind()` 源码也确认每帧无条件重查大小；关窗分支则是经源码检查的直接三行处理。

该验证走的是**软件渲染而非目标 Iris Xe 硬件路径**。Weston headless 的 pixman
输出没有通过 Wayland 宣告真实 DRM 设备，Mesa 因此退回 llvmpipe，而非直接打开
`/dev/dri/renderD128`。对作为任意宿主之上开发模式的嵌套后端，这是预期行为；
真实 Iris Xe 硬件加速由直接打开 DRM render node 的 TTY 后端（COMP-5）验证。

## COMP-2：Wayland 客户端

COMP-2 增加真实 `wayland_server::Display` 和 `ListeningSocket::bind_auto` socket
（使用首个可用的 `wayland-0`、`wayland-1` 等，而非示例硬编码的 `wayland-5`）。
代码拆成 `main.rs`（入口、后端/事件循环、渲染、socket accept 循环，会随里程碑变化）
和 `state.rs`（`App`/`ClientState` 及协议处理 trait，实现主要是增长）。这是实际内容
足够后再拆分，和 CTRL-1 到 CTRL-2 的时机相同。

`App` 持有 COMP-2 所需的五类 `wayland_frontend` 状态：
`CompositorState`（`wl_compositor`/`wl_subcompositor`、buffer commit）、
`ShmState`、`DmabufState`、`XdgShellState`（`xdg_wm_base`，含 toplevel/popup）及
`SeatState` 和一个 `Seat`（键盘、指针、触屏）。UI-6 已将完整的触屏 down/motion/up/
frame/cancel 序列交给 Smithay；在空白画布/边缘识别器完成前，触屏合成器手势会显示为
不支持。
`DataDeviceState`（剪贴板/DnD）也按 `minimal.rs` 接好。即使 COMP-2 的验证表未覆盖，
省略它会导致 COMP-6 先删除再添加处理器；所需四个 selection/DnD handler 只是少量
样板，并非新子系统。

**不止 `wl_shm`，还提供 dmabuf。** GPU 客户端需要它避免每帧 CPU 拷贝。
这里用较简单的 `DmabufState::create_global`：协议版本 3，格式来自
`renderer.dmabuf_formats()`。未使用 version 4 的 default feedback，因为它需要从
真实 DRM 设备构建 `main_device: libc::dev_t`，而嵌套 winit 环境的查询在 COMP-1
已失败；其多 GPU 提示对单 GPU 目标也无意义。`DmabufHandler::dmabuf_imported`
只创建 `wl_buffer`（`notifier.successful::<App>()`），真正 GPU import 在 buffer
首次成为渲染元素时由当帧 renderer 延迟完成。`App` 本身不持有 renderer。

**此时有意不使用 `desktop::Window`/`Space`。** Smithay `desktop` 模块的价值是管理
有位置的窗口，COMP-2 尚无位置模型（COMP-3 才引入世界坐标）。这里直接跟随
`minimal.rs`：遍历 `XdgShellState::toplevel_surfaces()`，用
`render_elements_from_surface_tree` 把每个 surface 渲染到固定 `(0,0)`；
尚无布局，所以全部客户端重叠。等 COMP-3 真正需要每窗口世界位置时再采用抽象。

**焦点暂为“最新映射的 toplevel 获胜”，并非真正点击聚焦。**
`XdgShellHandler::new_toplevel` 立即对新 surface 调用 `keyboard.set_focus`，仅用于满足
COMP-2 单客户端测试的“可接收输入”。`state.rs` 明确标注这是待替换策略。
同理，指针 motion/button 暂时发给键盘焦点 surface；没有位置模型时也无其他目标。

renderer 始终由 `main.rs` 的 `WinitGraphicsBackend` 持有，不移进 `App`。协议处理
（commit、dmabuf imported 等）无需直接操作像素，只有渲染循环需要。让 `App`
与 renderer 无关，也使 COMP-5 的 TTY 后端保持增量式：`state.rs` 不关心谁提供 renderer。

**真实测试发现必须提供 `wl_output` global。** ROADMAP 和首稿都未预计，但没有
output global 时，`foot` 会报 `no monitors available` 而完全不渲染。许多客户端在
创建 surface 前会查询 `wl_output`。创建一个与嵌套窗口同大小的 Smithay `Output`，
通过 `Output::create_global` 公布并实现空的 `OutputHandler` 后解决。真正的逐输出模型、
缩放传播、多输出及输出组绑定属于 COMP-4/5；这里仅足以解除普通客户端协议阻塞。

**实机验证记录**：临时 headless Weston 宿主中，`weston-simple-shm`、
`weston-simple-egl` 和 `weston-simple-dmabuf-egl` 均成功连接运行，后者证明 dmabuf
global 可协商。修复 `wl_output` 后，`foot` 和 `gtk4-demo` 也渲染真实内容；五个客户端
（包括三个并发）均可退出且不使合成器崩溃或卡死。无输入设备的 headless 环境无法
验证真实键入和点击切换焦点；输入转发代码已直接按 Smithay 正式 API 编译通过，
但该端到端缺口留到物理 TTY 验证。

## COMP-3：画布模型

这是项目的核心新设计。一张**画布**是在 `f64` 世界坐标中的无界二维平面，使用
`Point<f64, World>`（`nkdhr-canvas/src/world.rs` 中区别于 Smithay `Logical`/
`Physical` 的 marker 类型）。同文件中的 `Canvas` 按堆叠顺序保存所有已映射窗口
`ManagedWindow { surface, position }`。放置不受边界和平铺限制：窗口可以重叠、
使用负坐标或相隔任意远。

`GridSettings` 默认把交互几何对齐到以世界 `(0,0)` 为基准的网格：
`snap_to_grid = true`、`grid_size = 32`。关闭后恢复完全自由放置。网格策略不改变
世界模型，也不移动固定节点；它还会在导航结束后对齐 COMP-4 工作视图的画布锚点，
手势进行中视口始终连续。窗口**大小**按需从 Smithay 已 commit 的 buffer-size 状态
（`with_renderer_surface_state`）读取而不另存，因此“实际提交的内容大小”是唯一真值，
不会与渲染尺寸漂移。

这里同样不使用 `desktop::Window`/`Space`；`Canvas` 是 nkdhr 为 map/unmap、hit-test、
raise 和 cycle 定制的小模型，不是 Smithay desktop 的包装。

- **默认放置**：新窗口在最近 10 个窗口位置上级联。默认网格下从 `(96,96)` 开始，
  每个窗口移动一个网格间距，十个后循环。关闭吸附时保留旧的 `(100,100)`、
  `(140,140)` 等级联。这只是合理初始点，不是布局约束。
- **移动**：在窗口任意位置按 `super+左键拖动`，按指针位移连续更新世界位置。
  启用网格时，松开后由合成器短动画把内容左上角缓动到最近交点。其他窗口不会重排。
  物理测试否定了拖动过程中实时跳格，因为那会让窗口在格子间跳动、视觉上脱离指针。
- **缩放**：`super+右键拖动` 根据位移计算新逻辑尺寸，只把主动拖动的边或角对齐
  网格，对侧固定，并请求对应大小的 `xdg_toplevel` configure。客户端下一次 commit
  提供实际内容，`ManagedWindow::size` 会实时读取；合成器不假定客户端严格接受请求。
- **焦点**：恰好一个窗口（或无窗口）拥有键盘焦点。普通左键点击会聚焦并将窗口移到
  `Canvas` 堆叠顺序最末（hit-test 和渲染均视末项为最顶层），同时点击仍正常传给客户端，
  因而单击后台窗口会切换窗口并激活所点内容。`cycle_focus`（默认 Alt+Tab）不依赖
  指针位置。修饰键移动/缩放不实现 Smithay `PointerGrab`：后者用于协议可见 grab，
  而窗口管理器级手势在事件发往 seat 前就由合成器自己识别，无协议对象参与。
- **交互设置**：UI-6 新增有 1 MiB 上限的标量 `canvas.bindings`，其 schema-v1 JSON
  由 `nkdhr-ui` 编译为键盘/按钮/手势触发器及类型化 action 调用。空值选择完整标准
  文档，并把三个旧按键叶子作为迁移输入；非空文档具有最高权威。`nkdhrd` 只负责
  标量大小边界；领域校验、冲突分析、设备/能力可用性、最后有效代次和结构化诊断均
  属于合成器/共享 UI 编译器。`canvas.snap_to_grid` 与 `canvas.grid_size` 仍是普通
  类型化叶子。watcher 原子发布整个候选；`input.rs` 只规范化事件并查询编译结果，
  `actions.rs` 集中把稳定 action ID 映射到画布操作。任何配置值都不会执行代码。

**实机验证记录**：在临时 headless Weston 与运行中的 `nkdhrd` 下同时启动 12 个
`weston-simple-shm` 客户端，它们按设计落到 10 个不同世界位置并循环，无崩溃。
运行中执行 `nkdhrctl config set canvas.close_window w`，约一秒内出现按键重载日志。
空值在 CTRL-5 被拒绝并保留最后有效值；语法合法但不存在的键名通过存储校验后由
合成器捕获，记录警告并恢复内置默认。headless 环境当时无法物理验证 Super 拖动、
点击聚焦或真实快捷键；相关代码按 Smithay API 编译并通过借用检查，D-Bus/配置半边
则已完整端到端验证。后续物理 TTY 验收覆盖了这些交互。

## COMP-4：视口

`Viewport`（`canvas/world.rs`）是一台观察画布的相机，含世界空间中心点和缩放因子；
`Viewport::WORK` 是原点与 1:1。锚点是 COMP-5 输出布局提供的组内逻辑坐标，默认
为主显示器中心。因此 `Viewport::center` 表示“显示在画布锚点的世界点”，未必是宽
多屏包围矩形的中心。`to_group_logical`、`group_logical_to_world`、
`to_world_delta` 是世界与组坐标转换的唯一位置；之后由后端应用各输出的组偏移与
物理 scale。COMP-3 的固定原点/无缩放占位逻辑已经移除。COMP-4 最初在 `App`
保存一个视图，COMP-5 将同一状态移到每个输出组的 `GroupView`，数学关系不变。

每个 `GroupView` 用 `in_overview` 与 `pre_overview_viewport` 保存两种状态；
两个字段比引入第三个“转换中”状态的正式状态机更简单：

- **工作状态**（默认）：缩放固定 1:1，平移直接改变 `viewport.center` 的世界偏移。
  三条输入路径与 ROADMAP 的“键盘/指针/触控板手势”完全对应：
  - 指针：从空白画布开始的普通左键拖动成为 `Drag::Pan`，复用 COMP-3 的
    `input.rs` 拖动机制，而非 `PointerGrab`。合成器拥有的移动/缩放/平移在每次
    motion 仍会更新 Smithay `PointerHandle` 位置并使用 `focus = None`。相对输入后端
    从该位置计算下一点；若只改视口不更新指针，每个事件都会从原按下点起算，
    触控板拖动看起来就会冻结。
  - 触控板：类型化默认映射把恰好三指滑动注册为 `canvas.viewport.pan`，把恰好
    三指捏合注册为 `canvas.viewport.pinch`。滑动把 delta 转为 `viewport.center`
    移动；捏合在改变 zoom 时，让开始时的世界锚点保持在移动中的逻辑中心下。双指滚动仍是普通
    `InputEvent::PointerAxis`，在固定节点优先处理后原样发给当前指针焦点客户端，
    等同鼠标滚轮。早期把所有 axis 当成画布平移，导致 GTK 列表无法滚动，首次真实
    TTY 测试后被否决。其他未绑定指头数量通过标准 pointer-gestures 协议保留给应用。
    嵌套 winit 把原生 gesture 类型视为 `UnusedEvent`，因此三指画布平移是 TTY
    功能；普通应用滚动两个后端都支持。
  - 键盘：`super+方向键` 或标准 Vim H/J/K/L 每次移动固定 `PAN_STEP` 世界单位，并使用短暂 ease-out。
    新按键从当前显示视口开始，却把步长叠加到前一个动画目标，连续快速输入会合并，
    不丢距离也不闪过断裂位置。不能使用裸方向键，因为焦点客户端的文本光标等需要它；
    Smithay 普通键盘 repeat 会产生重复按下，无需合成器计时器。

  开启 `snap_to_grid` 时，指针/三指平移到松开前保持连续，然后用短动画把锚点世界点
  对齐到最近网格交点。键盘目标、标记和总览退出复用同一工作状态对齐目标。关闭网格
  后保留精确视口坐标。总览相机在 fit 或查看时是临时自由视角，不量化。工作状态下
  窗口绝不缩放，符合清晰度策略：长期使用为 1:1，仅总览接受缩小模糊。
- **总览状态**（临时）：通过 `super+overview` 进入/退出，默认键 `o`，由 CTRL-5
  支持；`Esc` 和点击空白也退出。`Viewport::fit_group` 根据所有窗口合并后的
  `Canvas::bounding_rect` 加固定 1.25 倍边距，计算最多 1:1、只缩小不放大的 zoom，
  并动画到达。点击窗口会动画到该窗口中心的 1:1，退出总览并聚焦、置顶。指针在
  总览中不发往任何客户端，直到选择窗口或退出。

**动画转换**：`world::Animation` 保存起止 viewport、`Instant`、`Duration` 与
ease-out-cubic。每次渲染循环在绘制前调用 `advance_animations`，推进所有组的可选动画，
覆盖总览进入/退出/取消和标记跳转，无需提前建立通用动画引擎。

**位置标记**（ROADMAP §2.3）：内存中的 `canvas::marks::Marks` 是普通
`HashMap<u8, Point<f64, World>>`，但在 CTRL-5 中作为单个字符串 `canvas.marks`
持久化，而不是嵌套表。当前 `Config1.Get`/`Set` 只支持标量叶子，且 `Set` 只能覆盖
已经存在的叶子；HashMap 形命名空间无法首次创建一个标记。由 `nkdhr-canvas` 内的
`marks::parse`/`format` 编解码整个集合，并配套单元测试，避免为一个命名空间改变
CTRL-5 引擎。COMP-5 按画布隔离，格式为
`v2;<hex-canvas>:<digit>:<x>,<y>;...`，十六进制让任意 UTF-8 画布名无歧义；
旧 `<digit>:<x>,<y>;...` 格式仍加载到 `default` 画布。
`super+alt+shift+<digit>` 保存当前中心并立即写入，`super+alt+<digit>` 动画跳转并在需要时
退出总览。数字按键使用原始未 shift 的 level（`KeysymHandle::raw_syms()`），
所以 `super+alt+shift+3` 仍指“标记 3”。

**编号工作区**位于第一等画布之上。`WorkspaceAssignments` 为每个正整数维护唯一的
全局归属，并为稳定输出组名称维护一个本地活动编号。新输出组取得最小未占用编号；
请求未显示编号会把它附着到当前组，请求已在另一组显示的编号会交换两个完整
`GroupView`。非活动视图保留画布、视口和焦点。`WorkspaceFade` 只在 300 ms
smoothstep 过渡期间保留旧画布/视口；输入立即指向新视图，两个后端以互补透明度按
前后顺序绘制两套窗口。`super+1…9/0` 对应工作区 1…10。

`canvas::placement` 是移动现有窗口与后续 launcher 新窗口共用的模态契约。
`PlacementGeometry` 会把当前物理显示器中心、指针点及八个边缘区域经目标 viewport
转换为世界坐标。相对摆放在未移动轴上保持中心对齐，在方向轴上保留网格大小的间距，
并且绝不裁切。`HeldDirections` 使用集合而非“最后按键”，因此左+右会抵消，松开任一
方向会立即显露另一方向。`PlacementSession` 在全部方向键清空后等待 110 ms 再提交，
期间新按键会取消截止时间。输入全程归合成器模态所有，终止键/鼠标的尾随 release
会被抑制；取消时先交换回来源工作区，再把同一 `ManagedWindow` 放回精确原坐标。
两个渲染后端都会在普通工作区内容上方绘制来自真实目标 rect 的半透明轮廓。

Phase 2 没有为“用键盘移动焦点窗口”分配快捷键。它被记录为 Phase 3 工具包出现后，
与完整快捷键、可发现性、设置和视觉反馈一同设计的候选项，再由 Phase 4 确定 shell
交互语言。画布已有位置与动画机制，因此延后不会要求重写合成器。

**实机验证记录**：临时 Weston/Pixman 宿主中，10 个并发 SHM 客户端在多个五秒窗口
保持 **57.5–57.7 fps（约 17.4ms/frame）**。这是软件 llvmpipe 的嵌套结果，
不能代表 Iris Xe 性能。标记则完整端到端验证：运行中通过 `nkdhrctl` 设置两个标记，
杀死并重启后日志显示 `loaded 2 saved mark(s)`；解析/格式化测试还覆盖往返、空串与
忽略坏条目。`canvas.overview` 也可通过 CTRL-5 往返。当时 headless 环境仍无法
产生真实指针/键盘事件；物理 TTY 回归后来验证了三种平移、总览、标记和正确输入路由。

## COMP-5：TTY 后端

可执行文件通过一个小型 `Backend` trait 提供两个永久后端。`--nested` 选择 winit
开发后端，`--tty` 选择真实后端。不传参数时，存在 `WAYLAND_DISPLAY` 或 `DISPLAY`
就使用 nested，否则使用 TTY。Cargo feature 使部署保持精简：`nested` 启用
`backend_winit`，`tty` 启用 GBM、udev、libinput、libseat、GLES 和 Smithay
multi-GPU renderer。两个后端都不是临时脚手架，协议/世界/输入层完全共用。

TTY 事件循环基于 calloop。`LibSeatSession` 管理 KMS 设备访问及暂停/恢复；
`UdevBackend` 枚举、热插拔 DRM card；`DrmScanner` 把 connector 映射到 CRTC；
`DrmOutputManager` 持有 GBM 支持的 atomic KMS surface。主 GPU 的 render node
直接打开并注册到 `GpuManager<GbmGlesBackend<...>>`；render node 不能成为 DRM
master，因此不能接管显示器。KMS primary node 独立打开，且只在其 connector 可用于
scanout 时打开。渲染在主 render node 上进行，scanout 设备有自身 render node 时
跨 GPU 拷贝；只有 scanout 的设备（包括 VKMS）借用主 render-node allocator，格式
限制为 linear modifier。没有引入 setuid helper 或 nkdhr 自有特权代码。

图形 VT 不会自动收到内核 console 切换。在 TTY 后端，`Ctrl+Alt+F1` 至
`Ctrl+Alt+F12` 会在客户端传递前被拦截并转换为 `LibSeatSession::change_vt`。
XKB 可能直接把组合解析为 `XF86_Switch_VT_n` keysym，所以分发既接受专用 keysym，
也在单独追踪 Ctrl+Alt 时检查未修饰 level-zero 功能键。第一次物理测试只检查
`modified_sym()`，因此快捷键静默失效，后据此修复。`App` 只携带一次性后端控制请求，
而不是 libseat handle，使共享输入层不依赖会话库，嵌套后端也不占用这些组合。
锁屏期间该绑定仍可用：切到另一个已认证 VT 不会解锁或暴露当前合成器会话。

切出 VT 前，后端先停止渲染，执行一次全设备 atomic reset 关闭所有 connector 和
plane，然后暂停每个 DRM output manager 并释放 master；完成后才请求 libseat
切换，避免 logind 先撤销设备权限、reset 随后失败的竞态。若切换请求失败，后端会
立即重新激活并扫描设备；异步 pause 事件以幂等方式重复暂停并挂起 libinput。
重新激活时恢复 libinput，带 connector/plane reset 激活 DRM，重置输出 buffer，
再扫描 connector，而不丢弃客户端、画布状态或焦点。

`canvas/output_group.rs` 把持久化的 `canvas.outputs` map 与当前 connector 名称解析。
配置坐标使用逻辑单位并归一化，支持负位置；物理 mode 大小除以正的分数 `scale`
得到每个输出的逻辑范围。为 `wl_output` 和 libinput 路由，组被确定性地放进一个
互不重叠的合成器全局坐标空间；各画布之间仍无空间关系。没有配置时，所有已连接
输出组成横向 `default` 组并绑定 `default` 画布。存在显式组后，未提及的热插拔
connector 会变成 `auto:<connector>` 单输出组/画布，所以不会空白。

每个解析后的组有一个组内逻辑 `canvas_anchor`。配置了
`CanvasOutputGroup::primary` 时取主显示器中心，否则取稳定 connector 名称顺序的
首个已连接成员中心；单输出组自然无歧义。schema 校验非空 `primary` 必须属于该组。
渲染和 hit-test 的所有世界/组转换都使用此锚点，所以 `Viewport::WORK` 中世界
`(0,0)` 显示在主显示器中心；增加显示器不会把原点悄悄改到组合矩形中心。

`App` 按画布名持有一等世界对象，按组名持有组视图状态。一个组视图恰有一个画布
绑定、viewport、总览状态和动画。同组每个输出的 render pass 读取同一 viewport，
将世界坐标转换到组的刚性逻辑矩形，减去该输出在组中的位置，最后应用该输出 scale。
不同组可独立平移、总览和接收新窗口；两个组也可以绑定同一画布而保留独立视图。
布局重整会保存已断开组/画布状态，拔插显示器不会丢掉其世界。位置标记使用已有
`canvas.marks` 标量，按画布存储且兼容旧单画布编码。

Wayland seat 的指针坐标保持合成器全局，但 hit-test 先寻找指针所在物理输出，选择其组，
减去组的 packed origin，再使用该组 viewport 到达世界坐标。跨入另一输出组会激活该组；
拖动始终绑定开始时的组。键盘操作作用于 active 组。

`DrmOutput::render_frame` 提供 Smithay 逐输出 damage history。只有非空帧排队；KMS
vblank 通过 `frame_submitted` 完成，Wayland frame callback 只发给实际在该画布上
呈现的窗口。未变化输出无需全重绘，热插拔和配置变化仍复用重整路径。每个输出最多
允许一个已渲染 KMS 帧在途；等待 vblank 时的输入与场景变化会合并到合成器状态，
`frame_submitted` 后第一帧呈现最新状态，避免高频输入积累过时位置并使指针/拖动物体
与显示同步。seat pause/activation 会清除合成器的 in-flight 标志以匹配 DRM manager
重置。成功激活后还对每个输出调用 `DrmOutput::reset_buffers`；新 swapchain slot 的
buffer age 为零，强制恢复后的第一帧全屏重绘。保留 pause 前 age 曾使第一次物理 VT
恢复只显示一块局部损伤，直到下一次输入才正常。

物理 connector 移除时，在丢弃 `DrmOutput` 前显式清除 KMS surface（DPMS off 并禁用
所有 plane），其余输出随后尽量恢复显式 buffer modifier。重连因此从干净 CRTC 开始，
不会继承旧 framebuffer/plane assignment。当前 SDR 合成器优先 8-bit ABGR/ARGB
scanout，10-bit 仅作 fallback；在没有 HDR/色彩管理策略前优先 10-bit 会引入无意义
dithering 和驱动差异，所以普通 SDR 路径保持默认。

双输出 VKMS 诊断在十秒空闲窗口测得 render-engine 增量为零、单核 CPU 占用 0.10%。
随后笔记本面板 active 本地 VT 测试在 TTY1/TTY2 往返，真实覆盖 libseat pause/resume：
客户端、焦点和画布状态保留，输入恢复，buffer-reset 修复无需额外输入就立即重绘全屏。
最终真实 eDP + HDMI 的 COMP-5 验收通过了单刚性组、两个独立组、实时配置变化、保留
客户端的物理拔插及双输出 VT 双向切换。长会话 damage/idle 观察归 COMP-8。

仅为安全开发，`NKDHR_DRM_DEVICE` 可覆盖主 render node，
`NKDHR_DRM_SCANOUT_DEVICE` 构成硬 KMS 设备边界：设置后不会打开任何其他 primary
DRM node，而非仅从 connector 扫描中排除。render override 必须是 render node，
scanout override 必须是 primary node。这保证 VKMS 诊断能使用真实 GPU 渲染，而不
获取真实面板的 DRM master 或改变 mode。生产环境正常发现 render GPU 并经 seat 打开
KMS 设备。合成器通常必须运行在本地 seat session；`LIBSEAT_BACKEND=noop` 只允许
隔离 VKMS 诊断，不能作为生产启动模式。

### 可复现的双输出 VKMS 实验

`crates/nkdhr-canvas/tools/vkms-lab.sh` 是永久开发工具，用于 COMP-5 不依赖硬件的
回归。它使用内核 VKMS configfs ABI 创建一个含两条完整 pipeline 的虚拟 DRM 设备
（每个 connector 有独立 encoder、CRTC 和 primary plane），两路可同时 scanout。
它走与物理显示器相同的 udev、connector scanner、atomic KMS、GBM、输出组和 damage
路径，不是合成器内部的假输出后端。

命令范围有意收窄并保证幂等安全：

- `setup` 只创建 `/sys/kernel/config/vkms/nkdhr-lab`，拒绝覆盖已有实例；构建失败时
  只回滚该实例；
- `connect <0|1>`/`disconnect <0|1>` 修改选定 connector 的 configfs 状态，
  由内核发出 udev 消费的 hotplug 事件；
- `show` 无需 root，报告实验实例与已暴露 DRM connector 状态；
- `audit <pid>` 检查合成器 `/proc/<pid>/fd` 链接，若发现不属于 VKMS 实验的 open
  primary DRM node 则失败；
- `teardown` 只禁用并删除 `nkdhr-lab`，用显式 unlink/rmdir 而非递归删除；故意不
  卸载 `vkms` 模块，因为可能还有其他实验实例。

修改操作需要 root，因为 configfs 表示活的内核对象；生产环境不会调用此工具。
VKMS 可证明合成器逻辑与内核 API 集成，但无法证明物理链路训练、EDID 特例、本地
logind seat handoff 或目标面板真实 mode，这些仍是物理验收项。

测试输出前必须审计合成器已打开的文件描述符：可以包含所选 VKMS primary node 与
真实 GPU render node，但不得包含任何被排除的真实 GPU primary node。这是硬边界的
可执行安全检查；只看 connector 日志不够，因为打开 primary node 可能在扫描前就
获得 DRM master。

## COMP-6：协议长尾

`protocols/` 保存两个后端共用的 global 及合成器策略。Smithay 负责多数协议对象
生命周期，但注册 global 并不等于功能完成：selection 必须跟随键盘焦点，pointer
constraint 必须改变 motion 传递，锁屏 surface 必须替换普通场景，screencopy 也必须
从最终合成输出回读。各协议策略如下：

- **剪贴板与 primary selection**：COMP-2 已为 DnD 接入
  `wayland::selection::data_device`；COMP-6 让剪贴板焦点跟随键盘焦点，并加入
  `wayland::selection::primary_selection`。selection 字节仍经协议 pipe 在提供方和
  接收方之间直接传输；合成器只保存元数据并转交 fd，不复制一份内容。
- **拖放**：`data_device` 状态及 `ClientDndGrabHandler`/`ServerDndGrabHandler`
  与真实指针焦点路径共用。DnD 目标是画布指针下最顶层 surface，跨输出组转换也一样。
  可选客户端 DnD 图标绘制在指针下方，并从同一后端无关渲染路径获得 frame callback。
- **服务端装饰策略**：`xdg-decoration-unstable-v1` 始终配置为 `ServerSide`，包括
  客户端请求或取消 CSD 时。合成器在同一画布 render list 中绘制边框/标题区域，统一
  damage、堆叠和 viewport 转换。没有绑定协商协议的客户端按 xdg-shell 要求继续 CSD。
- **截图**：nkdhr 通过 Smithay 的 `wayland_protocols_wlr` re-export 直接实现
  `wlr-screencopy-unstable-v1` 服务端（Smithay 0.7 有 binding，没有现成 state）。
  请求按所引用 `wl_output` 校验并排队，从下一张完整合成帧完成，在检查 stride、format、
  pool 边界后复制到客户端 XRGB8888 SHM buffer。请求是否含 `overlay_cursor` 会被分到
  连续不同帧，确保光标策略准确。锁定输出只暴露黑色/锁屏合成，绝不暴露被遮画布。
  嵌套 GLES 回读有一个关键修复：映射 texture 后 EGL current 不再绑定 winit window
  surface，swap 前需做一次无操作 render bind，否则第一次真实 `grim` 会产生 EGL
  `BadAlloc` 并使合成器退出。
- **`ext-session-lock`**：`wayland::session_lock` 收到请求即进入 locking，停止普通
  surface 输入，只渲染黑色或逐输出 lock surface。所有连接输出各呈现一帧保护内容后
  才发送 `locked`。锁屏客户端退出或崩溃仍保持保护，只有有效
  `unlock_and_destroy` 恢复此前组焦点。真实 PAM UI 属于 SESS-3。
- **指针限制**：`wayland::pointer_constraints` 只在对应 surface 有指针焦点时激活。
  locked pointer 获得 relative motion 而合成器光标不动；confined pointer 被限制在
  surface 或请求区域。两个后端都渲染客户端 cursor surface（含 hotspot）或内置 RGBA
  箭头；隐藏 winit 宿主光标避免双重显示。TTY 普通帧只允许 DRM cursor-plane scanout，
  不允许 primary/overlay direct scanout，既消除软件光标拖影又保留合成 primary plane。
  有待处理截图时该帧禁用 cursor plane，使 `overlay_cursor` 回读仍包含 framebuffer 光标。
- **空闲抑制**：`wayland::idle_inhibit` 跟踪活着且可见的 inhibitor surface。
  `App::idle_inhibited()` 是未来会话 idle/DPMS 路径唯一策略查询；死亡、未映射或隐藏
  surface 不会让会话常亮。
- **分数/整数缩放**：逐输出 scale 来自 COMP-5 同一 CTRL-5 输出布局配置。合成器向
  当前显示每个 toplevel 的输出发送 `wp-fractional-scale-v1.preferred_scale`；未绑定
  分数协议的客户端以 `wl_output` 向上取整整数 scale 为 fallback。合成器自有内存
  element 使用原生 image rect 作为采样源和按输出 scale 调整的物理目标大小，因此
  fallback 指针在 2x 输出上不会缩成逻辑尺寸的一半。
- **XWayland——解决 ROADMAP §8 的开放问题**：采用 Smithay 自带
  `smithay::xwayland`（`XWayland` + `X11Wm`/`XwmHandler`）的**进程内**方案，
  而非外部 `xwayland-satellite` 类代理。合成器直接扮演 X11 WM，无需增加 Xwayland
  服务器之外的进程和 IPC 面。COMP-6 把 `ManagedWindow` 从仅 XDG 的
  `ToplevelSurface` 改成统一 xdg-shell/X11 的 Smithay `desktop::Window`，所以 COMP-3
  的位置、焦点、移动/缩放模型无需并行实现。Wayland/X11 剪贴板和 primary selection
  双向桥接；Smithay 0.7 排队新 X11 selection owner 后，以无害 RANDR reply 往返作为
  flush barrier。清理后的 Xwayland 子进程环境显式保留 `LD_LIBRARY_PATH`，让 Nix/
  非系统运行时和解包诊断仍可工作。外部代理的崩溃隔离和懒启动收益，不足以抵消这个
  从头开发、整体审计的单进程合成器增加的 IPC 面。

真实客户端还发现了 ROADMAP 简表未点名的两个兼容部分：
`OutputManagerState::new_with_xdg_output` 提供逻辑输出几何，否则 `grim` 会把嵌套
输出推断为零大小；`PopupManager` 跟踪、渲染、hit-test、grab 并重新定位嵌套 xdg
popup。客户端发起的 xdg/X11 move 和八方向 resize 会在验证 active pointer grab 后
复用画布拖动机制。

**实机验证记录**：临时 headless Weston 下，真实 Wayland SHM、GTK4 和 pointer-
constraints 客户端成功映射；Wayland `wl-copy`/`wl-paste` 与 X11 `xsel` 通过进程内
Xwayland 双向复制了准确文本；强制 X11 的 GTK4 应用出现在 `grim` 截图中；1280×800
全屏和 256×256 区域截图成功；普通与 `grim -c` 截图只在可见 fallback 箭头处不同，
重复回读不终止合成器。真实 `swaylock` 请求在受保护帧呈现后才获确认。Fedora 缺少
`/etc/pam.d/swaylock`，其 PAM worker 随后崩溃，但 nkdhr fail-closed，崩溃后截图纯黑，
证明旧画布未暴露。真实 TTY 下，Weston constraints 客户端验证 confinement 与 locked
pointer relative motion；标准 data-source DnD 客户端验证拖放图标跟随并成功放入空目标。
Weston `--self-only` 无 data-source 兼容模式在 Smithay 0.7 不会收到 drop，因为
Smithay 会将无 source 的 offer 标为未验证；这记录为非阻塞上游兼容限制，普通 Wayland
DnD 已通过。可观察 idle inhibit 与有效 PAM 解锁仍属于后续环境/集成检查。由于宿主未
安装系统 Xwayland，嵌套测试曾临时解包可执行文件并在结束后清理；生产 X11 支持要求
`PATH` 中存在 `Xwayland`。

## COMP-7：画布组件宿主

这是 Phase 3/4（先 `nkdhr-ui`，后 shell）使用的接口。**固定节点**是任何拥有世界
空间位置但不是客户端窗口的对象，例如时钟、系统监视器及后续 shell chrome。
`widget_host.rs` 的接口仍有意保持极小且 object-safe。UI-5 扩展了与 renderer 无关
的 payload 和输入钩子，但没有把具体 renderer 或窗口系统事件带进世界模型：

```rust
pub trait PinnedNode {
    fn id(&self) -> &str;
    fn world_rect(&self) -> Rectangle<f64, World>;
    fn layer(&self) -> PinnedLayer;
    fn render_data(&self) -> PinnedRenderData<'_>;
    fn pointer_event(&mut self, event: PinnedPointerEvent) -> InputHandled;
    fn prepare_frame(&mut self, output_scale: f32) -> Result<(), String>;
    fn keyboard_event(&mut self, event: &UiEvent) -> InputHandled;
}

pub enum PinnedRenderData<'a> {
    Memory {
        buffer: &'a MemoryRenderBuffer,
        source_size: Size<i32, Logical>,
    },
    NkdhrUi {
        display_list: &'a DisplayList,
        textures: &'a TextureStore,
        commit: u64,
    },
}

pub enum PinnedLayer { BehindWindows, AboveWindows }
```

trait 返回与 renderer 无关的**数据**，而非泛型 `CanvasRenderElement<R>`。泛型 render
方法无法用作 `dyn PinnedNode`，固定成 `GlesRenderer` 又会破坏 TTY multi-GPU
renderer。画布宿主用任一后端提供的 renderer 把 `PinnedRenderData` 转换成通用
render element list。`UiPinnedNode` 现在可适配任意 object-safe `UiSurface`。每个
`(node, GLES context)` 都有自己的 context-bound `GlesBackend`；不可变 display list
在保留树外叠加 viewport 平移/缩放，并以完整 output target 进行 prepare。直接
`GlesRenderer` 与 TTY `MultiRenderer` frame 走同一条路径，不会生成中间 CPU 图片。

分层和 hit-test 使用相同两个显式 band。`AboveWindows` 节点先于窗口渲染和收取输入，
`BehindWindows` 在窗口之后；cursor 与 DnD icon 始终高于两者。指针事件携带节点局部
坐标和归一化 button/motion 数据，绝不传 `InputEvent<WinitInput>`，否则共享宿主会
暗中依赖 nested 后端。返回 `Captured` 后，同一事件不再进入客户端 surface，也不会
开始画布平移。
指针 capture 在离开节点边界后仍会继续路由。按下的保留式控件也可以取得画布局部
键盘焦点；点击其他位置会清除此焦点，从而让 toolkit 按键/文本与合成器或客户端
绑定互斥。

COMP-7 只交付一个永久开发测试件来端到端证明接口：设置
`NKDHR_CANVAS_DEMO_PINNED_IMAGE=1`，在默认画布固定世界坐标注册生成的 RGBA
图片。正常会话不启用。它视觉静态，但接收指针按下并记录 hit count，满足 ROADMAP
对渲染、分层与输入的标准，又不在 Phase 3/4 前添加临时工具包或产品组件。后续真实
TTY 测试确认它在平移/总览中的世界位置正确，behind-window 层级保持，空白部分接收
点击，而上方窗口内容仍优先获得输入。

UI-5 还在 nested 合成器中实时测试了可选的“外观设置”节点：真实保留式 display
list 在画布世界变换下完成记录和绘制，没有 GLES 错误。独立 Wayland/EGL 二进制
使用完全相同的 `AppearanceSurface` 模型、root 与 display-list 路径。

## COMP-8：稳定化

这不是功能杂项桶。先关闭审计发现的正确性问题，再运行长期 active-TTY 会话，以最终
配置重跑此前每个里程碑清单，并在持续负载下测试客户端崩溃韧性。记录首尾 RSS、CPU、
DRM engine counter、每次注入的客户端崩溃、合成器存活及准确时长。嵌套 soak 是有用
回归证据，但不能替代 COMP-5/8 要求的 active VT、真实面板与外部输出。Phase 2 可在
记录硬件缺口后称实现完成，但必须等物理检查及完整八小时 active-TTY soak 通过后才算
**验收**并让暂存文档毕业。所有者可以选择空闲或交互式 soak；各物理里程碑回归负责
交互正确性。

客户端断开清理采取防御式设计。正常 `xdg_toplevel.destroy` 或 XWM unmap 会立即
移除窗口；每帧维护 pass 还会删除 Smithay `IsAlive` 为 false 的 `desktop::Window`，
并清理死亡键盘焦点、surface 已死的移动/缩放 drag 和死亡 DnD icon。COMP-8 的
SIGKILL 压力测试证明只依赖协议 destroy callback 会在客户端和 fd 都消失后留下死窗口，
因此需要第二路径。两个后端都不保留 `insert_client` 返回的 `Client` handle；display
已拥有连接和 `ClientData`，嵌套循环再存一份只会形成无限连接历史。

最终有界 COMP-8 回归使用优化构建和 COMP-7 测试件，运行在临时 Weston/Pixman
headless 宿主，强杀 200 个 SHM/EGL 客户端。所有窗口均回收，合成器保持存活，窗口数
归零，FD 保持 30。之后 20 次、每 30 秒采样（首尾相隔 9.5 分钟）显示 RSS 从
130,676 KiB 经 allocator 预热到 131,704 KiB，样本 3–20 完全保持 131,704 KiB。
这只是崩溃/资源回归证据，不能替代 ROADMAP 所需八小时 active-TTY soak。

物理验收由 `crates/nkdhr-canvas/tools/soak-test.sh` 采集，不依赖交互式 agent 或终端
watcher。`run` 创建时间戳 state 目录，在系统睡眠抑制器下启动临时 user-systemd
采集器，再 `exec` release 合成器以保留本地 TTY/libseat 上下文。`start --pid` 可附加
到现有合成器。只有记录的 logind session 处于 active 时才累计目标秒数，因此 VT 离开
可观察且不会被误算为 nkdhr 使用时间；PID start-time 校验区分原进程与 PID 重用。

采集器记录进程 RSS/高水位、CPU tick、线程与 fd 数；去重后的逐 DRM client i915
fdinfo engine/memory counter；connector 状态与 enabled 签名；session 状态；采样间隙；
过滤后的内核 DRM failure；以及归属于合成器的 failure/panic 行。完成、进程提前退出或
显式停止监控时冻结报告，合成器继续运行后追加的输出不会改变历史判定。自动 warning
是有界筛选而非按负载审核的替代品：真实客户端会合法改变 buffer，但单调 RSS/FD 增长、
GPU reset 或没有任何 idle render-engine 间隔都需要调查。运行数据位于用户 state
目录，从不进入 Git worktree。

2026-08-05 的真实面板回归用原生 `foot` 和 Weston 客户端重跑单输出输入/呈现路径，
发现并修复两个缺陷：合成器自有 drag 现在同步更新 Smithay pointer 位置；每个 KMS
输出在 vblank 前最多排队一帧，普通帧允许 cursor-plane scanout。两者消除了触控板拖动
冻结以及持久指针/窗口拖影。默认网格放置、连续移动后缓动吸附、仅主动边缩放吸附、
负坐标/自由放置、平滑键盘平移、总览、标记、固定节点路由、标准 DnD 和 pointer
constraint 均在真实 TTY 观察通过。GTK 列表还暴露原 axis 策略吞掉应用滚动；修正后，
真实触控板确认双指滚动焦点客户端，恰好三指由 libinput 全局移动画布（即使位于客户端
上方）。Ctrl+Alt+Fn VT 切换和恢复时立即全重绘也通过。

2026-08-07 的外接显示器回归随后通过刚性组、独立输出组、保留客户端的物理热插拔和
干净双输出 VT handoff。2026-08-08，所有者接受连续八小时 active-TTY 空闲 soak 作为
这个早期合成器阶段的充分证据：956 个样本都属于原进程，RSS 只增长 192 KiB，FD 与
线程不变，平均 CPU 为单核 0.243%，内核 DRM 错误为零。soak 后 VT 日志发现最后一个
顺序竞态，修复为在 VT 切换请求之前清除并暂停 DRM。最后一分钟物理 TTY 回归记录
32/32 匹配样本、正确的 inactive/active 输出转换、零内核/合成器错误，并在切回时
立即全屏重绘。此结果通过后 Phase 2 正式验收，文档随即毕业。

## 渲染管线边界（为什么 Phase 3 是硬边界）

COMP-1 到 COMP-7 直接通过 Smithay `GlesRenderer`/`Frame` API 把所有内容渲染为
扁平纹理 quad（矩形、边框、窗口/固定节点内容）。Phase 2 完全没有圆角矩形、阴影、
文本等图元层，因为 `nkdhr-render`（UI-1）属于 Phase 3。Phase 2 窗口 chrome 因而
有意保持最简（平边框，无阴影和主题标题文字），不是遗漏；真实 chrome 属于 UI-4/
shell，在 Phase 3/4 设计，提前做只会重复劳动。`PinnedNode::render_data` 返回与 renderer
无关的 payload，再由宿主适配成轻量 `CanvasRenderElement`，也是同一边界：Phase 3
接入正式图元层时，无需向节点注册表暴露后端特定输入或 renderer 类型。

## Phase 4：输出本地 shell 宿主

`nkdhr-shell::ShellSurface` 与世界坐标 `PinnedNode` 分离。`ShellHost` 按物理输出名持有一棵
全输出透明 retained tree，只有实际占用的四角/四边中心区域绘制并命中；稳定的
`EdgeRegion` 身份不会随隐藏或重排消失。输出布局热更新会增删对应 surface，并同步清理
离开输出的 pointer、keyboard 与 button-capture 所有权。所有 surface 共享同一个
`ThemeRuntime`，但保留各自尺寸、比例、焦点和动画状态，因此多显示器活动不会互相接管。

合成顺序把 shell 放在窗口/画布之后、指针和拖放图标之前。它使用整输出透明 render
element，使材质的 backdrop blur 能采样已经绘制的窗口，而不是把窗口先拍平到临时纹理。
首个真实组合是已经确认的上中 calm 数字时间；其他区域只有稳定身份，还不能按成品计数。

自定义 GLES 绘制必须把 output-space damage/clip 经活动 frame projection 转成 OpenGL
bottom-left framebuffer 坐标。nested 后端使用 `Flipped180`；若直接把 top-left clip 交给
`glScissor`，无裁剪的玻璃外框仍可见，但文字与局部模糊会被裁到相反边。backdrop snapshot
还会在默认 framebuffer 上显式选择 `BACK` read buffer（FBO 使用 `COLOR_ATTACHMENT0`），
完成后恢复原 read buffer。单元测试覆盖两种坐标方向，Weston/Pixman 嵌套冒烟同时验证
时间字形、背景模糊和合成器持续存活。
