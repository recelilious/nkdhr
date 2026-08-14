# nkdhr 产品功能与验收证据矩阵

本文件是进入统一验收前的唯一完成口径。`done` 只表示功能、集成路径、自动回归与必要的
真实会话验证均有可定位证据；只有数据模型或组件框架时必须标为 `foundation`，不得按成品
计数。用户已经确认的交互不会因实现难度而缩减。

状态：`done` 已完成；`partial` 有可用子集；`foundation` 只有可复用基础；`missing` 尚未实现。

## 1. 合成器、显示器与窗口

| ID | 要求 | 状态 | 当前证据 | 进入验收仍需 |
|---|---|---|---|---|
| C-01 | Wayland/XWayland、剪贴板、DND、缩放、锁屏、截图与 TTY/嵌套后端 | done | `crates/nkdhr-canvas/src/{state,protocols,backends}`；Phase-2 实机与 soak 文档 | 统一验收再做本机长稳回归 |
| C-02 | 无限画布、重叠自由窗口、网格吸附、平移/总览/位置标记 | done | `canvas/world.rs`、`input.rs`、`canvas/marks.rs` | 与壁纸/绘制层组合回归 |
| C-03 | 多显示器独立本地活动；每个输出组独立工作区/视口/焦点 | done | `canvas/workspace.rs`、`state.rs`；Super+1…9/0；双后端 300 ms 淡变 | VKMS 与物理双屏最终回归 |
| C-04 | 工作区编号全局唯一；空闲编号归当前显示器；已在另一显示器显示时交换 | done | `WorkspaceAssignments` 三组单元测试 | 热插拔/交换实机回归 |
| C-05 | 新窗口与跨工作区窗口的中心/八方向摆放，方向键及标准 HJKL 可叠加抵消 | partial | `canvas/placement.rs`；Super+Shift+数字移动、110 ms 松键确认、鼠标边缘区、取消恢复 | 将同一会话接到启动器新窗口；把轮廓升级为正式材质 |
| C-06 | Super+Shift+方向逐格移动；Super+Ctrl+方向固定左上、只改右/下边 | done | 类型化 action 与 `actions.rs`；方向键/HJKL | 设置页可视化编辑最终回归 |
| C-07 | Super+Escape 关闭；默认终端/快捷应用与所有按键均可配置 | partial | 关闭、类型化热重载绑定已完成 | 默认终端、快捷应用、完整设置表面 |
| C-08 | 全屏时边缘组件自动隐藏/呼出 | missing | — | output-local shell 与全屏状态集成 |

## 2. 视觉、主题与动效

| ID | 要求 | 状态 | 当前证据 | 进入验收仍需 |
|---|---|---|---|---|
| V-01 | 静态为模糊液态玻璃 + claymorphism 体积；动态为灵动粘稠史莱姆 | partial | `nkdhr-render` blur/inset shadow；Settings 正式 golden | 后续所有 shell 组件统一采用，不允许平面占位皮肤 |
| V-02 | 斜向左上→右下阴影、弱边线、窄主题感知内阴影、内容避开阴影 | done | theme surface token 与 Settings golden/组件测试 | 新组件必须复用相同 token |
| V-03 | 圆角按组件尺寸变化；Maple Mono 默认；适当斜体 | partial | Settings 组合与上中 calm clock 已采用尺寸层级/Maple Mono 字体资源 | 其余 shell、launcher、画板完整采用 |
| V-04 | Tokyo Night/Nord 内置、自定义、壁纸取色、完整语义色、覆盖/保存/导入导出 | foundation | `nkdhr-theme` profile/library/wallpaper palette 与原子热同步 | 完整主题设置 UI、真实壁纸源与 shell 消费 |
| V-05 | 普通/专业模式；普通模式 UI 优先，专业模式开放参数与命令 | partial | Settings 专业 motion 工作区与主题事务 | 全设置分类、专业开关、命令入口 |
| V-06 | 动画曲线坐标轴、双击插点、切线/数值、撤销、预览、导入导出 | done | UI-7A…E、`motion_editor*`、Settings 集成与 golden | 新 motion consumer 覆盖 |
| V-07 | 动画曲线/时长/流体参数热重载；Reduced/Off 无空间动效 | done | `motion_runtime`、`theme_runtime`、CTRL-5 bridge；output-local shell 共享同一热主题 runtime | 后续 shell 动效逐项复用 |
| V-08 | 选中质量像粘液/磁流体传递，连续重定向、穿越内容折射 | done | `SelectionMassMotion` 与 Settings 左导航动态/策略测试 | 左上/右上 shell 链复用并做视觉验收 |
| V-09 | 动画存在小幅确定性差异，水面/待机信号持续运动 | foundation | semantic fluid/idle-water runtime | 应用聚合水面和系统链真实绘制 |

