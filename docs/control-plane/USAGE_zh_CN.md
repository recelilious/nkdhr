# 控制面 — 用户指南

*[English](USAGE.md)*

覆盖 `nkdhr-ipc`、`nkdhrd`、`nkdhrctl` 三个 crate,合称"控制面"
(ROADMAP.md 第一阶段,CTRL-1 … CTRL-5)。

`nkdhrd` 与 `nkdhrctl` 共同构成 nkdhr 的控制面:这是桌面环境与底层系统服务之间
所有系统级操作(电源、网络、音频、亮度、会话,以及 nkdhr 自身的配置)的中转层。
nkdhr 的其他部分——合成器、状态栏、设置、OSD——一律只通过这一层与系统交互;
nkdhr 内没有任何代码直接读取 `/sys` 或调用 `systemctl`、`nmcli` 等命令。

## 启动守护进程

`nkdhrd` 以 `systemd --user` 服务的形式运行,每个登录会话一个实例:

```
systemctl --user status nkdhrd
systemctl --user restart nkdhrd
journalctl --user -u nkdhrd
```

完整安装的 nkdhr 会在会话启动流程中自动启用 `nkdhrd`,通常不需要手动管理。

目前还没有安装程序:`crates/nkdhrd/resources/systemd/` 中的 unit 要求二进制位于
`/usr/libexec/nkdhrd`。从源码检出运行时,请改为在它所服务的登录会话中直接运行
守护进程:`cargo run -p nkdhrd`,或在构建后运行 `target/<profile>/nkdhrd`。这样
运行时它把日志输出到所在的终端而不是 journal,按 `Ctrl+C` 即可停止。

## `nkdhrctl`

`nkdhrctl` 是 `nkdhrd` 的命令行前端。每个子命令都通过 D-Bus 与正在运行的守护
进程通信;若守护进程不可达或操作被拒绝,会在 `stderr` 打印信息并以非零状态码
退出。

### 状态查询

```
nkdhrctl ping                # 守护进程存活则打印 "pong"
nkdhrctl status              # 守护进程版本、运行时长、已加载模块
```

### 读取系统状态

```
nkdhrctl battery             # 电量百分比、充电状态、剩余时间
nkdhrctl network             # 当前连接、信号强度、IP
nkdhrctl audio               # 音量、静音状态、默认输出/输入设备
nkdhrctl brightness          # 当前亮度百分比
nkdhrctl session             # 会话 ID、seat、空闲状态、锁定状态
```

每个命令默认打印人类可读文本;加 `--json` 输出机器可读格式(状态栏与 OSD
内部即使用此格式)。

### 变更系统状态

```
nkdhrctl brightness set 60          # 0-100
nkdhrctl audio set-volume 45
nkdhrctl audio mute | unmute
nkdhrctl network connect <ssid> --password <pw>   # 开放网络可省略 --password
nkdhrctl power off | reboot | suspend
```

每个变更类命令都会先核对对应的 `org.nkdhr.policy.*` polkit 授权动作。若当前
会话未获授权,`nkdhrctl` 会报告 polkit 拒绝原因并以非零状态码退出——不存在
"静默失败"的情况。

### 监听变化

```
nkdhrctl watch battery       # 每次真实变化输出一行 JSON,例如
                              # 插拔电源或电量跨过某个阈值
nkdhrctl watch network
nkdhrctl watch audio
nkdhrctl watch brightness
nkdhrctl watch session
```

`watch` 从不轮询:只有当 `nkdhrd` 从底层服务收到变化信号时才会打印,因此可以
放心长时间挂起运行。

### 配置

nkdhr 自身的设置(区别于各个应用程序自己的设置)统一存放在 `nkdhrd` 拥有的
一个 schema 校验存储中:

```
nkdhrctl config get <key>
nkdhrctl config set <key> <value>
nkdhrctl config watch <prefix>
```

