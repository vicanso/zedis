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

//! Stream live tail (`XREAD BLOCK $` and consumer-group `XREADGROUP BLOCK >`).

use super::*;
use crate::connection::StreamGroupTailSpec;
use gpui_kit::component::{
    checkbox::Checkbox,
    radio::{Radio, RadioGroup},
};

/// Block timeout per `XREAD` / `XREADGROUP` round (ms). Short enough that a
/// stop / key-switch is reflected promptly when the loop next wakes.
const TAIL_BLOCK_MS: u64 = 2000;
/// Max entries per tail round.
const TAIL_COUNT: usize = 200;
/// Ring-buffer cap — a hot stream trims oldest entries beyond this so a
/// long-running tail can't grow memory unbounded.
const TAIL_CAP: usize = 5000;
const DEFAULT_TAIL_CONSUMER: &str = "zedis-tail";

#[derive(Clone, Default)]
pub(super) struct StreamTailMode {
    pub group: Option<SharedString>,
    pub noack: bool,
}

struct StreamTailModeDialog {
    server_state: Entity<ZedisServerState>,
    use_group: bool,
    noack: bool,
    group_input: Entity<InputState>,
}

impl StreamTailModeDialog {
    fn new(
        server_state: Entity<ZedisServerState>,
        current: &StreamTailMode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let initial_group = current.group.clone().unwrap_or_default();
        let use_group = !initial_group.is_empty();
        let group_input = cx.new(|cx| InputState::new(window, cx).default_value(initial_group));
        Self {
            server_state,
            use_group,
            noack: if current.group.is_some() { current.noack } else { true },
            group_input,
        }
    }

    fn known_groups(&self, cx: &App) -> Vec<SharedString> {
        self.server_state
            .read(cx)
            .value()
            .and_then(|v| v.stream_value())
            .and_then(|sv| sv.info.as_ref())
            .map(|info| info.groups.iter().map(|g| g.name.clone()).collect())
            .unwrap_or_default()
    }
}

impl Render for StreamTailModeDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let groups = self.known_groups(cx);
        let mode_idx = usize::from(self.use_group);

        v_flex()
            .gap_3()
            .w_full()
            .child(
                RadioGroup::horizontal("stream-tail-mode-radio")
                    .selected_index(Some(mode_idx))
                    .child(Radio::new("tail-plain").label(i18n_stream_editor(cx, "tail_mode_plain")))
                    .child(Radio::new("tail-group").label(i18n_stream_editor(cx, "tail_mode_group")))
                    .on_click(cx.listener(|this, idx: &usize, _, cx| {
                        this.use_group = *idx == 1;
                        cx.notify();
                    })),
            )
            .when(self.use_group, |el| {
                el.when(!groups.is_empty(), |inner| {
                    let mut row = h_flex().gap_1().flex_wrap();
                    for (idx, name) in groups.into_iter().enumerate() {
                        let pick = name.clone();
                        row = row.child(
                            Button::new(("stream-tail-group-pick", idx))
                                .xsmall()
                                .outline()
                                .label(name)
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    let pick = pick.clone();
                                    this.group_input.update(cx, |input, cx| {
                                        input.set_value(pick, window, cx);
                                    });
                                    cx.notify();
                                })),
                        );
                    }
                    inner.child(row)
                })
                .child(
                    v_flex()
                        .gap_1()
                        .child(
                            Label::new(i18n_stream_editor(cx, "col_group"))
                                .text_xs()
                                .text_color(muted),
                        )
                        .child(Input::new(&self.group_input)),
                )
                .child(
                    Checkbox::new("stream-tail-noack")
                        .checked(self.noack)
                        .label(i18n_stream_editor(cx, "tail_noack"))
                        .on_click(cx.listener(|this, checked: &bool, _, cx| {
                            this.noack = *checked;
                            cx.notify();
                        })),
                )
            })
    }
}

impl ZedisStreamEditor {
    pub(super) fn open_tail_mode_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.server_state.update(cx, |s, cx| s.fetch_stream_info(cx));
        let server_state = self.server_state.clone();
        let current = self.tail_mode.clone();
        let dialog_view = cx.new(|cx| StreamTailModeDialog::new(server_state, &current, window, cx));
        let body = dialog_view.clone();
        let group_input = dialog_view.read(cx).group_input.clone();
        let editor_weak = cx.weak_entity();
        let ok_label = i18n_common(cx, "confirm");
        let cancel_label = i18n_common(cx, "cancel");

