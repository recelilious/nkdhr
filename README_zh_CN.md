# nkdhr

*一个自成一体的 Linux 桌面环境，运行在自研的 Wayland 合成器之上。*

> [English version / 英文版本](README.md)

nkdhr 是一个自成一体的 Linux 桌面环境。它运行在自研的 Wayland 合成器之上，
并由一个控制面在桌面与底层系统服务之间代理所有系统级操作（电源、网络、音频、
亮度、会话以及 nkdhr 自身的配置）。

项目处于早期开发阶段。目前已有的是控制面和合成器的显示协议层；构建在它们之上的
交互设计正在重新设计（见[状态](#状态)）。中英文文档保存在 `docs/` 目录。

## 范围

- **下边界**：原版最小安装的 Fedora Linux（Fedora 44 “Everything” 最小安装），
  除可用的网络配置和 `git` 外不添加任何东西。nkdhr 的目标是把机器从这一状态
  带到完整可用的桌面。
- **上边界**：处于或高于“文本编辑器”生态位的应用不在本项目中开发。nkdhr 只对
  它们做适配：由用户选择要使用的应用，nkdhr 为其生成一次配置（或在用户明确
  要求时再次生成），绝不强行覆盖用户自己的定制。
- **两者之间**的一切都在本项目中开发：登录界面、状态栏、启动器、通知、设置、
  文件管理器、任务管理器和锁屏都是 nkdhr 自己的，而不是 waybar、greetd 之类的
  第三方组件。

## 状态

### 控制面

`nkdhr-ipc`、`nkdhrd` 和 `nkdhrctl` 组成控制面（Phase 1，已完成）。`nkdhrd`
是运行在会话 D-Bus（`org.nkdhr.*`）上的无特权 `systemd --user` 守护进程。它从
UPower、NetworkManager、PipeWire、sysfs 背光和 logind 读取电池、网络、音频、
亮度和会话状态；在 polkit 授权下执行变更操作（亮度、音量与静音、连接 Wi-Fi、
关机、重启、挂起）；只在真实变化时发出信号，从不轮询；并持有 nkdhr 的配置：
`~/.config/nkdhr/` 下经 schema 校验的 TOML 存储，通过 D-Bus 提供 get、set 和
watch，手动编辑的文件会被重新校验，有效时立即生效。`nkdhrctl` 是它的命令行
前端。目前注册的唯一配置命名空间是 `outputs`，即合成器的显示器排布。

### 合成器

`nkdhr-compositor` 是 nkdhr 的 Wayland 合成器，现已精简为显示协议层：

- **后端**：用于开发的嵌套后端，在已有的 X11 或 Wayland 桌面中以窗口形式运行；
  以及 TTY 后端，从本地虚拟终端通过 DRM/KMS 驱动所有已连接的显示器，支持
  libseat 会话管理、udev 输出热插拔、多 GPU 配置（在一块 GPU 上渲染，由另一块
  GPU 扫描输出）以及 Ctrl+Alt+F1…F12 虚拟终端切换。
- **客户端**：原生 Wayland 客户端，以及通过集成 XWayland 服务器运行的 X11 应用。
- **协议**：剪贴板和主选区（在 Wayland 与 X11 之间双向共享）、拖放、客户端
  装饰协商、会话锁定（`ext-session-lock-v1`；锁屏程序崩溃时会话仍保持受保护）、
  屏幕捕获（`wlr-screencopy-unstable-v1`）、分数缩放（配合 `wp_viewporter`，
  应用在 1.5 等缩放比例下也能绘制清晰的缓冲区）、指针约束、相对指针运动、
  触控板手势、触摸输入和空闲抑制请求。
- **停止**：`SIGTERM`、`SIGINT` 和 `SIGHUP` 会让它干净地关闭，删除自己的
  Wayland 与 X11 套接字及锁文件，在 TTY 上还会交还显示器和 seat。
- **显示器**：多个显示器通过 `nkdhrd` 的 `outputs` 命名空间排布在同一个共享
  逻辑空间中（按 connector 设置位置和缩放，实时生效）；没有条目的显示器会自动
  并排放置。
- **窗口行为**：刻意保持最小的永久默认行为。新窗口在指针所在的显示器上居中
  打开并获得键盘焦点；点击窗口会使其获得焦点并置顶；应用通过自己的标题栏移动
  和调整大小，由合成器执行这些请求；装饰始终由客户端绘制。没有启动器、面板或
  工作区，除虚拟终端切换外也没有任何合成器快捷键。

后端与协议部分已在 2026-08-08 合成器各里程碑验收时完成验证，包括真实 TTY 运行、
物理输出热插拔和八小时稳定性测试；验证记录见
[合成器内部实现](docs/compositor/INTERNALS_zh_CN.md#历史)。

### 交互设计

第一版交互设计已于 2026-10-23 撤回，正在重新设计。它包括无限画布窗口模型
（可平移、缩放的世界坐标、总览、输出组、固定组件、编号工作区和位置标记）、
共享渲染与 UI 工具包、主题、动效系统、集成 shell 以及设置应用。其代码、文档和
产品验收矩阵已从主线移除，完整保存在 git 标签 `archive/interaction-v1` 中：

```sh
git checkout archive/interaction-v1      # 完整的先前状态
git show archive/interaction-v1:<path>   # 查看其中的单个文件
```

在新设计确定之前，合成器只提供上文所述的最小窗口行为。

### Crate

| Crate | 作用 |
|---|---|
| `nkdhr-ipc` | 所有与 `nkdhrd` 通信的 crate 共用的 D-Bus 接口定义和配置类型 |
| `nkdhrd` | 控制面守护进程 |
| `nkdhrctl` | `nkdhrd` 的命令行前端 |
| `nkdhr-compositor` | Wayland 合成器：显示协议层，带嵌套与 DRM/KMS 后端 |
| `nkdhr-sessiond`、`nkdhr-greet`、`nkdhr-files`、`nkdhr-tasks` | 为登录守护进程、登录界面、文件管理器和任务管理器预留，尚未实现 |

## 构建

nkdhr 在 Fedora Linux 上开发，使用稳定版 Rust 工具链（例如通过
[rustup](https://rustup.rs) 安装）。工作区会链接以下系统库，请先安装它们的
开发包：

```sh
sudo dnf install pipewire-devel clang-devel libinput-devel systemd-devel \
    libseat-devel mesa-libgbm-devel libdrm-devel libxkbcommon-devel
```

运行 X11 应用还需要在运行时安装 Xwayland（`xorg-x11-server-Xwayland`）。

```sh
cargo build --workspace                          # 所有 crate；合成器只带嵌套后端
cargo build -p nkdhr-compositor                  # 合成器，嵌套后端（默认）
cargo build -p nkdhr-compositor --features tty   # 合成器，嵌套与 TTY 后端
```

在当前桌面中以嵌套方式运行合成器，或在本地虚拟终端上使用 TTY 后端运行：

```sh
cargo run -p nkdhr-compositor          # 嵌套：在已有的 X11 或 Wayland 桌面中
target/debug/nkdhr-compositor --tty    # TTY：在本地虚拟终端上（需以 --features tty 构建）
```

合成器会打印应用连接所需的 `WAYLAND_DISPLAY` 和 `DISPLAY` 值。启动应用、排布
显示器和退出 TTY 会话的方法见[合成器用户指南](docs/compositor/USAGE_zh_CN.md)。
`nkdhrd` 以 `systemd --user` 服务运行，其 unit 和 polkit 策略位于
`crates/nkdhrd/resources/`；目前还没有安装程序。没有 `nkdhrd` 时合成器同样可以
运行，并会自动排布显示器。

提交改动前，请运行每个改动都必须通过的检查：

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
```

## 文档

- [控制面用户指南](docs/control-plane/USAGE_zh_CN.md) ·
  [English](docs/control-plane/USAGE.md)
- [控制面内部实现](docs/control-plane/INTERNALS_zh_CN.md) ·
  [English](docs/control-plane/INTERNALS.md)
- [合成器用户指南](docs/compositor/USAGE_zh_CN.md) ·
  [English](docs/compositor/USAGE.md)
- [合成器内部实现](docs/compositor/INTERNALS_zh_CN.md) ·
  [English](docs/compositor/INTERNALS.md)
- [合成器扩展接缝](docs/compositor/EXTENDING_zh_CN.md) ·
  [English](docs/compositor/EXTENDING.md)

## 协议

[PolyForm Noncommercial License 1.0.0](LICENSE.md)：任何非商业目的的使用、
修改、再分发完全自由；禁止商业使用。
