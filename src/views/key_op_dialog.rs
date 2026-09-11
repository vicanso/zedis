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

//! The form behind the editors' type-native operations (`KeyOpAction`).
//!
//! One dialog for all of them rather than one each: every one of them takes
//! at most two short values, and the difference between them is which
//! labels those carry and how the pair becomes a [`KeyOp`]. Keeping that
//! difference as data means adding an operation is a table row, and it
//! means the *validation* is written once — a delta that will not parse
//! keeps the dialog open instead of quietly sending a zero.
//!
//! The form and what runs the result are separate: the JSON tree opens the
//! same form with the path filled in, and for a plain string applies the
//! operation to its local document instead of sending it.

use crate::connection::{FromEnd, KeyOp};
use crate::helpers::KeyOpAction;
use crate::states::{ZedisServerState, dialog_button_props, i18n_common, i18n_key_ops};
use gpui::{App, Entity, SharedString, Window, prelude::*, px};
use gpui_kit::component::{
    ActiveTheme, Sizable, WindowExt,
    input::{Input, InputState},
    label::Label,
    v_flex,
};
use serde_json::Value;
use zedis_core::json::JsonPathOp;
use zedis_ui::ZedisDialog;

/// One input in the dialog: what it is called and what it starts as.
struct FieldSpec {
    /// i18n key under `[key_ops]`.
    label: &'static str,
    default: SharedString,
}

/// Values the caller already knows, so the form opens filled in: the JSON
/// tree opens an operation on the node under the pointer.
#[derive(Debug, Clone, Default)]
pub struct KeyOpPrefill {
    /// The JSONPath of the JSON operations.
    pub path: Option<String>,
    /// The value `JSON.SET` starts from — the node's current one.
    pub value: Option<String>,
}

/// The fields `action` asks for, in order.
fn fields(action: KeyOpAction, prefill: &KeyOpPrefill) -> Vec<FieldSpec> {
    let field = |label: &'static str, default: &str| FieldSpec {
        label,
        default: default.to_string().into(),
    };
    let path = || field("field_path", prefill.path.as_deref().unwrap_or("$"));
    let json_value = || field("field_json_value", prefill.value.as_deref().unwrap_or(""));
    match action {
        KeyOpAction::ListTrim => vec![
            field("field_start", "0"),
            // -1 is "to the end", so the default window keeps everything:
            // the form has to be *asked* to delete.
            field("field_stop", "-1"),
        ],
        KeyOpAction::ListPopHead | KeyOpAction::ListPopTail | KeyOpAction::ZsetPopMin | KeyOpAction::ZsetPopMax => {
            vec![field("field_count", "1")]
        }
        KeyOpAction::ZsetIncrBy => vec![field("field_member", ""), field("field_delta", "1")],
        KeyOpAction::HashIncrBy => vec![field("field_field", ""), field("field_delta", "1")],
        KeyOpAction::StringIncrBy => vec![field("field_delta", "1")],
        KeyOpAction::StringAppend => vec![field("field_text", "")],
        KeyOpAction::StringGetEx => vec![field("field_ttl", "")],
        KeyOpAction::JsonSet | KeyOpAction::JsonArrAppend => vec![path(), json_value()],
        KeyOpAction::JsonDel | KeyOpAction::JsonToggle | KeyOpAction::JsonClear => vec![path()],
        KeyOpAction::JsonNumIncrBy => vec![path(), field("field_delta", "1")],
        KeyOpAction::JsonStrAppend => vec![path(), field("field_text", "")],
    }
}

/// The dialog's title, and the label on its confirm button.
pub fn title_key(action: KeyOpAction) -> &'static str {
    match action {
        KeyOpAction::ListTrim => "ltrim",
        KeyOpAction::ListPopHead => "lpop",
        KeyOpAction::ListPopTail => "rpop",
        KeyOpAction::ZsetIncrBy => "zincrby",
        KeyOpAction::ZsetPopMin => "zpopmin",
        KeyOpAction::ZsetPopMax => "zpopmax",
        KeyOpAction::HashIncrBy => "hincrby",
        KeyOpAction::StringIncrBy => "incrby",
        KeyOpAction::StringAppend => "append",
        KeyOpAction::StringGetEx => "getex",
        KeyOpAction::JsonSet => "json_set",
        KeyOpAction::JsonDel => "json_del",
        KeyOpAction::JsonNumIncrBy => "json_numincrby",
        KeyOpAction::JsonToggle => "json_toggle",
        KeyOpAction::JsonArrAppend => "json_arrappend",
        KeyOpAction::JsonStrAppend => "json_strappend",
        KeyOpAction::JsonClear => "json_clear",
    }
}

