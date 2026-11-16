# Zedis UI Design Standards (gpui-kit)

Normative for `src/views/`, `src/components/` and `crates/zedis-ui/`. Distilled from the vendored gpui-kit Design Guides (`.claude/skills/gpui-kit-design-guides/`) and the Native Shell Design Guides (<https://nsh.dev/docs/design-guides.md>), adapted to Zedis: one desktop idiom shared by the desktop app and the web build (pointer + keyboard, compact controls, persistent navigation). Mobile, TUI, sound, springs and shaders from NSH do not apply.

`AGENTS.md` holds the must-follow summary; this file holds the tables, recipes and the finishing checklist. When they disagree, fix this file and `AGENTS.md` together.

## 1. Foundations: the UI layer

Zedis has three layers, and features only compose the top two:

| Layer | Owns | Examples |
| --- | --- | --- |
| `gpui_kit::component` | Behavior, focus, keyboard, base look | `Button`, `ButtonGroup`, `Input`, `Editor`, `Select`, `PopupMenu`, `DropdownMenu`, `Popover`, `Tooltip`, `Tab`, `Badge`, `Tag`, `Kbd`, `Skeleton`, `Spinner`, `Empty`, `Alert`, `Sheet` |
| `crates/zedis-ui` | Target-agnostic styled widgets | `ZedisCard`, `ZedisDialog`, `ZedisForm`, `ZedisTextTable`, `ZedisDivider`, `ZedisSelect`, `ZedisLoading` |
| `src/components/` | Redis-domain widgets | `KeyTypeBadge`, `kv_delegate`, `ZedisKvTable` |

- **Limited API.** A widget exposes what varies by meaning: `variant`, `Sizable` size, states (`selected`, `disabled`, `loading`), slots (label, icon, children) and events. Its colors, radii, paddings, hover and focus live inside it. Callers pass layout only (`w`, `flex_1`, alignment, margins).
- **A missing look becomes a variant first.** When a feature needs a look the layer lacks, add a variant, size or helper there, then use it. A missing color role becomes a theme-derived function in `src/helpers/color.rs` (like `card_background`), never a literal at the call site.
- **Extract on the second use.** Panel header (back button, title, trailing actions, one hairline), filter bar, stat chip, `Text | Tree` switch, status line and empty state are shared patterns: the second view that needs one extracts it.
- **No hand-built controls.** Never build a button, toggle, tab or menu from `div().on_click(...)`. The one exception is a virtualized row container inside `uniform_list` / `List`, which still owes hover, selected, keyboard and a domain `ElementId`.

## 2. Hierarchy and emphasis

- Few levels: panel title → section title → body → muted metadata → control labels.
- Build hierarchy with size, weight, spacing and hairlines before color or containers. Never nest a card inside a card; most regions need only a background, one hairline and spacing.
- **Emphasis is a budget.** One focal point per surface. If everything is colored, badged, bold or boxed, nothing reads as important. Review with accent color mentally removed: the task, the current selection and the next action must still be obvious.
- Badges are for short states, counts and classifications; keep most neutral (`KeyTypeBadge::plain`). Semantic colors only for their meaning.
- `Alert` is for exceptional information that needs attention now, not a decorated container for tips; prefer muted inline text.

## 3. Tokens

### Color roles (`cx.theme()`)

| Role | Tokens |
| --- | --- |
| Main surface and text | `background`, `foreground` |
| Named surfaces | `popover` / `popover_foreground`, `sidebar`, `title_bar`, `group_box`, `card_background(cx)` |
| Supporting text and fills | `muted`, `muted_foreground` |
| Principal commit / selection emphasis | `primary`, `primary_foreground` |
| Quiet emphasis | `secondary`, `accent` (+ `_foreground`) |
| Hover wash | `list_hover`, `table_hover` |
| Selected row / item | `list_active`, `table_active`, `tab_active` |
| Zebra | `table_even` |
| Meaning only | `danger`, `warning`, `success`, `info` |
| Data series | `chart_1` … `chart_5` |
| Structure and interaction | `border`, `input`, `ring`, `selection` |

No raw hex, `rgb(...)` or `hsla(...)` in views. Data palettes whose color *is* the data (key-type colors, tag colors, chart series beyond the theme, hex-dump address color) live in one helper each and must be checked in light and dark. Never assume foreground is black or background is white.

### Radius

- `theme().radius` for controls and popups, `theme().radius_lg` for containers that hold controls; small parts (tags, checkboxes) take a tighter step.
- Nested shapes are concentric: inner ≈ outer − gap. `rounded_full` only for intentional circles and pills; a tooltip or small button never becomes a capsule by accident.
- A radius governs the whole surface: child backgrounds, selection fills and scrolled content stay inside it (give children their own radius or clip with `overflow_hidden`, without clipping focus rings).
- A segmented `ButtonGroup` is one silhouette: only the outer corners are rounded, one divider per boundary.

### Typography

- At most three or four sizes and two weights per screen, from `text_xs` … `text_xl`; nothing below `text_xs` for reading.
- UI font for labels and prose. `get_mono_font_family()` only for Redis keys, values, commands, scripts, metrics, ids and shortcuts. Bold needs a concrete family (see GPUI Gotchas).
- Secondary text uses `muted_foreground`, not a smaller size alone. Comparable numbers are end-aligned in the mono font so digits line up.
- Long text truncates or wraps on purpose, with a recovery path (tooltip, copy button, open in editor).

### Spacing and the rem scale

GPUI Component makes the theme's base font size the window's `rem`, so `gap_*`, `p_*`, `h_*`, `w_*`, `size_*` and `text_*` follow Settings' UI font size. Choose a gap by what the two things mean to each other:

| Relationship | Helper | Default | Examples |
| --- | --- | --- | --- |
| Optical correction | `gap_0p5` | 2 | icon baseline nudge |
| Parts of one control | `gap_1` | 4 | icon + label in a menu row, title + description |
| Closely related controls | `gap_2` | 8 | toolbar buttons, dialog actions |
| One content group | `gap_3` | 12 | compact form rows, panel content spine (`px_3`) |
| Groups in one section | `gap_4` | 16 | panel padding, form groups |
| Separate sections | `gap_6` | 24 | major blocks of an inspector |
| Region boundary | `gap_8` | 32 | empty-state breathing room |

- Inside before outside: a component's padding is the component's; the gap between components is the parent's. No doubled insets.
- Equal gaps mean equal relationships; a title is closer to its description than to the next section.
- **No `px(...)` for spacing, type or ordinary control geometry.** Audited exceptions only: 1px hairlines, `uniform_list` fixed row heights, persisted or minimum pane widths, bitmap / chart / map geometry, window chrome insets. A literal that is an exception gets a short comment saying why.

## 4. Layout, proportion and alignment

- Stable shell: sidebar → key tree → content → status bar. Panels open in the content area; they do not re-arrange the shell.
- Master–detail starts near one-third / two-thirds with the detail taking the surplus. Every major region has a minimum, a comfortable default and a max; user-resized splits persist and are clamped to the current window (the kv-table entry panel is the reference).
- Elevation explains stacking, not importance: only floating layers (popover, menu, dialog, notification) get shadows. Base regions are separated by background and hairlines. Do not put a shadow on every card.
- Alignment spines: sibling bands (panel header, filter bar, list/table, footer, empty state) share one leading and trailing inset (`px_3`). Nested levels return exactly to the parent spine.
- Repeated rows keep fixed lanes for icon/badge, identity, metadata, numbers and trailing actions; a missing badge never moves the label (center icons in a fixed slot). Mixed text sizes in a row align by baseline; icons and text by center line.
- `justify_between` only when both sides truly own opposite edges.
- A hairline is drawn once, by the boundary owner. A scrollbar belongs to the region that scrolls, at its trailing edge: pad the content, not the scroll view.

## 5. Components and interaction states

### Component standards

- **One size per band.** Every `Button`, `ButtonGroup`, `Input`, `Select` and `DropdownMenu` trigger in one toolbar, header or filter row shares one `Sizable` step (usually `.small()`), so heights, text, icons and center lines agree.
- **Icon buttons** are square at the band's size, one size per group, each with `.tooltip(...)` and a domain `ElementId`.
- **One popup surface.** Use gpui-kit's `Popover`, `PopupMenu`, `DropdownMenu`, `Select` and `Tooltip`; never approximate one with an absolutely positioned `div` and hand-picked border/shadow.
- **Inputs and their lists share padding**, so text in a field and in its dropdown starts on one line.

### Action hierarchy

- At most one `.primary()` per decision surface: the commit that `Enter` submits. Toolbar and panel commands (`Search`, `Refresh`, `Add`, `Export`, `Open`, `Run`) are `.outline()` or `.ghost()`.
- Destructive commits use `.danger()` / `views/danger_confirm.rs`, labeled with the verb.
- `Button` for every in-app action and route switch; `Link` only for external URLs and `mailto:`.

### States every control is designed for

| State | Treatment |
| --- | --- |
| Rest | Clear affordance without noise |
| Hover | Quiet wash (`list_hover` / `table_hover` / ghost hover), weaker than selected; no size, weight, padding or layout change |
| Pressed | Visible within the click, stronger than hover |
| Open | A trigger stays visibly selected while its menu or popover is open |
| Focus-visible | Ring outside the border, never clipped by a tight `overflow_hidden` |
| Selected | Surface fill (`list_active`, `.selected(true)`, `tab_active`), never a leading `border_l` stripe. The key tree's tag-color edge is data, not selection |
| Disabled | No hover or press; a tooltip with the reason when it is not obvious |
| Loading | Keeps the control's size; `Button::loading(true)` needs an `.icon(...)` |
| Error | What happened and how to recover, next to the control |

### Pointer and command surfaces

- Default arrow cursor for buttons, rows, tabs and menu items; the pointing hand only for links and link-like content.
- Frequent actions stay visible. Region overflow goes in a `DropdownMenu`; object actions in a context `PopupMenu`, also reachable from a toolbar, menu or keyboard. Hover-revealed icons are only shortcuts to commands available elsewhere.

## 6. Overlays and feedback

- Pick the smallest surface: tooltip → popover → menu → notification → dialog → danger confirm → sheet.
- A tooltip supplies missing meaning (icon-only control, hidden shortcut, ambiguous scope); it does not repeat a visible label.
- Do not stack overlays (dialog chaining follows the `ZedisDialog` gotcha). `Escape` closes the topmost layer and focus returns to its trigger.
- Every overlay or footer action names an object the surface actually shows and a scope; hide inapplicable actions or disable them with a reason.
- Notifications are for asynchronous status. Skip a success toast when the result is already visible.

## 7. Motion

Motion explains change; it is never ambient decoration and never required to understand state.

- **Prefer built-in motion** from gpui-kit (`Tab` indicator, `Sheet`, `Progress`, `Skeleton`) before hand-rolled animation.
- **API:** `gpui::AnimationExt::with_animation(id, Animation::new(duration).with_easing(...), |el, delta| ...)` with easings such as `ease_in_out` and `ease_out_quint()`; check `cx.reduce_motion()` and render the end state at once when it is true.
- **Tokens, not literals.** When the first hand-rolled animation is needed, define the tokens once in `src/helpers/motion.rs` and use only them:

| Token | Duration | For |
| --- | --- | --- |
| `INSTANT` | 120 ms | hover wash, color change |
| `FAST` | 180 ms | press, control state |
| `BASE` | 240 ms | selection moving, glyph swap, a surface leaving |
| `SLOW` | 360 ms | a menu or dialog opening, a reveal |

  Ease-out entering, ease-in-out between two places, ease-in leaving; leaving is faster than entering; desktop pointer feedback stays under 200 ms.
- Animate color and opacity, not width/height; a layout change glides only when it is the user's own action (collapsing a pane), otherwise it jumps.
- Give each animation a stable `ElementId` keyed by the state it reflects (an epoch), so a re-render does not restart it. Never animate cells that tick with data (metrics, monitor feed, recycled key-tree rows).
- A continuous preview stays put: while the pointer moves among related targets, update content in place instead of fading out and back in.
- **Motion inventory.** For a new panel or a redesigned screen, list its state changes (hover/press, a moving selection, arriving content, an overlay opening or closing, loading, live data) with the tool and token, or `none` with a reason, in the change summary.

## 8. Data-heavy views

- Virtualize every collection that can grow (`uniform_list`, `List`, `ZedisKvTable`, `ZedisTextTable`); keep per-row state in data keyed by domain id, never in per-row entities.
- Keep headers and the identity column (the key) stable; truncate long keys with a recovery path.
- Distinguish focus, hover, active row and multi-selection; preserve selection by key across filtering and reordering.
- Keep sorting and filtering visible and reversible; use progressive disclosure for secondary columns and inspectors.
- A table compares consistent fields, a list scans heterogeneous items, a tree shows real hierarchy; never use a complex layout as decoration.
- Structured values (multi-line text, code, JSON) render in a read-only `Editor` (highlighting, line numbers, folding, search; edits snapped back on `InputEvent::Change` instead of dimmed `.disabled(true)`), paired with `ZedisJsonTree` behind a `Text | Tree` `ButtonGroup` when JSON-shaped. Never a raw `Label` dump.
- Design all content states: Empty (with the next action), Loading (only while actually waiting; a skeleton only when the shape is known), Error (cause + recovery), Truncated / Long.

## 9. Interface language (all 8 locales)

- Nouns for destinations and objects (`Keys`, `Clients`, `Slow Log`), verbs for commands (`Save`, `Export`, `Copy`), short phrases for states (`Offline`, `Read-only`). No wrappers such as `Management`, `Module`, `Function`, `Operation` unless they name a real concept. Let context carry context: inside Settings, `Appearance`, not `Appearance Settings`.
- English uses sentence case. Labels, buttons, tabs, menu items and placeholders take no final period; full sentences do. No exclamation marks in routine messages. Chinese sentences use full-width punctuation; short Chinese labels take none.
- Append `…` (one character) to a command that opens a dialog or needs more input (`Export…`, `Rename…`); not to commands that run at once.
- Confirmations: the title names the decision (`Delete "user:1"?`), the body adds only scope, consequence or recovery, the actions are `Cancel` and the result verb (`Delete`). Never "Are you sure…", "successfully", or a generic `Warning` / `Confirm` title when the condition can be named.
- Errors say what happened and the next step (`Couldn't connect. Check the host and port.`), not a bare code.
- Write each locale as natural interface language rather than translating the English shape; keep one product lexicon across toolbar, menu, dialog, palette and docs. Redis commands, key types and API identifiers keep their original form, in the mono font.

## 10. Finishing checklist

Run on every screen the change added or touched, in **light and dark**, at the **minimum window width** and at a **larger UI font size**. Report each group as passed or what was fixed; say explicitly when visual inspection was not possible (recipes: `~/.gemini/projects/zedis/memory/desktop-ui-click-test-recipe.md`, `verify-web-canvas-headless-chrome.md`). Behavior tests prove behavior, not the look.

- **Radius:** theme radii only; concentric nesting; no accidental pills; fills stay inside corners.
- **Typography:** ≤ 3–4 sizes, ≤ 2 weights; mono only for data; muted secondary text; deliberate truncation.
- **Spacing:** scale helpers only; gaps grow with relationship distance; no doubled insets; no unaudited `px(...)`.
- **Alignment:** shared spines; fixed row lanes; baselines and center lines; one hairline per boundary; scrollbars at the trailing edge.
- **Color and surfaces:** tokens only; one primary per surface; scarce badges and alerts; shadows only on floating layers.
- **Hover and pressed:** every pressable answers; quiet wash weaker than selected; no layout shift; disabled shows neither.
- **States:** focus ring visible and unclipped; selected / open / disabled / loading distinct; icon buttons square with tooltips.
- **Keyboard and accessibility (blocking):** the main task completes by keyboard (Tab order, arrows in groups, Enter, Escape returns focus); status never by color alone; contrast holds in both themes.
- **Content states:** empty, loading, error and long content designed.
- **Motion:** inventory written; tokens only; `reduce_motion` honored.
- **Tests:** `#[gpui::test]` covers the controls by shared ID constants.

## 11. Known debt (audit 2026-11-16)

Fix what you touch; never add more.

- ~114 `.primary()` calls in `src/views/`, many on toolbar commands.
- 23 raw color literals across 13 view files (`topology`, `key_tree/delegate`, `command_palette`, `status_bar`, `title_bar`, `shortcuts_overlay`, `multi_search`, `geo_map`, `recent_keys_palette`, `status_bar/telemetry`, `bitmap_editor`, `sidebar`, `bytes_editor`).
- ~562 `px(...)` literals in `src/views/`.
- 33 `cursor_pointer()` on non-link controls.
- Local helpers that should be shared (`value_search`'s stat chip, per-view panel headers).
