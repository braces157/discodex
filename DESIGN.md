# Discodex GPUI Usage Dashboard Design System (`DESIGN.md`)

Design tokens, layout geometry, and edge-state contracts for the native Windows GPUI Usage Dashboard (`src/dashboard/`).

---

## 1. Color Palette (Neutral Zinc × shadcn/Apple Dark)

| Role | Token | Value | Usage |
| :--- | :--- | :--- | :--- |
| Canvas | `BG_CANVAS` | `#09090B` | Root window frame (Zinc-950, zero gradients) |
| Surface | `BG_SURFACE` | `#111113` | Flat matte cards (`1px` `#27272A` hairline borders) |
| Recessed | `BG_RECESSED` | `#0C0C0E` | Segmented controls, plot well, heatmap tray, drawer, footer |
| Elevated | `BG_ELEVATED` | `#1E1E22` | Active segmented tabs, badges, effort tags |
| Tooltip | `BG_TOOLTIP` | `#141417` | Minimal inline crosshair pill |
| Hover | `BG_HOVER` / `BG_ROW_HOVER` | `#FFFFFF0B` / `#FFFFFF07` | Interactive buttons, chips, expandable model rows |
| Borders | `BORDER_SUBTLE` / `BORDER_OUTER` / `BORDER_STRONG` | `#27272A` / `#2E2E33` / `#3F3F46` | Crisp 1px Zinc hairline framing |
| Grid & Track | `GRID_LINE` / `TRACK_BG` | `#1F1F23` / `#222226` | Dashed chart reference lines and progress tracks |
| Text | `TEXT_PRIMARY` / `TEXT_SECONDARY` / `TEXT_MUTED` | `#FAFAFA` / `#A1A1AA` / `#71717A` | High-clarity monochrome hierarchy |
| Tokens / Primary | `ACCENT_LIME` | `#E4E4E7` | Crisp monochrome silver primary curve, bars, 12-week calendar ramp |
| Cost & Cached | `ACCENT_COST` | `#60A5FA` | Soft slate-blue secondary series, cache-read segments, Peak Hours matrix |
| Output / Calls | `ACCENT_CALLS` | `#94A3B8` | Cool muted steel output & calls series |
| Positive Delta | `ACCENT_POS` | `#4ADE80` | Quiet sage strictly reserved for `+X%` deltas & above-baseline splits |
| Reasoning | `ACCENT_REASONING` | `#818CF8` | Soft desaturated indigo reasoning sub-metric |
| Warning / Write | `ACCENT_AMBER` | `#D4A359` | Muted sand amber for incomplete-day markers and partial notices |
| Negative / Error | `ACCENT_ERR` | `#F87171` | Quiet rose strictly reserved for `-X%` deltas, below-baseline splits, and errors |

---

## 2. Typography & Iconography

- **UI Sans:** `Segoe UI Variable` (`11px` dense micro-labels, `12px–14px` headers & body)
- **Tabular Monospace:** `Cascadia Mono` (all token counts, USD estimates, ratios, chart ticks, heatmap stats)
- **Icons:** `Segoe Fluent Icons` (`\u{E718}`/`\u{E840}` Pin, `\u{E72C}` Refresh, `\u{E8BB}` Close, `\u{E740}`/`\u{E73F}` Wide/Compact, `\u{E946}` Info, `\u{E70F}` Edit rates, `\u{E70D}`/`\u{E70E}` Chevron)

---

## 3. Window Modes & Layout Architecture

- **Compact Tray Popup (`548 × 756` logical px, clamped to monitor work area):**
  - Top header with brand mark, action buttons, and full-width Period segmented bar (`Today`, `7 days`, `30 days`, `All`).
  - Single-column scroll flow: `Providers` → `Hero Bento` → `Telemetry Strip` → `Interactive Chart` → `Activity Heatmap` → `Token Mix` → `Models` → `Notes/Info`.
- **Wide Analytics Cockpit (`940 × 868` logical px, clamped & right-edge-safe on resize):**
  - Single-row header with Period segmented bar centered inline.
  - Balanced 2-column grid below header:
    - **Left Column:** `Hero Bento` (`Tokens` + `Est. API Cost` with ambient corner glow & full-width luminous monotone sparkline ribbons) → `Telemetry Strip` (`Calls`, `Cache Hit`, `Cache Saved`, `Task Time`) → `Interactive Chart` (`Area` / `Bar` / `Dots`, multi-series `Providers`, `188px` plot well, Detent editorial hero readout, Liveline right-of-cursor dimming + right Y-axis live/hover badges, EvilCharts floating glassmorphic tooltip + hatched live buffer bar, click-to-pin baseline splits, drag-to-select range comparison, caliper delta brackets, and `easeOutQuart` wave/wipe reveals) → `Activity Heatmap` (recessed well, peak glow cells, `12-Week` / `Peak Hours`).
    - **Right Column:** `Providers` (beveled proportional share bar + tinted filter chips) → `Token Mix` (4× MSAA GPU Donut Ring with centered KPI + 5-way progress breakdown) → `Models` (Detent proportional inline background bars + 9-metric breakdown drawer) → `Notice/Info`.

---

## 4. Six Edge-State Contracts

1. **Empty:** Period/filter combinations with zero activity show a guided empty state with one-click period jumps (`7d`, `30d`, `All`), never `NaN` or blank boxes.
2. **Loading:** Initial cold scan renders geometry-matched KPI cards and a surface placeholder while background preloader/worker completes.
3. **Overflow & Density:** Model names truncate (`min_w_0().truncate()`), provider chips wrap cleanly, window clamps to monitor `rcWork`, and scroll areas prevent horizontal clipping.
4. **Partial / Unpriced / Error:** Unknown prices remain `None` (`Unpriced`), partial sums append `*`, incomplete days show amber caps, and worker errors surface an inline `Retry` banner without hiding cached snapshots.
5. **Keyboard & Focus:** `Esc` (clear range/baseline, or close), `←` / `→` (scrub chart cursor), `1`–`4` (periods), `r` (refresh), `p` (pin), `w` (Compact ↔ Wide), `c` (cycle `Area` → `Bar` → `Dots`).
6. **Contrast:** Primary (`#F3F4F6`) and secondary (`#9CA3AF`) inks stay `>= 4.5:1` over `#14171C` and `#0D0F12`.
