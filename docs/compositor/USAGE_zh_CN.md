# 合成器 — 用户指南

> [English version / 英文版本](USAGE.md)

`nkdhr-compositor` 是 nkdhr 的 Wayland 合成器，即显示协议层：它带有一个
DRM/KMS 后端，用于在 Linux 虚拟终端上运行真实会话；还有一个嵌套后端，供开发时
作为窗口运行在已有桌面之中。它为原生 Wayland 客户端和 X11 应用程序（通过集成的
XWayland 服务器）提供服务，驱动每一台已连接的显示器，并在会话锁定期间保证会话
安全。

它刻意只提供一套小而固定的窗口行为：窗口居中打开，点击即聚焦窗口并将其提到最上
层，应用程序通过自己的标题栏移动窗口和调整窗口大小。nkdhr 的交互设计正在重新设计
中。之前的交互层（无限画布、工作区和集成 shell）已于 2026-10-23 移除，保存在
git 标签 `archive/interaction-v1` 中。

## 构建

```sh
cargo build -p nkdhr-compositor                    # 仅嵌套后端（默认）
cargo build -p nkdhr-compositor --features tty     # 嵌套与 TTY 后端
cargo build --workspace --all-features --release   # 全部构建，开启优化
```

`tty` 特性会链接 libseat、libudev、libinput、GBM 和 EGL。生成的二进制文件是
`target/<profile>/nkdhr-compositor`。

## 运行

### 嵌套模式（开发）

在已有的 X11 或 Wayland 会话中运行：

```sh
cargo run -p nkdhr-compositor
```

它会在当前桌面上打开一个标题为 `nkdhr-compositor`、大小为 1280×800 的窗口。这个
窗口就是一个输出；调整窗口大小即调整该输出的大小。指针位于窗口上方时，宿主自身的
光标会被隐藏，由合成器绘制自己的光标。

### 在 TTY 上运行

在本地虚拟终端上登录（例如 `Ctrl+Alt+F3`），然后运行：

```sh
target/release/nkdhr-compositor --tty
```

