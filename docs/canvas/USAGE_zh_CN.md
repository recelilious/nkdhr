# Canvas — 用户指南

> [English version / 英文版本](USAGE.md)

本指南覆盖 Phase 2 已实现并验收的 `nkdhr-canvas` 合成器及集成 shell
运行时（COMP-1 … COMP-8）。

`nkdhr-canvas` 是 nkdhr 的 Wayland 合成器。它提供一张可无限平移、缩放的
二维世界（“canvas/画布”），窗口和固定组件生活在其中，而不是固定桌面或平铺网格里。

## 运行方式

**嵌套模式（开发）**：在已有 X11 或 Wayland 会话中运行 `nkdhr-canvas`，
它会作为该桌面上的普通窗口打开。开发 nkdhr 时无需专用机器或切换 TTY：

```
nkdhr-canvas
```

**TTY 模式（实际使用）**：完整安装的 nkdhr 会在登录后由 `systemd` target
直接启动 `nkdhr-canvas`（SESS-3，Phase 5）。它会像其他 Wayland 合成器一样，
通过 DRM/KMS 接管显示器；非开发场景下通常不需要手动启动。

在这个后端中，`Ctrl+Alt+F1` 至 `Ctrl+Alt+F12` 通过 libseat 切换 Linux
虚拟终端。切回 nkdhr 所在 VT 会恢复原有会话和客户端。嵌套开发后端不会占用
宿主桌面的这些快捷键。

**没有备用显示器时进行双输出开发**：仓库附带 VKMS configfs 实验工具；
它创建两个真实的内核 DRM connector，而不是在合成器内部伪造输出：

```sh
sudo crates/nkdhr-canvas/tools/vkms-lab.sh setup
crates/nkdhr-canvas/tools/vkms-lab.sh show
# 使用真实 GPU 的 render node 设置 NKDHR_DRM_DEVICE，
# 使用上一步显示的 VKMS primary card 设置 NKDHR_DRM_SCANOUT_DEVICE。
crates/nkdhr-canvas/tools/vkms-lab.sh audit <canvas-pid>
sudo crates/nkdhr-canvas/tools/vkms-lab.sh disconnect 1
sudo crates/nkdhr-canvas/tools/vkms-lab.sh connect 1
sudo crates/nkdhr-canvas/tools/vkms-lab.sh teardown
```

合成器运行期间使用 `disconnect`/`connect`，可以验证真实 udev 热插拔处理。
`teardown` 只移除指定实验实例，不卸载 VKMS 模块。这是开发回归环境，不能替代
最终的本机 VT 与物理显示器验收。scanout 过滤器是硬安全边界：合成器拒绝把
非 render node 用作渲染覆盖，也不会打开未选中的 KMS card。VKMS 运行期间，
如果笔记本物理屏幕发生任何变化，应立即停止；物理面板变黑绝不是预期测试行为。

**八小时验收稳定性测试**：仓库内的 COMP-8 工具让测量不依赖交互式开发终端：

```sh
cargo build --workspace --all-features --release
# 单独运行 nkdhrd，然后从 nkdhr 的本地 TTY 启动：
crates/nkdhr-canvas/tools/soak-test.sh run --duration 8h

# 此后可在同一用户会话的任意终端调用：
crates/nkdhr-canvas/tools/soak-test.sh status
crates/nkdhr-canvas/tools/soak-test.sh stop
crates/nkdhr-canvas/tools/soak-test.sh report
```

`run` 会启动一个临时 user-systemd 采集服务，然后用 release 版 TTY 合成器
替换自身，使合成器继续持有本地控制 TTY。采集器会在终端命令结束后继续运行，
也不依赖 Codex 对话保持开启。默认每 30 秒采样一次，并把 CSV、事件/合成器/
内核日志、元数据和 Markdown 报告保存到 `$XDG_STATE_HOME/nkdhr/soak/`
（或 `~/.local/state/nkdhr/soak/`）。测量期间它会持有睡眠抑制器，只统计启动
会话处于 active 状态的时间，记录 VT/输出切换，并在采集完成后让合成器继续运行。
`stop` 只停止采集；需要时应另外退出合成器。

也可附加到已运行的合成器，而不重新启动：

```sh
crates/nkdhr-canvas/tools/soak-test.sh start --pid <canvas-pid> \
    --session <login-session-id> --duration 8h
```

自动判定会检测进程提前退出或 PID 重用、active 时长不足、DRM/内核故障、
归属于合成器的 failure/panic 日志、RSS 或文件描述符显著增长，以及完全没有
可观察到的 GPU 空闲区间。采集结束时会冻结终局报告，因此特意保持运行的合成器
后来追加的输出不会反过来改变本次结果。真实工作负载下的内存变化依赖具体内容，
所以 COMP-8 验收仍需查看 `report.md` 和原始样本，不能把单个阈值当作无泄漏证明。

