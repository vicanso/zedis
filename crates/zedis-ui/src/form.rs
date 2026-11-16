// Copyright 2026 Tree xie.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

mod add_fields;
mod field;

pub use add_fields::*;
pub use field::{ZedisFormActionsBuilder, ZedisFormField, ZedisFormFieldType, ZedisFormOptions};
use field::{ZedisFormCancelHandler, ZedisFormSubmitHandler};
use gpui::{
    AnyElement, App, ElementId, Entity, FontWeight, Render, SharedString, Subscription, Window, div, prelude::*, px,
};
use gpui_kit::component::alert::Alert;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::form::{Field, field, v_form};
use gpui_kit::component::highlighter::Language;
use gpui_kit::component::input::{
    Editor, EditorState, Input, InputEvent, InputState, NumberInput, NumberInputEvent, Position, StepAction, Textarea,
    TextareaState,
};
use gpui_kit::component::label::Label;
use gpui_kit::component::radio::RadioGroup;
use gpui_kit::component::scroll::ScrollableElement;
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::text::TextView;
use gpui_kit::component::v_flex;
use gpui_kit::component::{ActiveTheme, Disableable, StyledExt, h_flex};
use indexmap::IndexMap;
use std::collections::HashMap;
use std::mem::take;
use std::sync::Arc;

/// Arranges buttons following platform conventions.
///
/// - macOS/Linux: primary button on the right
/// - Windows: primary button on the left
pub fn platform_buttons(mut buttons: Vec<Button>) -> Vec<Button> {
    if cfg!(target_os = "windows") {
        buttons.reverse();
    }
    buttons
}

/// Runtime state wrapper for each field type, holding a GPUI entity handle.
///
/// The three text kinds are separate types, not one `InputState` with a mode:
/// gpui-component splits the single-line, multi-line and code-editing engines
/// into `InputState` / `TextareaState` / `EditorState`, each rendered by its
/// own element. The form treats all of them as "a field with a value", so the
/// fan-out lives in [`ZedisFormFieldState::value`] and
/// [`ZedisFormFieldState::set_value`] rather than at every call site.
enum ZedisFormFieldState {
    Input(Entity<InputState>),
    Textarea(Entity<TextareaState>),
    Editor(Entity<EditorState>),
    RadioGroup(Entity<usize>),
    Checkbox(Entity<bool>),
}

impl ZedisFormFieldState {
    /// The field's current value as a string, whatever widget backs it.
    fn value(&self, cx: &App) -> SharedString {
        match self {
            Self::Input(state) => state.read(cx).value(),
            Self::Textarea(state) => state.read(cx).value(),
            Self::Editor(state) => state.read(cx).value(),
            Self::RadioGroup(state) => state.read(cx).to_string().into(),
            Self::Checkbox(state) => state.read(cx).to_string().into(),
        }
    }

    /// Writes `value` back into the field, parsing it for the two non-text
    /// widgets the way [`Self::value`] rendered it.
    fn set_value(&self, value: &SharedString, window: &mut Window, cx: &mut App) {
        match self {
            Self::Input(state) => state.update(cx, |state, cx| state.set_value(value.clone(), window, cx)),
            Self::Textarea(state) => state.update(cx, |state, cx| state.set_value(value.clone(), window, cx)),
            Self::Editor(state) => state.update(cx, |state, cx| state.set_value(value.clone(), window, cx)),
            Self::RadioGroup(state) => state.update(cx, |state, _| *state = value.parse::<usize>().unwrap_or(0)),
            Self::Checkbox(state) => state.update(cx, |state, _| *state = value == "true"),
        }
    }

    /// Moves the caret to the end of a text field, so the auto-focused field
    /// opens ready to append rather than to overwrite. A no-op elsewhere.
    fn focus_at_end(&self, window: &mut Window, cx: &mut App) {
        let end = Position::new(0, u32::MAX);
        match self {
            Self::Input(state) => state.update(cx, |state, cx| state.set_cursor_position(end, window, cx)),
            Self::Textarea(state) => state.update(cx, |state, cx| state.set_cursor_position(end, window, cx)),
            Self::Editor(state) => state.update(cx, |state, cx| state.set_cursor_position(end, window, cx)),
            Self::RadioGroup(_) | Self::Checkbox(_) => {}
        }
    }
}

