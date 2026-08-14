# nkdhr

*一个自成一体的 Linux 桌面环境，核心是自研 Wayland 合成器，
窗口生活在一张可无限平移缩放的画布上。*

> [English version / 英文版本](README.md)

nkdhr 是一个自成一体的 Linux 桌面环境，核心是一个自研的 Wayland 合成器，
其窗口管理模型是一张可无限平移、缩放的画布：窗口以世界坐标自由摆放，
每个显示器是这个世界中的一个视口，组件（widget）可以直接钉在画布上。

所有系统级 UI——登录界面、状态栏、启动器、通知、设置、文件管理器、
任务管理器、锁屏——全部在本项目中基于同一套自研 OpenGL ES UI 栈开发，
目标运行环境是原版最小安装的 Fedora Linux。

项目处于早期开发阶段。已验收组件的中英文文档保存在 `docs/` 目录。

## 状态

Phase 2 已于 2026-08-08 验收。Phase 3 共用渲染层和 UI 工具包现已启动：
UI-1 已实现与渲染器解耦的图元显示列表、基于 Smithay GLES 的批处理后端、
确定性的黄金图像测试基准和真实离屏 GLES 交叉验证。在参考 Iris Xe 上，
其 2560×1600 原生分辨率、1,000 图元基准的 GPU p95 为 1.228 ms。
UI-2 已实现高级 Unicode 塑形、CJK/emoji 字体回退、彩色 emoji、段落布局缓存，
以及有界的遮罩/彩色字形图集；5,000 行、265,000 字形的裁剪滚动记录基准
达到 CPU p95 0.262 ms。UI-3 无样式框架核心现已实现：保留式代际标识、
keyed 重建、有限约束布局、排队式响应状态、统一绘制/命中顺序、焦点域、
指针捕获与 hover、语义树及宿主时钟动画。经过共同确认的产品层现已
转入生产代码：类型化密度、间距、圆角、排版、玻璃材质和运动配置，以及首批
公共 `GlassSurface`、`Button`、`Toggle`、`Slider`、`List`、`Scroll`、
`Text` 和 `TextInput` 实现已经存在。保留式组件树现已拥有统一的文字塑形/字形图集
资源边界：公共文字、组件标签与 TextInput 字形命中均通过同一个纹理仓库绘制真实
Unicode/CJK/emoji。`Scroll` 现已支持覆盖式滚动条拖拽与轨道翻页、Shift 滚轮和
Vim 方向键、由宿主时钟驱动且可中断的惯性、有界弹性反馈、可选吸附、嵌套滚动
精确余量传递、带版本的锚定/最小显露与条件式尾随；Reduced 模式会移除空间动效。
`List` 现已支持基于稳定身份的多选、区间/离散输入、同步真实焦点的键盘与 typeahead
导航、树形 disclosure、显式手柄鼠标/键盘重排、虚拟列表前后 extent、loading 行、
对象双击与上下文操作。`TextInput` 现已支持多行/BiDi 视觉选区片段、IME 组合选区、
多击/拖动与视觉/词/行导航、定向宿主剪贴板请求、非敏感内容撤销/重做、显式格式化
选区映射，以及带 generation 顺序保护的 change/blur/submit 验证。UI-3 现在也已具备
真正遵循绘制顺序的背景滤镜图元：标量 oracle 与 Smithay GLES 路径共享
圆角遮罩、变换、裁剪和可分离模糊语义，prepared list 会向合成器公开扩张后的背景
依赖 damage，避免递归采样上一帧的玻璃像素。`GlassSurface` 只在宿主确实具备能力时
记录该 pass，并保留已确认的不支持时补偿填充。已确认的“外观与交互”组合现已进入
`nkdhr-settings` 的宿主无关正式视图模型，不再只是测试 gallery：四档响应式框架、
导航/内容独立滚动、共享分组表面、专业检查器抽屉、实时响应控件和撤销/状态反馈均已
接入。只有最外层窗口会请求背景模糊；完整 CJK 的确定性软件 golden 与四档宽度 oracle
守护结果。专业检查器现由宿主时钟驱动可中断的面板/抽屉进出，在整个窄屏退出过程中
保持输入屏障；Reduced/Off 会移除空间位移。Button、Toggle、Slider 具备不改变布局的
可见 pending 边缘，后两者同时保留请求值与实际生效值。Settings 通过按设置项独立的
generation 排序 begin/complete token 支持并发等待，并阻止同一项的旧后端结果覆盖新请求；
全局速度会缩放控件、面板和流体运动的全部时长而不改变曲线。项目所有者已经接受静态
与转场帧，UI-3 因此收口。UI-4 框架现已在不改变这些视觉的前提下完成：
`nkdhr-theme` 定义带版本、可移植的 Tokyo Night/Nord 和壁纸基础 profile，支持稀疏
显式覆盖并始终携带冻结回退调色板；`nkdhrd` 把完整 profile 作为一个 CTRL-5 叶子
校验和发布；`nkdhr-ui` 提供不可变 generation 快照、类型化语义 token 读取和精确的
paint/layout 差异。live root 会在 retained tree 的安全边界同步，所以有效的颜色/尺寸
变更无需重启即可生效，无效候选则保留上一个已知有效 generation。外观设置现已持有
宿主无关的 profile 编辑事务：完整主题会立即通过同一 runtime 预览，取消时恢复已保存
基线，generation 排序的宿主请求则通过 CTRL-5 异步提交，不会阻塞 UI 线程。另一个原子
叶子 `theme.library` 保存经过完整校验的用户主题，并支持保存、复制和有界 JSON 导入/
导出；写入失败会保留本地工作，外部变更则会被采用或显式标为冲突，不会静默覆盖预览。
确定性且资源有界的壁纸适配器现可把宿主解码后的 RGBA8 像素转换为完整、可读的语义
调色板，并提供自动/深色/浅色外观以及色彩强度、对比度输入。实时链接的生成任务按
generation 排序，因此较慢的旧壁纸结果不能覆盖新结果；更新只替换冻结基础和来源，全部
显式覆盖都会保留。干净状态会产生原子持久化请求，已有本地修改则继续明确保持未保存。
一个有资源上限的声明式扩展 token 注册表完成了该框架：反向域名组声明类型化默认值和
精确 paint/layout 影响，稀疏 profile 值不能替换内置项或携带代码，无效组会保留上一份
有效快照。设置/资料库事务与 retained 组件使用同一份不可变值。多 root 测试证明共享
runtime 的显示器仍只在各自本地活动边界同步，也覆盖直接跨过中间 generation 的情况。
UI-5 现已让同一个 retained Settings 表面分别通过合成器直接 display list 与独立
Wayland/EGL 窗口运行。UI-6 完成类型化交互基础：有界且可热重载的键盘/按钮/手势
文档、经过验证的命名 action、最后有效快照、冲突诊断以及唯一的 instant/continuous
dispatcher 共同接管已有画布输入词汇；Settings 直接消费同一份有效快照而不自行重建。
Phase 3 现已进入 UI-7，其产品与交互规范已经与项目所有者共同确认；UI-7A 加入可移植
的 2–64 锚点分段曲线数据、确定性 f64 编译/求值、解析式超调/反向校验、形状不变的
精确新增点，以及从现有 cubic 无损迁移。UI-7B 进一步加入固定版本的预设快照、配置/
语义族/稳定组件/具体过渡四层原子继承与逐字段来源、编译后最后有效主题快照，以及事务式
预设资料库持久化。Balanced revision 1 精确保留已验收输出；其余已确认风格身份会等到
与所有者共同校准后再获得参数。UI-7C 进一步加入最终
Expressive/Standard/Reduced/Off 策略运行时、保留状态与速度且只追踪最新目标的中断、
多节点选中质量守恒，以及确定且不改变端点的语义流体/常动水面信号。这些仍只是执行基础；
UI-7D 现已加入无样式的专业曲线编辑文档、consumer 能力交集、精确插点、切线/数值
编辑、有界事务历史、安全关键帧剪贴板、规范化/真实时间视口、宿主时钟预览，以及统一的
定向鼠标/笔/触摸/触控板/键盘输入语义。UI-7E 已把经过所有者验收的专业工作区、
持久化转场编辑器、不可变预设浏览器和安全草稿替换流程组合进真实共享 Settings；左侧
导航也已成为首个策略运行时视觉采用者，以守恒选中质量穿越节点、折射其绘制内容，并从
当前可见质量和切线连续重定向。Reduced/Off 会直接收敛而不保留空间拓扑。
合成器也已有首个 Phase-4 shell 基础：全局编号工作区可在各输出组独立本地活动，保留
各自画布、视口和焦点；请求另一组正在显示的编号时安全交换，并在嵌套与 TTY 后端对窗口
堆栈做淡入淡出。统一类型化绑定把 Super+1…9/0 留给该模型，并将持久位置标记迁到
Super+Alt+数字。

