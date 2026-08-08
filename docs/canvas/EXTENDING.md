# Canvas — Extension Seam: Third-Party Pinned Widgets

> [中文版本 / Chinese version](EXTENDING_zh_CN.md)

Status: **future extension design**, not a shipped plugin loader. This document
records the contract COMP-7's `PinnedNode` trait already respects so
third-party pinned widgets can be registered later, after
SHELL-5 (the first-party pinned widgets: clock, system monitor) ships,
without breaking API changes. Do not build a plugin-loading mechanism as
part of COMP-7 itself — COMP-7 ships exactly one built-in `PinnedNode`
(a static pinned image) to prove the trait boundary; that's the whole
milestone.

## Goal

Let a third party add a new kind of pinned canvas widget (a weather panel,
a to-do list, anything with a world-space position and something to
render) without patching `nkdhr-canvas` or `nkdhr-ui` themselves.

## Why COMP-7's design already supports this

`PinnedNode` (see `INTERNALS.md`'s COMP-7 section) is
intentionally the *narrowest* interface that lets a node participate in
the canvas's render loop and input dispatch on equal footing with a
client window: an identity, a world-space rect, an explicit layer,
renderer-independent render data and backend-independent pointer events.
Nothing about it assumes the node is a first-party widget or backed by
`nkdhr-ui`. The in-process Rust trait itself is not an ABI promise to
independently compiled code; the later loader must adapt its stable boundary
into this registry.

## Design

A registry, `Vec<Box<dyn PinnedNode>>`, already exists inside
`canvas/world.rs` from COMP-7 onward (it's how the compositor's own
render loop iterates pinned nodes alongside windows) — nothing new there.
What this extension adds later is a *loading* mechanism on top:

- A pinned-widget manifest format (`~/.config/nkdhr/canvas-widgets/
  <name>/manifest.toml`: display name, a path to a shared library
  exposing a separately specified stable `extern "C"` boundary). The host
  adapter implements Rust's `PinnedNode`; the shared library never exchanges
  Rust trait objects or Smithay renderer types across the ABI.
- `nkdhr-settings` (SHELL-6) gains a pinned-widgets list, each entry
  toggle-able and drag-placeable on the canvas the same way a first-party
  one is — because to the canvas's render loop, there is no difference.
- Placement uses CTRL-5, but the exact representation is intentionally
  deferred until SHELL-5/6 designs the registry schema. Today's config store
  cannot create arbitrary nested leaves through `Set`; claiming
  `canvas.pinned-widgets.<name>.position` now would repeat the dynamic-key
  problem COMP-4 solved for marks. The later design must either use one
  encoded registry leaf or extend CTRL-5 generically before documenting a
  dotted dynamic path.

## Out of scope for the first version of this extension

- Sandboxing third-party widget code (no seccomp/namespace isolation) — a
  pinned widget runs with the full privileges of the compositor process
  it's loaded into. This is a meaningfully larger trust boundary than
  CTRL-EXT's custom commands (which run as ordinary child processes, not
  loaded into a privileged-adjacent graphics process) and needs its own
  security review before this extension actually ships, not just a design
  doc — flagged here so it isn't missed when SHELL-5 is done and this
  seam gets picked up.
- A non-Rust widget SDK (e.g. a WASM or scripting-language binding) —
  the `extern "C"` boundary above is the minimum to prove out-of-process
  compilation works at all; a friendlier authoring story is a later
  iteration on top of it.
