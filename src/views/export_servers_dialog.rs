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

use super::export_to_file_global;
use crate::connection::{RedisServer, get_servers};
use crate::states::{i18n_common, i18n_servers};
use gpui::{SharedString, Window, prelude::*, px};
use gpui_component::{
    ActiveTheme, Sizable,
    button::{Button, ButtonVariants},
    checkbox::Checkbox,
    h_flex,
    label::Label,
    v_flex,
};
use std::collections::HashSet;

pub struct ZedisExportServersDialog {
    /// Every configured server — the export candidates.
    servers: Vec<RedisServer>,
    /// Ids currently ticked for export. Defaults to all.
    selected: HashSet<String>,
    /// Include credential fields in the exported JSON.
    include_secrets: bool,
}

impl ZedisExportServersDialog {
    pub fn new(_window: &mut Window, _cx: &mut Context<Self>) -> Self {
        let servers = get_servers().unwrap_or_default();
        // Default to everything selected — the common "export my setup" intent.
        let selected = servers.iter().map(|s| s.id.clone()).collect();
        Self {
            servers,
            selected,
            include_secrets: false,
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

    pub fn include_secrets(&self) -> bool {
        self.include_secrets
    }

    /// Save the ticked servers as a JSON array to a file (Save dialog defaults
    /// to `~/Downloads`). No-op when nothing is ticked.
    fn save_to_file(&self, cx: &mut Context<Self>) {
        let selected = self.selected_servers();
        if selected.is_empty() {
            return;
        }
        let json = RedisServer::to_export_json_many(&selected, self.include_secrets).unwrap_or_default();
        let success = i18n_common(cx, "json_exported");
        let error = i18n_common(cx, "json_export_failed");
        export_to_file_global(cx, json.into_bytes(), "zedis-servers.json", success, error);
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

        let mut list = v_flex()
            .id("export-servers-list")
            .w_full()
            .gap_1()
            .max_h(px(280.))
            .overflow_y_scroll();
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

        // Secondary action: save the selection to a file (Copy stays the
        // dialog's primary OK). Defaults to ~/Downloads.
        let save_btn = Button::new("export-servers-save")
            .small()
            .outline()
            .label(i18n_servers(cx, "export_save_file"))
            .on_click(cx.listener(|this, _, _window, cx| this.save_to_file(cx)));

        v_flex()
            .w_full()
            .gap_3()
            .child(
                Label::new(i18n_servers(cx, "export_servers_hint"))
                    .text_xs()
                    .text_color(muted),
            )
            .child(h_flex().gap_2().child(secrets_btn).child(save_btn))
            .when(include_on, |this| {
                this.child(
                    Label::new(i18n_servers(cx, "export_secrets_warning"))
                        .text_xs()
                        .text_color(warning_color),
                )
            })
            .child(list)
            .child(
                Label::new(SharedString::from(format!("{selected_count} / {total}")))
                    .text_xs()
                    .text_color(muted),
            )
    }
}
