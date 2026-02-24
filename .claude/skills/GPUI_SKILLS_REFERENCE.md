# GPUI Development Skills Reference

This document consolidates all GPUI-related skills from `.claude/skills/` for reference. Use it when implementing actions, async, context, elements, entities, events, focus, globals, layout/style, testing, or new components.

---

## gpui-action

**Description:** Action definitions and keyboard shortcuts in GPUI. Use when implementing actions, keyboard shortcuts, or key bindings.

### Overview

Actions provide declarative keyboard-driven UI interactions in GPUI.

**Key Concepts:**
- Define actions with `actions!` macro or `#[derive(Action)]`
- Bind keys with `cx.bind_keys()`
- Handle with `.on_action()` on elements
- Context-aware via `key_context()`

### Quick Start

**Simple Actions:**

```rust
use gpui::actions;

actions!(editor, [MoveUp, MoveDown, Save, Quit]);

const CONTEXT: &str = "Editor";

pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("up", MoveUp, Some(CONTEXT)),
        KeyBinding::new("down", MoveDown, Some(CONTEXT)),
        KeyBinding::new("cmd-s", Save, Some(CONTEXT)),
        KeyBinding::new("cmd-q", Quit, Some(CONTEXT)),
    ]);
}

impl Render for Editor {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .key_context(CONTEXT)
            .on_action(cx.listener(Self::move_up))
            .on_action(cx.listener(Self::move_down))
            .on_action(cx.listener(Self::save))
    }
}
```

**Actions with Parameters:**

```rust
#[derive(Clone, PartialEq, Action, Deserialize)]
#[action(namespace = editor)]
pub struct InsertText {
    pub text: String,
}
```

### Key Formats

- Modifiers: `cmd-s`, `ctrl-c`, `alt-f`, `shift-tab`, `cmd-ctrl-f`
- Keys: `a-z`, `0-9`, `f1-f12`, `up`, `down`, `left`, `right`, `enter`, `escape`, `space`, `tab`, `backspace`, `delete`

### Best Practices

- Use contexts for context-aware bindings.
- Name actions clearly (verb-noun: `SaveDocument`, `CloseTab`, `TogglePreview`).
- Handle with listeners: `div().on_action(cx.listener(Self::on_action_save))`.

---

## gpui-async

**Description:** Async operations and background tasks in GPUI. Use when working with async, spawn, background tasks, or concurrent operations. Essential for handling async I/O, long-running computations, and coordinating between foreground UI updates and background work.

### Overview

GPUI provides an integrated async runtime for foreground UI updates and background computation.

**Key Concepts:**
- **Foreground tasks**: UI thread, can update entities (`cx.spawn`)
- **Background tasks**: Worker threads, CPU-intensive work (`cx.background_spawn`)
- All entity updates happen on the foreground thread

### Quick Start

**Foreground Tasks (UI Updates):**

```rust
cx.spawn(async move |cx| {
    let data = fetch_from_api().await;
    entity.update(cx, |state, cx| {
        state.data = Some(data);
        cx.notify();
    }).ok();
}).detach();
```

**Background Tasks (Heavy Work):**

```rust
cx.background_spawn(async move { heavy_computation().await })
    .then(cx.spawn(move |result, cx| {
        entity.update(cx, |state, cx| {
            state.result = result;
            cx.notify();
        }).ok();
    }))
    .detach();
```

### Common Pitfalls

- **Don't** update entities from background tasks directly.
- **Do** chain with a foreground task: `cx.background_spawn(...).then(cx.spawn(...)).detach()`.

---

## gpui-context

**Description:** Context management in GPUI including App, Window, and AsyncApp. Use when working with contexts, entity updates, or window operations. Different context types provide different capabilities for UI rendering, entity management, and async operations.

### Overview

**Context Types:**
- **`App`**: Global app state, entity creation
- **`Window`**: Window-specific operations, painting, layout
- **`Context<T>`**: Entity-specific context for component `T`
- **`AsyncApp`**: Async context for foreground tasks
- **`AsyncWindowContext`**: Async context with window access

