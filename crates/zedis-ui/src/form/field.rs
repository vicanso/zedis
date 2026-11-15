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

use super::{FORM_COLUMNS, ZedisForm, ZedisFormAddFieldsMode};
use gpui::{AnyElement, App, Pixels, SharedString, StyleRefinement, Window, prelude::*};
use gpui_kit::component::WindowExt;
use indexmap::IndexMap;
use std::rc::Rc;

/// Callback invoked on form submission with all field values collected as a map.
/// Returns `true` if the submission was handled successfully.
pub(super) type ZedisFormSubmitHandler =
    Rc<dyn Fn(IndexMap<SharedString, SharedString>, &mut Window, &mut Context<ZedisForm>) -> bool + 'static>;

/// Per-field validation callback. Returns `Some(error_message)` on failure, `None` on success.
pub(super) type ZedisFormValidateHandler = Rc<dyn Fn(&str) -> Option<SharedString> + 'static>;

/// Callback invoked when the cancel button is clicked.
/// Returns `true` if the cancellation was handled.
pub(super) type ZedisFormCancelHandler = Rc<dyn Fn(&mut Window, &mut Context<ZedisForm>) -> bool + 'static>;

/// Callback invoked to build the action buttons for the footer.
pub type ZedisFormActionsBuilder = Rc<dyn Fn(&mut Window, &mut Context<ZedisForm>) -> Vec<AnyElement>>;

/// Callback invoked to build the suffix element for an input field.
pub(super) type ZedisFormFieldSuffixBuilder = Rc<dyn Fn(&mut Window, &mut Context<ZedisForm>) -> AnyElement>;

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
    pub(super) style: StyleRefinement,
    /// Takes the height the form has left instead of a height of its own —
    /// the one editor of a form in [`ZedisFormOptions::fill_height`] mode.
    pub(super) fill: bool,
    pub(super) name: SharedString,
    pub(super) label: SharedString,
    pub(super) placeholder: SharedString,
    /// Assigns this field to a specific tab (display-only).
    /// The field is hidden when another tab is active, but its value is
    /// **always** included on form submission regardless of the active tab.
    pub(super) tab_index: Option<usize>,
    pub(super) default_value: Option<SharedString>,
    pub(super) field_type: ZedisFormFieldType,
    /// Options list for `RadioGroup` fields.
    pub(super) options: Option<Vec<SharedString>>,
    pub(super) validate: Option<ZedisFormValidateHandler>,
    pub(super) mask: bool,
    pub(super) required: bool,
    /// Whether this field should receive focus on the first render.
    pub(super) focus: bool,
    pub(super) readonly: bool,
    /// Makes this field conditionally dependent on a RadioGroup's selection.
    /// Unlike `tab_index`, when the condition is **not** met the field is both
    /// hidden from the UI **and** excluded from the submitted values.
    pub(super) visible_on: Option<(SharedString, Vec<usize>)>,
    /// Makes this field conditionally dependent on an Input-backed field
    /// holding a non-blank value (e.g. a key passphrase that is meaningless
    /// without a key). Same semantics as `visible_on`: hidden and excluded
    /// from submission while the condition is not met.
    pub(super) visible_on_filled: Option<SharedString>,
    pub(super) suffix_builder: Option<ZedisFormFieldSuffixBuilder>,
    /// Submit the text exactly as typed. Off by default, where the form
    /// trims — right for settings, names and numbers, wrong for a field
    /// whose text *is* data (a Redis value, a member, a hash field name).
    pub(super) verbatim: bool,
    /// Lay a `RadioGroup`'s options out one per line.
    pub(super) stacked: bool,
    /// How many of the form's twelve columns the field takes; the whole row
    /// when unset.
    pub(super) span: Option<u16>,
    /// Optional companion field name when this field is the first column of a
    /// collection pair (e.g. Hash `Field` + `Value` or ZSet `Member` + `Score`),
    /// allowing multi-line or tab-separated pastes to fill this pair and
    /// overflow remaining lines into dynamic (`+`) rows.
    pub(super) paste_pair_with: Option<SharedString>,
    /// When enabled and the active add-fields mode is single-column (e.g. List
    /// or Set), pasting a multi-line list into this field fills it with the
    /// first line and distributes remaining lines across dynamic (`+`) rows.
    pub(super) paste_single_lines: bool,
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
            paste_single_lines: false,
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

    /// Split multi-line pastes into `(self, extra_single_rows…)` whenever the
    /// form's active add-fields mode is single-column (e.g. List or Set), while
    /// keeping pretty-printed single JSON/XML documents intact.
    pub fn paste_single_lines(mut self) -> Self {
        self.paste_single_lines = true;
        self
    }
}

impl Styled for ZedisFormField {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl FluentBuilder for ZedisFormField {}

/// Configuration for constructing a [`ZedisForm`]. Collects field descriptors,
/// tab labels, button labels, and event handlers before entity creation.
pub struct ZedisFormOptions {
    pub(super) title: Option<SharedString>,
    pub(super) description: Option<SharedString>,
    pub(super) tabs: Option<Vec<SharedString>>,
    pub(super) fields: Vec<ZedisFormField>,
    pub(super) required_error_msg: SharedString,
    pub(super) confirm_label: SharedString,
    pub(super) confirm_tooltip: Option<SharedString>,
    pub(super) cancel_label: SharedString,
    pub(super) add_field_placeholder: SharedString,
    pub(super) add_value_placeholder: SharedString,
    pub(super) add_button_label: Option<SharedString>,
    pub(super) on_submit: Option<ZedisFormSubmitHandler>,
    pub(super) on_cancel: Option<ZedisFormCancelHandler>,
    pub(super) foot_actions: Option<ZedisFormActionsBuilder>,
    dialog_submit: Option<ZedisFormDialogSubmitHandler>,
    dialog_width: Option<Pixels>,
    dialog_max_height: Option<Pixels>,
    pub(super) support_add_fields: bool,
    /// When set, the add-fields section is only shown when the referenced
    /// RadioGroup's selected index is in the given list.
    pub(super) support_add_fields_on: Option<(SharedString, Vec<usize>)>,
    pub(super) add_fields_mode: Option<ZedisFormAddFieldsMode>,
    pub(super) add_fields_modes_on: Option<(SharedString, Vec<(usize, ZedisFormAddFieldsMode)>)>,
    /// Lay the fields out as a flex column that fills the parent, with the
    /// [`ZedisFormField::fill`] field taking what is left, instead of the
    /// form grid in a scroll container. For an inline form whose parent
    /// has a definite height — an entry panel — where a field should end
    /// exactly at the bottom without anyone computing its height.
    pub(super) fill_height: bool,
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
            add_button_label: None,
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

    /// Set a visible label on the `+` add-row button so multi-row entry is discoverable.
    pub fn add_button_label(mut self, label: impl Into<SharedString>) -> Self {
        self.add_button_label = Some(label.into());
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

impl FluentBuilder for ZedisFormOptions {}

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
