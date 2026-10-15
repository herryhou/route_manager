---
name: rm-web — 公車站牌 The Stop Plate
description: Transit-data QA/edit console styled as enamel bus-stop plates hanging on a printed line — enamel-blue topbar band, plate-white panels on mineral-grey ground, QA encoded by size and hatch before colour.
colors:
  enamel: "#0857A2"
  enamel-deep: "#06457F"
  enamel-soft: "#E8F0FA"
  plate: "#FAF8F4"
  plate-bright: "#FFFFFF"
  ground: "#E4E5E1"
  ink: "#16150F"
  ink-soft: "#5C5A50"
  hair: "#D9D6CC"
  alert: "#D9531E"
  alert-ink: "#B23F12"
  qa-ok: "#1E7A3C"
  qa-warn-text: "#8A5E00"
  qa-warn-dot: "#E8A013"
  qa-bad: "#C0331B"
  nodata: "#8A8880"
  endpoint-blue: "#7DA7D9"
typography:
  title:
    fontFamily: "Archivo, Noto Sans TC, PingFang TC, sans-serif"
    fontSize: "17px"
    fontWeight: 800
    letterSpacing: "0.01em"
  pane-title:
    fontFamily: "Archivo, Noto Sans TC, PingFang TC, sans-serif"
    fontSize: "14px"
    fontWeight: 700
  popup-title:
    fontFamily: "Archivo, Noto Sans TC, PingFang TC, sans-serif"
    fontSize: "19px"
    fontWeight: 700
  section-label:
    fontFamily: "Archivo, Noto Sans TC, PingFang TC, sans-serif"
    fontSize: "11px"
    fontWeight: 700
    letterSpacing: "0.05em"
  body:
    fontFamily: "Archivo, Noto Sans TC, PingFang TC, sans-serif"
    fontSize: "13px"
    fontWeight: 500
    lineHeight: "1.5"
  meta:
    fontFamily: "Archivo, Noto Sans TC, PingFang TC, sans-serif"
    fontSize: "11px"
    fontWeight: 400
    lineHeight: "1.5"
  route-code:
    fontFamily: "Archivo, Noto Sans TC, PingFang TC, sans-serif"
    fontSize: "13px"
    fontWeight: 800
rounded:
  plate: "10px"
  control: "8px"
  small: "6px"
  badge: "5px"
spacing:
  xs: "4px"
  sm: "8px"
  md: "10px"
  lg: "12px"
components:
  route-chip:
    backgroundColor: "{colors.plate-bright}"
    textColor: "{colors.enamel}"
    rounded: "{rounded.control}"
    padding: "1px 7px"
  band-button:
    backgroundColor: "{colors.plate}"
    textColor: "{colors.ink}"
    rounded: "{rounded.control}"
    padding: "7px 11px"
  topbar-chip:
    backgroundColor: "rgba(255, 255, 255, 0.13)"
    textColor: "rgba(255, 255, 255, 0.92)"
    rounded: "{rounded.small}"
    padding: "3px 9px"
  topbar-chip-hot:
    backgroundColor: "{colors.plate-bright}"
    textColor: "{colors.alert-ink}"
  tab-active:
    backgroundColor: "{colors.enamel}"
    textColor: "{colors.plate-bright}"
    rounded: "{rounded.control}"
    padding: "4px 11px"
  input-search:
    backgroundColor: "{colors.plate-bright}"
    textColor: "{colors.ink}"
    rounded: "{rounded.control}"
    padding: "7px 11px"
---

# Design System: rm-web — 公車站牌 The Stop Plate

## Overview

**Creative North Star: "The Stop Plate"** — every panel is an enamel bus-stop plate hanging on the line; the route chip and the ordered stop chain are the product's own atoms.

rm-web is an internal transit-data QA/edit console for a 1–5 person ops team, used on desktop mice for hours at a time, in zh-Hant. The visual world refuses the category default — a grey generic three-pane admin console with accent-coloured chrome. Instead: a 56px enamel-blue topbar band spans the top like the enamel plate header; below it, plate-white panels (10px radius) float with 8px gaps on a mineral-grey ground; ink hairlines do the ruling. Everything reads as printed hardware, not software chrome.

QA magnitude is never colour-alone: on the 站序鏈 stop chain, snap metres drive node diameter (8–13px) and hatch density before the green/amber/red hue is read. Orange is not a decorative accent — it is the alarm. Motion is honest static states: exactly one authored moment (selection isolation, 120ms), nothing animates on load.

