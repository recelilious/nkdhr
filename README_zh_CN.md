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
公共 `GlassSurface`、`Button`、`Toggle`、`Slider`、`List`、`Scroll` 和
`TextInput` 实现已经存在。UI-3 仍未完成；保留式文字呈现、完整高级组件行为、
合成器真实背景模糊和已确认的设置界面仍需接入并完成视觉验收。

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
