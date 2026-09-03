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

//! Multi-server export picker body.
//!
//! Reached from the Home tail "Export" card. Lets the user tick which
//! configured servers to export and whether credentials are included; the
//! servers view then serializes the selection to a JSON array and copies it to
//! the clipboard. This view is just the form body — the caller wraps it in a
//! [`zedis_ui::ZedisDialog`] and reads the selection on OK.

use crate::connection::{RedisServer, get_servers};
use crate::helpers::encrypt_share;
use crate::states::{ZedisGlobalStore, i18n_servers};
use gpui::{App, ClipboardItem, Entity, ScrollHandle, SharedString, Window, div, prelude::*, px};
use gpui_component::{
    ActiveTheme, Sizable, WindowExt,
    button::{Button, ButtonVariants},
    checkbox::Checkbox,
    h_flex,
    input::{Input, InputState},
    label::Label,
    notification::Notification,
    scroll::{Scrollbar, ScrollbarMode},
    v_flex,
};
use rust_i18n::t;
use std::collections::HashSet;
use tracing::warn;

pub struct ZedisExportServersDialog {
    /// Every configured server — the export candidates.
    servers: Vec<RedisServer>,
    /// Ids currently ticked for export. Defaults to all.
    selected: HashSet<String>,
    /// Include credential fields in the exported JSON.
    include_secrets: bool,
    /// Optional passphrase. Non-empty ⇒ the export is emitted as an encrypted
    /// share token (`ZEDIS1.…`) instead of plain JSON; empty keeps the
    /// original plain-JSON export unchanged.
    passphrase_state: Entity<InputState>,
    /// Drives the server list's native scroll area and its visible
    /// scrollbar (the capped list scrolls on its own; the dialog body's
    /// scroller stays dormant because the body always fits).
    list_scroll: ScrollHandle,
}

impl ZedisExportServersDialog {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let servers = get_servers().unwrap_or_else(|e| {
            warn!(error = %e, "export dialog: server list unavailable");
            Vec::new()
        });
        // Default to everything selected — the common "export my setup" intent.
        let selected = servers.iter().map(|s| s.id.clone()).collect();
        let passphrase_state = cx.new(|cx| {
            InputState::new(window, cx)
                .masked(true)
                .placeholder(i18n_servers(cx, "export_passphrase_placeholder"))
        });
        Self {
            servers,
            selected,
            include_secrets: false,
            passphrase_state,
            list_scroll: ScrollHandle::default(),
        }
    }

    /// The ticked servers, in list order.
    pub fn selected_servers(&self) -> Vec<RedisServer> {
        self.servers
            .iter()
            .filter(|s| self.selected.contains(&s.id))
            .cloned()
            .collect()
    }

    /// The export payload for the current selection: plain JSON, or — when a
    /// passphrase is set — an encrypted share token any machine can open with
    /// that passphrase. `None` when nothing is ticked (or serialization /
    /// encryption fails).
    pub fn export_payload(&self, cx: &App) -> Option<String> {
        let selected = self.selected_servers();
        if selected.is_empty() {
            return None;
        }
        let json = RedisServer::to_export_json_many(&selected, self.include_secrets).ok()?;
        let passphrase = self.passphrase_state.read(cx).value().to_string();
        if passphrase.is_empty() {
            Some(json)
        } else {
            encrypt_share(&json, &passphrase).ok()
        }
    }

    /// Copy the ticked servers as a JSON array to the clipboard. No-op when
    /// nothing is ticked. (Save to file is the dialog's primary OK action.)
    fn copy_to_clipboard(&self, window: &mut Window, cx: &mut Context<Self>) {
        let count = self.selected_servers().len();
        let Some(payload) = self.export_payload(cx) else {
            return;
        };
        cx.write_to_clipboard(ClipboardItem::new_string(payload));
        let locale = cx.global::<ZedisGlobalStore>().read(cx).locale().to_string();
        window.push_notification(
            Notification::success(SharedString::from(
                t!("servers.export_done_multi", count = count, locale = locale).to_string(),
            )),
            cx,
        );
    }

    fn toggle(&mut self, id: String, on: bool, cx: &mut Context<Self>) {
        if on {
            self.selected.insert(id);
        } else {
            self.selected.remove(&id);
        }
        cx.notify();
    }
}

impl Render for ZedisExportServersDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let warning_color = cx.theme().yellow;
        let include_on = self.include_secrets;
        let selected_count = self.selected.len();
        let total = self.servers.len();

        // Native scroll + a sibling `Scrollbar` (the help-popover pattern):
        // `max_h` keeps the box adaptive, and the visible bar gives the
        // affordance the bare `overflow_y_scroll` lacked. Deliberately NOT
        // gpui-component's `overflow_y_scrollbar` — nesting it inside the
        // dialog body's own scroller is the double-scroller shape form.rs
        // warns against, and `Scrollable + max_h` clips instead of scrolling.
        let mut list = v_flex()
            .id("export-servers-list")
            .w_full()
            .gap_1()
            .max_h(px(280.))
            .overflow_y_scroll()
            .track_scroll(&self.list_scroll);
        for server in &self.servers {
            let id = server.id.clone();
            let checked = self.selected.contains(&id);
            let label = SharedString::from(format!("{}  ·  {}:{}", server.name, server.host, server.port));
            list = list.child(
                Checkbox::new(SharedString::from(format!("export-srv-{id}")))
                    .label(label)
                    .checked(checked)
                    .on_click(cx.listener(move |this, on: &bool, _window, cx| {
                        this.toggle(id.clone(), *on, cx);
                    })),
            );
        }

        let mut secrets_btn = Button::new("export-servers-secrets")
            .small()
            .label(i18n_servers(cx, "export_include_secrets"));
        secrets_btn = if include_on {
            secrets_btn.primary()
        } else {
            secrets_btn.outline()
        };
        let secrets_btn = secrets_btn.on_click(cx.listener(|this, _, _window, cx| {
            this.include_secrets = !this.include_secrets;
            cx.notify();
        }));

        // Secondary action: copy the selection to the clipboard. Save to file
        // is the dialog's primary OK action.
        let copy_btn = Button::new("export-servers-copy")
            .small()
            .outline()
            .label(i18n_servers(cx, "export_copy_clipboard"))
            .on_click(cx.listener(|this, _, window, cx| this.copy_to_clipboard(window, cx)));

        v_flex()
            .w_full()
            .gap_3()
            .child(
                Label::new(i18n_servers(cx, "export_servers_hint"))
                    .text_xs()
                    .text_color(muted),
            )
            .child(h_flex().gap_2().child(secrets_btn).child(copy_btn))
            // Optional share passphrase: filled ⇒ the export (copy and save
            // alike) becomes an encrypted `ZEDIS1.…` token instead of JSON.
            .child(Input::new(&self.passphrase_state).appearance(true).w_full())
            .when(include_on, |this| {
                this.child(
                    Label::new(i18n_servers(cx, "export_secrets_warning"))
                        .text_xs()
                        .text_color(warning_color),
                )
            })
            .child(
                div().relative().w_full().child(list).child(
                    div()
                        .absolute()
                        .top_0()
                        .right_0()
                        .bottom_0()
                        .child(Scrollbar::vertical(&self.list_scroll).mode(ScrollbarMode::Always)),
                ),
            )
            .child(
                Label::new(SharedString::from(format!("{selected_count} / {total}")))
                    .text_xs()
                    .text_color(muted),
            )
    }
}
