# nkdhr

> [中文版本 / Chinese version](README_zh_CN.md)

nkdhr is a self-contained desktop environment for Linux, built around a
custom Wayland compositor whose window-management model is an infinite
pannable, zoomable canvas: windows are placed freely in world coordinates,
each output is a viewport into that world, and widgets can be pinned
directly onto the canvas.

All system-level UI — login screen, bar, launcher, notifications, settings,
file manager, task manager, lock screen — is developed in this project on a
single custom OpenGL ES UI stack, targeting a stock minimal Fedora Linux
base.

The project is in early development. Documentation will land in `docs/` as
components are completed.

## Status

Pre-implementation: planning and repository bootstrap.

## License

[PolyForm Noncommercial License 1.0.0](LICENSE.md): use, modification and
redistribution are free for any noncommercial purpose; commercial use is
not permitted.