pub struct ZedisKeyOpDialog {
    action: KeyOpAction,
    inputs: Vec<(&'static str, Entity<InputState>)>,
}

impl ZedisKeyOpDialog {
    pub fn new(action: KeyOpAction, prefill: &KeyOpPrefill, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let inputs = fields(action, prefill)
            .into_iter()
            .map(|spec| {
                let state = cx.new(|cx| InputState::new(window, cx).default_value(spec.default));
                (spec.label, state)
            })
            .collect();
        Self { action, inputs }
    }

    fn value(&self, index: usize, cx: &gpui::App) -> String {
        self.inputs
            .get(index)
            .map(|(_, state)| state.read(cx).value().trim().to_string())
            .unwrap_or_default()
    }

    /// The operation the current field values describe, or `None` when they
    /// do not describe one — an unparsable number, an empty member, a value
    /// that is not JSON. The caller keeps the dialog open in that case
    /// rather than sending something the user did not mean.
    pub fn key_op(&self, cx: &gpui::App) -> Option<KeyOp> {
        let first = self.value(0, cx);
        let second = self.value(1, cx);
        let json = |path: String, op: JsonPathOp| (!path.is_empty()).then_some(KeyOp::Json { path, op });
        match self.action {
            KeyOpAction::ListTrim => Some(KeyOp::ListTrim {
                start: first.parse().ok()?,
                stop: second.parse().ok()?,
            }),
            KeyOpAction::ListPopHead => Some(KeyOp::ListPop {
                end: FromEnd::Head,
                count: positive_count(&first)?,
            }),
            KeyOpAction::ListPopTail => Some(KeyOp::ListPop {
                end: FromEnd::Tail,
                count: positive_count(&first)?,
            }),
            KeyOpAction::ZsetPopMin => Some(KeyOp::ZsetPop {
                end: FromEnd::Head,
                count: positive_count(&first)?,
            }),
            KeyOpAction::ZsetPopMax => Some(KeyOp::ZsetPop {
                end: FromEnd::Tail,
                count: positive_count(&first)?,
            }),
            KeyOpAction::ZsetIncrBy => {
                if first.is_empty() {
                    return None;
                }
                Some(KeyOp::ZsetIncrBy {
                    member: first,
                    delta: second.parse().ok()?,
                })
            }
            KeyOpAction::HashIncrBy => {
                if first.is_empty() {
                    return None;
                }
                Some(KeyOp::HashIncrBy {
                    field: first,
                    delta: second.parse().ok()?,
                })
            }
            KeyOpAction::StringIncrBy => Some(KeyOp::StringIncrBy {
                delta: first.parse().ok()?,
            }),
            // An empty append is a no-op, not an error worth sending.
            KeyOpAction::StringAppend => (!first.is_empty()).then_some(KeyOp::StringAppend { text: first }),
            // Blank means PERSIST: "no expiry" is a real answer here, so an
            // empty field is valid rather than missing.
            KeyOpAction::StringGetEx => {
                if first.is_empty() {
                    Some(KeyOp::StringGetEx { ttl: None })
                } else {
                    Some(KeyOp::StringGetEx {
                        ttl: Some(first.parse().ok()?),
                    })
                }
            }
            KeyOpAction::JsonSet => json(first, JsonPathOp::Set(parse_json(&second)?)),
            KeyOpAction::JsonDel => json(first, JsonPathOp::Del),
            KeyOpAction::JsonNumIncrBy => json(first, JsonPathOp::NumIncrBy(second.parse().ok()?)),
            KeyOpAction::JsonToggle => json(first, JsonPathOp::Toggle),
            KeyOpAction::JsonArrAppend => json(first, JsonPathOp::ArrAppend(parse_json(&second)?)),
            KeyOpAction::JsonStrAppend => {
                if second.is_empty() {
                    return None;
                }
                json(first, JsonPathOp::StrAppend(second))
            }
            KeyOpAction::JsonClear => json(first, JsonPathOp::Clear),
        }
    }
}

/// A count that is worth sending: `0` would pop nothing and read as a bug.
fn positive_count(raw: &str) -> Option<u64> {
    raw.parse::<u64>().ok().filter(|count| *count > 0)
}

fn parse_json(raw: &str) -> Option<Value> {
    serde_json::from_str(raw).ok()
}

impl Render for ZedisKeyOpDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let mut body = v_flex().w_full().gap_3().child(
            Label::new(i18n_key_ops(cx, &format!("{}_hint", title_key(self.action))))
                .text_xs()
                .text_color(muted),
        );
        for (label, state) in &self.inputs {
            body = body.child(
                v_flex()
                    .gap_1()
                    .child(Label::new(i18n_key_ops(cx, label)).text_xs().text_color(muted))
                    // Inputs paint `size_full` and their state carries
                    // `flex_1`; pin the height so they don't inflate here.
                    .child(Input::new(state).small().h(px(32.))),
            );
        }
        body
    }
}

