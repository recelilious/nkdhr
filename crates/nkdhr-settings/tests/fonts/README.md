# Deterministic Settings text fixture

`NotoSansCJKsc.appearance.subset.otf` is a modified glyph subset of the
Fedora-packaged Noto Sans CJK SC variable font. It contains the Chinese text
used by the first accepted Settings composition so its visual oracle does not
depend on fonts installed by the test host.

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

Copyright remains with the original font authors. The subset is redistributed
under the SIL Open Font License 1.1 in
`crates/nkdhr-ui/tests/fonts/OFL-1.1.txt`; no Reserved Font Names are declared
by this source.
