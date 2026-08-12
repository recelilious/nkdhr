# 控制面 — 内部实现

*[English](INTERNALS.md)*

面向对象:参与 `nkdhr-ipc`、`nkdhrd`、`nkdhrctl` 开发的 nkdhr 贡献者
(ROADMAP.md 第一阶段,CTRL-1 … CTRL-5)。

## Crate 划分

| Crate | 内容 |
|---|---|
| `nkdhr-ipc` | D-Bus 接口 trait(通过 `zbus` 的 `#[interface]`/`#[proxy]` 宏)以及守护进程与客户端共享的线上数据类型。这里不包含任何行为逻辑——它就是契约本身,被所有与 `nkdhrd` 通信的 crate 引用。 |
| `nkdhrd` | 守护进程本体:session bus 上的一个 `zbus::Connection`,每个被聚合的系统服务对应一个 Rust 模块,加上配置存储和 polkit 授权检查。 |
| `nkdhrctl` | 一个轻量 CLI:用 `clap` 解析参数,构造对应的 `nkdhr-ipc` proxy,发起一次调用,格式化结果(文本或 `--json`),将 D-Bus 错误映射为退出码。 |

## 总线选择

`nkdhrd` 在 **session** D-Bus(而非 system bus)上拥有 `org.nkdhr.Daemon1`。
它聚合的每个后端要么天生局限于会话范围(PipeWire),要么本身就在 system bus
上暴露了会话安全、由 polkit 中介的方法,非特权会话可以直接调用(UPower、
NetworkManager、`logind`)。让 `nkdhrd` 本身保持无特权,意味着它不需要 setuid
辅助程序,也不需要 root 服务——整个技术栈中唯一的特权进程是
`nkdhr-sessiond`(SESS-1),而 `nkdhrd` 从不与它通信。

## 对象树

```
/org/nkdhr/Daemon1       org.nkdhr.Daemon1       Ping, GetStatus, GetVersion
/org/nkdhr/Power1        org.nkdhr.Power1        电源与电池
/org/nkdhr/Network1      org.nkdhr.Network1      NetworkManager 封装
/org/nkdhr/Audio1        org.nkdhr.Audio1        PipeWire 封装
/org/nkdhr/Brightness1   org.nkdhr.Brightness1   logind 亮度封装
/org/nkdhr/Session1      org.nkdhr.Session1      logind 会话封装
/org/nkdhr/Config1       org.nkdhr.Config1       CTRL-5 配置存储
```

`/org/nkdhr/Commands1` 已预留且保持未占用——见 EXTENDING.md(仍是 staging
文档:CTRL-EXT 本身尚未实现)。

每个模块接口统一暴露:

- **`GetStatus()`**,一次调用返回该模块的完整状态结构体。CTRL-2 选择了这种
  方式,而不是最初设想的逐字段 D-Bus 属性:部分模块(Network、Audio)只有
  在串联多次后端调用之后才能给出答案(例如 Network 要走 active connection →
  device → access point),因此"当前状态"本质上是一次复合读取——单个方法
  比对独立属性做 `GetAll` 更契合这个模型,而且 `org.nkdhr.Daemon1.GetStatus`
  (CTRL-1)已经确立了这一先例。
- 对于有变更能力的模块(CTRL-3),提供相应的 **方法**:
  `Power1.PowerOff()`/`Reboot()`/`Suspend()`、`Brightness1.Set(percent: u8)`、
  `Audio1.SetVolume(percent: u8)`/`SetMute(muted: bool)`(仅默认输出设备——
  默认输入设备不可变更)、`Network1.Connect(ssid: str, password: str)`
  (开放网络时 `password` 为空字符串)。
- 每个模块一个 `Changed` **信号**,携带新的状态结构体,仅在底层后端报告真实
  变化时触发(CTRL-4)——绝不基于定时器。`nkdhrctl watch <module>` 就是对
  相应信号的一个简单循环。各模块如何在不轮询的前提下检测"真实变化",见下方
  "变化监听器(CTRL-4)"。

## 后端模块

每个模块只封装一个系统服务(Brightness 例外,封装一个 sysfs 路径),且从不
触碰不属于自己的硬件:

