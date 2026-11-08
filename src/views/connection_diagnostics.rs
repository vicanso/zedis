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

//! Staged connection diagnostics dialog body.
//!
//! Opened from the server form's "Diagnose" button, a connection's menu and
//! the editor's "can't reach the server" panel. One row per stage (DNS → TCP
//! → SSH auth → SSH tunnel → TLS → AUTH → PING); a failed stage shows the
//! raw error plus a localized remediation hint, and the stages after it are
//! marked skipped. The desktop runs the probes itself and updates each row
//! as it finishes; a page asks the bridge, which is what dials the server
//! there, and fills the rows in from its answer.

#[cfg(target_family = "wasm")]
use crate::connection::diagnose_on_bridge;
#[cfg(not(target_family = "wasm"))]
use crate::connection::run_diagnostics;
use crate::connection::{DiagHint, DiagOutcome, DiagStage, DiagStatus, RedisServer, diag_stages};
use crate::states::{i18n_common, i18n_servers};
use gpui::{App, AsyncApp, Div, SharedString, Task, WeakEntity, Window, div, prelude::*, px};
use gpui_kit::component::{ActiveTheme, Icon, IconName, Sizable, h_flex, label::Label, spinner::Spinner, v_flex};
use std::time::Duration;
use zedis_ui::ZedisDialog;

enum RowState {
    Pending,
    Running,
    Done(DiagOutcome),
}

/// The render pieces of one stage row: the status slot, the muted text on
/// the right, and the optional error / hint sub-lines.
struct RowParts {
    slot: Div,
    right: Option<SharedString>,
    error: Option<SharedString>,
    /// `(text, warn)` — warn renders yellow, otherwise muted (informational).
    hint: Option<(SharedString, bool)>,
}

pub struct ZedisConnectionDiagnostics {
    rows: Vec<(DiagStage, RowState)>,
    /// Why there are no answers at all: the bridge could not be asked.
    failure: Option<SharedString>,
    /// Cancels the probe sequence when the dialog (and this view) is
    /// dropped; in-flight SSH work on the tokio runtime just gets its
    /// result discarded.
    _task: Task<()>,
}

fn stage_key(stage: DiagStage) -> &'static str {
    match stage {
        DiagStage::Dns => "diag_stage_dns",
        DiagStage::Tcp => "diag_stage_tcp",
        DiagStage::SshAuth => "diag_stage_ssh_auth",
        DiagStage::SshTunnel => "diag_stage_ssh_tunnel",
        DiagStage::Tls => "diag_stage_tls",
        DiagStage::Auth => "diag_stage_auth",
        DiagStage::Ping => "diag_stage_ping",
    }
}

fn hint_key(hint: DiagHint) -> &'static str {
    match hint {
        DiagHint::Dns => "diag_hint_dns",
        DiagHint::TcpRefused => "diag_hint_tcp_refused",
        DiagHint::TcpUnreachable => "diag_hint_tcp_unreachable",
        DiagHint::SshAuth => "diag_hint_ssh_auth",
        DiagHint::SshTunnel => "diag_hint_ssh_tunnel",
        DiagHint::Tls => "diag_hint_tls",
        DiagHint::AuthRequired => "diag_hint_auth_required",
        DiagHint::AuthRejected => "diag_hint_auth_rejected",
        DiagHint::AuthNotConfigured => "diag_auth_none",
        DiagHint::Redis => "diag_hint_redis",
    }
}

impl ZedisConnectionDiagnostics {
    pub fn new(server: RedisServer, cx: &mut Context<Self>) -> Self {
        let rows = diag_stages(&server)
            .into_iter()
            .map(|stage| (stage, RowState::Pending))
            .collect();
        let task = cx.spawn(async move |this, cx| {
            Self::run(server, this, cx).await;
        });
        Self {
            rows,
            failure: None,
            _task: task,
        }
    }

    /// Update one stage row; returns false when the view is gone (dialog
    /// closed) so the probe sequence can bail out.
    fn set_row(this: &WeakEntity<Self>, cx: &mut AsyncApp, stage: DiagStage, state: RowState) -> bool {
        this.update(cx, |view, cx| {
            if let Some(row) = view.rows.iter_mut().find(|(s, _)| *s == stage) {
                row.1 = state;
            }
            cx.notify();
        })
        .is_ok()
    }

    /// Run the probes here, each row updated as its stage finishes.
    #[cfg(not(target_family = "wasm"))]
    async fn run(server: RedisServer, this: WeakEntity<Self>, cx: &mut AsyncApp) {
        run_diagnostics(&server, |stage, outcome| {
            Self::set_row(&this, cx, stage, outcome.map_or(RowState::Running, RowState::Done))
        })
        .await;
    }

