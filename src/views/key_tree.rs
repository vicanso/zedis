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
    assets::CustomIconName,
    components::{FormDialog, FormField, SkeletonLoading, open_add_form_dialog},
    db::HistoryManager,
    helpers::{EditorAction, get_font_family, humanize_keystroke, validate_long_string, validate_ttl},
    states::{
        KeyType, QueryMode, ServerEvent, ZedisGlobalStore, ZedisServerState, dialog_button_props, get_session_option,
        i18n_common, i18n_key_tree, save_session_option,
    },
};
use ahash::{AHashMap, AHashSet};
use gpui::{
    Action, App, AppContext, Corner, Entity, FocusHandle, Focusable, Hsla, ScrollStrategy, SharedString, Subscription,
    Task, Window, div, prelude::*, px,
};
use gpui_component::{
    ActiveTheme, Disableable, Icon, IconName, IndexPath, StyledExt, WindowExt,
    button::{Button, ButtonVariants, DropdownButton},
    h_flex,
    input::{Input, InputEvent, InputState},
    label::Label,
    menu::ContextMenuExt,
    v_flex,
};
use gpui_component::{
    list::{List, ListDelegate, ListEvent, ListItem, ListState},
    menu::DropdownMenu,
};
use rust_i18n::t;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::{rc::Rc, str::FromStr, time::Duration};
use tracing::info;

// Constants for tree layout and behavior
const TREE_INDENT_BASE: f32 = 16.0; // Base indentation per level in pixels
const TREE_INDENT_OFFSET: f32 = 8.0; // Additional offset for all items
const EXPANDED_ITEMS_INITIAL_CAPACITY: usize = 10;
const KEY_TYPE_FADE_ALPHA: f32 = 0.8; // Background transparency for key type badges
const KEY_TYPE_BORDER_FADE_ALPHA: f32 = 0.5; // Border transparency for key type badges
const STRIPE_BACKGROUND_ALPHA_DARK: f32 = 0.1; // Odd row background alpha for dark theme
const STRIPE_BACKGROUND_ALPHA_LIGHT: f32 = 0.03; // Odd row background alpha for light theme

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Action)]
enum KeyTreeAction {
    Search(SharedString),
    Clear,
    DeleteMultipleKeys,
    DeleteKey(SharedString),
    DeleteFolder(SharedString),
    CollapseAllKeys,
    ToggleMultiSelectMode,
    AutoRefresh(u32),
}

#[derive(Default)]
struct KeyTreeState {
    /// current keyword
    keyword: SharedString,
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
    /// Index path to scroll to when the tree is updated
    scroll_to_index: Option<IndexPath>,
    /// Refresh interval in seconds
    refresh_interval_sec: u32,
}

#[derive(Default, Debug, Clone)]
struct KeyTreeItem {
    id: SharedString,
    label: SharedString,
    depth: usize,
    key_type: KeyType,
    expanded: bool,
    children_count: usize,
    is_folder: bool,
}

fn new_key_tree_items(
    mut keys: Vec<(SharedString, KeyType)>,
    keyword: SharedString,
    expanded_items: AHashSet<SharedString>,
    separator: &str,
    max_key_tree_depth: usize,
) -> Vec<KeyTreeItem> {
    keys.sort_unstable_by_key(|(k, _)| k.clone());
    let expanded_items_set = expanded_items.iter().map(|s| s.as_str()).collect::<AHashSet<&str>>();
    let mut items: AHashMap<SharedString, KeyTreeItem> = AHashMap::with_capacity(100);

    for (key, key_type) in keys {
        if !keyword.is_empty() && !key.contains(keyword.as_str()) {
            continue;
        }
        // no colon in the key, it's a simple key
        if !key.contains(separator) {
            items.insert(
                key.clone(),
                KeyTreeItem {
                    id: key.clone(),
                    label: key.clone(),
                    key_type,
                    ..Default::default()
                },
            );
            continue;
        }

        let mut dir = String::with_capacity(50);
        let mut key_tree_item: Option<KeyTreeItem> = None;
        // max levels of depth
        for (index, k) in key.splitn(max_key_tree_depth, separator).enumerate() {
            // if key_tree_item is not None, it means we are in a folder
            // because it's not the last part of the key
            let expanded = index == 0 || expanded_items_set.contains(dir.as_str());
            if let Some(key_tree_item) = key_tree_item.take() {
                let entry = items.entry(key_tree_item.id.clone()).or_insert_with(|| key_tree_item);
                entry.is_folder = true;
                entry.children_count += 1;
                entry.expanded = expanded;
            }

            if !expanded {
                break;
            }
            let name: SharedString = k.to_string().into();
            if index != 0 {
                dir.push_str(separator);
            };
            dir.push_str(k);

            key_tree_item = Some(KeyTreeItem {
                id: dir.clone().into(),
                label: name.clone(),
                key_type,
                depth: index,
                expanded,
                ..Default::default()
            });
        }
        if let Some(key_tree_item) = key_tree_item.take() {
            items.insert(key_tree_item.id.clone(), key_tree_item);
        }
    }

    let mut children_map: AHashMap<String, Vec<KeyTreeItem>> = AHashMap::new();

    let mut result = Vec::with_capacity(items.len());

    for item in items.into_values() {
        let size = item.id.len() - item.label.len();
        let parent_id = if size == 0 { "" } else { &item.id[..(size - 1)] };
        children_map.entry(parent_id.to_string()).or_default().push(item);
    }

    fn build_sorted_list(parent_id: &str, map: &mut AHashMap<String, Vec<KeyTreeItem>>, result: &mut Vec<KeyTreeItem>) {
        if let Some(mut children) = map.remove(parent_id) {
            children.sort_unstable_by(|a, b| b.is_folder.cmp(&a.is_folder).then_with(|| a.label.cmp(&b.label)));

            for child in children {
                let child_id = child.id.to_string();
                result.push(child);
                build_sorted_list(&child_id, map, result);
            }
        }
    }

    build_sorted_list("", &mut children_map, &mut result);

    result
}

