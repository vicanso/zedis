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

use crate::constants::SIDEBAR_WIDTH;
use crate::error::Error;
use crate::helpers::{get_key_tree_widths, get_or_create_config_dir};
use gpui::App;
use gpui::AppContext;
use gpui::Bounds;
use gpui::Context;
use gpui::Entity;
use gpui::Global;
use gpui::Pixels;
use gpui_component::{PixelsExt, ThemeMode};
use locale_config::Locale;
use serde::Deserialize;
use serde::Serialize;
use std::path::PathBuf;

type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub enum Route {
    #[default]
    Home,
    Editor,
}

const LIGHT_THEME_MODE: &str = "light";
const DARK_THEME_MODE: &str = "dark";

fn get_or_create_server_config() -> Result<PathBuf> {
    let config_dir = get_or_create_config_dir()?;
    let path = config_dir.join("zedis.toml");
    if path.exists() {
        return Ok(path);
    }
    std::fs::write(&path, "")?;
    Ok(path)
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ZedisAppState {
    route: Route,
    locale: Option<String>,
    bounds: Option<Bounds<Pixels>>,
    key_tree_width: Pixels,
    theme: Option<String>,
    font_size: Option<f32>,
}

#[derive(Debug, Clone)]
pub struct ZedisGlobalStore {
    app_state: Entity<ZedisAppState>,
}

impl ZedisGlobalStore {
    pub fn new(app_state: Entity<ZedisAppState>) -> Self {
        Self { app_state }
    }
    pub fn state(&self) -> Entity<ZedisAppState> {
        self.app_state.clone()
    }
    pub fn value(&self, cx: &App) -> ZedisAppState {
        self.app_state.read(cx).clone()
    }
    pub fn locale<'a>(&self, cx: &'a App) -> &'a str {
        self.app_state.read(cx).locale.as_deref().unwrap_or("en")
    }
    pub fn font_size(&self, cx: &App) -> f32 {
        self.app_state.read(cx).font_size.unwrap_or(12.0)
    }
    pub fn theme(&self, cx: &App) -> Option<ThemeMode> {
        self.app_state.read(cx).theme()
    }
    pub fn update<R, C: AppContext>(
        &self,
        cx: &mut C,
        update: impl FnOnce(&mut ZedisAppState, &mut Context<ZedisAppState>) -> R,
    ) -> C::Result<R> {
        self.app_state.update(cx, update)
    }
    pub fn read<'a>(&self, cx: &'a App) -> &'a ZedisAppState {
        self.app_state.read(cx)
    }
}

impl Global for ZedisGlobalStore {}

pub fn save_app_state(state: &ZedisAppState) -> Result<()> {
    let path = get_or_create_server_config()?;
    let value = toml::to_string(state)?;
    std::fs::write(path, value)?;
    Ok(())
}

impl ZedisAppState {
    pub fn try_new() -> Result<Self> {
        let path = get_or_create_server_config()?;
        let value = std::fs::read_to_string(path)?;
        let mut state: Self = toml::from_str(&value)?;
        if state.locale.clone().unwrap_or_default().is_empty()
            && let Some((lang, _)) = Locale::current().to_string().split_once("-")
        {
            state.locale = Some(lang.to_string());
        }
        state.route = Route::Home;

        Ok(state)
    }
    pub fn new() -> Self {
        Self { ..Default::default() }
    }
    pub fn key_tree_width(&self) -> Pixels {
        self.key_tree_width
    }
    pub fn content_width(&self) -> Option<Pixels> {
        let bounds = self.bounds?;
        let width = bounds.size.width.as_f32();
        let (key_tree_width, _, _) = get_key_tree_widths(self.key_tree_width);
        Some((width - SIDEBAR_WIDTH - key_tree_width.as_f32()).into())
    }
    pub fn set_key_tree_width(&mut self, width: Pixels) {
        self.key_tree_width = width;
    }
    pub fn route(&self) -> Route {
        self.route
    }
    pub fn bounds(&self) -> Option<&Bounds<Pixels>> {
        self.bounds.as_ref()
    }
    pub fn go_to(&mut self, route: Route, cx: &mut Context<Self>) {
        if self.route != route {
            self.route = route;
            cx.notify();
        }
    }
    pub fn font_size(&self) -> f32 {
        self.font_size.unwrap_or(12.0)
    }
    pub fn set_font_size(&mut self, font_size: Option<f32>) {
        self.font_size = font_size;
    }
    fn theme(&self) -> Option<ThemeMode> {
        match self.theme.as_deref() {
            Some(LIGHT_THEME_MODE) => Some(ThemeMode::Light),
            Some(DARK_THEME_MODE) => Some(ThemeMode::Dark),
            _ => None,
        }
    }

    pub fn set_bounds(&mut self, bounds: Bounds<Pixels>) {
        self.bounds = Some(bounds);
    }
    pub fn set_theme(&mut self, theme: Option<ThemeMode>) {
        match theme {
            Some(ThemeMode::Light) => {
                self.theme = Some(LIGHT_THEME_MODE.to_string());
            }
            Some(ThemeMode::Dark) => {
                self.theme = Some(DARK_THEME_MODE.to_string());
            }
            _ => {
                self.theme = None;
            }
        }
    }
    pub fn set_locale(&mut self, locale: String) {
        self.locale = Some(locale);
    }
}
