# gpui-kit Usage Guide

**Contents:** [Setup](#setup) · [Component Types](#component-types) · [Common Components](#common-components) (Button, Input & Editor, Select, Checkbox, Icon, Dialog, Notification, Tabs, Tooltip, Form, List, Calendar/DatePicker/TimeField, Diff, Empty, SelectableText, Scrollbar/Dock/Resizable/Presence) · [Theming](#theming) · [Layout Helpers](#layout-helpers) · [Overlay Layers](#overlay-layers-dialogs-sheets-notifications) · [Shared Traits](#shared-traits)

## Setup

Use the [tested application recipe](recipes.md) for complete examples and their verification command. Store subscription handles on the owning view; binding one to a constructor-local variable alone does not keep it alive after construction.

### 1. Cargo.toml

```toml
[dependencies]
gpui-kit = "0.11" # re-exports GPUI (gpui-pre 0.3.14), platform, base, component and default icons
```

- **Linux / FreeBSD `x11` default feature**: `gpui-kit 0.11` enables `default = ["component", "assets", "x11"]`. `x11` pulls `gpui_platform/x11` (and `gpui-fast-platform?/x11`), while `wayland` stays enabled unconditionally on non-mobile targets — set `default-features = false` (re-enabling `"component"`, `"assets"`) for a Wayland-only Linux/FreeBSD build.
- **GPUI Fast backend**: enable `features = ["gpui-fast"]` to select `gpui-fast 0.1.19`.
- **Zedis workspace wiring**: Zedis keeps `[workspace.dependencies]` renames of `gpui-pre-* = "0.3.14"` (`gpui`, `gpui_platform`, `gpui_macros`, `gpui_web`), and under `gpui-fast` aliases `extern crate gpui_kit as gpui;` in `src/lib.rs` and `crates/zedis-ui/src/lib.rs`. So in Zedis source use `gpui::…`, `#[gpui::test]` + `test_support::init_ui(cx)`, `gpui_kit::component::…` (`gpui_kit::component::init(cx)`), `gpui_kit::assets::{Assets, IconName}`, and `gpui_kit::base::…`.

### 2. Initialization

```rust
fn main() {
    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .run(move |cx| {
            gpui_kit::init(cx); // MUST be first (Zedis calls gpui_kit::component::init(cx))

            // Wraps the view in a `Root`, which enables dialogs, sheets,
            // notifications, tooltips and menus in this window.
            gpui_kit::open_window(WindowOptions::default(), cx, |_, cx| cx.new(|_| MyApp))
                .expect("Failed to open window");
        });
}
```

Use `gpui_kit::open_window` for application windows; it mounts `Root` as the window's root view. For centered bounds in `gpui-pre 0.3.14`, pass `Option<DisplayId>`: `WindowBounds::centered(None, size(px(960.), px(640.)), cx)`.

---

## Component Types

### Stateless (most components)

Used directly in `render`, no stored state:

```rust
use gpui_kit::component::button::Button;

impl Render for MyView {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        Button::new("btn").primary().label("Submit")
            .on_click(|_, _, _| println!("clicked"))
    }
}
```

### Stateful (Input, Select, Combobox, etc.)

Require an `Entity<State>` stored in your view:

```rust
use gpui_kit::component::input::{Input, InputState};

struct MyView {
    name: Entity<InputState>,
}

impl MyView {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self {
            name: cx.new(|cx| InputState::new(window, cx).placeholder("Your name")),
        }
    }
}

impl Render for MyView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        Input::new(&self.name)
    }
}
```

---

## Common Components

### Button

```rust
use gpui_kit::component::button::{Button, ButtonGroup};

// Variants
Button::new("btn").label("Default")
Button::new("btn").primary().label("Primary")
Button::new("btn").danger().label("Delete")
Button::new("btn").warning().label("Warning")
Button::new("btn").success().label("Success")
Button::new("btn").ghost().label("Ghost")
Button::new("btn").link().label("Link")

// States (`loading(true)` requires `.icon(...)` because the spinner replaces the icon)
Button::new("btn").label("Text").disabled(true)
Button::new("btn").icon(IconName::Loader).label("Text").loading(true)
Button::new("btn").label("Text").selected(true)
Button::new("btn").label("Details").aria_expanded(self.expanded)

// With icon
Button::new("btn").icon(IconName::Plus).label("Add")

// Sizes
Button::new("btn").xsmall().label("XS")
Button::new("btn").small().label("S")
Button::new("btn").large().label("L")

// Group
ButtonGroup::new("group")
    .child(Button::new("a").label("A"))
    .child(Button::new("b").label("B"))
    .on_click(|indices, _, _| { /* selected indices */ })
```

### Input

```rust
use gpui_kit::component::input::{Input, InputState};

// State setup (in new/init)
let input = cx.new(|cx| InputState::new(window, cx)
    .placeholder("Enter text...")
    .default_value("Hello")
);

// Render
Input::new(&input)
Input::new(&input).cleanable(true)           // clear button
Input::new(&input).disabled(true)
Input::new(&input).prefix(Icon::new(IconName::Search).small())
Input::new(&input).suffix(Button::new("b").ghost().icon(IconName::X).xsmall())
Input::new(&input).content_type(InputContentType::Password)
Input::new(&input).mask_toggle()             // password reveal toggle
Input::new(&input).appearance(false)         // remove default border/bg
Input::new(&input).list(&suggestions)        // suggestion List popup while focused & editable

// Reading value
let value = input.read(cx).value();

// Keep this subscription in the owning view (for example, `_subscriptions`).
self._subscriptions.push(cx.subscribe_in(&input, window, |view, state, event, window, cx| {
    match event {
        InputEvent::Change => { let v = state.read(cx).value(); }
        InputEvent::PressEnter { .. } => { /* submit */ }
        InputEvent::Focus | InputEvent::Blur => {}
    }
}));
```

### Editor (`GutterStyle`, folding, focused active line)

```rust
use gpui_kit::component::input::{Editor, EditorState, GutterStyle};

let editor = cx.new(|cx| {
    EditorState::new(window, cx)
        .language("json")
        .line_number(true)
        .folding(true)
        .active_line_only_when_focused(true)
});

// Programmatic folding on EditorState:
editor.update(cx, |state, cx| {
    state.fold_all(cx);
    state.unfold_all(cx);
    state.fold_at_level(2, cx);
    let _changed = state.set_folded(0, true, cx);
});

// Per-editor line-number styling:
Editor::new(&editor).gutter_style(
    GutterStyle::default()
        .with_gap(rems(0.5))
        .with_line_number_color(cx.theme().muted_foreground)
        .with_active_line_number_color(cx.theme().foreground),
)
```

### Select

```rust
use gpui_kit::component::{IndexPath, select::{Select, SelectState}};

// Simple string list
let state = cx.new(|cx| {
    SelectState::new(vec!["Apple", "Orange", "Banana"], Some(IndexPath::default()), window, cx)
});

// Render
Select::new(&state)
Select::new(&state).placeholder("Pick one")

// Reading selection
let selected = state.read(cx).selected_value();
```

### Checkbox / Switch / Radio

```rust
use gpui_kit::component::{checkbox::Checkbox, switch::Switch};

// Stateless (controlled)
Checkbox::new("cb").checked(self.checked)
    .on_change(cx.listener(|this, checked, _, cx| {
        this.checked = *checked;
        cx.notify();
    }))

Switch::new("sw").checked(self.enabled)
    .on_change(cx.listener(|this, checked, _, cx| {
        this.enabled = *checked;
        cx.notify();
    }))
```

### Icon

```rust
use gpui_kit::component::{Icon, IconName};

Icon::new(IconName::Check)
Icon::new(IconName::Search).small()
Icon::new(IconName::Plus).large().text_color(cx.theme().primary)
```

### Dialog

```rust
use gpui_kit::component::dialog::{Dialog, DialogAction, DialogClose, DialogFooter};

// Open from window context. `footer` takes an element, not a closure.
// DialogClose dismisses the dialog, so no manual close call is needed.
window.open_dialog(cx, |dialog, _, _| {
    dialog
        .title("Export Report")
        .child("Choose a destination for the exported file.")
        .footer(
            DialogFooter::new()
                .gap_2()
                .child(DialogClose::new().child(
                    Button::new("cancel").label("Cancel").outline(),
                ))
                .child(DialogAction::new().child(
                    Button::new("export").label("Export").primary(),
                )),
        )
});
```

### AlertDialog

Use `AlertDialog` — not `Dialog` — to confirm a consequential action. It is not
overlay-closable and has no close button, so the choice must be made. Name the
object in the title and the result on the confirming button; see the Design
Guides for the copy rules.

```rust
use gpui_kit::component::button::ButtonVariant;

window.open_alert_dialog(cx, |alert, _, _| {
    alert
        .title("Remove “Roadmap”?")
        .description("Files on disk aren’t deleted.")
        .confirm()
        .ok_text("Remove")
        .ok_variant(ButtonVariant::Danger)
        .on_ok(|_, _, _| true)
});
```

`button_props(DialogButtonProps)` takes the same properties as one value; it
overrides only the fields that value sets, so `confirm` and earlier callbacks
survive it.

### Notification

```rust
// Simple string message
window.push_notification("Saved successfully!", cx);

// With type variant
window.push_notification(
    Notification::info("File uploaded").title("Upload complete"),
    cx,
);
```

### Tabs

```rust
use gpui_kit::component::tab::{Tab, TabBar};

TabBar::new("tabs")
    .child(Tab::new().label("Overview"))
    .child(Tab::new().label("Settings"))
    .child(Tab::new().label("Logs"))
```

### Tooltip

```rust
// On any element with .id(), add .tooltip():
div()
    .id("my-btn")
    .tooltip(|window, cx| Tooltip::new("Delete item").build(window, cx))
    .child("Delete")

// Or on a Button directly:
Button::new("btn").icon(IconName::Trash).tooltip("Delete")

// Component tooltips open after 500 ms; override per trigger,
// or install `tooltip::TooltipDefaults` to change it app-wide:
Button::new("help").icon(IconName::Info).tooltip("Help")
    .tooltip_show_delay(Duration::ZERO)
```

### Form

```rust
use gpui_kit::component::form::{v_form, h_form, field};

// Vertical form
v_form()
    .child(field().label("Name").child(Input::new(&self.name)))
    .child(field().label("Email").child(Input::new(&self.email)))
    .footer(Button::new("submit").primary().label("Submit"))

// Horizontal label alignment
h_form()
    .child(field().label("Username").child(Input::new(&self.username)))
```

### List (searchable, virtualized)

```rust
use gpui_kit::component::list::{List, ListState, ListDelegate, ListItem, ListEvent};

// Implement ListDelegate for your data type, then:
let list_state = cx.new(|cx| ListState::new(MyDelegate::new(), window, cx));

// Render
List::new(&list_state)
// Keep this subscription in the owning view, alongside list_state.
self._subscriptions.push(cx.subscribe(&list_state, |this, _, event, cx| {
    if let ListEvent::Select(index_path) = event {
        // handle selection
    }
}));
```

### Calendar, DatePicker, and TimeField (`jiff::civil`)

`gpui-kit 0.11` uses `jiff::civil::{Date, DateTime, Time, Weekday}` and `.strftime(...)` (replacing `chrono`).

```rust
use gpui_kit::component::{
    calendar::{Calendar, CalendarState, Date},
    date_picker::{DatePicker, DatePickerState},
    time_field::{HourCycle, TimeField, TimeFieldEvent, TimeFieldState, TimePrecision},
};
use jiff::civil::{Weekday, date, time};

let cal = cx.new(|cx| CalendarState::new(window, cx));
Calendar::new(&cal).first_day_of_week(Weekday::Monday);

let picker = cx.new(|cx| {
    DatePickerState::new(window, cx)
        .time_precision(TimePrecision::Minute)
        .default_time(time(9, 30, 0, 0))
});
DatePicker::new(&picker);

let tf = cx.new(|cx| {
    TimeFieldState::new(window, cx)
        .precision(TimePrecision::Second)
        .hour_cycle(HourCycle::H23)
});
TimeField::new(&tf);
```

### Diff

`Diff` renders supplied unified/Git patches, merge conflicts, or partial file excerpts:

```rust
use gpui_kit::component::diff::{Diff, DiffFile, DiffSpanHighlight, DiffState};

// Full patch or non-overlapping excerpts keeping each excerpt's 1-based line numbers
let excerpt = DiffFile::excerpts("src/lib.rs", [(40, "let x = 1;\n")]);
let state = cx.new(|cx| DiffState::new(vec![excerpt], cx));

Diff::new(&state)
    .span_highlights(highlights) // Vec<DiffSpanHighlight>
    .on_text_hover(|pos, mods, window, cx| { /* highlight link while Cmd/Ctrl held */ })
    .on_text_click(|pos, event, window, cx| false) // return true to claim click
    .text_cursor(CursorStyle::PointingHand)
```

### Empty and SelectableText

```rust
use gpui_kit::base::SelectableText; // base-only primitive
use gpui_kit::component::{
    Icon, IconName,
    empty::{Empty, EmptyContent, EmptyDescription, EmptyHeader, EmptyMedia, EmptyMediaVariant, EmptyTitle},
};

Empty::new()
    .header(
        EmptyHeader::new()
            .media(EmptyMedia::new().with_variant(EmptyMediaVariant::Icon).child(Icon::new(IconName::Inbox)))
            .title(EmptyTitle::new().child("No items yet"))
            .description(EmptyDescription::new().child("Create one to get started.")),
    )
    .content(EmptyContent::new().child(action_button));

SelectableText::new("snippet", text).reveal_range(0..12);
```

### Scrollbar Placement, DockSkin, Resizable, and Presence

- **Left / top scrollbars**: `Scrollbar::vertical(&handle).placement(ScrollbarPlacement::BottomLeft)` (variants: `BottomRight`, `BottomLeft`, `TopRight`, `TopLeft`), and `InputState` / `EditorState` / `TextareaState` `.scrollbar_placement(placement)` or `.set_scrollbar_placement(placement, cx)`.
- **Dock split divider**: `DockSkin::set_split_divider_line_visible(false, cx)` hides the resting 1 px divider between split slots while keeping hover/drag indicators and hit area.
- **Resizable drag state**: `ResizableState::is_resizing(&self) -> bool` reports whether a handle is currently dragged (note: starting a drag does not notify; read during render).
- **Presence**: `gpui_kit::base::motion::Presence::new(id, present).sample(window, cx)` immediately returns `PresencePhase::Absent` without scheduling an animation frame when initially sampled closed (`present: false`).

---

## Theming

```rust
use gpui_kit::component::ActiveTheme as _;

// Access colors
cx.theme().primary
cx.theme().background
cx.theme().foreground
cx.theme().border
cx.theme().secondary
cx.theme().muted
cx.theme().danger

// Use in styles
div()
    .bg(cx.theme().secondary)
    .text_color(cx.theme().foreground)
    .border_color(cx.theme().border)
```

### Switch Theme

```rust
use gpui_kit::component::{Theme, ThemeMode};

// Switch light/dark: loads that mode's registered theme
Theme::change(ThemeMode::Dark, None, cx);
// or, as one edit among others
Theme::update(cx, |theme| theme.mode = ThemeMode::Dark);

// Load a named theme
Theme::update(cx, |theme| theme.apply_config(&theme_config));

// Edit fields; `update` keeps colors, tokens and the Base projection in step
Theme::update(cx, |theme| theme.radius = px(8.));
```

---

## Layout Helpers

`gpui_kit::component` extends GPUI with convenient layout methods:

```rust
use gpui_kit::component::{Icon, IconName, h_flex, label::Label, v_flex};

// h_flex() = div().flex().flex_row().items_center()
// v_flex() = div().flex().flex_col()

// Common patterns
h_flex().gap_2().items_center()
    .child(Icon::new(IconName::User))
    .child(Label::new("Username"))

v_flex().gap_4().p_4()
    .child(Input::new(&self.name))
    .child(Input::new(&self.email))
    .child(Button::new("submit").primary().label("Submit"))
```

---

## Overlay Layers (Dialogs, Sheets, Notifications)

The window's `Root` renders dialogs, sheets and notifications above the view, so a window opened with `gpui_kit::open_window` needs nothing else — `window.open_dialog(..)` just works. Applications define their own quit and close-window actions, key bindings, and confirmation flows. `open_window` returns `(AnyWindowHandle, Entity<V>)`. See [the tested application recipe](recipes.md) for a complete consumer view.

Root always mounts all three layers automatically without manual `Root::render_*_layer` calls.

---

## Shared Traits

Builders return the component so methods can be chained. Constructors follow component families: controls take a stable ID, retained controls take state, and compound parts may take no arguments. See [component conventions](conventions.md).

- `Sizable`: `.xsmall()` / `.small()` / `.medium()` (default) / `.large()`
- `Disableable`: `.disabled(bool)`
- `Selectable`: `.selected(bool)`
- `Styled`: any GPUI style methods (`.w()`, `.bg()`, `.p_2()`, etc.)

For any component not covered here, fetch its doc from:
`https://gpui-kit.com/component/{name}.md`