| 模块 | 后端 | 备注 |
|---|---|---|
| Power | 读取用 `org.freedesktop.UPower` 的 `DisplayDevice`(电池/交流电聚合状态);CTRL-3 的动作用 `org.freedesktop.login1`(`PowerOff`/`Reboot`/`Suspend`) | |
| Network | `org.freedesktop.NetworkManager`,仅处理主活动连接 | 基础功能仅支持 Wi-Fi 扫描/连接;有线网络与 VPN 管理留待后续里程碑 |
| Audio | 通过 `pipewire` 的 Rust 绑定(不存在 D-Bus API)——一个专用工作线程持有 PipeWire 的主循环(其类型基于 `Rc`,非 `Send`),并维护一份模块可同步读取的纯数据缓存。默认输出(播放)与默认输入(采集)设备都通过同一套按 `DeviceKind` 参数化的通用逻辑跟踪:媒体类别过滤(`Audio/Sink`/`Audio/Source`)加上对应的 `default.audio.*` metadata key。 | |
| Brightness | 读取 `/sys/class/backlight/<device>/{brightness,max_brightness}`,取找到的第一个设备 | CTRL-3 的 `Set` 走 `logind` 的 `SetBrightness`,而不是直接写 sysfs,因此 `nkdhrd` 不需要提升的文件权限——读取不需要这层中介,因为 sysfs backlight 文件本身全局可读 |
| Session | 主 seat 的活动会话——`Manager.GetSeat("seat0")` → `Seat.ActiveSession` → 该 `Session` 的 `Id`/`Seat`/`Active`/`IdleHint`/`LockedHint` | 刻意不使用 `Manager.GetSessionByPID` 查询 `nkdhrd` 自身 PID:`nkdhrd` 作为通用 `systemd --user` 服务运行,本身并不属于任何登录会话,这样查询必然失败。多 seat 硬件不在范围内;固定假设 `seat0`。 |

## 授权

每个变更类方法在调用后端**之前**,都会检查 `org.nkdhr.policy.*` 下一个专属
的 polkit 动作(安装为 `/usr/share/polkit-1/actions/org.nkdhr.policy.policy`,
仓库中对应 `crates/nkdhrd/resources/polkit/org.nkdhr.policy.policy`),通过
system bus 上的 `org.freedesktop.PolicyKit1.Authority.CheckAuthorization`
完成。`nkdhrd` 为每个变更能力定义了自己的动作
(`org.nkdhr.policy.power-off`、`...reboot`、`...suspend`、
`...network-connect`、`...brightness-set`、`...audio-set-volume`、
`...audio-set-mute`),而不是依赖各后端自身的默认规则,这样管理员只需在一处
(`/etc/polkit-1/rules.d/`)就能统一控制整个 nkdhr 的权限面,不受各后端本身
允许调用者做什么的影响。默认值沿用 logind 自身对等动作的惯例:该 seat 的
活动会话可免密操作(`allow_active: yes`);其他任何人都需要以管理员身份
认证(`auth_admin_keep`)。

传给 `CheckAuthorization` 的 polkit **subject** 是一个 `unix-process`
(PID + 取自 `/proc/<pid>/stat` 的启动时间),通过 `GetConnectionUnixProcessID`
**在调用本身抵达的那条 session bus 上**从调用者的 D-Bus unique name 解析
得到——而不是直接拿那个 unique name 构造一个 `system-bus-name` subject。
session bus 上的 unique name 在 system bus(polkit 的 `Authority` 所在处)上
毫无意义:两条总线各自独立分配 unique name,把其中一条上的名字当作在另一条上
同样有效来传递,会解析到 system bus 上当时恰好持有那个名字的任意无关连接
——有时报 `NameHasNoOwner` 错误,有时则是静默解析成一个真实但错误的身份。
这是 CTRL-3 开发期间的真实 bug,并非纸上谈兵:表现为本应被拒绝的变更调用却
成功了(这正是早期测试中亮度/音量/静音调用看起来"成功"的原因——它们从未
真正被授权,只是撞对/撞错了身份而已),以及在"错误身份"恰好不受信任时报出
一条 `Only trusted callers... can use CheckAuthorization() for subjects
belonging to other identities` 错误。修复方式见
`nkdhrd/src/backends/polkit.rs` 中 `check_authorization` 的文档注释。

