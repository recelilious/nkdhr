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

Phase 2 已于 2026-08-08 验收：Phase 1 控制面与 Phase 2 画布合成器均已实现，
包括真实 TTY 运行、
多显示器输出组、物理热插拔、画布导航、协议兼容及最终八小时稳定性测试。
下一步是 Phase 3 的共用渲染层和 UI 工具包，目前尚未开始。

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