合成器会像其他任何 Wayland 合成器一样，通过 DRM/KMS 接管每一台已连接的显示器，
使用的是 logind 授予你本地登录的 seat。它必须从本地 VT 启动；通过 SSH 启动的情况
见[故障排查](#故障排查)。从登录管理器和 systemd 启动 nkdhr 属于尚未构建的会话相关
工作，因此目前 TTY 后端需要手动启动。

请先在同一个登录会话中启动 `nkdhrd`：已安装的 `nkdhrd` 以 `systemd --user` 服务
的形式运行；从源码检出运行时则需要自己启动它（例如在启动合成器之前运行
`target/debug/nkdhrd &`）。控制面用户指南中的
[启动守护进程](../control-plane/USAGE_zh_CN.md#启动守护进程)一节涵盖了这两种
情况。合成器在启动时从 `nkdhrd` 读取一次输出布局，之后跟随它的变更通知；没有
`nkdhrd` 时合成器照样运行，并自动排布每一台显示器（见[排布输出](#排布输出)）。

### 选择后端

| 调用方式 | 后端 |
|---|---|
| `nkdhr-compositor --nested` | 嵌套 |
| `nkdhr-compositor --tty` | TTY |
| `nkdhr-compositor` | 设置了 `WAYLAND_DISPLAY` 或 `DISPLAY` 时为嵌套，否则为 TTY |

其他任何参数都会报错。请求一个构建二进制文件时未包含的后端同样会报错；TTY 后端
需要以 `--features tty` 构建。

### 启动应用程序

合成器没有启动器、面板，也没有应用程序快捷键。它会打印客户端的连接位置：

```
nkdhr-compositor: listening on WAYLAND_DISPLAY=wayland-1
nkdhr-compositor: XWayland ready on DISPLAY=:1
```

用这些值启动一个终端，再从它启动其他所有程序；它的子进程会继承这两个变量：

```sh
WAYLAND_DISPLAY=wayland-1 DISPLAY=:1 foot
```

嵌套运行时，请始终显式设置 `WAYLAND_DISPLAY`：否则，从宿主终端启动的客户端会
打开在宿主桌面上。同理，宿主的 `DISPLAY` 指向宿主的 X 服务器，而不是合成器的
XWayland。在 TTY 上，请从另一个 VT 或同一用户的 SSH 登录中启动第一个终端；
套接字位于同一个 `$XDG_RUNTIME_DIR` 中。

### 退出会话

- **嵌套**：关闭窗口，或在启动合成器的终端中按 `Ctrl+C`。关闭那个终端同样会让
  它停止。
- **TTY**：没有退出键。切换到另一个 VT（`Ctrl+Alt+F3`），登录，然后停止进程：
  `kill $(pidof nkdhr-compositor)`。同一用户的 SSH 登录同样可以做到，用
  `loginctl terminate-session` 结束登录会话也可以。合成器不必位于前台的 VT 上
  也能停止。

无论哪种方式，合成器都会干净地关闭。它在收到 `SIGTERM`（`kill` 默认发送的信号）、
`SIGINT`（`Ctrl+C`）和 `SIGHUP`（启动它的终端被关闭）时停止，并在日志中记录收到
的是哪一个：

```
nkdhr-compositor: received SIGTERM, shutting down
```

随后它会移除自己的 Wayland 套接字（`$XDG_RUNTIME_DIR` 中的 `wayland-N` 和
`wayland-N.lock`）以及 XWayland 的 X11 套接字和锁文件（`/tmp/.X11-unix/X<n>` 和
`/tmp/.X<n>-lock`），停止 XWayland；在 TTY 上，它还会把各台显示器恢复到接管时的
状态再交还，并释放 seat，使它所在的 VT 回到文本控制台。合成器退出时，客户端会
失去连接。

结束登录会话时会先发送 `SIGTERM`，紧接着发送 `SIGHUP`；这时日志行中记录的是
`SIGTERM`。合成器启动时就已被忽略的信号会继续被忽略：用 `nohup` 启动时，它在
终端关闭后仍会继续运行；由脚本在后台启动时，`Ctrl+C` 无法让它停止。

如果宿主桌面隐藏了嵌套合成器的窗口（在 GNOME 或 KDE 上被最小化，或在 sway 或
Hyprland 上位于未显示的工作区），它可能要等到窗口再次显示后才会停止，因为窗口
隐藏期间宿主不再让它绘制。

如果关闭过程卡住了，第二个 `SIGTERM` 或 `SIGINT`（再次运行 `kill`，或再按一次
`Ctrl+C`）会立即结束进程，不做上述任何清理。`SIGHUP` 从不计入其中，既不算作
第一个信号，也不算作第二个，因为会话管理器和终端经常紧跟在另一个信号之后发送它。
`kill -9`（`SIGKILL`）和崩溃同样会跳过清理，留下套接字和锁文件。它们是无害的：
下次启动时会接管残留的 Wayland 套接字，并复用残留的 X11 显示编号，除非其锁文件
指明的进程仍在运行。

## 窗口行为

这是协议层永久的默认行为。它刻意保持最小化，没有任何配置项。

- **打开**：新窗口在绘制出第一帧后出现，居中于指针所在的显示器。若窗口在某个方向
  上比该显示器大，则在该方向上改为与显示器的左边缘或上边缘对齐，从而保证它的标题栏
  和左边缘始终可以够到。窗口可以相互重叠；任何机制都不会平铺或重新排列它们。
- **焦点**：新打开的窗口获得键盘焦点，并被提到其他窗口之上。用任意鼠标按键点击
  一个窗口都会聚焦该窗口并将其提到最上层，而点击本身仍会传递给应用程序。点击没有
  窗口的地方，键盘焦点保持不变。焦点不跟随指针。打开菜单会把菜单所属的窗口提到最
  上层，打开的菜单持有键盘焦点；菜单关闭后，焦点回到该窗口。在打开的菜单之外点击
  会关闭菜单，但不会聚焦被点击的窗口，也不会将其提到最上层，不过点击本身仍会传递
  给它。在菜单打开期间新打开的窗口会关闭菜单并取得焦点。持有焦点的窗口关闭时，
  焦点移到剩余窗口中最上层的那个；若已没有剩余窗口，则不聚焦任何东西。指针下方的
  窗口总会接收指针输入，即使该窗口出现在那里之后指针一直没有移动，例如一个刚好在
  指针下方打开的窗口。
- **移动和调整大小**：拖动窗口自身的标题栏或边缘。合成器会为 Wayland 和 X11 窗口
  执行这些由客户端发起的移动和调整大小请求；从上边缘或左边缘调整大小时，对边保持
  不动。移动或调整大小总是延续发起它的那次按钮按下；从触摸屏发起的请求，以及已有
  一次移动或调整大小正在进行时的重复请求，都会被忽略。合成器没有用于移动、调整
  大小或关闭窗口的快捷键。
- **装饰**：客户端装饰。合成器会告诉每个协商装饰的客户端自行绘制装饰，自己从不
  绘制标题栏或边框。因此，如果应用程序不绘制任何装饰，它的窗口就没有标题栏，也
  无法以交互方式移动。
- **最大化、全屏和最小化**：不提供。合成器会把这一点告知应用程序，遵从这一点的
  应用程序会隐藏它们的最大化、最小化和窗口菜单控件。最大化或进入全屏的请求仍会
  得到响应，但状态保持不变；最小化和窗口菜单请求会被忽略。
- **X11 窗口**：普通 X11 窗口的居中与聚焦方式与 Wayland 窗口相同。菜单、工具提示
  和其他 override-redirect 窗口会精确地出现在应用程序放置它们的位置，且从不获取
  键盘焦点。
- **触摸**：一次触摸会传递给发起它的那根手指下方的窗口，并一直留在该窗口上，直到
  所有手指都抬起为止，其间加入的其他手指都归入同一次触摸。触摸不会聚焦窗口，也不
  会将窗口提到最上层，唯一的例外是：触摸打开的菜单会像任何菜单一样，把它所属的
  窗口提到最上层。触摸已打开菜单所属应用程序之外的任何位置都会关闭菜单，与在菜单
  之外点击一样，且这次触摸仍会传递给手指下方的内容。
- **键盘布局**：目前还没有布局设置。合成器使用 libxkbcommon 的默认值，它会遵从
  合成器环境中的 `XKB_DEFAULT_LAYOUT`、`XKB_DEFAULT_VARIANT`、`XKB_DEFAULT_MODEL`
  和 `XKB_DEFAULT_OPTIONS`：

  ```sh
  XKB_DEFAULT_LAYOUT=de target/release/nkdhr-compositor --tty
  ```

- **显示器变化**：添加、移除或重新排布显示器时，窗口在共享输出空间中的位置保持
  不变；它们不会随显示器一起移动。完全不在任何显示器上的窗口（例如它所在的显示器
  被拔掉了）会被移到指针所在的显示器上，并像新打开的窗口一样在那里居中，因此不会
  有窗口被困在视野之外。仍有一部分位于某台显示器上的窗口则保持原位。

## 切换虚拟终端

在 TTY 后端上，`Ctrl+Alt+F1` 到 `Ctrl+Alt+F12` 用于切换 Linux 虚拟终端，某些键盘
布局为这些组合键产生的专用 keysym `XF86Switch_VT_1` … `XF86Switch_VT_12` 同样
如此。这些按键专门保留给 VT 切换，永远不会传递给应用程序。会话锁定时它们同样
有效：切换到另一个 VT 不会解锁或暴露本会话。回到 nkdhr 所在的 VT 时，会话会恢复，
所有客户端、窗口和焦点都保持原样，并立即重绘每一台显示器。选择合成器当前所在的
VT 不会有任何效果。

嵌套后端把这些组合键留给宿主桌面处理。

## 排布输出

所有显示器排布在同一个共享的逻辑坐标空间中，通过 `nkdhrd` 的 `outputs`
namespace 配置，对应的文件是 `~/.config/nkdhr/outputs.toml`。每个条目是一个以
接口（connector）名称命名的表：

```toml
[eDP-1]
x = 0
y = 0
scale = 1.25

[HDMI-A-1]
x = 1536
y = 0
scale = 1.0
```

- **接口名称**是内核给出的名称（`eDP-1`、`DP-2`、`HDMI-A-1`……）。合成器会在每个
  接口接入时打印它的名称：`nkdhr-compositor: connected output HDMI-A-1 at …`。
  嵌套后端唯一的输出名为 `nkdhr-compositor-nested-0`。名称不能包含 `.`。名称与
  任何已连接接口都不匹配的表会被接受，但不起任何作用，因此拼错名称时，对应的
  显示器仍会被自动放置。
- **`x`、`y`** 是显示器左上角的位置，以逻辑像素为单位，可以为负数。只有相对布局
  才有意义：放置完所有显示器之后，合成器会平移整个布局，使其左上角位于 `(0, 0)`，
  同时保持各显示器的相对位置。两者默认都为 `0`。
- **`scale`** 是每个逻辑像素对应的物理像素数，必须是大于 0 的有限数；`1.25` 这样
  的小数值也可以。显示器的逻辑尺寸——也就是它在布局中占据的空间——等于其显示模式
  尺寸除以缩放比例，再四舍五入到最接近的整数逻辑像素，这也是告知应用程序的尺寸：
  上例中 1920×1080 的面板在 `1.25` 缩放下为 1536×864 逻辑像素，这就是那台显示器
  位于 `x = 1536` 的原因；而 2256×1504 的面板在 `1.75` 缩放下为 1289×859。默认为
  `1.0`。
- **未配置的显示器**永远不会被留成空白。没有条目的已连接接口会被自动放置：缩放
  比例为 `1.0`，位于已配置显示器的右侧，按接口名称顺序排列。文件为空时，所有
  显示器都会以这种方式从左到右依次放置。

指针只能在显示器本身之上移动：布局中显示器之间的空隙无法到达，指针会沿着显示器的
边缘滑动。显示器可以相互重叠；重叠区域显示布局中的同一部分。每台显示器都以其首选
模式运行；这个 namespace 只设置位置和缩放比例。没有输出管理协议，因此
`wlr-randr`、`kanshi` 之类的工具无法更改显示器设置。

### 更改布局

每当文件发生变化，`nkdhrd` 都会重新校验它，合成器则会立即应用有效的布局，无需
重启。无效的编辑（未知字段、无效的缩放比例、包含 `.` 的名称）会被拒绝，上一个有效
的布局继续生效；`nkdhrd` 的日志（`journalctl --user -u nkdhrd`，从源码检出运行时
则是它所在的终端）会说明原因。

添加接口需要编辑文件，因为 `nkdhrctl config set` 只会修改已经存在的值。条目存在
之后，就可以从命令行调整它：

```sh
printf '\n[DP-2]\n' >> ~/.config/nkdhr/outputs.toml   # 只有表头的空表也是完整的条目
nkdhrctl config set outputs.DP-2.x 3456
nkdhrctl config set outputs.eDP-1.scale 1.5
nkdhrctl config get outputs.HDMI-A-1.x
nkdhrctl config watch outputs
```

删除某个接口的表会让对应的显示器恢复自动放置，但 `nkdhrd` 只报告编辑后仍然存在的
值，因此单纯的删除不会向合成器发送任何变更通知。合成器会在 `outputs` 下一次发生
变化时一并应用这次删除，或在下次启动时应用。

早期 nkdhr 版本写入 `~/.config/nkdhr/` 的文件（例如 `canvas.toml`）会被忽略；保存
在那里的布局需要在 `outputs.toml` 中重新填写。

## 应用程序兼容性与会话安全

原生 Wayland 应用程序可以使用常规剪贴板、中键主选区（primary selection）、拖放、
客户端装饰协商、弹出窗口和菜单、分数缩放、表面裁剪与缩放（`wp_viewporter`，视频
播放器和浏览器也会用到它）、指针锁定与指针约束、相对指针移动、触摸以及空闲抑制
请求。触摸板双指滚动以普通滚动输入的形式送达，触摸板的滑动、捏合和按住手势会传递
给使用 pointer-gestures 协议的应用程序。

在缩放比例为 `1.25` 这类分数的显示器上，支持分数缩放的应用程序会精确地按该比例
绘制，保持清晰。只支持整数缩放的应用程序会按向上取整后的整数比例绘制（`1.25`
对应 `2`），再被缩小以适应显示器，看起来会略微发虚；完全不支持缩放的应用程序以
缩放比例 1 绘制，并被放大。

传统 X11 应用程序通过合成器集成的 XWayland 服务器运行，并与 Wayland 应用程序双向
共享剪贴板和主选区。必须安装 `Xwayland` 可执行文件；在 Fedora 上对应的软件包是
`xorg-x11-server-Xwayland`。未安装时会话照常启动，并在日志中记录 X11 兼容已被
禁用。X11 应用程序以缩放比例 1 绘制，在设置了缩放的显示器上会被放大，因此在那里
看起来会发虚。依赖窗口管理器提供标题栏的 X11 应用程序不会有标题栏（见上文的
“装饰”）。

截图工具使用 `wlr-screencopy-unstable-v1`，每次捕获一台显示器（或其中的一块
区域）。捕获的图像是该显示器按其配置的缩放比例最终合成的画面。只有在请求时才会
包含光标（`grim -c`）。

`ext-session-lock-v1` 客户端（锁屏程序）在合成器确认会话已锁定之前，就已保护好
每一台已连接的显示器。从第一次锁定请求开始，直到锁屏程序自己解锁为止，任何普通
窗口都不会收到键盘、指针或触摸输入，除锁屏画面（或黑屏）外不绘制任何内容，截图
中也只包含这一受保护的画面。只有锁屏程序自己的表面会被显示并接收键盘输入，
只有它自己的解锁才能结束锁定，其他应用程序在锁定期间试图发起的拖放会被立即取消。
锁定开始时正在进行的操作都会被取消而不是完成：打开的菜单会关闭，拖放不会被放下，
移动或调整大小停在当前位置，按在窗口上的手指不再传递给该窗口。锁定期间若某台
显示器的缩放比例或尺寸发生变化（在 `outputs.toml` 中修改了缩放比例，或调整了嵌套
窗口的大小），锁屏画面会随之调整大小以适应它。如果锁屏程序崩溃，会话会保持锁定和
黑屏；在第一次锁定仍然有效期间，第二个锁屏程序会被拒绝。VT 切换仍然可用，因此
可以从另一个 VT 停止该会话。

合成器会记录可见窗口发出的空闲抑制请求，但 nkdhr 目前还没有空闲或显示器电源
策略：没有任何东西会自行让显示器黑屏或休眠。

## 开发工具

### 没有备用显示器也能使用两个输出：VKMS 实验环境

仓库附带一个基于 configfs 的 VKMS 实验环境，它会创建两个真实的内核 DRM 接口（名为
`Virtual-1` 和 `Virtual-2`），而不是在合成器内部伪造输出：

```sh
sudo crates/nkdhr-compositor/tools/vkms-lab.sh setup
crates/nkdhr-compositor/tools/vkms-lab.sh show
# 运行 nkdhr-compositor --tty，并将 NKDHR_DRM_DEVICE 设为真实 GPU 的
# 渲染节点，将 NKDHR_DRM_SCANOUT_DEVICE 设为 `show` 列出的 VKMS card。
crates/nkdhr-compositor/tools/vkms-lab.sh audit <compositor-pid>
sudo crates/nkdhr-compositor/tools/vkms-lab.sh disconnect 1
sudo crates/nkdhr-compositor/tools/vkms-lab.sh connect 1
sudo crates/nkdhr-compositor/tools/vkms-lab.sh teardown
```

在合成器运行时使用 `disconnect`/`connect`，即可测试真实的 udev 热插拔处理。
`teardown` 只移除实验环境自己的实例，VKMS 模块保持加载。在测试输出之前先运行
`audit`：如果合成器持有实验环境之外的任何 DRM 主节点，它就会失败。

这两个环境变量是带有硬性安全边界的开发用覆盖项：`NKDHR_DRM_DEVICE` 必须指向一个
渲染节点（render node），`NKDHR_DRM_SCANOUT_DEVICE` 必须指向一个主节点（primary
node），并且设置了 scanout 覆盖项后，合成器完全不会打开其他任何显示设备。如果在
VKMS 运行期间笔记本的物理显示屏发生了变化，请立即停止；物理面板变黑绝不是预期的
测试行为。`LIBSEAT_BACKEND=noop` 可以用于隔离的 VKMS 运行，但绝不能用于真实会话。
该实验环境是一个回归测试环境，不能代替在本地 VT 上使用物理显示器进行的测试。

### 长时间稳定性：soak 运行器

soak 运行器独立于任何交互式 shell，对一次长时间的 TTY 会话进行测量：

```sh
cargo build --workspace --all-features --release
# 另行启动 nkdhrd，然后在 nkdhr 的本地 VT 上运行：
crates/nkdhr-compositor/tools/soak-test.sh run --duration 8h

# 之后，在同一用户会话的任意终端中：
crates/nkdhr-compositor/tools/soak-test.sh status
crates/nkdhr-compositor/tools/soak-test.sh stop
crates/nkdhr-compositor/tools/soak-test.sh report
```

`run` 会启动一个临时（transient）的用户级 systemd 采集器，然后把自身替换为
`target/release/nkdhr-compositor --tty`（或 `--` 之后给出的命令），从而让合成器
保有本地控制 TTY。启动采集器的终端关闭后，采集器仍会继续运行。它默认每 30 秒采样
一次（`--interval`），并把 CSV 样本、事件/合成器/内核日志、元数据和一份 Markdown
报告保存在 `$XDG_STATE_HOME/nkdhr/soak/`（或 `~/.local/state/nkdhr/soak/`）下。
测量期间它持有一个睡眠抑制锁（sleep inhibitor），只统计发起运行的那个登录会话处于
活动状态的时间，记录 VT 与输出的切换，并在采集完成后让合成器继续运行。`stop` 只
结束采集；合成器需要另行停止。

要在不重启的情况下测量一个已在运行的合成器：

```sh
crates/nkdhr-compositor/tools/soak-test.sh start --pid <compositor-pid> \
    --session <login-session-id> --duration 8h
```

自动判定会标记以下情况：提前退出或 PID 被复用、活动时长不完整、DRM 或内核故障、
合成器的失败或 panic 日志行、内存大幅增长、文件描述符增长，以及完全没有出现任何
GPU 空闲区间。报告在采集完成时即被冻结，因此仍在运行的合成器之后的输出不会改变
它。内存表现取决于工作负载，因此请阅读 `report.md` 和原始样本，而不要把某一个
阈值当作没有泄漏的证明。

## 故障排查

- **在真实硬件上什么都不渲染，或合成器在启动时退出**：该会话的 journal 中会显示
  EGL 或 DRM 错误。在项目功能完整之前，nkdhr 只把 Intel Iris Xe 作为唯一受支持的
  GPU；其他 GPU 是已知的空缺，暂时还不算需要报告的 bug。
- **通过 SSH 使用 `--tty` 时报告无法打开 seat 或会话**：请从本地 VT 启动。logind
  把 DRM 和输入设备的访问权限授予活动的本地 seat，而不是远程登录。
- **应用程序打开在宿主桌面上，而不是嵌套合成器中**：把 `WAYLAND_DISPLAY`（对 X11
  应用程序还有 `DISPLAY`）设为合成器打印出的值。
- **X11 应用程序无法启动**：安装 `xorg-x11-server-Xwayland` 并重启合成器。缺少
  Xwayland 不影响 Wayland 客户端。
- **窗口没有标题栏，无法移动**：它的应用程序自己不绘制装饰；而合成器刻意不绘制
  任何装饰。
- **拔掉或重新排布显示器后，某个窗口跑到了某台显示器的正中**：它当时已不在任何
  显示器上，因此合成器把它移到了指针所在的显示器。仍有一部分位于某台显示器上的
  窗口会保持原位；可以拖动它的标题栏把它拖回来。
- **所有窗口都不响应输入，且屏幕全黑**：锁屏程序处于活动状态，或在锁定期间崩溃
  了。锁定开始时合成器会记录 `protecting session while lock client prepares surfaces`，
  结束时记录 `session unlocked`。锁屏程序崩溃后，请切换 VT，停止合成器（见
  [退出会话](#退出会话)），然后重新启动它。
- **对 `outputs.toml` 的修改没有生效**：`journalctl --user -u nkdhrd` 会显示校验
  错误，`nkdhrctl config get outputs.<connector>.x` 会显示当前生效的值。删除的表只有
  在下一次变化或重启时才会生效（见上文）。
- **`nkdhrctl config set outputs.<connector>.x …` 报告未知 key**：该接口还没有
  条目；请先在文件中添加它的表。
- **`pkill -x nkdhr-compositor` 找不到进程**：进程名会被截断为 15 个字符。请使用
  `pidof nkdhr-compositor` 或 `pkill -f`。
- **合成器记录了 `shutting down`，却没有退出**：再次运行
  `kill $(pidof nkdhr-compositor)`（或再按一次 `Ctrl+C`）；它收到的第二个
  `SIGTERM` 或 `SIGINT` 会立即结束进程。如果它记录的是 `received SIGHUP`，这次
  挂断不计数，因此需要 `kill` 它两次。
- **`wayland-N`、`wayland-N.lock`、`/tmp/.X11-unix/X<n>` 或 `/tmp/.X<n>-lock`
  残留了下来**：合成器被 `SIGKILL` 杀死、被第二个 `SIGTERM` 或 `SIGINT` 结束，
  或者崩溃了。这些文件是无害的；下次启动时会复用或跳过它们。
- **soak 测试期间，另一个桌面会话让屏幕黑屏或让机器挂起**：请先停止那个会话的空闲
  管理器。运行器会阻止睡眠，但无法安全地更改另一个合成器的显示器电源策略。