### Context Hierarchy

```
App (Global)
  └─ Window (Per-window)
       └─ Context<T> (Per-component)
            └─ AsyncApp (In async tasks)
                 └─ AsyncWindowContext (Async + Window)
```

### Common Operations

- **Entity:** `cx.new(...)`, `entity.update(cx, ...)`, `entity.read(cx)`
- **Notifications:** `cx.notify()`, `cx.emit(MyEvent::Updated)`
- **Observe/Subscribe:** `cx.observe(&entity, ...)`, `cx.subscribe(&entity, ...)`
- **Window:** `window.is_window_focused()`, `window.bounds()`, `window.remove_window()`
- **Async:** `cx.spawn(...)`, `cx.background_spawn(...)`

---

## gpui-element

**Description:** Implementing custom elements using GPUI's low-level Element API (vs. high-level Render/RenderOnce APIs). Use when you need maximum control over layout, prepaint, and paint phases for complex, performance-critical custom UI components that cannot be achieved with Render/RenderOnce traits.

### When to Use

- Need fine-grained control over layout calculation
- Building complex, performance-critical components
- Implementing custom layout algorithms (masonry, circular, etc.)
- High-level `Render`/`RenderOnce` APIs are insufficient

**Prefer `Render`/`RenderOnce` for:** Simple components, standard layouts, declarative UI.

### Core Concepts

**Three-Phase Rendering:**
1. **request_layout**: Calculate sizes and positions, return layout ID and state
2. **prepaint**: Create hitboxes, compute final bounds, prepare for painting
3. **paint**: Render element, set up interactions (mouse events, cursor styles)

**Key Operations:**
- Layout: `window.request_layout(style, children, cx)`
- Hitboxes: `window.insert_hitbox(bounds, behavior)`
- Painting: `window.paint_quad(...)`
- Events: `window.on_mouse_event(handler)`

---

## gpui-entity

**Description:** Entity management and state handling in GPUI. Use when working with entities, managing component state, coordinating between components, handling async operations with state updates, or implementing reactive patterns. Entities provide safe concurrent access to application state.

### Overview

An `Entity<T>` is a handle to state of type `T`, providing safe access and updates.

**Key Methods:**
- `entity.read(cx)` → `&T`
- `entity.read_with(cx, |state, cx| ...)` → `R`
- `entity.update(cx, |state, cx| ...)` → `R`
- `entity.downgrade()` → `WeakEntity<T>`
- `entity.entity_id()` → `EntityId`

