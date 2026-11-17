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

use super::ZedisSidebar;
use crate::helpers::SidebarServerAction;
use crate::states::{ZedisGlobalStore, i18n_common, i18n_servers, i18n_sidebar};
use gpui::{App, Context, DismissEvent, Pixels, SharedString, WeakEntity, Window, px};
use gpui_kit::component::menu::PopupMenu;
use zedis_ui::{ZedisFormField, ZedisFormOptions};

/// The new group dialog's one field.
const NEW_GROUP_FIELD: &str = "group";

/// Audited fixed modal width for the Move-to-New-Group prompt.
const NEW_GROUP_DIALOG_WIDTH: Pixels = px(420.);

/// What a connection's menu in the list offers, and about which connection.
pub(super) struct ServerMenu {
    pub(super) sidebar: WeakEntity<ZedisSidebar>,
    pub(super) id: SharedString,
    /// The group it is in; `None` for none.
    pub(super) group: Option<String>,
    /// Every named group the list shows.
    pub(super) groups: Vec<String>,
    pub(super) ungrouped: SharedString,
    /// The link toggle: `Some(true)` offers Disconnect, `Some(false)`
    /// Reconnect, `None` (no workspace tab on it) neither.
    pub(super) link: Option<bool>,
}

impl ServerMenu {
    /// Populate the right-click context menu for a connection entry and keep
    /// the sidebar aware of which row currently owns an open popup.
    pub(super) fn build(&self, menu: PopupMenu, window: &mut Window, cx: &mut Context<PopupMenu>) -> PopupMenu {
        // Built as it opens: the row drops its tooltip and holds its open wash until dismiss.
        let sidebar = self.sidebar.clone();
        let _ = sidebar.update(cx, |sidebar, cx| {
            sidebar.set_menu_open(Some(self.id.clone()), cx);
        });
        cx.subscribe_self(move |_, _: &DismissEvent, cx| {
            let _ = sidebar.update(cx, |sidebar, cx| {
                sidebar.set_menu_open(None, cx);
            });
        })
        .detach();
        let id = self.id.clone();
        let action = |make: fn(SharedString) -> SidebarServerAction| Box::new(make(id.clone()));
        let menu = menu
            .menu(i18n_sidebar(cx, "server_open"), action(SidebarServerAction::Open))
            .menu(
                i18n_sidebar(cx, "server_open_new_tab"),
                action(SidebarServerAction::OpenInNewTab),
            )
            .separator()
            .menu(i18n_sidebar(cx, "server_edit"), action(SidebarServerAction::Edit))
            .menu(i18n_sidebar(cx, "server_clone"), action(SidebarServerAction::Clone))
            .menu(
                i18n_sidebar(cx, "server_diagnose"),
                action(SidebarServerAction::Diagnose),
            );
        // Every group the list shows, out of the one it is in, and a new one
        // named in a dialog (#219) — which is why the submenu is there even
        // before any group exists: creating the first one used to mean
        // opening the connection's form.
        let (id, current, groups, ungrouped, new_group) = (
            self.id.clone(),
            self.group.clone(),
            self.groups.clone(),
            self.ungrouped.clone(),
            i18n_sidebar(cx, "server_new_group"),
        );
        let menu = menu.submenu(
            i18n_sidebar(cx, "server_move_to_group"),
            window,
            cx,
            move |submenu, _, _| {
                let to = |group: &str| {
                    Box::new(SidebarServerAction::MoveToGroup {
                        id: id.clone(),
                        group: group.to_string().into(),
                    })
                };
                let submenu = groups.iter().fold(submenu, |submenu, group| {
                    submenu.menu_with_check(group.clone(), current.as_ref() == Some(group), to(group))
                });
                submenu
                    .menu_with_check(ungrouped.clone(), current.is_none(), to(""))
                    .separator()
                    .menu(
                        new_group.clone(),
                        Box::new(SidebarServerAction::MoveToNewGroup(id.clone())),
                    )
            },
        );
        let menu = menu.separator();
        let menu = match self.link {
            Some(true) => menu.menu(
                i18n_sidebar(cx, "server_disconnect"),
                action(SidebarServerAction::Disconnect),
            ),
            Some(false) => menu.menu(
                i18n_sidebar(cx, "server_reconnect"),
                action(SidebarServerAction::Reconnect),
            ),
            None => menu,
        };
        menu.menu(i18n_sidebar(cx, "server_delete"), action(SidebarServerAction::Delete))
    }
}

/// Name a group and move `server_id` into it — the sidebar menu's
/// Move to Group › New Group… (#219). One field, the connection form's own
/// wording for it; a name that is already a group simply moves there, and
/// surrounding spaces are not part of a name (`set_server_group` trims).
pub(crate) fn open_new_group_dialog(server_id: SharedString, window: &mut Window, cx: &mut App) {
    ZedisFormOptions::new(vec![
        ZedisFormField::new(NEW_GROUP_FIELD, i18n_servers(cx, "group"))
            .placeholder(i18n_servers(cx, "group_placeholder"))
            .required()
            .focus(),
    ])
    .title(i18n_sidebar(cx, "new_group_title"))
    .confirm_label(i18n_sidebar(cx, "new_group_confirm"))
    .cancel_label(i18n_common(cx, "cancel"))
    .on_dialog_submit(move |values, _window, cx| {
        let name = values
            .get(NEW_GROUP_FIELD)
            .map(|name| name.trim().to_string())
            .unwrap_or_default();
        if name.is_empty() {
            return false;
        }
        cx.global::<ZedisGlobalStore>()
            .clone()
            .update(cx, |state, cx| state.set_server_group(&server_id, Some(name), cx));
        true
    })
    .dialog_width(NEW_GROUP_DIALOG_WIDTH)
    .open_dialog(window, cx);
}
