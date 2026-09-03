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
//! Opened from the server form's "Diagnose" button. Runs the probes from
//! `connection::diagnostics` sequentially and renders one row per stage
//! (DNS → TCP → SSH auth → SSH tunnel → TLS → AUTH → PING), updating live
//! as each finishes. A failed stage shows the raw error plus a localized
//! remediation hint; the stages after it are marked skipped.

use crate::connection::{
    DiagHint, DiagOutcome, DiagStage, DiagStatus, RedisServer, diag_stages, diag_timeout, dial_endpoint, probe_dns,
    probe_redis, probe_ssh_auth, probe_ssh_tunnel, probe_tcp,
};
use crate::states::{i18n_common, i18n_servers};
use gpui::{App, AsyncApp, Div, SharedString, Task, WeakEntity, Window, div, prelude::*, px};
use gpui_component::{ActiveTheme, Icon, IconName, Sizable, h_flex, label::Label, spinner::Spinner, v_flex};
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
        Self { rows, _task: task }
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

    /// Mark every not-yet-finished stage as skipped (after a failure).
    fn skip_rest(this: &WeakEntity<Self>, cx: &mut AsyncApp) {
        let _ = this.update(cx, |view, cx| {
            for row in view.rows.iter_mut() {
                if matches!(row.1, RowState::Pending | RowState::Running) {
                    row.1 = RowState::Done(DiagOutcome::skipped(None, None));
                }
            }
            cx.notify();
        });
    }

    async fn run(server: RedisServer, this: WeakEntity<Self>, cx: &mut AsyncApp) {
        let timeout = diag_timeout(&server);
        let (dial_host, dial_port) = dial_endpoint(&server);

        // DNS + TCP — not for a Unix socket, which has neither (the stage
        // list already omits the rows).
        if !server.is_unix_socket() {
            if !Self::set_row(&this, cx, DiagStage::Dns, RowState::Running) {
                return;
            }
            let (dns, addrs) = probe_dns(&dial_host, dial_port, timeout).await;
            let failed = dns.status == DiagStatus::Failed;
            if !Self::set_row(&this, cx, DiagStage::Dns, RowState::Done(dns)) {
                return;
            }
            if failed {
                Self::skip_rest(&this, cx);
                return;
            }

            if !Self::set_row(&this, cx, DiagStage::Tcp, RowState::Running) {
                return;
            }
            let tcp = probe_tcp(&addrs, timeout).await;
            let failed = tcp.status == DiagStatus::Failed;
            if !Self::set_row(&this, cx, DiagStage::Tcp, RowState::Done(tcp)) {
                return;
            }
            if failed {
                Self::skip_rest(&this, cx);
                return;
            }
        }

        // SSH auth + tunnel target
        if server.is_ssh_tunnel() {
            if !Self::set_row(&this, cx, DiagStage::SshAuth, RowState::Running) {
                return;
            }
            let (auth, session) = probe_ssh_auth(&server).await;
            if !Self::set_row(&this, cx, DiagStage::SshAuth, RowState::Done(auth)) {
                return;
            }
            let Some(session) = session else {
                Self::skip_rest(&this, cx);
                return;
            };
            if !Self::set_row(&this, cx, DiagStage::SshTunnel, RowState::Running) {
                return;
            }
            let tunnel = probe_ssh_tunnel(session, &server).await;
            let failed = tunnel.status == DiagStatus::Failed;
            if !Self::set_row(&this, cx, DiagStage::SshTunnel, RowState::Done(tunnel)) {
                return;
            }
            if failed {
                Self::skip_rest(&this, cx);
                return;
            }
        }

        // TLS / AUTH / PING: one real end-to-end connect, attributed by
        // error classification (see connection::diagnostics). Spin the
        // first of those rows while the connect runs.
        let first = if server.tls.unwrap_or(false) {
            DiagStage::Tls
        } else {
            DiagStage::Auth
        };
        if !Self::set_row(&this, cx, first, RowState::Running) {
            return;
        }
        let probe = probe_redis(&server).await;
        if !Self::set_row(&this, cx, DiagStage::Tls, RowState::Done(probe.tls)) {
            return;
        }
        if !Self::set_row(&this, cx, DiagStage::Auth, RowState::Done(probe.auth)) {
            return;
        }
        Self::set_row(&this, cx, DiagStage::Ping, RowState::Done(probe.ping));
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
        list
    }
}

/// Open the staged diagnostics dialog for the given (possibly unsaved)
/// server config. Stacks on top of the server form dialog.
pub fn open_connection_diagnostics(server: RedisServer, window: &mut Window, cx: &mut App) {
    let view = cx.new(|cx| ZedisConnectionDiagnostics::new(server, cx));
    let view_child = view.clone();
    ZedisDialog::new(i18n_servers(cx, "diag_title"))
        .w(px(520.))
        .ok_text(i18n_common(cx, "confirm"))
        .child(move || view_child.clone())
        .open(window, cx);
}