/// A dynamic form component built on GPUI. Manages a heterogeneous list of
/// form fields (text inputs, number inputs, checkboxes, radio groups), optional
/// tab-based grouping, validation, and submit/cancel actions.
///
/// Construct via [`ZedisFormOptions`] and `cx.new(|cx| ZedisForm::new(...))`.
pub struct ZedisForm {
    id: ElementId,
    title: Option<SharedString>,
    description: Option<SharedString>,
    confirm_label: SharedString,
    confirm_tooltip: Option<SharedString>,
    cancel_label: SharedString,
    /// One-shot flag: focus the designated field on the first render only.
    should_focus: bool,
    field_states: Vec<(ZedisFormField, ZedisFormFieldState)>,
    add_field_states: Vec<(Entity<InputState>, Entity<InputState>)>,
    add_field_placeholder: SharedString,
    add_value_placeholder: SharedString,
    add_button_label: Option<SharedString>,
    add_button_tooltip: Option<SharedString>,
    remove_button_tooltip: Option<SharedString>,
    tab_selected_index: Entity<usize>,
    support_add_fields: bool,
    support_add_fields_on: Option<(SharedString, Vec<usize>)>,
    add_fields_mode: Option<ZedisFormAddFieldsMode>,
    add_fields_modes_on: Option<(SharedString, Vec<(usize, ZedisFormAddFieldsMode)>)>,
    errors: HashMap<SharedString, SharedString>,
    required_msg: SharedString,
    on_submit: Option<ZedisFormSubmitHandler>,
    on_cancel: Option<ZedisFormCancelHandler>,
    foot_actions: Option<ZedisFormActionsBuilder>,
    tabs: Option<Vec<SharedString>>,
    _subscriptions: Vec<Subscription>,
    pub is_processing: bool,
    disabled: bool,
    pub(super) in_dialog: bool,
    fill_height: bool,
    pending_field_updates: Vec<(SharedString, SharedString)>,
}