## 3. 八区域 shell

| ID | 要求 | 状态 | 当前证据 | 进入验收仍需 |
|---|---|---|---|---|
| S-01 | 输出本地 overlay 层；四角与四边中心独立布局/命中/动画 | partial | `nkdhr-shell` + `shell_host.rs`；每物理输出独立 retained surface、八个稳定区域 ID、输入隔离、热插拔清理；nested GLES 冒烟通过 | 组合其余七区并完成各区动画/命中回归 |
| S-02 | 左上：项目图标独立；悬停系统信息，点击居中设置 | missing | — | shell host、设置调起与系统摘要 |
| S-03 | 左上：已打开应用链，应用聚合多一层背景，工作区独立 | missing | 窗口与焦点基础存在 | app identity、窗口聚合、液态节点 UI |
| S-04 | Alt+Tab 快按普通轮换；按住 Alt 展开当前工作区空间预览 | partial | 快按 Alt+Tab 已有 | hold 状态机、预览图、连线、选中移动 |
| S-05 | 预览按真实相对位置；重叠窗口图标可重叠；聚合应用可分裂左右并按最近使用收回 | missing | — | app/window MRU、空间投影、分裂/回收动效 |
| S-06 | 多页面同应用：母图标水面常动，最多五份水面分割；点/星数量守恒 | foundation | 通用流体 runtime | 专用聚合组件与所有计数状态测试 |
| S-07 | 应用过多时专用折叠节点，仍按同样逻辑分裂展开 | missing | — | overflow identity 与预览展开 |
| S-08 | 上中：默认数字时间，交互时扩展为复杂“灵动岛” | partial | output-local calm 数字时间已真实组合；Maple Mono、Tokyo Night clay/glass、背景模糊与输入屏障通过 nested 截图 | 由后续正式需求实现扩展子状态，不擅自补产品行为 |
| S-09 | 右上：电源/Wi‑Fi/蓝牙/音量/亮度等粘稠节点链；悬停摘要、点击详情 | foundation | `nkdhrd` 已有 power/network/audio/brightness/session | Bluetooth、UI、动作与状态 generation |
| S-10 | 左中：鼠标靠近显示的画板工具栏；设置/命令可完全关闭 | missing | — | 与绘制层共同实现 |
| S-11 | 右中：插件自定义栏，可承载 AI/临时终端等 | missing | — | 沙箱化插件 API、生命周期、权限与示例 |
| S-12 | 下中：Super 单键显隐的启动器/搜索/命令/临时终端 | missing | 动作、文本输入、列表组件基础已完成 | output-local 应用与会话状态机 |
| S-13 | 左下：Ctrl+Alt+V 系统剪贴板历史，图片/链接/文件/文字 | missing | Wayland selection 只有当前项 | 历史守护、敏感策略、预览与粘贴 |

## 4. 启动器、系统命令与终端

| ID | 要求 | 状态 | 当前证据 | 进入验收仍需 |
|---|---|---|---|---|
| L-01 | 应用、动作与指令统一搜索；键盘/鼠标选择 | foundation | `List`、`TextInput`、typed action catalog | launcher 应用索引与统一结果模型 |
| L-02 | `/` 为 nkdhr 系统命令；分类、参数规范、逐段提示和 Tab 补全 | missing | `nkdhrctl` 有部分后端动作 | 命令 AST、补全、usage、执行反馈 |
| L-03 | Tab 无可补时等同 Enter；应用列表中 Tab 也等同 Enter | missing | TextInput 可声明 Tab completion | launcher 级规则与测试 |
| L-04 | 强 UI 项的命令自动打开设置到准确位置 | missing | Settings 页面模型存在 | deeplink/action 路由 |
| L-05 | `:` 执行第三方 shell 命令并向上扩为临时终端，复用默认终端配置 | missing | — | PTY、终端模拟器、尺寸/输入/退出策略 |
| L-06 | Super 收起会强制终止临时终端；按钮或默认 Super+Space 迁移为正式终端窗口 | missing | — | 进程组安全终止与 surface handoff |
| L-07 | 成功/参数错误/调度错误在底栏明确反馈 | missing | Settings 有 generation feedback 模式 | launcher feedback 组件 |