**Entity Types:** `Entity<T>` (strong), `WeakEntity<T>` (weak, doesn't prevent cleanup).

### Core Principles

- **Always use weak references in closures** to prevent retain cycles.
- **Use inner context** inside `entity.update` closure: `inner_cx.notify()`, not `cx.notify()`.
- **Avoid nested updates**; do sequential updates instead.

### Common Use Cases

1. Component state (reactive)
2. Shared state between components
3. Parent-child coordination (use weak refs)
4. Async state updates
5. Observations (reacting to other entities)

---

## gpui-event

**Description:** Event handling and subscriptions in GPUI. Use when implementing events, observers, or event-driven patterns. Supports custom events, entity observations, and event subscriptions for coordinating between components.

### Overview

**Event Mechanisms:**
- **Custom Events**: Define and emit type-safe events
- **Observations**: React to entity state changes
- **Subscriptions**: Listen to events from other entities
- **Global Events**: App-wide event handling

### Quick Start

**Define and Emit:**

```rust
#[derive(Clone)]
enum MyEvent { DataUpdated(String), ActionTriggered }

cx.emit(MyEvent::DataUpdated(data));
```

**Subscribe:**

```rust
cx.subscribe(&source, |this, emitter, event: &MyEvent, cx| {
    match event {
        MyEvent::DataUpdated(data) => this.handle_update(data.clone(), cx),
        MyEvent::ActionTriggered => this.handle_action(cx),
    }
}).detach();
```

**Observe:**

```rust
cx.observe(&target, |this, observed, cx| {
    println!("Target changed");
    cx.notify();
}).detach();
```

### Best Practices

- Detach subscriptions: `.detach()` to keep them alive.
- Use clear event types (e.g. enum with descriptive variants).
- Avoid mutual subscriptions that can create event loops.

---

## gpui-focus-handle

**Description:** Focus management and keyboard navigation in GPUI. Use when handling focus, focus handles, or keyboard navigation. Enables keyboard-driven interfaces with proper focus tracking and navigation between focusable elements.

### Overview

**Key Concepts:**
- **FocusHandle**: Reference to focusable element
- **Focus tracking**: Current focused element
- **Keyboard navigation**: Tab/Shift-Tab between elements
- **Focus events**: on_focus, on_blur

### Quick Start

```rust
struct FocusableComponent { focus_handle: FocusHandle }

impl FocusableComponent {
    fn new(cx: &mut Context<Self>) -> Self {
        Self { focus_handle: cx.focus_handle() }
    }
}

// In render:
div()
    .track_focus(&self.focus_handle)
    .on_action(cx.listener(Self::on_enter))
```

### Focus Management

- `self.focus_handle.focus(cx)` — focus
- `self.focus_handle.is_focused(cx)` — check focus
- `cx.blur()` — blur

Elements with `track_focus()` participate in Tab order automatically.

### Best Practices

- Track focus on interactive elements.
- Provide visual focus indicators (e.g. border when `is_focused`).

---

## gpui-global

**Description:** Global state management in GPUI. Use when implementing global state, app-wide configuration, or shared resources.

### Overview

**Key Trait:** `Global` — implement on types to make them globally accessible.

### Quick Start

```rust
impl Global for AppSettings {}

cx.set_global(AppSettings { theme: Theme::Dark, language: "en".to_string() });
let settings = cx.global::<AppSettings>();

cx.update_global::<AppSettings, _>(|settings, cx| {
    settings.theme = new_theme;
});
```

### When to Use

**Use Globals for:** App-wide configuration, feature flags, shared services (HTTP client, logger), read-only reference data.

**Use Entities for:** Component-specific state, frequently changing state, state that needs notifications.

### Best Practices

- Use `Arc` for shared resources in globals.
- Prefer immutable by default; use interior mutability when needed.
- Don't overuse globals; use entities for component state.

---

## gpui-layout-and-style

**Description:** Layout and styling in GPUI. Use when styling components, layout systems, or CSS-like properties.

### Overview

**Key Concepts:**
- Flexbox layout system
- Styled trait for chaining styles
- Size units: `px()`, `rems()`, `relative()`
- Colors, borders, shadows

### Quick Start

```rust
div()
    .w(px(200.))
    .h(px(100.))
    .bg(rgb(0x2196F3))
    .text_color(rgb(0xFFFFFF))
    .rounded(px(8.))
    .p(px(16.))
    .flex()
    .flex_row()
    .gap(px(8.))
    .items_center()
    .justify_between()
```

### Theme Integration

```rust
div()
    .bg(cx.theme().surface)
    .text_color(cx.theme().foreground)
    .when(is_hovered, |el| el.bg(cx.theme().hover))
```

---

## gpui-style-guide

**Description:** GPUI Component project style guide based on gpui-component code patterns. Use when writing new components, reviewing code, or ensuring consistency with existing gpui-component implementations. Covers component structure, trait implementations, naming conventions, and API patterns observed in the actual codebase.

### Component Structure

- `#[derive(IntoElement)]` on struct
- Fields: `id: ElementId`, `base: Div`, `style: StyleRefinement`, then configuration, content, callbacks
- Implement: `InteractiveElement`, `StatefulInteractiveElement`, `Styled`, `RenderOnce`
- Optional: `Sizable`, `Selectable`, `Disableable`
- Callbacks: `Rc<dyn Fn(Args, &mut Window, &mut App) + 'static>`
- Optional children: `Option<AnyElement>` or `Vec<AnyElement>`

### Trait Patterns

- **Sizable:** `with_size(mut self, size)` and size field
- **Selectable:** `selected(mut self, selected: bool)`, `is_selected(&self)`
- **Disableable:** `disabled(mut self, disabled: bool)`, `is_disabled(&self)`

### Variant Patterns

- Enum variants (e.g. `ButtonVariant::Primary`, `Secondary`, `Danger`, …)
- Trait-based API: `fn primary(self)`, `fn danger(self)` delegating to `with_variant`
- Custom variant struct with builder methods when needed

### Quick Checklist

- [ ] `#[derive(IntoElement)]`, `id`, `base`, `style`
- [ ] `InteractiveElement`, `StatefulInteractiveElement`, `Styled`, `RenderOnce`
- [ ] `Sizable` / `Selectable` / `Disableable` as appropriate
- [ ] `Rc<dyn Fn>` for callbacks, theme via `cx.theme()`
- [ ] Follow field organization and import patterns

---

## gpui-test

**Description:** Writing tests for GPUI applications. Use when testing components, async operations, or UI behavior.

### Overview

- Use `#[gpui::test]` and `TestAppContext` for GPUI tests.
- If the test does not require windows or rendering, prefer a simple Rust test without `#[gpui::test]` and `TestAppContext`.

### Test Contexts

**TestAppContext** — entity and app operations without windows:

```rust
#[gpui::test]
fn test_entity_operations(cx: &mut TestAppContext) {
    let entity = cx.new(|cx| MyComponent::new(cx));
    entity.update(cx, |component, cx| {
        component.value = 42;
        cx.notify();
    });
    let value = entity.read_with(cx, |component, _| component.value);
    assert_eq!(value, 42);
}
```

**VisualTestContext** — window and rendering:

```rust
#[gpui::test]
fn test_with_window(cx: &mut TestAppContext) {
    let window = cx.update(|cx| {
        cx.open_window(Default::default(), |_, cx| cx.new(|cx| MyComponent::new(cx))).unwrap()
    });
    let mut cx = VisualTestContext::from_window(window.into(), cx);
    // ...
}
```

### Attributes

- `#[gpui::test]` — basic or async test
- `#[gpui::test(iterations = 10)]` — property test with `StdRng`

---

## new-component

**Description:** Create new GPUI components. Use when building components, writing UI elements, or creating new component implementations.

### Instructions

1. **Follow existing patterns**: Base implementation on components in `crates/ui/src` (e.g. Button, Select).
2. **Style consistency**: Follow existing component styles and Shadcn UI patterns.
3. **Component type decision:**
   - Stateless elements for simple components (e.g. Button).
   - Stateful elements for complex components with data (e.g. Select, SelectState).
4. **API consistency**: Match API style of other elements.
5. **Documentation**: Add component documentation.
6. **Stories**: Add component stories in the story folder.

### Component Types

- **Stateless**: Pure presentation, no internal state.
- **Stateful**: Manage their own state and data.

---

## generate-component-story

**Description:** Create story examples for components. Use when writing stories, creating examples, or demonstrating component usage.

### Instructions

1. **Follow existing patterns**: Base stories on `crates/story/src/stories` (e.g. `tabs_story.rs`, `group_box_story.rs`).
2. **Use sections**: Organize with `section!` for each major part.
3. **Comprehensive coverage**: Include all options, variants, and usage examples.

### Typical Story Structure

- Basic usage
- Variants and states
- Interactive examples
- Edge cases and error states

---

## generate-component-documentation

**Description:** Generate documentation for new components. Use when writing docs, documenting components, or creating component documentation.

### Instructions

1. **Follow existing patterns**: Use styles from the `docs` folder (e.g. `button.md`, `accordion.md`).
2. **Reference implementations**: Base docs on the same-named story in `crates/story/src/stories`.
3. **API references**: Use markdown code blocks and links to docs.rs where applicable.

### Content to Include

- Component description and purpose
- Props/API documentation
- Usage examples
- Visual examples (if applicable)

---

*Generated from `.claude/skills/` GPUI-related skills. For full references (e.g. api-reference.md, patterns.md) see the corresponding skill folders under `.claude/skills/`.*
