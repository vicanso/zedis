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

use crate::assets::CustomIconName;
use crate::connection::QueryMode;
use crate::states::KeyType;
use crate::states::ZedisServerState;
use crate::states::i18n_common;
use crate::states::i18n_key_tree;
use ahash::AHashSet;
use gpui::AppContext;
use gpui::Corner;
use gpui::Entity;
use gpui::Hsla;
use gpui::SharedString;
use gpui::Subscription;
use gpui::Window;
use gpui::div;
use gpui::prelude::*;
use gpui::px;
use gpui_component::ActiveTheme;
use gpui_component::Disableable;
use gpui_component::Icon;
use gpui_component::IconName;
use gpui_component::StyledExt;
use gpui_component::button::ButtonVariants;
use gpui_component::button::{Button, DropdownButton};
use gpui_component::h_flex;
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::label::Label;
use gpui_component::list::ListItem;
use gpui_component::tree::TreeState;
use gpui_component::tree::tree;
use gpui_component::v_flex;
use tracing::info;

// Constants for tree layout and behavior
const TREE_INDENT_BASE: f32 = 16.0; // Base indentation per level in pixels
const TREE_INDENT_OFFSET: f32 = 8.0; // Additional offset for all items
const EXPANDED_ITEMS_INITIAL_CAPACITY: usize = 10;
const AUTO_EXPAND_THRESHOLD: usize = 20; // Auto-expand tree if fewer than this many keys
const KEY_TYPE_FADE_ALPHA: f32 = 0.8; // Background transparency for key type badges
const KEY_TYPE_BORDER_FADE_ALPHA: f32 = 0.5; // Border transparency for key type badges
const STRIPE_BACKGROUND_ALPHA_DARK: f32 = 0.1; // Odd row background alpha for dark theme
const STRIPE_BACKGROUND_ALPHA_LIGHT: f32 = 0.03; // Odd row background alpha for light theme

#[derive(Default)]
struct KeyTreeState {
    server_id: SharedString,
    /// Unique ID for the current key tree (changes when keys are reloaded)
    key_tree_id: SharedString,
    /// Whether the tree is empty (no keys found)
    is_empty: bool,
    /// Current query mode (All/Prefix/Exact)
    query_mode: QueryMode,
    /// Error message to display if key loading fails
    error: Option<SharedString>,
    /// Set of expanded folder paths (persisted during tree rebuilds)
    expanded_items: AHashSet<SharedString>,
}

/// Key tree view component for browsing and filtering Redis keys
///
/// Displays Redis keys in a hierarchical tree structure with:
/// - Folder navigation for key namespaces (using colon separators)
/// - Key type indicators (String, List, etc.) with color-coded badges
/// - Multiple query modes (All, Prefix, Exact)
/// - Real-time filtering and search
/// - Expandable/collapsible folders
/// - Visual feedback for selected keys
pub struct ZedisKeyTree {
    state: KeyTreeState,

    /// Reference to server state for Redis operations
    server_state: Entity<ZedisServerState>,

    /// Tree component state for rendering hierarchical structure
    tree_state: Entity<TreeState>,

    /// Input field state for keyword filtering
    keyword_state: Entity<InputState>,

    /// Event subscriptions for reactive updates
    _subscriptions: Vec<Subscription>,
}

