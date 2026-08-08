# nkdhr

*A self-contained Linux desktop environment built around a custom Wayland
compositor where windows live on an infinite pannable, zoomable canvas.*

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

The project is in early development. Accepted component documentation lives
in `docs/` in English and Simplified Chinese.

## Status

Phase 2 was accepted on 2026-08-08. Phase 3 — the shared rendering and UI
toolkit — is now underway: UI-1 provides the renderer-independent primitive
display list, batched Smithay GLES backend, deterministic golden-image oracle
and live offscreen cross-check. Its 1,000-primitive native 2560×1600 benchmark
reaches a 1.228 ms p95 GPU duration on the reference Iris Xe. UI-2 adds
advanced Unicode shaping, CJK/emoji font fallback, color emoji, cached
paragraph layout and bounded mask/color glyph atlases. Its 5,000-line,
265,000-glyph scrolling benchmark records clipped text at 0.262 ms CPU p95.
UI-3 retained widgets and layout are next.

## Documentation

- [Control plane user guide](docs/control-plane/USAGE.md) ·
  [中文](docs/control-plane/USAGE_zh_CN.md)
- [Control plane internals](docs/control-plane/INTERNALS.md) ·
  [中文](docs/control-plane/INTERNALS_zh_CN.md)
- [Canvas user guide](docs/canvas/USAGE.md) ·
  [中文](docs/canvas/USAGE_zh_CN.md)
- [Canvas internals](docs/canvas/INTERNALS.md) ·
  [中文](docs/canvas/INTERNALS_zh_CN.md)
- [Pinned-widget extension seam](docs/canvas/EXTENDING.md) ·
  [中文](docs/canvas/EXTENDING_zh_CN.md)

## License

[PolyForm Noncommercial License 1.0.0](LICENSE.md): use, modification and
redistribution are free for any noncommercial purpose; commercial use is
not permitted.
