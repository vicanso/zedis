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

//! Consumer-group, entry-claim, stream-ID, and XTRIM modal dialogs and forms
//! for [`ZedisStreamEditor`].

use super::ZedisStreamEditor;
use crate::states::{
    StreamRefPolicy, StreamTrim, ZedisGlobalStore, dialog_button_props, escalate_dangerous_body, i18n_common,
    i18n_stream_editor,
};
use gpui::{Context, Entity, Render, SharedString, Window, prelude::*, px};
use gpui_kit::component::{
    ActiveTheme, Sizable, WindowExt,
    button::{Button, ButtonVariants},
    h_flex,
    input::{Input, InputState},
    label::Label,
    v_flex,
};
use rust_i18n::t;
use zedis_ui::ZedisDialog;

impl ZedisStreamEditor {
    /// XGROUP CREATE dialog: group name + start ID (defaults to `$`).
    pub(super) fn create_group_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let name_state =
            cx.new(|cx| InputState::new(window, cx).placeholder(i18n_stream_editor(cx, "group_name_placeholder")));
        let id_state = cx.new(|cx| InputState::new(window, cx).default_value("$"));
        let name_label = i18n_stream_editor(cx, "group_name");
        let id_label = i18n_stream_editor(cx, "start_id");
        let id_hint = i18n_stream_editor(cx, "start_id_hint");
        let body = cx.new(|_| StreamCreateGroupForm {
            name: name_state.clone(),
            id: id_state.clone(),
            name_label,
            id_label,
            id_hint,
        });
        let focus_name = name_state.clone();
        let submit_name = name_state;
        let submit_id = id_state;
        let server_state = self.server_state.clone();

