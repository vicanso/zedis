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

use std::env;

mod action;
mod ai;
mod app_identity;
mod color;
mod common;
mod font;
mod fs;
mod logger;
mod string;
mod syntax;
mod tag;
mod time;
mod updater;

pub use action::*;
pub use ai::{AiEndpoint, analyze_report};
pub use app_identity::with_app_identity;
pub use color::card_background;
pub use common::*;
pub use font::*;
pub use fs::*;
pub use logger::{init_logger, logs_dir};
pub use string::*;
pub use syntax::register_extra_languages;
pub use tag::{resolve_tag_chip, resolve_tag_color, theme_color_for_tag};
pub use time::{parse_duration, unix_ts, unix_ts_millis};
pub use updater::{UpdateInfo, download_and_verify, fetch_latest_release, open_installer};
// Pure (GUI-free) logic lives in the `zedis-core` crate; re-exported here so
// call sites keep using `crate::helpers::*` unchanged.
pub use zedis_core::csv::build_csv;
pub use zedis_core::diff::*;
pub use zedis_core::fuzzy::fuzzy_score;
pub use zedis_core::hex::{bytes_to_hex_text, parse_hex_text};
pub use zedis_core::jsonpath::{
    JsonPathOutcome, is_json_container, jsonpath_completion_prefix, jsonpath_key_suggestions, run_jsonpath,
};
pub use zedis_core::ttl::{TtlFilter, format_ttl_chip, ttl_chip_kind};
pub use zedis_core::ttl_cache::*;
pub use zedis_core::validate::*;
pub fn is_development() -> bool {
    env::var("RUST_ENV").unwrap_or_default() == "dev"
}
