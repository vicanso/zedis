# Zedis UI Design Standards (gpui-kit)

Normative for `src/views/`, `src/components/` and `crates/zedis-ui/`, aligned with `gpui-kit 0.11.0` (`gpui-component 0.11.0` / `gpui-base 0.11.0` / `gpui-pre 0.3.14`). Distilled from the vendored gpui-kit Design Guides (`.claude/skills/gpui-kit-design-guides/`) and the Native Shell Design Guides (<https://nsh.dev/docs/design-guides.md>), adapted to Zedis: one desktop idiom shared by the desktop app and the web build (pointer + keyboard, compact controls, persistent navigation). Mobile, TUI, sound, springs and shaders from NSH do not apply.

`AGENTS.md` holds the must-follow summary; this file holds the component selection matrix, `gpui-kit 0.11.0` technical rules, design tables, finishing checklist, and the dated audit baseline. When they disagree, fix this file and `AGENTS.md` together.

## 1. Foundations: the UI layer

Zedis has three layers, and features compose from the top down—always reach for `gpui_kit::component` (and `gpui_kit::base` when not re-exported) first:

| Layer | Owns | Examples |
| --- | --- | --- |
| `gpui_kit::component` (+ `gpui_kit::base`) | Behavior, focus, keyboard, base look | `Button`, `ButtonGroup`, `DropdownButton`, `Input` (+ `.list()`), `Textarea`, `Editor` (+ `GutterStyle`), `NumberInput`, `Select`, `Combobox`, `Switch`, `Checkbox`, `Radio`/`RadioGroup`, `Calendar`/`DatePicker`/`TimeField`, `DataTable`, `List`, `Tree`, `VirtualList`, `DescriptionList`, `Diff`, `Dialog`, `Sheet`, `Popover`/`PopupMenu`/`DropdownMenu`/`Tooltip`/`HoverCard`, `Command`, `Tab`/`TabBar`, `Badge`, `Tag`, `Kbd`, `Empty`, `Skeleton`, `Spinner`, `Progress`, `Alert`, `Separator`, `Clipboard`, `Resizable`, `Scrollbar`/`ScrollbarPlacement`, `Toolbar`, `Chart`, `TextView`, `gpui_kit::base::SelectableText` |
| `crates/zedis-ui` | Target-agnostic styled widgets | `ZedisCard`, `ZedisDialog`, `ZedisForm`, `ZedisTextTable` (`fitted_table`), `ZedisDivider`, `ZedisSelect`, `ZedisSkeletonLoading`, `ZedisPanelHeader` (`panel_header`), `ZedisStatChip` (`stat_chip`), `ZedisHintBanner` (`hint_banner`), `help_popover`, `ZedisAboutPage` |
| `src/components/` | Redis-domain widgets | `KeyTypeBadge`, `ZedisKvDelegate` (`ZedisKvTable` / `kv_delegate`) |

- **Limited API.** A widget exposes what varies by meaning: `variant`, `Sizable` size, states (`selected`, `disabled`, `loading`), slots (label, icon, children) and events. Its colors, radii, paddings, hover and focus live inside it. Callers pass layout only (`w`, `flex_1`, alignment, margins).
- **A missing look becomes a variant first.** When a feature needs a look the layer lacks, add a variant, size or helper there, then use it. A missing color role becomes a theme-derived function in `src/helpers/color.rs` (like `card_background`), never a literal at the call site.
- **Extract on the second use.** Panel header (back button, title, trailing actions, one hairline), filter bar, stat chip, `Text | Tree` switch, status line and empty state are shared patterns: the second view that needs one extracts it.
- **No hand-built controls.** Never build a button, toggle, tab or menu from `div().on_click(...)`. The one exception is a virtualized row container inside `uniform_list` / `List`, which still owes hover, selected, keyboard and a domain `ElementId`.

### Component selection rules (use `gpui-kit` first)

Check `gpui_kit::component::*` (and `gpui_kit::base::*`) before using `crates/zedis-ui`, and only use `crates/zedis-ui` or `src/components/` when the domain/layout wrapper adds real app semantics (documented below). Hand-rolling `div` UI when a kit component fits is forbidden.

| UI need | Prefer `gpui_kit` first | Justified `crates/zedis-ui` or `src/components/` wrapper | Do not hand-roll |
| --- | --- | --- | --- |
| Actions, segmented switches, toolbars | `Button`, `ButtonGroup`, `DropdownButton`, `Toolbar` / `ToolbarGroup` | `ZedisPanelHeader` (`panel_header`) for wrapping panel headers (`px_3`, `border_b_1`) | `div().on_click(...)` buttons/tabs/sort headers; icon buttons missing `.tooltip(...)` + domain `ElementId` |
| Single-line text & autocomplete | `Input`, `Input::list(&Entity<ListState<D>>)` for completions | — | `EditorState` for non-LSP single-line suggestions (reserve `Editor` + `CompletionProvider` for semantic LSP completions) |
| Code, structured text & copyable prose | `Editor` (`GutterStyle`, folding, `active_line_only_when_focused`), `Textarea`, `gpui_kit::base::SelectableText` | — | Unselectable `Label` dumps for multi-line/copyable text; `.disabled(true)` for read-only values (use `InputEvent::Change` snap-back) |
| Pickers & boolean/choice inputs | `Select`, `Combobox`, `Switch`, `Checkbox`, `Radio`, `RadioGroup` | `ZedisSelect` (lightweight `usize`-indexed string lists) | Custom clickable `div` toggles or picker rows |
| Date & time inputs | `Calendar`, `DatePicker`, `TimeField` (all `jiff::civil`) | — | Raw `InputState` clock strings with custom parsers; `chrono` types |
| Tables, lists & key-value summaries | `DataTable`, `List` / `ListItem`, `Tree`, `VirtualList`, `DescriptionList` | `ZedisTextTable` (`fitted_table`: ring buffer, 3-mode sort, proportional `.fills()`, hover copy); `ZedisKvDelegate` (Redis collection editing) | Fixed-`px` `h_flex` key-value rows (`DescriptionList` fits) or unvirtualized scrolling tables |
| Dialogs, drawers, menus & popups | `Dialog`, `Sheet`, `Popover`, `PopupMenu`, `DropdownMenu`, `Tooltip`, `HoverCard`, `Command` | `ZedisDialog` (alerts with `.show_cancel(true)`, Cancel-first `.danger()` focus, bodyless focus wiring); `help_popover` | Hand-positioned `div` popups; inline `Input`s in `ZedisDialog` (use a view entity) |
| Forms | `Form` (`v_form` / `h_form` / `field`) | `ZedisForm` (conditional visibility, `fill_height()`, dialog footer hoisting, TSV paste) | Hardcoded window-minus-chrome height math when `fill_height()` + `fill()` works |
| Status pills, badges & metric chips | `Tag` (inline pills/chips), `Badge` (`.dot()` / `.count()` corner overlay on icons/avatars) | `KeyTypeBadge` (`.plain(true)` in tree, mono pill in header); `ZedisStatChip` (`stat_chip`, candidate to delegate styling to `Tag`) | Custom `rounded_full` / `border_1` status `div` pills or dots |
| Keyboard shortcut keycaps | `Kbd` on native desktop | `humanize_keystroke` (`uses_command_key()`) while `wasm32` needs runtime Apple-keyboard detection | Hand-styled keycap `div`s on desktop; remember `Kbd::format` uses compile-time `#[cfg(target_os = "macos")]` (wrong on `wasm32`) |
| Empty states, loading & progress | `Empty` (`EmptyHeader`, `EmptyMedia`, `EmptyTitle`, `EmptyDescription`, `EmptyContent`), `Skeleton`, `Spinner`, `Progress` | `ZedisSkeletonLoading` | Custom centered empty `v_flex` panels or `div().rounded_full()` progress bars |
| Inline notices & warnings | `Alert` (`Alert::info` / `warning` / `error`, `.on_close`) | Legacy `ZedisHintBanner` (prefer `Alert::info` going forward) | Custom `theme.warning` / `danger` / `muted` `h_flex` banners |
| Diffs | `Diff` (`DiffState`, `DiffFile::excerpts`, `DiffSpanHighlight`, `on_text_*`) | — | Custom `Label`-per-line diff lists |
| Splits, cards, dividers & scrollbars | `h_resizable` / `v_resizable`, `Separator`, `Scrollbar` (`ScrollbarPlacement`), `DockSkin` | `ZedisCard` (connection/action cards), `ZedisDivider` (auto-interleaved cluster dividers) | Manual `w(px(1.))` / `h_px()` hairline `div`s |

### `gpui-kit 0.11.0` & `gpui-pre 0.3.14` technical rules

- **Blanket `IntoElement` on `Element` (`gpui-pre 0.3.14`):** `pub trait Element: 'static + Sized` drops `IntoElement` as a supertrait and provides `impl<T: Element> IntoElement for T`. Never write a manual `impl IntoElement for MyElement` (`E0119`); `#[derive(IntoElement)]` for `RenderOnce` stays valid.
- **`jiff::civil` replaces `chrono` (`gpui-component 0.11.0`):** `calendar::Date` wraps `jiff::civil::Date` (`first_day_of_week` defaults to `jiff::civil::Weekday::Sunday`), `date_picker::DateTime` wraps `jiff::civil::DateTime`, and `TimeFieldState` / `TimeFieldEvent::Change` use `jiff::civil::Time` (formatted with `.strftime(...)`).
- **Non-generic `Path`, `FillOptions`, & `WindowBounds::centered` (`gpui-pre 0.3.14`):** `gpui::Path` is non-generic (`PathBuilder::build(self) -> Result<Path, Error>`); `FillOptions` has only `pub fill_rule: FillRule` (`tolerance` removed — call `PathBuilder::fill()` directly); `WindowBounds::centered` takes `(display_id: Option<DisplayId>, size: Size<Pixels>, cx: &App)`.
- **`Input` suggestions & `Editor` `GutterStyle` / folding (`0.11.0`):**
  - `Input::list(&Entity<ListState<D>>)` shows a native suggestion popup under single-line `Input` while focused and editable (`Enter` emits `InputEvent::PressEnter` until `Down` highlights an item).
  - `Editor::gutter_style(GutterStyle)` customizes `with_gap`, `with_line_number_color`, `with_active_line_number_color`, `with_line_number_font_family`, and `with_line_number_font_size` while keeping `Editor`'s built-in line-number reserve (`MAX_LINE_NUMBER_DIGITS = 9` in `gpui-base 0.11.0`).
  - `EditorState::active_line_only_when_focused(bool)` / `set_active_line_only_when_focused` suppresses inactive-pane line highlights.
  - Public folding on `EditorState` (and `InputState`/`TextareaState` via the underlying `gpui_kit::base::input::InputBaseState`, active in code-editor mode): `set_folded(start_line, folded, cx)`, `fold_all(cx)`, `unfold_all(cx)`, `fold_at_level(level, cx)`, and `fold_candidates()`.
- **Left/top `Scrollbar` (`ScrollbarPlacement`):** `Scrollbar::placement(ScrollbarPlacement::{BottomRight, BottomLeft, TopRight, TopLeft})` and `InputState`/`TextareaState`/`EditorState::scrollbar_placement(...)` / `set_scrollbar_placement(...)` allow left/top scrollbars when layout calls for them (default trailing edge stays `BottomRight`).
- **New `0.11.0` capabilities (`Diff`, `SelectableText`, `Button`, `Resizable`, `DockSkin`, `Presence`):** `Diff::span_highlights`, `DiffFile::excerpts(path, parts)` (partial file excerpts preserving 1-based line numbers), and `Diff::{on_text_hover, on_text_click, text_cursor}`; `gpui_kit::base::SelectableText::{reveal_range, on_reveal}`; `Button::aria_expanded(bool)` for disclosures; `ResizableState::is_resizing()`; `DockSkin::set_split_divider_line_visible(bool, cx)`; and `Presence` returning `PresencePhase::Absent` immediately when closed on first sample.
- **Linux `gpui-kit/x11` & `gpui-fast 0.1.19`:** `gpui-kit 0.11.0` enables `x11` by default alongside `wayland` (omit default features for Wayland-only Linux builds); `--features gpui-fast` compiles against `gpui-fast 0.1.19` via `extern crate gpui_kit as gpui;` in `src/lib.rs` and `crates/zedis-ui/src/lib.rs`.
- **UI integration testing (`TESTING.md` / `gpui_kit::test`):** Place `.test_support()` before `.track_focus(&handle)` on custom controls and forward public `track_focus` on wrappers (`native_parts_forward_their_public_focus_binding`). `Input`/`Textarea`/`Editor` project component IDs to their native editor (`window.find(id)` reads role/label/value/focus while `find_by_label`/role queries enumerate native owners once). Use `find` / `try_find` / `within`, immutable `ElementSnapshot` (re-query after actions), `press` (commands) vs `input` (text), `click_at` for clipped targets, `render_frame` + `wait_for` for deferred effects, and reduced motion (set by `test_support::init_ui(cx)`).

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

- **Follow the §1 component selection rules.** Prefer `gpui_kit::component::*` (and `gpui_kit::base::*`) over custom `div` chrome: `Tag` for inline pills/chips, `Badge` (`.dot()` / `.count()`) for icon/avatar corner overlays, `Alert` for inline/closable banners, `Separator` (or `ZedisDivider` for interleaved clusters) for hairlines, `Toolbar` / `ToolbarGroup` inside `ZedisPanelHeader` when arrow-key navigation across button groups is helpful, `Input::list(&ListState<D>)` for single-line autocomplete, and `Kbd` for desktop shortcut keycaps (noting `Kbd::format` uses compile-time `#[cfg(target_os = "macos")]`, so browser builds on macOS still fall back to `humanize_keystroke` via `uses_command_key()`).
- **One size per band.** Every `Button`, `ButtonGroup`, `Input`, `Select` and `DropdownMenu` trigger in one toolbar, header or filter row shares one `Sizable` step (usually `.small()`), so heights, text, icons and center lines agree.
- **Icon buttons** are square at the band's size, one size per group, each with `.tooltip(...)` and a domain `ElementId`.
- **Disclosures & triggers.** Set `.aria_expanded(open)` (alongside `.selected(open)` where appropriate) on `Button` triggers that control popovers, menus, or collapsible sections.
- **One popup surface.** Use gpui-kit's `Popover`, `PopupMenu`, `DropdownMenu`, `Select`, `HoverCard`, and `Tooltip`; never approximate one with an absolutely positioned `div` and hand-picked border/shadow.
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

- **Prefer built-in motion** from gpui-kit (`Tab` indicator, `Sheet`, `Progress`, `Skeleton`, `Presence` — which in 0.11.0 returns `PresencePhase::Absent` immediately when closed on first sample) before hand-rolled animation.
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

- Virtualize every collection that can grow (`uniform_list`, `List`, `DataTable`, `ZedisKvTable`, `ZedisTextTable`); keep per-row state in data keyed by domain id, never in per-row entities.
- Use `DescriptionList` for multi-field key-value summary grids instead of fixed-`px` `h_flex` rows.
- Keep headers and the identity column (the key) stable; truncate long keys with a recovery path.
- Distinguish focus, hover, active row and multi-selection; preserve selection by key across filtering and reordering.
- Keep sorting and filtering visible and reversible; use progressive disclosure for secondary columns and inspectors.
- A table compares consistent fields, a list scans heterogeneous items, a tree shows real hierarchy; never use a complex layout as decoration.
- Structured values (multi-line text, code, JSON) render in a read-only `Editor` (highlighting, line numbers, `GutterStyle`, folding via `fold_all`/`unfold_all`/`fold_at_level`/`set_folded`, `active_line_only_when_focused`, search; edits snapped back on `InputEvent::Change` instead of dimmed `.disabled(true)`), paired with `ZedisJsonTree` behind a `Text | Tree` `ButtonGroup` when JSON-shaped. Copyable plain-text output (such as diagnostics or `DOCTOR` prose) uses `gpui_kit::base::SelectableText` (`reveal_range`) or a read-only `Editor` — never an unselectable `Label` dump.
- Render diffs through `gpui_kit::component::diff::Diff` (`DiffState`, `DiffFile::excerpts`, `DiffSpanHighlight`, `on_text_hover`/`on_text_click`/`text_cursor`) and progress/share bars through `Progress`; render date/time inputs through `Calendar`, `DatePicker`, and `TimeField` (`jiff::civil`).
- Design all content states using `Empty` (`EmptyHeader`, `EmptyMedia`, `EmptyTitle`, `EmptyDescription`, `EmptyContent`) for empty/disconnected states (with the next action), `Skeleton` / `ZedisSkeletonLoading` / `Spinner` for Loading (only while actually waiting; a skeleton only when the shape is known), `Alert` / inline recovery for Error (cause + recovery), and Truncated / Long states.

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

## 11. Known debt (audit 2026-11-27 baseline)

**Rule:** fix debt in files and widgets you touch; never add new debt.

Historical cleanup (`2026-11-16`): centralized `src/helpers/motion.rs`, eliminated raw `rgb(...)`/`hsla(...)` literals from `src/views/`, removed non-link `.cursor_pointer()`, converted filter `.primary()` buttons to `.selected(active)`, extracted `panel_header` and `stat_chip` into `crates/zedis-ui`, fixed `account.rs` spinner `.icon(...)`, fixed `ZedisDivider` hairline color, and standardized 8-locale copy.

### Compliant invariants (verified 2026-11-27)
- **Zero raw color literals** (`rgb`/`rgba`/`hsla`/hex) in `src/views/**`, `src/components/**`, `crates/zedis-ui/src/**`, `src/root*`, `src/dialogs*` (only 2 `gpui::black()`/`white()`/`transparent_black()` fallbacks remain at `src/views/status_bar/server_status.rs:199` and `src/views/key_tree/delegate.rs:232`).
- **Zero violations** for: `.cursor_pointer()` (`0`), `Button::loading(true)` missing `.icon(...)` (`0`), layout-shifting hover rules (`0`), manual `impl IntoElement` on `Element` (`0`), `window.blur` (`0`), `InputState` `\n` placeholders (`0`), inline `Input` in `ZedisDialog` (`0`), `overflow_y_scrollbar()` + `max_h` on the same element (`0`), `.font_bold()` (`0`, and 5/5 `FontWeight::BOLD` pair with `get_mono_font_family()`), `Label` inheriting parent `.text_color()` (`0`), bare `div()` wrapping `uniform_list` directly (`0`), and hand-rolled `with_animation` (`0`, all tokens centralized in `src/helpers/motion.rs`).

### Audited baseline exceptions (keep intentional)
- **Audited `px(...)` exceptions (147/572 total)**: 1px hairlines/dividers/`px(0.)` (44), persisted/min pane, dialog, sidebar, & overlay bounds (62), canvas/chart/bitmap/geo geometry (37), and fixed `uniform_list`/table row heights (4).
- **Adaptive `max_h` scroll containers**: `crates/zedis-ui/src/help.rs`, `src/views/command_palette.rs`, `src/views/recent_keys_palette.rs`, `src/views/export_servers_dialog.rs`, `src/views/geo_map.rs`, and `src/views/shortcuts_overlay.rs:209` intentionally pair `.max_h(...)` with GPUI native `.overflow_y_scroll().track_scroll(...)` + sibling `Scrollbar` (whereas `shortcuts_overlay.rs:233-241` uses a fixed `.h(px(420.))` and can migrate to `.overflow_y_scrollbar()`).

### Open debt baseline (2026-11-27)
1. **Component adoption gaps (`gpui-kit 0.11.0`)**:
   - *Inline pills & overlay dots (`Tag` / `Badge`)*: `crates/zedis-ui/src/card.rs:314-324` (`CardVariant::Action` at `L223-264` also hand-builds clickable `div`s), `crates/zedis-ui/src/stat_chip.rs` (structural overlap with `Tag`), `src/components/kv_delegate.rs:582-592` (`format_chip`; `L615-625` manual copy button vs `Clipboard`), `src/components/key_type_badge.rs` (non-plain mode overlaps `Tag::custom`), `src/views.rs:224-247` (`unavailable_chip`), `status_bar/server_status.rs:198-227`, `title_bar.rs:506-515`, `servers/grid.rs:71-77,236-242`, `config_editor/render.rs:71-78`, `sidebar.rs:602-614,663-682`, `hll_editor.rs:271-277`, `vector_set_editor.rs:732-738`, `probabilistic_editor.rs:307-313`, `timeseries_editor.rs:224-233`, `topology/replication.rs:63-72`, `root/tabs.rs:468-470`.
   - *Inline notices & warnings (`Alert`)*: `crates/zedis-ui/src/hint.rs` (`ZedisHintBanner`), `src/views/persistence.rs:363-379`, `src/views/server_info.rs:626-636`, `src/views/pubsub_editor.rs:710-717`.
   - *Hand-built progress bars (`Progress`)*: `src/views/memory_analysis/render.rs:938-945`, `src/views/slowlog_editor/top_commands.rs:538-545`, `src/views/hotkeys.rs:468-480`.
   - *Hand-built empty/disconnected states (`Empty`)*: rich states (`unsupported_panel.rs`, `editor/render.rs`, `value_search.rs`, `keyspace_notifications.rs`, `value_diff.rs`, `key_tree/render.rs`, `sidebar.rs`, `config_editor/render.rs`) and centered text empty states (`search_manager/render.rs`, `workbench/results.rs`, `server_info.rs`, `pubsub_editor.rs`, `memory_analysis/render.rs`, `monitor.rs`, `hotkeys.rs`, `json_tree.rs`, `probabilistic_editor.rs`).
   - *Manual 1px hairlines (`Separator` / `ZedisDivider`)*: `src/views/pubsub_editor.rs:613`, `src/views/value_diff.rs:564`, `src/views/servers/grid.rs:245`, `src/views/sidebar.rs:275`.
   - *Unadopted `0.11.0` & kit widgets*: `Input::list` (`src/views/bytes_editor.rs:562-607` JSONPath bar still cites pre-0.11 single-line suggestion gap; `src/views/terminal.rs:628-680` suggestion popup), `EditorState` folding (`bytes_editor.rs`, `editor/more_menu.rs`), `TimeField` (`src/views/expire_at_dialog.rs:116-120,219` hand-parses `InputState` clock), `Diff` (`src/views/key_timeline_dialog.rs:117-192,849-893` bespoke diff; `src/views/config_editor/render.rs:242-325` `ConfigDiff` non-virtualized rows), `Kbd` (`editor/render.rs:840-858`, `shortcuts_overlay.rs:163-171`, `command_palette.rs:124-143`, `terminal.rs:615` — deferred/caveated on wasm due to `Kbd::format` compile-time `target_os = "macos"`), `gpui_kit::base::SelectableText` (`server_report_dialog.rs:150-157`, `crates/zedis-ui/src/about.rs:168-204`), `DescriptionList` (`persistence.rs:632-688`, `features_dialog.rs:106-134`; `persistence.rs:692-785` also hand-builds a fixed-width cluster table).
2. **Unaudited `px(...)` spacing & sizing (425/572 total: 424/569 core UI + 1/3 in `src/root/tabs.rs`)**:
   - Spacer divs/gaps/margins (`persistence.rs:1034-1066`, `crates/zedis-ui/src/form.rs:916,981`, `command_palette.rs:928`, `key_tree/delegate.rs:275,279,432`, `connection_diagnostics.rs:168`, `features_dialog.rs:107`).
   - Fixed column/control widths & heights not scaling with UI font size — top files: `persistence.rs` (19), `geo_map.rs` (18), `metrics/alerts_dialog.rs` (17), `slowlog_editor/top_commands.rs` (14), `benchmark/render.rs` (13), `search_manager/render.rs` (11), `timeseries_editor.rs` (11), `memory_analysis/render.rs` (10), `stream_editor/info_pending.rs` (10), `server_load.rs` (9), `slowlog_editor/latency.rs` (9), `clients_manager.rs` (8), plus `crates/zedis-ui/src/text_table.rs:251-252` and `src/components/kv_delegate.rs:270-271`.
   - Markdown heading `px(...)` ignoring `_base` (`crates/zedis-ui/src/help.rs:42-44`, `src/views/memory_analysis/rules.rs:42-45`) and fixed spinner/swatch/dot `px(...)` sizes (`key_tag_dialog.rs`, `root/tabs.rs:35`, `Spinner::with_size(px(14.))` in 4 views).
3. **Interactive controls, `.primary()` budget, IDs, & states**:
   - *19 custom clickable `div`/`h_flex`/`v_flex().on_click` controls*: 16 lacking `Role::Button` & `tab_stop` (`server_load.rs:638-704`, `workbench.rs:397-412`, `key_tag_dialog.rs:163-172,375-385`, `key_timeline_dialog.rs:665-677`, `keyspace_notifications/table.rs:169-176`, `monitor/summary.rs:266-280`, `lua_script_library/debugger.rs:647-657`, `search_manager/render.rs:420-429`, `benchmark/render.rs:693-704`, `workbench/snippets.rs:228-238`, `vector_set_editor.rs:612-645,691-700`, `topology/{cluster_load,cluster_nodes,cluster_slots}.rs`, `servers/grid.rs:246`, `key_tree/render.rs:242-274`) + 3 with `Role::Button` lacking `tab_stop` / keyboard trigger (`root/tabs.rs:416-438`, `title_bar.rs:403-433,570-585`).
   - *`.primary()` overuse (44 total)*: 2 toggle/picker controls (`key_export.rs:698`, `slowlog_editor/commandlog.rs:163`), 7 multi-`.primary()` decision surfaces (`key_export.rs`, `compare_window.rs`, `keyspace_notifications.rs`, `search_manager/{render,cursor}.rs`, `migration_window/sections.rs`, `function_editor.rs`, `lua_script_library/render.rs`), and 4 non-commit toolbar/navigation buttons (`servers/grid.rs:120`, `proto_editor.rs:811`, `script_editor.rs:836`, `unsupported_panel.rs:132`).
   - *States & IDs*: 1 icon-only `Button` missing `.tooltip(...)` (`multi_search.rs:714`), `list_active` used on `.hover(...)` (`vector_set_editor.rs:612,697`, `geo_map.rs:1046`), `.underline()` on non-link cell (`monitor/summary.rs:273`), and 47 index-based `ElementId`s on `Button::new` (e.g. `form/add_fields.rs`, `root/tabs.rs`, `acl_manager.rs`, `migration_window/sections.rs`, `servers/grid.rs`, `workbench.rs`).
4. **Layout wrappers, color contrast tests, & accessibility**:
   - *5 block `div().flex_1().min_h_0()` wrappers* around entities/`AnyView`/`AnyElement` (`acl_manager.rs:799`, `editor/render.rs:151,1148`, `secondary_window.rs:170`, `search_manager.rs:1125` — use `v_flex()`).
   - *Color contrast test gaps (`src/helpers/color.rs`)*: `hex_address_color`, `canvas_surface_background`, `geo_node_colors`, `TOPOLOGY_MASTER_PALETTE`, and `topology_heat_color` lack automated WCAG AA 4.5:1 tests.
   - *Status by color alone (3 sites)*: `lua_script_library/debugger.rs:658` (breakpoint dot without icon/label/tooltip), `key_tag_dialog.rs:153,168,378` (selected swatch border color only), `topology/cluster_slots.rs:480,489-495` (slot bar segments drop `_label`).
   - *Focus traps & key contexts*: custom overlays (`command_palette.rs`, `recent_keys_palette.rs`, `shortcuts_overlay.rs`) lack `.focus_trap(...)`; only 4 `key_context` declarations exist across `src/views/`.
   - *Font-weight advisory*: `FontWeight::SEMIBOLD` / `.font_semibold()` / `.font_medium()` sites mostly omit an explicit concrete font family (safe on bold since `5/5` `FontWeight::BOLD` use `get_mono_font_family()`).