        ZedisDialog::new(i18n_stream_editor(cx, "create_group_title"))
            .w(px(480.))
            .ok_text(i18n_common(cx, "confirm"))
            .cancel_text(i18n_common(cx, "cancel"))
            .button_props(
                dialog_button_props(cx)
                    .ok_text(i18n_common(cx, "confirm"))
                    .cancel_text(i18n_common(cx, "cancel")),
            )
            .child(move || body.clone())
            .on_ok(move |_, _window, cx| {
                let name = submit_name.read(cx).value().to_string();
                if name.trim().is_empty() {
                    // Keep the dialog open until a group name is given.
                    return false;
                }
                let raw_id = submit_id.read(cx).value().to_string();
                let id = if raw_id.trim().is_empty() {
                    "$".to_string()
                } else {
                    raw_id
                };
                server_state.update(cx, |state, cx| {
                    state.create_stream_group(name.trim().to_string().into(), id.into(), cx);
                });
                true
            })
            .open(window, cx);
        focus_name.update(cx, |state, cx| state.focus(window, cx));
    }

    /// XGROUP SETID dialog for a specific group.
    pub(super) fn setid_group_dialog(&mut self, group: SharedString, window: &mut Window, cx: &mut Context<Self>) {
        let id_state = cx.new(|cx| InputState::new(window, cx).default_value("$"));
        let id_hint = i18n_stream_editor(cx, "setid_hint");
        let body = cx.new(|_| StreamSingleInputForm {
            input: id_state.clone(),
            hint: id_hint,
        });
        let focus_id = id_state.clone();
        let submit_id = id_state;
        let server_state = self.server_state.clone();

        ZedisDialog::new(i18n_stream_editor(cx, "setid_title"))
            .w(px(480.))
            .ok_text(i18n_common(cx, "confirm"))
            .cancel_text(i18n_common(cx, "cancel"))
            .button_props(
                dialog_button_props(cx)
                    .ok_text(i18n_common(cx, "confirm"))
                    .cancel_text(i18n_common(cx, "cancel")),
            )
            .child(move || body.clone())
            .on_ok(move |_, _window, cx| {
                let raw_id = submit_id.read(cx).value().to_string();
                if raw_id.trim().is_empty() {
                    return false;
                }
                let group = group.clone();
                server_state.update(cx, |state, cx| {
                    state.set_stream_group_id(group, raw_id.trim().to_string().into(), cx);
                });
                true
            })
            .open(window, cx);
        focus_id.update(cx, |state, cx| state.focus(window, cx));
    }

    /// XGROUP CREATECONSUMER dialog: a consumer name (Redis 6.2+).
    pub(super) fn create_consumer_dialog(&mut self, group: SharedString, window: &mut Window, cx: &mut Context<Self>) {
        let name_state =
            cx.new(|cx| InputState::new(window, cx).placeholder(i18n_stream_editor(cx, "create_consumer_placeholder")));
        let hint = i18n_stream_editor(cx, "create_consumer_hint");
        let body = cx.new(|_| StreamSingleInputForm {
            input: name_state.clone(),
            hint,
        });
        let focus_name = name_state.clone();
        let submit_name = name_state;
        let server_state = self.server_state.clone();

        ZedisDialog::new(i18n_stream_editor(cx, "create_consumer_title"))
            .w(px(480.))
            .ok_text(i18n_common(cx, "confirm"))
            .cancel_text(i18n_common(cx, "cancel"))
            .button_props(
                dialog_button_props(cx)
                    .ok_text(i18n_common(cx, "confirm"))
                    .cancel_text(i18n_common(cx, "cancel")),
            )
            .child(move || body.clone())
            .on_ok(move |_, _window, cx| {
                let name = submit_name.read(cx).value().trim().to_string();
                if name.is_empty() {
                    return false;
                }
                let group = group.clone();
                server_state.update(cx, |state, cx| {
                    state.create_stream_consumer(group, name.into(), cx);
                });
                true
            })
            .open(window, cx);
        focus_name.update(cx, |state, cx| state.focus(window, cx));
    }

    /// XGROUP DELCONSUMER: pick one of the group's consumers.
    ///
    /// A picker rather than a name field, unlike its create counterpart:
    /// deleting the wrong consumer silently discards its pending entries,
    /// and a typo would have to be *right* to be safe. Each choice carries
    /// its pending count, so the cost is on screen at the moment of the
    /// decision rather than in the toast afterwards.
    pub(super) fn delete_consumer_dialog(
        &mut self,
        group: SharedString,
        consumers: Vec<(SharedString, u64)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if consumers.is_empty() {
            return;
        }
        let hint = i18n_stream_editor(cx, "delete_consumer_hint");
        let picker = cx.new(|_| StreamDeleteConsumerPicker {
            consumers,
            selected: None,
            hint,
        });
        let body = picker.clone();
        let server_state = self.server_state.clone();

        ZedisDialog::new(i18n_stream_editor(cx, "delete_consumer_title"))
            .alert()
            .danger()
            .w(px(480.))
            .ok_text(i18n_common(cx, "delete"))
            .cancel_text(i18n_common(cx, "cancel"))
            .child(move || body.clone())
            .on_ok(move |_, _window, cx| {
                let Some(consumer) = picker.read(cx).selected.clone() else {
                    return false;
                };
                let group = group.clone();
                server_state.update(cx, |state, cx| {
                    state.delete_stream_consumer(group, consumer, cx);
                });
                true
            })
            .open(window, cx);
    }

    /// XSETID: the stream's own last-generated id.
    ///
    /// Two steps on purpose. The form takes the id; the confirmation then
    /// says what lowering it means — `XADD` can mint ids that already
    /// existed, and consumer groups keep positions that no longer line up.
    pub(super) fn set_stream_id_dialog(
        &mut self,
        current_last_id: SharedString,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let id_state = cx.new(|cx| InputState::new(window, cx).default_value(current_last_id.to_string()));
        let hint = i18n_stream_editor(cx, "setid_stream_hint");
        let body = cx.new(|_| StreamSingleInputForm {
            input: id_state.clone(),
            hint,
        });
        let focus_id = id_state.clone();
        let submit_id = id_state;
        let server_state = self.server_state.clone();
        let server_id = self.server_state.read(cx).server_id().to_string();

        ZedisDialog::new(i18n_stream_editor(cx, "setid_stream_title"))
            .w(px(480.))
            .ok_text(i18n_common(cx, "confirm"))
            .cancel_text(i18n_common(cx, "cancel"))
            .button_props(
                dialog_button_props(cx)
                    .ok_text(i18n_common(cx, "confirm"))
                    .cancel_text(i18n_common(cx, "cancel")),
            )
            .child(move || body.clone())
            .on_ok(move |_, window, cx| {
                let id = submit_id.read(cx).value().trim().to_string();
                if id.is_empty() {
                    return false;
                }
                let server_state = server_state.clone();
                let message = i18n_stream_editor(cx, "setid_stream_prompt").to_string();
                // Close this form, then ask. Answering `true` after opening
                // the confirm closed the confirm instead — it is the top of
                // the stack by then — so the form stayed, the question was
                // never seen and `XSETID` never sent (the same bug
                // `open_key_op_form` records for the pops).
                window.close_dialog(cx);
                ZedisDialog::new_alert(
                    i18n_stream_editor(cx, "setid_stream_title"),
                    escalate_dangerous_body(cx, &server_id, message),
                )
                .button_props(dialog_button_props(cx).ok_text(i18n_common(cx, "confirm")))
                .on_ok(move |_, window, cx| {
                    server_state.update(cx, |state, cx| {
                        state.set_stream_id(id.clone().into(), cx);
                    });
                    window.close_dialog(cx);
                    true
                })
                .open(window, cx);
                false
            })
            .open(window, cx);
        focus_id.update(cx, |state, cx| state.focus(window, cx));
    }

    /// XGROUP DESTROY confirmation. Destructive — drops the group and
    /// its entire PEL, so it routes through the standard alert dialog.
    pub(super) fn confirm_destroy_group(&mut self, group: SharedString, window: &mut Window, cx: &mut Context<Self>) {
        let locale = cx.global::<ZedisGlobalStore>().read(cx).locale().to_string();
        let message = t!(
            "stream_editor.destroy_group_prompt",
            group = group.as_ref(),
            locale = locale
        )
        .to_string();
        let server_state = self.server_state.clone();
        let server_id = self.server_state.read(cx).server_id().to_string();
        let group_for_ok = group.clone();

        ZedisDialog::new_alert(
            i18n_stream_editor(cx, "destroy_group_title"),
            escalate_dangerous_body(cx, &server_id, message),
        )
        .danger()
        .ok_text(i18n_common(cx, "delete"))
        .cancel_text(i18n_common(cx, "cancel"))
        .on_ok(move |_, window, cx| {
            let group = group_for_ok.clone();
            server_state.update(cx, |state, cx| {
                state.destroy_stream_group(group, cx);
            });
            window.close_dialog(cx);
            true
        })
        .open(window, cx);
    }

    /// XCLAIM dialog for one pending entry: target consumer name (with quick-pick chips for existing consumers).
    pub(super) fn claim_entry_dialog(
        &mut self,
        group: SharedString,
        entry_id: SharedString,
        consumers: Vec<SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let form = cx.new(|cx| StreamClaimForm::new(consumers, window, cx));
        let focus_consumer = form.read(cx).consumer.clone();
        let body = form.clone();
        let server_state = self.server_state.clone();

        ZedisDialog::new(i18n_stream_editor(cx, "claim_title"))
            .w(px(480.))
            .ok_text(i18n_common(cx, "confirm"))
            .cancel_text(i18n_common(cx, "cancel"))
            .button_props(
                dialog_button_props(cx)
                    .ok_text(i18n_common(cx, "confirm"))
                    .cancel_text(i18n_common(cx, "cancel")),
            )
            .child(move || body.clone())
            .on_ok(move |_, _window, cx| {
                let consumer = form.read(cx).consumer.read(cx).value().to_string();
                if consumer.trim().is_empty() {
                    return false;
                }
                let group = group.clone();
                let entry_id = entry_id.clone();
                server_state.update(cx, |state, cx| {
                    state.claim_stream_entry(group, consumer.trim().to_string().into(), entry_id, cx);
                });
                true
            })
            .open(window, cx);
        focus_consumer.update(cx, |state, cx| state.focus(window, cx));
    }

    /// XAUTOCLAIM dialog for one group: target consumer, minimum idle
    /// time and how many entries to claim at most.
    pub(super) fn autoclaim_dialog(
        &mut self,
        group: SharedString,
        consumers: Vec<SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let form = cx.new(|cx| StreamAutoclaimForm::new(consumers, window, cx));
        let focus_consumer = form.read(cx).consumer.clone();
        let body = form.clone();
        let server_state = self.server_state.clone();

        ZedisDialog::new(i18n_stream_editor(cx, "autoclaim_title"))
            .w(px(480.))
            .ok_text(i18n_common(cx, "confirm"))
            .cancel_text(i18n_common(cx, "cancel"))
            .button_props(
                dialog_button_props(cx)
                    .ok_text(i18n_common(cx, "confirm"))
                    .cancel_text(i18n_common(cx, "cancel")),
            )
            .child(move || body.clone())
            .on_ok(move |_, _window, cx| {
                let form_ref = form.read(cx);
                let consumer = form_ref.consumer.read(cx).value().to_string();
                if consumer.trim().is_empty() {
                    return false;
                }
                let Ok(min_idle_ms) = form_ref.min_idle.read(cx).value().trim().parse::<u64>() else {
                    return false;
                };
                let Ok(count) = form_ref.count.read(cx).value().trim().parse::<usize>() else {
                    return false;
                };
                if count == 0 {
                    return false;
                }
                let group = group.clone();
                server_state.update(cx, |state, cx| {
                    state.autoclaim_stream_entries(
                        group,
                        consumer.trim().to_string().into(),
                        min_idle_ms,
                        count.min(1000),
                        cx,
                    );
                });
                true
            })
            .open(window, cx);
        focus_consumer.update(cx, |state, cx| state.focus(window, cx));
    }

    /// XTRIM dialog: one input — a plain number trims by MAXLEN (keep
    /// the newest n), an `ms-seq` id trims by MINID (drop older) — plus,
    /// on Redis 8.2+, the reference-policy toggle (KEEPREF / DELREF /
    /// ACKED) — then the standard destructive confirm with the exact
    /// command spelled out (PROD-escalated).
    pub(super) fn trim_stream_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let show_policy = self.server_state.read(cx).supports_stream_ref_policies();
        let form = cx.new(|cx| StreamTrimForm::new(show_policy, window, cx));
        let focus_threshold = form.read(cx).threshold.clone();
        let body = form.clone();
        let server_state = self.server_state.clone();
        let server_id = self.server_state.read(cx).server_id().to_string();

        ZedisDialog::new(i18n_stream_editor(cx, "trim_title"))
            .w(px(480.))
            .ok_text(i18n_common(cx, "confirm"))
            .cancel_text(i18n_common(cx, "cancel"))
            .button_props(
                dialog_button_props(cx)
                    .ok_text(i18n_common(cx, "confirm"))
                    .cancel_text(i18n_common(cx, "cancel")),
            )
            .child(move || body.clone())
            .on_ok(move |_, window, cx| {
                let (raw, policy) = {
                    let form = form.read(cx);
                    (
                        form.threshold.read(cx).value().trim().to_string(),
                        form.policy_to_send(),
                    )
                };
                let locale = cx.global::<ZedisGlobalStore>().read(cx).locale().to_string();
                let (trim, mut message) = if raw.contains('-') {
                    let message = t!("stream_editor.trim_confirm_minid", id = raw.as_str(), locale = locale);
                    (StreamTrim::MinId(raw.clone()), message.to_string())
                } else if let Ok(n) = raw.parse::<u64>() {
                    let message = t!("stream_editor.trim_confirm_maxlen", n = n, locale = locale);
                    (StreamTrim::MaxLen(n), message.to_string())
                } else {
                    // Neither a count nor an id — keep the dialog open.
                    return false;
                };
                // A non-default reference policy changes what the trim does
                // to consumer groups — spell it in the confirm (the option
                // word is the command syntax, deliberately untranslated).
                if let Some(policy) = policy.filter(|p| *p != StreamRefPolicy::KeepRef) {
                    message = format!("{message} · {}", policy.word());
                }
                // Close the form ourselves before stacking the confirm on
                // top — returning true would auto-close the *topmost*
                // dialog, i.e. the alert we are about to open.
                window.close_dialog(cx);
                let server_state = server_state.clone();
                ZedisDialog::new_alert(
                    i18n_stream_editor(cx, "trim_title"),
                    escalate_dangerous_body(cx, &server_id, message),
                )
                .danger()
                .ok_text(i18n_stream_editor(cx, "trim"))
                .cancel_text(i18n_common(cx, "cancel"))
                .on_ok(move |_, window, cx| {
                    let trim = trim.clone();
                    server_state.update(cx, |state, cx| state.trim_stream(trim, policy, cx));
                    window.close_dialog(cx);
                    true
                })
                .open(window, cx);
                false
            })
            .open(window, cx);
        focus_threshold.update(cx, |state, cx| state.focus(window, cx));
    }
}

