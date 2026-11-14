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

//! Dynamic (`+`) multi-row fields and clipboard paste auto-splitting for [`ZedisForm`].

use super::{ZedisForm, row};
use gpui::{App, Context, SharedString, Window, prelude::*};
use gpui_kit::component::button::Button;
use gpui_kit::component::form::Field;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::{Disableable, IconName, Sizable, h_flex};
use indexmap::IndexMap;

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

    pub(super) fn initial_rows(&self) -> usize {
        match self {
            Self::Single { initial_rows, .. } | Self::Pair { initial_rows, .. } => *initial_rows,
        }
    }

    pub(super) fn is_single(&self) -> bool {
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

/// Non-empty lines extracted from clipboard text, stripping `\r`.
pub fn split_paste_lines(text: &str) -> Vec<&str> {
    text.split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
        .filter(|line| !line.trim().is_empty())
        .collect()
}

/// Whether multi-line clipboard text looks like a single pretty-printed JSON or
/// XML document rather than a newline-delimited list of elements (such as plain
/// items or JSONLines / NDJSON).
pub fn looks_like_single_structured_doc(text: &str, lines: &[&str]) -> bool {
    if lines.len() < 2 {
        return false;
    }
    let trimmed = text.trim();
    let wrapped_json =
        (trimmed.starts_with('{') && trimmed.ends_with('}')) || (trimmed.starts_with('[') && trimmed.ends_with(']'));
    if wrapped_json {
        let all_lines_self_contained = lines.iter().all(|line| {
            let l = line.trim();
            (l.starts_with('{') && l.ends_with('}')) || (l.starts_with('[') && l.ends_with(']'))
        });
        if !all_lines_self_contained {
            return true;
        }
    }
    if trimmed.starts_with('<') && trimmed.ends_with('>') {
        let all_lines_self_contained = lines.iter().all(|line| {
            let l = line.trim();
            l.starts_with('<') && l.ends_with('>') && (l.ends_with("/>") || l.contains("</"))
        });
        if !all_lines_self_contained {
            return true;
        }
    }
    false
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

impl ZedisForm {
    pub(super) fn default_pair_mode(&self) -> ZedisFormAddFieldsMode {
        ZedisFormAddFieldsMode::Pair {
            field_placeholder: self.add_field_placeholder.clone(),
            value_placeholder: self.add_value_placeholder.clone(),
            initial_rows: 1,
        }
    }

    pub(super) fn active_add_fields_mode(&self, cx: &App) -> Option<ZedisFormAddFieldsMode> {
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

    /// Returns `true` if the dynamic add-fields section should be displayed.
    pub(super) fn should_show_add_fields(&self, cx: &App) -> bool {
        if !self.support_add_fields {
            return false;
        }
        if let Some((ref radio_name, ref indices)) = self.support_add_fields_on {
            let selected = self.radio_group_selected(radio_name, cx);
            return indices.contains(&selected);
        }
        true
    }

    pub(super) fn sync_add_fields_for_radio(&mut self, radio_name: &str, window: &mut Window, cx: &mut Context<Self>) {
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

    pub(super) fn collect_add_field_values(&self, values: &mut IndexMap<SharedString, SharedString>, cx: &App) {
        let Some(mode) = self.active_add_fields_mode(cx) else {
            return;
        };
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

    pub(super) fn render_add_fields_section(&self, form_disabled: bool, cx: &mut Context<Self>) -> Vec<Field> {
        let Some(mode) = self.active_add_fields_mode(cx) else {
            return Vec::new();
        };
        let mut out = Vec::with_capacity(self.add_field_states.len() + 1);
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
            row_flex = row_flex
                .child(
                    Input::new(value_state)
                        .disabled(form_disabled)
                        .on_paste(move |clipboard, window, cx| {
                            let Some(text) = clipboard.text() else {
                                return false;
                            };
                            form_entity.update(cx, |this, cx| {
                                this.paste_add_field_lines(index, true, &text, window, cx)
                            })
                        }),
                )
                .child(
                    Button::new(("remove-add-field", index))
                        .icon(IconName::CircleX)
                        .disabled(form_disabled)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.remove_add_field(index, cx);
                        })),
                );
            out.push(row().child(row_flex));
        }

        let mut add_btn = Button::new("add-add-field")
            .icon(IconName::Plus)
            .disabled(form_disabled);
        if let Some(label) = &self.add_button_label {
            add_btn = add_btn.outline().small().label(label.clone());
        }
        out.push(
            row().child(
                h_flex()
                    .justify_end()
                    .child(add_btn.on_click(cx.listener(move |this, _, window, cx| {
                        this.add_field(window, cx);
                    }))),
            ),
        );
        out
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

    /// Distribute multi-line clipboard text pasted into a static single-element
    /// field (`field_name`), filling the static field with the first line and
    /// overflowing remaining lines into dynamic (`+`) rows when single-column
    /// add-fields mode is active.
    ///
    /// Pretty-printed multi-line JSON/XML documents stay intact as a single
    /// element, whereas plain newline lists and JSONLines (NDJSON) split cleanly.
    pub fn paste_static_single_lines(
        &mut self,
        field_name: &str,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(mode) = self.active_add_fields_mode(cx) else {
            return false;
        };
        if !mode.is_single() || !text.contains('\n') {
            return false;
        }
        let lines = split_paste_lines(text);
        if lines.len() < 2 || looks_like_single_structured_doc(text, &lines) {
            return false;
        }

        self.set_field_value(field_name, lines[0].to_string(), window, cx);
        for (offset, line) in lines.into_iter().skip(1).enumerate() {
            while self.add_field_states.len() <= offset {
                self.add_field(window, cx);
            }
            if let Some((_, value_state)) = self.add_field_states.get(offset) {
                let val: SharedString = line.to_string().into();
                value_state.update(cx, |s, cx| s.set_value(val, window, cx));
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

#[cfg(test)]
mod tests {
    use super::{
        EXTRA_PAIR_DUP_PREFIX, EXTRA_SINGLE_ROW_PREFIX, decode_extra_pair_key, extra_pair_rows, extra_single_rows,
        looks_like_single_structured_doc, split_paste_lines, split_paste_pair,
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
    fn structured_doc_detection_distinguishes_pretty_json_from_ndjson_and_lists() {
        let plain = "alpha\nbeta\ngamma";
        assert!(!looks_like_single_structured_doc(plain, &split_paste_lines(plain)));

        let ndjson = "{\"id\":1,\"name\":\"a\"}\n{\"id\":2,\"name\":\"b\"}";
        assert!(!looks_like_single_structured_doc(ndjson, &split_paste_lines(ndjson)));

        let pretty_json = "{\n  \"id\": 1,\n  \"name\": \"Alice\"\n}";
        assert!(looks_like_single_structured_doc(
            pretty_json,
            &split_paste_lines(pretty_json)
        ));

        let pretty_array = "[\n  \"a\",\n  \"b\"\n]";
        assert!(looks_like_single_structured_doc(
            pretty_array,
            &split_paste_lines(pretty_array)
        ));

        let xml_doc = "<user>\n  <name>Alice</name>\n</user>";
        assert!(looks_like_single_structured_doc(xml_doc, &split_paste_lines(xml_doc)));
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
        values.insert("2".into(), "m2".into());
        values.insert(format!("{EXTRA_PAIR_DUP_PREFIX}1:2").into(), "m3".into());
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
