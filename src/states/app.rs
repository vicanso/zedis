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

use crate::error::Error;
use crate::helpers::get_or_create_config_dir;
use gpui::Bounds;
use gpui::Pixels;
use gpui::prelude::*;
use gpui_component::ThemeMode;
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
    bounds: Option<Bounds<Pixels>>,
    key_tree_width: Pixels,
    theme: Option<String>,
}

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
        // TODO 暂时不支持指定route，后续修改
        state.route = Route::Home;

        Ok(state)
    }
    pub fn new() -> Self {
        Self {
            ..Default::default()
        }
    }

    pub fn key_tree_width(&self) -> Pixels {
        self.key_tree_width
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
    pub fn theme(&self) -> Option<ThemeMode> {
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
}