impl ZedisForm {
    /// Create a new form entity from the given options.
    ///
    /// This wires up GPUI entities for each field and subscribes to input
    /// change events so validation errors are cleared as the user types.
    pub fn new(
        id: impl Into<ElementId>,
        options: ZedisFormOptions,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let id = id.into();
        let fields = options.fields;
        let mut subscriptions = Vec::new();
        let mut field_states = Vec::with_capacity(fields.len());

        for field in &fields {
            let name = field.name.clone();
            match field.field_type {
                ZedisFormFieldType::Input | ZedisFormFieldType::InputNumber => {
                    let state = cx.new(|cx| {
                        InputState::new(window, cx)
                            .placeholder(field.placeholder.clone())
                            .masked(field.mask)
                    });
                    if let Some(default_value) = &field.default_value {
                        state.update(cx, |state, cx| {
                            state.set_value(default_value, window, cx);
                        });
                    }

                    // Clear validation errors when the user edits the field,
                    // and submit on Enter — a single-line affordance, which is
                    // why the two multi-line kinds below only watch `Change`.
                    let name_clone = name.clone();
                    subscriptions.push(cx.subscribe_in(&state, window, move |this, _state, event, window, cx| {
                        match event {
                            InputEvent::Change => {
                                this.on_value_change(name_clone.clone(), cx);
                            }
                            InputEvent::PressEnter { .. } if !this.in_dialog => {
                                this.submit(window, cx);
                            }
                            _ => {}
                        }
                    }));

                    // Handle increment/decrement steps for number inputs.
                    if field.field_type == ZedisFormFieldType::InputNumber {
                        subscriptions.push(cx.subscribe_in(&state, window, move |this, state, event, window, cx| {
                            let NumberInputEvent::Step(action) = event;
                            let value = state.read(cx).value().parse::<i64>().unwrap_or_default();
                            let new_val = match action {
                                StepAction::Increment => value.saturating_add(1),
                                StepAction::Decrement => value.saturating_sub(1),
                            };
                            if new_val != value {
                                state.update(cx, |state, cx| {
                                    state.set_value(new_val.to_string(), window, cx);
                                });
                            }
                            this.on_value_change(name.clone(), cx);
                        }));
                    }

                    field_states.push((field.clone(), ZedisFormFieldState::Input(state)));
                }
                ZedisFormFieldType::AutoGrow(min_rows, max_rows) => {
                    let state = cx.new(|cx| {
                        TextareaState::new(window, cx)
                            .placeholder(field.placeholder.clone())
                            .auto_grow(min_rows, max_rows)
                    });
                    if let Some(default_value) = &field.default_value {
                        state.update(cx, |state, cx| {
                            state.set_value(default_value, window, cx);
                        });
                    }
                    let name_clone = name.clone();
                    subscriptions.push(
                        cx.subscribe_in(&state, window, move |this, _state, event, _window, cx| {
                            if matches!(event, InputEvent::Change) {
                                this.on_value_change(name_clone.clone(), cx);
                            }
                        }),
                    );

                    field_states.push((field.clone(), ZedisFormFieldState::Textarea(state)));
                }
                ZedisFormFieldType::Editor => {
                    let state = cx.new(|cx| {
                        EditorState::new(window, cx)
                            .language(Language::from_str("json").name())
                            .placeholder(field.placeholder.clone())
                            .line_number(true)
                            .indent_guides(true)
                            .searchable(true)
                            .soft_wrap(true)
                    });
                    if let Some(default_value) = &field.default_value {
                        state.update(cx, |state, cx| {
                            state.set_value(default_value, window, cx);
                        });
                    }
                    let name_clone = name.clone();
                    subscriptions.push(
                        cx.subscribe_in(&state, window, move |this, _state, event, _window, cx| {
                            if matches!(event, InputEvent::Change) {
                                this.on_value_change(name_clone.clone(), cx);
                            }
                        }),
                    );

                    field_states.push((field.clone(), ZedisFormFieldState::Editor(state)));
                }
                ZedisFormFieldType::Checkbox => {
                    let default_value = field.default_value.as_ref().map(|v| v == "true").unwrap_or(false);
                    let state = cx.new(|_cx| default_value);
                    field_states.push((field.clone(), ZedisFormFieldState::Checkbox(state)));
                }
                ZedisFormFieldType::RadioGroup => {
                    let default_value = field
                        .default_value
                        .as_ref()
                        .and_then(|v| v.parse::<usize>().ok())
                        .unwrap_or(0);
                    let state = cx.new(|_cx| default_value);
                    field_states.push((field.clone(), ZedisFormFieldState::RadioGroup(state)));
                }
            }
        }

        let mut this = Self {
            id,
            field_states,
            errors: HashMap::new(),
            required_msg: options.required_error_msg,
            title: options.title,
            description: options.description,
            confirm_label: options.confirm_label,
            cancel_label: options.cancel_label,
            tabs: options.tabs,
            on_submit: options.on_submit,
            on_cancel: options.on_cancel,
            confirm_tooltip: options.confirm_tooltip,
            tab_selected_index: cx.new(|_cx| 0),
            should_focus: true,
            foot_actions: options.foot_actions,
            add_field_states: Vec::with_capacity(1),
            add_field_placeholder: options.add_field_placeholder,
            add_value_placeholder: options.add_value_placeholder,
            add_button_label: options.add_button_label,
            add_button_tooltip: options.add_button_tooltip,
            remove_button_tooltip: options.remove_button_tooltip,
            support_add_fields: options.support_add_fields,
            support_add_fields_on: options.support_add_fields_on,
            add_fields_mode: options.add_fields_mode,
            add_fields_modes_on: options.add_fields_modes_on,
            is_processing: false,
            disabled: false,
            in_dialog: false,
            fill_height: options.fill_height,
            pending_field_updates: Vec::new(),
            _subscriptions: subscriptions,
        };
        if this.support_add_fields {
            let initial_rows = this.active_add_fields_mode(cx).map_or(0, |mode| mode.initial_rows());
            for _ in 0..initial_rows {
                this.add_field(window, cx);
            }
        }
        this
    }