impl ZedisKeyTree {
    /// Create a new key tree view with event subscriptions
    ///
    /// Sets up reactive updates when server state changes and
    /// initializes UI components (tree, search input).
    pub fn new(server_state: Entity<ZedisServerState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut subscriptions = Vec::new();

        // Subscribe to server state changes to rebuild tree when keys change
        subscriptions.push(cx.observe(&server_state, |this, _model, cx| {
            this.update_key_tree(cx);
        }));

        // Initialize tree state for hierarchical rendering
        let tree_state = cx.new(|cx| TreeState::new(cx));

        // Initialize keyword search input with placeholder
        let keyword_state = cx.new(|cx| {
            InputState::new(window, cx)
                .clean_on_escape()
                .placeholder(i18n_common(cx, "filter_placeholder"))
        });
        keyword_state.update(cx, |state, cx| {
            state.focus(window, cx);
        });

        let server_state_value = server_state.read(cx);
        let server_id = server_state_value.server_id().to_string();
        let query_mode = server_state_value.query_mode();

        // Subscribe to search input events (Enter key triggers filter)
        subscriptions.push(cx.subscribe_in(&keyword_state, window, |view, _, event, _, cx| {
            if let InputEvent::PressEnter { .. } = &event {
                view.handle_filter(cx);
            }
        }));

        info!(server_id, "Creating new key tree view");

        let mut this = Self {
            state: KeyTreeState {
                query_mode,
                server_id: server_id.into(),
                expanded_items: AHashSet::with_capacity(EXPANDED_ITEMS_INITIAL_CAPACITY),
                ..Default::default()
            },
            tree_state,
            keyword_state,
            server_state,
            _subscriptions: subscriptions,
        };

        // Initial tree build
        this.update_key_tree(cx);

        this
    }

    /// Update the key tree structure when server state changes
    ///
    /// Rebuilds the tree only if the tree ID has changed (indicating new keys loaded).
    /// Preserves expanded folder state across rebuilds. Auto-expands all folders
    /// if the total key count is below the threshold.
    fn update_key_tree(&mut self, cx: &mut Context<Self>) {
        let server_state = self.server_state.read(cx);

        tracing::debug!(
            key_tree_server_id = server_state.server_id(),
            key_tree_id = server_state.key_tree_id(),
            "Server state updated"
        );

        self.state.query_mode = server_state.query_mode();

        // Skip rebuild if tree ID hasn't changed (same keys)
        if self.state.key_tree_id == server_state.key_tree_id() {
            return;
        }

        // Auto-expand all folders if key count is small
        let expand_all = server_state.scan_count() < AUTO_EXPAND_THRESHOLD;
        let items = server_state.key_tree(&self.state.expanded_items, expand_all);

        // Clear expanded items if tree is now empty
        if items.is_empty() {
            self.state.expanded_items.clear();
        }

        // Update empty state (only if not currently scanning)
        self.state.is_empty = items.is_empty() && !server_state.scaning();

        // Update tree component with new items
        self.tree_state.update(cx, |state, cx| {
            state.set_items(items, cx);
            cx.notify();
        });
    }

    /// Handle filter/search action when user submits keyword
    ///
    /// Delegates to server state to perform the actual filtering based on
    /// current query mode. Ignores if a scan is already in progress.
    fn handle_filter(&mut self, cx: &mut Context<Self>) {
        // Don't trigger filter while already scanning
        if self.server_state.read(cx).scaning() {
            return;
        }

        let keyword = self.keyword_state.read(cx).value();
        self.server_state.update(cx, move |handle, cx| {
            handle.handle_filter(keyword, cx);
        });
    }

