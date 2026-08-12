# nkdhr UI Stack — Extension Rules

> [中文版本 / Chinese version](EXTENDING_zh_CN.md)

This document defines supported extension seams for Phase 3. It does not turn
the in-process Rust API into a stable binary plugin ABI. External loading,
version negotiation and sandboxing are implemented only by the later feature
that owns them.

## Design goals

Extensions should be able to add reusable widgets, theme token groups and
typed actions without depending on compositor backends or bypassing the
control plane. Extensions must preserve the same behavior in integrated and
standalone hosts.

## Reusable widgets

A reusable widget depends on `nkdhr-ui` public traits and records through the
provided paint context. It must not:

- access Smithay, EGL or raw GLES state;
- assume it is hosted in the compositor rather than a Wayland client;
- inspect another widget's private retained node;
- open system-service connections instead of using a typed application model;
- run blocking work during measure, arrange, paint or input dispatch.

Custom widgets expose semantic properties and events. Internal animation uses
the root clock. Accessibility/semantic data is part of the widget contract,
not optional metadata to add after visual behavior.

The implemented extension contract is `Widget`: a descriptor creates private
type-erased retained state, compares its previous descriptor in `update`, and
participates in `measure`, `arrange`, `paint`, `animation`, `event` and
`semantics` passes. Pass contexts expose the current stable ID/state, root-local
reactive watches, invalidation and animation-frame requests. Child traversal
remains explicit, so a container owns layout and paint order without reaching
into child state. Geometry-changing animation belongs in `AnimationCtx`; paint-
only timelines may continue sampling the same root clock from `PaintCtx`.
Custom input widgets declare focusability, focus-scope, pointer and clipping
behavior; event handlers request focus/capture changes, which the root applies
after callback dispatch.

`set_handled_and_continue` is reserved for a child that deliberately delegates
part of one composite transaction to an ancestor; it is not ordinary event
bubbling. The ancestor may then use `request_child_focus` to synchronize a
logical cursor with a direct focusable child. Hosts must supply modifier state
and their system-normalized nonzero click count on pointer press/release;
widgets must not invent another double-click threshold.

Clipboard access is also host-owned. A custom editable widget calls
`read_clipboard_text`/`write_clipboard_text`; it never blocks on a platform API.
The host must return read text to the `WidgetId` carried by the request, not to
whatever control happens to be focused later. Sensitive widgets must define
copy/history policy explicitly and must not place secrets in semantic values.

A custom nested scroll container must report only its mathematically
unconsumed delta with `EventCtx::handoff_scroll`; it must not mark a partial
scroll handled and replay the original delta in an ancestor. Visual boundary
feedback belongs in `Widget::scroll_boundary`, which is invoked only after the
remainder has exhausted the bubbling path. Pointer-captured text, object and
thumb drags must use normal handled propagation and never opt into scroll
handoff. Overlay controls may use `PaintCtx::register_pointer_overlay`, but the
registered rectangle must match a control actually painted above the children.
An entering overlay may use `PaintCtx::paint_child_clipped` to add one reveal
clip to that child only. Do not turn on `clips_children` for an entire panel
merely to hide one moving drawer: doing so also cuts sibling material shadows
and changes their hit clips. The arranged child, reveal clip and any pointer
barrier must describe the same visible region for the full enter/exit lifetime.

This public mechanism does not grant permission to redefine nkdhr's standard
component design ad hoc. The first owner-approved built-ins now exist as
`GlassSurface`, `Button`, `Toggle`, `Slider`, `List`/`ListItem`, `Scroll` and
`Text`/`TextInput`; extensions should compose or wrap them when their semantics fit.
Reusable structural or application-private widgets may still use `Widget`
directly. Adding a new standard family or changing built-in appearance/state
vocabulary remains an owner-controlled product-design change, while completing
the explicitly recorded advanced behavior of an already approved family is an
implementation task under its existing contract.

Custom text-producing widgets use `MeasureCtx::layout_text` and
`PaintCtx::draw_text`; they do not create their own atlas or texture store.
Their host root must be constructed with one `TextResources`, and renderer
submission must use that root's texture store so recorded glyph IDs remain in
the same resource namespace.

When a visual can be expressed with existing primitives, the widget records
those primitives. New primitive types are accepted only when they represent a
general drawing capability, have a deterministic software-oracle definition,
a GLES implementation, golden coverage and a batching strategy.

`BackdropBlurPrimitive` is an ordering and damage dependency, not a decorative
fill. A custom host that advertises backdrop capability must place the UI list
after the layers it should sample, call `PreparedDisplayList::expand_damage`
before repainting those layers, and submit the same expanded physical damage to
`GlesBackend::draw`. A host unable to satisfy all three requirements must leave
the capability disabled so material resolution selects its readable fallback.