/// Open the form for `action` and hand what it builds to `on_op`.
///
/// Values that do not describe an operation keep the form open; the hint
/// above the fields already says what each one wants.
pub fn open_key_op_form(
    action: KeyOpAction,
    prefill: KeyOpPrefill,
    on_op: impl Fn(KeyOp, &mut Window, &mut App) + 'static,
    window: &mut Window,
    cx: &mut App,
) {
    let view = cx.new(|cx| ZedisKeyOpDialog::new(action, &prefill, window, cx));
    let view_child = view.clone();
    let view_ok = view.clone();
    ZedisDialog::new(i18n_key_ops(cx, title_key(action)))
        .w(px(400.))
        .ok_text(i18n_key_ops(cx, "run"))
        .cancel_text(i18n_common(cx, "cancel"))
        .button_props(
            dialog_button_props(cx)
                .ok_text(i18n_key_ops(cx, "run"))
                .cancel_text(i18n_common(cx, "cancel")),
        )
        .child(move || view_child.clone())
        .on_ok(move |_, window, cx| {
            let Some(op) = view_ok.read(cx).key_op(cx) else {
                return false;
            };
            on_op(op, window, cx);
            true
        })
        .open(window, cx);
}

/// Run `op` against `key` — behind a second, plainly-worded confirmation
/// when it destroys data. The confirmation comes after the form rather
/// than as a checkbox inside it: the dialog that collects "keep rows 0 to
/// 9" is the wrong place to also carry the warning that everything else
/// goes away. `title_key` names the operation's `_confirm` text.
pub fn run_key_op_confirmed(
    server_state: Entity<ZedisServerState>,
    key: SharedString,
    op: KeyOp,
    title_key: &'static str,
    window: &mut Window,
    cx: &mut App,
) {
    let destructive = op.is_destructive();
    let run = move |cx: &mut App| {
        server_state.update(cx, |state, cx| state.run_key_operation(key.clone(), op.clone(), cx));
    };
    if !destructive {
        run(cx);
        return;
    }
    let confirm = i18n_key_ops(cx, &format!("{title_key}_confirm"));
    let confirm_title = i18n_common(cx, "remove_title");
    ZedisDialog::new_alert(confirm_title, confirm.to_string())
        .button_props(dialog_button_props(cx))
        .on_ok(move |_, window, cx| {
            run(cx);
            window.close_dialog(cx);
            true
        })
        .open(window, cx);
}

/// Open the form for `action`, then run what it builds against `key`.
pub fn open_key_op_dialog(
    server_state: Entity<ZedisServerState>,
    key: SharedString,
    action: KeyOpAction,
    window: &mut Window,
    cx: &mut App,
) {
    open_key_op_dialog_prefilled(server_state, key, action, KeyOpPrefill::default(), window, cx);
}

/// [`open_key_op_dialog`] with fields the caller already knows filled in.
pub fn open_key_op_dialog_prefilled(
    server_state: Entity<ZedisServerState>,
    key: SharedString,
    action: KeyOpAction,
    prefill: KeyOpPrefill,
    window: &mut Window,
    cx: &mut App,
) {
    let title_key = title_key(action);
    open_key_op_form(
        action,
        prefill,
        move |op, window, cx| {
            run_key_op_confirmed(server_state.clone(), key.clone(), op, title_key, window, cx);
        },
        window,
        cx,
    );
}
