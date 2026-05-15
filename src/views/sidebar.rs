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

use crate::{
    connection::get_servers,
    helpers::resolve_tag_color,
    states::{GlobalEvent, Route, ZedisGlobalStore, i18n_sidebar},
};
use gpui::{Context, Hsla, SharedString, Subscription, Window, div, prelude::*, px};
use gpui_component::scroll::ScrollableElement;
use gpui_component::tooltip::Tooltip;
use gpui_component::{ActiveTheme, Icon, IconName, label::Label, list::ListItem, v_flex};
use tracing::info;

// Constants for UI layout
const SERVER_LIST_ITEM_BORDER_WIDTH: f32 = 3.0;

/// Internal state for sidebar component
///
/// Caches server list to avoid repeated queries and tracks current selection.
#[derive(Default)]
struct SidebarState {
    /// Cached server entries shown in the list. First entry is the synthetic
    /// home item (id+name empty, no tag color).
    server_entries: Vec<SidebarEntry>,

    /// Currently selected server ID (empty string means home page)
    server_id: SharedString,
}

#[derive(Clone, Default)]
struct SidebarEntry {
    id: SharedString,
    name: SharedString,
    tag: SharedString,
    color: Option<Hsla>,
}

/// Sidebar navigation component
///
/// Features:
/// - Star button (link to GitHub)
/// - Server list for quick navigation between servers and home
/// - Settings menu with theme and language options
///
/// The sidebar provides quick access to:
/// - Home page (server management)
/// - Connected Redis servers
/// - Application settings (theme, language)
pub struct ZedisSidebar {
    /// Internal state with cached server list
    state: SidebarState,

    /// Event subscriptions for reactive updates
    _subscriptions: Vec<Subscription>,
}

impl ZedisSidebar {
    /// Create a new sidebar component with event subscriptions
    ///
    /// Sets up listeners for:
    /// - Server selection changes (updates current selection)
    /// - Server list updates (refreshes displayed servers)
    pub fn new(_window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut subscriptions = vec![];

        let global_state = cx.global::<ZedisGlobalStore>().state();
        subscriptions.push(cx.subscribe(&global_state, |this, _global_state, event, cx| {
            match event {
                GlobalEvent::ServerListUpdated => {
                    this.update_server_names(cx);
                }
                GlobalEvent::ServerSelected(server_id, _) => {
                    // Refresh server list when servers are added/removed/updated
                    this.state.server_id = server_id.clone();
                }
                _ => {}
            }
            cx.notify();
        }));

        let mut this = Self {
            state: SidebarState::default(),
            _subscriptions: subscriptions,
        };

        info!("Creating new sidebar view");

        // Load initial server list
        this.update_server_names(cx);
        this
    }

    /// Update cached server list from server state
    ///
    /// Rebuilds the server_names list with:
    /// - First entry: (empty, empty) for home page
    /// - Remaining entries: (server_id, server_name) for each configured server
    fn update_server_names(&mut self, _cx: &mut Context<Self>) {
        // Start with home page entry
        let mut entries = vec![SidebarEntry::default()];

        if let Ok(servers) = get_servers() {
            entries.extend(servers.iter().map(|server| SidebarEntry {
                id: server.id.clone().into(),
                name: server.name.clone().into(),
                tag: server.tag_label().unwrap_or_default().to_string().into(),
                color: resolve_tag_color(server.tag_color.as_deref()),
            }));
            self.state.server_entries = entries;
        }
    }

