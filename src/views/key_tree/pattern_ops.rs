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

//! Arbitrary glob-pattern bulk operations dialog: preview how many keys a
//! `SCAN MATCH` pattern reaches, then batch-delete, batch-expire, or persist
//! them.

use super::*;
use crate::connection::{ExpireCondition, PrefixImpact, ServerDb, count_keys_matching, floors};
use gpui_kit::component::radio::{Radio, RadioGroup};

#[derive(Clone, Copy, PartialEq, Eq)]
enum PatternOpMode {
    Delete,
    SetTtl,
    Persist,
}

const PATTERN_OP_MODES: [(PatternOpMode, &str); 3] = [
    (PatternOpMode::Delete, "pattern_ops_delete"),
    (PatternOpMode::SetTtl, "pattern_ops_set_ttl"),
    (PatternOpMode::Persist, "pattern_ops_persist"),
];

const TTL_CONDITION_CHOICES: [(Option<ExpireCondition>, &str); 5] = [
    (None, "batch_ttl_cond_none"),
    (Some(ExpireCondition::Nx), "batch_ttl_cond_nx"),
    (Some(ExpireCondition::Xx), "batch_ttl_cond_xx"),
    (Some(ExpireCondition::Gt), "batch_ttl_cond_gt"),
    (Some(ExpireCondition::Lt), "batch_ttl_cond_lt"),
];

enum PatternSubmission {
    MissingPattern,
    InvalidTtl,
    Delete {
        pattern: SharedString,
    },
    SetTtl {
        pattern: SharedString,
        ttl_secs: u64,
        condition: Option<ExpireCondition>,
    },
    Persist {
        pattern: SharedString,
    },
}

struct PatternOpsConfig {
    server_id: String,
    at: ServerDb,
    can_delete: bool,
    can_mutate: bool,
    show_condition: bool,
    initial_pattern: String,
}

