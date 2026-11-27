---
name: gpui-kit
description: 'How to build desktop applications with GPUI Kit, the Rust framework published as the gpui-kit crate (GPUI plus gpui_kit::component, gpui_kit::base, gpui_kit::assets). Use when setting up a gpui-kit app, choosing or using a component (Button, Input, Select, Dialog, Sheet, Tabs, Sidebar, List, DataTable, Tree, Chart, etc.), handling component state, theming, or window overlays, and for GPUI mechanics: actions and keybindings, async tasks, contexts, custom elements, entities, events, focus, global state, layout and styling, ElementId, and tests including UI integration testing. Holds the normative Coding Guides: read them before any architecture, state-ownership, public API, naming, or testing decision. Pairs with the gpui-kit-design-guides skill for the Design Guides.'
---

# GPUI Kit

Applications depend on `gpui-kit` (`0.11.0`, pinned to `gpui-pre 0.3.14` or optional `gpui-fast 0.1.19`). Upstream GPUI is `use gpui_kit::*;`, and each layer is reachable by name: `gpui_kit::component` (styled components), `gpui_kit::base` (unstyled behavior), `gpui_kit::assets` (default icons), and `gpui_kit::platform`.

- **Linux / FreeBSD `x11` default feature**: `gpui-kit` enables `default = ["component", "assets", "x11"]`. `x11` turns on `gpui_platform/x11` (and `gpui-fast-platform?/x11`), while `wayland` remains enabled unconditionally on non-mobile targets — set `default-features = false` (keeping `"component"`, `"assets"`) for a Wayland-only Linux/FreeBSD build.
- **Zedis workspace wiring**: Zedis pins `gpui-kit = "0.11.0"` alongside `[workspace.dependencies]` renames of `gpui-pre-* = "0.3.14"` (`gpui`, `gpui_platform`, `gpui_macros`, `gpui_web`), and under `--features gpui-fast` aliases `extern crate gpui_kit as gpui;` in `src/lib.rs` and `crates/zedis-ui/src/lib.rs`. Therefore, inside Zedis:
  - GPUI types and macros keep `gpui::…` and `#[gpui::test]` (use `test_support::init_ui(cx)` in `src/` UI tests).
  - Components, overlays, and initialization use `gpui_kit::component::…` (`gpui_kit::component::init(cx)`), icons use `gpui_kit::assets::{Assets, IconName}`, and low-level base primitives (`SelectableText`, `Presence`, `NavStack`) use `gpui_kit::base::…`.

Start with [component-family conventions](references/conventions.md) to choose the constructor, state owner, event, and layout contract for the task. Learn the family once, then verify the specific component supports the capability.

For a complete compiled view with retained state, subscriptions, and overlay
layers, read [the tested application recipe](references/recipes.md).

## Read the Guides First

Two guides hold the rules this skill assumes. They are requirements, not
inspiration. Read the guide file itself; do not answer from this page, from a
similar file in the codebase, or from training data.

| Guide                                                                        | Read before                                                                                                                   |
| ---------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------- |
| [Design Guides](../gpui-kit-design-guides/references/design-guides.md)       | Choosing components, layout, spacing, hierarchy, color, density, interaction states, overlays, motion, interface copy          |
| [Coding Guides](references/coding-guides.md)                                 | Crate layering, `RenderOnce` vs `Entity<T>`, state ownership, `ElementId`, events, focus, async, public API, naming, testing |

Read the Design Guides first when the change has a visible surface: code
structure preserves product intent, it does not replace it. If the design
skill is not installed, fetch `https://gpui-kit.com/docs/design-guides.md`.
The local copies link directly across skills (`references/coding-guides.md` and
`../gpui-kit-design-guides/references/design-guides.md`), with
`https://gpui-kit.com/docs/coding-guides.md` and
`https://gpui-kit.com/docs/getting-started.md` as upstream references.

### Coding Guides section map

