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

use super::{ServerView, ZedisGlobalStore};
use crate::connection::{CommandStatus, ServerCommand};
use crate::helpers::humanize_keystroke;
use gpui::App;
use gpui::SharedString;
use rust_i18n::t;

pub fn i18n_common<'a>(cx: &'a App, key: &'a str) -> SharedString {
    let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
    t!(format!("common.{key}"), locale = locale).into()
}

/// Tooltip for the tool pages' back-to-editor button with the `Esc` shortcut
/// appended — every tool page binds Esc to the same navigation
/// (`NavAction::Back` in the `Workspace` context), so the hint lives here
/// once instead of being repeated at every call site.
pub fn back_to_editor_tooltip(cx: &App) -> SharedString {
    format!(
        "{} ({})",
        i18n_common(cx, "back_to_editor"),
        humanize_keystroke("escape")
    )
    .into()
}

pub fn i18n_multi_search<'a>(cx: &'a App, key: &'a str) -> SharedString {
    let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
    t!(format!("multi_search.{key}"), locale = locale).into()
}

pub fn i18n_hints<'a>(cx: &'a App, key: &'a str) -> SharedString {
    let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
    t!(format!("hints.{key}"), locale = locale).into()
}

/// Toast body for the one-time first-connection hint, with the two global
/// shortcuts resolved for the current platform (⌘ on macOS, Ctrl elsewhere).
pub fn first_connect_hint(cx: &App) -> SharedString {
    let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
    t!(
        "hints.first_connect",
        palette = humanize_keystroke("secondary-k"),
        search = humanize_keystroke("secondary-shift-f"),
        locale = locale
    )
    .into()
}

/// Body of the key tree's no-SCAN banner, with the recent-keys shortcut
/// resolved for the current platform (matches `RecentKeysAction::Toggle`'s
/// `secondary-p` binding).
pub fn key_tree_no_scan_body(cx: &App) -> SharedString {
    let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
    t!(
        "features.key_tree_no_scan_body",
        shortcut = humanize_keystroke("secondary-p"),
        locale = locale
    )
    .into()
}

pub fn i18n_sidebar<'a>(cx: &'a App, key: &'a str) -> SharedString {
    let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
    t!(format!("sidebar.{key}"), locale = locale).into()
}

pub fn i18n_servers<'a>(cx: &'a App, key: &'a str) -> SharedString {
    let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
    t!(format!("servers.{key}"), locale = locale).into()
}

pub fn i18n_command_palette<'a>(cx: &'a App, key: &'a str) -> SharedString {
    let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
    t!(format!("command_palette.{key}"), locale = locale).into()
}

pub fn i18n_recent_keys_palette<'a>(cx: &'a App, key: &'a str) -> SharedString {
    let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
    t!(format!("recent_keys_palette.{key}"), locale = locale).into()
}

pub fn i18n_shortcuts<'a>(cx: &'a App, key: &'a str) -> SharedString {
    let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
    t!(format!("shortcuts.{key}"), locale = locale).into()
}

pub fn i18n_editor<'a>(cx: &'a App, key: &'a str) -> SharedString {
    let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
    t!(format!("editor.{key}"), locale = locale).into()
}

pub fn i18n_key_tree<'a>(cx: &'a App, key: &'a str) -> SharedString {
    let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
    t!(format!("key_tree.{key}"), locale = locale).into()
}

pub fn i18n_status_bar<'a>(cx: &'a App, key: &'a str) -> SharedString {
    let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
    t!(format!("status_bar.{key}"), locale = locale).into()
}

pub fn i18n_list_editor<'a>(cx: &'a App, key: &'a str) -> SharedString {
    let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
    t!(format!("list_editor.{key}"), locale = locale).into()
}

pub fn i18n_kv_table<'a>(cx: &'a App, key: &'a str) -> SharedString {
    let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
    t!(format!("kv_table.{key}"), locale = locale).into()
}

pub fn i18n_set_editor<'a>(cx: &'a App, key: &'a str) -> SharedString {
    let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
    t!(format!("set_editor.{key}"), locale = locale).into()
}

pub fn i18n_zset_editor<'a>(cx: &'a App, key: &'a str) -> SharedString {
    let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
    t!(format!("zset_editor.{key}"), locale = locale).into()
}