        ZedisDialog::new(i18n_stream_editor(cx, "tail_start_tooltip"))
            .w(px(440.))
            .ok_text(ok_label.clone())
            .cancel_text(cancel_label.clone())
            .button_props(dialog_button_props(cx).ok_text(ok_label).cancel_text(cancel_label))
            .child(move || body.clone())
            .on_ok(move |_, _window, cx| {
                let (use_group, group_val, noack) = {
                    let d = dialog_view.read(cx);
                    (d.use_group, d.group_input.read(cx).value().trim().to_string(), d.noack)
                };
                if use_group && group_val.is_empty() {
                    return false;
                }
                if let Some(editor) = editor_weak.upgrade() {
                    editor.update(cx, |this, cx| {
                        this.tail_mode = StreamTailMode {
                            group: use_group.then(|| SharedString::from(group_val)),
                            noack,
                        };
                        if !this.tailing {
                            this.toggle_tail(cx);
                        } else {
                            cx.notify();
                        }
                    });
                }
                true
            })
            .open(window, cx);
        group_input.update(cx, |field, cx| field.focus(window, cx));
    }

    /// Start/stop the live-tail loop (`XREAD BLOCK $` or `XREADGROUP BLOCK >`).
    pub(super) fn toggle_tail(&mut self, cx: &mut Context<Self>) {
        if self.tailing {
            self.tail_task = None;
            self.tailing = false;
            cx.notify();
            return;
        }

        let server_state = self.server_state.clone();
        let server_id = server_state.read(cx).server_id().to_string();
        let db = server_state.read(cx).db();
        let writable = server_state.read(cx).can(Capability::MutateContainer);
        let Some(key) = server_state.read(cx).key() else {
            return;
        };
        let key = key.to_string();
        let group_spec = self
            .tail_mode
            .group
            .as_ref()
            .filter(|_| writable)
            .map(|group| StreamGroupTailSpec {
                group: group.to_string(),
                consumer: DEFAULT_TAIL_CONSUMER.to_string(),
                noack: self.tail_mode.noack,
            });
        let entity = cx.entity().downgrade();
        let (tx, rx) = channel::unbounded::<Vec<RedisStreamEntry>>();

        let task = cx.spawn(async move |_handle, cx| {
            let at = ServerDb::new(server_id, db);
            let opened = match group_spec {
                Some(spec) => StreamTail::open_group(&at, &key, spec).await,
                None => StreamTail::open(&at, &key).await,
            };
            let mut tail = match opened {
                Ok(tail) => tail,
                Err(e) => {
                    let kind = e.connection_kind();
                    let _ = entity.update(cx, |this: &mut ZedisStreamEditor, cx| {
                        this.tailing = false;
                        this.tail_task = None;
                        this.notify_tail_failed(kind, cx);
                        cx.notify();
                    });
                    return;
                }
            };
            let bg = cx.background_spawn(async move {
                while let Ok(entries) = tail.next_batch(TAIL_BLOCK_MS, TAIL_COUNT).await {
                    if entries.is_empty() {
                        continue;
                    }
                    let entries = project_stream_entries(entries);
                    if tx.send(entries).await.is_err() {
                        break;
                    }
                }
            });

            while let Ok(batch) = rx.recv().await {
                let key_cl = key.clone();
                let r = entity.update(cx, |this: &mut ZedisStreamEditor, cx| {
                    this.server_state.update(cx, |state, cx| {
                        state.append_tail_entries(&key_cl, batch, TAIL_CAP, cx);
                    });
                });
                if r.is_err() {
                    break;
                }
            }

            drop(bg);
            let _ = entity.update(cx, |this: &mut ZedisStreamEditor, cx| {
                this.tailing = false;
                this.tail_task = None;
                cx.notify();
            });
        });

        self.tail_task = Some(task);
        self.tailing = true;
        cx.notify();
    }

    fn notify_tail_failed(&self, kind: ConnectionErrorKind, cx: &mut Context<Self>) {
        let reason = i18n_status_bar(cx, kind.reason_key());
        let msg: SharedString = format!("{}: {}", i18n_stream_editor(cx, "tail_stopped"), reason).into();
        cx.global::<ZedisGlobalStore>().clone().update(cx, |_, cx| {
            cx.emit(GlobalEvent::Notification(NotificationAction::new_error(msg)));
        });
    }
}
