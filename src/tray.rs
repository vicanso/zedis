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

use crate::connection::{RedisServer, get_servers};
use crate::states::Route::{Editor, Home, Settings};
use crate::states::{RedisMetrics, ZedisAppState, ZedisGlobalStore, get_metrics_cache};
use gpui::{App, BorrowAppContext, Context};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;
use tracing::error;
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu};
use tray_icon::{Icon, TrayIconBuilder};

const MENU_ID_QUIT: &str = "quit";
const MENU_ID_SHOW: &str = "show";
const MENU_ID_NEW_CONNECTION: &str = "new_connection";
const MENU_ID_PREFERENCES: &str = "preferences";
const MENU_ID_SERVER_PREFIX: &str = "server:";

fn load_icon() -> Icon {
    let icon_bytes = include_bytes!("../assets/icon.png");
    let img = image::load_from_memory(icon_bytes).expect("Failed to load tray icon");
    let rgba = img.to_rgba8();
    let (width, height) = rgba.dimensions();
    Icon::from_rgba(rgba.into_raw(), width, height).expect("Failed to create tray icon")
}

/// Holds references to dynamic menu items so we can update text without rebuilding.
struct TrayMenuState {
    active_label: MenuItem,
    mem_label: MenuItem,
    ops_label: MenuItem,
    server_items: Vec<(String, MenuItem)>,
    server_ids_snapshot: Vec<String>,
}

impl TrayMenuState {
    fn build(servers: &[RedisServer]) -> (Menu, Self) {
        let menu = Menu::new();

        // Header
        let _ = menu.append(&MenuItem::with_id(MENU_ID_SHOW, "Zedis", true, None));
        let _ = menu.append(&PredefinedMenuItem::separator());

        // Active server info
        let active_label = MenuItem::new("Active: --", false, None);
        let mem_label = MenuItem::new("  Mem: --", false, None);
        let ops_label = MenuItem::new("  OPS: --", false, None);
        let _ = menu.append(&active_label);
        let _ = menu.append(&mem_label);
        let _ = menu.append(&ops_label);
        let _ = menu.append(&PredefinedMenuItem::separator());

        // Quick Connect submenu
        let quick_connect = Submenu::new("Quick Connect", true);
        let mut server_items = Vec::with_capacity(servers.len());
        let mut server_ids_snapshot = Vec::with_capacity(servers.len());
        for server in servers {
            let label = format!("○ {}", server.name);
            let id = format!("{MENU_ID_SERVER_PREFIX}{}", server.id);
            let item = MenuItem::with_id(id, label, true, None);
            let _ = quick_connect.append(&item);
            server_items.push((server.id.clone(), item));
            server_ids_snapshot.push(server.id.clone());
        }
        let _ = menu.append(&quick_connect);

        // New Connection
        let _ = menu.append(&MenuItem::with_id(
            MENU_ID_NEW_CONNECTION,
            "New Connection...",
            true,
            None,
        ));
        let _ = menu.append(&PredefinedMenuItem::separator());

        // Preferences
        let _ = menu.append(&MenuItem::with_id(MENU_ID_PREFERENCES, "Preferences...", true, None));
        let _ = menu.append(&PredefinedMenuItem::separator());

        // Quit
        let _ = menu.append(&MenuItem::with_id(MENU_ID_QUIT, "Quit Zedis", true, None));

        let state = Self {
            active_label,
            mem_label,
            ops_label,
            server_items,
            server_ids_snapshot,
        };
        (menu, state)
    }

    /// Update menu item texts in-place. Returns true if server list changed and a full rebuild is needed.
    fn refresh(
        &self,
        servers: &[RedisServer],
        active_server_id: Option<&str>,
        active_metrics: Option<&RedisMetrics>,
    ) -> bool {
        // Check if server list changed
        let current_ids: Vec<&str> = servers.iter().map(|s| s.id.as_str()).collect();
        let snapshot_ids: Vec<&str> = self.server_ids_snapshot.iter().map(|s| s.as_str()).collect();
        if current_ids != snapshot_ids {
            return true;
        }

        let has_active = active_metrics.is_some();

        // Update active server section
        if let (Some(server_id), Some(m)) = (active_server_id, active_metrics) {
            let server_name = servers
                .iter()
                .find(|s| s.id == server_id)
                .map(|s| s.name.as_str())
                .unwrap_or(server_id);
            self.active_label.set_text(format!("Active: {server_name}"));
            self.mem_label.set_text(format!(
                "  Mem: {}",
                humansize::format_size(m.used_memory, humansize::DECIMAL)
            ));
            let ops_text = if m.latency_ms > 100 {
                format!("  OPS: {} (High Latency)", m.instantaneous_ops_per_sec)
            } else {
                format!("  OPS: {}", m.instantaneous_ops_per_sec)
            };
            self.ops_label.set_text(ops_text);
        } else {
            self.active_label.set_text("Active: --");
            self.mem_label.set_text("  Mem: --");
            self.ops_label.set_text("  OPS: --");
        }

        // Update Quick Connect indicators
        for (sid, item) in &self.server_items {
            let is_active = has_active && active_server_id == Some(sid.as_str());
            let indicator = if is_active { "● " } else { "○ " };
            let name = servers
                .iter()
                .find(|s| &s.id == sid)
                .map(|s| s.name.as_str())
                .unwrap_or(sid.as_str());
            item.set_text(format!("{indicator}{name}"));
        }

        false
    }
}