**固定节点宿主冒烟测试**：开发者可在任一后端设置
`NKDHR_CANVAS_DEMO_PINNED_IMAGE=1`，启用永久保留的 COMP-7 测试件。
它会在默认画布的固定世界坐标添加一张生成图片；图片随世界平移/缩放、位于窗口
下方、捕获指针输入，并记录每次按下。该环境变量只是诊断开关，不是用户组件配置，
正常会话中不设置。

## 画布模型

每张画布是一张无限二维平面。窗口可以重叠，可以位于任意正、负世界坐标，
不存在平铺槽位或画布边界。默认情况下，新窗口放置与交互式移动/缩放会对齐到
32 单位的世界网格，便于保持空间规划规整。网格只是放置辅助，不是布局管理器：
它不会移动其他窗口、固定节点或正在进行的平移。

普通工作视图还具有一个屏幕空间锚点，默认是主显示器中心。初始时该锚点显示
世界坐标 `(0,0)`；指针或三指平移结束后，它会像移动窗口一样平滑吸附到最近的
网格交点。总览是临时自由相机，退出后会回到对齐的工作视图。开关与间距均支持热重载：

```
nkdhrctl config set canvas.snap_to_grid false
nkdhrctl config set canvas.grid_size 64
```

`grid_size` 必须是 1 到 4096 之间的逻辑世界单位。使用
`nkdhrctl config set canvas.snap_to_grid true` 重新启用。默认一个会话只有一张画布；
何时适合使用第二张画布，见下文“多画布与多显示器”。

## 应用兼容性与会话安全

原生 Wayland 应用可使用普通剪贴板、中键 primary selection、拖放、服务端装饰协商、
分数缩放提示、指针锁定/限制及空闲抑制。旧 X11 应用通过合成器内置的 XWayland
服务器运行，并与原生窗口共享画布放置、焦点和剪贴板模型。系统必须安装 `Xwayland`
可执行文件；Fedora 对应软件包是 `xorg-x11-server-Xwayland`。若缺失，Wayland
会话仍会启动，日志只会说明 X11 兼容功能已禁用。

截图客户端使用 `wlr-screencopy-unstable-v1`，每次捕获一个输出。图像是最终合成
结果，并包含该输出的配置缩放。会话锁定时，截图只包含受保护的黑色/锁屏场景。
光标叠加需要显式请求（`grim -c`）；普通截图不包含光标。

`ext-session-lock-v1` 客户端会先保护所有已连接输出，合成器才确认锁定成功。
从首次锁定请求到有效解锁之间，普通画布表面既不会接收键盘/指针输入，也不会被
渲染或通过 screencopy 暴露。

- **平移**：在空白画布按住左键拖动、触控板三指滑动，或按 `super+方向键`。
  触控板双指滚动和鼠标滚轮仍作为普通应用滚动输入，绝不会移动画布。每个键盘步进
  都使用短暂缓动。启用网格吸附时，指针/三指自由移动会持续跟手，松开后显示锚点
  平滑对齐到最近网格交点；连续按键会扩展待到达的目标，而不是闪过互不连续的位置。
  平移时窗口大小和比例始终不变——它只以固定 1:1 缩放移动视野，这是绝大多数
  工作时间所处的状态。
- **总览**：`super+o` 缩小视图，一次查看当前画布上的全部窗口。单击窗口会以 1:1
  缩放进入它；单击空白处、再次按 `super+o` 或按 `Esc` 会取消并返回原位置。
  这不是小地图或窗口列表，而是前往画布远处窗口的方式。
- **位置标记**：`super+shift+<0-9>` 将当前视图位置记录到对应数字；
  `super+<0-9>` 以动画跳回。标记会跨重启保存。这相当于快速返回常用地点——
  精神上类似“工作区”，但没有固定槽位的刚性。
- **移动/缩放窗口**：在窗口任意位置 `super+拖动` 可移动，`super+右键拖动` 可缩放。
  默认网格开启时，窗口移动过程中持续跟随指针，松开后平滑吸附到最近网格交点。
  缩放只对齐主动拖动的边或角，对侧保持固定。使用服务端装饰的应用还会获得一个
  最小合成器标题区域，可发起普通拖动；修饰键手势仍是在窗口任意位置进行精确画布
  操作的方式。使用客户端装饰的应用也可发出标准 xdg-shell/X11 移动及八方向缩放请求。
- **焦点**：单击窗口使其获得焦点并升到其他窗口上方，同时点击事件仍会传给实际
  单击的内容；`alt+tab` 在所有已映射窗口间循环焦点，不受指针位置影响。
  焦点不会跟随鼠标。