**Key Characteristics:**
- Enamel-blue band topbar; plate-white panels on mineral-grey ground
- 8px gap rhythm; 10px plate radius, 8px control radius
- Ink hairlines (`#16150F` / `#D9D6CC`) instead of grey borders on structure
- QA encoded by size + hatch + colour, in that order
- One authored motion moment; static everywhere else
- Zero inline SVG in markup; icon slots filled from one authored 2px-stroke set
- zh-Hant chrome is a deliberate product decision, not an i18n gap

## Colors

A cool enamel-and-mineral palette: one saturated institutional blue, warm paper neutrals, and a strictly rationed alert orange. QA states own three hues whose *use* is magnitude-graded.

### Primary
- **Enamel Blue** (`#0857A2`): the world's identity — the topbar band, active tabs, route-chip borders and numerals, route path lines, bearing markers, selection/focus states. Also aliased as `--accent`.
- **Enamel Deep** (`#06457F`): pressed/hover shade for enamel surfaces.
- **Enamel Soft** (`#E8F0FA`): enamel tint for selected route rows and button hovers on plates.

### Secondary
- **Alert Orange** (`#D9531E`) / **Alert Ink** (`#B23F12`): reserved for alarms only — hot QA chips, the active 編輯 toggle, the 存線 save button, selected path-piece highlight, effective-line overlay, `pp-hot` stale text. Never decoration.
- **QA Green** (`#1E7A3C`): clean projection (snap p50 < 15m, matched ≥ 90%), good badges, PROJECTED method dots.
- **QA Amber Dot** (`#E8A013`): amber nodes, warn badges, station rings, hatch colour. Its text form is **QA Amber Text** (`#8A5E00`) — the dot value fails AA on plate-white, so it never renders as text.
- **QA Red** (`#C0331B`): outliers/NO_PATH (snap ≥ 40m, dev p95 ≥ 100m, matched < 50%), bad badges, red dashed no-path segments.

### Neutral
- **Plate White** (`#FAF8F4`): panel/plate ground. **Plate Bright** (`#FFFFFF`): lifted state ground (selected chain stop, route-chip fill, inputs, hot chips).
- **Mineral Grey** (`#E4E5E1`): page ground and statusbar; the world between plates.
- **Ink** (`#16150F`): primary text, plate-rim rules, the chain's printed connector line, raw-compare dashed line.
- **Ink Soft** (`#5C5A50`): secondary text, metres, sequence numerals, meta, icons at rest.
- **Hairline** (`#D9D6CC`): 1px field rules, input borders, scrollbar thumbs, kv-table dividers.
- **No-data Grey** (`#8A8880`): missing measurements — nodata badges, NO_PATH method dots, `snapColor(null)`.
- **Endpoint Blue** (`#7DA7D9`): ENDPOINT projection method dot (the one soft tint in the QA set).

### Named Rules
**The One Shout Rule.** Alert orange appears only where the system is warning you or where an edit session is live (`.hot` chips, `#edit-toggle.on`, `.le-save`, piece highlight). If a surface isn't an alarm or an intervention, it doesn't get orange — even as an accent.

**The Size Before Colour Rule.** QA magnitude is encoded by node diameter and hatch density first; hue only reinforces. Any new QA encoding must survive a greyscale read.

**The Amber Split Rule.** `#E8A013` fills dots, badges, rings and hatches; `#8A5E00` is the only amber allowed on text. Never render the dot value as text colour.

**The No-Default-Surface Rule.** Browser-provided surfaces come from the palette: `::selection` is enamel, carets are enamel, checkbox/select `accent-color` is enamel, focus rings are 2px enamel outlines, scrollbars are hairline-thin. Zero platform defaults survive.

## Typography

**Display/UI Font:** Archivo (500–800) — Latin and all numerals
**Body Font:** Noto Sans TC (400/500/700) — zh-Hant text
**Stack:** `"Archivo", "Noto Sans TC", "PingFang TC", sans-serif` (Google Fonts link in `index.html`; PingFang TC is the offline fallback)

**Character:** Archivo's condensed-grotesque numerals give the console its transit-signage voice — route codes and counters are set Archivo 800 with `font-variant-numeric: tabular-nums` so digit columns don't jitter. Noto Sans TC carries the Chinese at matched weights, and the PingFang fallback keeps zh-Hant legible offline. Every numeric span (`<b>` in chips, `.chain-seq`, `.chain-metre`, `.badge`, `.log-meta`, `.pp-code`) is tabular.