/// The XTRIM dialog body as a **view entity** (a dialog body holding an
/// `InputState` must be one — see the CLAUDE.md dialog gotcha, and the
/// policy toggle needs `cx.notify` to repaint anyway): the threshold input
/// plus, on Redis 8.2+, the reference-policy toggle row.
struct StreamTrimForm {
    threshold: Entity<InputState>,
    /// Offer the KEEPREF / DELREF / ACKED toggle (Redis 8.2+ only).
    show_policy: bool,
    policy: StreamRefPolicy,
}

impl StreamTrimForm {
    fn new(show_policy: bool, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let threshold =
            cx.new(|cx| InputState::new(window, cx).placeholder(i18n_stream_editor(cx, "trim_placeholder")));
        Self {
            threshold,
            show_policy,
            policy: StreamRefPolicy::default(),
        }
    }

    /// What the dialog submits: `None` when the server predates the
    /// option words (nothing extra is sent on the wire).
    fn policy_to_send(&self) -> Option<StreamRefPolicy> {
        self.show_policy.then_some(self.policy)
    }
}

impl Render for StreamTrimForm {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        // Option words are command syntax — shown verbatim, tooltips carry
        // the localized meaning.
        let policies: [(StreamRefPolicy, &'static str); 3] = [
            (StreamRefPolicy::KeepRef, "trim_policy_keepref_tooltip"),
            (StreamRefPolicy::DelRef, "trim_policy_delref_tooltip"),
            (StreamRefPolicy::Acked, "trim_policy_acked_tooltip"),
        ];
        v_flex()
            .gap_2()
            .w_full()
            .child(Input::new(&self.threshold))
            .child(Label::new(i18n_stream_editor(cx, "trim_hint")).text_xs())
            .when(self.show_policy, |this| {
                let mut row = h_flex().gap_1().items_center().child(
                    Label::new(i18n_stream_editor(cx, "trim_policy_label"))
                        .text_xs()
                        .text_color(muted),
                );
                for (policy, tooltip_key) in policies {
                    let active = self.policy == policy;
                    row = row.child(
                        Button::new(SharedString::from(format!("stream-trim-{}", policy.word())))
                            .xsmall()
                            .when(active, |b| b.primary())
                            .when(!active, |b| b.outline())
                            .label(policy.word())
                            .tooltip(i18n_stream_editor(cx, tooltip_key))
                            .on_click(cx.listener(move |this, _, _w, cx| {
                                this.policy = policy;
                                cx.notify();
                            })),
                    );
                }
                this.child(row)
            })
    }
}