key 是以点分隔的路径:先是 namespace,再按嵌套的表逐级向下,直到具体的值
(例如 `outputs.eDP-1.scale`)。`get` 和 `set` 只针对单个值;`watch` 接受一个
前缀——一个 namespace、更深一层的点分路径,或者省略以监听所有变化。底层文件是
`~/.config/nkdhr/` 下的纯 TOML 文件,每个 namespace 一个;可以直接用文本编辑器
修改——`nkdhrd` 会检测到变化并重新校验,校验通过则生效,不通过则拒绝(保留上一个
有效值)并在 journal 中记录诊断信息。

`nkdhrctl config set` 只会修改已经存在的值:未知的 key 会被拒绝而不是被创建,
因此拼写错误永远不会凭空多出一项设置。

#### 输出布局(`outputs`)

目前唯一注册的 namespace 是 `outputs`,它告诉合成器每台显示器的位置和缩放比例。
`~/.config/nkdhr/outputs.toml` 中每个输出对应一个表,表名是内核报告的接口
(connector)名称(`eDP-1`、`DP-2`、`HDMI-A-1` 等):

```toml
[eDP-1]
x = 0
y = 0
scale = 1.25

[DP-1]
x = 1536
y = 0
scale = 1.0
```

| 字段 | 默认值 | 含义 |
|---|---|---|
| `x`、`y` | `0` | 输出左上角在合成器全局逻辑坐标空间中的位置,输出位置与指针共用这一坐标空间。取整数;负值同样有效,因为只有它与其他输出之间的相对布局才有意义。 |
| `scale` | `1.0` | 逻辑缩放比例:每个逻辑像素对应的物理像素数。必须是大于 0 的有限数;`1.25` 这样的小数缩放会通过 fractional-scale 协议告知应用程序。 |

位置使用逻辑坐标,因此一个输出占据的范围是其显示模式尺寸除以缩放比例:上例中
1920×1200 的面板在 1.25 缩放下宽 1536 个逻辑像素,这就是 `DP-1` 从 `x = 1536`
开始的原因。任何字段都可以省略并取默认值,因此单独一个 `[DP-1]` 表也是有效条目。
未知字段、空的接口名称、包含 `.` 的名称或无效的缩放比例,都会让 `nkdhrd` 拒绝整个
文件。接口名称不会与已连接的输出比对,因此拼错的表名(`[HDMI-A1]`)会被接受,只是
匹配不到任何输出;请与输出接入时合成器打印的名称核对。

没有对应表的输出不需要任何配置:合成器会自动排布它们——按接口名称顺序,在已配置
的输出之后从左到右依次排开,缩放比例为 1.0。因此 `outputs.toml` 缺失或为空时,
所有输出会并排摆放。合成器如何解析布局,见合成器用户指南
([`../compositor/USAGE_zh_CN.md`](../compositor/USAGE_zh_CN.md))。

由于 `config set` 从不创建 key,要为某个输出添加表,需要编辑文件;`nkdhrd` 会立即
读取这一变化。此后,它的每个字段也都可以从命令行修改:

```
nkdhrctl config set outputs.eDP-1.scale 1.5
nkdhrctl config set outputs.DP-1.x -1920   # 把 1920 宽的 DP-1 放到 eDP-1 左侧
nkdhrctl config watch outputs
```

`~/.config/nkdhr/` 中不属于任何已注册 namespace 的 TOML 文件会被 `nkdhrd` 忽略,
例如早期 nkdhr 版本写入的 `canvas.toml` 和 `theme.toml`;可以直接删除它们。

## 故障排查

- `nkdhrctl` 提示 "daemon not running":检查
  `systemctl --user status nkdhrd` 和 journal。
- 变更类命令被拒绝:用 `pkaction --verbose org.nkdhr.policy.<action>` 查看
  当前的授权规则;管理员可以在 `/etc/polkit-1/rules.d/` 中修改这些规则。
- 手动编辑的配置文件未生效:`journalctl --user -u nkdhrd` 会显示导致拒绝的
  校验错误。
