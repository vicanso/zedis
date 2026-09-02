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

//! GUI-free core logic shared by the Zedis app: pure functions with no gpui
//! dependency, so they compile and test without the UI stack. UI strings stay
//! in the app crate (rust-i18n is per-crate) — modules here return data only.

pub mod capability;
pub mod csv;
pub mod diff;
pub mod env;
pub mod features;
pub mod fs;
pub mod fuzzy;
pub mod hex;
pub mod jsonpath;
pub mod keysizes;
pub mod rdb;
pub mod search_params;
pub mod string;
pub mod ttl;
pub mod ttl_cache;
pub mod validate;
