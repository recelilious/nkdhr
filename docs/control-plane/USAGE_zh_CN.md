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

key 是以点分隔的路径(例如 `theme.profile`、`canvas.grid_size` 和
`canvas.outputs.<group>...`)。底层文件是 `~/.config/nkdhr/` 下的纯 TOML
文件,可以直接用文本编辑器修改——`nkdhrd` 会检测到变化并重新校验,校验通过则
生效,不通过则拒绝(保留上一个有效值)并在 journal 中记录诊断信息。

目前已注册的 namespace 是 `canvas` 和 `theme`。UI-4 把完整可移植主题保存为
标量叶子 `theme.profile`:其中是一份带版本的 JSON 文档,由不可变内置/壁纸基础
和稀疏显式覆盖组成。相邻标量叶子 `theme.library` 则是带版本且经过完整校验的
用户已保存主题集合。每项操作都保持在一个叶子内,因此各自的导入、校验、发布和
`Changed` 通知都是同一个原子事务。壁纸主题始终携带冻结后的完整调色板,所以
导出后即使没有原壁纸仍可使用;实时重新生成只替换基础,不会改写显式覆盖对象。
可用 `nkdhrctl config get theme.profile`、`nkdhrctl config get theme.library`
查看,或用 `nkdhrctl config watch theme` 监听。设置应用提供即时预览、取消、保存、
复制和有界 JSON 导入/导出;直接 `config set` 主要用于导入一份已经准备好且通过
校验的 JSON 文档。

## 故障排查

- `nkdhrctl` 提示 "daemon not running":检查
  `systemctl --user status nkdhrd` 和 journal。
- 变更类命令被拒绝:用 `pkaction --verbose org.nkdhr.policy.<action>` 查看
  当前的授权规则;管理员可以在 `/etc/polkit-1/rules.d/` 中修改这些规则。
- 手动编辑的配置文件未生效:`journalctl --user -u nkdhrd` 会显示导致拒绝的
  校验错误。
