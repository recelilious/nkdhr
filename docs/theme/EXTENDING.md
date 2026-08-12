# Theme extension tokens

> [中文版本 / Chinese version](EXTENDING_zh_CN.md)

Status: the declarative registry and runtime path are shipped. This is not a
plugin loader and does not load executable code from a theme profile.

An extension owns one reverse-DNS group such as
`extension.com.example.widget`. Before creating a `ThemeRuntime`, its host
registers a bounded list of token descriptors in `ThemeExtensionRegistry`.
Each descriptor declares one default, one value type and whether a change
requires paint or layout. The supported types are boolean, bounded integer,
bounded number, bounded string, color and a closed choice list.

Profiles store only sparse values, without schemas or code:

```json
{
  "overrides": {
    "extension": {
      "com.example.widget": {
        "gap": 12.5,
        "accent": "#aabbccff"
      }
    }
  }
}
```

The runtime path of those leaves is
`extension.com.example.widget.gap` and
`extension.com.example.widget.accent`. Missing values resolve to the
registered defaults. An unknown group, unknown token, wrong JSON type or
out-of-range value rejects the complete candidate before publication, leaving
the previous immutable generation visible. Extension groups cannot replace
built-in leaves such as `palette.*` or `spacing.*`.

Widgets declare the namespaced paths they read through `ThemeReadSet` and read
their resolved values from `ThemeSnapshot::read_extension`. The retained tree
then uses the descriptor's paint/layout classification exactly like a built-in
token. Multiple roots sharing one runtime still synchronize independently at
their own activity boundaries; a root may skip intermediate generations and
diff directly from its last observed snapshot.

`ThemeProfileLibrary` and the Settings editor have registry-aware entry points
so preview, validation and import/export use identical declarations. Every
process that validates persisted extension values must receive that same
registry. The current static `nkdhrd` setup intentionally has an empty
extension registry, so third-party groups are not persistable through CTRL-5
until the future plugin-loading milestone distributes trusted declarations to
the daemon, Settings and shell together.

Resource bounds are enforced at registration and resolution: at most 256
groups, 256 tokens per group, 256 choices per choice token and 64 KiB per
string token. Profiles remain subject to the existing 1 MiB limit.
