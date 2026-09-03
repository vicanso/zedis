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

//! Single-key cross-server copy dialog body.
//!
//! Reached from the editor key bar's "…" menu. Lets the user pick a target
//! server + db (and whether to overwrite an existing key), then the editor
//! copies the selected key's value and TTL to that destination via
//! `DUMP` / `RESTORE`. This view is just the form body; the editor wraps it
//! in a [`zedis_ui::ZedisDialog`] and reads the selection on OK.

use crate::connection::{ConflictMode, get_servers};
use crate::states::i18n_copy;
use gpui::{Entity, SharedString, Window, prelude::*, px};
use gpui_kit::component::{
    ActiveTheme, Sizable,
    button::{Button, ButtonVariants},
    checkbox::Checkbox,
    h_flex,
    input::{Input, InputState},
    label::Label,
    v_flex,
};

pub struct ZedisCopyKeyDialog {
    /// `(id, name)` of every configured server — the copy targets.
    servers: Vec<(SharedString, SharedString)>,
    /// Currently picked target server id.
    selected_server_id: Option<SharedString>,
    /// Target db number input.
    db_input: Entity<InputState>,
    /// Overwrite an existing destination key (`RESTORE … REPLACE`).
    overwrite: bool,
    /// Whether to show the overwrite checkbox + RESTORE version note. The
    /// cross-server *diff* reuses this dialog purely as a server / db picker,
    /// so it hides both.
    show_overwrite: bool,
}

impl ZedisCopyKeyDialog {
    pub fn new(
        source_id: SharedString,
        source_db: usize,
        show_overwrite: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let servers: Vec<(SharedString, SharedString)> = get_servers()
            .unwrap_or_default()
            .into_iter()
            .map(|s| (s.id.into(), s.name.into()))
            .collect();
        // Default to the first server that isn't the source (the common
        // "copy elsewhere" intent), falling back to the first available.
        let selected_server_id = servers
            .iter()
            .find(|(id, _)| id != &source_id)
            .or_else(|| servers.first())
            .map(|(id, _)| id.clone());
        let db_input = cx.new(|cx| InputState::new(window, cx).default_value(source_db.to_string()));
        Self {
            servers,
            selected_server_id,
            db_input,
            overwrite: false,
            show_overwrite,
        }
    }

    /// Picked target server id, if any server is configured.
    pub fn target_server_id(&self) -> Option<SharedString> {
        self.selected_server_id.clone()
    }

    /// Target db number (0 when the field is blank / unparsable).
    pub fn target_db(&self, cx: &gpui::App) -> usize {
        self.db_input.read(cx).value().trim().parse().unwrap_or(0)
    }

    /// Conflict policy from the overwrite checkbox.
    pub fn conflict(&self) -> ConflictMode {
        if self.overwrite {
            ConflictMode::Overwrite
        } else {
            ConflictMode::Skip
        }
    }

    fn select_server(&mut self, id: SharedString, cx: &mut Context<Self>) {
        self.selected_server_id = Some(id);
        cx.notify();
    }
}

impl Render for ZedisCopyKeyDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let selected = self.selected_server_id.clone();

        let mut server_row = h_flex().gap_2().flex_wrap();
        for (id, name) in &self.servers {
            let is_selected = selected.as_ref() == Some(id);
            let id_click = id.clone();
            let button = Button::new(SharedString::from(format!("copy-srv-{id}")))
                .small()
                .label(name.clone());
            let button = if is_selected {
                button.primary()
            } else {
                button.outline()
            };
            server_row = server_row.child(
                button.on_click(cx.listener(move |this, _, _window, cx| this.select_server(id_click.clone(), cx))),
            );
        }

        v_flex()
            .w_full()
            .gap_3()
            .child(Label::new(i18n_copy(cx, "target_server")).text_xs().text_color(muted))
            .child(server_row)
            .child(Label::new(i18n_copy(cx, "target_db")).text_xs().text_color(muted))
            .child(Input::new(&self.db_input).small().w(px(120.)))
            .when(self.show_overwrite, |this| {
                this.child(
                    Checkbox::new("copy-overwrite")
                        .label(i18n_copy(cx, "overwrite"))
                        .checked(self.overwrite)
                        .on_click(cx.listener(|this, checked: &bool, _window, cx| {
                            this.overwrite = *checked;
                            cx.notify();
                        })),
                )
                .child(
                    Label::new(i18n_copy(cx, "version_note"))
                        .text_xs()
                        .text_color(cx.theme().yellow),
                )
            })
    }
}