/// View entity for the `XCLAIM` dialog body (target consumer input + optional
/// quick-pick consumer chips).
struct StreamClaimForm {
    consumer: Entity<InputState>,
    consumers: Vec<SharedString>,
}

impl StreamClaimForm {
    fn new(consumers: Vec<SharedString>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let consumer =
            cx.new(|cx| InputState::new(window, cx).placeholder(i18n_stream_editor(cx, "claim_consumer_placeholder")));
        Self { consumer, consumers }
    }
}

impl Render for StreamClaimForm {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .gap_2()
            .w_full()
            .when(!self.consumers.is_empty(), |this| {
                let mut row = h_flex().gap_1().flex_wrap();
                for (idx, name) in self.consumers.iter().enumerate() {
                    let pick = name.clone();
                    row = row.child(
                        Button::new(("stream-claim-pick", idx))
                            .xsmall()
                            .outline()
                            .label(name.clone())
                            .on_click(cx.listener(move |this, _, window, cx| {
                                let pick = pick.clone();
                                this.consumer.update(cx, |input, cx| {
                                    input.set_value(pick, window, cx);
                                });
                                cx.notify();
                            })),
                    );
                }
                this.child(row)
            })
            .child(Input::new(&self.consumer))
            .child(Label::new(i18n_stream_editor(cx, "claim_hint")).text_xs())
    }
}