## Theme token groups

UI-4's token runtime, read tracking and bounded declarative extension registry
are implemented. This is not a plugin loader: a trusted host assembles one
`ThemeExtensionRegistry` before constructing its runtime, and every validator
of persisted values must receive the same immutable declaration set.

An extension may define a namespaced typed token group. Names use reverse-DNS
ownership beneath `extension.<owner>.<name>`. A group declares defaults, value
types, validation and whether each token affects layout or paint.

Extensions cannot replace the types or semantics of built-in tokens. A theme
that omits an extension group leaves that extension's defaults active. An
invalid extension value rejects the complete candidate before publication, so
the previous built-in and extension snapshot remains intact. Profiles store
only sparse values beneath `overrides.extension`; they cannot carry schema or
code. Boolean, bounded integer/number/string, normalized color and closed
choice values are supported, with explicit group/token/string/choice limits.

An extension widget declares its dynamic paths in `ThemeReadSet`, receives the
complete immutable generation through `Widget::apply_theme_snapshot`, and uses
`ThemeSnapshot::read_extension`. Its descriptor's declared paint/layout impact
then participates in the same token-exact retained-tree invalidation as a
built-in. Settings preview and profile/library import/export expose registry-
aware entry points. The current static daemon deliberately has an empty
registry; persisted third-party values wait for the later trusted loader that
can distribute identical declarations to daemon, Settings and shell.

## Typed actions

The public `ActionCatalog`/`ActionRegistry<C, P>` contract is implemented. An
extension action registers:

- a stable namespaced name;
- a localized description key;
- a closed argument schema;
- declarative host capabilities controlling availability;
- whether it is instantaneous or continuous;
- an invocation handler returning a structured result.

Action names use `extension.<owner>.<action>` and lowercase ASCII dotted/hyphen
segments. Registration fails on duplicate/invalid names, invalid schema
bounds, more than 512 total actions or more than 32 arguments on one action.
Arguments are data only; schemas cannot request evaluation of shell, Rust,
JavaScript or configuration expressions. Add the descriptor to the complete
trusted catalog before compiling bindings, then attach exactly one host
adapter; do not make an unknown action name fall back to a generic callback.

Key/button/gesture binding documents are also bounded. Extension triggers use
the common compiler so modifier normalization, context/device/origin conflict
analysis, client two-finger/touch ownership and unsupported-device reporting
cannot be bypassed. An extension must not intercept a client stream itself
after its binding was rejected or marked unavailable.

Continuous adapters must accept the declared phase vocabulary. The central
dispatcher owns the interaction ID and terminal call; an adapter must not
invent a second end/cancel path, queue phases, or retain a context/payload
borrow after returning. Cleanup on cancel must be idempotent. Host code that
consumes a begin must also suppress the rest of that physical stream after an
asynchronous cancel so it is not leaked to a client mid-sequence.

Discoverability uses the exact published `BindingSnapshot`. Settings models
must not independently parse a file, reconstruct a catalog, or display a
requested binding as live when the effective generation rejected it.

Actions that need privileged work call an existing `nkdhrd` method. A future
CTRL-EXT custom command is still polkit- and schema-gated; UI action
registration never grants authority by itself.

## Canvas integration

Pinned widgets use the stable host concepts established by COMP-7: identity,
world rectangle, explicit layer, renderer-independent render payload and
node-local input. An extension works through the `nkdhr-ui` canvas adapter; it
does not implement a backend-specific `PinnedNode` solely to reach raw GLES.

UI-5's concrete adapter is `UiPinnedNode` over an object-safe `UiSurface`.
Reusable applications should implement or compose that surface boundary and
let the host own placement, scale, input normalization and context-bound GLES
resources. A surface commit must advance whenever its display-list commands or
referenced texture revisions change; otherwise a host is allowed to reuse the
last prepared frame. Extensions must not cache a `PreparedDisplayList` across
GLES contexts or apply canvas world transforms inside the retained tree.

The later third-party pinned-widget loader adapts its versioned boundary into
the in-process registry. That loader is intentionally deferred until the
first-party SHELL-5 widgets have established the real lifecycle and permission
needs.

## Compatibility

The Rust crates follow source compatibility within a release series but do not
promise a stable Rust ABI. Persisted extension configuration and action names
are versioned data contracts. Removing or changing one requires a migration
path and a deprecation period.

Every extension-facing addition must include:

1. public API documentation and one minimal example;
2. deterministic layout/input tests;
3. golden coverage for new visuals;
4. verification in both canvas and standalone hosts where applicable;
5. a statement of resource limits and teardown behavior.
