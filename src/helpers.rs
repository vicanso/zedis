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
mod common;
mod font;
mod fs;
mod fuzzy;
mod hex;
mod jsonpath;
mod string;
mod syntax;
mod tag;
mod time;
mod ttl;
mod ttl_cache;
mod validate;

pub use action::*;
pub use common::*;
pub use font::*;
pub use fs::*;
pub use fuzzy::fuzzy_score;
pub use hex::{bytes_to_hex_text, parse_hex_text};
pub use jsonpath::{
    JsonPathOutcome, is_json_container, jsonpath_completion_prefix, jsonpath_key_suggestions, run_jsonpath,
};
pub use string::*;
pub use syntax::register_extra_languages;
pub use tag::resolve_tag_color;
pub use time::{parse_duration, unix_ts, unix_ts_millis};
pub use ttl::{TtlChipKind, format_ttl_chip, ttl_chip_kind};
pub use ttl_cache::*;
pub use validate::*;
pub fn is_development() -> bool {
    env::var("RUST_ENV").unwrap_or_default() == "dev"
}