    /// Put the keyboard inside `tab_index`'s first text field.
    ///
    /// Switching tabs unmounts whatever was focused, and gpui does not hand
    /// the focus anywhere: it leaves the window. That is not only a lost
    /// caret — a dialog's Escape is dispatched along the *focus path*, so a
    /// form inside one stopped being dismissable the moment the user clicked
    /// a second tab. Focus therefore moves with the tab.
    ///
    /// Checkboxes and radio groups are skipped because `focus_at_end` does
    /// nothing for them, and arming a checkbox the user did not reach for
    /// would be a worse surprise than the one being fixed. A tab with no
    /// text field falls back to the form's always-visible fields, which is
    /// enough to keep the focus path — and the Escape — inside the dialog.
    fn focus_first_field_of_tab(&self, tab_index: usize, window: &mut Window, cx: &mut App) {
        let focusable = |only_this_tab: bool| {
            self.field_states.iter().find(|(field, state)| {
                let on_tab = if only_this_tab {
                    field.tab_index == Some(tab_index)
                } else {
                    field.tab_index.is_none()
                };
                on_tab
                    && self.should_render_field(field, tab_index, cx)
                    && !matches!(
                        state,
                        ZedisFormFieldState::RadioGroup(_) | ZedisFormFieldState::Checkbox(_)
                    )
            })
        };
        if let Some((_, state)) = focusable(true).or_else(|| focusable(false)) {
            state.focus_at_end(window, cx);
        }
    }

    /// Whether this field should be rendered in the current view.
    /// Checks both tab membership (`tab_index`) and conditional dependency
    /// (`visible_on`).
    fn should_render_field(&self, field: &ZedisFormField, active_tab_index: usize, cx: &App) -> bool {
        if let Some(tab_index) = field.tab_index
            && tab_index != active_tab_index
        {
            return false;
        }
        self.should_collect_field_value(field, cx)
    }

    /// Whether this field's value should be collected on form submission.
    /// Only checks `visible_on` / `visible_on_filled` — fields on inactive
    /// tabs (`tab_index`) are still submitted because tab switching is
    /// purely a UI concern.
    fn should_collect_field_value(&self, field: &ZedisFormField, cx: &App) -> bool {
        if let Some((ref radio_name, ref indices)) = field.visible_on {
            let selected = self.radio_group_selected(radio_name, cx);
            if !indices.contains(&selected) {
                return false;
            }
        }
        if let Some(ref input_name) = field.visible_on_filled
            && self.input_value(input_name, cx).trim().is_empty()
        {
            return false;
        }
        true
    }

    /// Returns the live value of a text-backed field by name (empty when the
    /// name doesn't resolve to one).
    ///
    /// `visible_on_filled` asks whether the user typed something, so the two
    /// widgets that always carry a value — a checkbox reads `"false"`, a radio
    /// group `"0"` — are deliberately not answered here.
    fn input_value(&self, name: &str, cx: &App) -> String {
        self.field_states
            .iter()
            .find_map(|(f, s)| {
                if f.name.as_ref() != name {
                    return None;
                }
                match s {
                    ZedisFormFieldState::Input(_)
                    | ZedisFormFieldState::Textarea(_)
                    | ZedisFormFieldState::Editor(_) => Some(s.value(cx).to_string()),
                    ZedisFormFieldState::RadioGroup(_) | ZedisFormFieldState::Checkbox(_) => None,
                }
            })
            .unwrap_or_default()
    }

    /// Returns the selected index of a RadioGroup field by name.
    fn radio_group_selected(&self, name: &str, cx: &App) -> usize {
        self.field_states
            .iter()
            .find_map(|(f, s)| {
                if f.name.as_ref() == name
                    && let ZedisFormFieldState::RadioGroup(state) = s
                {
                    return Some(*state.read(cx));
                }
                None
            })
            .unwrap_or(0)
    }

    /// Clear the validation error for a specific field when its value changes.
    fn on_value_change(&mut self, name: SharedString, cx: &mut Context<Self>) {
        self.errors.remove(&name);
        cx.notify();
    }