- **关闭窗口**：`super+q`（无需寻找关闭按钮——目前没有，理由与上述移动/缩放相同）。

## 多画布与多显示器

输出（显示器）通过 `~/.config/nkdhr/canvas.toml` 中的 `canvas.outputs`
组织成**输出组**（图形设置界面在 shell 阶段提供；外部编辑校验见 control-plane
用户指南）。每个组只绑定一张画布：

- **一个组包含所有显示器**：每块屏幕显示同一画布，作为一个宽视口一起平移和缩放；
  这是大多数多屏配置的默认方案。
- **多个组（通常每组一块显示器）**：每块屏幕拥有完全独立、可分别平移和缩放的画布；
  更接近其他桌面的“每个显示器独立工作区”，但实际上并非工作区。

配置中未提及的显示器会自动成为独立的单输出组，因此新插入的未配置显示器不会黑屏。

没有 `outputs` 表时，所有已连接显示器会从左到右放入绑定 `default` 画布的
`default` 组。双显示器刚性组合可写为：

```toml
[outputs.desk]
canvas = "main"
primary = "eDP-1"

[outputs.desk.members.eDP-1]
x = 0
y = 0
scale = 1.0

[outputs.desk.members.DP-1]
x = 1920
y = 0
scale = 1.0
```

connector 名称就是输出连接时 nkdhr-canvas 打印的 DRM 名称。坐标是组内逻辑像素，
可以为负；nkdhr 会归一化布局而不改变相对位置。`scale` 必须有限且大于零。
一个输出只能属于一个组。`primary` 可省略；如果设置，必须是该组成员，它的逻辑
中心将成为画布锚点。单输出组自然使用唯一显示器。多输出组未指定主显示器时，按
稳定 connector 名称顺序选择第一个已连接成员。有效文件编辑会实时重载；无效编辑
由 `nkdhrd` 拒绝，最后一个有效布局继续生效。

## 快捷键

三项按键与网格策略可配置。按键配置只指定键本身，并分别结合固定修饰键
（关闭/总览用 Super，循环焦点用 Alt）：

```
nkdhrctl config set canvas.close_window <key>    # 默认 q，与 Super 同按
nkdhrctl config set canvas.cycle_focus <key>     # 默认 Tab，与 Alt 同按
nkdhrctl config set canvas.overview <key>        # 默认 o，与 Super 同按
nkdhrctl config set canvas.snap_to_grid <bool>   # 默认 true
nkdhrctl config set canvas.grid_size <integer>   # 默认 32
```

`<key>` 是 xkbcommon 键名（`q`、`Tab`、`F4` 等）。运行中的 nkdhr-canvas
会立即应用更改，无需重启。无法识别的键名会被记录并忽略（继续使用内置默认值），
不会破坏绑定。平移（`super+方向键`、空白画布左键拖动、三指滑动）、移动/缩放
（`super+拖动`/`super+右键拖动`）、标记设置/跳转
（`super+shift+<0-9>`/`super+<0-9>`）和 `Esc` 暂不可配置。
上述按键和网格设置都通过 CTRL-5 管理。

## 故障排查

- 真实硬件上没有画面或启动崩溃：查看会话的 `journalctl`；GPU/驱动问题会显示为
  EGL 或 DRM 错误。在项目达到功能完整前，nkdhr-canvas 只把 Intel Iris Xe
  作为受支持 GPU（ROADMAP §2.1）；其他 GPU 是完工后的已知缺口，目前不作为 bug。
- TTY 稳定性测试期间另一个桌面会话使机器黑屏或挂起：先停止该会话的空闲管理器。
  测试工具能阻止系统睡眠，但不能安全改写或禁用另一个合成器的 DPMS 策略。
- 通过 SSH 启动 `--tty` 时报告无法打开 seat/session：请从本地 VT 启动。
  libseat/logind 只向 active 本地 seat 授予 DRM 与输入权限，不会授予无关的远程登录。
  `LIBSEAT_BACKEND=noop` 只用于隔离的 VKMS 开发，生产会话不得使用。
- X11 应用无法启动：安装 `xorg-x11-server-Xwayland` 并重启合成器。缺少 Xwayland
  按设计不会致命，也不影响原生 Wayland 客户端。
- 窗口无法移动、缩放或获得焦点：查看 `nkdhrctl watch session`；若会话报告
  `locked: true`，画布输入正按设计只路由到锁屏。
- 位置标记或快捷键重启后未保留：查看 `nkdhrctl config get canvas.marks`
  及 `canvas.overview` 等单独按键叶子。外部编辑若校验失败会保留最后有效值；
  如何查找拒绝原因见 control-plane 用户指南的故障排查部分。