nkdhr 从不设置 `AllowUserInteraction` 标志——它的授权完全基于 seat 活动状态
(见 `.policy` 文件的 `allow_active`),从不弹出交互式提示,因此检查过程绝不
会阻塞等待 polkit agent。

## 变化监听器(CTRL-4)

每个模块的 `Changed` 信号由一个小型后台监听器发出,该监听器在守护进程的
session bus 连接构建完成后(`nkdhrd/src/main.rs`,`request_name_with_flags`
之后)立即启动——而不是从任何 `#[interface]` 方法内部触发,因为"后端发生了
变化"这件事本来就不是由一次 D-Bus 调用触发的。每个监听器都会重新计算该模块
的完整状态,只有在与上一次发出的状态不同的情况下才发出 `Changed`,因此一个
并不影响 nkdhr 所暴露内容的后端信号(例如 nkdhr 不读取的某个 UPower 属性)
不会产生任何 D-Bus 流量。

按后端形态,一共用了三种不同的底层机制:

- **Power、Network、Session** —— `org.freedesktop.DBus.Properties.PropertiesChanged`。
  `backends/dbus_properties.rs` 是一个小型、永久性的共享辅助模块:它在给定
  的 destination/path 上打开一个原始的 `zbus::blocking::Proxy` 访问
  `Properties` 接口,并返回一个基于阻塞的 `PropertiesChanged` 迭代器。
  调用方不解码信号载荷(哪些属性变了、变成了什么)——收到信号本身就意味着
  "重新计算",这比逐属性追踪更简单,而且对"每模块一个整体状态"这种模型来说
  同样精确。三个监听器各自订阅一个*稳定*对象,因为订阅是在守护进程启动时
  建立一次、之后不会重建的:
  - Power 直接监听 UPower 的 `DisplayDevice`——与 `GetStatus()` 读取的是
    同一个对象,其路径永不改变。
  - Network 只监听 NetworkManager 的**根对象**,不监听主活动连接自身的
    子对象(这些子对象会随连接变化而出现/消失)。这是一个刻意的范围取舍:
    会错过完全局限于子对象内部的变化(例如 Wi-Fi 信号强度漂移但没有伴随
    状态转换),但能可靠捕获每一次连接/断开,因为
    `PrimaryConnection`/`ActiveConnections`/`State` 都位于根对象上,并且
    在每次这样的转换时都会变化。
  - Session 监听的是监听器*启动那一刻*(守护进程启动时)主 seat 上活动的
    那个会话。之后发生的完整会话切换——快速用户切换,或同一 seat 上的重新
    登录——在 `nkdhrd` 重启之前都不会被观察到,因为要做到这一点需要注意 seat
    自身 `ActiveSession` 指针的变化并重新订阅新的会话对象,而这在本项目
    单 seat、单用户的目标场景下暂不需要。`GetStatus()` 不受影响:它总是
    重新解析当前的活动会话。
- **Brightness** —— 直接用 `inotify` 监听背光设备 `brightness` 文件的
  `IN_MODIFY` 事件,因为 sysfs 本身没有对应的 D-Bus 信号。内核背光驱动会在
  每次变化时写入这个文件(通过 `sysfs_notify()`),无论触发者是谁——
  `nkdhrd` 自身(通过 `logind` 的 `SetBrightness`)、一个快捷键,还是其他
  进程——因此这个方式能捕获所有来源,不只是 nkdhr 自己的写入。
- **Audio** —— 完全没有独立的监听线程。`nkdhrd` 的 PipeWire 连接
  (`backends/pipewire_client.rs`)本身就是完全事件驱动的:它的
  `Tracker::reconcile()` 在每个相关 PipeWire 事件上都会运行。CTRL-4 在连接
  建好之后(`modules::audio::attach_watcher`),给它挂上一个回调
  (`PipeWireHandle::on_change`),由 `Audio` 模块注册一次;`reconcile()`
  会在每次更新后调用这个回调,回调自己完成"状态比对再发信号"的工作。复用
  既有的工作线程,避免了为了监听变化而单独再开一条 PipeWire 连接。