    fn cancel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(on_cancel) = &self.on_cancel {
            on_cancel(window, cx);
        }
    }

    pub fn try_get_values(&mut self, cx: &mut Context<Self>) -> Option<IndexMap<SharedString, SharedString>> {
        self.errors.clear();
        let mut has_errors = false;
        let mut values = IndexMap::new();

        for (field, state) in &self.field_states {
            if !self.should_collect_field_value(field, cx) {
                continue;
            }
            let value = submitted_value(state.value(cx).as_ref(), field.verbatim);

            if field.required && value.is_empty() {
                self.errors.insert(field.name.clone(), self.required_msg.clone());
                has_errors = true;
                continue;
            }

            if let Some(validate_fn) = &field.validate
                && let Some(err_msg) = validate_fn(&value)
            {
                self.errors.insert(field.name.clone(), err_msg);
                has_errors = true;
            }
            values.insert(field.name.clone(), value.into());
        }

        if has_errors {
            cx.notify();
            return None;
        }
        self.collect_add_field_values(&mut values, cx);
        Some(values)
    }

    /// Validate all fields, collect their values, and invoke the submit handler.
    /// Runs required-checks first, then custom validators per field.
    pub fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_processing {
            return;
        }
        let Some(values) = self.try_get_values(cx) else {
            return;
        };
        let Some(on_submit) = &self.on_submit else {
            return;
        };
        if on_submit(values, window, cx) {
            self.is_processing = true;
            cx.notify();
        }
    }

    /// Cancel / confirm / custom foot actions.
    ///
    /// Shared by inline forms and dialog footers so the action row can live
    /// outside a scroll container when the form is hosted in a Dialog.
    pub(super) fn render_action_bar(&self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let parent_id = Arc::new(self.id.clone());
        let buttons_disabled = self.disabled || self.is_processing;
        let mut buttons = Vec::with_capacity(2);
        if self.on_cancel.is_some() {
            let button_id = ElementId::NamedChild(parent_id.clone(), "cancel".into());
            buttons.push(
                Button::new(button_id)
                    .label(self.cancel_label.clone())
                    .disabled(buttons_disabled)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.cancel(window, cx);
                    })),
            );
        }
        if self.on_submit.is_some() {
            let button_id = ElementId::NamedChild(parent_id.clone(), "confirm".into());
            buttons.push(
                Button::new(button_id)
                    .label(self.confirm_label.clone())
                    .disabled(buttons_disabled)
                    .when_some(self.confirm_tooltip.clone(), |this, tooltip| this.tooltip(tooltip))
                    .primary()
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.submit(window, cx);
                    })),
            );
        }

        let buttons = platform_buttons(buttons);
        let mut right_buttons = h_flex().justify_end().gap_2();
        let mut left_buttons = h_flex().justify_start().gap_2();

        let mut exists_buttons = false;
        if !buttons.is_empty() {
            right_buttons = right_buttons.children(buttons);
            exists_buttons = true;
        }
        if let Some(builder) = &self.foot_actions {
            left_buttons = left_buttons.children(builder(window, cx));
            exists_buttons = true;
        }
        if !exists_buttons {
            return None;
        }
        Some(
            h_flex()
                .w_full()
                .justify_between()
                .child(left_buttons)
                .child(right_buttons)
                .gap_4()
                .into_any_element(),
        )
    }

    /// Disable or enable all form inputs and buttons.
    pub fn set_disabled(&mut self, disabled: bool, cx: &mut Context<Self>) {
        self.disabled = disabled;
        cx.notify();
    }

    /// Set the value of a static field by name.
    pub fn set_field_value(
        &mut self,
        name: &str,
        value: impl Into<SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let value = value.into();
        let mut synced_radio = None;
        for (field, state) in &self.field_states {
            if field.name.as_ref() == name {
                state.set_value(&value, window, cx);
                if matches!(state, ZedisFormFieldState::RadioGroup(_)) {
                    synced_radio = Some(field.name.clone());
                }
                break;
            }
        }
        if let Some(radio_name) = synced_radio {
            self.sync_add_fields_for_radio(radio_name.as_ref(), window, cx);
        }
        cx.notify();
    }

    pub fn reset_form(
        &mut self,
        values: &IndexMap<SharedString, SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        for (field, state) in &self.field_states {
            if let Some(value) = values.get(&field.name) {
                state.set_value(value, window, cx);
            }
        }
        self.should_focus = true;
    }

    /// Read the current value of a field by name without triggering validation.
    pub fn get_field_value(&self, name: &str, cx: &App) -> SharedString {
        self.field_states
            .iter()
            .find_map(|(f, s)| {
                if f.name.as_ref() != name {
                    return None;
                }
                Some(s.value(cx))
            })
            .unwrap_or_default()
    }

    /// Queue a field value update to be applied on the next render (when `Window` is available).
    pub fn schedule_field_update(&mut self, name: SharedString, value: SharedString) {
        self.pending_field_updates.push((name, value));
    }
}