struct KeyTreeDelegate {
    items: Vec<KeyTreeItem>,
    selected_index: Option<IndexPath>,
    enabled_multiple_selection: bool,
    selected_items: AHashSet<SharedString>,
    readonly: bool,
}

impl KeyTreeDelegate {
    /// Renders the colored badge for key types (String, Hash, etc.)
    fn render_key_type_badge(&self, key_type: &KeyType) -> impl IntoElement {
        if key_type == &KeyType::Unknown {
            return div().into_any_element();
        }

        let color = key_type.color();
        let mut bg = color;
        bg.fade_out(KEY_TYPE_FADE_ALPHA);
        let mut border = color;
        border.fade_out(KEY_TYPE_BORDER_FADE_ALPHA);

        Label::new(key_type.as_str())
            .text_size(px(10.))
            .w(px(36.))
            .text_center()
            .bg(bg)
            .text_color(color)
            .border_1()
            .px_1()
            .rounded_sm()
            .border_color(border)
            .into_any_element()
    }
    fn toggle_multiple_selection(&mut self, cx: &mut Context<ListState<Self>>) {
        self.enabled_multiple_selection = !self.enabled_multiple_selection;
        if self.enabled_multiple_selection {
            self.selected_items.clear();
        }
        cx.notify();
    }
}

impl ListDelegate for KeyTreeDelegate {
    type Item = ListItem;

    fn items_count(&self, _section: usize, _cx: &App) -> usize {
        self.items.len()
    }