### Hierarchy
- **Wordmark** (800, 17px, +0.01em): the `rm-web` title on the enamel band, beside the brand glyph.
- **Popup title** (700, 19px): stop/station name in popups — the largest body-face size, plate-signage scale.
- **Pane title** (700, 14px): `dpane-title` in each plate rim.
- **Section label** (700, 11px, +0.05em): `h3`/`.list-head` — 經過路線, 圖層, 站點 — small caps-like quiet headers.
- **Body** (500, 13px, 1.5): the global base set on `body`; list rows, kv values, chain names.
- **Button/label** (700, 12.5px/1): band buttons and `.le-btn`.
- **Meta** (400, 11px): timestamps, editors, notes, `pp-code`, tile note.
- **Route code** (800, tabular): the chip atom — see Components.

### Named Rules
**The Tabular Counters Rule.** Any number the UI updates — chip counts, sequence numbers, metres, badges, times — is `tabular-nums`. Route chips and the wordmark additionally take Archivo 800.

## Layout

A fixed three-pane console: 56px enamel topbar → 8px-gapped main row → 28px statusbar. The main row is sidebar (280px, fixed) / Leaflet map (flex 1) / detail column (360px, fixed, hidden until a selection). The detail column stacks two plates at a 2:1 flex ratio — context pane (route/QA lists) over point pane (stop/station/piece drill-down), each with its own rim header (title + back/forward/close nav) and independently scrolling body.

Spacing rhythm is an 8px base: 8px gaps between plates and around the main row; internal padding steps at 10–12px (search margin 10px, pane bodies and layer controls 10px 12px, list rows 6px 8–12px); micro-steps at 2–6px inside chips and rims. The statusbar is ground-coloured, 12px ink-soft text.

Responsive: the tool targets desktop mouse interaction. A single `max-width: 900px` breakpoint stacks the plates vertically so every region stays reachable — the topbar wraps (`height: auto; min-height: 56px`), the sidebar caps at 42vh, the map gets at least 48vh, and the detail column goes full-width. No other breakpoints; no touch affordances.

Architecture note for extenders: assets are `include_str!`-embedded in a single axum binary with `Cache-Control: no-cache`. A new JS file must be registered in **both** `crates/rm-web/src/handlers/assets.rs` (a `/js/x.js` match arm) and `crates/rm-web/src/handlers/mod.rs` (a route). Zero-build vanilla ES modules; Leaflet is vendored; `index.html` gets the tile template injected as `window.RM_TILES`.

## Elevation & Depth

Flat-by-default with a single reserved lift. Plates sit on the mineral ground by colour contrast and hairlines, not shadows — no resting shadow exists anywhere in the system. One shadow token is minted, `--lift: 0 2px 10px rgba(22,21,15,0.16)`, and it is spent only on genuine state elevation: the selected chain stop (`.chain-stop.sel`) and, at close kin values, popups (`0 4px 16px rgba(22,21,15,0.18)`) and the floating line-edit buttons (`0 2px 8px`), which hover above the map rather than the page. Map depth is handled by dimming the basemap (tile pane at `opacity: 0.75`) so data layers read forward.

### Shadow Vocabulary
- **Selection lift** (`0 2px 10px rgba(22,21,15,0.16)` — `--lift`): the selected stop plate, and nothing else at rest-state transitions.
- **Popup shadow** (`0 4px 16px rgba(22,21,15,0.18)`): Leaflet popups, which float over the map.
- **Floating button shadow** (`0 2px 8px rgba(22,21,15,0.2)`): line-edit overlay buttons on the map.

### Named Rules
**The One Lift Rule.** `--lift` marks *the* selected thing. New components don't get shadows for hover or decoration — a hover is a background tint, never a lift.

## Shapes

Plates are rounded rectangles at 10px (sidebar, map container, detail panes, popups). Controls sit at 8px (search, tabs, band buttons, route chips, list-row hover rounding, bearing wrapper); sub-controls at 6px (topbar chips, mini-buttons, pane-nav buttons, geometry-source select); badges at 5px.

Borders are structural speech. The route chip carries the world's heaviest stroke — 2px enamel on white — because it is the product's atom. Structural rims are 1.5px ink (pane-head rules, popup borders, hot-chip inset rings, search focus border); field rules and input borders are 1.5px/1px hairline. Hot QA chips invert the rim: white ground with an inset 2px alert ring (`box-shadow: inset 0 0 0 2px var(--alert)`). The chain's geometry is a grid — `26px 22px 1fr auto` (seq / node column / name / metres) — with a 2px ink connector line drawn through the node column via `::before`, trimmed to half-height at first/last stops like a printed route diagram.

## Components

