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

mod action;
mod ai;
mod app_identity;
mod color;
mod common;
mod crash;
mod datetime;
mod diagnostics;
mod font;
mod keybindings;
mod local_data;
mod logger;
mod proxy;
mod single_instance;
mod syntax;
mod tag;
mod updater;
mod zip;

pub use action::*;
pub use ai::{AiEndpoint, analyze_report, suggest_command};
pub use app_identity::with_app_identity;
pub use color::card_background;
pub use common::*;
pub use crash::{CrashContext, CrashReport, install_panic_hook, take_pending_crash};
pub use datetime::*;
pub use diagnostics::{DiagnosticsInput, export_diagnostics};
pub use font::*;
pub use keybindings::{ensure_keybindings_file, keybinding_overrides, load_keybinding_overrides};
pub use local_data::{export_local_data_file, import_local_data_file};
pub use logger::{init_logger, logs_dir};
pub use proxy::{is_valid_proxy_setting, set_configured_proxy};
pub use single_instance::{
    InstanceMessage, InstanceRole, claim_instance, instance_messages, post_instance_message, release_instance,
    take_instance_server,
};
pub use syntax::register_extra_languages;
pub use tag::{resolve_tag_chip, resolve_tag_color, theme_color_for_tag};
#[cfg(target_os = "macos")]
pub use updater::relaunch;
pub use updater::{
    Delivery, UpdateInfo, download_and_verify, fetch_latest_release, focus_installer_ui, install_update,
    installer_requires_quit,
};
// Pure logic lives in `zedis-core`, fs/crypto/time in `zedis-connection`;
// re-exported here so call sites keep using `crate::helpers::*` unchanged.
pub use zedis_connection::string::*;
pub use zedis_connection::time::{parse_duration, unix_ts, unix_ts_millis};
pub use zedis_core::csv::build_csv;
pub use zedis_core::diff::*;
pub use zedis_core::env::is_development;
pub use zedis_core::fs::*;
pub use zedis_core::fuzzy::{fuzzy_score, fuzzy_score_prepared, prepare_fuzzy_query};
pub use zedis_core::hex::{bytes_to_hex_text, parse_hex_text};
pub use zedis_core::jsonpath::{
    JsonPathOutcome, is_json_container, jsonpath_completion_prefix, jsonpath_key_suggestions, run_jsonpath,
};
pub use zedis_core::ttl::{TtlFilter, format_ttl_chip, ttl_chip_kind};
pub use zedis_core::validate::*;