    fn render_item(
        &mut self,
        ix: IndexPath,
        _window: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) -> Option<Self::Item> {
        let yellow = cx.theme().colors.yellow;
        let entry = self.items.get(ix.row)?;
        let icon = if !entry.is_folder {
            // Key item: Show type badge (String, List, etc.)
            self.render_key_type_badge(&entry.key_type).into_any_element()
        } else if entry.expanded {
            // Expanded folder: Show open folder icon
            Icon::new(IconName::FolderOpen).text_color(yellow).into_any_element()
        } else {
            // Collapsed folder: Show closed folder icon
            Icon::new(IconName::Folder).text_color(yellow).into_any_element()
        };

        let even_bg = cx.theme().background;
        let is_dark = cx.theme().is_dark();

        // Zebra striping for better readability
        let odd_bg = if is_dark {
            Hsla::white().alpha(STRIPE_BACKGROUND_ALPHA_DARK)
        } else {
            Hsla::black().alpha(STRIPE_BACKGROUND_ALPHA_LIGHT)
        };
        let is_folder = entry.is_folder;

        let label_color = if is_folder {
            cx.theme().foreground.alpha(0.85)
        } else {
            cx.theme().foreground
        };

        let bg = if ix.row.is_multiple_of(2) { even_bg } else { odd_bg };
        let show_check_icon = self.enabled_multiple_selection && !is_folder;
        let selected = if show_check_icon && let Some(item) = self.items.get(ix.row) {
            let id = &item.id;
            self.selected_items.contains(id)
        } else {
            false
        };
        let selected_items_count = self.selected_items.len();
        let id = entry.id.clone();
        let readonly = self.readonly;
        Some(
            ListItem::new(ix)
                .font_family(get_font_family())
                .w_full()
                .bg(bg)
                .py_2()
                .px_2()
                .pl(px(TREE_INDENT_BASE) * entry.depth + px(TREE_INDENT_OFFSET))
                .child(
                    div()
                        .context_menu(move |mut menu, _window, _cx| {
                            if readonly {
                                return menu;
                            }
                            let id = id.clone();
                            if selected && selected_items_count > 1 {
                                let text = t!("key_tree.delete_keys_tooltip", count = selected_items_count);
                                menu = menu.menu_element_with_icon(
                                    CustomIconName::ListX,
                                    Box::new(KeyTreeAction::DeleteMultipleKeys),
                                    move |_, _cx| Label::new(text.clone()),
                                );
                            } else {
                                menu = if is_folder {
                                    menu.menu_element_with_icon(
                                        CustomIconName::X,
                                        Box::new(KeyTreeAction::DeleteFolder(id)),
                                        move |_, cx| Label::new(i18n_key_tree(cx, "delete_folder_tooltip")),
                                    )
                                } else {
                                    menu.menu_element_with_icon(
                                        CustomIconName::X,
                                        Box::new(KeyTreeAction::DeleteKey(id)),
                                        move |_, cx| Label::new(i18n_key_tree(cx, "delete_key_tooltip")),
                                    )
                                };
                            }
                            menu
                        })
                        .child(
                            div()
                                .h_flex()
                                .gap_2()
                                .flex_1()
                                .min_w_0()
                                .child(icon)
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .child(Label::new(entry.label.clone()).text_color(label_color).text_ellipsis()),
                                )
                                .when(show_check_icon, |this| {
                                    let check_icon = if selected {
                                        CustomIconName::SquareCheck
                                    } else {
                                        CustomIconName::Square
                                    };
                                    this.child(Icon::new(check_icon))
                                })
                                .when(entry.is_folder, |this| {
                                    this.child(
                                        Label::new(entry.children_count.to_string())
                                            .text_sm()
                                            .text_color(cx.theme().muted_foreground),
                                    )
                                }),
                        ),
                ),
        )
    }

    fn set_selected_index(&mut self, ix: Option<IndexPath>, _window: &mut Window, _cx: &mut Context<ListState<Self>>) {
        if self.enabled_multiple_selection
            && let Some(ix) = ix
            && let Some(item) = self.items.get(ix.row)
        {
            let id = &item.id;
            if self.selected_items.contains(id) {
                self.selected_items.remove(id);
            } else {
                self.selected_items.insert(id.clone());
            }
        }
        self.selected_index = ix;
    }
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
    focus_handle: FocusHandle,

    auto_refresh_task: Option<Task<()>>,

    state: KeyTreeState,

    current_keyword: Entity<SharedString>,

    /// Reference to server state for Redis operations
    server_state: Entity<ZedisServerState>,

    /// Delegate for the key tree list
    // key_tree_delegate: Entity<KeyTreeDelegate>,

    /// State for the key tree list
    key_tree_list_state: Entity<ListState<KeyTreeDelegate>>,

    /// Input field state for keyword filtering
    keyword_state: Entity<InputState>,

    /// Whether to enter add key mode
    should_enter_add_key_mode: Option<bool>,

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

        let focus_handle = cx.focus_handle();
        focus_handle.focus(window);

        // Subscribe to server state changes to rebuild tree when keys change
        subscriptions.push(cx.observe(&server_state, |this, _model, cx| {
            this.update_key_tree(false, cx);
        }));
        subscriptions.push(
            cx.subscribe(&server_state, |this, server_state, event, cx| match event {
                ServerEvent::KeyCollapseAll => {
                    this.state.expanded_items.clear();
                    this.update_key_tree(true, cx);
                }
                ServerEvent::ServerSelected(_) => {
                    this.reset(cx);
                }
                ServerEvent::ServerInfoUpdated => {
                    let readonly = server_state.read(cx).readonly();
                    this.key_tree_list_state.update(cx, |state, _cx| {
                        state.delegate_mut().readonly = readonly;
                    });
                }
                ServerEvent::EditionActionTriggered(action) => {
                    if action == &EditorAction::Create {
                        this.should_enter_add_key_mode = Some(true);
                        cx.notify();
                    }
                }
                ServerEvent::KeyScanFinished => {
                    let keys = server_state.read(cx).keys();
                    let global_state = cx.global::<ZedisGlobalStore>().read(cx);
                    if keys.len() > global_state.auto_expand_threshold() {
                        return;
                    }
                    let key_separator = global_state.key_separator();
                    let mut expanded_items: AHashSet<SharedString> = AHashSet::new();
                    keys.iter().for_each(|(key, _)| {
                        if !key.contains(key_separator) {
                            return;
                        }
                        let parts: Vec<&str> = key.split(key_separator).collect();
                        for i in 1..parts.len() {
                            let prefix = parts[..i].join(key_separator);
                            expanded_items.insert(prefix.into());
                        }
                    });
                    this.state.expanded_items = expanded_items;
                    cx.notify();
                }
                _ => {}
            }),
        );

        // Initialize keyword search input with placeholder
        let keyword_state = cx.new(|cx| {
            InputState::new(window, cx)
                .clean_on_escape()
                .placeholder(i18n_common(cx, "filter_placeholder"))
        });
        // initial focus
        keyword_state.update(cx, |state, cx| {
            state.focus(window, cx);
        });

        let server_state_value = server_state.read(cx);
        let server_id = server_state_value.server_id().to_string();
        let mut query_mode = QueryMode::All;
        let mut refresh_interval_sec = 0;
        if let Ok(option) = get_session_option(&server_id) {
            query_mode = option
                .query_mode
                .as_deref()
                .and_then(|s| QueryMode::from_str(s).ok())
                .unwrap_or(QueryMode::All);
            refresh_interval_sec = option.refresh_interval_sec.unwrap_or_default();
        }
        let readonly = server_state_value.readonly();

        // Subscribe to search input events (Enter key triggers filter)
        subscriptions.push(cx.subscribe_in(&keyword_state, window, |view, _, event, _, cx| {
            if let InputEvent::PressEnter { .. } = &event {
                view.handle_filter(cx);
            }
        }));

        info!(server_id, "Creating new key tree view");

        let delegate = KeyTreeDelegate {
            items: Vec::new(),
            enabled_multiple_selection: false,
            selected_index: None,
            selected_items: AHashSet::with_capacity(5),
            readonly,
        };
        let key_tree_list_state = cx.new(|cx| ListState::new(delegate, window, cx));
        subscriptions.push(cx.subscribe(&key_tree_list_state, |view, _, event, cx| match event {
            ListEvent::Select(ix) => {
                view.select_item_by_index(ix, false, cx);
            }
            ListEvent::Confirm(ix) => {
                view.select_item_by_index(ix, true, cx);
            }
            _ => {}
        }));

        let mut this = Self {
            focus_handle,
            state: KeyTreeState {
                query_mode,
                server_id: server_id.into(),
                refresh_interval_sec,
                expanded_items: AHashSet::with_capacity(EXPANDED_ITEMS_INITIAL_CAPACITY),
                ..Default::default()
            },
            current_keyword: cx.new(|_cx| SharedString::default()),
            key_tree_list_state,
            keyword_state,
            server_state,
            should_enter_add_key_mode: None,
            auto_refresh_task: None,
            _subscriptions: subscriptions,
        };

        // Initial tree build
        this.update_key_tree(true, cx);
        this.start_auto_refresh(cx);

        this
    }

    fn start_auto_refresh(&mut self, cx: &mut Context<Self>) {
        let auto_refresh_interval_sec = self.state.refresh_interval_sec;
        if auto_refresh_interval_sec == 0 {
            self.auto_refresh_task = None;
            return;
        }
        let server_state = self.server_state.clone();
        let current_keyword = self.current_keyword.clone();
        self.auto_refresh_task = Some(cx.spawn(async move |_, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_secs(auto_refresh_interval_sec as u64))
                    .await;
                let keyword = current_keyword
                    .update(cx, |state, _cx| state.clone())
                    .unwrap_or_default();
                info!(keyword = keyword.as_str(), "auto refresh");
                let _ = server_state.update(cx, move |handle, cx| {
                    handle.handle_auto_refresh(keyword, cx);
                });
            }
        }));
    }

    fn reset(&mut self, _cx: &mut Context<Self>) {
        self.state = KeyTreeState::default();
    }
    fn reset_expand(&mut self, _cx: &mut Context<Self>) {
        self.state.expanded_items.clear();
        self.state.scroll_to_index = Some(IndexPath::new(0));
    }

    /// Update the key tree structure when server state changes
    ///
    /// Rebuilds the tree only if the tree ID has changed (indicating new keys loaded).
    /// Preserves expanded folder state across rebuilds. Auto-expands all folders
    /// if the total key count is below the threshold.
    fn update_key_tree(&mut self, force_update: bool, cx: &mut Context<Self>) {
        let server_state = self.server_state.read(cx);
        let key_tree_id = server_state.key_tree_id();

        tracing::debug!(
            key_tree_server_id = server_state.server_id(),
            key_tree_id,
            "Server state updated"
        );

        self.state.query_mode = server_state.query_mode();

        // Skip rebuild if tree ID hasn't changed (same keys)
        if !force_update && self.state.key_tree_id == key_tree_id {
            return;
        }
        self.state.key_tree_id = key_tree_id.to_string().into();

        // Auto-expand all folders if key count is small
        let keys_snapshot: Vec<(SharedString, KeyType)> =
            server_state.keys().iter().map(|(k, v)| (k.clone(), *v)).collect();
        let readonly = server_state.readonly();
        let expanded_items = self.state.expanded_items.clone();

        let view_handle = cx.entity().downgrade();
        let keyword = self.state.keyword.clone();

        self.key_tree_list_state.update(cx, move |_state, cx| {
            let app_state = cx.global::<ZedisGlobalStore>().value(cx);
            let separator = app_state.key_separator().to_string();
            let max_key_tree_depth = app_state.max_key_tree_depth();
            cx.spawn(async move |handle, cx| {
                let task = cx.background_spawn(async move {
                    let start = std::time::Instant::now();
                    let items =
                        new_key_tree_items(keys_snapshot, keyword, expanded_items, &separator, max_key_tree_depth);
                    tracing::debug!("Key tree build time: {:?}", start.elapsed());
                    items
                });

                let result = task.await;
                if result.is_empty() {
                    let _ = view_handle.update(cx, |view: &mut ZedisKeyTree, cx| {
                        view.reset_expand(cx);
                    });
                }
                handle.update(cx, |this, cx| {
                    this.delegate_mut().selected_items.clear();
                    this.delegate_mut().items = result;
                    this.delegate_mut().readonly = readonly;
                    cx.notify();
                })
            })
            .detach();
        });
    }

    /// Handle filter/search action when user submits keyword
    ///
    /// Delegates to server state to perform the actual filtering based on
    /// current query mode. Ignores if a scan is already in progress.
    fn handle_filter(&mut self, cx: &mut Context<Self>) {
        // Don't trigger filter while already scanning
        let server_state = self.server_state.read(cx);
        if server_state.scanning() {
            return;
        }

        let keyword = self.keyword_state.read(cx).value();
        self.state.keyword = keyword.clone();

        let server_id_clone = server_state.server_id().to_string();
        let keyword_clone = keyword.clone();
        self.current_keyword
            .update(cx, |state, _cx| *state = keyword_clone.clone());
        cx.spawn(async move |_, cx| {
            let _ = cx
                .background_spawn(async move {
                    let _ = HistoryManager::add_record(server_id_clone.as_str(), keyword_clone.as_str());
                })
                .await;
        })
        .detach();
        self.server_state.update(cx, move |handle, cx| {
            handle.handle_filter(keyword, cx);
        });
    }
    fn handle_clear_history(&mut self, cx: &mut Context<Self>) {
        let server_state = self.server_state.read(cx);
        let server_id = server_state.server_id().to_string();
        self.server_state.update(cx, |state, cx| {
            state.clear_search_history(cx);
        });
        cx.spawn(async move |_, cx| {
            let _ = cx
                .background_spawn(async move {
                    let _ = HistoryManager::clear_history(server_id.as_str());
                })
                .await;
        })
        .detach();
    }

    fn handle_add_key(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let category_list = ["String", "List", "Set", "Zset", "Hash"];
        let fields = vec![
            FormField::new(i18n_key_tree(cx, "category"))
                .with_options(category_list.iter().map(|s| s.to_string().into()).collect()),
            FormField::new(i18n_common(cx, "key"))
                .with_placeholder(i18n_common(cx, "key_placeholder"))
                .with_focus()
                .with_validate(validate_long_string),
            FormField::new(i18n_common(cx, "ttl"))
                .with_placeholder(i18n_common(cx, "ttl_placeholder"))
                .with_validate(validate_ttl),
        ];
        let server_state = self.server_state.clone();
        let handle_submit = Rc::new(move |values: Vec<SharedString>, window: &mut Window, cx: &mut App| {
            if values.len() != 3 {
                return false;
            }
            let index = values[0].parse::<usize>().unwrap_or(0);
            let category = category_list.get(index).cloned().unwrap_or_default();

            server_state.update(cx, |this, cx| {
                this.add_key(category.to_string().into(), values[1].clone(), values[2].clone(), cx);
            });
            window.close_dialog(cx);
            true
        });

        open_add_form_dialog(
            FormDialog {
                title: i18n_key_tree(cx, "add_key_title"),
                fields,
                handle_submit,
            },
            window,
            cx,
        );
        let entity_id = cx.entity_id();
        cx.defer(move |cx| {
            cx.notify(entity_id);
        });
    }

    fn get_tree_status_view(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let server_state = self.server_state.read(cx);
        // if scanning, return None
        if server_state.scanning() {
            if self.key_tree_list_state.read(cx).delegate().items.is_empty() {
                return Some(div().m_5().child(SkeletonLoading::new()).into_any_element());
            }
            return None;
        }
        if !self.state.is_empty && self.state.error.is_none() {
            return None;
        }

        let mut text = SharedString::default();

        if self.state.query_mode == QueryMode::Exact {
            if let Some(value) = server_state.value()
                && value.is_expired()
            {
                text = i18n_key_tree(cx, "key_not_exists");
            }
        } else {
            text = self
                .state
                .error
                .clone()
                .unwrap_or_else(|| i18n_key_tree(cx, "no_keys_found"))
        }
        if text.is_empty() {
            return Some(h_flex().into_any_element());
        }
        Some(
            div()
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
                .into_any_element(),
        )
    }

    fn select_item_by_index(&mut self, ix: &IndexPath, toggle: bool, cx: &mut Context<Self>) {
        let Some((id, is_folder)) = self.key_tree_list_state.update(cx, |state, _cx| {
            let item = state.delegate().items.get(ix.row)?;
            let id = item.id.clone();
            let is_folder = item.is_folder;
            Some((id, is_folder))
        }) else {
            return;
        };
        self.select_item(id, is_folder, toggle, cx);
    }

    fn select_item(&mut self, item_id: SharedString, is_folder: bool, toggle: bool, cx: &mut Context<Self>) {
        if is_folder {
            if self.state.expanded_items.contains(&item_id) {
                if !toggle {
                    return;
                }
                // User clicked an expanded folder -> collapse it
                self.state.expanded_items.remove(&item_id);
            } else {
                // User clicked a collapsed folder -> expand it and load data
                self.state.expanded_items.insert(item_id.clone());
                self.server_state.update(cx, |state, cx| {
                    state.scan_prefix(format!("{}:", item_id.as_str()).into(), cx);
                });
            }
            self.update_key_tree(true, cx);
        } else {
            let is_selected = self.server_state.read(cx).key().as_ref() == Some(&item_id);
            // Select Key
            if !is_selected {
                self.server_state.update(cx, |state, cx| {
                    state.select_key(item_id.clone(), cx);
                });
            }
        }
    }

    /// Render the tree view or empty state message
    ///
    /// Displays:
    /// - Tree structure with keys and folders (normal state)
    /// - "Key not exists" message (Exact mode with expired key)
    /// - Error or "no keys found" message (empty state)
    fn render_tree(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(status_view) = self.get_tree_status_view(cx) {
            return status_view.into_any_element();
        }

        div()
            .p_1()
            .bg(cx.theme().sidebar)
            .text_color(cx.theme().sidebar_foreground)
            .h_full()
            .child(List::new(&self.key_tree_list_state))
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
        let server_state_clone = self.server_state.clone();
        let server_state = self.server_state.read(cx);
        let scanning = server_state.scanning();
        let readonly = server_state.readonly();
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
            .dropdown_menu_with_anchor(Corner::TopLeft, move |menu, _window, cx| {
                let mut menu = menu.label(i18n_key_tree(cx, "search_history"));
                let keywords = server_state_clone.read(cx).search_history();
                let no_keywords = keywords.is_empty();
                for keyword in keywords {
                    menu = menu.menu_element(Box::new(KeyTreeAction::Search(keyword.clone())), move |_, _cx| {
                        Label::new(keyword.clone())
                    });
                }
                if !no_keywords {
                    menu = menu.menu_element_with_icon(
                        CustomIconName::Eraser,
                        Box::new(KeyTreeAction::Clear),
                        move |_, cx| Label::new(i18n_key_tree(cx, "clear_history")),
                    );
                }
                menu.separator()
                    .label(i18n_key_tree(cx, "query_mode"))
                    .menu_element_with_check(query_mode == QueryMode::All, Box::new(QueryMode::All), |_, cx| {
                        Label::new(i18n_key_tree(cx, "query_mode_all")).ml_2().text_xs()
                    })
                    .menu_element_with_check(query_mode == QueryMode::Prefix, Box::new(QueryMode::Prefix), |_, cx| {
                        Label::new(i18n_key_tree(cx, "query_mode_prefix")).ml_2().text_xs()
                    })
                    .menu_element_with_check(query_mode == QueryMode::Exact, Box::new(QueryMode::Exact), |_, cx| {
                        Label::new(i18n_key_tree(cx, "query_mode_exact")).ml_2().text_xs()
                    })
            });
        let search_btn = Button::new("key-tree-search-btn")
            .ghost()
            .loading(scanning)
            .disabled(scanning)
            .icon(IconName::Search)
            .on_click(cx.listener(|this, _, _, cx| {
                this.handle_filter(cx);
            }));
        // keyword input
        let keyword_input = Input::new(&self.keyword_state)
            .w_full()
            .flex_1()
            .px_0()
            .mr_2()
            .prefix(query_mode_dropdown)
            .suffix(search_btn)
            .cleanable(true);
        let enabled_multiple_selection = self.key_tree_list_state.read(cx).delegate().enabled_multiple_selection;
        let refresh_interval_sec = self.state.refresh_interval_sec;

        let more_dropdown = Button::new("key-tree-more-dropdown")
            .outline()
            .icon(Icon::new(IconName::Ellipsis))
            .dropdown_menu_with_anchor(Corner::TopRight, move |menu, window, cx| {
                menu.menu_element_with_icon(
                    Icon::new(CustomIconName::ListChecvronsDownUp),
                    Box::new(KeyTreeAction::CollapseAllKeys),
                    move |_, cx| Label::new(i18n_key_tree(cx, "collapse_keys")),
                )
                .when(!readonly, |this| {
                    let icon = if enabled_multiple_selection {
                        Icon::new(IconName::Check)
                    } else {
                        Icon::new(CustomIconName::ListCheck)
                    };
                    this.menu_element_with_icon(icon, Box::new(KeyTreeAction::ToggleMultiSelectMode), move |_, cx| {
                        Label::new(i18n_key_tree(cx, "toggle_multi_select_mode"))
                    })
                })
                .submenu_with_icon(
                    Some(Icon::new(CustomIconName::RotateCw)),
                    i18n_key_tree(cx, "auto_refresh"),
                    window,
                    cx,
                    move |submenu, _window, cx| {
                        let mut submenu = submenu;
                        for interval in [0, 1, 5, 10, 30, 60, 120] {
                            let label = if interval == 0 {
                                i18n_key_tree(cx, "disable_auto_refresh")
                            } else {
                                format!("{}s", interval).into()
                            };
                            submenu = submenu.menu_element_with_check(
                                refresh_interval_sec == interval,
                                Box::new(KeyTreeAction::AutoRefresh(interval)),
                                move |_, _cx| Label::new(label.clone()),
                            )
                        }

                        submenu
                    },
                )
            });

        h_flex()
            .p_2()
            .gap_2()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(keyword_input)
            .child(
                Button::new("key-tree-add-btn")
                    .disabled(readonly)
                    .when(readonly, |this| this.tooltip(i18n_common(cx, "disable_in_readonly")))
                    .when(!readonly, |this| {
                        let tooltip = format!(
                            "{} ({})",
                            i18n_key_tree(cx, "add_key_tooltip"),
                            humanize_keystroke("cmd-n")
                        );
                        this.tooltip(tooltip)
                    })
                    .outline()
                    .icon(CustomIconName::FilePlusCorner)
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.handle_add_key(window, cx);
                    })),
            )
            .child(more_dropdown)
    }
}