/// View entity for the `XAUTOCLAIM` dialog body (target consumer input +
/// quick-pick chips, min idle ms, and count).
struct StreamAutoclaimForm {
    consumer: Entity<InputState>,
    min_idle: Entity<InputState>,
    count: Entity<InputState>,
    consumers: Vec<SharedString>,
}

impl StreamAutoclaimForm {
    fn new(consumers: Vec<SharedString>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let consumer =
            cx.new(|cx| InputState::new(window, cx).placeholder(i18n_stream_editor(cx, "claim_consumer_placeholder")));
        let min_idle = cx.new(|cx| InputState::new(window, cx).default_value("60000"));
        let count = cx.new(|cx| InputState::new(window, cx).default_value("100"));
        Self {
            consumer,
            min_idle,
            count,
            consumers,
        }
    }
}

impl Render for StreamAutoclaimForm {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .gap_2()
            .w_full()
            .when(!self.consumers.is_empty(), |this| {
                let mut row = h_flex().gap_1().flex_wrap();
                for (idx, name) in self.consumers.iter().enumerate() {
                    let pick = name.clone();
                    row = row.child(
                        Button::new(("stream-autoclaim-pick", idx))
                            .xsmall()
                            .outline()
                            .label(name.clone())
                            .on_click(cx.listener(move |this, _, window, cx| {
                                let pick = pick.clone();
                                this.consumer.update(cx, |input, cx| {
                                    input.set_value(pick, window, cx);
                                });
                                cx.notify();
                            })),
                    );
                }
                this.child(row)
            })
            .child(Input::new(&self.consumer))
            .child(Label::new(i18n_stream_editor(cx, "autoclaim_min_idle")).text_xs())
            .child(Input::new(&self.min_idle))
            .child(Label::new(i18n_stream_editor(cx, "autoclaim_count")).text_xs())
            .child(Input::new(&self.count))
            .child(Label::new(i18n_stream_editor(cx, "autoclaim_hint")).text_xs())
    }
}

