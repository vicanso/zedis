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
//! in a [`zedis_ui::ZedisDialog`] and reads the selection on OK. The
//! cross-server diff and the config comparison open the same body as a
//! plain server / db picker ([`CopyDialogPurpose::PickServer`]).

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

/// What the dialog is opened for, which decides what it shows and where it
/// starts.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CopyDialogPurpose {
    /// The editor's *Copy to…*: a write. The overwrite checkbox and the DUMP
    /// compatibility note are shown, and the *source* server is preselected
    /// — the first remote server used to be, so one click on Copy wrote the
    /// key to a server the user had not chosen, possibly production (#235).
    /// Copying into another database of the same server, the safe case, is
    /// one click; a remote target is a choice.
    CopyKey,
    /// A server / db picker for a read — the cross-server diff, the config
    /// comparison. Nothing is written, so the first server that is not the
    /// source is preselected (comparing a server with itself says nothing),
    /// and the copy's checkbox and note are not shown.
    PickServer,
}

/// The server a freshly opened dialog starts on: `None` only when no server
/// is configured.
fn default_target(
    servers: &[(SharedString, SharedString)],
    source_id: &SharedString,
    purpose: CopyDialogPurpose,
) -> Option<SharedString> {
    let preferred = match purpose {
        CopyDialogPurpose::CopyKey => servers.iter().find(|(id, _)| id == source_id),
        CopyDialogPurpose::PickServer => servers.iter().find(|(id, _)| id != source_id),
    };
    preferred.or_else(|| servers.first()).map(|(id, _)| id.clone())
}

pub struct ZedisCopyKeyDialog {
    /// `(id, name)` of every configured server — the copy targets.
    servers: Vec<(SharedString, SharedString)>,
    /// The server the key is read from: preselected for a copy, and the one
    /// target the DUMP compatibility note does not apply to (#236).
    source_id: SharedString,
    /// Currently picked target server id.
    selected_server_id: Option<SharedString>,
    /// Target db number input.
    db_input: Entity<InputState>,
    /// Overwrite an existing destination key (`RESTORE … REPLACE`).
    overwrite: bool,
    purpose: CopyDialogPurpose,
}

impl ZedisCopyKeyDialog {
    pub fn new(
        source_id: SharedString,
        source_db: usize,
        purpose: CopyDialogPurpose,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let servers: Vec<(SharedString, SharedString)> = get_servers()
            .unwrap_or_default()
            .into_iter()
            .map(|s| (s.id.into(), s.name.into()))
            .collect();
        let selected_server_id = default_target(&servers, &source_id, purpose);
        let db_input = cx.new(|cx| InputState::new(window, cx).default_value(source_db.to_string()));
        Self {
            servers,
            source_id,
            selected_server_id,
            db_input,
            overwrite: false,
            purpose,
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
        let show_overwrite = self.purpose == CopyDialogPurpose::CopyKey;
        // Within one server the copy is a DUMP and a RESTORE of the server's
        // own payload, which it always reads: the note about a target that
        // cannot would be a warning about nothing (#236).
        let same_server = selected.as_ref() == Some(&self.source_id);

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
            .when(show_overwrite, |this| {
                this.child(
                    Checkbox::new("copy-overwrite")
                        .label(i18n_copy(cx, "overwrite"))
                        .checked(self.overwrite)
                        .on_click(cx.listener(|this, checked: &bool, _window, cx| {
                            this.overwrite = *checked;
                            cx.notify();
                        })),
                )
                .child(if same_server {
                    Label::new(i18n_copy(cx, "same_server_note"))
                        .text_xs()
                        .text_color(muted)
                } else {
                    Label::new(i18n_copy(cx, "version_note"))
                        .text_xs()
                        .text_color(cx.theme().yellow)
                })
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn servers() -> Vec<(SharedString, SharedString)> {
        ["aivencloud", "upstash", "local"]
            .into_iter()
            .map(|name| (SharedString::from(format!("id-{name}")), SharedString::from(name)))
            .collect()
    }

    /// A copy starts on the server the key is on, whatever its place in the
    /// list, so a remote target is chosen rather than clicked past (#235); a
    /// picker for a read starts on another server, since comparing a server
    /// with itself says nothing. A source that is no longer configured, or
    /// the only server, falls back to the first.
    #[test]
    fn a_copy_starts_on_its_source_and_a_picker_on_another_server() {
        let servers = servers();
        let local = SharedString::from("id-local");
        assert_eq!(
            default_target(&servers, &local, CopyDialogPurpose::CopyKey).as_deref(),
            Some("id-local")
        );
        assert_eq!(
            default_target(&servers, &local, CopyDialogPurpose::PickServer).as_deref(),
            Some("id-aivencloud")
        );
        let gone = SharedString::from("id-gone");
        assert_eq!(
            default_target(&servers, &gone, CopyDialogPurpose::CopyKey).as_deref(),
            Some("id-aivencloud")
        );
        let only = &servers[2..];
        assert_eq!(
            default_target(only, &local, CopyDialogPurpose::PickServer).as_deref(),
            Some("id-local")
        );
        assert_eq!(default_target(&[], &local, CopyDialogPurpose::CopyKey), None);
    }
}
