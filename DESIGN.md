# Discodex GPUI Usage Dashboard Design System (`DESIGN.md`)

Design tokens, layout geometry, and edge-state contracts for the native Windows GPUI Usage Dashboard (`src/dashboard/`).

---

## 1. Color Palette (Obsidian + Lime/Emerald)

| Role | Token | Value | Usage |
| :--- | :--- | :--- | :--- |
| Canvas | `BG_CANVAS` | `#0D0F12` | Root window frame |
| Surface | `BG_SURFACE` | `#14171C` | KPI bento, telemetry strip, chart, heatmap, token mix, model list |
| Recessed | `BG_RECESSED` | `#0A0C0F` | Segmented controls, pill groups, model detail drawer, footer |
| Elevated | `BG_ELEVATED` | `#1E232B` | Active segmented tabs, badges, effort tags, empty icon container |
| Hover | `BG_HOVER` / `BG_ROW_HOVER` | `#FFFFFF0D` / `#FFFFFF08` | Interactive buttons, chips, expandable model rows |
| Borders | `BORDER_SUBTLE` / `BORDER_OUTER` / `BORDER_STRONG` | `#FFFFFF12` / `#FFFFFF1F` / `#FFFFFF24` | Hairline 1px surface framing and dividers |
| Grid & Track | `GRID_LINE` / `TRACK_BG` | `#FFFFFF0A` / `#FFFFFF0C` | Chart reference lines and progress tracks |
| Text | `TEXT_PRIMARY` / `TEXT_SECONDARY` / `TEXT_MUTED` | `#F3F4F6` / `#9CA3AF` / `#6B7280` | `>= 4.5:1` primary/secondary readability on dark surfaces |
| Tokens / Primary | `ACCENT_LIME` | `#C5F277` | Active status dot, token bars, sparklines, positive deltas, 12-week heatmap peak |
| Cost & Cached | `ACCENT_COST` | `#6EE7B7` | API cost KPI, cache-read bar segments, `Cache Saved`, Peak Hours heatmap |
| Output / Calls | `ACCENT_CALLS` | `#7DD3FC` | Output token segments, calls series, negative/cool delta badges |
| Reasoning | `ACCENT_REASONING` | `#A78BFA` | Reasoning token sub-metric and legend |
| Warning / Write | `ACCENT_AMBER` | `#F5B971` | Cache write segments, incomplete-day markers, partial data notices |
| Error | `ACCENT_ERR` | `#F87171` | Scan/worker failure banners |

---

## 2. Typography & Iconography

- **UI Sans:** `Segoe UI Variable` (`11px` dense micro-labels, `12px–14px` headers & body)
- **Tabular Monospace:** `Cascadia Mono` (all token counts, USD estimates, ratios, chart ticks, heatmap stats)
- **Icons:** `Segoe Fluent Icons` (`\u{E718}`/`\u{E840}` Pin, `\u{E72C}` Refresh, `\u{E8BB}` Close, `\u{E740}`/`\u{E73F}` Wide/Compact, `\u{E946}` Info, `\u{E70F}` Edit rates, `\u{E70D}`/`\u{E70E}` Chevron)

---

## 3. Window Modes & Layout Architecture

- **Compact Tray Popup (`520 × 720` logical px, clamped to monitor work area):**
  - Top header with brand mark, action buttons, and full-width Period segmented bar (`Today`, `7 days`, `30 days`, `All`).
  - Single-column scroll flow: `Providers` → `Hero Bento` → `Telemetry Strip` → `Interactive Chart` → `Activity Heatmap` → `Token Mix` → `Models` → `Notes/Info`.
- **Wide Analytics Cockpit (`880 × 836` logical px, clamped & right-edge-safe on resize):**
  - Single-row header with Period segmented bar centered inline.
  - Balanced 2-column grid below header:
    - **Left Column:** `Hero Bento` (`Tokens` + `Est. API Cost` with GPUI `canvas` sparklines) → `Telemetry Strip` (`Calls`, `Cache Hit`, `Cache Saved`, `Task Time`) → `Interactive Chart` (`Stacked` / `Area`, `Mix` / `Providers`, `24h` on Today, hover crosshair + readout) → `Activity Heatmap` (`12-Week` streak calendar / `Peak Hours` 7×24 matrix).
    - **Right Column:** `Providers` (multi-color proportional share bar + clickable filter chips) → `Token Mix` (5-way token composition) → `Models` (sortable by `Tokens`, `Cost`, `Calls`, `Cache %`; click any row for inline 9-metric breakdown drawer) → `Notice/Info`.

---

## 4. Six Edge-State Contracts

1. **Empty:** Period/filter combinations with zero activity show a guided empty state with one-click period jumps (`7d`, `30d`, `All`), never `NaN` or blank boxes.
2. **Loading:** Initial cold scan renders geometry-matched KPI cards and a surface placeholder while background preloader/worker completes.
3. **Overflow & Density:** Model names truncate (`min_w_0().truncate()`), provider chips wrap cleanly, window clamps to monitor `rcWork`, and scroll areas prevent horizontal clipping.
4. **Partial / Unpriced / Error:** Unknown prices remain `None` (`Unpriced`), partial sums append `*`, incomplete days show amber caps, and worker errors surface an inline `Retry` banner without hiding cached snapshots.
5. **Keyboard & Focus:** `Esc` (close), `1`–`4` (periods), `r` (refresh), `p` (pin), `w` (Compact ↔ Wide), `c` (Bar ↔ Area).
6. **Contrast:** Primary (`#F3F4F6`) and secondary (`#9CA3AF`) inks stay `>= 4.5:1` over `#14171C` and `#0D0F12`.