struct StreamCreateGroupForm {
    name: Entity<InputState>,
    id: Entity<InputState>,
    name_label: SharedString,
    id_label: SharedString,
    id_hint: SharedString,
}

impl Render for StreamCreateGroupForm {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .gap_2()
            .w_full()
            .child(Label::new(self.name_label.clone()).text_xs())
            .child(Input::new(&self.name))
            .child(Label::new(self.id_label.clone()).text_xs())
            .child(Input::new(&self.id))
            .child(Label::new(self.id_hint.clone()).text_xs())
    }
}

struct StreamSingleInputForm {
    input: Entity<InputState>,
    hint: SharedString,
}

impl Render for StreamSingleInputForm {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .gap_2()
            .w_full()
            .child(Input::new(&self.input))
            .child(Label::new(self.hint.clone()).text_xs())
    }
}

struct StreamDeleteConsumerPicker {
    consumers: Vec<(SharedString, u64)>,
    selected: Option<SharedString>,
    hint: SharedString,
}

impl Render for StreamDeleteConsumerPicker {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut row = h_flex().gap_2().flex_wrap();
        for (name, pending) in &self.consumers {
            let active = self.selected.as_ref() == Some(name);
            let choose = name.clone();
            row = row.child(
                Button::new(SharedString::from(format!("stream-del-consumer-{name}")))
                    .small()
                    .when(active, |b| b.primary())
                    .when(!active, |b| b.outline())
                    .label(format!("{name} ({pending})"))
                    .on_click(cx.listener(move |this, _, _window, cx| {
                        this.selected = Some(choose.clone());
                        cx.notify();
                    })),
            );
        }
        v_flex()
            .gap_2()
            .w_full()
            .child(row)
            .child(Label::new(self.hint.clone()).text_xs())
    }
}
