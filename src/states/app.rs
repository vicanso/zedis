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

use crate::connection::{
    RedisServer, get_servers, save_servers, set_redis_connection_timeout, set_redis_response_timeout,
};
use crate::constants::SIDEBAR_WIDTH;
use crate::error::Error;
use crate::helpers::{get_key_tree_widths, get_or_create_config_dir};
use crate::states::i18n_common;
use chrono::Local;
use gpui::{Action, App, AppContext, Bounds, Context, Entity, EventEmitter, Global, Pixels, SharedString};
use gpui_component::{ThemeMode, dialog::DialogButtonProps};
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;
use sys_locale::get_locale;
use tracing::{error, info};
use uuid::Uuid;

type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub enum Route {
    #[default]
    Home,
    Editor,
    Settings,
    Protos,
    Scripts,
    Metrics,
    Slowlog,
    MemoryAnalysis,
    Clients,
    Monitor,
    Config,
    Acl,
    Search,
    Functions,
    LuaScripts,
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
    /// Japanese language
    Ja,
    /// Russian language
    Ru,
    /// Portuguese language
    Pt,
    /// German language
    De,
    /// French language
    Fr,
    /// Spanish language
    Es,
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
    Protos,
    Scripts,
}

/// Server-scoped tools that open a sub-route. Triggered from the status bar
/// "Tools" dropdown so the bar itself does not need a button per route.
#[derive(Clone, Copy, PartialEq, Debug, Deserialize, JsonSchema, Action)]
pub enum ServerToolsAction {
    Monitor,
    Config,
    Acl,
    Search,
    Functions,
    LuaScripts,
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

/// Notification category for user feedback
#[derive(Clone, PartialEq, Debug, Deserialize, JsonSchema, Default)]
pub enum NotificationCategory {
    #[default]
    Info,
    Success,
    Warning,
    Error,
}

/// Notification action that can be triggered in the UI
#[derive(Clone, PartialEq, Debug, Deserialize, JsonSchema, Action, Default)]
pub struct NotificationAction {
    pub title: Option<SharedString>,
    pub category: NotificationCategory,
    pub message: SharedString,
}

impl NotificationAction {
    /// Creates a new info notification
    pub fn new_info(message: SharedString) -> Self {
        Self {
            category: NotificationCategory::Info,
            message,
            ..Default::default()
        }
    }

    /// Creates a new success notification
    pub fn new_success(message: SharedString) -> Self {
        Self {
            category: NotificationCategory::Success,
            message,
            ..Default::default()
        }
    }

    /// Creates a new warning notification
    pub fn new_warning(message: SharedString) -> Self {
        Self {
            category: NotificationCategory::Warning,
            message,
            ..Default::default()
        }
    }

    /// Creates a new error notification
    pub fn new_error(message: SharedString) -> Self {
        Self {
            category: NotificationCategory::Error,
            message,
            ..Default::default()
        }
    }

