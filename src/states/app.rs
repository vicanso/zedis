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

use crate::constants::SIDEBAR_WIDTH;
use crate::error::Error;
use crate::helpers::{get_key_tree_widths, get_or_create_config_dir};
use gpui::{Action, App, AppContext, Bounds, Context, Entity, Global, Pixels};
use gpui_component::{PixelsExt, ThemeMode};
use locale_config::Locale;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use std::path::PathBuf;
use tracing::{error, info};

type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub enum Route {
    #[default]
    Home,
    Editor,
    Settings,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub enum FontSize {
    Small,
    #[default]
    Medium,
    Large,
}
impl FontSize {
    pub fn to_pixels(self) -> Option<f32> {
        match self {
            FontSize::Small => Some(14.0),
            FontSize::Medium => None,
            FontSize::Large => Some(18.0),
        }
    }
}

/// Theme selection actions for the settings menu
#[derive(Clone, Copy, PartialEq, Debug, Deserialize, JsonSchema, Action)]
pub enum ThemeAction {
    /// Light theme mode
    Light,
    /// Dark theme mode
    Dark,
    /// Follow system theme
    System,
}

/// Locale/language selection actions for the settings menu
#[derive(Clone, Copy, PartialEq, Debug, Deserialize, JsonSchema, Action)]
pub enum LocaleAction {
    /// English language
    En,
    /// Chinese language
    Zh,
}

#[derive(Clone, Copy, PartialEq, Debug, Deserialize, JsonSchema, Action)]
pub enum FontSizeAction {
    Large,
    Medium,
    Small,
}
#[derive(Clone, Copy, PartialEq, Debug, Deserialize, JsonSchema, Action)]
pub enum SettingsAction {
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
    font_size: Option<FontSize>,
    max_key_tree_depth: Option<usize>,
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
    pub fn font_size(&self) -> FontSize {
        self.font_size.unwrap_or(FontSize::Medium)
    }
    pub fn max_key_tree_depth(&self) -> usize {
        self.max_key_tree_depth.unwrap_or(5)
    }
    pub fn set_max_key_tree_depth(&mut self, max_key_tree_depth: usize) {
        if max_key_tree_depth == 0 {
            self.max_key_tree_depth = None;
            return;
        }
        self.max_key_tree_depth = Some(max_key_tree_depth);
    }
    pub fn set_font_size(&mut self, font_size: Option<FontSize>) {
        self.font_size = font_size;
    }
    pub fn theme(&self) -> Option<ThemeMode> {
        match self.theme.as_deref() {
            Some(LIGHT_THEME_MODE) => Some(ThemeMode::Light),
            Some(DARK_THEME_MODE) => Some(ThemeMode::Dark),
            _ => None,
        }
    }
    pub fn locale(&self) -> &str {
        self.locale.as_deref().unwrap_or("en")
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

/// Update app state in background, persist to disk, and refresh UI
///
/// This helper function abstracts the common pattern for updating global state:
/// 1. Apply mutation to app state
/// 2. Save updated state to disk asynchronously
/// 3. Refresh all windows to apply changes
///
/// Used for theme and locale changes to ensure consistency across the app.
///
/// # Arguments
/// * `cx` - Context for spawning async tasks
/// * `action_name` - Human-readable action name for logging
/// * `mutation` - Callback to modify the app state
#[inline]
pub fn update_app_state_and_save<F>(cx: &App, action_name: &'static str, mutation: F)
where
    F: FnOnce(&mut ZedisAppState, &App) + Send + 'static + Clone,
{
    let store = cx.global::<ZedisGlobalStore>().clone();

    cx.spawn(async move |cx| {
        // Step 1: Update global state with the mutation
        let current_state = store.update(cx, |state, cx| {
            mutation(state, cx);
            state.clone() // Return clone for async persistence
        });

        // Step 2: Persist to disk in background executor
        if let Ok(state) = current_state {
            cx.background_executor()
                .spawn(async move {
                    if let Err(e) = save_app_state(&state) {
                        error!(error = %e, action = action_name, "Failed to save state");
                    } else {
                        info!(action = action_name, "State saved successfully");
                    }
                })
                .await;
        }

        // Step 3: Refresh windows to apply visual changes (theme/locale)
        cx.update(|cx| cx.refresh_windows()).ok();
    })
    .detach();
}