impl Render for ZedisForm {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Apply any deferred field updates now that we have access to `Window`.
        for (name, value) in std::mem::take(&mut self.pending_field_updates) {
            for (field, state) in &self.field_states {
                if field.name == name {
                    state.set_value(&value, window, cx);
                    break;
                }
            }
        }

        // Auto-focus the designated field on the first render, then clear the flag.
        if take(&mut self.should_focus) {
            for (field, state) in &self.field_states {
                if field.focus {
                    state.focus_at_end(window, cx);
                    break;
                }
            }
        }

        // The fields are collected first: the form grid in a scroll
        // container is the usual home, a flex column that fills the parent
        // the fill-height one, and both take the same items.
        let mut items: Vec<FormItem> = Vec::new();
        if let Some(title) = self.title.clone() {
            items.push(FormItem::field(
                row().child(Label::new(title).text_lg().font_semibold()),
            ));
        }
        if let Some(description) = self.description.clone() {
            items.push(FormItem::field(
                row().child(
                    Label::new(description)
                        .text_sm()
                        .text_color(cx.theme().muted_foreground),
                ),
            ));
        }
        let parent_id = Arc::new(self.id.clone());

        // Render optional tab bar for multi-tab forms.
        if let Some(tabs) = &self.tabs {
            let tab_selected_index = self.tab_selected_index.clone();
            let tab_bar_id = ElementId::NamedChild(parent_id.clone(), "tab-bar".into());
            let mut tab_bar = TabBar::new(tab_bar_id)
                .underline()
                .mb_3()
                .selected_index(*tab_selected_index.read(cx))
                // A listener rather than a plain closure: the handler needs
                // the form itself to move focus onto the new tab (see
                // `focus_first_field_of_tab`).
                .on_click(cx.listener(move |this, selected_index: &usize, window, cx| {
                    tab_selected_index.update(cx, |state, cx| {
                        *state = *selected_index;
                        cx.notify();
                    });
                    this.focus_first_field_of_tab(*selected_index, window, cx);
                }));
            for tab in tabs {
                tab_bar = tab_bar.child(Tab::new().label(tab.clone()));
            }
            items.push(FormItem::field(row().child(tab_bar)));
        }

        let new_field = |item: &ZedisFormField| {
            field()
                .col_span(item.span.unwrap_or(FORM_COLUMNS))
                .required(item.required)
                .label(item.label.clone())
        };

        // Read the active tab index once to avoid repeated entity reads inside the loop.
        let active_tab_index = *self.tab_selected_index.read(cx);

        let form_disabled = self.disabled;

