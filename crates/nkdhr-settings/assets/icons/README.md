# Settings navigation icons

These source SVGs are selected from Lucide at commit
`113a3b1a3bda9a31d30f4b056cd434ce9462828e`. They implement the rounded
monoline icon direction accepted for nkdhr's UI and match the Settings
composition reference.

Runtime masks are generated at 96×96 so the renderer can tint and sample one
single-channel texture at any approved optical size:

```sh
for icon in accessibility bell gamepad-2 image mouse-pointer-2 palette panel-top panels-top-left plug; do
  magick "crates/nkdhr-settings/assets/icons/${icon}.svg" \
    -background white -resize 96x96 -colorspace Gray -negate -depth 8 \
    "gray:crates/nkdhr-settings/assets/icons/${icon}.alpha8"
done
```

The original and generated forms are redistributed under `LICENSE` in this
directory.