    /// Sets the title for the notification
    pub fn with_title(mut self, title: SharedString) -> Self {
        self.title = Some(title);
        self
    }
}

pub enum GlobalEvent {
    /// A notification has been emitted.
    Notification(NotificationAction),
    /// User selected a different server
    ServerSelected(SharedString, usize),
    /// Server list config has been modified (add/remove/edit).
    ServerListUpdated,
    /// Route has been changed.
    RouteChanged(Route),
}

/// Direction passed to [`ZedisGlobalStore::reorder_server`].
#[derive(Debug, Clone, Copy)]
pub enum ReorderDirection {
    Up,
    Down,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ZedisAppState {
    route: Route,
    query: Option<HashMap<String, String>>,
    locale: Option<String>,
    bounds: Option<Bounds<Pixels>>,
    key_tree_width: Pixels,
    theme: Option<String>,
    font_size: Option<FontSize>,
    max_key_tree_depth: Option<usize>,
    key_separator: Option<String>,
    auto_expand_threshold: Option<usize>,
    key_scan_count: Option<usize>,
    max_truncate_length: Option<usize>,
    redis_connection_timeout: Option<Duration>,
    redis_response_timeout: Option<Duration>,
    selected_server: Option<(String, usize)>,
    tray_enabled: Option<bool>,
    /// When `true`, the key tree fetches TTL per key during SCAN and shows
    /// a TTL chip next to each leaf. When `false` the TTL pipeline command
    /// is skipped (cheaper scans on large dbs) and no chip is rendered.
    /// Defaults to `true`.
    show_key_tree_ttl: Option<bool>,
}

impl EventEmitter<GlobalEvent> for ZedisAppState {}

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
    ) -> R {
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
        let mut state = if value.is_empty() {
            Self::default()
        } else {
            toml::from_str(&value)?
        };
        if state.locale.clone().unwrap_or_default().is_empty() {
            if let Some(locale) = get_locale() {
                // Try to extract the language code from the locale string
                // Handle formats like: "en-US", "zh-CN", "en", "zh", etc.
                let lang = if let Some((lang, _)) = locale.split_once('-') {
                    lang
                } else if let Some((lang, _)) = locale.split_once('_') {
                    // Some systems use underscore: "en_US"
                    lang
                } else {
                    // Already a simple language code like "en" or "zh"
                    locale.as_str()
                };
                state.locale = Some(lang.to_lowercase());
            } else {
                // Fallback to English if locale detection fails
                state.locale = Some("en".to_string());
            }
        }
        state.route = Route::Home;

        if let Some(redis_connection_timeout) = state.redis_connection_timeout {
            set_redis_connection_timeout(redis_connection_timeout);
        }
        if let Some(redis_response_timeout) = state.redis_response_timeout {
            set_redis_response_timeout(redis_response_timeout);
        }

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
        Some((width - SIDEBAR_WIDTH.as_f32() - key_tree_width.as_f32()).into())
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
    fn go_to_with_query(&mut self, route: Route, query: Option<HashMap<String, String>>, cx: &mut Context<Self>) {
        if self.route == route && self.query == query {
            return;
        }
        self.query = query;
        self.route = route;
        cx.emit(GlobalEvent::RouteChanged(route));
        cx.notify();
    }
    pub fn go_to(&mut self, route: Route, cx: &mut Context<Self>) {
        self.go_to_with_query(route, None, cx);
    }
    pub fn go_with_query(&mut self, route: Route, query: HashMap<String, String>, cx: &mut Context<Self>) {
        self.go_to_with_query(route, Some(query), cx);
    }
    pub fn get_route_query(&self) -> Option<&HashMap<String, String>> {
        self.query.as_ref()
    }
    pub fn toggle_route(&mut self, routes: (Route, Route), cx: &mut Context<Self>) {
        let route = if self.route == routes.0 { routes.1 } else { routes.0 };
        self.route = route;
        cx.emit(GlobalEvent::RouteChanged(route));
        cx.notify();
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
    pub fn set_redis_connection_timeout(&mut self, redis_connection_timeout: Option<Duration>) {
        if let Some(redis_connection_timeout) = redis_connection_timeout {
            set_redis_connection_timeout(redis_connection_timeout);
        }
        self.redis_connection_timeout = redis_connection_timeout;
    }
    pub fn set_redis_response_timeout(&mut self, redis_response_timeout: Option<Duration>) {
        if let Some(redis_response_timeout) = redis_response_timeout {
            set_redis_response_timeout(redis_response_timeout);
        }
        self.redis_response_timeout = redis_response_timeout;
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
    pub fn key_separator(&self) -> &str {
        self.key_separator.as_deref().unwrap_or(":")
    }
    pub fn set_key_separator(&mut self, key_separator: String) {
        if key_separator.is_empty() {
            self.key_separator = None;
            return;
        }
        self.key_separator = Some(key_separator);
    }
    pub fn max_truncate_length(&self) -> usize {
        self.max_truncate_length.unwrap_or(1000)
    }
    pub fn set_max_truncate_length(&mut self, max_truncate_length: usize) {
        self.max_truncate_length = Some(max_truncate_length);
    }
    pub fn redis_connection_timeout(&self) -> String {
        self.redis_connection_timeout
            .map(|timeout| timeout.as_secs().to_string())
            .unwrap_or_default()
    }
    pub fn redis_response_timeout(&self) -> String {
        self.redis_response_timeout
            .map(|timeout| timeout.as_secs().to_string())
            .unwrap_or_default()
    }
    pub fn key_scan_count(&self) -> usize {
        self.key_scan_count.unwrap_or(1_000)
    }
    pub fn set_key_scan_count(&mut self, key_scan_count: usize) {
        self.key_scan_count = Some(key_scan_count);
    }
    pub fn auto_expand_threshold(&self) -> usize {
        self.auto_expand_threshold.unwrap_or(100)
    }
    pub fn set_auto_expand_threshold(&mut self, auto_expand_threshold: usize) {
        self.auto_expand_threshold = Some(auto_expand_threshold);
    }
    pub fn tray_enabled(&self) -> bool {
        self.tray_enabled.unwrap_or(true)
    }
    pub fn set_tray_enabled(&mut self, enabled: bool) {
        self.tray_enabled = Some(enabled);
    }
    pub fn show_key_tree_ttl(&self) -> bool {
        self.show_key_tree_ttl.unwrap_or(true)
    }
    pub fn set_show_key_tree_ttl(&mut self, enabled: bool) {
        self.show_key_tree_ttl = Some(enabled);
    }
    pub fn selected_server(&self) -> Option<&(String, usize)> {
        self.selected_server.as_ref()
    }
    pub fn clear_selected_server(&mut self, cx: &mut Context<Self>) {
        self.selected_server = None;
        cx.emit(GlobalEvent::ServerSelected(SharedString::default(), 0));
    }
    pub fn set_selected_server(&mut self, selected_server: (String, usize), cx: &mut Context<Self>) {
        let (server_id, db) = selected_server.clone();
        cx.emit(GlobalEvent::ServerSelected(server_id.into(), db));
        self.selected_server = Some(selected_server);
    }
    pub fn remove_server(&mut self, id: &str, cx: &mut Context<Self>) {
        let id = id.to_string();
        cx.spawn(async move |handle, cx| {
            let task = cx.background_spawn(async move {
                let mut servers = get_servers()?;
                servers.retain(|s| s.id != id);
                save_servers(servers.clone()).await?;
                Ok(())
            });
            let result: Result<()> = task.await;
            if let Err(e) = &result {
                error!(error = %e, "Failed to remove server");
            }
            handle.update(cx, |_this, cx| {
                cx.emit(GlobalEvent::ServerListUpdated);
                cx.notify();
            })
        })
        .detach();
    }
    /// Swap the `sort_order` of `server_id` with the adjacent neighbor
    /// in the same `group`, in the requested direction. No-op at the
    /// edge of the group. Persists and broadcasts `ServerListUpdated`
    /// so the grid re-renders in the new order.
    ///
    /// Implementation note: instead of swapping just two sort_order
    /// values, we (a) collect the in-group entries in their current
    /// sorted-on-display order, (b) renumber the entire group as
    /// `0..n` so legacy entries with `sort_order: None` get real
    /// values, then (c) perform the index swap. This guarantees the
    /// operation is observable even on data saved before this field
    /// existed (everyone tied at `None` would otherwise no-op).
    pub fn reorder_server(&mut self, server_id: &str, direction: ReorderDirection, cx: &mut Context<Self>) {
        let server_id = server_id.to_string();
        cx.spawn(async move |handle, cx| {
            let task = cx.background_spawn(async move {
                let mut servers = get_servers()?;
                let Some(target_idx) = servers.iter().position(|s| s.id == server_id) else {
                    return Ok::<(), Error>(());
                };
                let group_key = servers[target_idx]
                    .group
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(String::from);
                let in_same_group = |s: &RedisServer| {
                    s.group
                        .as_deref()
                        .map(str::trim)
                        .filter(|g| !g.is_empty())
                        .map(String::from)
                        == group_key
                };

                // (a) Collect indices belonging to the target's group,
                // in current display order. `get_servers()` already
                // returns canonical sort order, so this list is
                // monotonic.
                let group_indices: Vec<usize> = servers
                    .iter()
                    .enumerate()
                    .filter(|(_, s)| in_same_group(s))
                    .map(|(i, _)| i)
                    .collect();
                let Some(pos_in_group) = group_indices.iter().position(|&i| i == target_idx) else {
                    return Ok(());
                };

                // (c) Determine the swap partner's position in-group.
                let swap_pos = match direction {
                    ReorderDirection::Up if pos_in_group > 0 => pos_in_group - 1,
                    ReorderDirection::Down if pos_in_group + 1 < group_indices.len() => pos_in_group + 1,
                    _ => return Ok(()), // at edge — nothing to do
                };

                // (b) Renumber 0..n in current order, then write the
                // swapped positions back. This means even legacy data
                // (`sort_order: None`) ends up with a stable, distinct
                // sort_order after the first reorder click.
                let mut new_order: Vec<i64> = (0..group_indices.len() as i64).collect();
                new_order.swap(pos_in_group, swap_pos);
                for (slot, &server_idx) in group_indices.iter().enumerate() {
                    servers[server_idx].sort_order = Some(new_order[slot]);
                }

                save_servers(servers).await?;
                Ok(())
            });
            let _: Result<()> = task.await;
            handle.update(cx, |_this, cx| {
                cx.emit(GlobalEvent::ServerListUpdated);
                cx.notify();
            })
        })
        .detach();
    }

    pub fn upsert_server(&mut self, mut server: RedisServer, cx: &mut Context<Self>) {
        if server.id.is_empty() {
            server.id = Uuid::now_v7().to_string();
        }
        server.updated_at = Some(Local::now().to_string());
        cx.spawn(async move |handle, cx| {
            let task = cx.background_spawn(async move {
                if server.name.is_empty() {
                    return Err(Error::Invalid {
                        message: "Server name is required".to_string(),
                    });
                }
                let mut servers = get_servers()?;
                if let Some(existing_server) = servers.iter_mut().find(|s| s.id == server.id) {
                    // Preserve the existing sort_order on update unless
                    // the caller explicitly supplied one (reorder
                    // buttons set it; the edit form leaves it None).
                    if server.sort_order.is_none() {
                        server.sort_order = existing_server.sort_order;
                    }
                    *existing_server = server;
                } else {
                    // New server: append to the tail of its group by
                    // assigning max(sort_order)+1 within that group.
                    if server.sort_order.is_none() {
                        let new_group = server.group.as_deref().map(str::trim).filter(|s| !s.is_empty());
                        let next = servers
                            .iter()
                            .filter(|s| s.group.as_deref().map(str::trim).filter(|g| !g.is_empty()) == new_group)
                            .filter_map(|s| s.sort_order)
                            .max()
                            .map(|m| m + 1)
                            .unwrap_or(0);
                        server.sort_order = Some(next);
                    }
                    servers.push(server);
                }
                save_servers(servers.clone()).await?;
                Ok(())
            });
            let result: Result<()> = task.await;

            handle.update(cx, |_this, cx| {
                if let Err(e) = &result {
                    error!(error = %e, "Failed to upsert server");
                    cx.emit(GlobalEvent::Notification(NotificationAction::new_error(
                        e.to_string().into(),
                    )));
                    return;
                }
                cx.emit(GlobalEvent::ServerListUpdated);
                cx.notify();
            })
        })
        .detach();
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
        let state = store.update(cx, |state, cx| {
            mutation(state, cx);
            state.clone() // Return clone for async persistence
        });

        // Step 2: Persist to disk in background executor
        cx.background_executor()
            .spawn(async move {
                if let Err(e) = save_app_state(&state) {
                    error!(error = %e, action = action_name, "Failed to save state");
                } else {
                    info!(action = action_name, "State saved successfully");
                }
            })
            .await;

        // Step 3: Refresh windows to apply visual changes (theme/locale)
        cx.update(|cx| cx.refresh_windows());
    })
    .detach();
}

pub fn dialog_button_props(cx: &App) -> DialogButtonProps {
    DialogButtonProps::default()
        .cancel_text(i18n_common(cx, "cancel"))
        .ok_text(i18n_common(cx, "delete"))
}
