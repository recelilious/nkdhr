# Deterministic Settings text fixture

`MapleMonoNF-CN.appearance.subset.ttf` and its Italic companion are modified
glyph subsets of Maple Mono NF CN. They keep the owner-approved default UI face
and contextual italic metadata deterministic. `NotoSansCJKsc.appearance.subset.otf`
remains the fallback oracle for the same Settings strings, so tests do not
depend on fonts installed by the host.

Regenerate the Maple Mono fixtures with fonttools (replace `$MAPLE_DIR` with a
Maple Mono NF CN installation):

```sh
pyftsubset "$MAPLE_DIR/MapleMono-NF-CN-Regular.ttf" \
  --text-file=crates/nkdhr-settings/tests/fonts/appearance-settings.txt \
  --output-file=crates/nkdhr-settings/tests/fonts/MapleMonoNF-CN.appearance.subset.ttf \
  --layout-features='*' --glyph-names --symbol-cmap --legacy-cmap \
  --notdef-glyph --notdef-outline --recommended-glyphs --name-legacy \
  --name-languages='*' --drop-tables= --no-hinting

pyftsubset "$MAPLE_DIR/MapleMono-NF-CN-Italic.ttf" \
  --text-file=crates/nkdhr-settings/tests/fonts/appearance-settings.txt \
  --output-file=crates/nkdhr-settings/tests/fonts/MapleMonoNF-CN-Italic.appearance.subset.ttf \
  --layout-features='*' --glyph-names --symbol-cmap --legacy-cmap \
  --notdef-glyph --notdef-outline --recommended-glyphs --name-legacy \
  --name-languages='*' --drop-tables= --no-hinting
```

Regenerate it with fonttools from the SC face of `NotoSansCJK-VF.ttc`:

```sh
pyftsubset /usr/share/fonts/google-noto-sans-cjk-vf-fonts/NotoSansCJK-VF.ttc \
  --font-number=2 \
  --text-file=crates/nkdhr-settings/tests/fonts/appearance-settings.txt \
  --output-file=crates/nkdhr-settings/tests/fonts/NotoSansCJKsc.appearance.subset.otf \
  --layout-features='*' --glyph-names --symbol-cmap --legacy-cmap \
  --notdef-glyph --notdef-outline --recommended-glyphs --name-legacy \
  --name-languages='*' --drop-tables= --no-hinting
```

Maple Mono copyright is held by The Maple Mono Project Authors (2022); Noto
copyright remains with its original authors. These subsets are redistributed
under the SIL Open Font License 1.1 in
`crates/nkdhr-ui/tests/fonts/OFL-1.1.txt`; no Reserved Font Names are declared
by either source.