**从被调度的方法之外发出信号。** 上面 Power/Network/Session/Brightness 的
`Changed` 发送,都发生在 zbus 的 `#[interface]` 宏不会直接提供
`SignalEmitter` 的上下文中——是一个后台线程,而不是正在被调度的方法调用。
这个模式(直接照搬 `zbus::blocking::ObjectServer::interface` 自己的文档
示例,不是逆向工程出来的)是:

```rust
let iface = session.object_server().interface::<_, Power>(POWER_OBJECT_PATH)?;
zbus::block_on(Power::changed(iface.signal_emitter(), status))?;
```

`session.object_server().interface::<_, T>(path)` 查找*已经注册*的对象对应
的 `InterfaceRef<T>`,其 `signal_emitter()` 给出一个可在任何调度之外使用的
`&SignalEmitter`。`T::changed(...)` 就是接口里
`#[zbus(signal)] async fn changed(signal_emitter: &SignalEmitter<'_>,
status: T) -> zbus::Result<()>;` 这个签名展开后生成的函数——用它而不是手写
D-Bus 调用(把 `"org.nkdhr.Power1"`/`"Changed"` 硬编码为字符串字面量),
意味着接口名和成员名永远不会与真正的接口定义脱节,考虑到本项目已经两次
被完全同类的字符串/名称不匹配问题咬过(见下方"zbus proxy 踩坑记录"),这一点
很重要。`zbus::block_on` 把(按 zbus 信号代码生成规则必然是异步的)`changed`
函数桥接进这些监听器纯阻塞式线程的风格里;这个用法不带下文所述"async
sandwich"死锁的风险,因为这些监听线程本身并不是被它们调用 `block_on` 的那条
连接驱动的。

`Config1.Set`(CTRL-5)是唯一不需要这个模式的信号发送场景:`Set` 本身就是
被调度的调用,因此其接口方法直接带一个
`#[zbus(signal_emitter)] emitter: SignalEmitter<'_>` 参数,调用
`emitter.changed(key, value).await?` 即可——不需要
`object_server().interface()` 查找。详见下方"配置存储(CTRL-5)"。

**哪些已经过真实验证,哪些只确认了能干净启动。** Brightness 的 `inotify`
路径已针对真实背光设备做过端到端验证(见 PROGRESS.md 的 CTRL-4 记录)。
Power、Network、Session 的监听器已确认能无错误地完成订阅,并在较长时间内
保持守护进程健康,但要观察到真实的 `Changed` 信号,需要触发一次真实的后端
变化——这要么需要物理硬件访问(Power 需要插拔交流电源),要么在基于 SSH
的开发会话里过于危险而不便尝试(Network,因为这条 SSH 连接本身就走将要被
切换的那条 Wi-Fi 链路),要么被 `logind` 自身拒绝(Session 的 `IdleHint`
在 `Type=tty` 会话上拒绝直接的空闲/锁定控制,其自动空闲转换需要真实的
控制台输入,这个远程会话无法生成)。这些留给下一个能物理/控制台访问机器的人
去补齐。

## 配置存储(CTRL-5)

CTRL-5 最初没有注册 namespace,因为 CTRL-1 … CTRL-4 没有真正需要持久化的
设置,提前定义后续 schema 只会变成臆测。COMP-3 随后注册了 `canvas`,UI-4
现在注册了 `theme`。后者包含标量 `profile` 与 `library` 叶子;其中的 JSON
payload 必须先由共享的纯数据 crate `nkdhr-theme` 完成完整校验,活动 profile
还要完成全部继承解析和跨字段校验,namespace 才能提交。每项操作使用一个叶子是
有意为之:稀疏覆盖和已保存集合都包含数组与嵌套结构,而一次活动主题或资料库编辑
必须作为一个原子整体切换。原本的通用引擎仍由
`nkdhrd/src/backends/config_store.rs` 中仅供测试的 namespace 覆盖,每个真实
namespace 还会补充自己的校验和 last-known-good 测试。后续阶段要注册新的
namespace,做法是在一个 `serde` 派生的结构体
上实现 `Namespace` trait(`backends::config_store::Namespace`——**位于
`nkdhrd`,而非 `nkdhr-ipc`**:客户端仍使用下面这套通用的点分 key `Config1`
IPC;UI-4 可移植 profile 的类型则放在守护进程校验和 UI 解析共同使用、与后端
无关的 `nkdhr-theme` crate 中;其中的壁纸适配器只借用宿主已解码的 RGBA8 视图,
最终仅保存生成调色板,不保存图片字节),然后在 `nkdhrd/src/main.rs`
的 `static NAMESPACES: &[NamespaceSchema]` 列表里加一条
`NamespaceSchema::of::<T>()`。