        for (index, (field, field_state)) in self.field_states.iter().enumerate() {
            if !self.should_render_field(field, active_tab_index, cx) {
                continue;
            }

            let field_disabled = form_disabled || field.readonly;

            match field_state {
                ZedisFormFieldState::Input(state) => {
                    if field.field_type == ZedisFormFieldType::InputNumber {
                        items.push(FormItem::field(
                            new_field(field).child(NumberInput::new(state).disabled(field_disabled)),
                        ));
                    } else {
                        let mut input = Input::new(state)
                            .disabled(field_disabled)
                            .when(field.mask, |this| this.mask_toggle())
                            .refine_style(&field.style);
                        if let Some(builder) = &field.suffix_builder {
                            input = input.suffix(builder(window, cx));
                        }
                        if let Some(companion) = field.paste_pair_with.clone() {
                            let field_name = field.name.clone();
                            let form_entity = cx.entity().clone();
                            input = input.on_paste(move |clipboard, window, cx| {
                                let Some(text) = clipboard.text() else {
                                    return false;
                                };
                                form_entity.update(cx, |this, cx| {
                                    this.paste_static_pair_lines(
                                        field_name.as_ref(),
                                        companion.as_ref(),
                                        false,
                                        &text,
                                        window,
                                        cx,
                                    )
                                })
                            });
                        } else if field.paste_single_lines {
                            let field_name = field.name.clone();
                            let form_entity = cx.entity().clone();
                            input = input.on_paste(move |clipboard, window, cx| {
                                let Some(text) = clipboard.text() else {
                                    return false;
                                };
                                form_entity.update(cx, |this, cx| {
                                    this.paste_static_single_lines(field_name.as_ref(), &text, window, cx)
                                })
                            });
                        }
                        items.push(FormItem::field(new_field(field).child(input)));
                    }
                }
                ZedisFormFieldState::Textarea(state) => {
                    // `mask` and `suffix` are single-line adornments and have
                    // no counterpart here — gpui-component keeps them on
                    // `Input` alone.
                    let mut textarea = Textarea::new(state).disabled(field_disabled).refine_style(&field.style);
                    if let Some(companion) = field.paste_pair_with.clone() {
                        let field_name = field.name.clone();
                        let form_entity = cx.entity().clone();
                        textarea = textarea.on_paste(move |clipboard, window, cx| {
                            let Some(text) = clipboard.text() else {
                                return false;
                            };
                            form_entity.update(cx, |this, cx| {
                                this.paste_static_pair_lines(
                                    field_name.as_ref(),
                                    companion.as_ref(),
                                    true,
                                    &text,
                                    window,
                                    cx,
                                )
                            })
                        });
                    } else if field.paste_single_lines {
                        let field_name = field.name.clone();
                        let form_entity = cx.entity().clone();
                        textarea = textarea.on_paste(move |clipboard, window, cx| {
                            let Some(text) = clipboard.text() else {
                                return false;
                            };
                            form_entity.update(cx, |this, cx| {
                                this.paste_static_single_lines(field_name.as_ref(), &text, window, cx)
                            })
                        });
                    }
                    items.push(FormItem::field(new_field(field).child(textarea)));
                }
                ZedisFormFieldState::Editor(state) => {
                    let editor = Editor::new(state).disabled(field_disabled);
                    if self.fill_height && field.fill {
                        let control = editor.flex_1().min_h_0().w_full().into_any_element();
                        items.push(FormItem::Fill(fill_field(field, control, cx)));
                    } else {
                        items.push(FormItem::field(
                            new_field(field).child(editor.refine_style(&field.style)),
                        ));
                    }
                }
                ZedisFormFieldState::Checkbox(state) => {
                    let id = ElementId::NamedChild(parent_id.clone(), index.to_string().into());
                    let state_clone = state.clone();
                    items.push(FormItem::field(
                        new_field(field).child(
                            Checkbox::new(id)
                                .label(field.placeholder.clone())
                                .checked(*state.read(cx))
                                .disabled(field_disabled)
                                .on_click(move |check, _, cx| {
                                    state_clone.update(cx, |state, _| {
                                        *state = *check;
                                    });
                                }),
                        ),
                    ));
                }
                ZedisFormFieldState::RadioGroup(state) => {
                    let id = ElementId::NamedChild(parent_id.clone(), index.to_string().into());
                    let state = state.clone();
                    let selected = *state.read(cx);
                    let form_entity = cx.entity().clone();
                    let radio_name = field.name.clone();
                    items.push(FormItem::field(
                        new_field(field).child(
                            if field.stacked {
                                RadioGroup::vertical(id)
                            } else {
                                RadioGroup::horizontal(id)
                            }
                            .children(field.options.clone().unwrap_or_default())
                            .selected_index(Some(selected))
                            .disabled(field_disabled)
                            .on_click(move |index, window, cx| {
                                state.update(cx, |state, _| {
                                    *state = *index;
                                });
                                let radio_name = radio_name.clone();
                                form_entity.update(cx, |this, cx| {
                                    this.sync_add_fields_for_radio(radio_name.as_ref(), window, cx);
                                    cx.notify();
                                });
                            }),
                        ),
                    ));
                }
            }
        }

        for add_item in self.render_add_fields_section(form_disabled, cx) {
            items.push(FormItem::field(add_item));
        }