    /// Render the tree view or empty state message
    ///
    /// Displays:
    /// - Tree structure with keys and folders (normal state)
    /// - "Key not exists" message (Exact mode with expired key)
    /// - Error or "no keys found" message (empty state)
    fn render_tree(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let server_state = self.server_state.read(cx);

        // Handle empty states when not scanning
        if !server_state.scaning() && (self.state.is_empty || self.state.error.is_some()) {
            // Special case: Exact mode with expired/non-existent key
            if self.state.query_mode == QueryMode::Exact {
                if let Some(value) = server_state.value()
                    && value.is_expired()
                {
                    return h_flex()
                        .w_full()
                        .items_center()
                        .justify_center()
                        .gap_2()
                        .pt_5()
                        .px_2()
                        .child(Label::new(i18n_key_tree(cx, "key_not_exists")).text_sm())
                        .into_any_element();
                }
                return h_flex().into_any_element();
            }

            // Show error message or generic "no keys found"
            let text = self
                .state
                .error
                .clone()
                .unwrap_or_else(|| i18n_key_tree(cx, "no_keys_found"));
            return div()
                .h_flex()
                .w_full()
                .items_center()
                .justify_center()
                .gap_2()
                .pt_5()
                .px_2()
                .child(Icon::new(IconName::Info).text_sm())
                .child(
                    div()
                        .flex_1()
                        .overflow_hidden()
                        .child(Label::new(text).text_sm().whitespace_normal()),
                )
                .into_any_element();
        }
        // Prepare colors and state for tree rendering
        let view = cx.entity();
        let yellow = cx.theme().colors.yellow;
        let selected_key = server_state.key().unwrap_or_default();
        let server_state = self.server_state.clone();
        let even_bg = cx.theme().background;

        // Zebra striping for better readability
        let odd_bg = if cx.theme().is_dark() {
            Hsla::white().alpha(STRIPE_BACKGROUND_ALPHA_DARK)
        } else {
            Hsla::black().alpha(STRIPE_BACKGROUND_ALPHA_LIGHT)
        };

        let list_active_color = cx.theme().list_active;
        let list_active_border_color = cx.theme().list_active_border;
        tree(&self.tree_state, move |ix, entry, _selected, _window, cx| {
            view.update(cx, |_, cx| {
                let item = entry.item();

                // Render appropriate icon based on item type
                let icon = if !entry.is_folder() {
                    // Key item: Show type badge (String, List, etc.)
                    let key_type = server_state.read(cx).key_type(&item.id).unwrap_or(&KeyType::Unknown);

                    if key_type == &KeyType::Unknown {
                        div().into_any_element()
                    } else {
                        // Create colored badge with faded background and border
                        let key_type_color = key_type.color();
                        let mut key_type_bg = key_type_color;
                        key_type_bg.fade_out(KEY_TYPE_FADE_ALPHA);
                        let mut key_type_border = key_type_color;
                        key_type_border.fade_out(KEY_TYPE_BORDER_FADE_ALPHA);

                        Label::new(key_type.as_str())
                            .text_xs()
                            .bg(key_type_bg)
                            .text_color(key_type_color)
                            .border_1()
                            .px_1()
                            .rounded_sm()
                            .border_color(key_type_border)
                            .into_any_element()
                    }
                } else if entry.is_expanded() {
                    // Expanded folder: Show open folder icon
                    Icon::new(IconName::FolderOpen).text_color(yellow).into_any_element()
                } else {
                    // Collapsed folder: Show closed folder icon
                    Icon::new(IconName::Folder).text_color(yellow).into_any_element()
                };
                // Determine background color: selected > zebra striping
                let bg = if item.id == selected_key {
                    list_active_color
                } else if ix % 2 == 0 {
                    even_bg
                } else {
                    odd_bg
                };

                // Show child count for folders
                let count_label = if entry.is_folder() {
                    Label::new(item.children.len().to_string())
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                } else {
                    Label::new("")
                };

                // Only clone minimal data: id and folder flag
                let item_id = item.id.clone();
                let is_folder = item.is_folder();

                let handle_select_item = cx.listener(move |this, _, _window, cx| {
                    if is_folder {
                        // Check REAL-TIME expanded state from our state management
                        // Note: item.is_expanded() reflects render-time state from TreeState,
                        // but we need to check if it's ACTUALLY in our expanded set
                        let currently_in_expanded_set = this.state.expanded_items.contains(&item_id);

                        if currently_in_expanded_set {
                            // User clicked an expanded folder -> collapse it
                            this.state.expanded_items.remove(&item_id);
                        } else {
                            // User clicked a collapsed folder -> expand it and load data
                            this.state.expanded_items.insert(item_id.clone());
                            this.server_state.update(cx, |state, cx| {
                                state.scan_prefix(format!("{}:", item_id.as_str()).into(), cx);
                            });
                        }
                        return;
                    }
                    if this.server_state.read(cx).key() == Some(item_id.clone()) {
                        return;
                    }

                    // Key click: Select the key for editing
                    this.server_state.update(cx, |state, cx| {
                        state.select_key(item_id.clone(), cx);
                    });
                });
                ListItem::new(ix)
                    .w_full()
                    .bg(bg)
                    .py_1()
                    .px_2()
                    .pl(px(TREE_INDENT_BASE) * entry.depth() + px(TREE_INDENT_OFFSET))
                    .when(item.id == selected_key, |this| {
                        this.border_r_3().border_color(list_active_border_color)
                    })
                    .child(
                        h_flex()
                            .gap_2()
                            .child(icon)
                            .child(div().flex_1().text_ellipsis().child(item.label.clone()))
                            .child(count_label),
                    )
                    .on_click(handle_select_item)
            })
        })
        .text_sm()
        .p_1()
        .bg(cx.theme().sidebar)
        .text_color(cx.theme().sidebar_foreground)
        .h_full()
        .into_any_element()
    }
    /// Render the search/filter input bar with query mode selector
    ///
    /// Features:
    /// - Query mode dropdown (All/Prefix/Exact) with visual indicators
    /// - Search input field with placeholder
    /// - Search button (with loading state during scan)
    /// - Clearable input (X button appears when text entered)
    fn render_keyword_input(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let server_state = self.server_state.read(cx);
        let scaning = server_state.scaning();
        let server_id = server_state.server_id();
        if server_id != self.state.server_id.as_str() {
            self.state.server_id = server_id.to_string().into();
            self.keyword_state.update(cx, |state, cx| {
                state.set_value(SharedString::default(), window, cx);
            });
        }
        let query_mode = self.state.query_mode;

        // Select icon based on query mode
        let icon = match query_mode {
            QueryMode::All => Icon::new(IconName::Asterisk), // * for all keys
            QueryMode::Prefix => Icon::new(CustomIconName::ChevronUp), // ~ for prefix
            QueryMode::Exact => Icon::new(CustomIconName::Equal), // = for exact match
        };
        let query_mode_dropdown = DropdownButton::new("dropdown")
            .button(Button::new("key-tree-query-mode-btn").ghost().px_2().icon(icon))
            .dropdown_menu_with_anchor(Corner::TopLeft, move |menu, _, _| {
                // Build menu with checkmarks for current mode
                menu.menu_element_with_check(query_mode == QueryMode::All, Box::new(QueryMode::All), |_, cx| {
                    Label::new(i18n_key_tree(cx, "query_mode_all")).ml_2().text_xs()
                })
                .menu_element_with_check(query_mode == QueryMode::Prefix, Box::new(QueryMode::Prefix), |_, cx| {
                    Label::new(i18n_key_tree(cx, "query_mode_prefix")).ml_2().text_xs()
                })
                .menu_element_with_check(
                    query_mode == QueryMode::Exact,
                    Box::new(QueryMode::Exact),
                    |_, cx| Label::new(i18n_key_tree(cx, "query_mode_exact")).ml_2().text_xs(),
                )
            });
        // Search button (shows loading spinner during scan)
        let search_btn = Button::new("key-tree-search-btn")
            .ghost()
            .tooltip(i18n_key_tree(cx, "search_tooltip"))
            .loading(scaning)
            .disabled(scaning)
            .icon(IconName::Search)
            .on_click(cx.listener(|this, _, _, cx| {
                this.handle_filter(cx);
            }));
        // keyword input
        let keyword_input = Input::new(&self.keyword_state)
            .w_full()
            .flex_1()
            .px_0()
            .prefix(query_mode_dropdown)
            .suffix(search_btn)
            .cleanable(true);
        h_flex()
            .p_2()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(keyword_input)
    }
}

impl Render for ZedisKeyTree {
    /// Main render method - displays search bar and tree structure
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .h_full()
            .w_full()
            .child(self.render_keyword_input(window, cx))
            .child(self.render_tree(cx))
            .on_action(cx.listener(|this, e: &QueryMode, _window, cx| {
                let new_mode = *e;

                // Step 1: Update server state with new query mode
                this.server_state.update(cx, |state, cx| {
                    state.set_query_mode(new_mode, cx);
                });

                // Step 2: Update local UI state
                this.state.query_mode = new_mode;
            }))
    }
}