Read the whole guide for a new crate, module, or feature. For a narrow change,
read "Architecture at a glance" and "Rules for coding agents" first, then the
section for the change (`grep -n '^## ' references/coding-guides.md`).

| Section                              | Read when                                                              |
| ------------------------------------ | ---------------------------------------------------------------------- |
| Architecture at a glance             | Always; crate layering, ownership boundary                             |
| Bootstrap and root ownership         | `main`, `init`, `Root`, window creation, app-level state               |
| Understand GPUI's phases and contexts | Anything touching `App`, `Window`, `Context<T>`, render vs update      |
| Choose the right unit                | Deciding `RenderOnce` vs `Entity<T>` vs custom `Element`               |
| State ownership                      | Where a piece of state lives, who mutates it, `Entity<State>` handles  |
| Stable identity                      | `ElementId`, lists, repeated elements, keyed state                     |
| Rendering and composition            | `render`, builder chains, `when`/`map`, child composition              |
| Behavior and presentation boundary   | `gpui-base` vs `gpui-component` vs application code                    |
| Theme and styling                    | `cx.theme()`, tokens, `Styled`, sizes, variants                        |
| Events, actions, and focus           | `cx.emit`, `subscribe`, `actions!`, keybindings, `FocusHandle`         |
| Async work and side effects          | `cx.spawn`, `background_spawn`, `Task`, I/O, timers                    |
| Layout, measurement, and scrolling   | Flex layout, sizing, `overflow`, scroll handles, measuring             |
| Lists, tables, and large data        | `VirtualList`, `List`, `DataTable`, delegates, large collections       |
| Public API design                    | Anything `pub`: builders, private fields, setter and reader naming     |
| Platform and capability boundaries   | macOS/Windows/Linux/wasm differences, feature gates                    |
| File and naming conventions          | New files, modules, type and method names, `Kind` suffix, `Context`    |
| Testing strategy                     | What to test, `#[gpui_kit::test]`, `TestAppContext`                    |
| Performance rules                    | Render cost, allocation, re-render triggers                            |
| Common failure modes                 | Before finishing; invented APIs, state in render, index ids            |
| Rules for coding agents              | Always when an agent writes code                                       |
| Implementation checklist             | Before finishing; run every item against the work                     |

### Non-negotiables

A floor, not a substitute for the guides.

- **Never invent an API.** Search the current source for the real signature.
  Do not translate a React, CSS, or older-GPUI example by analogy; a
  plausible-looking method that does not exist is the most common failure.
