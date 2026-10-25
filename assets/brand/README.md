# Discodex logo

The open D and detached lime dot represent Discodex and live presence.
The logo uses forest charcoal `#20291F`, warm ivory `#F4F3E9`, and live lime `#C5F277`.

`discodex-icon.svg` is the editable master. It has a charcoal plate and a transparent exterior.
`discodex-mark.svg` removes the plate for use on light backgrounds.
`discodex-mono-dark.svg` and `discodex-mono-light.svg` are single-color versions.
PNG exports range from 16 to 1024 pixels. `preview.png` shows the identity and small sizes.

Keep the dot separate from the D. Preserve the proportions and clear space in the SVG.
Use the app icon at small sizes. Do not add shadows, gradients, or outlines.

To rebuild with Node.js, run these commands from the repository root:

```powershell
npm install --prefix tools/brand --no-save --package-lock=false sharp@0.35.5
node tools/brand/build.cjs
```

The script updates `assets/discodex.ico`, the website icons, and `site/assets/og-card.png`.
It also rebuilds the PNG sizes and SVG variants in this directory.
Rebuild the Windows executable with `cargo build --release` to embed the new icon.

The original concept was generated with the built-in imagegen tool, then rebuilt as flat SVG geometry.
`concept-prompt.txt` preserves the generation prompt. The SVG master is the source for every export.