fn collect_refresh_data(cx: &App) -> (Vec<RedisServer>, Option<String>, Option<RedisMetrics>) {
    let store = cx.global::<ZedisGlobalStore>().clone();
    let active = store.read(cx).selected_server().map(|(id, _)| id.clone());
    let active_metrics = active.as_ref().and_then(|id| {
        let metrics = get_metrics_cache().list_metrics(id);
        metrics.last().copied()
    });
    let servers = get_servers().unwrap_or_default();
    (servers, active, active_metrics)
}

/// Tray menu action sent from the blocking listener thread to the async handler.
enum TrayAction {
    Quit,
    Show,
    Preferences,
    NewConnection,
    SelectServer(String),
}

fn refresh_tray_menu(state: &Rc<RefCell<TrayMenuState>>, tray: &Rc<tray_icon::TrayIcon>, cx: &App) {
    let (servers, active, active_metrics) = collect_refresh_data(cx);
    let need_rebuild = state
        .borrow()
        .refresh(&servers, active.as_deref(), active_metrics.as_ref());
    if need_rebuild {
        let (new_menu, new_state) = TrayMenuState::build(&servers);
        new_state.refresh(&servers, active.as_deref(), active_metrics.as_ref());
        *state.borrow_mut() = new_state;
        tray.set_menu(Some(Box::new(new_menu)));
    }
}

pub fn init_tray(cx: &mut App) {
    let icon = load_icon();
    let servers = get_servers().unwrap_or_default();
    let (menu, menu_state) = TrayMenuState::build(&servers);

    let tray = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip("Zedis")
        .with_icon(icon)
        .build();

    match tray {
        Ok(tray) => {
            let tray = Rc::new(tray);
            let state = Rc::new(RefCell::new(menu_state));

            // Channel for sending actions from blocking thread to async handler
            let (action_tx, action_rx) = smol::channel::unbounded::<TrayAction>();

            // Task 1: Blocking event listener on a dedicated thread (zero CPU when idle)
            std::thread::spawn(move || {
                let receiver = MenuEvent::receiver();
                loop {
                    let Ok(event) = receiver.recv() else {
                        return;
                    };
                    let id_str = event.id().0.clone();
                    let action = match id_str.as_str() {
                        MENU_ID_QUIT => TrayAction::Quit,
                        MENU_ID_SHOW => TrayAction::Show,
                        MENU_ID_PREFERENCES => TrayAction::Preferences,
                        MENU_ID_NEW_CONNECTION => TrayAction::NewConnection,
                        id if id.starts_with(MENU_ID_SERVER_PREFIX) => {
                            let server_id = id.strip_prefix(MENU_ID_SERVER_PREFIX).unwrap_or("").to_string();
                            TrayAction::SelectServer(server_id)
                        }
                        _ => continue,
                    };
                    if action_tx.send_blocking(action).is_err() {
                        return;
                    }
                }
            });

            // Task 2: Async handler for tray actions (processes events from the channel)
            {
                let tray = Rc::clone(&tray);
                let state = Rc::clone(&state);
                cx.spawn(async move |cx| {
                    loop {
                        let Ok(action) = action_rx.recv().await else {
                            return;
                        };
                        cx.update(|cx| {
                            match action {
                                TrayAction::Quit => {
                                    cx.quit();
                                    return;
                                }
                                TrayAction::Show => {
                                    cx.activate(true);
                                }
                                TrayAction::Preferences => {
                                    cx.activate(true);
                                    cx.update_global::<ZedisGlobalStore, ()>(
                                        |store: &mut ZedisGlobalStore, cx: &mut App| {
                                            store.update(
                                                cx,
                                                |state: &mut ZedisAppState, cx: &mut Context<ZedisAppState>| {
                                                    state.go_to(Settings, cx);
                                                },
                                            );
                                        },
                                    );
                                }
                                TrayAction::NewConnection => {
                                    cx.activate(true);
                                    cx.update_global::<ZedisGlobalStore, ()>(
                                        |store: &mut ZedisGlobalStore, cx: &mut App| {
                                            store.update(
                                                cx,
                                                |state: &mut ZedisAppState, cx: &mut Context<ZedisAppState>| {
                                                    let mut query = HashMap::new();
                                                    query.insert("new".to_string(), "true".to_string());
                                                    state.go_with_query(Home, query, cx);
                                                    state.clear_selected_server(cx);
                                                },
                                            );
                                        },
                                    );
                                }
                                TrayAction::SelectServer(server_id) => {
                                    cx.activate(true);
                                    cx.update_global::<ZedisGlobalStore, ()>(
                                        |store: &mut ZedisGlobalStore, cx: &mut App| {
                                            store.update(
                                                cx,
                                                |state: &mut ZedisAppState, cx: &mut Context<ZedisAppState>| {
                                                    state.go_to(Editor, cx);
                                                    state.set_selected_server((server_id.clone(), 0), cx);
                                                },
                                            );
                                        },
                                    );
                                }
                            }
                            refresh_tray_menu(&state, &tray, cx);
                        });
                    }
                })
                .detach();
            }

            // Task 3: Periodic metrics refresh (5s interval, only updates text)
            cx.spawn(async move |cx| {
                loop {
                    cx.background_executor().timer(Duration::from_secs(5)).await;
                    cx.update(|cx| {
                        refresh_tray_menu(&state, &tray, cx);
                    });
                }
            })
            .detach();
        }
        Err(e) => {
            error!(error = %e, "Failed to create tray icon");
        }
    }
}