impl Render for ZedisKeyTree {
    /// Main render method - displays search bar and tree structure
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(scroll_to_index) = self.state.scroll_to_index.take() {
            self.key_tree_list_state.update(cx, |state, cx| {
                state.scroll_to_item(scroll_to_index, ScrollStrategy::Top, window, cx);
            });
        }
        if let Some(true) = self.should_enter_add_key_mode.take() {
            self.handle_add_key(window, cx);
        }
        v_flex()
            .id("key-tree-container")
            .track_focus(&self.focus_handle)
            .h_full()
            .w_full()
            .child(self.render_keyword_input(window, cx))
            .child(self.render_tree(cx))
            .on_action(cx.listener(|this, e: &QueryMode, _window, cx| {
                let new_mode = *e;

                let server_id = this.server_state.read(cx).server_id();
                if let Ok(mut option) = get_session_option(server_id) {
                    option.query_mode = Some(new_mode.to_string());
                    save_session_option(server_id, option, cx);
                }

                // Step 1: Update server state with new query mode
                this.server_state.update(cx, |state, cx| {
                    state.set_query_mode(new_mode, cx);
                });

                // Step 2: Update local UI state
                this.state.query_mode = new_mode;
            }))
            .on_action(cx.listener(|this, e: &KeyTreeAction, window, cx| match e {
                KeyTreeAction::AutoRefresh(interval) => {
                    this.state.refresh_interval_sec = *interval;
                    this.start_auto_refresh(cx);
                    let server_id = this.server_state.read(cx).server_id();
                    if let Ok(mut option) = get_session_option(server_id) {
                        option.refresh_interval_sec = Some(*interval);
                        save_session_option(server_id, option, cx);
                    }
                }
                KeyTreeAction::CollapseAllKeys => {
                    this.server_state.update(cx, |state, cx| {
                        state.collapse_all_keys(cx);
                    });
                }
                KeyTreeAction::ToggleMultiSelectMode => {
                    this.key_tree_list_state.update(cx, |state, cx| {
                        state.delegate_mut().toggle_multiple_selection(cx);
                    });
                }
                KeyTreeAction::Search(keyword) => {
                    this.keyword_state.update(cx, |state, cx| {
                        state.set_value(keyword, window, cx);
                    });
                    this.handle_filter(cx);
                }
                KeyTreeAction::Clear => {
                    this.handle_clear_history(cx);
                }
                KeyTreeAction::DeleteMultipleKeys => {
                    let keys = this.key_tree_list_state.update(cx, |state, _cx| {
                        state
                            .delegate()
                            .selected_items
                            .iter()
                            .cloned()
                            .collect::<Vec<SharedString>>()
                    });
                    let server_state = this.server_state.clone();
                    window.open_dialog(cx, move |dialog, _, cx| {
                        let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
                        let text =
                            t!("key_tree.delete_keys_prompt", keys = keys.join(", "), locale = locale).to_string();
                        let server_state = server_state.clone();
                        let keys = keys.clone();
                        dialog
                            .confirm()
                            .button_props(dialog_button_props(cx))
                            .title(i18n_key_tree(cx, "delete_keys_title"))
                            .child(text)
                            .on_ok(move |_, _, cx| {
                                server_state.update(cx, |state, cx| {
                                    state.unlink_key(keys.clone(), cx);
                                });
                                true
                            })
                    });
                }
                KeyTreeAction::DeleteKey(id) => {
                    let id = id.clone();
                    let server_state = this.server_state.clone();
                    window.open_dialog(cx, move |dialog, _, cx| {
                        let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
                        let text = t!("key_tree.delete_key_prompt", key = id.clone(), locale = locale).to_string();
                        let id = id.clone();
                        let server_state = server_state.clone();
                        dialog
                            .confirm()
                            .button_props(dialog_button_props(cx))
                            .title(i18n_key_tree(cx, "delete_key_title"))
                            .child(text)
                            .on_ok(move |_, _, cx| {
                                server_state.update(cx, |state, cx| {
                                    state.delete_key(id.clone(), cx);
                                });
                                true
                            })
                    });
                }
                KeyTreeAction::DeleteFolder(id) => {
                    let id = id.clone();
                    let server_state = this.server_state.clone();
                    window.open_dialog(cx, move |dialog, _, cx| {
                        let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
                        let text =
                            t!("key_tree.delete_folder_prompt", folder = id.clone(), locale = locale).to_string();
                        let id = id.clone();
                        let server_state = server_state.clone();
                        dialog
                            .confirm()
                            .button_props(dialog_button_props(cx))
                            .title(i18n_key_tree(cx, "delete_folder_title"))
                            .child(text)
                            .on_ok(move |_, _, cx| {
                                server_state.update(cx, |state, cx| {
                                    state.delete_folder(id.clone(), cx);
                                });
                                true
                            })
                    });
                }
            }))
            .on_action(cx.listener(|this, event: &EditorAction, window, cx| match event {
                EditorAction::Search => {
                    this.keyword_state.focus_handle(cx).focus(window);
                }
                _ => {
                    cx.propagate();
                }
            }))
    }
}