struct PatternOpsForm {
    server_id: String,
    at: ServerDb,
    can_delete: bool,
    can_mutate: bool,
    show_condition: bool,
    mode: PatternOpMode,
    condition: Option<ExpireCondition>,
    pattern: Entity<InputState>,
    ttl: Entity<InputState>,
    counting: bool,
    impact: Option<Result<PrefixImpact, String>>,
    _count_task: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl PatternOpsForm {
    fn new(config: PatternOpsConfig, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let PatternOpsConfig {
            server_id,
            at,
            can_delete,
            can_mutate,
            show_condition,
            initial_pattern,
        } = config;
        let pattern = cx.new(|cx| {
            let mut state =
                InputState::new(window, cx).placeholder(i18n_key_tree(cx, "pattern_ops_pattern_placeholder"));
            if !initial_pattern.is_empty() {
                state.set_value(initial_pattern, window, cx);
            }
            state
        });
        let ttl = cx.new(|cx| InputState::new(window, cx).placeholder(i18n_key_tree(cx, "batch_ttl_placeholder")));
        let sub = cx.subscribe_in(&pattern, window, |this, _, event, _window, cx| {
            if let InputEvent::PressEnter { .. } = event {
                this.preview_count(cx);
            } else if let InputEvent::Change = event {
                this.impact = None;
                cx.notify();
            }
        });
        let mode = if can_delete {
            PatternOpMode::Delete
        } else {
            PatternOpMode::SetTtl
        };
        Self {
            server_id,
            at,
            can_delete,
            can_mutate,
            show_condition,
            mode,
            condition: None,
            pattern,
            ttl,
            counting: false,
            impact: None,
            _count_task: None,
            _subscriptions: vec![sub],
        }
    }

    fn preview_count(&mut self, cx: &mut Context<Self>) {
        let pattern = self.pattern.read(cx).value().trim().to_string();
        if pattern.is_empty() {
            return;
        }
        let at = self.at.clone();
        self.counting = true;
        self.impact = None;
        cx.notify();
        self._count_task = Some(cx.spawn(async move |this, cx| {
            let result = count_keys_matching(&at, &pattern).await.map_err(|e| e.to_string());
            let _ = this.update(cx, |this, cx| {
                this.counting = false;
                this.impact = Some(result);
                cx.notify();
            });
        }));
    }

    fn available_modes(&self) -> Vec<(PatternOpMode, &'static str)> {
        PATTERN_OP_MODES
            .iter()
            .copied()
            .filter(|(mode, _)| match mode {
                PatternOpMode::Delete => self.can_delete,
                PatternOpMode::SetTtl | PatternOpMode::Persist => self.can_mutate,
            })
            .collect()
    }

    fn preview_text(&self, cx: &App) -> Option<SharedString> {
        let locale = cx.global::<ZedisGlobalStore>().read(cx).locale().to_string();
        let text = match self.impact.as_ref()? {
            Err(error) => error.clone(),
            Ok(impact) if impact.keys == 0 && impact.complete => {
                t!("key_tree.pattern_ops_preview_none", locale = &locale).to_string()
            }
            Ok(impact) => {
                let count = group_thousands(impact.keys);
                let mut text = if impact.complete {
                    t!("key_tree.pattern_ops_preview_count", count = count, locale = &locale).to_string()
                } else {
                    t!("key_tree.pattern_ops_preview_partial", count = count, locale = &locale).to_string()
                };
                if let Some(bytes) = impact.estimated_bytes() {
                    let size = humansize::format_size(bytes, humansize::DECIMAL);
                    text.push(' ');
                    text.push_str(&t!(
                        "key_tree.delete_folder_impact_memory",
                        size = size,
                        locale = &locale
                    ));
                }
                text
            }
        };
        Some(text.into())
    }

    fn submission(&self, cx: &App) -> PatternSubmission {
        let pattern: SharedString = self.pattern.read(cx).value().trim().to_string().into();
        if pattern.is_empty() {
            return PatternSubmission::MissingPattern;
        }
        match self.mode {
            PatternOpMode::Delete => PatternSubmission::Delete { pattern },
            PatternOpMode::SetTtl => match ttl_secs(self.ttl.read(cx).value().as_ref()) {
                Some(secs) => PatternSubmission::SetTtl {
                    pattern,
                    ttl_secs: secs,
                    condition: self.show_condition.then_some(self.condition).flatten(),
                },
                None => PatternSubmission::InvalidTtl,
            },
            PatternOpMode::Persist => PatternSubmission::Persist { pattern },
        }
    }
}

impl Render for PatternOpsForm {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let danger = cx.theme().danger;
        let modes = self.available_modes();
        let mode_index = modes.iter().position(|(m, _)| *m == self.mode);
        let cond_index = TTL_CONDITION_CHOICES.iter().position(|(c, _)| *c == self.condition);
        let preview = self.preview_text(cx);
        let delete_warning = escalate_dangerous_body(
            cx,
            &self.server_id,
            i18n_key_tree(cx, "pattern_ops_delete_warning").to_string(),
        );

        v_flex()
            .gap_3()
            .w_full()
            .child(
                v_flex()
                    .gap_1()
                    .child(
                        Label::new(i18n_key_tree(cx, "pattern_ops_pattern_label"))
                            .text_xs()
                            .text_color(muted),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .w_full()
                            .child(div().flex_1().child(Input::new(&self.pattern).small()))
                            .child(
                                Button::new("pattern-ops-preview-btn")
                                    .outline()
                                    .small()
                                    .loading(self.counting)
                                    .icon(Icon::new(IconName::Search))
                                    .label(i18n_key_tree(cx, "pattern_ops_preview"))
                                    .on_click(cx.listener(|this, _, _window, cx| {
                                        this.preview_count(cx);
                                    })),
                            ),
                    ),
            )
            .when_some(preview, |this, text| {
                this.child(Label::new(text).text_xs().text_color(muted))
            })
            .child(
                v_flex()
                    .gap_1()
                    .child(
                        Label::new(i18n_key_tree(cx, "pattern_ops_action_label"))
                            .text_xs()
                            .text_color(muted),
                    )
                    .child(
                        RadioGroup::vertical("pattern-ops-mode")
                            .selected_index(mode_index)
                            .children(
                                modes
                                    .iter()
                                    .map(|(_, key)| Radio::new(*key).label(i18n_key_tree(cx, key))),
                            )
                            .on_click(cx.listener(move |this, index: &usize, _window, cx| {
                                let modes = this.available_modes();
                                if let Some((mode, _)) = modes.get(*index) {
                                    this.mode = *mode;
                                    cx.notify();
                                }
                            })),
                    ),
            )
            .when(self.mode == PatternOpMode::Delete, |this| {
                this.child(Label::new(delete_warning).text_xs().text_color(danger))
            })
            .when(self.mode == PatternOpMode::SetTtl, |this| {
                this.child(Input::new(&self.ttl).small())
                    .when(self.show_condition, |this| {
                        this.child(
                            v_flex()
                                .gap_1()
                                .child(
                                    Label::new(i18n_key_tree(cx, "batch_ttl_condition"))
                                        .text_xs()
                                        .text_color(muted),
                                )
                                .child(
                                    RadioGroup::vertical("pattern-ops-ttl-cond")
                                        .selected_index(cond_index)
                                        .children(
                                            TTL_CONDITION_CHOICES
                                                .iter()
                                                .map(|(_, key)| Radio::new(*key).label(i18n_key_tree(cx, key))),
                                        )
                                        .on_click(cx.listener(|this, index: &usize, _window, cx| {
                                            this.condition = TTL_CONDITION_CHOICES.get(*index).and_then(|(c, _)| *c);
                                            cx.notify();
                                        })),
                                ),
                        )
                    })
            })
    }
}