## 5. 壁纸、网格与画板

| ID | 要求 | 状态 | 当前证据 | 进入验收仍需 |
|---|---|---|---|---|
| W-01 | 固定壁纸模式：壁纸不动，窗口+网格移动；工作区可独立壁纸与淡变 | foundation | 工作区与 theme palette 已有 | 解码、GPU 背景、每工作区状态与淡变 |
| W-02 | 画布壁纸模式：壁纸、网格与画布完全同步 | missing | — | 世界坐标背景渲染 |
| W-03 | 1 或 3×3 壁纸模板；每张独立拉伸/裁剪；邻边虚化融合并无限重复 | missing | 需求已定为 3×3，不做指数任意矩阵 | 编辑 UI、离线拼合、瓦片采样与边界测试 |
| W-04 | 默认十字网格，壁纸上可读；网格可配置 | partial | 32 单位吸附逻辑存在 | 真正背景网格绘制、样式设置 |
| W-05 | 画布绘制达到 Excalidraw 级核心：选择、手绘、形状、箭头、文字、撤销等 | missing | retained UI/纹理/输入基础 | 绘制文档、工具、Rough.js/Rough-rs 等许可审计 |
| W-06 | 关机保存、开机恢复绘制；窗口只尽量在原位置重开 | missing | — | 版本化场景存储、恢复协调器 |
| W-07 | 画布导入/导出配置与截图 | partial | 输出 screencopy 已有 | 场景格式、全画布/选区导出、组合截图 |
| W-08 | 手绘字体许可可用则引用，否则 Maple Mono | missing | Maple Mono 资源基础 | 字体选择与 attribution |

## 6. 会话、游戏与扩展

| ID | 要求 | 状态 | 当前证据 | 进入验收仍需 |
|---|---|---|---|---|
| P-01 | 应用规则可自动进入游戏模式 | missing | — | app identity、规则与系统策略事务 |
| P-02 | 仅当所有触发自动游戏模式的应用都退出才自动退出 | missing | — | 引用计数/集合守恒与崩溃清理 |
| P-03 | 右栏插件可自定义，并支持 AI 示例 | missing | extension token 不是插件运行时 | 清单、权限、进程隔离、host API、示例 |
| P-04 | 设置、文件、任务、greeter 与 sessiond 是真实应用/服务 | missing | 四个 crate 仍为空 `main` | 实现并与共享 UI/主题/会话集成 |
| P-05 | 关机/重启/睡眠、登录会话与自动恢复串联 | partial | `nkdhrd` power/session 与 compositor lock 已有 | sessiond/greeter/systemd/恢复流程 |

## 7. 统一验收门槛

只有下列条件同时满足才可以请求项目所有者进入验收：

1. 本矩阵所有产品项均为 `done`，或经项目所有者明确移出当前版本；不能以
   `foundation` 代替 `done`。
2. `cargo clippy --workspace --all-targets --all-features -- -D warnings` 与
   `cargo test --workspace --all-features` 全通过，所有预期视觉变更有已检查 golden。
3. 嵌套后端完成键鼠/触控板、launcher、命令、终端、剪贴板、画板与恢复回归。
4. VKMS 双输出完成独立工作区、跨屏交换、热插拔、壁纸/网格与 output-local shell 回归。
5. 本机 TTY 完成 Intel Iris Xe、XWayland、锁屏、screencopy、VT 往返、睡眠恢复与
   8 小时 soak；不会修改或依赖另一个桌面会话的设置。
6. 许可清单包含所有采用的手绘算法、字体、图标及资源出处；导入数据均有大小、版本、
   路径与恶意输入边界。