- 磁盘存储:`~/.config/nkdhr/` 下的 TOML 文件,每个逻辑 namespace 一个文件
  (例如 `theme.toml`、`canvas.toml`);除用户自己用编辑器修改外,`nkdhrd`
  是唯一的写入方。一个 namespace 的文件始终保存其完整的*物化*状态(每个
  字段都存在,缺省值已经填好)——绝不是稀疏的差异——因为无论是来自
  `Config1.Set` 还是一次重新校验过的外部编辑,每次写入都会先完整反序列化为
  具体的 Rust 结构体,再重新序列化回磁盘。
- schema:每个 namespace 都有一个带版本的 schema(一个带
  `#[serde(deny_unknown_fields, default)]` 的 `serde` 派生结构体,外加一个
  `Namespace::validate` 方法处理 `deny_unknown_fields` 表达不了的跨字段
  校验);未知的 key 会被拒绝而不是被静默丢弃,因此无论是来自 `Config1.Set`
  还是手动编辑的文件,拼写错误都会立刻暴露出来。
- 监听:`nkdhrd` 用单个 `inotify` watch 监听整个配置目录
  (`IN_CLOSE_WRITE | IN_MOVED_TO`,同时覆盖原地保存和大多数编辑器及
  `nkdhrd` 自身写入所用的"写临时文件再改名"模式),根据发生变化的文件名
  分发处理。外部编辑会触发对应 namespace 的重新校验;被拒绝的文件会保持
  守护进程内存中(上一个已知有效)的值继续生效,并在日志中记录诊断信息
  (见 USAGE.md 的故障排查小节)。`Config1.Set` 会直接发出 `Changed`(见
  上方"从被调度的方法之外发出信号"),因此监听器随后重新加载自己刚写入的
  文件只是一次同值空操作,不会产生重复信号。
- IPC:`Config1.Get(key) -> Variant`、`Config1.Set(key, Variant)`、
  用于批量读取的 `Config1.GetAll(prefix) -> {key: Variant}`(供第四阶段的
  设置 UI 使用),以及一个 `Changed(key, Variant)` 信号。
- IPC 上支持的值类型:布尔值、整数、浮点数、字符串。数组和嵌套表在磁盘上的
  TOML 结构中是支持的(schema 可以自由嵌套),但暂不作为 `Get`/`Set` 的叶子
  值或 `GetAll` 的条目——目前还没有 namespace 需要这个能力,扩展
  `config_store::json_to_variant`/`variant_to_json` 中的转换逻辑只需要加一个
  match 分支,不需要重新设计。
- 验证情况:`config_store.rs` 中有四个单元测试(文件缺失时使用默认值、
  set 成功持久化并拒绝非法值、外部编辑触发带前后差异对比的重新加载、
  `get_all` 的展平逻辑),都针对一个仅供测试使用的 namespace;此外还针对
  真实守护进程用一个临时的 scratch namespace 做了一遍完整的 D-Bus 端到端
  验证(`nkdhrctl config get/set/watch`)——详见 PROGRESS.md 的 CTRL-5 记录。

## 单实例保证

`nkdhrd` 在启动时通过 `RequestName`/`DO_NOT_QUEUE` 在 session bus 上请求
`org.nkdhr.Daemon1`。第二个实例的请求会立即失败;它会记录冲突并以非零状态码
退出,而不是排队等待第一个实例(这正是 CTRL-1 的验收标准)。

