# Provider artwork

Discodex ships one mark per harness. Discord renders `assets[large_image]` from
a URL, so the 30 marks are served from this repository instead of being
hotlinked from vendor sites and third-party icon packages.

## Why the set is normalised

Thirty brand assets arrive in thirty different states: a transparent knot, a
white glyph on nothing, a purple app tile, a 2700x1392 banner, a black-on-black
avatar. Dropped into a Discord profile unchanged, they read as 30 unrelated
images and half of them disappear on a dark background.

Every mark is therefore rebuilt to one geometry:

- **512x512**, RGBA, transparent outside the plate.
- A **rounded-square plate** at 86% of the canvas, corner radius 23.5% of the
  plate, in `#16171B`.
- The artwork is **optically centred** inside the plate at 60% of its edge.
- Marks that are already a brand tile (Kiro, Aider, Gemini CLI, Factory Droid,
  Kilo Code, Amp, Codex) keep their own plate; it is cropped to the same
  footprint and corner radius so the two kinds sit in one grid.
- A small **optical correction** shrinks dense marks and grows thin ones, so a
  solid disc and an outline carry similar weight at 32px.

## Marks

| Provider | File | Kind |
| --- | --- | --- |
| OpenAI Codex | `assets/logos/codex.png` | tile |
| Claude Code | `assets/logos/claude-code.png` | glyph |
| DeepSeek Harness | `assets/logos/deepseek-harness.png` | glyph |
| ChatGPT | `assets/logos/chatgpt.png` | glyph |
| Claude | `assets/logos/claude.png` | glyph |
| Google Gemini | `assets/logos/gemini.png` | glyph |
| Cursor | `assets/logos/cursor.png` | glyph |
| Windsurf | `assets/logos/windsurf.png` | glyph |
| Google Antigravity | `assets/logos/antigravity.png` | glyph |
| Trae | `assets/logos/trae.png` | glyph |
| Zed AI | `assets/logos/zed.png` | glyph |
| OpenCode | `assets/logos/opencode.png` | glyph |
| GitHub Copilot | `assets/logos/github-copilot.png` | glyph |
| GitHub Copilot CLI | `assets/logos/copilot-cli.png` | glyph |
| Cline | `assets/logos/cline.png` | glyph |
| Roo Code | `assets/logos/roo-code.png` | glyph |
| Continue | `assets/logos/continue.png` | glyph |
| Perplexity | `assets/logos/perplexity.png` | glyph |
| Microsoft Copilot | `assets/logos/microsoft-copilot.png` | glyph |
| Poe | `assets/logos/poe.png` | glyph |
| Qwen Code | `assets/logos/qwen-code.png` | glyph |
| Block Goose | `assets/logos/goose.png` | glyph |
| Charm Crush | `assets/logos/crush.png` | glyph |
| Junie | `assets/logos/junie.png` | glyph |
| Kiro | `assets/logos/kiro.png` | tile |
| Aider | `assets/logos/aider.png` | tile |
| Gemini CLI | `assets/logos/gemini-cli.png` | tile |
| Factory Droid | `assets/logos/factory-droid.png` | tile |
| Kilo Code | `assets/logos/kilo-code.png` | tile |
| Amp | `assets/logos/amp.png` | tile |

## Rebuilding

The pipeline is two steps, and only the first touches the network:

```powershell
python tools/logos/vendor.py     # fetch tools/logos/sources/
python tools/logos/build.py      # write assets/logos/ + manifest.json
```

`vendor.py` rasterises SVG sources at 1024px with a headless Chromium/Edge
instance. Set `DISCODEX_CHROME` if the browser is not in a standard location.

`build.py` needs Pillow. It is offline and deterministic: the same vendored
sources always produce the same bytes, so a rebuild never shows up as a
spurious diff.

`build.py --sheet` also writes `assets/_preview/logos-contact-sheet.png`, which
shows every mark on a dark and a light card at 110px and 40px.

The landing page uses a 128px rendition in `site/assets/logos/` so the page does
not pull 512px files for 18px images.

## Changing a mark

1. Edit the entry in `tools/logos/sources.json` (`source`, and `mode` if the
   artwork changes from a transparent glyph to a filled tile).
2. Run `python tools/logos/vendor.py --force`.
3. Run `python tools/logos/build.py --sheet` and look at the contact sheet.
4. If the slug changed, update `Provider::logo_slug` and the matching
   `LOGO_*` constant in `src/provider.rs`.

`src/provider.rs` tests assert that every provider has a unique kebab-case slug
and a URL ending in `/assets/logos/<slug>.png`, so a mismatch fails CI.

## Attribution

The marks are the trademarks of their respective owners and are used here only
to identify the tool a session belongs to. `tools/logos/sources.json` records
the exact source URL for each one, and `assets/logos/manifest.json` records the
source digest the current PNGs were built from.