## 文档

- [控制面用户指南](docs/control-plane/USAGE_zh_CN.md) ·
  [English](docs/control-plane/USAGE.md)
- [控制面内部实现](docs/control-plane/INTERNALS_zh_CN.md) ·
  [English](docs/control-plane/INTERNALS.md)
- [画布用户指南](docs/canvas/USAGE_zh_CN.md) ·
  [English](docs/canvas/USAGE.md)
- [画布内部实现](docs/canvas/INTERNALS_zh_CN.md) ·
  [English](docs/canvas/INTERNALS.md)
- [固定组件扩展接口](docs/canvas/EXTENDING_zh_CN.md) ·
  [English](docs/canvas/EXTENDING.md)
- [主题扩展令牌](docs/theme/EXTENDING_zh_CN.md) ·
  [English](docs/theme/EXTENDING.md)
- [共享 UI 用户与应用开发指南](docs/ui/USAGE_zh_CN.md) ·
  [English](docs/ui/USAGE.md)
- [共享 UI 内部实现](docs/ui/INTERNALS_zh_CN.md) ·
  [English](docs/ui/INTERNALS.md)
- [共享 UI 扩展规则](docs/ui/EXTENDING_zh_CN.md) ·
  [English](docs/ui/EXTENDING.md)

## 协议

[PolyForm Noncommercial License 1.0.0](LICENSE.md)：任何非商业目的的使用、
修改、再分发完全自由；禁止商业使用。