    /// Render the scrollable server list
    ///
    /// Shows:
    /// - Home page item (always first)
    /// - All configured server items
    ///
    /// Current selection is highlighted with background color and border.
    /// Clicking an item navigates to that server or home page.
    fn render_server_list(&self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let entries = self.state.server_entries.clone();
        let current_server_id_clone = self.state.server_id.clone();
        let is_match_route = !matches!(
            cx.global::<ZedisGlobalStore>().read(cx).route(),
            Route::Protos | Route::Scripts | Route::Config
        );

        let home_label = i18n_sidebar(cx, "home");
        let list_active_color = cx.theme().list_active;
        let list_active_border_color = cx.theme().list_active_border;
        let chip_text_color = cx.theme().background;
        // Every row shows the same dashboard glyph; at full foreground
        // weight it's pure repetition competing with the name. Mute it
        // so the server name reads as primary.
        let muted_icon_color = cx.theme().muted_foreground;

        // Build all rows up front. We deliberately do not virtualise via
        // `uniform_list` here: tagged rows render an extra chip and we want
        // each row to size to its own content (no padding gap on untagged
        // rows). Server counts are bounded (typically < 50), so this is fine.
        let rows: Vec<gpui::AnyElement> = entries
            .into_iter()
            .enumerate()
            .map(|(index, entry)| {
                let is_home = entry.id.is_empty();
                let is_current = is_match_route && entry.id == current_server_id_clone;

                let name = if entry.name.is_empty() {
                    home_label.clone()
                } else {
                    entry.name.clone()
                };
                // Full, untruncated name for the row tooltip — the
                // visible label uses text_ellipsis in this narrow
                // strip so long names ("aliyun-clu…") are unreadable
                // without it.
                let full_name = name.clone();

                let server_id = entry.id.clone();
                let tag_color = entry.color;
                let tag_text = entry.tag.clone();
                let has_chip = !tag_text.is_empty() && tag_color.is_some();
                let chip_color = tag_color.unwrap_or_else(gpui::black);

                let item = ListItem::new(("sidebar-redis-server", index))
                    .w_full()
                    .when(is_current, |this| this.bg(list_active_color))
                    .py_3()
                    .border_r(px(SERVER_LIST_ITEM_BORDER_WIDTH))
                    .when(is_current, |this| this.border_color(list_active_border_color))
                    .child(
                        v_flex()
                            .items_center()
                            .gap_1()
                            .child(Icon::new(IconName::LayoutDashboard).text_color(muted_icon_color))
                            .child(Label::new(name).text_ellipsis().text_xs())
                            .when(has_chip, |this| {
                                this.child(
                                    div()
                                        .px_1()
                                        .rounded_sm()
                                        .bg(chip_color)
                                        .child(Label::new(tag_text.clone()).text_xs().text_color(chip_text_color)),
                                )
                            }),
                    )
                    .on_click(move |_, _window, cx| {
                        if is_current {
                            return;
                        }
                        let route = if is_home { Route::Home } else { Route::Editor };
                        cx.update_global::<ZedisGlobalStore, ()>(|store, cx| {
                            store.update(cx, |state, cx| {
                                state.go_to(route, cx);
                                if server_id.is_empty() {
                                    state.clear_selected_server(cx);
                                } else {
                                    state.set_selected_server((server_id.to_string(), 0), cx);
                                }
                            });
                        });
                    });

                // Tag color is conveyed solely by the chip (which
                // requires non-empty tag text). A color without text
                // is treated as not configured, so there's no
                // separate edge strip.
                //
                // ListItem doesn't impl InteractiveElement, so the
                // full-name tooltip lives on a thin stateful wrapper.
                // Home row is never truncated, so skip its tooltip.
                div()
                    .id(("sidebar-redis-server-row", index))
                    .w_full()
                    .child(item)
                    .when(!is_home, |this| {
                        this.tooltip(move |window, cx| Tooltip::new(full_name.clone()).build(window, cx))
                    })
                    .into_any_element()
            })
            .collect();

        v_flex()
            .id("sidebar-redis-servers")
            .size_full()
            .overflow_y_scrollbar()
            .children(rows)
    }
}

impl Render for ZedisSidebar {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .size_full()
            .id("sidebar-container")
            .justify_start()
            .border_r_1()
            .border_color(cx.theme().border)
            .child(div().flex_1().size_full().child(self.render_server_list(window, cx)))
    }
}