pub fn i18n_hash_editor<'a>(cx: &'a App, key: &'a str) -> SharedString {
    let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
    t!(format!("hash_editor.{key}"), locale = locale).into()
}

pub fn i18n_settings<'a>(cx: &'a App, key: &'a str) -> SharedString {
    let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
    t!(format!("settings.{key}"), locale = locale).into()
}

pub fn i18n_metrics<'a>(cx: &'a App, key: &'a str) -> SharedString {
    let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
    t!(format!("metrics.{key}"), locale = locale).into()
}
pub fn i18n_timeseries<'a>(cx: &'a App, key: &'a str) -> SharedString {
    let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
    t!(format!("timeseries.{key}"), locale = locale).into()
}
pub fn i18n_probabilistic<'a>(cx: &'a App, key: &'a str) -> SharedString {
    let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
    t!(format!("probabilistic.{key}"), locale = locale).into()
}
pub fn i18n_hll<'a>(cx: &'a App, key: &'a str) -> SharedString {
    let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
    t!(format!("hll.{key}"), locale = locale).into()
}
pub fn i18n_bitmap<'a>(cx: &'a App, key: &'a str) -> SharedString {
    let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
    t!(format!("bitmap.{key}"), locale = locale).into()
}
pub fn i18n_copy<'a>(cx: &'a App, key: &'a str) -> SharedString {
    let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
    t!(format!("copy.{key}"), locale = locale).into()
}
pub fn i18n_server_load<'a>(cx: &'a App, key: &'a str) -> SharedString {
    let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
    t!(format!("server_load.{key}"), locale = locale).into()
}
pub fn i18n_server_info<'a>(cx: &'a App, key: &'a str) -> SharedString {
    let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
    t!(format!("server_info.{key}"), locale = locale).into()
}
pub fn i18n_value_search<'a>(cx: &'a App, key: &'a str) -> SharedString {
    let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
    t!(format!("value_search.{key}"), locale = locale).into()
}
pub fn i18n_vector_set<'a>(cx: &'a App, key: &'a str) -> SharedString {
    let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
    t!(format!("vector_set.{key}"), locale = locale).into()
}
pub fn i18n_persistence<'a>(cx: &'a App, key: &'a str) -> SharedString {
    let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
    t!(format!("persistence.{key}"), locale = locale).into()
}
pub fn i18n_keyspace_notifications<'a>(cx: &'a App, key: &'a str) -> SharedString {
    let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
    t!(format!("keyspace_notifications.{key}"), locale = locale).into()
}
pub fn i18n_key_tag<'a>(cx: &'a App, key: &'a str) -> SharedString {
    let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
    t!(format!("key_tag.{key}"), locale = locale).into()
}
pub fn i18n_topology<'a>(cx: &'a App, key: &'a str) -> SharedString {
    let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
    t!(format!("topology.{key}"), locale = locale).into()
}

pub fn i18n_proto_editor<'a>(cx: &'a App, key: &'a str) -> SharedString {
    let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
    t!(format!("proto_editor.{key}"), locale = locale).into()
}

pub fn i18n_pubsub_editor<'a>(cx: &'a App, key: &'a str) -> SharedString {
    let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
    t!(format!("pubsub_editor.{key}"), locale = locale).into()
}

pub fn i18n_script_editor<'a>(cx: &'a App, key: &'a str) -> SharedString {
    let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
    t!(format!("script_editor.{key}"), locale = locale).into()
}

pub fn i18n_slowlog_editor<'a>(cx: &'a App, key: &'a str) -> SharedString {
    let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
    t!(format!("slowlog_editor.{key}"), locale = locale).into()
}

pub fn i18n_clients_manager<'a>(cx: &'a App, key: &'a str) -> SharedString {
    let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
    t!(format!("clients_manager.{key}"), locale = locale).into()
}

pub fn i18n_monitor<'a>(cx: &'a App, key: &'a str) -> SharedString {
    let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
    t!(format!("monitor.{key}"), locale = locale).into()
}

// Tray-only: the tray module is compiled out on Linux.
#[cfg(not(target_os = "linux"))]
pub fn i18n_tray(cx: &App, key: &str) -> String {
    let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
    t!(format!("tray.{key}"), locale = locale).to_string()
}

pub fn i18n_trash<'a>(cx: &'a App, key: &'a str) -> SharedString {
    let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
    t!(format!("trash.{key}"), locale = locale).into()
}