### Topbar band chips
- **Style:** translucent white plates on enamel (`rgba(255,255,255,0.13)`, 6px radius, 12px text); each is `label <b>count</b>`.
- **Passive vs active:** chips without `.chip-click` lose the plate (transparent, no padding) — only clickable QA chips read as controls.
- **Hot state:** `.hot` (non-zero issue count) turns the chip into a white plate with the inset 2px alert ring and alert-ink text — the one shout on the band.
- **JS contract:** `setChip(id, text)` in app.js requires the element to contain a `<b>` child, and toggles `.hot` only on `.chip-click` elements. New band chips must ship `.chip-click` + `<b>` or the wiring silently no-ops. `<b>—</b>` is the load-state glyph (no data yet), not prose.

### Band buttons (編輯 / 更新)
- **Style:** little plate-white buttons (8px radius, 700 12.5px, 7px 11px padding) so they read as buttons against enamel; they deliberately keep `chip chip-click` classes for the `setChip` contract.
- **States:** hover → enamel-soft; disabled → 0.55 opacity; `#edit-toggle.on` → alert orange (the edit session is an intervention); `.hot` → same white-plus-alert-ring as QA chips.

### Route chip (the atom)
- **Style:** white ground, 2px enamel border, 8px radius, Archivo 800 tabular numerals in enamel, `1px 7px` padding, min-width 36px, centred.
- **Variants:** sidebar list cells (`.route-code`, non-interactive), clickable panel/popup chips (`.chip-route`: hover fills enamel with white text), outlier/matchdev row leads.
- **Never restyle ad hoc** — it appears in the sidebar, popups, detail panes, and QA lists; one definition serves all.

### 站序鏈 stop chain (signature component)
The route context pane's ordered stop list — each row a plate on the printed line. Grid columns: sequence numeral (ink-soft, 600, tabular, right-aligned) / QA node / stop name (ellipsis) / metres.
- **The printed line:** a 2px ink rule at 0.85 opacity runs through the node column, skipping half-height at the first and last stops.
- **The QA node:** a circle with a 1.5px ink ring on a plate-ground square cell. `qaDot(s)` in colors.js returns the state class plus inline custom properties: `--nd` (node diameter, **8–13px**, scaling `min(snap,120)/120`) and `--nh` (hatch period). Green `q-ok` is solid; amber `q-warn` hatches at period `3.5 − snap/40` px (min 1.6); red `q-bad` hatches at a tight fixed 2px. Hatches are 45° `repeating-linear-gradient`s in translucent white over the dot colour. Unknown snap (null, not NO_PATH) takes the amber-magnitude default so an unknown reads "not clean".
- **Metres discipline:** metres text renders **only off-green** (`.chain-metre.q-warn` in amber text, `.q-bad` in red, 700, 11.5px, tabular). Clean stops show nothing — the chain stays scannable.
- **Selection isolation (the single authored motion):** clicking a row adds `.sel` (plate-bright ground + `--lift` + name weight 700) and `.has-sel` on the chain (all siblings recede to `opacity: 0.66`). Transition is 120ms ease-out on background-color, box-shadow, opacity. Nothing else in the product animates state at this level.

### Tabs / segmented direction toggle
Pill buttons (8px radius, 1.5px hairline border, white ground, 500 12px); hover rims and tints enamel; active fills enamel with white 700 text. Two uses: 去程/返程 direction toggle and 站序/QA 剖面 view switch (`.view-tabs`, JS-toggled panes).

### Detail panes (plates with rims)
`.dpane` plates, 10px radius, context 2/3 + point 1/3. The rim (`.dpane-head`) is the plate's header: 8px 12px padding, **1.5px ink bottom rule** — heavier than a hairline, it reads as stamped. Pane nav (back/fwd/close) are bare ink-soft icon buttons; disabled history nav dims in place (`opacity: 0.35`) rather than hiding, so nav width stays stable.

### Lists (routes, stations, QA rows, logs)
Hover is a 5% ink wash on an 8px-radius row — no borders, no zebra. Selected route row: enamel-soft ground + inset 1.5px enamel ring. Direction QA dots: 9px circles of `snapColor(p50)` with a 1.5px plate-white halo. Edit-log rows are display-only (`.log-row` cursor default) with action badges (update/move_station → warn amber, undo → good green) and a single ↩ mini-button on the newest undoable row (the server 409s the rest).

### Badges
11px 700 white-on-colour pills, 5px radius, 0 5px padding: `.good` green, `.warn` amber-dot with **ink** text (white fails on `#E8A013`), `.bad` red, `.nodata` grey.

