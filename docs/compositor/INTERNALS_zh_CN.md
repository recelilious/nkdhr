# 合成器 — 内部实现

> [English version / 英文版本](INTERNALS.md)

面向读者：参与 `nkdhr-compositor` 开发的贡献者。本文引用的每个 Smithay API，其事
实依据都是 `~/.cargo/registry` 下所拉取的 Smithay 0.7.0 自身源码（尤其是它的
`examples/` 和模块文档），而不是对旧版 Smithay 的记忆；这些 API 在历次版本中发生
过很大变化。

## 范围

本 crate 是 nkdhr 的显示协议层：在真实显示器上为 Wayland 和 X11 客户端提供服务所
需的一切，外加一套永久性的最小窗口行为，使其本身即可独立使用。它不包含任何交互设
计：没有工作区、启动器、shell、快捷键绑定目录、动画或 UI 渲染。未来的交互层将通
过 [EXTENDING_zh_CN.md](EXTENDING_zh_CN.md) 中描述的接缝接入。

2026-10-23 之前，在这一层之上还有一个交互层，位于当时名为 `nkdhr-canvas` 的
crate 中：画布模型、视口与概览（COMP-3、COMP-4）、固定小组件宿主（COMP-7）、编号
工作区、模态窗口放置、类型化绑定语言、输出本地的 shell 以及 UI 渲染适配器。交互
设计被撤回以重新设计时，它被整体移除。其代码和文档保存在 git 标签
`archive/interaction-v1` 中（`git show archive/interaction-v1:<path>`）。
COMP-1、COMP-2、COMP-5、COMP-6 和 COMP-8 中后端与协议方面的工作得以保留，并在下
文描述；[历史](#历史)一节保留了它们的验证记录。

## Crate 结构

```
crates/nkdhr-compositor/
  Cargo.toml        特性：nested（默认）、tty
  src/
    main.rs         信号处理函数、后端选择
    shutdown.rs     ShutdownSignal：把 SIGTERM、SIGINT 和 SIGHUP
                    作为一个被轮询的关闭请求
    state.rs        App：协议状态、seat、窗口存储、焦点策略、弹出窗口抓取、
                    Smithay handler
    windows.rs      ManagedWindow、WindowStore、初始放置规则与保持可达规则
    outputs.rs      OutputLayout 解析器与 `outputs` namespace 监听器
    input.rs        输入路由器
    grabs.rs        MoveGrab 与 ResizeGrab（客户端发起的移动/调整大小）
    render.rs       渲染元素列表、逐输出合成、输出归属、
                    帧回调、refresh
    cursor.rs       光标图像状态与内置的后备箭头
    backends/
      mod.rs        Backend trait、SourceGuard
      winit.rs      嵌套后端
      tty.rs        DRM/KMS + GBM + libinput + libseat 后端
    protocols/
      mod.rs        ProtocolState：协议长尾与会话锁
      xwayland.rs   进程内 X11 窗口管理器与选区桥接
      screencopy.rs wlr-screencopy 服务端
  tools/
    vkms-lab.sh     双输出 VKMS 实验环境
    soak-test.sh    长时间运行稳定性数据采集器
```

Cargo 特性让部署保持精简。`nested` 启用 Smithay 的 `backend_winit`；`tty` 启用
`backend_gbm`、`backend_libinput`、`backend_session_libseat`、`backend_udev`、
`renderer_gl` 和 `renderer_multi`，外加 `smithay-drm-extras` 与 `thiserror`。
Smithay 的基础特性是 `desktop`、`wayland_frontend` 和 `xwayland`。唯一的 nkdhr
依赖是 `nkdhr-ipc`，由 outputs 监听器配合 `zbus` 使用。`signal-hook` 安装信号处
理函数，`libc` 提供 `sigaction` 调用，用于读取合成器所继承的信号处置方式
（见[关闭](#关闭)）。

`backends/` 之外的一切都由两个后端共享，并且从不知道正在运行的是哪一个后端。后端
拥有自己的事件循环、渲染器、Smithay `Output` 对象和呈现（presentation），并在固
定的时机调用共享层：每个输入事件调用 `input::handle`，每次客户端调度之后立即调用
`App::finish_client_dispatch`，每次循环迭代调用一次 `render::refresh`，合成每个
输出时调用 `render::output_elements`，每个输出的渲染过程之后调用帧回调与输出归属
辅助函数，每当重新解析布局时调用 `App::set_output_layout`。它还会在每次循环迭代
中检查一次关闭请求。

## 后端选择

`main.rs` 首先安装信号处理函数（`ShutdownSignal::install`，见[关闭](#关闭)），然
后根据其唯一的参数选择后端：`--nested` 运行 winit，`--tty` 运行 TTY 后端；不带参
数时，仅当设置了 `WAYLAND_DISPLAY` 或 `DISPLAY` 时选择嵌套后端，否则选择 TTY。任
何其他参数都是错误，选择一个其特性未被编译进来的后端同样是错误。两个后端都实现了
只有一个方法的 `Backend` trait，其 `run(self, &ShutdownSignal)` 在合成器停止时返
回；两者都不是另一者的脚手架。

## `App`

`App`（`state.rs`）是实现所有 handler 的 Smithay 状态类型。它持有协议状态和窗口
存储，此外别无他物：

- Smithay 协议状态：`CompositorState`、`XdgShellState`、`ShmState`、
  `DmabufState`、`OutputManagerState`（创建时带 xdg-output）、`SeatState` 及其唯
  一一个带有键盘、指针和触摸的 `Seat`、`PopupManager`，以及
  `ProtocolState`（`protocols/mod.rs`，协议长尾与会话锁运行时）。
- `cursor`（`cursor.rs`）和可选的拖放图标。
- `touch_points`：当前按下的触摸点及其最后位置，由输入路由器记录。Smithay 私下维
  护这份列表，而会话锁需要它来取消每一个进行中的触摸序列
  （见[会话锁](#会话锁)）。
- `user_actions`：最近一次按键按下、按键释放、按钮按下和触摸按下，各自附带其
  serial 以及它被送达的客户端，由输入路由器记录。`xdg_popup.grab` 只针对其中之一
  才会被受理（见[协议](#协议)）。
- `popup_grab`：最近一次被授予的 `PopupGrab`，保留到合成器一侧对其抓取链的持有被
  释放为止。它决定另一个客户端的菜单能否打开，也是菜单外的触摸、每次循环迭代对抓
  取的收尾以及会话锁的作用对象（见[协议](#协议)）。
- 一个标志，标记会话锁定期间由锁屏客户端以外的客户端发起的拖动，
  `App::finish_client_dispatch` 会在该次调度之后将其取消
  （见[会话锁](#会话锁)）。
- `windows`：`WindowStore`。
- `output_layout`：后端当前 `OutputLayout` 的副本，通过 `App::set_output_layout`
  替换。协议 handler（尤其是提交时的放置规则）无法访问后端，因此 `App` 保留自己
  的副本。
- `focus_before_lock`：解锁时要恢复的键盘焦点。
- VT 切换接缝：`vt_switching_enabled` 加上一次性的 `pending_vt_switch`。只有 TTY
  后端调用 `enable_vt_switching`；输入路由器调用 `request_vt_switch`，后端在每个
  输入事件之后通过 `take_vt_switch_request` 取走请求。因此共享的输入层从不接触
  libseat。

`KeyboardFocusTarget` 是 `Wayland(WlSurface)` 或 `X11(X11Surface)`；它涵盖原生窗
口、X11 窗口和会话锁表面。每个 Wayland 客户端带有 `ClientState`（其
`CompositorClientState`）；Xwayland 客户端带有 Smithay 的 `XWaylandClientData`，
`client_compositor_state` 两者都接受。

渲染器从不存放在 `App` 中。协议处理中没有任何东西接触像素：缓冲区导入在渲染时惰
性进行，使用后端传入的任意渲染器。正是因为 `App` 与渲染器无关，同一个 `App` 才能
既服务嵌套后端的 `GlesRenderer`，又服务 TTY 后端的多 GPU 渲染器。

## 窗口存储

`windows.rs` 持有唯一一份客户端窗口列表。

`ManagedWindow` 包装了 Smithay 的 `desktop::Window`（它用同一个类型表示 xdg
toplevel 和 X11 表面），并附带：

- 一个 `WindowId`：进程内唯一、单调递增且从不复用的数字，因此过期的引用不会与更
  新的窗口混淆；
- `location: Option<Point<i32, Logical>>`，窗口未放置时为 `None`；
- `ResizeGrab` 使用的进行中的调整大小状态。

它的访问器覆盖协议层所需的一切：根 `wl_surface` 和 X11 表面、
`matches_surface`（对根表面、任意子表面和任意弹出窗口为真，通过
`Window::with_surfaces` 实现）、`geometry`、`bbox`（包含弹出窗口）和
`global_bbox`、供焦点策略使用的 `keyboard_target` 与 `owns_focus`、
`size_limits`（客户端的最小与最大尺寸提示），以及 `request_size`（一次 xdg
configure 或一次 X11 configure）。协议层中没有任何东西会关闭窗口，因此不存在关闭
操作。

**位置约定。** 窗口的位置是其根表面原点在合成器全局逻辑空间中的位置：用 Smithay
的术语来说，就是窗口的 `(0, 0)` 点。渲染时把窗口绘制在
`location - output.global_location` 处，命中测试则把 `point - location` 传给
`Window::surface_under`。窗口的可见窗框是按位置偏移后的 `geometry()`；对于在其
xdg 窗口几何之外绘制阴影的客户端，`geometry().loc` 就是阴影内缩量。对于 X11 窗
口，`geometry().loc` 为 `(0, 0)`，因此位置等于 X11 位置。

**层叠顺序。** 存储是一个自底向上排序的向量；最后一个元素位于最顶层。渲染时自顶
向下遍历它，命中测试返回自顶向下的第一个匹配。`raise` 把窗口移到末尾。

**操作。** 每一次变更都经由存储完成：插入一个未放置的 toplevel、放置它（同时将其
提升）以及取消放置、插入一个已放置的 X11 窗口、移除窗口、提升窗口、设置其位置、
按 id、按根 `wl_surface` 或按 `X11Surface` 查找窗口、查找最顶层的可聚焦窗口（已
放置且不是 override-redirect）、遍历窗口，以及回收 `IsAlive` 为 false 的窗口。在
某个全局坐标点上的表面查找会返回该窗口、该点下方确切的表面（根表面、子表面或弹出
窗口，通过 `Window::surface_under` 配合 `WindowSurfaceType::ALL` 遵循输入区域）
以及该表面的全局原点，这正是指针焦点所需要的。未放置的窗口只能通过协议 handler
使用的表面查找（按根 `wl_surface` 或按 `X11Surface`）找到；命中测试、焦点、渲染
和帧回调都会跳过它们，输出归属会让它们离开所有输出。

### 窗口生命周期

- **xdg toplevel。** `new_toplevel` 以未放置状态插入窗口。提交处理函数
  （`CompositorHandler::commit`）运行 Smithay 的 `on_commit_buffer_handler`、
  `PopupManager::commit`，并对被提交表面所属的根表面对应的窗口运行
  `Window::on_commit`（它会刷新窗口的包围盒）；同步子表面在其父表面的提交应用它
  时处理。随后，只要该 toplevel 尚未收到过初始 configure，就向它发送初始
  configure：在其首次提交时，以及在空缓冲区取消映射之后再次发送（Smithay 的缓冲
  区处理函数会将其标记为需要重新初始化）。初始 configure 不携带尺寸（因此由客户
  端自行选择尺寸），也不携带任何状态。当一个未放置的 toplevel 带着已附加的缓冲区
  提交时，其尺寸已知：它按初始放置规则放置、被提升，并通过焦点策略获得焦点。一个
  已放置的 toplevel 若提交空缓冲区，会重新变为未放置，并被视为已取消映射（焦点回
  退）；它下一次带缓冲区的提交会像新窗口一样放置它。`toplevel_destroyed` 移除
  它。
- **X11 受管窗口。** `map_window_request` 调用 `set_mapped(true)`。一旦 X11 窗口
  拥有了它的 `wl_surface`（立即拥有，或稍后通过
  `XWaylandShellHandler::surface_associated` 获得，在 `pending_x11_maps` 中跟
  踪），它就按同一规则根据其 X11 几何放置、被 configure 到该位置、插入到最顶层并
  获得焦点。
- **X11 override-redirect 窗口**（菜单、工具提示）。它以自身的 X11 位置插入到最
  顶层，并跟随每一个 `configure_notify`。它从不因点击而被提升，也从不获得焦点。
- **移除。** `unmapped_window`、`destroyed_window`、`toplevel_destroyed` 以及死
  亡客户端回收会移除窗口；如果它持有键盘焦点，则运行焦点回退。

上述每一种变更都可能改变一个未移动的指针下方的内容，而指针焦点原本只跟随指针移
动。因此每一种变更之后都会调用
`input::refresh_pointer_focus`（见[输入路由](#输入路由)）。

### 初始放置规则

`windows::initial_location(geometry, &OutputLayout, pointer)` 是决定新窗口去向的
唯一函数。它的输入是窗口的几何（尺寸及其可见窗框的内缩量）、布局和当前指针位置：

1. 目标输出是包含指针的那个输出（`OutputLayout::output_at`）。如果指针不在任何输
   出上（例如指针所在的输出刚被拔出、而下一次移动尚未把指针重新限制回来时），则
   使用布局顺序中的第一个输出。如果根本没有输出，可见窗框的左上角被放在
   `(0, 0)`。
2. 在每个轴上独立判断：如果窗框不大于输出，则将其居中
   （`output_start + (output_extent - frame_extent) / 2`，整数除法）；如果大于输
   出，窗框从输出的起点开始，使标题栏和左边缘保持在屏幕上。
3. 返回的位置会减去几何内缩量，从而被居中的是可见窗框而不是阴影。

该规则从不移动任何其他窗口。它的单元测试覆盖居中、单轴超尺寸和双轴超尺寸、窗框内
缩、第一个输出的回退以及空布局。

### 保持可达规则

窗口绝不能被困在没有任何显示器显示它、用户也无法把它找回来的地方。每次输出布局变
化之后（热插拔、配置变更、嵌套窗口调整大小），
`windows::location_to_keep_reachable(location, geometry, &OutputLayout, pointer)`
会应用到每一个已放置的受管窗口：可见窗框不再与任何输出相交的窗口，会按初始放置规
则重新放置（指针下方的输出，否则是第一个输出）。仍与某个输出相交的窗口保持其位
置，即使它的大部分现在已在屏幕之外，因此重新排列显示器不会移动任何仍然可达的东
西。不含任何输出的布局不会移动任何东西；当输出恢复时规则会再次运行。
override-redirect 窗口交给它们的客户端处理。重新定位既不改变层叠顺序，也不改变焦
点。单元测试覆盖保持不动的窗口（完全可见与勉强可见）、被困的窗口（包括只触碰到某
个输出远端边缘的窗口）以及空布局。

## 输出布局

`outputs.rs` 把 `outputs` namespace 和已连接 connector 的集合转化为一个扁平、确
定的布局。

`ConnectedOutput { name, physical_size }` 是后端报告的内容：DRM connector 名称及
模式尺寸，或者嵌套窗口的输出。
`OutputLayout::resolve(&OutputPlacements, &[ConnectedOutput])` 为每个已连接的
connector 生成
`ResolvedOutput { name, physical_size, logical_size, scale, global_location }`：

1. `logical_size` 是物理尺寸除以缩放比例，在每个轴上四舍五入到最接近的整逻辑像
   素，且至少为 1×1。这与 Smithay 计算其通过 xdg-output 公布的尺寸的方式
   （`to_logical(scale).to_i32_round()`）完全一致，XWayland 的 RandR 又转而报告
   该尺寸，因此合成器用来排布、限制指针以及确定锁表面尺寸的矩形，正是告知客户端
   的那些矩形。
2. 有条目的已连接 connector 按其配置的 `(x, y)` 和配置的缩放比例放置，按
   connector 名称排序。未连接 connector 的条目会被忽略，不占用任何空间。
3. 没有条目的已连接 connector 获得缩放比例 `1.0`，并按 connector 名称顺序从左到
   右排布。第一个从已配置输出包围盒的右边缘开始，与其顶部对齐；之后的每一个从前
   一个的右边缘开始。如果没有已配置的输出，排布从 `(0, 0)` 开始。
4. 整个布局会被平移，使其包围盒从 `(0, 0)` 开始。相对位置保持不变；因此配置负坐
   标没有问题。

`nkdhrd` 接受任意 `i32` 位置和任意正缩放比例，因此每一步都采用饱和运算而不会溢
出：排布游标、商超过 `i32::MAX` 的逻辑尺寸、归一化以及初始放置规则中的居中
（Smithay 自身的点与矩形运算也以同样的方式饱和）。荒谬的配置会产生荒谬但有效的布
局，绝不会 panic。

结果按布局顺序排列：先是已配置的输出，然后是自动排布的输出。它可以回答
`output(name)`、`output_at(point)`（布局顺序中第一个其半开矩形包含该点的输出）、
用于映射绝对输入设备的包围尺寸，以及 `clamp(point)`。限制（clamp）把指针保持在输
出矩形的并集之内，而不是它们的包围盒之内：位于某个输出内的点保持不变；否则它会被
分别限制到每个输出矩形中，取最近的结果。输出之间没有死区，一次会离开并集的移动会
沿着并集的边缘滑动。被限制后的点最远只到输出远端边缘之前一个逻辑像素处，因为半开
矩形不包含远端边缘。如果根本没有输出，点保持不变。重叠的已配置输出不会被拒绝；每
个输出只是显示全局空间中属于它自己的矩形。解析器的单元测试覆盖自动排布、负位置的
归一化、取决于缩放比例的逻辑尺寸及其舍入（包括低于一半的小数部分，与 xdg-output
的舍入方式一致）、在已配置区域之后的排布、坐标范围末端的饱和、被忽略的未连接条
目、跨越间隙的 `output_at`、限制到并集之内、空布局以及展平 key 的解码。

### `outputs` 监听器

`OutputsConfig::watch()` 连接到 session bus，并通过 `Config1.GetAll("outputs")`
读取一次该 namespace。随后一个后台线程订阅 `Config1.Changed`；对于 `outputs.` 下
的任意 key，它会重新读取整个 namespace、替换共享快照，并递增一个原子的代次
（generation）计数器。后端在每次循环迭代中比较一次代次，在其变化时重新解析布局。
如果没有 session bus、没有 `nkdhrd`，或者 `nkdhrd` 不认识该 namespace，快照为
空，所有输出都会自动排布。

key 以展平形式 `outputs.<connector>.<field>` 到达。`nkdhrd` 的 schema 禁止
connector 名称中出现 `.`，因此 key 可以无歧义地拆分。`x` 和 `y` 接受任意能放入
`i32` 的整数变体；`scale` 以 double 形式到达（以整数发送的整数值同样被接受）。缺
失的字段、类型不符合预期的值，以及不是有限正数的缩放比例，都会取
`OutputPlacement` 的默认值，因此格式错误的值永远不会产生退化的输出。`nkdhrd` 只
为编辑后仍存在的 key 发出 `Changed`，因此删除整个条目不会产生信号；合成器会在下
一次有信号的变更时或下次启动时察觉到这次删除。

### 应用布局

解析出新布局的后端会把每个 Smithay `Output` 的当前状态设为其模式、
`Scale::Fractional(scale)` 和 `global_location`，它们由 `wl_output` 和
xdg-output 对外公布（`wl_output` 的整数缩放是向上取整后的分数缩放）。随后它调用
`App::set_output_layout`，该函数存储副本，协调以输出名称为键的协议簿记（会话锁的
覆盖范围与锁的键盘焦点，以及待处理的 screencopy 请求；针对已消失输出的请求会失
败），应用[保持可达规则](#保持可达规则)，最后刷新指针焦点。除此之外窗口不会被移
动：窗口的位置是全局的，不会跟随其输出。

## 焦点

焦点策略位于 `state.rs` 中，由几个具名函数组成，供 crate 的其余部分调用：

- `App::focus_window(id)` 提升窗口并赋予其键盘焦点。映射窗口和点击窗口都使用它。
  提升 X11 窗口时也会在 X 服务器中重新调整其层叠位置（`X11Wm::raise_window`），
  X11 客户端依赖 X 服务器的层叠顺序，例如用来找到 X11 窗口之间拖动的目标。如果存
  在弹出窗口抓取，会先将其解除（`App::dismiss_popup_grab`），就像点击其菜单之外
  时那样：Smithay 的 `PopupKeyboardGrab` 把键盘焦点保持在菜单上，并静默丢弃其他
  任何焦点设置，因此若不如此，一个在菜单打开期间出现的窗口会被提升到菜单所属窗口
  之上，却仍然没有焦点。
- `App::focus_topmost()` 是回退：当持有键盘焦点的窗口被移除时，焦点移到最顶层的
  可聚焦窗口，或者不交给任何窗口。
- 会话锁定期间，实时键盘焦点属于锁表面。此时这两个函数改为更新
  `focus_before_lock`：锁定期间映射的窗口成为解锁时要恢复的焦点；如果被记住的窗
  口消失了，被记住的焦点会回退到剩余窗口中最顶层的那个。锁定开始时，被记住的是拥
  有焦点的窗口，而绝不是通过其抓取持有键盘的弹出窗口，因为锁定会解除该弹出窗口：
  弹出窗口的抓取链代表其根窗口，所依据的规则与抓取链关闭时把焦点交还给根窗口的规
  则相同（见下文），即使该弹出窗口在锁定前一刻刚被销毁、其抓取链尚未收尾也是如
  此。
- `App::keep_keyboard_on_lock_surface()` 在锁定期间把实时焦点保持在锁表面上
  （见[会话锁](#会话锁)）。
- 被授予的弹出窗口抓取会把键盘焦点交给该弹出窗口，因为 xdg-shell 把焦点交给最顶
  层的抓取弹出窗口，即使其客户端此前并未持有焦点（通过触摸打开的菜单）也是如此。
  它还会提升弹出窗口的根窗口：弹出窗口在其所属窗口的层叠级别上绘制和命中测试，因
  此若不如此，一个不在最顶层的窗口的菜单会有一部分被其他窗口遮挡，而触摸被遮挡的
  部分会把菜单解除。在抓取链打开期间，焦点停留在其最顶层的抓取弹出窗口上；当嵌套
  的弹出窗口关闭时，焦点移到父弹出窗口。
- 当一条持有键盘焦点的弹出窗口抓取链关闭时——无论是其客户端销毁了这些弹出窗口（选
  中了某一项、Escape）、菜单外的点击或触摸解除了它们，还是根窗口消失了——焦点会回
  到该抓取链的根窗口，而不是最顶层窗口，并且该窗口不会被再次提升（授予抓取时已经
  提升过）。当抓取链因指针输入或按键而结束时，Smithay 的弹出窗口抓取会自行完成这
  一点。其他情况下由 `App::settle_popup_grab` 或 `App::dismiss_popup_grab` 通过
  `App::focus_popup_root` 完成，当根窗口已消失或已取消映射时，它改为运行焦点回
  退。收尾在下一次循环迭代中运行，早于死亡客户端回收可能对被销毁弹出窗口所持焦点
  施加的回退（见[协议](#协议)）。

`focus_window` 会提升窗口，这可能改变指针下方的内容；其调用方在整个变更完成之后
才刷新指针焦点，从而由单次移动描述最终结果。

override-redirect 窗口和未放置的 toplevel 永远不是焦点目标。实时焦点通过
`App::set_keyboard_focus` 设置，它经由 seat 的 `KeyboardHandle` 完成；随后
`SeatHandler::focus_changed` 使协议状态与结果保持一致：恰好只有获得焦点的窗口带
有 xdg `Activated`（或 X11 激活状态），configure 只发送给状态确实发生变化的
toplevel（并且绝不早于其初始 configure），剪贴板和主选区焦点跟随获得焦点的表面所
属的客户端，锁定期间则被清除。Smithay 0.7 只在出现新焦点时调用 `focus_changed`，
清除焦点时不会调用，因此 `set_keyboard_focus` 在清除焦点时自行执行同样的簿记；否
则最后一个获得焦点的窗口会保持激活状态并保留剪贴板焦点。

## 输入路由

`input::handle(app, &OutputLayout, event)` 是输入的唯一入口。TTY 后端传入除设备
添加/移除之外的所有 libinput 事件；嵌套后端传入 winit 的输入事件。路由器只参考
seat、窗口存储、布局、锁定状态以及打开的弹出窗口抓取链（抓取链之外的触摸会解除
它），只记录哪些触摸点处于按下状态以及每种用户操作的最近一次
（`App::user_actions`），并且恰好只有一个合成器快捷键：VT 切换。

1. **会话已锁定。** 如果锁定生效，事件走 fail-closed 的锁定路径（见下文），其他
   任何逻辑都不运行。
2. **键盘。** 每个按键都经过 Smithay 的 `KeyboardHandle::input` 过滤器。当
   `vt_switching_enabled` 时，VT 按键在按下和释放时都会被拦截，按下时记录
   `request_vt_switch(n)`；其他所有按键都转发给获得焦点的表面。每个按键事件都作
   为最近一次按键按下或释放记录到 `App::user_actions` 中，并附带接收它的表面所属
   的客户端；被拦截时则不附带客户端。
3. **指针移动。** 相对事件的目标是 `current + delta`，绝对事件映射到布局的包围尺
   寸上；两种目标都会被限制到输出的并集之内。目标下方的表面成为指针焦点。活动的
   指针约束按[协议](#协议)中所述的方式得到遵守。相对事件还会为
   `zwp_relative_pointer_v1` 产生 `relative_motion`（带有未加速的增量）。每个事
   件以 `pointer.frame` 结束。
4. **按钮。** 按下时，如果指针没有活动的抓取，路由器会查找指针下方的窗口；如果它
   是一个已放置的、非 override-redirect 的窗口，`focus_window` 会聚焦并提升它。
   在没有窗口的地方按下不会改变焦点。随后在指针所在处重新计算指针焦点（见下
   文），并原样转发按钮事件，因此即使在指针此后未移动的情况下场景发生了变化（某
   个客户端调整了大小、出现了一个弹出窗口），按下事件也恰好到达焦点规则命中测试
   到的那个表面。在抓取期间跳过聚焦步骤，意味着一次解除弹出窗口的点击（通过
   Smithay 的 `PopupPointerGrab`）或一次结束移动的点击不会同时切换窗口。以其他方
   式关闭的菜单（通过触摸或按键选中某一项、Escape）的 `PopupPointerGrab` 会在下
   一次循环迭代时被释放（见[协议](#协议)），因此下一次点击照常聚焦。每次按下都作
   为最近一次按钮按下记录到 `App::user_actions` 中，并附带接收它的表面所属的客户
   端：即 Smithay 送达之后的指针焦点；在移动或调整大小期间则不附带客户端。
5. **滚动轴（axis）。** 每个滚动轴事件都会被转发，包括零增量的停止帧，客户端中的
   惯性滚动依赖这些停止帧。帧携带来源、相对方向、连续值、v120 离散值，以及对于手
   指来源在值为零时的 `stop`。只报告离散步进的设备（经由嵌套后端的鼠标滚轮）会得
   到由步进推导出的连续值，每格 15 个逻辑像素，与 libinput 对滚轮的报告一致：
   Smithay 只在值非零时发送 `wl_pointer.axis`，而没有它的 `axis_value120` 对客户
   端来说不是一次有效的滚动。
6. **手势。** 滑动、捏合和按住事件原样转发给 seat，并通过
   `zwp_pointer_gestures_v1` 全局对象到达客户端；触摸板滚动属于滚动轴输入，不受
   影响。
7. **触摸。** 位置映射到布局的包围尺寸上。按下和移动事件连同触摸点下方的表面一起
   转发，抬起、帧和取消事件则原样转发。Smithay 把每个触摸序列保持在它开始时所在
   的表面上：移动事件发往该触摸点按下时所在的表面，并且在一根手指按下期间，后续
   手指都发往第一根手指的表面。路由器记录哪些触摸点处于按下状态及其位置
   （`App::touch_points`），并把每次触摸按下作为最近一次触摸按下记录到
   `App::user_actions` 中，附带接收它的表面所属的客户端（Smithay `TouchDownGrab`
   的起始焦点；当触摸拖放吞掉了它时则不附带客户端）。触摸从不聚焦或提升窗口。一
   次在打开的弹出窗口抓取链所属客户端之外（落在另一个客户端的表面上或不落在任何
   表面上）开始新触摸序列的触摸按下，会先通过 `App::dismiss_popup_grab` 解除该抓
   取链，就像 Smithay 的 `PopupPointerGrab` 对外部点击所做的那样，然后照常送达；
   Smithay 没有与该抓取相对应的触摸版本。一个序列中的后续手指无论下方是什么都发
   往第一根手指的表面，因此它们永远不会解除抓取链。

绝对设备（触摸屏、绘图板）映射到整个布局的包围盒上，而不是映射到单个输出上。

**刷新指针焦点。** Smithay 只在移动时改变指针焦点，因此
`input::refresh_pointer_focus(app)` 会在指针当前位置重新运行移动步骤，不带相对移
动，并使用合成的时间戳：一次朝向当前位于指针下方表面的零距离移动，约束处理与真实
移动相同。它在指针静止而其下方内容发生任何变化之后运行：放置 xdg toplevel 或映射
X11 窗口（受管或 override-redirect）、移动 override-redirect 窗口、取消放置或移
除窗口、回收死亡窗口、采用新的输出布局（这涵盖了保持可达规则引起的重新定位）、释
放弹出窗口抓取以及解锁，此外还在转发按下事件之前运行。会话锁定期间它什么也不做，
因为此时由锁定路径掌管指针焦点；指针抓取处于活动状态时它也什么都不做，因为抓取自
行决定焦点。它绝不能在指针抓取自身的回调中运行（`MoveGrab` 和 `ResizeGrab` 在那
里移动窗口），因为 Smithay 在调用这些回调时持有指针；因此 `App::move_window` 不
触碰指针焦点。

**锁定路径** 只路由到锁表面。键盘输入仍会经过 VT 切换拦截，然后发往获得焦点的表
面，而它只可能是锁表面。指针移动的限制方式与未锁定路径完全相同；指针焦点是指针下
方输出的锁表面，位于该输出的全局位置。按钮按下会先把键盘焦点移到该锁表面（如果它
仍存活），再转发事件。滚动轴、手势和触摸事件以同样的方式以锁表面为目标；锁定开始
前已开始的触摸序列在锁定开始时就已被取消，因此其中没有任何一个仍能到达窗口。窗口
存储从不被查询。

**VT 按键。** XKB 可能把 `Ctrl+Alt+Fn` 直接解析为 `XF86Switch_VT_n` keysym。因此
拦截逻辑接受来自修饰后符号的该 keysym，并另外在按住 Ctrl 和 Alt 时接受来自未经修
饰的原始零级符号中的 `F1`…`F12`。仅依赖 `modified_sym()` 曾导致第一次物理测试悄
无声息地漏掉了这个绑定。

## 抓取

`grabs.rs` 以 Smithay `PointerGrab` 的形式实现由客户端发起的交互式移动和调整大
小，风格与 Smithay 自身的示例一致。请求只在以下条件下才会被接受：针对已放置的窗
口、从不在锁定期间，并且只在指针的活动抓取是 Smithay 的隐式 `ClickGrab`（即按下
后一直按住的按钮的抓取）且其起始焦点属于该窗口（其根表面、某个子表面或某个弹出窗
口）时：

- `xdg_toplevel.move`/`resize`：seat 必须是合成器的 seat，并且
  `PointerHandle::has_grab(serial)` 对请求的 serial 必须成立。
- X11 `_NET_WM_MOVERESIZE`（`XwmHandler::move_request`/`resize_request`）：X11
  消息不指明 serial，因此新抓取沿用指针当前抓取的 serial。

只有隐式抓取才携带移动或调整大小所延续的那次按下。在 `MoveGrab` 或 `ResizeGrab`
已经运行时重复发出的请求与该抓取共享 serial，但其起始数据会把已经移动过的窗口与
原始按下位置配对，从而把偏移量应用两次；Smithay 的 `PopupPointerGrab` 把全局原点
报告为其起始位置。这两种情况都会被拒绝，由触摸发起的请求也会被拒绝，因为它根本没
有指针抓取。抓取以 `Focus::Clear` 安装，因此在它运行期间没有任何表面拥有指针焦
点，其起始数据就是点击抓取的起始数据。

- **`MoveGrab`** 把窗口位置设为其初始位置加上指针相对抓取起点的偏移，四舍五入到
  逻辑像素。X11 窗口会被重新 configure 到新位置，使 X 服务器所认为的窗口位置保持
  同步（见 [XWayland](#xwayland)）。
- **`ResizeGrab`** 根据所请求边缘的指针偏移计算新尺寸，并限制在客户端的最小与最
  大尺寸提示范围内，且至少为 1×1。xdg toplevel 会收到一个带有该尺寸和 `Resizing`
  状态的 configure。当抓取结束时（无论出于何种原因），它会收到一个不带
  `Resizing` 的最终 configure。在调整大小期间，以及直到客户端提交其对该最终
  configure 的响应为止（通过 `XdgShellHandler::ack_configure` 跟踪），提交处理函
  数都会移动窗口，使得从顶部或左侧调整大小时，对边保持在抓取开始时的位置：
  `location.x = initial.x + (initial_width - current_width)`，`y` 同理，其中宽度
  指窗口几何的宽度。X11 窗口则一并 configure 新尺寸和调整后的位置，因为 X11 几何
  由窗口管理器设定。在此期间移动窗口（例如保持可达规则在输出变化后重新定位它）会
  永久取消这一校正：X11 窗口从此保持被移动后的位置，并且只有当抓取自身的调整大小
  仍在进行中时（`ResizeState::grab_ended`），抓取的结束才会为最终 configure 启用
  校正，因此窗口永远不会被放回到相对于调整大小开始时的位置。

两种抓取都在最后一个按钮释放时结束，并在那一刻把指针焦点交给指针下方的表面。
Smithay 自身的焦点恢复会重放抓取移动窗口或调整窗口大小之前最后一次移动所计算出的
焦点，那可能是位于窗口旧位置后面的表面。其他每个 `PointerGrab` 方法都把事件直接
透传。所属窗口已死亡的抓取会在其下一个事件时自行结束，锁定会话会结束任何抓取
（见[会话锁](#会话锁)）。抓取的算术运算与布局的一样采用饱和运算。

## 渲染

`render.rs` 用 Smithay 的 `render_elements!` 宏定义元素类型：

```rust
OutputRenderElement<R>:
    Surface = WaylandSurfaceRenderElement<R>,        // 客户端表面、光标表面、DnD 图标
    Memory  = MemoryRenderBufferRenderElement<R>,    // 内置的后备光标
```

除此之外没有别的东西要绘制：背景是帧的清屏颜色，一个中性的深色常量，锁定时为黑
色。

`render::output_elements(renderer, app, &ResolvedOutput, include_cursor)` 按从前
到后的顺序返回一个输出的元素：

1. 光标，除非某个不带光标叠加的待处理 screencopy 将其排除：位于
   `pointer - hotspot` 的客户端光标表面，或者用于命名光标和背景的内置 24×24 箭
   头；
2. 未锁定时，位于指针处加上其偏移量的拖放图标；
3. 锁定时，位于输出原点的该输出的锁表面（如果锁屏客户端已提供；否则只有黑色）；
4. 未锁定时，每一个包围盒（包括弹出窗口）与该输出重叠的已放置窗口，自顶向下，通
   过 `Window::render_elements` 在 `(location - output.global_location)` 处生
   成，并按输出缩放比例转换为物理像素。弹出窗口是其父窗口元素的一部分。

两个后端绘制同样的列表；只有呈现方式不同。

**输出归属与分数缩放。** 合成一个输出之后，后端调用
`render::update_output_membership`。每个包围盒（包括弹出窗口）与该输出矩形重叠的
已放置窗口都会进入该输出，其子表面和弹出窗口各自按自身位置判断（Smithay 的
`output_update` 发送 `wl_surface.enter` 和 `leave`）；其他所有窗口（包括未放置的
窗口）都离开该输出。窗口的首选分数缩放是其包围盒所重叠的输出中最高的缩放比例；不
与任何输出重叠的窗口保留其上一次的首选值。会话锁定期间，该输出的锁表面进入该输出
并首选其缩放比例。在显示于任何地方之前就请求首选缩放的表面（通常是首个缓冲区之前
的新窗口），从新窗口将被放置的那个输出（指针下方的输出，否则是第一个输出）的缩放
比例开始，因此它的第一帧就以正确的分辨率绘制
（`FractionalScaleHandler::new_fractional_scale`）。

**帧回调。** 每个输出的渲染过程之后，`render::send_frame_callbacks` 把帧回调发给
与该输出重叠的每个已放置窗口，包括弹出窗口和子表面（`Window::send_frame`，以该输
出作为这些表面的主输出），并把 DnD 图标自己的帧回调发给它；锁定期间只有该输出的
锁表面会收到帧回调，与所绘制的内容一致。光标表面在每次渲染过程中都会收到其回调。

**`render::refresh(app)`** 在每次循环迭代中、渲染之前运行一次，只做弹出窗口簿记
和死亡客户端回收，顺序如下：`PopupManager::cleanup`，它记录客户端已销毁的弹出窗
口，并可能结束一条弹出窗口抓取链；`App::settle_popup_grab`，它释放已经结束的抓取
链，并把该抓取链所持有的键盘焦点交还给抓取链的根窗口（见[协议](#协议)）；然后是
回收，因此回收的焦点回退永远不会把焦点从已关闭菜单所属的窗口上夺走。回收会丢弃
`IsAlive` 为 false 的窗口、死亡的空闲抑制表面、死亡的 DnD 图标以及已死亡的被记住
的锁前焦点，在键盘焦点死亡时运行焦点回退，在锁定期间把实时键盘焦点保持在一个存活
的锁表面上，并在回收了窗口时刷新指针焦点。正常的 `xdg_toplevel.destroy` 或 XWM
取消映射会立即移除其窗口；这一过程是为进程突然死亡而存在的：一次 SIGKILL 压力测
试表明，仅靠协议的销毁回调会留下死亡窗口。

## 后端

### 嵌套后端（`backends/winit.rs`）

嵌套后端通过 `smithay::backend::winit::init_from_attributes` 和一个
`GlesRenderer`，把合成器作为一个 1280×800、标题为 `nkdhr-compositor` 的客户端窗
口，运行在开发者所使用的任意 X11 或 Wayland 桌面中。宿主光标在该窗口上方被隐藏。

它拥有一个名为 `nkdhr-compositor-nested-0`（制造商 `nkdhr`，型号 `nested`）的
`Output`，其模式为窗口尺寸、60 Hz。窗口调整大小或新的 outputs 代次会重新解析布
局。dmabuf 全局对象通过 `DmabufState::create_global`（协议版本 3）依据渲染器的格
式列表创建。监听套接字来自 `ListeningSocket::bind_auto("wayland", 0..32)`，由
`run` 自身而不是某个事件源持有，每次循环迭代接受一次连接。XWayland 在自己的
calloop 循环上运行，每次迭代以零超时调度一次。

每次迭代依次：调度 winit 事件（输入交给 `input::handle`）、在窗口被请求关闭或收
到关闭信号时离开循环、按需重新解析布局、调用 `render::refresh`、渲染整个窗口、处
理 screencopy、提交帧、更新输出归属、发送帧回调、接受一个待处理的客户端、调度
XWayland 和 Wayland 客户端、调用 `App::finish_client_dispatch`，然后 flush。无论
是出于这两种原因中的哪一种，还是因为出错而离开循环，都会执行[关闭](#关闭)中所述
的拆除过程。嵌套输出的 `wl_output` 全局对象的存活时间与进程相同。它没有自己的
damage 跟踪或帧节奏控制：每次迭代都重绘整个窗口，并以宿主所能接受的最快速度交换
缓冲区，表现为持续的 CPU 占用。对于开发用后端来说这是可以接受的。它每五秒记录一
次帧率和窗口数。

它以嵌套输出的缩放比例，通过 `draw_render_elements` 绘制这些元素；TTY 后端的 DRM
合成器从其 `Output` 取得的也是这一缩放比例。元素携带的是物理位置，但根据绘制时所
用的缩放比例来确定自身尺寸（表面的逻辑尺寸或视口目标尺寸乘以该缩放比例），因此以
任何其他缩放比例绘制，都会把窗口放在正确的位置，却以错误的尺寸绘制它们。相应地，
内置的后备光标被赋予其逻辑尺寸，而从不是预先乘以缩放比例的尺寸。

以下是从 Smithay 的示例中看不出来的细节：

- **`WinitEvent::CloseRequested` 必须显式结束循环。** Smithay 的
  `examples/minimal.rs` 忽略了它，这会让进程在其窗口关闭后继续运行。
  `PumpStatus::Exit` 是一个单独的信号（winit 自身的循环终止），并不会仅仅因为窗
  口关闭就触发。
- **调整大小不需要专门处理。** `WinitGraphicsBackend::bind()` 每一帧都会重新查询
  窗口尺寸并调整其 `EGLSurface` 的大小。
- **`Transform::Flipped180`** 把 GL 位于左下角的帧缓冲原点与 Smithay 位于左上角
  的逻辑约定对齐，与 `minimal.rs` 的做法相同。
- **Screencopy 回读所需的重新绑定。** `GlesRenderer::map_texture` 会让 EGL 上下文
  保持为当前上下文，但不带窗口表面。在 `submit` 之前，一次空操作的渲染过程会重新
  绑定它；没有这一步时，第一次真正的 `grim` 捕获导致 EGL 报告 `BadSurface`，其尝
  试的恢复以 `BadAlloc` 失败，并使合成器崩溃退出。
- 在无头、软件渲染的宿主下，Mesa 会为嵌套 EGL 上下文回退到
  llvmpipe（`failed to get driver name for fd -1`）。这对嵌套后端来说是预期行
  为；真正的 GPU 加速由 TTY 后端来检验。

### TTY（`backends/tty.rs`）

TTY 后端基于 calloop。`LibSeatSession` 掌管设备访问以及暂停/恢复；`UdevBackend`
枚举 DRM 设备并处理其热插拔；`DrmScanner` 把 connector 映射到 CRTC；
`DrmOutputManager` 拥有由 GBM 支撑的原子 KMS 表面。整个过程不涉及任何 setuid 辅
助程序或 nkdhr 自有的特权代码。会话最先被打开；XWayland 在 DRM 设备初始化之前启
动；dmabuf（版本 3）和 SHM 的格式列表来自主 GPU 的渲染器。

**GPU。** 主渲染 GPU 是 `NKDHR_DRM_DEVICE`（如已设置），否则是 seat 主 GPU 的渲
染节点，再否则是 seat 上的第一个渲染节点。它被直接打开，并以高上下文优先级注册到
`GpuManager<GbmGlesBackend<…>>`。渲染节点的描述符保存在一个不具备任何 DRM 控制
trait 的独立 `RenderDeviceFd` 类型中，因为构造 `DrmDeviceFd` 会尝试获取 DRM
master；渲染节点永远无法接管显示。KMS 主节点通过 libseat 单独打开，并且只针对有
资格进行 scanout 的设备。渲染在主 GPU 上进行，当某个 scanout 设备有自己的渲染节
点时，再跨 GPU 拷贝；仅用于 scanout 的设备（例如 VKMS）借用主渲染节点的分配器，
并把格式限制为线性 modifier。

**开发安全边界。** `NKDHR_DRM_DEVICE` 必须指定一个渲染节点，
`NKDHR_DRM_SCANOUT_DEVICE` 必须指定一个主节点；其他任何值都会在启动时被拒绝。设
置了 scanout 覆盖时，其他任何 DRM 主节点都根本不会被打开（无论是在启动时还是热插
拔时），而不仅仅是被排除在 connector 扫描之外。这使得 VKMS 运行可以在真实 GPU 上
渲染，而不会在开发者的真实面板上获取 DRM master 或进行模式设置。生产环境会正常发
现渲染 GPU 并打开 seat 的 KMS 设备。合成器通常运行在本地 seat 会话中；
`LIBSEAT_BACKEND=noop` 只适用于隔离的 VKMS 诊断。

**输出。** 一个已连接的 connector 成为一个名为
`<interface>-<interface id>`（`eDP-1`、`HDMI-A-1`、`Virtual-1`）的 `Output`，以
其首选模式（或其列出的第一个模式）驱动，并拥有自己的 `wl_output` 全局对象。该全
局对象只有在输出的 KMS 表面初始化完成后才会创建，因此无法驱动的 connector 永远不
会被公布。当 connector 或其整个设备消失时，丢弃该输出的 `SurfaceData` 会让全局对
象退役：它会被立即禁用（这会宣告其移除，同时保持其可绑定），并在十秒后由一个
calloop 定时器移除。立即移除会在客户端的绑定可能已经在路上时释放它（客户端刚刚看
到它被公布，或者正处于其第一次 registry 往返中），而 wayland-server 会以协议错误
杀死这样的客户端；wayland-server 推荐的正是这种先禁用、后移除的顺序。scanout 格
式优先选择 8 位 ABGR/ARGB，10 位格式只作为后备保留：在 nkdhr 拥有 HDR/色彩管理策
略之前选择 10 位，只会徒增输出端抖动和驱动特定的行为，毫无益处。任何 connector
变化都会重新解析布局。

**事件循环。** 事件源包括 Wayland 套接字、display（一个 `Generic` 源，调度客户
端，然后调用 `App::finish_client_dispatch`）、udev、libinput、会话通知器、每个设
备一个用于 vblank 的 DRM 通知器，以及 XWayland。每次迭代以 250 ms 为上限进行调
度，在收到关闭信号时离开循环，在 outputs 代次变化时重新解析布局，调用
`render::refresh`，渲染每个输出并 flush 客户端。客户端提交、输入、vblank 和热插
拔会立即唤醒循环；这个上限的存在，是为了让监听线程发布的配置变更以及关闭信号在原
本空闲的会话上也能在四分之一秒内生效。信号本身不会唤醒循环：`epoll_wait` 被信号
中断时，calloop 会重新调用它，而不是返回。会话暂停期间循环仍在继续调度，因此当另
一个 VT 位于前台时，关闭请求同样会得到响应。无论是因为关闭信号、恢复失败
（`running` 被清除）还是出错而离开循环，都会执行[关闭](#关闭)中所述的拆除过程。

**呈现与 damage。** `DrmOutput::render_frame` 提供了 Smithay 的逐输出 damage 跟
踪，因此未发生变化的输出会产生一个不被排队的空帧，空闲时的 GPU 占用保持在接近
零。每个输出至多允许一个已渲染的帧处于在途状态：当一帧在等待 vblank 时，后续的输
入和客户端变化会累积在合成器状态中，而不是被渲染到中间缓冲区，`frame_submitted`
之后的第一次渲染过程呈现的是最新状态。这使指针移动与呈现保持同步，而不是在高频率
输入下排队一堆过时的位置。普通帧只允许光标平面
scanout（`FrameFlags::ALLOW_CURSOR_PLANE_SCANOUT`），从不进行主平面或叠加平面的
直接 scanout，这消除了软件光标的拖影，同时让合成后的主平面对 screencopy 保持可
读。

**热插拔。** 被移除 connector 的 KMS 表面会在其 `DrmOutput` 被丢弃之前被显式清除
（DPMS 关闭、禁用所有平面），然后该设备上剩余的输出会尝试恢复显式缓冲区
modifier。因此重新连接时会从一个干净的 CRTC 开始，而不是从过时的帧缓冲或平面分配
开始。移除整个设备会注销其 DRM 通知器，若为次要 GPU，还会注销其渲染节点。客户端
和窗口能在每一次热插拔中存活，包括恰好在其 connector 消失时绑定 `wl_output` 的客
户端。

**VT 切换与交接。** 每个输入事件之后，后端取走 `take_vt_switch_request`；针对合
成器当前已占用的 VT（从 `/proc/self/fd/0` 读取）的请求不做任何事。否则，在请求
libseat 切换之前，后端停止渲染，执行一次禁用所有 connector 和平面的设备级原子重
置，暂停每个 DRM 输出管理器并释放 master，然后才调用
`LibSeatSession::change_vt`。只在这一系列操作之后才请求切换，避免了 logind 在重
置运行之前就撤销设备访问的竞态。如果重置或切换失败，后端会立即重新激活并重新扫描
设备，如果连这一步也失败则停止运行。异步的暂停事件会幂等地重复暂停，并挂起
libinput。激活时会恢复 libinput，以 connector 和平面已重置的状态重新激活 DRM，对
每个输出调用 `DrmOutput::reset_buffers`，清除在途标记并重新扫描 connector，而不
触碰客户端、窗口或焦点。缓冲区重置很重要：新的 swapchain 槽位的缓冲区年龄
（buffer age）为零，会强制完整重绘，而保留暂停前的年龄曾使第一次物理 VT 恢复测试
只重绘了一块受损区域，直到下一个输入事件为止。

## 关闭

`SIGTERM`、`SIGINT` 和 `SIGHUP` 请求合成器停止。第一个这样的信号启动优雅关闭；在
先前的 `SIGTERM` 或 `SIGINT` 之后到达的 `SIGTERM` 或 `SIGINT` 会立即结束进程。
`SIGHUP` 不参与这一机制：它既不会强制退出，也不会使之后的信号能够强制退出，因为
挂断信号通常紧跟在另一个信号之后到达。logind 以 `SendSIGHUP=yes` 停止登录会话的
scope，因此结束会话（`loginctl terminate-session`、注销、关闭系统）会先投递
`SIGTERM`，紧接着投递 `SIGHUP`；而关闭终端可能会让 shell 和内核各投递一次
`SIGHUP`。如果挂断信号参与了强制退出，恰恰是这些普通的停止方式可能会跳过清理。

合成器启动时已被忽略（`SIG_IGN`）的停止信号保持被忽略。否则 `signal-hook` 会用它
的处理函数替换这一处置方式，但这是启动者的决定：`nohup` 忽略 `SIGHUP`，以便程序
比其终端存活得更久；而没有作业控制的 shell 会在它于后台启动的命令中忽略
`SIGINT`。

**处理函数。** `ShutdownSignal::install`（`shutdown.rs`）是 `main` 所做的第一件
事，早于后端创建任何套接字、锁文件、线程或子进程，因此不存在这样一个时刻：这些东
西已经存在，而信号仍会以其默认动作杀死进程。对每个停止信号，它先通过 `sigaction`
查询当前的处置方式，并跳过已被忽略的信号；否则它注册 `signal-hook` 的 flag 动
作，这些动作按注册顺序运行。对于 `SIGTERM` 和 `SIGINT`：先是
`flag::register_usize(signal, forcing, signal)`，然后是
`flag::register_conditional_shutdown(signal, 1, forcing_seen)`，再然后是
`flag::register(signal, forcing_seen)`。对于 `SIGHUP`，只有
`flag::register_usize(signal, other, signal)`。第一次收到 `SIGTERM` 或 `SIGINT`
时，信号编号被记录下来，条件关闭发现 `forcing_seen` 仍为 false 而什么也不做，随
后 `forcing_seen` 被置位。之后再收到一次时，`forcing_seen` 已经置位，条件关闭便
从处理函数中调用 `_exit(1)`。记录排在最前，是因为信号可能在两次注册之间到达：这
样在每一种部分注册的状态下，该信号要么记录一个停止请求，要么什么也不做；而如果在
记录存在之前就先置位 `forcing_seen`，落在这个窗口中的信号可能不会被记录，并使下
一次普通的停止请求在没有清理的情况下强制退出。这些动作只触及原子变量和 `_exit`，
两者都是异步信号安全的；合成器的其他任何部分都不在信号上下文中运行。

`SIGHUP` 不置位 `forcing_seen` 之所以重要，是因为两个信号未必按发送顺序被处理。
内核把发给进程的信号投递给任意一个未阻塞该信号的线程。当 logind 的 `SIGHUP` 到
达、而主线程尚未运行其 `SIGTERM` 处理函数时，内核会优先把它交给另一个空闲线程
（zbus 的线程或 `outputs` 监听器的线程），两个处理函数随后以任意顺序并发运行。假
如挂断信号置位了 `SIGTERM` 处理函数所检查的标志，该 `SIGTERM` 就可能发现它已被置
位，从而在没有清理的情况下结束进程；一个带八个空闲线程的独立复现程序大约每十次运
行就有一次以这种方式结束。

`ShutdownSignal::received` 在记录了 `SIGTERM` 或 `SIGINT` 时返回其名称，否则返回
所记录的 `SIGHUP` 的名称（`signal_hook::low_level::signal_name`），在尚无信号到
达时返回 `None`。两类信号分开记录，是因为出于同样的原因，无论是第一个还是最后一
个被记录的信号，都不一定是 logind 的 `SIGTERM`。

刻意不使用 calloop 的 `Signals` 事件源。它基于 `signalfd`，后者要求这些信号被阻
塞，而它只在创建它的线程中阻塞这些信号。信号掩码会被此后创建的线程继承，并且在
`fork` 和 `exec` 之后依然保留，因此 Xwayland 和其他每个子进程启动时都会阻塞
`SIGTERM`、`SIGINT` 和 `SIGHUP`，无法被它们停止；而更早创建的线程（`outputs` 监
听器的线程、zbus 的线程）不会阻塞它们，内核投递给这种线程的信号仍会在没有清理的
情况下结束进程。通过 `sigaction` 安装的处理函数不改变任何掩码，并且 `exec` 会在
子进程中把它们重置为默认动作。

**轮询。** 每个后端在每次循环迭代中检查一次 `ShutdownSignal::received`。当它返回
一个名称时，后端记录 `nkdhr-compositor: received <SIGNAL>, shutting down` 并离开
其循环。这一行由 `shutdown::log_shutdown` 写出，它会忽略写入错误，而不是像
`println!` 那样 panic：在 `SIGHUP` 之后，stdout 所指向的终端往往已经挂断。TTY 循
环会在其 250 ms 的调度上限之内察觉（见 [TTY](#ttybackendsttyrs)）。嵌套循环在其
下一次迭代时察觉，通常在一帧之内。不过，它的缓冲区交换要等待宿主的帧回调（适用
EGL 默认的交换间隔 1；Smithay 从不改变它），而有些宿主会停止向它们不显示的窗口发
送帧回调：GNOME 或 KDE 上被最小化的窗口，sway 或 Hyprland 上位于隐藏工作区的窗
口。当嵌套窗口这样被隐藏时，停止请求只有在它再次显示之后才会生效。取消交换的节流
会让嵌套后端变成忙循环，对于开发用后端来说并不值得；第二个 `SIGTERM` 或 `SIGINT`
仍会立即结束它，不做清理。

**为什么丢弃事件循环还不够。** 大多数在 `Drop` 时会释放进程之外某种资源的对象
（TTY 的 Wayland 套接字、带有 X11 锁的 XWayland 句柄、TTY 的 DRM 通知器和
libseat seat）都由 calloop 事件源持有，而 calloop 只有在指向某个循环的最后一个
`LoopHandle` 消失时才会释放该循环的事件源。持有指向自身所在循环的句柄的回调构成
一个引用环，它会让该循环及其中的每个事件源在 `EventLoop` 值被丢弃之后依然存活。
运行 XWayland 的循环（TTY 唯一的循环、嵌套后端的 XWayland 循环）在 XWayland 一经
生成就会出现这样的环：合成器自己的 XWayland 回调持有一个句柄以启动窗口管理器，而
`X11Wm::start_wm` 注册 Smithay 的 X11 事件源时所用的回调也持有一个句柄，这一点
nkdhr 无法改变。因此在 `run` 结束时丢弃 `EventLoop` 不会释放任何东西。每个后端转
而通过一个 `SourceGuard`（`backends/mod.rs`）持有每一个这样的事件源，guard 被丢
弃时会把该事件源从其循环中移除；`LoopHandle::remove` 会立即丢弃该事件源及其回
调。这些 guard 的持有方式保证了离开 `run` 的每一条路径都会丢弃它们：在嵌套后端中
由 `run` 的局部变量持有；在 TTY 上由 `TtyState` 持有，其 `Drop` 按下文给出的顺序
移除它们，此外还由每个 DRM 设备的 `DeviceData` 持有。这涵盖了关闭信号、嵌套窗口
关闭、TTY 恢复失败、循环中出错、事件源注册之后的启动失败（包括某个 DRM 设备初始
化失败），以及经由 `run` 展开（unwind）的 panic，但在 TTY 上有一个例外：在持有
wayland-backend 状态互斥锁期间引发的 panic 会使该锁中毒（poison），随后丢弃
XWayland 句柄会在拆除过程中再次 panic，从而中止（abort）进程。Wayland 套接字在这
一步之前就已释放，因此这种情况下损失的只是 X11 文件和之后的各个步骤。
`backends/mod.rs` 中的一个单元测试固定了这一切所依赖的 calloop 行为。

**释放的内容。**

- Wayland 套接字。wayland-server 的 `ListeningSocket` 在被丢弃时删除
  `$XDG_RUNTIME_DIR/wayland-N` 和 `wayland-N.lock`。在 TTY 上，它位于
  `register_wayland_sources` 所注册的 `ListeningSocketSource` 中，该事件源会被移
  除；嵌套后端则在 `run` 中、任何循环之外持有一个普通的 `ListeningSocket`，它在
  返回时被丢弃。
- XWayland。移除持有 Smithay `XWayland` 句柄的事件源（在 TTY 循环中由
  `TtyState::start_xwayland` 注册，嵌套后端则把它注册在自己的 XWayland 循环中）
  会丢弃该句柄：这会断开 Xwayland 客户端，从而使服务器退出，并丢弃句柄所拥有的
  `X11Lock`，后者会删除 `/tmp/.X11-unix/X<n>` 和 `/tmp/.X<n>-lock`
  （见 [XWayland](#xwayland)）。
- 在 TTY 上，Wayland 套接字事件源最先被移除，XWayland 事件源其次；嵌套后端同样先
  丢弃其套接字，再丢弃其 XWayland guard。然后是每个 DRM 设备
  （`TtyState::devices`）连同其通知器事件源（`DeviceData::_notifier`）。Smithay
  的 `DrmDeviceNotifier` 与设备共享其内部状态，因此只有当通知器和设备都已不在
  时，原子 DRM 设备才会（在会话处于活动状态时）恢复它在启动时发现的 KMS 状态（文
  本控制台），并释放 DRM master。然后是 libinput 事件源。libinput 上下文同样由
  `TtyState` 持有，会话回调通过它挂起和恢复该上下文，因此仅移除事件源并不会关闭
  输入设备；拆除过程会挂起该上下文，从而在 seat 仍处于打开状态时通过它关闭这些设
  备。最后是会话通知器：`LibSeatSessionNotifier` 拥有 libseat seat
  （`LibSeatSession` 只持有对它的弱引用），丢弃它会关闭 seat，从而把 VT 交还给
  seat 管理器（logind 或 seatd）。

文件最先被删除，因此之后失败的拆除步骤不会把它们遗留下来。优雅关闭以状态 0 从
`main` 返回。

**不会被清理的内容。** 强制退出（从处理函数中调用 `_exit(1)`）、`SIGKILL` 以及不
展开的崩溃（诸如 `SIGSEGV` 的致命信号、abort）会跳过这一切。它们遗留下的东西会在
下次启动时被接管或跳过：wayland-server 在能够锁住某个套接字名称的 `.lock` 文件时
（内核已释放死亡进程的 `flock`）接管该名称，并在绑定之前删除过时的套接字；
Smithay 的 `X11Lock` 会接管所记录的 PID 已不存在的锁文件，并在绑定之前删除过时的
X11 套接字，而当所记录的 PID 属于一个正在运行的进程时，则改用下一个 display 编
号。

## 协议

`ProtocolState`（`protocols/mod.rs`）拥有两个后端共享的全局对象和策略。仅靠注册
往往不够：选区跟随键盘焦点，约束改变移动事件的投递方式，锁表面取代整个场景，
screencopy 则回读合成结果。

- **核心。** `wl_compositor`/`wl_subcompositor`、`wl_shm`、
  `zwp_linux_dmabuf_v1`（版本 3）、`xdg_wm_base`、一个 `wl_seat`、每个输出一个
  `wl_output`，以及 `zxdg_output_manager_v1`。dmabuf 缓冲区在不进行急切导入的情
  况下即被接受（`notifier.successful`）；真正的导入发生在某个渲染元素首次使用该
  缓冲区时，不兼容的缓冲区会表现为渲染时的导入错误。
- **弹出窗口。** `PopupManager` 跟踪、渲染、命中测试并抓取 xdg 弹出窗口。
  `xdg_popup.grab` 只在合成器的 seat 上受理，并且在会话锁定期间从不受理（弹出窗
  口不是锁表面）。它的 serial 必须是最近一次按键按下、按键释放、按钮按下或触摸按
  下的 serial，并且该操作必须已送达该弹出窗口的客户端（`App::user_actions`，由输
  入路由器记录）。届时该操作可能已经结束，并且与 Mutter 一样，按下在其释放之后仍
  然有效：GTK 在按钮释放或手指抬起时才打开菜单按钮的 popover，并指明的是按下或触
  摸按下；即使是在按下时打开的菜单，慢速客户端的抓取请求也可能在释放之后才到达；
  而从键盘打开的菜单指明的是最近一次按键按下或释放。Smithay 的
  `has_grab(serial)` 只匹配当前活动的抓取，而释放已经结束了该抓取，因此它不被参
  考；键盘焦点同样不被参考，因为触摸从不移动键盘焦点。每种操作只保留其最近一次，
  并且该记录在会话锁开始时被清除，因此过期的 serial 无法打开菜单。抓取还必须可以
  开始（`state.rs` 中的 `may_start_popup_grab`）。不得有其他客户端的弹出窗口抓取
  链处于打开状态：Smithay 每个 seat 只维护一条抓取链，对于父窗口不是该链最顶层弹
  出窗口的抓取，会以一个协议错误（`not_the_topmost_popup`）作答，从而断开发出请
  求的客户端。并且抓取必须可以接管指针：指针没有抓取，或者其抓取是某个菜单的
  `PopupPointerGrab`（该客户端自己的外层菜单，或一条已经结束但尚未释放的抓取
  链），或者是 Smithay 的隐式 `ClickGrab` 且起始于无表面、已死亡的表面或该弹出窗
  口客户端的某个表面。接管打开菜单的那次按下的隐式抓取，或者外层菜单的抓取，正是
  菜单得以打开、嵌套和相互替换的方式。移动、调整大小或拖动会保留指针，另一个客户
  端按住的按钮也是如此：一个应用程序的菜单不得从另一个应用程序那里夺走按住的按
  钮。被授予的抓取会安装 `PopupPointerGrab` 和 `PopupKeyboardGrab`，把键盘焦点移
  到弹出窗口，提升弹出窗口的根窗口（见[焦点](#焦点)），并保存在
  `App::popup_grab` 中。未被授予的抓取（无论是出于上述任何原因，还是因为 Smithay
  的 `grab_popup` 拒绝了它）会立即解除该弹出窗口（通过其弹出窗口树，这也会让它停
  止被绘制），这是 xdg-shell 的要求；否则客户端会显示一个没有任何东西能关闭的菜
  单。

  只要其中某个弹出窗口仍持有抓取，抓取链就处于打开状态；一旦每个抓取弹出窗口都已
  被销毁或解除，或者其根窗口已消失，抓取链就结束。这比 Smithay 的
  `PopupGrab::has_ended` 更早，后者还要等待客户端销毁已被解除的弹出窗口：在一个
  菜单之外点击之后，下一个菜单常常在第一个客户端销毁其弹出窗口之前就打开了，而
  Smithay 此时已经会接受它。抓取链以下列方式之一关闭：

  - 其客户端销毁这些弹出窗口（选中了某一项、Escape）。Smithay 的
    `PopupPointerGrab` 和 `PopupKeyboardGrab` 只有在下一个指针事件或按键时才会注
    意到这一点，在此之前会把指针焦点保持在该客户端上，并阻止下一次点击聚焦窗口；
    在触摸屏上或仅使用键盘时，这种状态可能无限期地持续下去。因此
    `App::settle_popup_grab` 在每次循环迭代中运行，于 `render::refresh` 中在
    `PopupManager::cleanup` 之后调用。一旦抓取链结束，它会先结束键盘抓取，然后结
    束指针抓取并恢复指针焦点，把键盘焦点交还给抓取链的根窗口
    （见[焦点](#焦点)），并丢弃 `App::popup_grab`。在抓取链打开期间，它把键盘焦
    点保持在最顶层的抓取弹出窗口上；在嵌套弹出窗口关闭后，Smithay 的键盘抓取只会
    在下一次按键时才更新焦点。
  - 在抓取链所属客户端之外的点击。Smithay 的 `PopupPointerGrab` 解除每一个抓取弹
    出窗口（`PopupGrab::ungrab` 配合 `PopupUngrabStrategy::All`），结束自身和键
    盘抓取，把键盘焦点交还给根窗口，并投递该按下事件。该点击不会同时聚焦它所落在
    的窗口（见[输入路由](#输入路由)）。
  - 在抓取链所属客户端之外的触摸。输入路由器通过 `App::dismiss_popup_grab` 做同
    样的事，然后投递该触摸。
  - 会话锁，它以同样的方式解除抓取链（见[会话锁](#会话锁)）。

  通过 `PopupGrab::ungrab` 解除（而不是仅发送 `popup_done`）还会让 Smithay 的每
  seat 抓取链忘掉这些弹出窗口，因此一个从不销毁它们的客户端（挂起或被停止的客户
  端）无法留下一个最顶层弹出窗口，使下一个客户端的菜单变成协议错误。父窗口已死亡
  的弹出窗口会被解除。`reposition` 请求会应用新的定位器（positioner）。弹出窗口
  被精确放置在其定位器指定的位置；合成器不会为了让它们保持在某个输出上而滑动或翻
  转它们。
- **窗口状态请求。** `xdg_wm_base` 以空的窗口管理能力集创建，因此
  `xdg_toplevel.wm_capabilities` 告诉客户端最大化、全屏、最小化和窗口菜单均不可
  用，遵守这一点的客户端会隐藏这些控件。Smithay 的默认 handler 仍然会应答这些请
  求：最大化和全屏请求会收到一个保持状态不变的 configure；取消最大化、取消全屏、
  最小化和窗口菜单请求会被忽略。来自 X11 客户端的同类请求也会被忽略。
- **剪贴板与主选区**（`wl_data_device`、`zwp_primary_selection_v1`）。选区焦点跟
  随键盘焦点。选区字节通过协议管道直接在客户端之间传输；合成器只中转元数据和文件
  描述符。
- **拖放。** 与数据设备共享状态。拖放目标是指针下方最顶层的表面；可选的图标绘制
  在指针处，按拖动开始时记录的光标热点偏移，并在放下时被清除。会话锁定期间只有锁
  屏客户端可以拖动（见[会话锁](#会话锁)）。
- **装饰**（`zxdg_decoration_manager_v1`）。每个 `new_decoration`、
  `request_mode` 和 `unset_mode` 都以 `ClientSide` 作答：在 toplevel 的初始
  configure 之前，该模式随初始 configure 一起发送；之后则立即发送一个
  configure。合成器不绘制任何形式的装饰。从不绑定该协议的客户端按 xdg-shell 的要
  求自行绘制装饰。
- **指针约束**（`zwp_pointer_constraints_v1`）。约束只有在以下情况下才会被激活：
  其表面持有指针焦点、是指针下方的表面，并且指针位于约束区域之内（没有区域的约束
  覆盖整个表面）：`input::activate_pointer_constraint` 在每次移动更新指针焦点之
  后检查这一点，并在约束创建时检查一次。检查实际的指针焦点，可以让约束在抓取拖着
  指针经过的表面上保持未激活：合成器的移动或调整大小会清除指针焦点，而 Smithay
  的隐式点击抓取把焦点保持在被按下的表面上。检查区域可以防止限制（confinement）
  在区域之外开始，否则每一次移动都会被拒绝，光标会冻结。被锁定的指针会收到相对移
  动，而合成器光标保持不动；Smithay 保留光标位置提示。被限制的指针会拒绝会离开表
  面或区域的移动。当指针焦点移到另一个表面，或其表面不再位于指针下方（它移动了、
  取消映射了或被遮挡了）时，约束会被停用。锁定路径从不激活约束。
- **相对指针**（`zwp_relative_pointer_manager_v1`）：由相对移动提供数据，包括在
  指针锁定生效期间。
- **指针手势**（`zwp_pointer_gestures_v1`）：触摸板的滑动、捏合和按住手势由输入
  路由器转发，到达拥有指针焦点的客户端。
- **空闲抑制**（`zwp_idle_inhibit_manager_v1`）。抑制表面按每个抑制器跟踪一次
  （`IdleInhibitors`）：协议允许一个表面上有多个抑制器，而 Smithay 分别报告每一
  次创建和销毁，因此一个表面会持续抑制，直到其最后一个抑制器被销毁。当任何存活的
  抑制表面属于一个与已连接输出重叠的已放置窗口时，`App::idle_inhibited()` 为真。
  它是会话的空闲与显示电源策略将要使用的唯一查询；该使用方目前尚不存在，因此该方
  法带有一个有文档说明的 `dead_code` 许可。
- **分数缩放**（`wp_fractional_scale_manager_v1`）：首选缩放由[渲染](#渲染)中所
  述的输出归属过程维护，该过程也为新表面提供初始首选值。客户端据此以该缩放比例绘
  制缓冲区，并把表面的逻辑尺寸设为其视口目标尺寸，因此该协议只有与
  `wp_viewporter` 一起才能使用。找不到 viewporter 的客户端改为以整数缩放比例绘制
  （即向上取整后的分数缩放比例，与 `wl_output` 所公布的一致），合成器随后再将其
  缩小。
- **视口**（`wp_viewporter`）：`ProtocolState` 中的 `ViewporterState`，通过
  `delegate_viewporter!` 调度。Smithay 在合成器已在使用的提交路径中完成协议工
  作，因此 nkdhr 不添加任何自己的 handler。Smithay 随表面的第一个视口对象安装的
  预提交钩子会拒绝没有目标尺寸的非整数源尺寸（`bad_size`）；随后
  `on_commit_buffer_handler` 用 `ensure_viewport_valid` 对照缓冲区检查源矩形
  （`out_of_buffer`），并把裁剪区域和目标尺寸存入表面的 `RendererSurfaceState`
  视图。所有确定表面尺寸的地方都读取该视图，而从不读取缓冲区：渲染（经过裁剪和缩
  放的表面元素）、`Window::bbox` 和 `Window::geometry`（并经由它们影响放置、保持
  可达规则、输出归属和帧回调）、通过 `Window::surface_under` 和输入区域进行的命
  中测试，以及 Smithay 的锁表面尺寸检查，它把视口目标尺寸与 configure 时给出的尺
  寸进行比较（`dimensions_mismatch`）。Screencopy 回读的是合成后的帧，不需要任何
  处理。nkdhr 中唯一的缓冲区查询，是在放置 toplevel 时查询是否已附加缓冲区。
- **光标。** 客户端光标表面按其热点绘制；命名光标和背景使用内置的 24×24 箭头，并
  按输出缩放以保持其逻辑尺寸。
- **XWayland shell**（`xwayland_shell_v1`）：把 X11 窗口与其 `wl_surface` 关联起
  来；见 [XWayland](#xwayland)。
- **会话锁**（`ext_session_lock_manager_v1`）和
  **screencopy**（`zwlr_screencopy_manager_v1`）各有专门的章节。

## XWayland

X11 支持是**进程内**的：Smithay 的 `XWayland` 生成 Xwayland 服务器，合成器本身通
过 `X11Wm`/`XwmHandler` 充当 X11 窗口管理器。`xwayland-satellite` 风格的外部代理
能带来崩溃隔离和惰性启动，但也会多出第二个进程和一层 IPC 面；对于一个作为单一进
程来审计的合成器而言，进程内模型是更简单的信任边界，而且它是 Smithay 一等支持的
模型。

- **启动。** XWayland 在两个后端上都于启动时生成，使用自动选择的 display 编号和
  Smithay 清理过的环境，该环境刻意保留了 `LD_LIBRARY_PATH`，从而非系统运行时
  （Nix profile、解包的诊断软件包）仍能正常工作。当服务器报告 `Ready` 时，运行
  `X11Wm::start_wm`，合成器记录 `XWayland ready on DISPLAY=:<n>`。缺少
  `Xwayland` 可执行文件时会记录日志，此外不予理会。
- **Display 锁。** `/tmp/.X<n>-lock` 文件和 `/tmp/.X11-unix/X<n>` 套接字属于
  Smithay 的 `X11Lock`，由注册为事件源的 `XWayland` 句柄拥有，而不是由 Xwayland
  进程拥有。在 `Ready`（或 `Error`）之后，该事件源会禁用自身而不是被移除，因此句
  柄和锁在整个会话期间都保持存活，这些文件在后端于关闭时移除该事件源时才被删除
  （见[关闭](#关闭)）。Xwayland 运行在合成器的进程组中，因此在启动嵌套合成器的终
  端中按 `Ctrl+C` 也会传到它那里，它会自行退出；这些文件照样会被删除。
- **坐标。** X11 根坐标空间就是合成器全局逻辑空间，一一对应。因此受管窗口在其存
  储位置发生变化时（放置、移动、调整大小）都会被 configure 到该位置，从而 X 服务
  器所认为的每个窗口的位置始终与合成器一致；X11 客户端依据它来定位自己的
  override-redirect 菜单。
- **Configure 请求。** 对于尚未映射的窗口，请求按原样应用（随后由映射时的放置决
  定位置）。对于已映射的受管窗口，应用所请求的尺寸，位置则保持由合成器决定。
  override-redirect 窗口自行 configure；`configure_notify` 更新其位置。
- **层叠顺序。** 在存储中提升一个受管窗口，也会在 X 服务器中提升它
  （`X11Wm::raise_window`），因此 X11 的层叠顺序与显示内容一致。
- **选区。** 剪贴板和主选区双向桥接。Smithay 0.7 会将新的 X11 选区所有者请求排队
  而不 flush，因此在把 Wayland 选区发布给 X11 之后，合成器会发出一个无害的 RANDR
  请求（`get_randr_primary_output`）作为 flush 屏障；否则 X11 客户端可能读到过时
  的选区。会话锁定期间 X11 客户端无法访问选区。

## 会话锁

`ext-session-lock-v1` 从第一个请求起就是 fail-closed 的：

1. 收到 `lock` 时，会话立即受到保护。首先，当前键盘焦点会作为拥有它的窗口保存到
   `focus_before_lock` 中；当某个弹出窗口通过其抓取持有键盘时，保存的是该弹出窗
   口抓取链的根窗口（见[焦点](#焦点)）。随后，`App::popup_grab` 中的弹出窗口抓取
   链通过 `PopupGrab::ungrab(PopupUngrabStrategy::All)` 被解除，就像在其外部点击
   时那样，根窗口的其他不持有抓取的弹出窗口也随之一并解除：xdg-shell 要求在弹出
   窗口抓取结束时解除这些弹出窗口，否则菜单会一直打开而再没有东西能关闭它；而
   `ungrab` 会让 Smithay 的每 seat 抓取链忘掉它们，因此一个挂起、从不销毁它们的
   客户端无法让解锁后打开的下一个菜单变成协议错误（见[协议](#协议)）。接下来，在
   任何东西能够移动键盘焦点之前，结束键盘抓取：Smithay 的 `PopupKeyboardGrab` 在
   活动期间会忽略焦点变化，因此若不如此，一个打开的弹出窗口在锁定期间会继续接收
   按键；而一个在它活动期间结束的 `PopupPointerGrab` 也会结束它，并把键盘焦点短
   暂地交给菜单所属的窗口。随后每个表面都失去指针焦点，因此进行中的拖放会离开其
   目标并被取消而不是被放下；合成器还会结束任何活动的指针抓取（移动、调整大小、
   弹出窗口、拖放），取消每一个进行中的触摸序列，清除 DnD 图标，清除键盘焦点，并
   发送一次没有焦点的指针移动。被中断的调整大小照常发送其最终 configure。最后，
   用户操作记录（`App::user_actions`）被清除，因此锁定之前的任何 serial 都无法在
   锁定结束后获得弹出窗口抓取。

   触摸需要单独的步骤，因为 Smithay 把触摸序列保持在其开始时所在的表面上：一根按
   在窗口上的手指会持续向该窗口发送其移动，而在它按下期间，Smithay 还会把每一根
   新手指也发往那里。`input::cancel_touch_sequences` 先把触摸拖放从其目标上移开
   （这样它会被取消，而不是被放下），结束触摸抓取，并向每个拥有触摸点的表面发送
   `wl_touch.cancel`；这些触摸点在抬起之前不会到达任何地方。Smithay 的取消只会到
   达那些有比上一帧更新的事件的触摸点，因此每个记录下来的触摸点会先收到一次移动
   到其当前位置的移动事件。
2. 从此以后，输入走路由器的锁定路径，不会开始新的弹出窗口抓取、移动或调整大小，
   渲染只绘制黑色和锁表面，帧回调只发给锁表面，screencopy 只暴露这一合成结果，
   X11 选区访问被拒绝。在此期间映射的窗口会被放置，但不接收输入，也不会被绘制。
3. 锁表面只接受来自持有当前生效锁的客户端。Smithay 会从任何锁对象（包括被拒绝的
   锁对象）交出 `get_lock_surface`，并且只对照该客户端自己的 `wl_output` 绑定检
   查输出，因此若不如此，来自另一个客户端的表面会在屏幕上取代锁屏客户端的表面，
   并接收用户键入的内容。来自任何其他客户端的表面，或者针对不在布局中的输出的表
   面，都会被忽略：从不被 configure、绘制或聚焦。被接受的锁表面会被适配到其输
   出：它先被告知该输出的缩放比例作为其首选分数缩放，然后被 configure 为布局中该
   输出的逻辑尺寸（即 xdg-output 公布的尺寸），因此它为该 configure 绘制的帧已经
   与输出相匹配。每次输出布局变化之后，每个锁表面都会被重新适配
   （`ProtocolState::fit_lock_surfaces`）：从 `outputs` namespace 应用的缩放或位
   置变化，或者嵌套窗口的调整大小，都会在会话保持锁定期间改变输出的逻辑尺寸，而
   停留在旧尺寸的表面会让输出的一部分保持黑色，或让锁屏界面的一部分落在屏幕之
   外。Smithay 只在缩放比例变化时发送缩放比例，只在尺寸与上一次发送或确认的尺寸
   不同时发送 configure。每次渲染之后，锁表面会进入其输出。

   键盘焦点保持在一个存活的锁表面上（`App::keep_keyboard_on_lock_surface`）：每
   当焦点已死亡、缺失或不是当前锁表面之一时，它会移到指针下方输出的锁表面，否则
   移到布局顺序中第一个拥有锁表面的输出的锁表面。这在每个新锁表面出现时、死亡客
   户端回收之后以及每次输出布局变化之后运行。Smithay 会把已销毁的表面保留为键盘
   焦点，直到焦点被再次设置，因此若没有这一步，一个替换了持有焦点的锁表面的锁屏
   客户端（其输出休眠了或被拔出了）在用户点击之前将无法接收键盘输入。点击会把键
   盘焦点移到指针下方的存活锁表面。

   对于任何指明了指针或触摸当前抓取 serial 的客户端，Smithay 都会为其开始拖放
   （在锁定期间，该 serial 就是锁屏客户端自己的按下），并且它没有提供任何拒绝的
   途径。因此，锁定期间由锁屏客户端以外的任何客户端发起的拖动（通过其数据源或图
   标识别；两者都没有的拖动视为外来拖动）会在发起它的那次调度之后立即被
   `App::finish_client_dispatch` 取消，这会结束 Smithay 安装的抓取；其数据源收到
   `cancelled`，其图标从不显示，指针焦点回到锁表面。
4. 合成器只有在每个已连接的输出都呈现了一帧受保护的画面之后才发送 `locked`：在
   TTY 上，是在这样的一帧完成 vblank 时；在嵌套后端上，是在受保护的帧提交之后。
   在此期间断开的输出不再计入。
5. 只有在当前生效且已确认的锁上调用 `unlock_and_destroy` 才会结束锁定。Smithay
   0.7 在任何锁对象上都接受该请求，包括其 `lock` 被拒绝的锁对象：它会发出
   `invalid_unlock`，但随后仍然调用 `SessionLockHandler::unlock`，而后者无法分辨
   是哪个锁发出的请求。因此合成器自己为该接口实现
   `Dispatch<ExtSessionLockV1, _>`，而不是使用 `delegate_session_lock!`：在任何
   其他锁对象上、或者在发送 `locked` 之前在当前生效的锁上调用的
   `unlock_and_destroy`，会收到 `invalid_unlock` 并且不改变任何东西
   （`may_unlock`）；其他所有请求原样交给 Smithay，其余的会话锁接口则像该宏那样
   被委托。一次有效的解锁会重置锁运行时，并在 `focus_before_lock` 所指的表面仍存
   活时恢复它；如果它已死亡，焦点回退会选择最顶层的窗口。随后刷新指针焦点，因此
   指针下方的表面无需移动就能收到解锁后的第一次点击。

在锁定生效期间的第二个锁请求会被拒绝（丢弃它会发送 `finished`），即使第一个锁屏
客户端已经崩溃也是如此：此时会话保持受保护、黑屏且不响应输入，而 VT 切换仍然有
效，因此可以从另一个 VT 恢复。另一个客户端被拒绝的锁既不能解锁会话，也不能显示锁
表面。锁管理器全局对象向每个客户端提供。基于 PAM 的解锁 UI 属于会话部分的工作。

## 屏幕捕获（screencopy）

nkdhr 直接基于 Smithay 的 `wayland_protocols_wlr` 绑定实现了
`wlr-screencopy-unstable-v1`（版本 3）的服务端；Smithay 0.7 没有现成的
screencopy 状态。

- `capture_output` 和 `capture_output_region` 解析 `wl_output`，并以输出的缩放比
  例把区域从逻辑像素转换为物理像素，再与模式求交集。死亡或没有模式的输出，或者空
  区域，会使该帧失败。客户端会得到一个恰好为该尺寸的 `wl_shm` XRGB8888 缓冲区
  （对于版本 3，随后会发送 `buffer_done`）。
- `copy`/`copy_with_damage` 校验缓冲区（格式、尺寸、stride、偏移量和内存池边
  界），拒绝复用 frame 对象，并把请求排在该输出的名称之下。
- 后端用该输出下一个完整合成的帧来完成排队的请求。带 `overlay_cursor` 和不带
  `overlay_cursor` 的请求在相继的帧上处理，因此每个客户端得到的恰好是它所请求的
  光标策略。`copy_with_damage` 把整个区域报告为已损坏（damage），当回读结果是翻
  转的时设置 `YInvert` 标志。
- 在 TTY 上，存在待处理请求的帧会禁用光标平面 scanout，使光标位于帧缓冲中，然后
  把主 swapchain 缓冲区导出为 dmabuf，导入它并拷贝该区域。在嵌套后端上，区域直接
  从窗口帧缓冲中拷贝（配合上文提到的 EGL 重新绑定）。
- 针对断开连接的输出的请求会失败。

## 验证

- **门禁检查。** `cargo fmt --all --check`、
  `cargo clippy --workspace --all-targets --all-features -- -D warnings`、
  `cargo test --workspace --all-features`，以及分别以默认（`nested`）特性和
  `tty` 特性构建本 crate。
- **单元测试** 覆盖布局解析器（包括 xdg-output 舍入和饱和运算）和 `outputs`
  namespace 的解码、初始放置规则、保持可达规则、调整大小的几何计算（边缘、固定的
  对边、尺寸提示、饱和运算）以及跨越最终 configure 和跨越抓取期间移动的调整大小
  状态、只有当前生效且已确认的锁才能解锁的规则、空闲抑制器计数、为弹出窗口抓取保
  留的用户操作 serial、决定弹出窗口抓取能否开始的规则、决定触摸是否解除打开的弹
  出窗口抓取链的规则、VT 按键表、VT 组合键规则和 VT 路径解析器、SDR 优先的
  scanout 格式顺序、后备光标图像、停止信号处理函数（只有重复的 `SIGTERM` 或
  `SIGINT` 才会强制退出、挂断信号从不使之后的信号能够强制退出、信号名称的报告、
  启动时已被忽略的信号保持被忽略），以及 `SourceGuard` 为之而存在的 calloop 行
  为。VT 路径解析器和 scanout 格式测试位于 TTY 后端中，只在启用 `tty` 特性的构建
  中运行，该构建会链接系统的 libseat、libudev、libinput、GBM 和 libdrm。需要真实
  客户端的协议行为（焦点、会话锁、抓取、输出归属）由下面的冒烟运行来验证。
- **嵌套冒烟运行** 使用一个临时的 `weston --backend=headless --renderer=pixman`
  宿主，事后删除。真实客户端（`weston-simple-shm`、`weston-simple-egl`、
  `weston-simple-dmabuf-egl`、`foot`、GTK 4 应用程序、`grim`、
  `wl-copy`/`wl-paste`、经由 XWayland 的 X11 客户端）连接到嵌套套接字。这样的宿
  主没有真实的键盘或指针，因此交互式输入在 TTY 上验证。
- **关闭检查。** 在连接了一个 Wayland 客户端和一个 X11 客户端的情况下，
  `SIGTERM`、`SIGINT` 和 `SIGHUP` 各自都能停止合成器，输出
  `received <SIGNAL>, shutting down` 一行并以退出状态 0 结束，之后
  `$XDG_RUNTIME_DIR` 中既不残留 `wayland-N` 套接字，也不残留其 `.lock` 文件，
  `/tmp/.X11-unix/X<n>` 和 `/tmp/.X<n>-lock` 也都不残留；关闭嵌套窗口同样不会留
  下任何东西。像 logind 那样发送 `SIGTERM` 并紧接着发送 `SIGHUP`，仍能干净地关
  闭，并记录为 `SIGTERM`；而在拆除完成之前被处理的第二个 `SIGTERM` 会以状态 1 结
  束进程。运行这些检查时有两点注意事项：从一个不忽略 `SIGINT` 的环境启动合成器
  （交互式 shell，或带 `set -m` 的脚本；非交互式 shell 启动后台作业时会忽略
  `SIGINT`，合成器随后会继续忽略它，`/proc/<pid>/status` 中的 `SigIgn` 会显示这
  一点）；并且只在第一个 `SIGTERM` 被处理之后（出现 `shutting down` 一行之后，或
  者几毫秒之后）才发送第二个，因为内核会合并两个都仍处于待决状态的相同信号。在
  TTY 上，VT 会回到其文本控制台，包括信号在另一个 VT 位于前台时到达的情况。
- **VKMS 实验环境**（`tools/vkms-lab.sh`）。它通过内核的 VKMS configfs ABI 构建
  一个带有两条完整管线（每个 connector 各有自己的 encoder、CRTC 和主平面）的虚拟
  DRM 设备，因此两个输出同时 scanout，真实的 udev、connector 扫描器、原子 KMS、
  GBM 和 damage 路径都得到检验。命令被刻意限定得很窄：`setup` 只创建
  `/sys/kernel/config/vkms/nkdhr-lab`，拒绝覆盖已有实例，并回滚构建失败的操作；
  `connect`/`disconnect <0|1>` 改变一个 connector 的 configfs 状态，这会发出由
  udev 投递的内核热插拔事件；`show` 不需要 root；`audit <pid>` 读取
  `/proc/<pid>/fd`，如果有任何打开的 DRM 主节点不属于实验环境则失败；`teardown`
  用显式的 `unlink`/`rmdir`（从不递归删除）只禁用并移除 `nkdhr-lab`，并保持模块
  已加载。在检验输出之前先进行审计：仅凭 connector 日志是不够的，因为打开主节点
  可能在任何 connector 扫描之前就获取了 DRM master。VKMS 证明的是合成器逻辑和内
  核 API 集成，而不是链路训练、EDID 怪癖、本地 logind 交接或真实面板的模式；这些
  仍属于物理验收项目。
- **Soak 运行器**（`tools/soak-test.sh`）。`run` 创建一个带时间戳的状态目录，在
  系统睡眠抑制器下启动一个临时的用户 systemd 采集器，然后 `exec` release 版合成
  器，使其保留本地 TTY 和 libseat 上下文；`start --pid` 则附加到一个正在运行的合
  成器上。只有所记录的 logind 会话处于活动状态的秒数才会计入目标时长，并且通过
  PID 启动时间检查来区分原始进程与 PID 复用。采集器记录 RSS 和内存峰值、CPU
  tick、线程数和描述符数、按 DRM 客户端去重的 i915 fdinfo 引擎与内存计数器、
  connector 状态、会话状态、采样间隙、经过过滤的内核 DRM 失败，以及合成器自身的
  失败或 panic 行（`nkdhr-compositor: failed…`、`panicked at`）。报告在完成、进
  程提前死亡或显式停止时被冻结。自动警告是筛查标准，不能代替结合工作负载的审查。
  运行时数据位于用户的状态目录中，绝不放在 Git 工作树中。

## 历史

本 crate 所承载代码对应的各里程碑的验证记录。2026-10-23 之前的结果是在旧的 crate
名称下、交互层仍然存在时记录的；它们所检验的后端和协议路径就是上文所描述的这些。

- **COMP-1（嵌套骨架，2026-08-03）。** 在一个无头 Weston 宿主中以嵌套方式干净地
  启动，以所请求的 1280×800 创建了窗口，并在整个运行期间渲染了每一帧；在十分钟内
  每 30 秒采样一次的 `VmRSS` 保持平稳。`CloseRequested` 的处理和 `bind()` 的调整
  大小行为已从 Smithay 的源码中得到确认。
- **COMP-2（Wayland 客户端，2026-08-03）。** 事实证明 `wl_output` 全局对象是必需
  的而非可选的：没有它 `foot` 拒绝启动（`no monitors available`），尽管
  `wl_shm`、xdg-shell 或 `wl_seat` 中没有任何东西要求它。dmabuf 全局对象使用版本
  3 且不带默认 feedback，因为 feedback 需要一个可解析的主设备，而嵌套后端无法提
  供。`weston-simple-shm`、`weston-simple-egl`、`weston-simple-dmabuf-egl`、
  `foot` 和 `gtk4-demo` 都能干净地连接、渲染和断开，其中三个同时运行。
- **COMP-5（TTY 后端，2026-08-07）。** 双输出 VKMS 诊断在十秒空闲窗口内测得渲染
  引擎增长为零，CPU 占用为单核的 0.10%。在笔记本面板上，真实的 libseat 暂停/恢复
  （从 TTY1 切到 TTY2 再切回）保留了客户端和焦点，恢复了输入，并在修复缓冲区重置
  之后立即重绘了整个面板。在真实的 eDP + HDMI 配置上，实时排列变更、保留客户端的
  物理拔出与重新插入，以及双向干净的双输出 VT 交接均通过。
- **COMP-6（协议长尾，2026-08-08）。** 在无头 Weston 宿主下：SHM、GTK 4 和指针约
  束客户端均完成映射；剪贴板和主选区通过真实的进程内 Xwayland，在
  Wayland（`wl-copy`/`wl-paste`）与 X11（`xsel`）之间双向拷贝了精确的文本；一个
  被强制使用 X11 的 GTK 4 应用程序在 `grim` 捕获中正常渲染；完整的 1280×800 捕获
  和 256×256 区域捕获均成功；默认捕获与 `grim -c` 捕获之间只相差后备箭头；反复回
  读没有使合成器停止运行。一个真实的 `swaylock` 锁达到了受保护的呈现，并且只在此
  之后才被确认；当其 PAM 工作进程崩溃时（宿主没有 `/etc/pam.d/swaylock`），会话
  保持受保护，截图为纯黑。在 TTY 上，Weston 的 constraints 客户端确认了限制和锁
  定指针的相对移动，其数据源 DnD 客户端确认了图标跟踪和成功的放下。Weston 的
  `--self-only` 模式（没有数据源的 DnD）在 Smithay 0.7 下收不到放下事件，Smithay
  会在 `wl_data_device.drop` 之前把每个没有数据源的 offer 标记为未验证；这是上游
  的限制，普通 DnD 才是受支持的路径。可观察的空闲抑制、基于 PAM 的解锁以及已安装
  （而非解包）的 Xwayland 仍作为集成检查项保留。
- **COMP-8（稳定化，2026-08-08）。** 一次 SIGKILL 压力测试表明，在客户端突然死亡
  后，仅靠协议的销毁回调会在窗口栈中留下死亡窗口，这就是回收机制存在的原因；两个
  后端都不保留 `insert_client` 返回的 `Client` 句柄，因为 display 拥有该连接，多
  余的句柄会成为无界增长的逐连接历史。一次优化构建的嵌套运行强制杀死了 200 个
  SHM/EGL 客户端：每个窗口都被回收，窗口数回到零，描述符数保持在 30，RSS 从 20
  个样本中的第三个起稳定在 131,704 KiB。八小时的活动 TTY 空闲 soak 测试在全部
  956 个样本中都保持为原始进程，RSS 增长 192 KiB，描述符数和线程数保持不变，平均
  占用单核的 0.243%，没有记录到任何内核 DRM 失败。该 soak 测试的一份 VT 日志暴露
  了交接竞态，由此引出了先重置后切换的顺序；修复之后最后一次一分钟的物理回归测试
  记录了 32/32 个匹配样本、正确的输出转换、没有错误，并在返回时立即完整重绘。
