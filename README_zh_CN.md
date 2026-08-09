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
指针捕获与 hover、语义树及宿主时钟动画。经过共同确认的产品层现已开始
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
记录该 pass，并保留已确认的不支持时补偿填充。UI-3 仍未完成；接下来需要接入已确认
的设置界面，并对真实静态玻璃和转场输出完成视觉验收。

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

## 协议

[PolyForm Noncommercial License 1.0.0](LICENSE.md)：任何非商业目的的使用、
修改、再分发完全自由；禁止商业使用。