**实现踩坑(zbus 5.18):** `connection::Builder::name()` 自身的文档注释声称
总是会设置 `DoNotQueue`,但实际发布的代码从未添加这个标志——通过
`.name(BUS_NAME)` 构建的第二个实例只会一直排队,而不会失败。因此 `nkdhrd`
不使用 `Builder::name()`;它只用 `.serve_at()` 构建连接,然后显式调用
`Connection::request_name_with_flags(BUS_NAME,
RequestNameFlags::DoNotQueue.into())`。升级 zbus 前,应对照其 changelog
重新检查这一点是否已在上游修复。

## 目前踩过的 zbus proxy 坑

在为 CTRL-2/CTRL-3/CTRL-4 编写后端 proxy(`nkdhrd/src/backends/*.rs`)时
遇到的真实问题,新增任何 proxy 时都值得再检查一遍:

- **方法名中的缩写。** zbus 的 snake_case→PascalCase 转换只把每个下划线
  分隔单词的首字母大写,因此 Rust 里的 `get_session_by_pid` 会变成
  `GetSessionByPid`——而不是真正的 D-Bus 方法名 `GetSessionByPID`。名字
  错了并不会给出清晰的报错:它只是不匹配任何策略里的 `send_member=`
  规则,于是被 dbus-broker 的默认拒绝策略当作一个笼统的 `AccessDenied`
  吞掉,看起来像权限问题而不是拼写错误。目前已经踩到**两次**
  (CTRL-2 的 `GetSessionByPID`,以及 CTRL-3 polkit subject 解析中的
  `GetConnectionUnixProcessID`——后者的报错是 `UnknownMethod: Invalid
  method call`,因为在错误大小写下这个名字确实不存在,而不是撞上了某条
  策略规则)。任何真实 D-Bus 名字里带有连续大写(`PID`、`SSID`、`URL` 等)
  的方法/属性,都需要显式加 `#[zbus(name = "...")]` 覆盖——在给一个不熟悉
  的接口写新调用时,应该去核对它真正的成员名(`busctl introspect`),
  而不是信任自动转换。
- **`Optional<bool>` 会 panic。** `zvariant::Optional<T>` 用 `T` 的默认值
  编码"缺失",而 `bool::default()`(`false`)没法和真正的"false"区分开——
  编码或解码 `Optional<bool>` 会按设计直接 panic(见
  `zvariant::Optional` 自身的文档注释)。任何本质上是"可能为空的布尔值"的
  线上字段,都需要换一种表示方式(比如用一个普通 `bool`,让 `false` 兼职
  当"未知"哨兵值,就像 `Audio1` 的 `muted` 字段那样;或者在确实需要把
  `false` 和"未知"区分开时,用一个小的三态枚举)。
- **"async sandwich" 很容易在不经意间踩到,不只是理论风险。** zbus 自己的
  文档警告过:不要在一次调用**内部**——该调用正是被这条连接自身的 object
  server 调度的——对同一条连接调用*阻塞*版本的 proxy 方法(阻塞封装内部的
  `block_on` 会等待同一条连接的 executor 取得进展,而当前线程恰好正被这次
  等待卡住)。CTRL-3 早期的一版草稿就正好这么做了:一个非 async 的
  `Brightness1::set` 用 `#[zbus(connection)]` 在同一条连接上通过*阻塞*
  proxy 调用获取调用者的 PID,结果卡死了整个守护进程(该连接上后续所有调用
  都会挂起,包括无关的 `Ping`),直到重启才恢复。修复方式是把接口方法本身
  改成 `async fn`,并且专门针对"这次调用抵达的那条连接"改用*异步* proxy
  变体(`.await`,而不是 `ProxyBlocking::new(...)`)。对一条真正独立的连接
  发起调用(比如 `nkdhrd` 自己的 `system` bus 连接,全程通过
  `zbus::blocking::Connection` 使用)则完全没有这种风险,即使身处一个
  `async fn` 内部也可以照常用阻塞方式调用——只有*同一条连接*上的重入
  才是危险的。

## systemd unit

`nkdhrd.service` 是一个 `systemd --user` unit(`Type=dbus`,
`BusName=org.nkdhr.Daemon1`),由 D-Bus 激活启动,或者由安装程序配置的
会话启动 target 主动启动。日志走 journal,syslog identifier 为
`nkdhrd`;没有单独的日志文件。