/// Open the pattern-based bulk operations dialog.
pub(super) fn open_pattern_ops_dialog(tree: &ZedisKeyTree, window: &mut Window, cx: &mut Context<ZedisKeyTree>) {
    let keyword = tree.keyword_state.read(cx).value().trim().to_string();
    let initial = if keyword.is_empty() {
        String::new()
    } else if keyword.contains('*') || keyword.contains('?') {
        keyword
    } else {
        format!("*{keyword}*")
    };
    open_pattern_ops_dialog_with_pattern(tree.server_state.clone(), initial, window, cx);
}

/// Open the pattern-based bulk operations dialog pre-populated with `initial_pattern`.
pub(crate) fn open_pattern_ops_dialog_with_pattern(
    server_state: Entity<ZedisServerState>,
    initial_pattern: String,
    window: &mut Window,
    cx: &mut App,
) {
    let (server_id, at, can_delete, can_mutate, show_condition) = {
        let state = server_state.read(cx);
        (
            state.server_id().to_string(),
            state.at(),
            state.can(Capability::DeleteKeys),
            state.can(Capability::SetTtl),
            state.supports(floors::EXPIRE_CONDITIONS),
        )
    };
    if !can_delete && !can_mutate {
        return;
    }

    let form = cx.new(|cx| {
        PatternOpsForm::new(
            PatternOpsConfig {
                server_id,
                at,
                can_delete,
                can_mutate,
                show_condition,
                initial_pattern,
            },
            window,
            cx,
        )
    });
    let body = form.clone();
    let pattern_field = form.read(cx).pattern.clone();
    let ok_label = i18n_key_tree(cx, "pattern_ops_apply");
    let cancel_label = i18n_common(cx, "cancel");

    ZedisDialog::new(i18n_key_tree(cx, "pattern_ops_title"))
        .w(px(420.))
        .ok_text(ok_label.clone())
        .cancel_text(cancel_label.clone())
        .button_props(dialog_button_props(cx).ok_text(ok_label).cancel_text(cancel_label))
        .child(move || body.clone())
        .on_ok(move |_, window, cx| match form.read(cx).submission(cx) {
            PatternSubmission::MissingPattern => {
                window.push_notification(Notification::error(i18n_key_tree(cx, "pattern_ops_pattern_empty")), cx);
                false
            }
            PatternSubmission::InvalidTtl => {
                window.push_notification(Notification::error(i18n_common(cx, "ttl_invalid")), cx);
                false
            }
            PatternSubmission::Delete { pattern } => {
                server_state.update(cx, |state, cx| state.delete_keys_by_pattern(pattern, cx));
                true
            }
            PatternSubmission::SetTtl {
                pattern,
                ttl_secs,
                condition,
            } => {
                server_state.update(cx, |state, cx| {
                    state.batch_set_ttl_by_pattern(pattern, Some(ttl_secs), condition, cx)
                });
                true
            }
            PatternSubmission::Persist { pattern } => {
                server_state.update(cx, |state, cx| state.batch_set_ttl_by_pattern(pattern, None, None, cx));
                true
            }
        })
        .open(window, cx);
    pattern_field.update(cx, |field, cx| field.focus(window, cx));
}