- **One dependency (or Zedis's aliased workspace setup).** Standalone apps depend
  on `gpui-kit` alone (`use gpui_kit::*;`); Zedis aliases `gpui-pre 0.3.14`
  as `gpui::` alongside `gpui-kit 0.11.0` (`gpui_kit::component`,
  `gpui_kit::base`, `gpui_kit::assets`).
- **Framework owns behavior, application owns presentation.** Do not put
  colors, sizing, or layout in `gpui-base`; do not put interaction behavior in
  application styling code.
- **Stable identity.** Repeated elements need domain-derived `ElementId`s, not
  list indexes.
- **No `pub` fields across the seam.** Public data types use builders and
  reader methods.
- **Spell `Context` out.** `cx` is GPUI's; name anything else after what it
  holds.
- **`gpui-pre 0.3.14` breaking APIs:**
  - `Element` has a blanket `impl<T: Element> IntoElement for T` (`pub trait Element: 'static + Sized`) — **never** write a manual `impl IntoElement for MyElement` (`E0119`); `#[derive(IntoElement)]` remains valid for `RenderOnce`.
  - `Path` is non-generic (`pub struct Path`); `FillOptions` has only `fill_rule` (`tolerance` removed — use `PathBuilder::fill()`).
  - `WindowBounds::centered(display_id: Option<DisplayId>, size, cx)` takes `Option<DisplayId>` as its first argument.
- **`jiff` replaces `chrono`:** `Calendar`, `DatePicker`, and `TimeField` use `jiff::civil::{Date, DateTime, Time, Weekday}` and `.strftime(...)` — never `chrono`.

## Documentation

- **Find a task/component page**: use `https://gpui-kit.com/llms.txt`, then load its Markdown page.
- **Full reference**: `https://gpui-kit.com/llms-full.txt` is available when broad reference is actually needed
- **Per-component API**: fetch `https://gpui-kit.com/component/{name}.md`,
  e.g. `button.md`, `input.md`, `select.md`, `dialog.md`, `data-table.md`
- **Any site page** can be fetched as Markdown by appending `.md` to the URL

## Quick Reference

Setup and examples: [references/usage.md](references/usage.md).

```rust
use gpui_kit::*;

gpui_kit::application()
    .with_assets(gpui_kit::assets::Assets)
    .run(|cx| {
        gpui_kit::init(cx);                       // first, before anything else
        gpui_kit::open_window(options, cx, |window, cx| {
            cx.new(|cx| AppView::new(window, cx))
        })
        .expect("failed to open window");
    });
```

- **Stateless** (`RenderOnce`): build in `render`:
  `Button::new("save").primary().label("Save").on_click(|_, _, _| {})`
- **Stateful**: hold `Entity<State>` in the view, pass a reference in `render`:
  `let input = cx.new(|cx| InputState::new(window, cx));` then `Input::new(&self.input)`
- **Sizes**: `.xsmall()` `.small()` `.medium()` (default) `.large()`
- **Theme**: `cx.theme().primary` · `.background` · `.foreground` · `.border` · `.muted`
- **Overlays**: `window.open_dialog(...)`, `open_sheet(...)`, `push_notification(...)`
  via `gpui_kit::component::WindowExt`

## Component Catalog

Import paths are relative to `gpui_kit::component::`, so `input::{Input, InputState}`
means `use gpui_kit::component::input::{Input, InputState};`. Base-only primitives
(not re-exported by `component`) live under `gpui_kit::base::`. For the full API
fetch the component's `.md` doc or inspect `gpui-component` / `gpui-base` source.

### Input & Form

| Component       | Import                                                              | Notes                                                                                                       |
| --------------- | ------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------- |
| `Input`         | `input::{Input, InputState, InputGroup, AnyInputState}`             | Stateful. Single-line; `.list(&list_state)` suggestion popup, `InputContentType::DateTime`, mask, validate |
| `Textarea`      | `input::{Textarea, TextareaState}`                                  | Stateful. Multi-line text (`auto_grow`, `rows`)                                                             |
| `Editor`        | `input::{Editor, EditorState, GutterStyle}`                         | Stateful. Code editor (`tree-sitter`/`syntect`), `Editor::gutter_style`, `active_line_only_when_focused`, public `fold_all`/`unfold_all`/`fold_at_level`/`set_folded` |
| `Highlighter`   | `highlighter::{SyntaxHighlighter, HighlightTheme, LanguageRegistry}` | Syntax highlighting & language grammar registry                                                             |
| `History`       | `history::{History, UndoHistory}`                                   | Undo/redo history utilities                                                                                 |
| `NumberInput`   | `input::{NumberInput, NumberInputEvent, NumberStep}`                | Stateful. Numeric with step                                                                                 |
| `OtpInput`      | `input::{OtpInput, OtpState, OtpEvent}`                             | Stateful. One-time password                                                                                 |
| `SpeechButton`  | `speech::{SpeechButton, SpeechState}`                               | Stateful. Dictation, `speech` feature                                                                       |
| `Select`        | `select::{Select, SelectState, SelectEvent}`                        | Stateful. Dropdown picker (`selected_value()`)                                                              |
| `Combobox`      | `combobox::{Combobox, ComboboxState}`                               | Stateful. Searchable select                                                                                 |
| `Checkbox`      | `checkbox::Checkbox`                                                | Stateless. `on_change` / `on_click` receives `&bool`                                                        |
| `Switch`        | `switch::Switch`                                                    | Stateless. Toggle (`on_change`)                                                                             |
| `Radio`         | `radio::{Radio, RadioGroup}`                                        | Stateless. (`on_change`)                                                                                    |
| `Slider`        | `slider::{Slider, SliderState}`                                     | Stateful.                                                                                                   |
| `Toggle`        | `button::{Toggle, ToggleGroup}`                                     | Stateless. Single toggle or toggle group                                                                    |
| `Rating`        | `rating::Rating`                                                    | Stateless.                                                                                                  |
| `Stepper`       | `stepper::{Stepper, StepperItem}`                                   | Stateless. Multi-step progress                                                                              |
| `ColorPicker`   | `color_picker::{ColorPicker, ColorSelect, ColorPickerState}`        | Stateful. Full picker or swatch selector                                                                    |
| `DatePicker`    | `date_picker::{DatePicker, DatePickerState, DateTime, DateRangePreset}` | Stateful. Uses `jiff::civil::{Date, DateTime, Time, Weekday}`                                            |
| `TimeField`     | `time_field::{TimeField, TimeFieldState, TimeFieldEvent, TimePrecision, HourCycle}` | Stateful. Segmented time editor (`jiff::civil::Time`), 24/12-hour                                           |
| `Calendar`      | `calendar::{Calendar, CalendarState, Date, Matcher}`                | Stateful. Inline month view (`jiff::civil::{Date, Weekday}`)                                                |
| `Form`          | `form::{Form, Field, v_form, h_form, field}`                        | Layout container (`columns`, `label_layout`, `footer`)                                                      |
| `Questionnaire` | `questionnaire::*`                                                  | Guided multi-question flow                                                                                  |

### Display & Feedback

| Component        | Import                                                                                  | Notes                                                                                  |
| ---------------- | --------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------- |
| `Button`         | `button::{Button, ButtonGroup}`                                                         | Stateless. `.aria_expanded(bool)` disclosure state; `.loading(true)` requires `.icon(...)` |
| `Icon`           | `{Icon, IconName}`                                                                      | Stateless. Full Lucide catalog also at `gpui_kit::assets::IconName`                    |
| `Badge`          | `badge::Badge`                                                                          | Stateless. Count, dot, or icon overlay badge                                           |
| `Tag`            | `tag::{Tag, TagVariant}`                                                                | Stateless. Status chip / closable tag                                                  |
| `Avatar`         | `avatar::{Avatar, AvatarGroup}`                                                         | Stateless.                                                                             |
| `Label`          | `label::Label`                                                                          | Stateless. Form label (sets own `.text_color`)                                         |
| `Kbd`            | `kbd::Kbd`                                                                              | Stateless. Keyboard shortcut badge (uses compile-time `target_os = "macos"` formatting) |
| `Link`           | `link::Link`                                                                            | Stateless. External URLs / `mailto:` only                                              |
| `Separator`      | `separator::{Separator, SeparatorStyle}`                                                | Stateless. Horizontal/vertical solid or dashed hairline                                |
| `Empty`          | `empty::{Empty, EmptyHeader, EmptyMedia, EmptyMediaVariant, EmptyTitle, EmptyDescription, EmptyContent}` | Stateless. Structured empty state with media, title, description, and action slots     |
| `Alert`          | `alert::Alert`                                                                          | Stateless. Info/success/warning/error                                                  |
| `Spinner`        | `spinner::Spinner`                                                                      | Stateless. Loading indicator                                                           |
| `Skeleton`       | `skeleton::Skeleton`                                                                    | Stateless. Loading placeholder                                                         |
| `Shimmer`        | `shimmer::{ShimmerText, ShimmerStyle}`                                                  | Stateless. Streaming-text shimmer                                                      |
| `Marker`         | `marker::{Marker, MarkerVariant}`                                                       | Stateless. Inline status marker                                                        |
| `Progress`       | `progress::{Progress, ProgressCircle}`                                                  | Stateless.                                                                             |
| `Tooltip`        | `tooltip::Tooltip`                                                                      | Via `.tooltip()` on elements                                                           |
| `HoverCard`      | `hover_card::{HoverCard, HoverCardState}`                                               | Stateful.                                                                              |
| `Clipboard`      | `clipboard::Clipboard`                                                                  | Stateless. Copy button                                                                 |
| `TextView`       | `text::{Text, TextView, TextViewState, markdown, html}`                                 | Rich text / Markdown / HTML (`TextView::markdown(id, text)`)                           |
| `SelectableText` | `gpui_kit::base::SelectableText`                                                        | Base-only (`gpui_kit::base`). Selectable text with `.reveal_range(...)` / `.on_reveal(...)` |
| Image            | `gpui_kit::{img, ImageSource, ObjectFit}` (Zedis: `gpui::`)                             | GPUI's `img()` element                                                                 |

### Overlay & Popups

| Component        | Import                                                                                          | Notes                                                                                  |
| ---------------- | ----------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------- |
| `Dialog`         | `dialog::{Dialog, DialogContent, DialogHeader, DialogTitle, DialogDescription, DialogFooter, DialogClose, DialogAction, DialogButtonProps, DialogEntrance}` + `WindowExt` | Via `window.open_dialog(...)`                                                          |
| `AlertDialog`    | `dialog::AlertDialog` + `WindowExt`                                                             | Via `window.open_alert_dialog(...)`                                                    |
| `Sheet`          | `sheet::Sheet` + `WindowExt`                                                                    | Side panel, via `window.open_sheet(...)`                                               |
| `Notification`   | `notification::Notification` + `WindowExt`                                                      | Via `window.push_notification(...)`                                                    |
| `Popover`        | `popover::{Popover, PopoverState}`                                                              | Floating overlay (deferred prepaint focus move)                                        |
| `Menu`           | `menu::{AppMenuBar, ContextMenu, ContextMenuExt, DropdownMenu, PopupMenu, PopupMenuItem}`       | Context (`.context_menu(...)` + `PopupMenu`), dropdown, and app menus                  |
| `NativeMenu`     | `native_menu::NativeMenu`                                                                       | Native/context menu builder (used by `Editor`)                                         |
| `DropdownButton` | `button::DropdownButton`                                                                        | Button with dropdown menu                                                              |
| `Command`        | `command::{Command, CommandState, CommandGroup}`                                                | Stateful. Command palette                                                              |
| Focus trap       | `FocusTrapElement`                                                                              | `.focus_trap(id, &handle)` on a container; `Dialog` and `Sheet` have it built in       |
| `Presence`       | `gpui_kit::base::motion::{Presence, PresencePhase, PresenceSample}`                             | Mount/exit presence; initially closed returns `PresencePhase::Absent` with no frame    |

### Navigation & Layout

| Component         | Import                                                                   | Notes                                                                                  |
| ----------------- | ------------------------------------------------------------------------ | -------------------------------------------------------------------------------------- |
| `Tabs` / `TabBar` | `tab::{Tab, TabBar, TabVariant}`                                         | Tabbed interface (`Tab::new().label(...)`)                                             |
| `Sidebar`         | `sidebar::{Sidebar, SidebarMenu, ...}`                                   | App navigation panel                                                                   |
| `TitleBar`        | `TitleBar`                                                               | Window title bar                                                                       |
| `StatusBar`       | `status_bar::StatusBar`                                                  | Window status bar                                                                      |
| `Toolbar`         | `toolbar::{Toolbar, ToolbarGroup}`                                       | Grouped action toolbar                                                                 |
| `Breadcrumb`      | `breadcrumb::Breadcrumb`                                                 | Navigation breadcrumb                                                                  |
| `Pagination`      | `pagination::Pagination`                                                 | Page navigation                                                                        |
| `Accordion`       | `accordion::Accordion`                                                   | Collapsible sections                                                                   |
| `Collapsible`     | `collapsible::Collapsible`                                               | Single collapsible                                                                     |
| `GroupBox`        | `group_box::GroupBox`                                                    | Labeled container                                                                      |
| `Dock`            | `dock::{DockArea, DockLayout, DockPlacement, DockSkin, DockState, Panel, TitleStyle, TabGroup, DragPanelPreview}` | Dockable layout (`DockSkin::set_split_divider_line_visible(bool, cx)`)                 |
| `GridLayout`      | `grid_layout::{grid_layout, GridLayout, GridLayoutState, GridItem, GridArea}` | Draggable/resizable dashboard grid                                                     |
| `Resizable`       | `resizable::{h_resizable, v_resizable, resizable_panel, ResizablePanelGroup, ResizableState}` | Draggable split panes (`ResizableState::is_resizing()`)                                |
| `ResizeHandle`    | `resizable::{resize_handle, HandleEdge}`                                 | Standalone resize edge                                                                 |
| `Scrollbar`       | `scroll::{Scrollbar, ScrollbarAxis, ScrollbarPlacement, ScrollableElement}` | Custom scrollbar (`BottomRight`, `BottomLeft`, `TopRight`, `TopLeft`)                  |
| `WindowBorder`    | `{WindowBorder, window_border}`                                          | Client-side window border / padding helpers                                            |

### Data Display

| Component         | Import                                                                                       | Notes                                                                                                       |
| ----------------- | -------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------- |
| `DataTable`       | `table::{DataTable, TableState, TableDelegate, Column}`                                      | Stateful. Full-featured virtualized table                                                                   |
| `Diff`            | `diff::{Diff, DiffState, DiffFile, DiffSpanHighlight, DiffDecoration, DiffGutterMarker, DiffTextPosition}` | Readonly patch/conflict/excerpt display; `.span_highlights(...)`, `DiffFile::excerpts(...)`, `DiffState::has_line(...)`, `.on_text_hover(...)` / `.on_text_click(...)` / `.text_cursor(...)` |
| `Table`           | `table::{Table, ...}`                                                                        | Simpler table                                                                                               |
| `VirtualList`     | `{v_virtual_list, h_virtual_list}`                                                           | High-perf large lists                                                                                       |
| `List`            | `list::{List, ListState, ListDelegate, ListItem, ListEvent}`                                 | Stateful. Searchable list                                                                                   |
| `SearchableList`  | `searchable_list::{SearchableListState, SearchableListDelegate, SearchableVec, SearchableListChange}` | Shared searchable/selectable list engine behind `Select` & `Combobox`                                       |
| `Tree`            | `tree::{tree, Tree, TreeState, TreeItem, TreeEntry}`                                         | Stateful. Hierarchy                                                                                         |
| `DescriptionList` | `description_list::{DescriptionList, DescriptionItem}`                                       | Key-value pairs                                                                                             |
| `Settings`        | `setting::Settings`                                                                          | Settings panel                                                                                              |

### Chat & Messaging

| Component         | Import                                                   | Notes                                 |
| ----------------- | -------------------------------------------------------- | ------------------------------------- |
| `Message`         | `message::{Message, MessageContent, MessageAlignment}`   | Stateless. Chat message row           |
| `Bubble`          | `bubble::{Bubble, BubbleContent, BubbleVariant}`         | Stateless. Message bubble             |
| `Attachment`      | `attachment::{Attachment, AttachmentContent, ...}`       | Stateless. File/media attachment card |
| `MessageScroller` | `message_scroller::{MessageScroller, MessageScrollerState}` | Stateful. Auto-scrolling message list |

### Charts

| Component | Import                                                                                 | Notes                          |
| --------- | -------------------------------------------------------------------------------------- | ------------------------------ |
| `Chart`   | `chart::{AreaChart, BarChart, CandlestickChart, LineChart, PieChart, RadarChart, SankeyChart}` | Charts                         |
| `Plot`    | `plot::Plot`                                                                           | `#[derive(IntoPlot)]` for data |

## Testing

**UI integration testing** means rendering real components in headless windows,
simulating input, and checking state, focus, layout and owner callbacks. Use
`#[gpui_kit::test]` (or `#[gpui::test]` with `test_support::init_ui(cx)` in Zedis)
to run tests and `gpui_kit::test` to operate and inspect the UI.
When asked to add component interaction coverage, describe it as UI integration testing.

For unit tests, GPUI context tests or UI integration tests, read
[Testing](references/gpui/test.md). It includes dependency setup, a complete
runnable UI flow, scoped native interactions, frame/async handling and the
limits of accessibility and geometry assertions. Use the production view and
verify the action's outcome through normal Rust assertions.

## GPUI References

Load the file for the mechanism the task touches. Each file starts with a
contents line.

| Topic                       | File                                                   | Load when                                                       |
| --------------------------- | ------------------------------------------------------ | --------------------------------------------------------------- |
| Actions & keybindings       | [action.md](references/gpui/action.md)                 | `actions!`, `bind_keys`, `on_action`, `key_context`             |
| Async & background tasks    | [async.md](references/gpui/async.md)                   | `cx.spawn`, `background_spawn`, `Task`, async I/O               |
| Context management          | [context.md](references/gpui/context.md)               | `App`, `Window`, `Context<T>`, `AsyncApp`                       |
| Custom elements (low-level) | [element.md](references/gpui/element.md)               | `Element` trait, `request_layout`, `prepaint`, `paint`          |
| Entity state                | [entity.md](references/gpui/entity.md)                 | `Entity<T>`, `WeakEntity`, state management                     |
| Events & subscriptions      | [event.md](references/gpui/event.md)                   | `cx.emit`, `cx.subscribe`, `cx.observe`                         |
| Focus & keyboard nav        | [focus-handle.md](references/gpui/focus-handle.md)     | `FocusHandle`, `track_focus`, Tab navigation                    |
| Global state                | [global.md](references/gpui/global.md)                 | `Global` trait, `cx.set_global`, app-wide config                |
| Layout & styling            | [layout-style.md](references/gpui/layout-style.md)     | `div()`, `h_flex()`, `v_flex()`, flexbox, overflow, positioning |
| ElementId                   | [element-id.md](references/gpui/element-id.md)         | `ElementId`, `.id()`, uniqueness rules, stateful elements       |
| Testing                     | [test.md](references/gpui/test.md)                     | `#[gpui_kit::test]`, `TestAppContext`, `VisualTestContext`      |

Deep dives, for when the topic file is not enough:

- **Element trait**: [element-api.md](references/gpui/element-api.md) (complete API, hitbox, events) ·
  [element-patterns.md](references/gpui/element-patterns.md) (text, interactive, container, composite) ·
  [element-examples.md](references/gpui/element-examples.md) (full examples) ·
  [element-best-practices.md](references/gpui/element-best-practices.md) (performance, state, pitfalls) ·
  [element-advanced.md](references/gpui/element-advanced.md) (custom layouts, async updates, virtual lists)
- **Entities**: [entity-api.md](references/gpui/entity-api.md) (complete API, lifecycle) ·
  [entity-patterns.md](references/gpui/entity-patterns.md) (model-view, cross-entity, observer) ·
  [entity-best-practices.md](references/gpui/entity-best-practices.md) (memory, performance) ·
  [entity-advanced.md](references/gpui/entity-advanced.md) (collections, registry, debounce, state machines)
- **Testing**: [test-examples.md](references/gpui/test-examples.md) (organization, setup, assertions, running tests) ·
  [test-reference.md](references/gpui/test-reference.md) (re-entrancy, property tests, mocking)
