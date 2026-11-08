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

use gpui::{
    AnyElement, App, ElementId, Entity, FontWeight, Pixels, Render, SharedString, StyleRefinement, Subscription,
    Window, div, prelude::*, px,
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
use gpui_kit::component::{ActiveTheme, Disableable, IconName, StyledExt, WindowExt, h_flex};
use indexmap::IndexMap;
use std::collections::HashMap;
use std::mem::take;
use std::rc::Rc;

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
use std::sync::Arc;

/// Callback invoked on form submission with all field values collected as a map.
/// Returns `true` if the submission was handled successfully.
type ZedisFormSubmitHandler =
    Rc<dyn Fn(IndexMap<SharedString, SharedString>, &mut Window, &mut Context<ZedisForm>) -> bool + 'static>;

/// Per-field validation callback. Returns `Some(error_message)` on failure, `None` on success.
type ZedisFormValidateHandler = Rc<dyn Fn(&str) -> Option<SharedString> + 'static>;

/// Callback invoked when the cancel button is clicked.
/// Returns `true` if the cancellation was handled.
type ZedisFormCancelHandler = Rc<dyn Fn(&mut Window, &mut Context<ZedisForm>) -> bool + 'static>;

/// Callback invoked to build the action buttons for the footer.
pub type ZedisFormActionsBuilder = Rc<dyn Fn(&mut Window, &mut Context<ZedisForm>) -> Vec<AnyElement>>;

/// Callback invoked to build the suffix element for an input field.
type ZedisFormFieldSuffixBuilder = Rc<dyn Fn(&mut Window, &mut Context<ZedisForm>) -> AnyElement>;

/// Supported field widget types for the form builder.
#[derive(Clone, Default, PartialEq, Debug)]
pub enum ZedisFormFieldType {
    #[default]
    Input,
    InputNumber,
    RadioGroup,
    Checkbox,
    /// Auto-growing text area with `(min_rows, max_rows)`.
    AutoGrow(usize, usize),
    Editor,
}

/// Declarative field descriptor used to configure a form field before the
/// form entity is created. Uses the builder pattern for ergonomic construction.
#[derive(Clone)]
pub struct ZedisFormField {
    style: StyleRefinement,
    /// Takes the height the form has left instead of a height of its own —
    /// the one editor of a form in [`ZedisFormOptions::fill_height`] mode.
    fill: bool,
    name: SharedString,
    label: SharedString,
    placeholder: SharedString,
    /// Assigns this field to a specific tab (display-only).
    /// The field is hidden when another tab is active, but its value is
    /// **always** included on form submission regardless of the active tab.
    tab_index: Option<usize>,
    default_value: Option<SharedString>,
    field_type: ZedisFormFieldType,
    /// Options list for `RadioGroup` fields.
    options: Option<Vec<SharedString>>,
    validate: Option<ZedisFormValidateHandler>,
    mask: bool,
    required: bool,
    /// Whether this field should receive focus on the first render.
    focus: bool,
    readonly: bool,
    /// Makes this field conditionally dependent on a RadioGroup's selection.
    /// Unlike `tab_index`, when the condition is **not** met the field is both
    /// hidden from the UI **and** excluded from the submitted values.
    visible_on: Option<(SharedString, Vec<usize>)>,
    /// Makes this field conditionally dependent on an Input-backed field
    /// holding a non-blank value (e.g. a key passphrase that is meaningless
    /// without a key). Same semantics as `visible_on`: hidden and excluded
    /// from submission while the condition is not met.
    visible_on_filled: Option<SharedString>,
    suffix_builder: Option<ZedisFormFieldSuffixBuilder>,
    /// Submit the text exactly as typed. Off by default, where the form
    /// trims — right for settings, names and numbers, wrong for a field
    /// whose text *is* data (a Redis value, a member, a hash field name).
    verbatim: bool,
    /// Lay a `RadioGroup`'s options out one per line.
    stacked: bool,
    /// How many of the form's twelve columns the field takes; the whole row
    /// when unset.
    span: Option<u16>,
    /// Optional companion field name when this field is the first column of a
    /// collection pair (e.g. Hash `Field` + `Value` or ZSet `Member` + `Score`),
    /// allowing multi-line or tab-separated pastes to fill this pair and
    /// overflow remaining lines into dynamic (`+`) rows.
    paste_pair_with: Option<SharedString>,
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

impl ZedisFormField {
    /// Create a new field descriptor with the given internal name and display label.
    pub fn new(name: impl Into<SharedString>, label: impl Into<SharedString>) -> Self {
        Self {
            name: name.into(),
            label: label.into(),
            placeholder: SharedString::default(),
            default_value: None,
            field_type: ZedisFormFieldType::Input,
            options: None,
            validate: None,
            tab_index: None,
            required: false,
            focus: false,
            mask: false,
            readonly: false,
            visible_on: None,
            visible_on_filled: None,
            style: StyleRefinement::default(),
            fill: false,
            suffix_builder: None,
            verbatim: false,
            stacked: false,
            span: None,
            paste_pair_with: None,
        }
    }

    /// Set the placeholder text shown when the field is empty.
    pub fn placeholder(mut self, text: impl Into<SharedString>) -> Self {
        self.placeholder = text.into();
        self
    }

    /// Set the widget type for this field (defaults to `Input`).
    pub fn field_type(mut self, ty: ZedisFormFieldType) -> Self {
        self.field_type = ty;
        self
    }

    /// Mark the field as required; empty values will trigger a validation error.
    pub fn required(mut self) -> Self {
        self.required = true;
        self
    }

    /// Set the list of options for `RadioGroup` fields.
    pub fn options(mut self, options: Vec<SharedString>) -> Self {
        self.options = Some(options);
        self
    }

    /// Attach a custom validation function to this field.
    pub fn validate(mut self, validate: impl Fn(&str) -> Option<SharedString> + 'static) -> Self {
        self.validate = Some(Rc::new(validate));
        self
    }

    /// Set the initial value for this field.
    pub fn default_value(mut self, value: impl Into<SharedString>) -> Self {
        self.default_value = Some(value.into());
        self
    }

    /// Assign this field to a specific tab index for multi-tab forms.
    pub fn tab_index(mut self, index: usize) -> Self {
        self.tab_index = Some(index);
        self
    }

    /// Enable password masking on this field.
    pub fn mask(mut self) -> Self {
        self.mask = true;
        self
    }

    /// Submit this field's text as typed, without trimming: its leading and
    /// trailing whitespace is part of the data. `required` then rejects only
    /// a truly empty value, since a lone space is a value too.
    pub fn verbatim(mut self) -> Self {
        self.verbatim = true;
        self
    }

    /// Request that this field receives keyboard focus on the first render.
    pub fn focus(mut self) -> Self {
        self.focus = true;
        self
    }

    /// Mark this field as read-only (renders the widget as disabled).
    pub fn readonly(mut self) -> Self {
        self.readonly = true;
        self
    }

    /// Share a row: take `columns` of the form's twelve, so this field and
    /// the next sit side by side — a host and its port, 9 and 3. Fields are
    /// placed in order, and one that does not fit starts the next row.
    pub fn span(mut self, columns: u16) -> Self {
        self.span = Some(columns.clamp(1, FORM_COLUMNS));
        self
    }

    /// Lay a `RadioGroup`'s options out one per line instead of in a row.
    /// For options that are phrases: a row of those wraps, and four
    /// exclusive choices broken over two lines read as two groups of two.
    pub fn stacked(mut self) -> Self {
        self.stacked = true;
        self
    }

    /// Make this field visible only when the RadioGroup with the given name
    /// has its selected index in `indices`.
    pub fn visible_on(mut self, name: impl Into<SharedString>, indices: &[usize]) -> Self {
        self.visible_on = Some((name.into(), indices.to_vec()));
        self
    }

    /// Make this field visible only while the Input-backed field with the
    /// given name holds a non-blank value. Re-evaluated live as the user
    /// types (the form re-renders on every input change); while hidden the
    /// field is also excluded from the submitted values.
    pub fn visible_when_filled(mut self, name: impl Into<SharedString>) -> Self {
        self.visible_on_filled = Some(name.into());
        self
    }

    /// Attach a suffix element to the input (e.g. an icon action button).
    /// The builder receives `Context<ZedisForm>` so it can create listeners.
    /// Only applies to `Input`-type fields.
    pub fn suffix<F, E>(mut self, builder: F) -> Self
    where
        F: Fn(&mut Window, &mut Context<ZedisForm>) -> E + 'static,
        E: IntoElement,
    {
        self.suffix_builder = Some(Rc::new(move |window, cx| builder(window, cx).into_any_element()));
        self
    }
}

impl ZedisFormField {
    /// Let this field take the height the form has left. Only an inline
    /// form in [`ZedisFormOptions::fill_height`] mode honours it; elsewhere
    /// the field keeps the height its style says.
    pub fn fill(mut self) -> Self {
        self.fill = true;
        self
    }

    /// Pair this field with `companion` so pasting multi-line or tab-separated
    /// pairs (`left\tright`, `left=right`, `left: right`, `left right`) into
    /// this field fills `(self, companion)` with the first line and distributes
    /// remaining lines across dynamic (`+`) rows.
    pub fn paste_pair_with(mut self, companion: impl Into<SharedString>) -> Self {
        self.paste_pair_with = Some(companion.into());
        self
    }
}

impl Styled for ZedisFormField {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl gpui::prelude::FluentBuilder for ZedisFormField {}

/// Key prefix used in the submitted `IndexMap` for single-column dynamic add-field rows.
pub const EXTRA_SINGLE_ROW_PREFIX: &str = "__extra_single_row_";

/// Key prefix used in the submitted `IndexMap` for two-column dynamic add-field rows
/// whose first column is empty or collides with an earlier key in the map.
pub const EXTRA_PAIR_DUP_PREFIX: &str = "__extra_pair_dup_";

/// Shape and placeholders for dynamic (`+`) rows in [`ZedisForm`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ZedisFormAddFieldsMode {
    /// One input per dynamic row (e.g. List value, Set member).
    Single {
        placeholder: SharedString,
        initial_rows: usize,
    },
    /// Two side-by-side inputs per dynamic row (e.g. Hash field+value, Zset score+member, Stream field+value).
    Pair {
        field_placeholder: SharedString,
        value_placeholder: SharedString,
        initial_rows: usize,
    },
}

impl ZedisFormAddFieldsMode {
    /// Construct a single-column dynamic row mode.
    pub fn single(placeholder: impl Into<SharedString>, initial_rows: usize) -> Self {
        Self::Single {
            placeholder: placeholder.into(),
            initial_rows,
        }
    }

    /// Construct a two-column dynamic row mode.
    pub fn pair(
        field_placeholder: impl Into<SharedString>,
        value_placeholder: impl Into<SharedString>,
        initial_rows: usize,
    ) -> Self {
        Self::Pair {
            field_placeholder: field_placeholder.into(),
            value_placeholder: value_placeholder.into(),
            initial_rows,
        }
    }

    fn initial_rows(&self) -> usize {
        match self {
            Self::Single { initial_rows, .. } | Self::Pair { initial_rows, .. } => *initial_rows,
        }
    }

    fn is_single(&self) -> bool {
        matches!(self, Self::Single { .. })
    }
}

/// Extract all single-column dynamic row values from a submitted form map, in row order.
pub fn extra_single_rows(values: &IndexMap<SharedString, SharedString>) -> Vec<SharedString> {
    values
        .iter()
        .filter(|(k, _)| k.starts_with(EXTRA_SINGLE_ROW_PREFIX))
        .map(|(_, v)| v.clone())
        .collect()
}

/// Decode a dynamic two-column key if it was disambiguated with [`EXTRA_PAIR_DUP_PREFIX`].
pub fn decode_extra_pair_key(key: &str) -> SharedString {
    if let Some(rest) = key.strip_prefix(EXTRA_PAIR_DUP_PREFIX)
        && let Some((_, raw_field)) = rest.split_once(':')
    {
        return SharedString::from(raw_field.to_string());
    }
    SharedString::from(key.to_string())
}

/// Extract all two-column dynamic `(field, value)` rows from a submitted form map,
/// skipping any static form field names listed in `static_keys` and any single-column rows.
pub fn extra_pair_rows(
    values: &IndexMap<SharedString, SharedString>,
    static_keys: &[&str],
) -> Vec<(SharedString, SharedString)> {
    values
        .iter()
        .filter_map(|(k, v)| {
            if k.starts_with(EXTRA_SINGLE_ROW_PREFIX) || static_keys.contains(&k.as_ref()) {
                return None;
            }
            let field = decode_extra_pair_key(k.as_ref());
            Some((field, v.clone()))
        })
        .collect()
}

/// Configuration for constructing a [`ZedisForm`]. Collects field descriptors,
/// tab labels, button labels, and event handlers before entity creation.
pub struct ZedisFormOptions {
    title: Option<SharedString>,
    description: Option<SharedString>,
    tabs: Option<Vec<SharedString>>,
    fields: Vec<ZedisFormField>,
    required_error_msg: SharedString,
    confirm_label: SharedString,
    confirm_tooltip: Option<SharedString>,
    cancel_label: SharedString,
    add_field_placeholder: SharedString,
    add_value_placeholder: SharedString,
    on_submit: Option<ZedisFormSubmitHandler>,
    on_cancel: Option<ZedisFormCancelHandler>,
    foot_actions: Option<ZedisFormActionsBuilder>,
    dialog_submit: Option<ZedisFormDialogSubmitHandler>,
    dialog_width: Option<Pixels>,
    dialog_max_height: Option<Pixels>,
    support_add_fields: bool,
    /// When set, the add-fields section is only shown when the referenced
    /// RadioGroup's selected index is in the given list.
    support_add_fields_on: Option<(SharedString, Vec<usize>)>,
    add_fields_mode: Option<ZedisFormAddFieldsMode>,
    add_fields_modes_on: Option<(SharedString, Vec<(usize, ZedisFormAddFieldsMode)>)>,
    /// Lay the fields out as a flex column that fills the parent, with the
    /// [`ZedisFormField::fill`] field taking what is left, instead of the
    /// form grid in a scroll container. For an inline form whose parent
    /// has a definite height — an entry panel — where a field should end
    /// exactly at the bottom without anyone computing its height.
    fill_height: bool,
}

impl Default for ZedisFormOptions {
    fn default() -> Self {
        Self {
            tabs: None,
            title: None,
            description: None,
            fields: Vec::new(),
            required_error_msg: "Required".into(),
            confirm_tooltip: None,
            confirm_label: "Confirm".into(),
            cancel_label: "Cancel".into(),
            add_field_placeholder: "Enter field".into(),
            add_value_placeholder: "Enter value".into(),
            on_submit: None,
            on_cancel: None,
            foot_actions: None,
            dialog_submit: None,
            dialog_width: None,
            dialog_max_height: None,
            support_add_fields: false,
            support_add_fields_on: None,
            add_fields_mode: None,
            add_fields_modes_on: None,
            fill_height: false,
        }
    }
}

impl ZedisFormOptions {
    /// Create form options from a list of field descriptors.
    pub fn new(fields: Vec<ZedisFormField>) -> Self {
        Self {
            fields,
            ..Default::default()
        }
    }

    /// Set the tab labels for a multi-tab form layout.
    pub fn tabs(mut self, tabs: Vec<SharedString>) -> Self {
        self.tabs = Some(tabs);
        self
    }

    /// Override the default "Required" validation error message.
    pub fn required_error_msg(mut self, msg: impl Into<SharedString>) -> Self {
        self.required_error_msg = msg.into();
        self
    }

    /// Set the label for the confirm/submit button.
    pub fn confirm_label(mut self, label: impl Into<SharedString>) -> Self {
        self.confirm_label = label.into();
        self
    }

    /// Set the tooltip for the confirm/submit button.
    pub fn confirm_tooltip(mut self, tooltip: impl Into<SharedString>) -> Self {
        self.confirm_tooltip = Some(tooltip.into());
        self
    }

    /// Set the label for the cancel button.
    pub fn cancel_label(mut self, label: impl Into<SharedString>) -> Self {
        self.cancel_label = label.into();
        self
    }

    /// Set the title of the form.
    pub fn title(mut self, title: impl Into<SharedString>) -> Self {
        self.title = Some(title.into());
        self
    }

    /// Set the description of the form.
    pub fn description(mut self, description: impl Into<SharedString>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// Attach a submit handler that receives all field values on form submission.
    pub fn on_submit(
        mut self,
        on_submit: impl Fn(IndexMap<SharedString, SharedString>, &mut Window, &mut Context<ZedisForm>) -> bool + 'static,
    ) -> Self {
        self.on_submit = Some(Rc::new(on_submit));
        self
    }

    /// Attach a cancel handler invoked when the cancel button is clicked.
    pub fn on_cancel(mut self, on_cancel: impl Fn(&mut Window, &mut Context<ZedisForm>) -> bool + 'static) -> Self {
        self.on_cancel = Some(Rc::new(on_cancel));
        self
    }

    /// Support adding fields to the form.
    pub fn fill_height(mut self) -> Self {
        self.fill_height = true;
        self
    }

    pub fn support_add_fields(mut self) -> Self {
        self.support_add_fields = true;
        self
    }

    /// Conditionally show the add-fields section only when the referenced
    /// RadioGroup's selected index is in `indices`.
    pub fn support_add_fields_on(mut self, name: impl Into<SharedString>, indices: &[usize]) -> Self {
        self.support_add_fields = true;
        self.support_add_fields_on = Some((name.into(), indices.to_vec()));
        self
    }

    /// Enable single-column dynamic add-field rows with the given placeholder.
    pub fn support_add_single_fields(mut self, placeholder: impl Into<SharedString>, initial_rows: usize) -> Self {
        let placeholder: SharedString = placeholder.into();
        self.support_add_fields = true;
        self.add_value_placeholder = placeholder.clone();
        self.add_fields_mode = Some(ZedisFormAddFieldsMode::Single {
            placeholder,
            initial_rows,
        });
        self
    }

    /// Enable two-column dynamic add-field rows with the given placeholders.
    pub fn support_add_pair_fields(
        mut self,
        field_placeholder: impl Into<SharedString>,
        value_placeholder: impl Into<SharedString>,
        initial_rows: usize,
    ) -> Self {
        let field_placeholder: SharedString = field_placeholder.into();
        let value_placeholder: SharedString = value_placeholder.into();
        self.support_add_fields = true;
        self.add_field_placeholder = field_placeholder.clone();
        self.add_value_placeholder = value_placeholder.clone();
        self.add_fields_mode = Some(ZedisFormAddFieldsMode::Pair {
            field_placeholder,
            value_placeholder,
            initial_rows,
        });
        self
    }

    /// Configure a dynamic add-fields mode for a specific index of a `RadioGroup`.
    pub fn support_add_fields_mode_on(
        mut self,
        name: impl Into<SharedString>,
        index: usize,
        mode: ZedisFormAddFieldsMode,
    ) -> Self {
        let name: SharedString = name.into();
        self.support_add_fields = true;
        match &mut self.support_add_fields_on {
            Some((_, indices)) => {
                if !indices.contains(&index) {
                    indices.push(index);
                }
            }
            None => {
                self.support_add_fields_on = Some((name.clone(), vec![index]));
            }
        }
        match &mut self.add_fields_modes_on {
            Some((_, modes)) => {
                if let Some((_, existing)) = modes.iter_mut().find(|(i, _)| *i == index) {
                    *existing = mode;
                } else {
                    modes.push((index, mode));
                }
            }
            None => {
                self.add_fields_modes_on = Some((name, vec![(index, mode)]));
            }
        }
        self
    }

    /// Set the placeholder for the add field input.
    pub fn add_field_placeholder(mut self, placeholder: impl Into<SharedString>) -> Self {
        self.add_field_placeholder = placeholder.into();
        self
    }

    /// Set the placeholder for the add value input.
    pub fn add_value_placeholder(mut self, placeholder: impl Into<SharedString>) -> Self {
        self.add_value_placeholder = placeholder.into();
        self
    }

    /// Set the action buttons for the footer.
    pub fn foot_actions<F, I>(mut self, builder: F) -> Self
    where
        F: Fn(&mut Window, &mut Context<ZedisForm>) -> I + 'static,
        I: IntoIterator,
        I::Item: IntoElement,
    {
        self.foot_actions = Some(Rc::new(move |window, cx| {
            builder(window, cx).into_iter().map(|e| e.into_any_element()).collect()
        }));
        self
    }
}

impl gpui::prelude::FluentBuilder for ZedisFormOptions {}

/// Handler closure invoked on dialog form submission.
/// Receives field values as an `IndexMap`, and should return `true` if the
/// submission was handled successfully (the dialog will be closed automatically).
type ZedisFormDialogSubmitHandler =
    Rc<dyn Fn(IndexMap<SharedString, SharedString>, &mut Window, &mut App) -> bool + 'static>;

impl ZedisFormOptions {
    /// Opens this form inside a modal dialog overlay.
    ///
    /// This is a convenience method that wraps the form in a `window.open_dialog`
    /// call, automatically handling dialog close on submit/cancel.
    ///
    /// The submit handler receives `IndexMap<SharedString, SharedString>` and
    /// `&mut App` (not `&mut Context<ZedisForm>`), making it easier to use from
    /// any context without needing a reference to the form entity.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// ZedisFormOptions::new(vec![
    ///     ZedisFormField::new("name", "Name").placeholder("Enter name").focus(),
    ///     ZedisFormField::new("age", "Age").field_type(ZedisFormFieldType::InputNumber),
    /// ])
    /// .title("Add User")
    /// .on_dialog_submit(move |values, window, cx| {
    ///     let name = values.get("name").cloned().unwrap_or_default();
    ///     // ... handle submission ...
    ///     true
    /// })
    /// .open_dialog(window, cx);
    /// ```
    pub fn on_dialog_submit(
        mut self,
        handler: impl Fn(IndexMap<SharedString, SharedString>, &mut Window, &mut App) -> bool + 'static,
    ) -> Self {
        self.dialog_submit = Some(Rc::new(handler));
        self
    }

    /// Set the width of the dialog.
    pub fn dialog_width(mut self, width: Pixels) -> Self {
        self.dialog_width = Some(width);
        self
    }

    /// Cap the **entire dialog panel** (title + scroll body + footer), not just
    /// the form fields. The dialog body already scrolls; this only sets `max_h`
    /// on the panel so that scrollbar can engage.
    pub fn dialog_max_height(mut self, max_height: Pixels) -> Self {
        self.dialog_max_height = Some(max_height);
        self
    }

    pub fn open_dialog(mut self, window: &mut Window, cx: &mut App) {
        // Wrap the dialog-level submit handler into a form-level on_submit.
        // The wrapper auto-closes the dialog when the handler returns true.
        if let Some(dialog_handler) = self.dialog_submit.take() {
            self.on_submit = Some(Rc::new(move |values, window, cx| {
                if dialog_handler(values, window, cx) {
                    window.close_dialog(cx);
                    return true;
                }
                false
            }));
        }

        // Set on_cancel to close the dialog.
        self.on_cancel = Some(Rc::new(|window, cx| {
            window.close_dialog(cx);
            true
        }));

        let title = self.title.clone();
        self.title = None;
        let dialog_width = self.dialog_width.take();
        let max_height = self.dialog_max_height.take();
        // Create the form entity once; the dialog closure (called every frame)
        // just clones the entity handle.
        let form = cx.new(|cx| {
            let mut f = ZedisForm::new("dialog-form", self, window, cx);
            f.in_dialog = true;
            f
        });

        window.open_dialog(cx, move |dialog, window, cx| {
            let mut d = dialog.overlay(true).overlay_closable(true);
            if let Some(w) = dialog_width {
                d = d.width(w);
            }
            // Apply max_h on the *dialog panel* so its built-in body
            // `overflow_y_scrollbar` can engage. Nesting a max_h wrapper
            // around the form and giving the form its own scrollbar
            // produces a dead side track (content height equals the
            // scroll viewport, so nothing scrolls) — the broken bar on
            // the add/edit server dialog.
            if let Some(mh) = max_height {
                d = d.max_h(mh);
            }
            if let Some(t) = &title {
                d = d.title(t.clone());
            }
            // Action row lives in the dialog footer (sibling of the scroll
            // body) so cancel/confirm/foot_actions stay pinned while fields
            // scroll — same pattern as the update dialog.
            let footer = form.update(cx, |form, cx| form.render_action_bar(window, cx));
            // Override the Dialog's default on_ok (which closes on Enter)
            // to trigger form validation and submit instead.
            let form_for_ok = form.clone();
            let mut d = d.child(form.clone()).on_ok(move |_, window, cx| {
                form_for_ok.update(cx, |form, cx| {
                    form.submit(window, cx);
                });
                // Don't close here — the submit handler closes on success.
                false
            });
            if let Some(footer) = footer {
                d = d.footer(footer);
            }
            d
        });
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
    in_dialog: bool,
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

    fn default_pair_mode(&self) -> ZedisFormAddFieldsMode {
        ZedisFormAddFieldsMode::Pair {
            field_placeholder: self.add_field_placeholder.clone(),
            value_placeholder: self.add_value_placeholder.clone(),
            initial_rows: 1,
        }
    }

    fn active_add_fields_mode(&self, cx: &App) -> Option<ZedisFormAddFieldsMode> {
        if !self.should_show_add_fields(cx) {
            return None;
        }
        if let Some((ref radio_name, ref modes)) = self.add_fields_modes_on {
            let selected = self.radio_group_selected(radio_name, cx);
            if let Some((_, mode)) = modes.iter().find(|(i, _)| *i == selected) {
                return Some(mode.clone());
            }
        }
        if let Some(ref mode) = self.add_fields_mode {
            return Some(mode.clone());
        }
        Some(self.default_pair_mode())
    }

    fn sync_add_fields_for_radio(&mut self, radio_name: &str, window: &mut Window, cx: &mut Context<Self>) {
        let controls_add_fields = self
            .support_add_fields_on
            .as_ref()
            .is_some_and(|(name, _)| name.as_ref() == radio_name)
            || self
                .add_fields_modes_on
                .as_ref()
                .is_some_and(|(name, _)| name.as_ref() == radio_name);
        if !controls_add_fields {
            return;
        }
        self.add_field_states.clear();
        let initial_rows = self.active_add_fields_mode(cx).map_or(0, |mode| mode.initial_rows());
        for _ in 0..initial_rows {
            self.add_field(window, cx);
        }
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

    /// Returns `true` if the dynamic add-fields section should be displayed.
    fn should_show_add_fields(&self, cx: &App) -> bool {
        if !self.support_add_fields {
            return false;
        }
        if let Some((ref radio_name, ref indices)) = self.support_add_fields_on {
            let selected = self.radio_group_selected(radio_name, cx);
            return indices.contains(&selected);
        }
        true
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
        if let Some(mode) = self.active_add_fields_mode(cx) {
            let is_single = mode.is_single();
            for (index, (field_state, value_state)) in self.add_field_states.iter().enumerate() {
                if is_single {
                    let value = value_state.read(cx).value();
                    values.insert(format!("{EXTRA_SINGLE_ROW_PREFIX}{index}").into(), value);
                } else {
                    let field = field_state.read(cx).value();
                    let value = value_state.read(cx).value();
                    if !field.is_empty() && !values.contains_key(&field) {
                        values.insert(field, value);
                    } else {
                        values.insert(format!("{EXTRA_PAIR_DUP_PREFIX}{index}:{field}").into(), value);
                    }
                }
            }
        }
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
    fn render_action_bar(&self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
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
        let mut right_buttons = h_flex().justify_end().gap_4();
        let mut left_buttons = h_flex().justify_start().gap_4();

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

    pub fn remove_add_field(&mut self, index: usize, cx: &mut Context<Self>) {
        self.add_field_states.remove(index);
        cx.notify();
    }

    pub fn add_field(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (field_placeholder, value_placeholder) = match self.active_add_fields_mode(cx) {
            Some(ZedisFormAddFieldsMode::Single { placeholder, .. }) => (SharedString::default(), placeholder),
            Some(ZedisFormAddFieldsMode::Pair {
                field_placeholder,
                value_placeholder,
                ..
            }) => (field_placeholder, value_placeholder),
            None => (self.add_field_placeholder.clone(), self.add_value_placeholder.clone()),
        };
        self.add_field_states.push((
            cx.new(|cx| InputState::new(window, cx).placeholder(field_placeholder)),
            cx.new(|cx| InputState::new(window, cx).placeholder(value_placeholder)),
        ));
        cx.notify();
    }

    /// Return the current number of dynamic add-field rows.
    pub fn add_field_count(&self) -> usize {
        self.add_field_states.len()
    }

    /// Set the value of a dynamic add-field row by index.
    pub fn set_add_field_values(
        &mut self,
        index: usize,
        field_value: impl Into<SharedString>,
        value: impl Into<SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some((field_state, value_state)) = self.add_field_states.get(index) {
            let field_value = field_value.into();
            let value = value.into();
            field_state.update(cx, |s, cx| s.set_value(field_value, window, cx));
            value_state.update(cx, |s, cx| s.set_value(value, window, cx));
        }
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

    /// Distribute multi-line (or tab-separated pair) clipboard text across
    /// dynamic add-field rows starting at `start_row`.
    ///
    /// Returns `true` when the paste was handled across dynamic rows so the
    /// caller can stop default single-line paste propagation.
    pub fn paste_add_field_lines(
        &mut self,
        start_row: usize,
        is_value_col: bool,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(mode) = self.active_add_fields_mode(cx) else {
            return false;
        };
        let lines = split_paste_lines(text);
        if mode.is_single() {
            if !text.contains('\n') || lines.len() < 2 {
                return false;
            }
            for (offset, line) in lines.into_iter().enumerate() {
                let row_ix = start_row + offset;
                while self.add_field_states.len() <= row_ix {
                    self.add_field(window, cx);
                }
                if let Some((_, value_state)) = self.add_field_states.get(row_ix) {
                    let val: SharedString = line.to_string().into();
                    value_state.update(cx, |s, cx| s.set_value(val, window, cx));
                }
            }
            cx.notify();
            return true;
        }

        let single_line_tab = !is_value_col && lines.len() == 1 && lines[0].contains('\t');
        if (!text.contains('\n') || lines.len() < 2) && !single_line_tab {
            return false;
        }

        for (offset, line) in lines.into_iter().enumerate() {
            let row_ix = start_row + offset;
            while self.add_field_states.len() <= row_ix {
                self.add_field(window, cx);
            }
            let Some((field_state, value_state)) = self.add_field_states.get(row_ix) else {
                continue;
            };
            if is_value_col {
                let val: SharedString = line.to_string().into();
                value_state.update(cx, |s, cx| s.set_value(val, window, cx));
            } else {
                let (field_part, value_part) = split_paste_pair(line);
                let field_val: SharedString = field_part.to_string().into();
                field_state.update(cx, |s, cx| s.set_value(field_val, window, cx));
                if let Some(val_part) = value_part {
                    let val: SharedString = val_part.to_string().into();
                    value_state.update(cx, |s, cx| s.set_value(val, window, cx));
                }
            }
        }
        cx.notify();
        true
    }

    /// Distribute multi-line or tab-separated pair clipboard text pasted into
    /// a static pair field (`field_name`, `companion_name`), filling the
    /// static pair with the first line and overflowing remaining lines into
    /// dynamic (`+`) rows.
    ///
    /// When `require_all_pairs` is true (used for `Textarea` fields that can
    /// legitimately hold a single multi-line JSON/text document), the paste
    /// is only split if every non-empty line parses as a `(left, right)` pair.
    pub fn paste_static_pair_lines(
        &mut self,
        field_name: &str,
        companion_name: &str,
        require_all_pairs: bool,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(mode) = self.active_add_fields_mode(cx) else {
            return false;
        };
        if mode.is_single() {
            return false;
        }
        let lines = split_paste_lines(text);
        if lines.is_empty() {
            return false;
        }
        let single_line_tab = lines.len() == 1 && lines[0].contains('\t');
        if (!text.contains('\n') || lines.len() < 2) && !single_line_tab {
            return false;
        }
        if require_all_pairs && !lines.iter().all(|line| split_paste_pair(line).1.is_some()) {
            return false;
        }

        let (first_left, first_right) = split_paste_pair(lines[0]);
        self.set_field_value(field_name, first_left.to_string(), window, cx);
        if let Some(right) = first_right {
            self.set_field_value(companion_name, right.to_string(), window, cx);
        }

        for (offset, line) in lines.into_iter().skip(1).enumerate() {
            while self.add_field_states.len() <= offset {
                self.add_field(window, cx);
            }
            let Some((field_state, value_state)) = self.add_field_states.get(offset) else {
                continue;
            };
            let (field_part, value_part) = split_paste_pair(line);
            let field_val: SharedString = field_part.to_string().into();
            field_state.update(cx, |s, cx| s.set_value(field_val, window, cx));
            if let Some(val_part) = value_part {
                let val: SharedString = val_part.to_string().into();
                value_state.update(cx, |s, cx| s.set_value(val, window, cx));
            }
        }
        cx.notify();
        true
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
                row().child(Label::new(title).text_lg().font_weight(FontWeight::BOLD)),
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

        if let Some(mode) = self.active_add_fields_mode(cx) {
            let is_single = mode.is_single();
            for (index, (field_state, value_state)) in self.add_field_states.iter().enumerate() {
                let mut row_flex = h_flex().gap_2();
                if !is_single {
                    let form_entity = cx.entity().clone();
                    row_flex = row_flex.child(Input::new(field_state).disabled(form_disabled).on_paste(
                        move |clipboard, window, cx| {
                            let Some(text) = clipboard.text() else {
                                return false;
                            };
                            form_entity.update(cx, |this, cx| {
                                this.paste_add_field_lines(index, false, &text, window, cx)
                            })
                        },
                    ));
                }
                let form_entity = cx.entity().clone();
                row_flex =
                    row_flex
                        .child(Input::new(value_state).disabled(form_disabled).on_paste(
                            move |clipboard, window, cx| {
                                let Some(text) = clipboard.text() else {
                                    return false;
                                };
                                form_entity.update(cx, |this, cx| {
                                    this.paste_add_field_lines(index, true, &text, window, cx)
                                })
                            },
                        ))
                        .child(
                            Button::new(("remove-add-field", index))
                                .icon(IconName::CircleX)
                                .disabled(form_disabled)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.remove_add_field(index, cx);
                                })),
                        );
                items.push(FormItem::field(row().child(row_flex)));
            }
            items.push(FormItem::field(
                row().child(
                    h_flex().justify_end().child(
                        Button::new("add-add-field")
                            .icon(IconName::Plus)
                            .disabled(form_disabled)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.add_field(window, cx);
                            })),
                    ),
                ),
            ));
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
const FORM_COLUMNS: u16 = 12;

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

/// Non-empty lines extracted from clipboard text, stripping `\r`.
pub fn split_paste_lines(text: &str) -> Vec<&str> {
    text.split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
        .filter(|line| !line.trim().is_empty())
        .collect()
}

/// Parse a pasted line into `(first_column, optional_second_column)`.
///
/// Recognises:
/// 1. Tab-delimited (`field\tvalue` — TSV / spreadsheet copy, preserving spacing around `\t`).
/// 2. Equals-delimited (`field=value` — `.env` / properties).
/// 3. Colon-space delimited (`field: value` — YAML / header copy; bare `:` without space is
///    kept whole so Redis colon-namespaced identifiers like `user:1001` are not split).
/// 4. Whitespace-delimited (`member 42.5`).
pub fn split_paste_pair(line: &str) -> (&str, Option<&str>) {
    if let Some((left, right)) = line.split_once('\t')
        && !left.is_empty()
    {
        return (left, Some(right));
    }
    if let Some((left, right)) = line.split_once('=')
        && !left.trim().is_empty()
    {
        return (left.trim(), Some(right.trim()));
    }
    if let Some((left, right)) = line.split_once(": ")
        && !left.trim().is_empty()
    {
        return (left.trim(), Some(right.trim()));
    }
    let trimmed = line.trim();
    if let Some((left, right)) = trimmed.split_once(char::is_whitespace)
        && !left.is_empty()
        && !right.trim().is_empty()
    {
        return (left, Some(right.trim()));
    }
    (line, None)
}

#[cfg(test)]
mod tests {
    use super::{
        EXTRA_PAIR_DUP_PREFIX, EXTRA_SINGLE_ROW_PREFIX, decode_extra_pair_key, extra_pair_rows, extra_single_rows,
        split_paste_lines, split_paste_pair, submitted_value,
    };
    use gpui::SharedString;
    use indexmap::IndexMap;

    #[test]
    fn paste_lines_and_pairs_split_cleanly_while_keeping_redis_colons() {
        assert_eq!(split_paste_lines("a\r\nb\n\nc\n"), vec!["a", "b", "c"]);
        // Tab-delimited keeps exact cell values.
        assert_eq!(
            split_paste_pair("user:1001\tAlice Smith"),
            ("user:1001", Some("Alice Smith"))
        );
        // Equals and colon-space split and trim.
        assert_eq!(split_paste_pair("user:1001 = Alice"), ("user:1001", Some("Alice")));
        assert_eq!(split_paste_pair("user:1001: Alice"), ("user:1001", Some("Alice")));
        // Space-delimited (e.g. ZSET `member score`).
        assert_eq!(split_paste_pair("player:1 42.5"), ("player:1", Some("42.5")));
        // Bare Redis colon identifier stays intact as a single field/member.
        assert_eq!(split_paste_pair("user:1001"), ("user:1001", None));
    }

    #[test]
    fn only_a_verbatim_field_keeps_its_surrounding_whitespace() {
        // A setting or a number is trimmed, as it always was.
        assert_eq!(submitted_value("  6379 \n", false), "6379");
        // A value is data: an indented YAML line and its final newline stay.
        assert_eq!(submitted_value("  a: 1\n", true), "  a: 1\n");
        assert_eq!(submitted_value("name ", true), "name ");
    }

    #[test]
    fn extra_single_rows_preserves_duplicate_values_and_skips_static_fields() {
        let mut values: IndexMap<SharedString, SharedString> = IndexMap::new();
        values.insert("key".into(), "my-list".into());
        values.insert("value".into(), "first".into());
        values.insert(format!("{EXTRA_SINGLE_ROW_PREFIX}0").into(), "dup".into());
        values.insert(format!("{EXTRA_SINGLE_ROW_PREFIX}1").into(), "dup".into());
        values.insert(format!("{EXTRA_SINGLE_ROW_PREFIX}2").into(), "third".into());

        let rows = extra_single_rows(&values);
        assert_eq!(
            rows,
            vec![
                SharedString::from("dup"),
                SharedString::from("dup"),
                SharedString::from("third"),
            ]
        );
    }

    #[test]
    fn extra_pair_rows_decodes_duplicate_and_empty_first_columns() {
        let mut values: IndexMap<SharedString, SharedString> = IndexMap::new();
        values.insert("key".into(), "my-zset".into());
        values.insert("score".into(), "1".into());
        values.insert("member".into(), "m1".into());
        // First dynamic row with unique non-empty score "2":
        values.insert("2".into(), "m2".into());
        // Second dynamic row with duplicate score "2":
        values.insert(format!("{EXTRA_PAIR_DUP_PREFIX}1:2").into(), "m3".into());
        // Third dynamic row with empty score "":
        values.insert(format!("{EXTRA_PAIR_DUP_PREFIX}2:").into(), "m4".into());

        let pairs = extra_pair_rows(&values, &["key", "score", "member"]);
        assert_eq!(
            pairs,
            vec![
                (SharedString::from("2"), SharedString::from("m2")),
                (SharedString::from("2"), SharedString::from("m3")),
                (SharedString::from(""), SharedString::from("m4")),
            ]
        );
        assert_eq!(
            decode_extra_pair_key(&format!("{EXTRA_PAIR_DUP_PREFIX}1:2")),
            SharedString::from("2")
        );
    }
}
