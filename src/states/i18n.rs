// Copyright 2025 Tree xie.
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

pub fn i18n_sidebar<'a>(cx: &'a App, key: &'a str) -> SharedString {
    let locale = cx.global::<ZedisGlobalStore>().locale(cx);
    t!(format!("sidebar.{key}"), locale = locale).into()
}

pub fn i18n_servers<'a>(cx: &'a App, key: &'a str) -> SharedString {
    let locale = cx.global::<ZedisGlobalStore>().locale(cx);
    t!(format!("servers.{key}"), locale = locale).into()
}

pub fn i18n_editor<'a>(cx: &'a App, key: &'a str) -> SharedString {
    let locale = cx.global::<ZedisGlobalStore>().locale(cx);
    t!(format!("editor.{key}"), locale = locale).into()
}

pub fn i18n_key_tree<'a>(cx: &'a App, key: &'a str) -> SharedString {
    let locale = cx.global::<ZedisGlobalStore>().locale(cx);
    t!(format!("key_tree.{key}"), locale = locale).into()
}

pub fn i18n_status_bar<'a>(cx: &'a App, key: &'a str) -> SharedString {
    let locale = cx.global::<ZedisGlobalStore>().locale(cx);
    t!(format!("status_bar.{key}"), locale = locale).into()
}

pub fn i18n_list_editor<'a>(cx: &'a App, key: &'a str) -> SharedString {
    let locale = cx.global::<ZedisGlobalStore>().locale(cx);
    t!(format!("list_editor.{key}"), locale = locale).into()
}