    /// Ask the bridge, which dials the server for a page. It answers every
    /// stage at once, so the first one spins until then.
    #[cfg(target_family = "wasm")]
    async fn run(server: RedisServer, this: WeakEntity<Self>, cx: &mut AsyncApp) {
        let Some(first) = diag_stages(&server).first().copied() else {
            return;
        };
        if !Self::set_row(&this, cx, first, RowState::Running) {
            return;
        }
        match diagnose_on_bridge(server).await {
            Ok(rows) => {
                for row in rows {
                    if !Self::set_row(&this, cx, row.stage, RowState::Done(row.outcome())) {
                        return;
                    }
                }
            }
            Err(e) => {
                let _ = this.update(cx, |view, cx| {
                    for row in view.rows.iter_mut() {
                        row.1 = RowState::Pending;
                    }
                    view.failure = Some(e.message.into());
                    cx.notify();
                });
            }
        }
    }
}

impl Render for ZedisConnectionDiagnostics {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let green = cx.theme().green;
        let red = cx.theme().red;
        let yellow = cx.theme().yellow;

        let mut list = v_flex().w_full().gap_2();
        for (stage, state) in &self.rows {
            let name = i18n_servers(cx, stage_key(*stage));
            // Fixed-width status slot so the stage labels align.
            let slot = div().w(px(18.)).flex_none().flex().justify_center();
            let parts = match state {
                RowState::Pending => RowParts {
                    slot: slot.text_color(muted).child("○"),
                    right: None,
                    error: None,
                    hint: None,
                },
                RowState::Running => RowParts {
                    slot: slot.child(Spinner::new().with_size(px(14.)).color(muted)),
                    right: None,
                    error: None,
                    hint: None,
                },
                RowState::Done(outcome) => match outcome.status {
                    DiagStatus::Success => {
                        let mut right: Vec<String> = vec![];
                        if let Some(detail) = &outcome.detail {
                            right.push(detail.clone());
                        }
                        if outcome.elapsed > Duration::ZERO {
                            right.push(format!("{}ms", outcome.elapsed.as_millis()));
                        }
                        RowParts {
                            slot: slot.text_color(green).child(Icon::new(IconName::Check).small()),
                            right: if right.is_empty() {
                                None
                            } else {
                                Some(right.join(" · ").into())
                            },
                            error: None,
                            hint: None,
                        }
                    }
                    DiagStatus::Failed => RowParts {
                        slot: slot.text_color(red).child(Icon::new(IconName::CircleX).small()),
                        right: None,
                        error: outcome.error.clone().map(Into::into),
                        hint: outcome.hint.map(|hint| (i18n_servers(cx, hint_key(hint)), true)),
                    },
                    DiagStatus::Skipped => RowParts {
                        slot: slot.text_color(muted).child("—"),
                        right: Some(
                            outcome
                                .detail
                                .clone()
                                .map(Into::into)
                                .unwrap_or_else(|| i18n_servers(cx, "diag_status_skipped")),
                        ),
                        error: None,
                        // Informational note (e.g. "no password configured"),
                        // rendered muted rather than as a warning.
                        hint: outcome.hint.map(|hint| (i18n_servers(cx, hint_key(hint)), false)),
                    },
                },
            };

            let mut row = v_flex().w_full().gap_1().child(
                h_flex()
                    .w_full()
                    .gap_2()
                    .child(parts.slot)
                    .child(Label::new(name).text_sm())
                    .child(div().flex_1())
                    .when_some(parts.right, |this, text| {
                        this.child(Label::new(text).text_xs().text_color(muted))
                    }),
            );
            if let Some(error) = parts.error {
                row = row.child(Label::new(error).text_xs().text_color(red).ml(px(26.)));
            }
            if let Some((hint, warn)) = parts.hint {
                let color = if warn { yellow } else { muted };
                row = row.child(Label::new(hint).text_xs().text_color(color).ml(px(26.)));
            }
            list = list.child(row);
        }
        list.when_some(self.failure.clone(), |list, failure| {
            list.child(Label::new(failure).text_xs().text_color(red))
        })
    }
}

/// Open the staged diagnostics dialog for the given (possibly unsaved)
/// server config. Stacks on top of the server form dialog.
pub fn open_connection_diagnostics(server: RedisServer, window: &mut Window, cx: &mut App) {
    let view = cx.new(|cx| ZedisConnectionDiagnostics::new(server, cx));
    let view_child = view.clone();
    ZedisDialog::new(i18n_servers(cx, "diag_title"))
        .w(px(520.))
        .ok_text(i18n_common(cx, "close"))
        .child(move || view_child.clone())
        .open(window, cx);
}
