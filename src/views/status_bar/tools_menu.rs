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

//! The Tools menu: the server's panels in four groups by purpose, each
//! entry kept, disabled with its reason, or left out by what the server and
//! this connection can do.

use super::*;

/// Everything that decides which Tools-menu entries are enabled: the
/// module / version / server-type gates, the access mode (Import Keys and
/// FLUSH need writes), and the probed command matrix.
pub(super) struct ToolsMenuGates {
    pub(super) supports_search: bool,
    pub(super) supports_acl: bool,
    pub(super) supports_functions: bool,
    pub(super) supports_topology: bool,
    pub(super) readonly: bool,
    pub(super) features: Arc<ServerFeatures>,
}

/// One Tools-menu entry, built when the menu opens and moved into the
/// submenu that draws it.
struct ToolEntry {
    label: SharedString,
    icon: Icon,
    action: ServerToolsAction,
    disabled: bool,
}

impl ZedisStatusBar {
    /// Build the "Tools" dropdown: four submenus by purpose — observability,
    /// query & scripting, keys & data, administration — then Topology and the
    /// capability matrix, so the top level stays eight rows however many
    /// panels the server can open. Items dispatch [`ServerToolsAction`],
    /// handled centrally on the `Zedis` root; a submenu inherits the
    /// dropdown's action context, so its items reach the same handlers.
    ///
    /// What the server does not have — a module that is not loaded, a
    /// version below the feature's floor, a command it answers `unknown
    /// command` to — is left out rather than greyed: it is nothing this user
    /// can change, the ⌘K palette leaves it out the same way, and Server
    /// capabilities lists every command with its verdict. What can change
    /// stays in the menu, disabled with the reason: a command this ACL user
    /// is denied, and a write on a read-only connection. A submenu with
    /// nothing left in it is left out too.
    pub(super) fn render_tools_menu(
        this: PopupMenu,
        gates: &ToolsMenuGates,
        window: &mut Window,
        cx: &mut Context<PopupMenu>,
    ) -> PopupMenu {
        let ToolsMenuGates {
            supports_search,
            supports_acl,
            supports_functions,
            supports_topology,
            readonly,
            features,
        } = gates;
        let features: &ServerFeatures = features;
        let locale = cx.global::<ZedisGlobalStore>().read(cx).locale().to_string();
        let tool = |label: SharedString,
                    icon: Icon,
                    action: ServerToolsAction,
                    required: &[ServerCommand],
                    writes: bool|
         -> Option<ToolEntry> {
            let statuses: Vec<(ServerCommand, CommandStatus)> = required
                .iter()
                .map(|command| (*command, features.status(*command)))
                .filter(|(_, status)| !status.is_usable())
                .collect();
            if statuses.iter().any(|(_, status)| *status == CommandStatus::Missing) {
                return None;
            }
            // Read-only wins the wording: it is the reason the user can undo.
            let reason = if writes && *readonly {
                Some(i18n_common(cx, "disable_in_readonly"))
            } else {
                statuses.first().map(|(command, _)| {
                    t!("features.menu_suffix", command = command.label(), locale = &locale)
                        .to_string()
                        .into()
                })
            };
            Some(ToolEntry {
                label: match &reason {
                    Some(reason) => format!("{label}  ·  {reason}").into(),
                    None => label,
                },
                icon,
                action,
                disabled: reason.is_some(),
            })
        };

        // A panel this build does not have is not listed
        // (`ServerView::in_this_build`): the browser has no `MONITOR`, no
        // keyspace events, no Pub/Sub and no Topology, and an entry that
        // opened a page saying so was an entry to nothing.
        let observability: Vec<ToolEntry> = [
            ServerView::Monitor
                .in_this_build()
                .then(|| {
                    tool(
                        i18n_monitor(cx, "title"),
                        Icon::new(CustomIconName::Radar),
                        ServerToolsAction::Monitor,
                        ServerView::Monitor.required_commands(),
                        false,
                    )
                })
                .flatten(),
            tool(
                i18n_server_load(cx, "title"),
                Icon::new(CustomIconName::Zap),
                ServerToolsAction::ServerLoad,
                ServerView::ServerLoad.required_commands(),
                false,
            ),
            // HOTKEYS tracking — Redis 8.6; absent from the menu elsewhere.
            tool(
                i18n_hotkeys(cx, "title"),
                Icon::new(CustomIconName::Flame),
                ServerToolsAction::Hotkeys,
                ServerView::Hotkeys.required_commands(),
                false,
            ),
            // `notify-keyspace-events` may be empty: the panel offers a
            // one-click Enable. Its hard dependency is SUBSCRIBE.
            ServerView::KeyspaceNotifications
                .in_this_build()
                .then(|| {
                    tool(
                        i18n_keyspace_notifications(cx, "title"),
                        Icon::new(CustomIconName::AudioWaveform),
                        ServerToolsAction::KeyspaceNotifications,
                        ServerView::KeyspaceNotifications.required_commands(),
                        false,
                    )
                })
                .flatten(),
            // RedisTimeSeries only.
            tool(
                i18n_timeseries(cx, "explorer_title"),
                Icon::new(CustomIconName::Activity),
                ServerToolsAction::TimeSeriesExplorer,
                ServerView::TimeSeriesExplorer.required_commands(),
                false,
            ),
            // Pub/Sub is the editor suite's channel mode, mirrored here next
            // to its observability siblings.
            tool(
                i18n_key_tree(cx, "pubsub_mode"),
                Icon::new(CustomIconName::Rss),
                ServerToolsAction::PubsubMode,
                &[],
                false,
            ),
            tool(
                i18n_server_info(cx, "title"),
                Icon::new(IconName::Info),
                ServerToolsAction::ServerInfo,
                ServerView::ServerInfo.required_commands(),
                false,
            ),
        ]
        .into_iter()
        .flatten()
        .collect();

        let scripting: Vec<ToolEntry> = [
            tool(
                i18n_value_search(cx, "title"),
                Icon::new(IconName::Search),
                ServerToolsAction::ValueSearch,
                ServerView::ValueSearch.required_commands(),
                false,
            ),
            // RediSearch / valkey-search, when the module is loaded.
            supports_search
                .then(|| {
                    tool(
                        i18n_search(cx, "title"),
                        Icon::new(IconName::Search),
                        ServerToolsAction::Search,
                        ServerView::Search.required_commands(),
                        false,
                    )
                })
                .flatten(),
            // `FUNCTION`, Redis 7.0+.
            supports_functions
                .then(|| {
                    tool(
                        i18n_functions(cx, "title"),
                        Icon::new(IconName::Asterisk),
                        ServerToolsAction::Functions,
                        ServerView::Functions.required_commands(),
                        false,
                    )
                })
                .flatten(),
            // The saved scripts are local, so the library opens everywhere;
            // running one needs EVAL and degrades inside.
            tool(
                i18n_lua_scripts(cx, "title"),
                Icon::new(IconName::SquareTerminal),
                ServerToolsAction::LuaScripts,
                &[],
                false,
            ),
        ]
        .into_iter()
        .flatten()
        .collect();

        let mut data: Vec<ToolEntry> = Vec::new();
        // The recycle bin — the local database's, or the bridge's.
        data.extend(tool(
            i18n_trash(cx, "menu"),
            Icon::new(CustomIconName::FileXCorner),
            ServerToolsAction::Trash,
            &[],
            false,
        ));
        // Import: the migration window on the desktop, a JSON/CSV dialog in
        // a page. Disabled, not hidden, on a read-only connection so the user
        // knows where it lives.
        data.extend(tool(
            i18n_status_bar(cx, "import_keys_menu"),
            Icon::new(CustomIconName::Upload),
            ServerToolsAction::ImportKeys,
            &[],
            true,
        ));
        // Export: the migration window on the desktop, a download in a page.
        if saves_files() {
            data.extend(tool(
                i18n_status_bar(cx, "export_keys_menu"),
                Icon::new(CustomIconName::Download),
                ServerToolsAction::ExportKeys,
                &[],
                false,
            ));
        }
        // A window on the desktop, a dialog in the browser.
        data.extend(tool(
            i18n_status_bar(cx, "compare_keys_menu"),
            Icon::new(CustomIconName::GitCompareArrows),
            ServerToolsAction::CompareKeys,
            &[],
            false,
        ));

        let mut admin: Vec<ToolEntry> = [
            // CONFIG GET is the one managed clouds most often take away.
            tool(
                i18n_config_editor(cx, "title"),
                Icon::new(IconName::Settings),
                ServerToolsAction::Config,
                ServerView::Config.required_commands(),
                false,
            ),
            // ACL, Redis 6.0+.
            supports_acl
                .then(|| {
                    tool(
                        i18n_acl(cx, "title"),
                        Icon::new(IconName::CircleUser),
                        ServerToolsAction::Acl,
                        ServerView::Acl.required_commands(),
                        false,
                    )
                })
                .flatten(),
            tool(
                i18n_persistence(cx, "title"),
                Icon::new(CustomIconName::HardDrive),
                ServerToolsAction::Persistence,
                ServerView::Persistence.required_commands(),
                false,
            ),
        ]
        .into_iter()
        .flatten()
        .collect();
        // FLUSHDB / FLUSHALL go through the same destructive-command confirm
        // a typed one hits in the terminal: the menu adds an entry point, not
        // a second policy. Kept apart from the panels below a divider.
        let flush: Vec<ToolEntry> = [
            tool(
                i18n_status_bar(cx, "flush_db_menu"),
                Icon::new(CustomIconName::Eraser),
                ServerToolsAction::FlushDb,
                &[ServerCommand::FlushDb],
                true,
            ),
            tool(
                i18n_status_bar(cx, "flush_all_menu"),
                Icon::new(CustomIconName::DatabaseZap),
                ServerToolsAction::FlushAll,
                &[ServerCommand::FlushAll],
                true,
            ),
        ]
        .into_iter()
        .flatten()
        .collect();
        let flush_after = (!admin.is_empty() && !flush.is_empty()).then_some(admin.len());
        admin.extend(flush);

        let groups = [
            (
                Icon::new(IconName::ChartPie),
                i18n_status_bar(cx, "group_observability"),
                observability,
                None,
            ),
            (
                Icon::new(CustomIconName::Braces),
                i18n_status_bar(cx, "group_scripting"),
                scripting,
                None,
            ),
            (
                Icon::new(CustomIconName::Database),
                i18n_status_bar(cx, "group_data"),
                data,
                None,
            ),
            (
                Icon::new(IconName::Settings),
                i18n_status_bar(cx, "group_admin"),
                admin,
                flush_after,
            ),
        ];
        let topology_label = i18n_topology(cx, "title");
        // The probed command matrix: a count suffix here rather than a badge
        // in the bar.
        let unusable = features.unusable().len();
        let capabilities_label: SharedString = if unusable == 0 {
            format!("{}…", i18n_features(cx, "dialog_title")).into()
        } else {
            format!(
                "{}…  ·  {}",
                i18n_features(cx, "dialog_title"),
                t!("status_bar.capabilities_limited", count = unusable, locale = &locale)
            )
            .into()
        };

        let mut menu = this;
        for (icon, label, entries, separator_at) in groups {
            if entries.is_empty() {
                continue;
            }
            menu = menu.submenu_with_icon(Some(icon), label, window, cx, move |mut submenu, _, _| {
                for (index, entry) in entries.iter().enumerate() {
                    if separator_at == Some(index) {
                        submenu = submenu.separator();
                    }
                    submenu = submenu.menu_with_icon_and_disabled(
                        entry.label.clone(),
                        entry.icon.clone(),
                        Box::new(entry.action),
                        entry.disabled,
                    );
                }
                submenu
            });
        }
        // The page adapts to Cluster / Sentinel / Standalone; absent only
        // while the server type is unknown.
        if *supports_topology && ServerView::Topology.in_this_build() {
            menu = menu.menu_with_icon(
                topology_label,
                Icon::new(CustomIconName::Network),
                Box::new(ServerToolsAction::Topology),
            );
        }
        menu.menu_with_icon(
            capabilities_label,
            Icon::new(CustomIconName::ListCheck),
            Box::new(ServerToolsAction::Capabilities),
        )
        // Everything the server says about itself, read once, to paste into
        // a ticket — beside the matrix, the other thing asked of a server
        // that misbehaves.
        .menu_with_icon(
            i18n_troubleshoot(cx, "menu"),
            // Two `IconName`s are in scope here: the component set's has no
            // FileText.
            Icon::new(gpui_kit::assets::IconName::FileText),
            Box::new(ServerToolsAction::TroubleshootingReport),
        )
    }
}