        // Render validation errors as a markdown alert.
        if !self.errors.is_empty() {
            let alert_id = ElementId::NamedChild(parent_id.clone(), "alert".into());
            let textview_id = ElementId::NamedChild(parent_id.clone(), "textview".into());
            let error_text = self
                .errors
                .iter()
                .map(|(name, value)| format!("- {name}: {value}"))
                .collect::<Vec<_>>()
                .join("\n");
            items.push(FormItem::field(
                row().child(Alert::error(alert_id, TextView::markdown(textview_id, error_text))),
            ));
        }

        // Inline forms keep the action bar in-body. Dialog forms hoist it to
        // the Dialog footer (see `open_dialog`) so it stays pinned while the
        // field list scrolls.
        if !self.in_dialog
            && let Some(bar) = self.render_action_bar(window, cx)
        {
            items.push(FormItem::field(row().child(bar)));
        }

        // Fill mode: a flex column the parent sizes, every field at its own
        // height except the one that takes the rest — and no scroll
        // container, which is what would make that impossible.
        if self.fill_height {
            return v_flex()
                .size_full()
                .gap_y(px(8.))
                .children(items.into_iter().map(|item| match item {
                    FormItem::Field(field) => (*field).flex_none().into_any_element(),
                    FormItem::Fill(element) => element,
                }))
                .into_any_element();
        }
        let mut form_container = v_form().columns(usize::from(FORM_COLUMNS)).w_full().gap_2();
        for item in items {
            form_container = match item {
                FormItem::Field(field) => form_container.child(*field),
                FormItem::Fill(element) => form_container.child(row().child(element)),
            };
        }

        // Dialog already owns the body scrollbar (gpui-component `Dialog`
        // wraps children in `overflow_y_scrollbar`). Nesting another one
        // here — especially under a `max_h` parent — leaves a non-scrolling
        // side track. Standalone (non-dialog) forms still need their own.
        if self.in_dialog {
            form_container.into_any_element()
        } else {
            div()
                .size_full()
                .overflow_y_scrollbar()
                .child(form_container)
                .into_any_element()
        }
    }
}

/// The form's grid: a field takes the whole row unless it asks for a share
/// of it ([`ZedisFormField::span`]).
pub(super) const FORM_COLUMNS: u16 = 12;

/// A form item that takes the whole row — everything that is not a spanned
/// field: the title, the tab bar, an error, the add-field rows.
fn row() -> Field {
    field().col_span(FORM_COLUMNS)
}

/// The least a fill field keeps when its parent is short.
const FILL_FIELD_MIN_HEIGHT: f32 = 96.0;

/// One entry of a form's body: a field of the grid, or the element that
/// takes the height left in fill mode.
enum FormItem {
    Field(Box<Field>),
    Fill(AnyElement),
}

impl FormItem {
    fn field(field: Field) -> Self {
        FormItem::Field(Box::new(field))
    }
}

/// A field that takes the height the form has left: the label the other
/// fields wear, over a control that flexes.
fn fill_field(field: &ZedisFormField, control: AnyElement, cx: &App) -> AnyElement {
    v_flex()
        .flex_1()
        // Shrinks with the parent, but not below a few lines of editor.
        .min_h(px(FILL_FIELD_MIN_HEIGHT))
        .w_full()
        .gap(px(2.))
        .child(
            h_flex()
                .gap_1()
                .items_center()
                .text_sm()
                .font_weight(FontWeight::MEDIUM)
                .child(field.label.clone())
                .when(field.required, |this| {
                    this.child(div().text_color(cx.theme().danger).child("*"))
                }),
        )
        .child(v_flex().flex_1().min_h_0().w_full().child(control))
        .into_any_element()
}

/// The text a field submits: trimmed, unless the field is verbatim.
fn submitted_value(raw: &str, verbatim: bool) -> String {
    if verbatim {
        raw.to_string()
    } else {
        raw.trim().to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::submitted_value;

    #[test]
    fn only_a_verbatim_field_keeps_its_surrounding_whitespace() {
        // A setting or a number is trimmed, as it always was.
        assert_eq!(submitted_value("  6379 \n", false), "6379");
        // A value is data: an indented YAML line and its final newline stay.
        assert_eq!(submitted_value("  a: 1\n", true), "  a: 1\n");
        assert_eq!(submitted_value("name ", true), "name ");
    }
}
