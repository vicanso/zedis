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

use super::ZedisGlobalStore;
use gpui::App;
use gpui::SharedString;
use rust_i18n::t;

pub fn i18n_common<'a>(cx: &'a App, key: &'a str) -> SharedString {
    let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
    t!(format!("common.{key}"), locale = locale).into()
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

pub fn i18n_tray(cx: &App, key: &str) -> String {
    let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
    t!(format!("tray.{key}"), locale = locale).to_string()
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
