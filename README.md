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
UI-3's style-neutral framework core is now implemented: retained generational
identity, keyed reconciliation, finite constraint layout, queued reactive
state, shared paint/hit order, focus scopes, pointer capture/hover, semantics
and host-clocked animation. Its owner-reviewed product layer is now in
production translation: typed density/spacing/radius/typography, glass-material
and motion profiles plus the first public `GlassSurface`, `Button`, `Toggle`,
`Slider`, `List`, `Scroll`, `Text` and `TextInput` implementations are present.
The retained tree now owns a shared shaping/atlas resource boundary: public text,
component labels and TextInput glyph hit testing render real Unicode/CJK/emoji
through the same texture store. `Scroll` now adds overlay thumb dragging and
track paging, Shift-wheel and Vim-key navigation, host-clocked interruptible
inertia, bounded elastic feedback, opt-in snapping, exact nested-delta handoff,
revisioned anchoring/minimal reveal and conditional tail following; reduced
motion removes its spatial effects. `List` now adds stable-identity multi-
selection with range/discontiguous input, real-focus keyboard/typeahead
navigation, tree disclosure, explicit-handle pointer and keyboard reorder,
virtual leading/trailing extent, loading rows, object double-click and context
actions. `TextInput` now adds multiline/BiDi visual selection fragments, IME
composition selection, multi-click/drag and visual/word/line navigation,
targeted host clipboard requests, non-sensitive undo/redo, explicit formatter
selection mapping and generation-ordered change/blur/submit validation. UI-3
now also has a real painter-order backdrop-filter primitive: the scalar oracle
and Smithay GLES path share rounded masks, transforms, clips and separable blur
semantics, while prepared lists expose dependency-expanded compositor damage
instead of recursively sampling prior glass pixels. `GlassSurface` emits that
pass only for a capable host and retains the approved compensated fallback.
The accepted Appearance & Interaction composition now lives in
`nkdhr-settings` as a host-independent production view model rather than a
test-only gallery. It implements the exact four responsive frames, independent
navigation/content scrolling, grouped setting surfaces, professional inspector
drawer, live reactive controls and undo/status feedback. Only the outer window
requests backdrop blur; a deterministic full-CJK software golden and all four
width oracles guard the result. The inspector now uses interruptible host-
clocked panel/drawer entry and exit while preserving its input barrier through
the complete narrow-screen transition; Reduced/Off removes spatial movement.
Button, Toggle and Slider expose stable visible pending edges, and the latter
two preserve requested versus effective values. Settings provides per-setting
generation-ordered begin/complete feedback tokens, allowing independent
concurrent pending states while stale same-setting replies cannot replace newer
state; its global speed control scales every control, panel and fluid
duration without changing the selected curves. The standalone/in-compositor
window hosts still belong to later milestones. UI-3 remains open for owner
review of the real static-glass frame, compact navigation icons and transition
output.

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
