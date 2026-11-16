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

// GPUI Fast (`gpui-fast`, see the root manifest): the kit under the name
// `gpui`, so these widgets are written against the engine the components
// they wrap are built on.
#[cfg(feature = "gpui-fast")]
extern crate gpui_kit as gpui;

mod about;
mod card;
mod dialog;
mod divider;
mod form;
mod help;
mod hint;
mod loading;
mod panel_header;
mod select;
mod stat_chip;
mod text_table;

pub use about::*;
pub use card::*;
pub use dialog::*;
pub use divider::*;
pub use form::*;
pub use help::*;
pub use hint::*;
pub use loading::*;
pub use panel_header::*;
pub use select::*;
pub use stat_chip::*;
pub use text_table::*;
