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
mod string;
mod time;
mod ttl_cache;
mod validate;

pub use action::*;
pub use common::*;
pub use font::get_font_family;
pub use fs::*;
pub use string::*;
pub use time::{parse_duration, unix_ts, unix_ts_millis};
pub use ttl_cache::*;
pub use validate::*;
pub fn is_development() -> bool {
    env::var("RUST_ENV").unwrap_or_default() == "dev"
}

pub fn is_windows() -> bool {
    cfg!(target_os = "windows")
}