pub fn i18n_memory_analysis<'a>(cx: &'a App, key: &'a str) -> SharedString {
    let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
    t!(format!("memory_analysis.{key}"), locale = locale).into()
}

pub fn i18n_stream_editor<'a>(cx: &'a App, key: &'a str) -> SharedString {
    let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
    t!(format!("stream_editor.{key}"), locale = locale).into()
}

pub fn i18n_config_editor<'a>(cx: &'a App, key: &'a str) -> SharedString {
    let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
    t!(format!("config_editor.{key}"), locale = locale).into()
}

pub fn i18n_migration<'a>(cx: &'a App, key: &'a str) -> SharedString {
    let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
    t!(format!("migration.{key}"), locale = locale).into()
}

pub fn i18n_acl<'a>(cx: &'a App, key: &'a str) -> SharedString {
    let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
    t!(format!("acl.{key}"), locale = locale).into()
}

pub fn i18n_search<'a>(cx: &'a App, key: &'a str) -> SharedString {
    let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
    t!(format!("search.{key}"), locale = locale).into()
}

pub fn i18n_functions<'a>(cx: &'a App, key: &'a str) -> SharedString {
    let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
    t!(format!("functions.{key}"), locale = locale).into()
}

pub fn i18n_geo_map<'a>(cx: &'a App, key: &'a str) -> SharedString {
    let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
    t!(format!("geo_map.{key}"), locale = locale).into()
}

pub fn i18n_lua_scripts<'a>(cx: &'a App, key: &'a str) -> SharedString {
    let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
    t!(format!("lua_scripts.{key}"), locale = locale).into()
}

pub fn i18n_features<'a>(cx: &'a App, key: &'a str) -> SharedString {
    let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
    t!(format!("features.{key}"), locale = locale).into()
}

/// Localized name of a server panel — every panel section carries a `title`
/// key, so the placeholder page, the capability dialog and the status-bar
/// chips can all name a panel the same way.
pub fn server_view_title(cx: &App, view: ServerView) -> SharedString {
    match view {
        ServerView::Editor => i18n_editor(cx, "title"),
        ServerView::Metrics => i18n_metrics(cx, "title"),
        ServerView::Slowlog => i18n_slowlog_editor(cx, "title"),
        ServerView::MemoryAnalysis => i18n_memory_analysis(cx, "title"),
        ServerView::Clients => i18n_clients_manager(cx, "title"),
        ServerView::Monitor => i18n_monitor(cx, "title"),
        ServerView::Config => i18n_config_editor(cx, "title"),
        ServerView::Acl => i18n_acl(cx, "title"),
        ServerView::Search => i18n_search(cx, "title"),
        ServerView::Functions => i18n_functions(cx, "title"),
        ServerView::LuaScripts => i18n_lua_scripts(cx, "title"),
        ServerView::Persistence => i18n_persistence(cx, "title"),
        ServerView::KeyspaceNotifications => i18n_keyspace_notifications(cx, "title"),
        ServerView::Topology => i18n_topology(cx, "title"),
        ServerView::ServerLoad => i18n_server_load(cx, "title"),
        ServerView::ValueSearch => i18n_value_search(cx, "title"),
        ServerView::ServerInfo => i18n_server_info(cx, "title"),
    }
}

/// Localized reason a command is unusable: "CONFIG GET — denied for this
/// user (NOPERM)".
pub fn command_status_label(cx: &App, command: ServerCommand, status: CommandStatus) -> SharedString {
    format!("{} — {}", command.label(), i18n_features(cx, status.i18n_key())).into()
}

/// "CONFIG GET is unavailable on this server: denied for this user (NOPERM)"
/// — the one-time notice when a command first fails as unsupported/denied.
pub fn command_unavailable_message(cx: &App, command: ServerCommand, status: CommandStatus) -> SharedString {
    let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
    let reason = t!(format!("features.{}", status.i18n_key()), locale = locale).to_string();
    t!(
        "features.command_unavailable",
        command = command.label(),
        reason = reason,
        locale = locale
    )
    .to_string()
    .into()
}

pub fn i18n_crash<'a>(cx: &'a App, key: &'a str) -> SharedString {
    let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
    t!(format!("crash.{key}"), locale = locale).into()
}

pub fn i18n_update<'a>(cx: &'a App, key: &'a str) -> SharedString {
    let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
    t!(format!("update.{key}"), locale = locale).into()
}