### Inputs
Search and selects: plate-bright ground, 1.5px hairline border, 8px radius (select 6px), enamel caret and accent colour; focus moves the border to enamel and kills the outline. Inline edit buttons (`.mini-btn`) are 1.5px hairline plates that rim enamel on hover; line-edit session buttons (`.le-btn`) are floating plate-buttons with the soft shadow, save (`le-save`) in alert orange.

### Popups & the bearing dial
Popups are plates: plate ground, 1.5px ink border, 10px radius, ink rules, Leaflet's chrome re-skinned (bar buttons, tip). The header row (`.pp-head`) pairs a 36px **bearing dial** — rounded-square ink/enamel frame, solid arrow rotated to the bearing, optional spread wedge tinted amber-dot past the 45° verify threshold — with a 19px 700 name and 11px code. 經過路線 chip blocks cap at 132px with a thin scrolling area so big interchanges stay on screen.

### Map glyphs
Bearing markers are 24px dot+arrow groups (enamel dot with white halo, solid enamel arrow), rotated around the stop point; with a click handler the glyph itself is the target (`bearing-click`). Viewport stops are 2.5px ink-soft dots; stations are 7px amber rings. Route paths are 4px enamel; `no_path` segments are 3px red dashed `6 6`; raw compare draws ink-dashed source, grey squares, and the effective line in 4px orange. All encodings are named in the legend, a plate floating over the map's bottom-left corner, which app.js fills statically from world constants — new line encodings must add a legend row.

### Icons
Zero inline SVG in `index.html`; static chrome marks slots with `data-icon="name"` and boot() fills them from `js/icons.js` (`I.back/fwd/close/edit/refresh/undo/warn/check/panelL/panelR/brand`). One set: 24px grid, 2px round strokes, `currentColor`, rendered at 14px (`.ic`; brand at 22px). The brand mark is the stop plate itself — a rounded rectangle with two stop dots. JS-generated HTML imports `I` directly (`I.undo`, `I.edit`, `I.warn` inline in panel markup).

## Do's and Don'ts

### Do:
- **Do** keep QA magnitude tri-encoded — node diameter (`--nd` 8–13px), hatch period (`--nh`), then hue — for any new QA surface; reuse `qaDot()`/`snapColor()`/`devColor()`/`matchedBadge()` from `colors.js` rather than re-deriving thresholds.
- **Do** reuse the route chip (white, 2px enamel, 8px radius, Archivo 800 tabular) wherever a route code appears.
- **Do** source every icon from `js/icons.js` via a `[data-icon]` slot or the `I` import; keep the 2px-stroke/`currentColor` grammar for new glyphs.
- **Do** write zh-Hant chrome (站牌, 去程/返程, 存線, 經過路線…) — it is the product's deliberate voice.
- **Do** keep browser surfaces in-world: enamel selection/caret/accent, 2px enamel focus-visible outlines, thin hairline scrollbars.
- **Do** register any new JS file in both `handlers/assets.rs` and `handlers/mod.rs`; assets stay zero-build `include_str!` ES modules with no-cache headers.
- **Do** honour `prefers-reduced-motion`: strip state transitions; the busy spinner stays but slows to 1.5s (it is functional feedback).
- **Do** keep data-changing actions explicit and user-triggered (the Apply-updates button model) — never implicit auto-refresh of edited data.
- **Do** propagate a proven interaction pattern to its sibling elements (highlight, arm/confirm) — map interactions stay consistent across stop/station/segment.
- **Do** keep programmatic viewport moves inside the whitelist (CLAUDE.md "no programmatic zoom or pan"): boot `fitBounds`, the four pan flows, and the route-panel 全覽 button (`fitRoutePath`) — nothing else.

### Don't:
- **Don't** use alert orange (`#D9531E`) decoratively — it is alarms and live edit sessions only (The One Shout Rule).
- **Don't** render `#E8A013` as text colour — amber text is `#8A5E00` (The Amber Split Rule).
- **Don't** animate on load, pulse, or add transitions beyond state tints; selection isolation at 120ms ease-out is the world's one authored motion.
- **Don't** use unicode glyphs as icons (✎, ⟳, ←, ✓) — the craft floor bans them; the icon set exists for this.
- **Don't** give components resting shadows or hover lifts; depth is colour + hairline, and `--lift` marks selection (and map-floaters) only.
- **Don't** introduce a build step, framework, or un-embedded asset — the single-binary `include_str!` architecture is a product constraint, not a preference.
- **Don't** do geo math client-side; the server owns projection/snap/polyline logic (lineEdit.js collects `[lat,lon]` only).
- **Don't** put metres on clean (green) chain stops — silence is the clean signal.
- **Don't** make non-actionable information look actionable (or the reverse) — affordance must be truthful.
