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
use crate::helpers::fast_contains_ignore_case;
use crate::states::ZedisGlobalStore;
use crate::states::i18n_common;
use crate::states::i18n_list_editor;
use crate::states::{RedisListValue, ZedisServerState};
use gpui::App;
use gpui::Entity;
use gpui::Hsla;
use gpui::SharedString;
use gpui::Subscription;
use gpui::TextAlign;
use gpui::Window;
use gpui::div;
use gpui::prelude::*;
use gpui::px;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::form::field;
use gpui_component::form::v_form;
use gpui_component::input::Input;
use gpui_component::input::InputEvent;
use gpui_component::input::InputState;
use gpui_component::label::Label;
use gpui_component::list::{List, ListDelegate, ListItem, ListState};
use gpui_component::radio::RadioGroup;
use gpui_component::v_flex;
use gpui_component::{ActiveTheme, Sizable};
use gpui_component::{Disableable, IndexPath};
use gpui_component::{Icon, IconName};
use gpui_component::{WindowExt, h_flex};
use rust_i18n::t;
use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;
use tracing::info;

// UI layout constants
const INDEX_WIDTH: f32 = 50.0; // Width for item index column
const ACTION_WIDTH: f32 = 80.0; // Width for action buttons column
const INDEX_WIDTH_WITH_PADDING: f32 = INDEX_WIDTH + 10.0;
const ACTION_WIDTH_WITH_PADDING: f32 = ACTION_WIDTH + 10.0;
const KEYWORD_INPUT_WIDTH: f32 = 200.0;

// Visual styling constants
const STRIPE_BACKGROUND_ALPHA_DARK: f32 = 0.1; // Odd row background alpha for dark theme
const STRIPE_BACKGROUND_ALPHA_LIGHT: f32 = 0.03; // Odd row background alpha for light theme

/// Delegate responsible for rendering Redis List items with editing capabilities
///
/// This delegate manages the display and interaction for list items, including:
/// - Rendering each item with index, value, and action buttons
/// - Handling inline editing of values
/// - Managing selection and update states
/// - Loading more items on scroll (infinite scroll)
#[derive(Debug)]
struct RedisListValues {
    /// Reference to parent editor view for event callbacks
    view: Entity<ZedisListEditor>,

    /// Current list data from Redis (loaded items and metadata)
    list_value: Arc<RedisListValue>,

    visible_items: Vec<SharedString>,
    visible_item_indexes: Option<Vec<usize>>,

    /// Reference to server state for Redis operations
    server_state: Entity<ZedisServerState>,

    /// Currently selected item index (if any)
    selected_index: Option<IndexPath>,

    /// Input field state for inline editing
    value_state: Entity<InputState>,

    /// Index of item currently being edited (shows input field)
    updated_index: Option<usize>,

    /// Original value before editing (for update operation)
    default_value: SharedString,

    /// Keyword for filtering items
    keyword: Option<SharedString>,
}
impl RedisListValues {
    /// Get the current item counts (loaded vs total)
    ///
    /// Returns a tuple of (items currently loaded, total items in Redis list)
    pub fn get_counts(&self) -> (usize, usize) {
        (self.list_value.values.len(), self.list_value.size)
    }

    /// Check if the server is currently busy with an operation
    ///
    /// Returns true if loading or updating data, false otherwise.
    /// Used to disable UI actions during operations.
    fn loading(&self, cx: &App) -> bool {
        self.server_state.read(cx).value().is_some_and(|v| v.is_busy())
    }

    /// Recalculate the visible items based on the keyword
    ///
    /// If the keyword is None, all items are visible.
    /// Otherwise, only items that contain the keyword are visible.
    fn recalc_visible_items(&mut self) {
        let keyword = self.keyword.clone().unwrap_or_default().to_lowercase();
        if keyword.is_empty() {
            self.visible_items = self.list_value.values.clone();
            self.visible_item_indexes = None;
            return;
        };

        let mut visible_item_indexes = Vec::with_capacity(10);
        let mut visible_items = Vec::with_capacity(10);
        for (index, item) in self.list_value.values.iter().enumerate() {
            if fast_contains_ignore_case(item.as_str(), &keyword) {
                visible_item_indexes.push(index);
                visible_items.push(item.clone());
            }
        }

        self.visible_items = visible_items;
        self.visible_item_indexes = Some(visible_item_indexes);
    }
}
impl ListDelegate for RedisListValues {
    type Item = ListItem;

    /// Return the total number of items to display
    fn items_count(&self, _section: usize, _cx: &App) -> usize {
        self.visible_items.len()
    }

    /// Render a single list item with inline editing and action buttons
    ///
    /// Each item shows:
    /// - Index number (1-based)
    /// - Value (as text or editable input)
    /// - Update button (toggles edit mode / saves changes)
    /// - Delete button (confirms and removes item)
    fn render_item(
        &mut self,
        ix: IndexPath,
        _window: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) -> Option<Self::Item> {
        let is_busy = self.loading(cx);
        let even_bg = cx.theme().background;

        // Zebra striping for better readability
        let odd_bg = if cx.theme().is_dark() {
            Hsla::white().alpha(STRIPE_BACKGROUND_ALPHA_DARK)
        } else {
            Hsla::black().alpha(STRIPE_BACKGROUND_ALPHA_LIGHT)
        };

        let row = ix.row;
        self.visible_items.get(row).map(|item| {
            let real_index = self
                .visible_item_indexes
                .as_ref()
                .map(|indexes| indexes.get(row).copied().unwrap_or(row))
                .unwrap_or(row);

            let show_index = row + 1; // Display as 1-based index
            let bg = if show_index.is_multiple_of(2) { even_bg } else { odd_bg };

            // Check if this item is currently being edited
            let is_updated = self.updated_index == Some(real_index);

            // Render either input field (edit mode) or label (display mode)
            let content = if is_updated {
                div()
                    .mx_2()
                    .child(Input::new(&self.value_state).small())
                    .flex_1()
                    .into_any_element()
            } else {
                Label::new(item).pl_4().text_sm().flex_1().into_any_element()
            };

            let update_view = self.view.clone();
            let delete_view = self.view.clone();
            let default_value = item.clone();
            let remove_value = item.clone();

            // Update button: Toggles between edit mode (pen icon) and save mode (check icon)
            let update_btn = Button::new(("zedis-editor-list-action-update-btn", show_index))
                .small()
                .ghost()
                .mr_2()
                .tooltip(i18n_list_editor(cx, "update_tooltip"))
                .when(!is_updated, |this| this.icon(Icon::new(CustomIconName::FilePenLine)))
                .when(is_updated, |this| this.icon(Icon::new(IconName::Check)))
                .disabled(is_busy)
                .on_click(move |_event, _window, cx| {
                    cx.stop_propagation();
                    update_view.clone().update(cx, |this, cx| {
                        if is_updated {
                            // Save the edited value
                            this.handle_update_value(real_index, cx);
                        } else {
                            // Enter edit mode
                            this.handle_update_index(default_value.clone(), real_index, cx);
                        }
                    });
                });

            // Delete button: Shows confirmation dialog before removing item
            let delete_btn = Button::new(("zedis-editor-list-action-delete-btn", show_index))
                .small()
                .ghost()
                .tooltip(i18n_list_editor(cx, "delete_tooltip"))
                .icon(Icon::new(CustomIconName::FileXCorner))
                .disabled(is_busy)
                .on_click(move |_event, window, cx| {
                    cx.stop_propagation();
                    delete_view.update(cx, |this, cx| {
                        this.handle_delete_item(real_index, show_index, remove_value.clone(), window, cx);
                    });
                });

            ListItem::new(("zedis-editor-list-item", show_index))
                .gap(px(0.))
                .bg(bg)
                .child(
                    h_flex()
                        .px_2()
                        .py_1()
                        .child(
                            Label::new(show_index.to_string())
                                .text_align(TextAlign::Right)
                                .text_sm()
                                .w(px(INDEX_WIDTH)),
                        )
                        .child(content)
                        .child(h_flex().w(px(ACTION_WIDTH)).child(update_btn).child(delete_btn)),
                )
        })
    }
    /// Update the selected item index
    fn set_selected_index(&mut self, ix: Option<IndexPath>, _window: &mut Window, cx: &mut Context<ListState<Self>>) {
        self.selected_index = ix;
        cx.notify();
    }

    /// Load more items from Redis when user scrolls to bottom (infinite scroll)
    ///
    /// Checks conditions before loading:
    /// - Not already loading
    /// - Not all items loaded yet
    /// - Current count less than total count
    fn load_more(&mut self, _window: &mut Window, cx: &mut Context<ListState<Self>>) {
        // Skip if already loading
        if self.loading(cx) {
            return;
        }
        // Check if we've loaded everything
        if self.list_value.values.len() >= self.list_value.size {
            return;
        }

        // Trigger loading next batch
        self.server_state.update(cx, |this, cx| {
            this.load_more_list_value(cx);
        });
    }
}

/// Redis List editor component with inline editing capabilities
///
/// Features:
/// - Display list items with index, value, and actions
/// - Inline editing of individual items
/// - Delete items with confirmation
/// - Infinite scroll to load more items
/// - Search/filter functionality (planned)
/// - Shows loaded count vs total count
pub struct ZedisListEditor {
    /// List component state with custom delegate
    list_state: Entity<ListState<RedisListValues>>,

    /// Reference to server state for Redis operations
    server_state: Entity<ZedisServerState>,

    /// Input field state for inline value editing
    value_state: Entity<InputState>,

    /// Input field state for new value input
    new_value_state: Entity<InputState>,

    new_value_mode: Option<usize>,

    /// Input field state for keyword search/filter
    keyword_state: Entity<InputState>,

    /// Temporary storage for default value when entering edit mode
    input_default_value: Option<SharedString>,

    /// Event subscriptions for reactive updates
    _subscriptions: Vec<Subscription>,
}

impl ZedisListEditor {
    /// Create a new list editor with event subscriptions
    ///
    /// Sets up reactive updates when server state changes and
    /// initializes input fields for inline editing.
    pub fn new(server_state: Entity<ZedisServerState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut subscriptions = Vec::new();

        // Subscribe to server state changes to update list when data changes
        subscriptions.push(cx.observe(&server_state, |this, _model, cx| {
            this.update_list_values(cx);
        }));

        // Initialize value input field for inline editing
        let value_state = cx.new(|cx| {
            InputState::new(window, cx)
                .clean_on_escape()
                .placeholder(i18n_list_editor(cx, "value_placeholder"))
        });

        let new_value_state = cx.new(|cx| {
            InputState::new(window, cx)
                .clean_on_escape()
                .placeholder(i18n_list_editor(cx, "value_placeholder"))
        });

        // Initialize keyword search input field
        let keyword_state = cx.new(|cx| {
            InputState::new(window, cx)
                .clean_on_escape()
                .placeholder(i18n_list_editor(cx, "keyword_placeholder"))
        });
        subscriptions.push(cx.subscribe(&keyword_state, |this, model, event, cx| {
            if let InputEvent::Change = &event {
                this.update_keyword(model.read(cx).value(), cx);
            }
        }));

        let view = cx.entity();

        // Subscribe to value input events (Blur cancels edit, Enter saves)
        subscriptions.push(
            cx.subscribe_in(&value_state, window, |view, _, event, _, cx| match &event {
                InputEvent::Blur => {
                    view.handle_blur(cx);
                }
                InputEvent::PressEnter { .. } => {
                    if let Some(updated_index) = view.list_state.read(cx).delegate().updated_index {
                        view.handle_update_value(updated_index, cx);
                    }
                }
                _ => {}
            }),
        );

        // Create list delegate with initial data
        let mut deletage = RedisListValues {
            view,
            server_state: server_state.clone(),
            list_value: Default::default(),
            selected_index: Default::default(),
            value_state: value_state.clone(),
            default_value: Default::default(),
            keyword: Default::default(),
            visible_items: Default::default(),
            visible_item_indexes: Default::default(),
            updated_index: None,
        };

        // Load initial data if available
        if let Some(data) = server_state.read(cx).value().and_then(|v| v.list_value()) {
            deletage.list_value = data.clone();
            deletage.recalc_visible_items();
        };

        let list_state = cx.new(|cx| ListState::new(deletage, window, cx));

        info!("Creating new list editor view");

        Self {
            server_state,
            list_state,
            value_state,
            keyword_state,
            input_default_value: None,
            new_value_mode: Some(0),
            new_value_state,
            _subscriptions: subscriptions,
        }
    }

    /// Handle input blur event - cancel edit mode
    fn handle_blur(&mut self, cx: &mut Context<Self>) {
        self.list_state.update(cx, |this, cx| {
            this.delegate_mut().updated_index = None;
            cx.notify();
        });
    }

    /// Update list values when server state changes
    fn update_list_values(&mut self, cx: &mut Context<Self>) {
        let server_state = self.server_state.read(cx);
        let Some(data) = server_state.value().and_then(|v| v.list_value()) else {
            return;
        };

        let items = data.clone();
        self.list_state.update(cx, |this, cx| {
            let delegete = this.delegate_mut();
            delegete.list_value = items;
            delegete.recalc_visible_items();
            cx.notify();
        });
    }
    fn update_keyword(&mut self, keyword: SharedString, cx: &mut Context<Self>) {
        self.list_state.update(cx, |this, cx| {
            let delegete = this.delegate_mut();
            delegete.keyword = Some(keyword);
            delegete.recalc_visible_items();
            cx.notify();
        });
    }

    /// Enter edit mode for a specific item
    ///
    /// Stores the original value and switches the item to input field display
    fn handle_update_index(&mut self, value: SharedString, index: usize, cx: &mut Context<Self>) {
        self.input_default_value = Some(value.clone());
        self.list_state.update(cx, |this, _cx| {
            let delegate = this.delegate_mut();
            delegate.default_value = value;
            delegate.updated_index = Some(index);
        });
    }
    /// Handle delete item action with confirmation dialog
    ///
    /// Shows a confirmation dialog before deleting the item from Redis
    fn handle_delete_item(
        &mut self,
        index: usize,
        row: usize,
        value: SharedString,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let server_state = self.server_state.clone();
        window.open_dialog(cx, move |dialog, _, cx| {
            let locale = cx.global::<ZedisGlobalStore>().locale(cx);
            let message = t!(
                "list_editor.delete_list_item_prompt",
                row = row,
                value = value,
                locale = locale
            )
            .to_string();
            let server_state = server_state.clone();

            dialog.confirm().child(message).on_ok(move |_, window, cx| {
                server_state.update(cx, |this, cx| {
                    this.delete_list_item(index, cx);
                });
                window.close_dialog(cx);
                true
            })
        });
    }

    /// Save the edited value to Redis
    ///
    /// Exits edit mode and sends the updated value to server
    fn handle_update_value(&mut self, index: usize, cx: &mut Context<Self>) {
        let original_value = self.list_state.read(cx).delegate().default_value.clone();

        // Exit edit mode
        self.list_state.update(cx, |this, _cx| {
            this.delegate_mut().updated_index = None;
        });

        // Get new value and trigger update
        let value = self.value_state.read(cx).value();
        self.server_state.update(cx, |this, cx| {
            this.update_list_value(index, original_value, value, cx);
        });
    }
    /// Handle push value action
    ///
    /// Pushes the value to the list
    fn handle_push_value(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let mode = self.new_value_mode.unwrap_or_default();
        let value = self.new_value_state.read(cx).value();
        self.server_state.update(cx, |this, cx| {
            this.push_list_value(value, mode, cx);
        });
    }
}

impl Render for ZedisListEditor {
    /// Main render method - displays header, list items, and footer
    ///
    /// Layout:
    /// - Header: Column labels (Index, Value, Action)
    /// - Body: Scrollable list of items with infinite scroll
    /// - Footer: Search input and item count indicator
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let value_label = i18n_list_editor(cx, "value");
        let action_label = i18n_list_editor(cx, "action");
        let list_state = self.list_state.read(cx).delegate();
        let (items_count, total_count) = list_state.get_counts();
        let text_color = cx.theme().muted_foreground;

        // Set focus to input field when entering edit mode
        if let Some(value) = self.input_default_value.take() {
            self.value_state.update(cx, |this, cx| {
                this.set_value(value, window, cx);
                this.focus(window, cx);
            });
        }

        let handle_add_value = cx.listener(move |this, _event, window, cx| {
            this.new_value_state.update(cx, |this, cx| {
                this.set_value(SharedString::default(), window, cx);
            });
            let new_value_state = this.new_value_state.clone();
            let view = cx.entity();
            let view_update = view.clone();
            let handle_submit = Rc::new(move |window: &mut Window, cx: &mut App| {
                view_update.update(cx, |this, cx| {
                    this.handle_push_value(window, cx);
                });
                window.close_dialog(cx);
                true
            });
            let focus_handle_done = Cell::new(false);

            window.open_dialog(cx, move |dialog, window, cx| {
                let new_value_mode = view.read(cx).new_value_mode;
                dialog
                    .title(i18n_list_editor(cx, "add_value_title"))
                    .overlay(true)
                    .child({
                        if !focus_handle_done.get() {
                            new_value_state.clone().update(cx, |this, cx| {
                                this.focus(window, cx);
                            });
                            focus_handle_done.set(true);
                        }
                        v_form()
                            .child(
                                field().label(i18n_list_editor(cx, "positon")).child(
                                    RadioGroup::horizontal("add-value-positon-group")
                                        .children(["RPUSH", "LPUSH"])
                                        .selected_index(new_value_mode)
                                        .on_click({
                                            let view = view.clone();
                                            move |index, _, cx| {
                                                view.update(cx, |this, cx| {
                                                    this.new_value_mode = Some(*index);
                                                    cx.notify();
                                                });
                                                cx.stop_propagation();
                                            }
                                        }),
                                ),
                            )
                            .child(
                                field()
                                    .label(i18n_list_editor(cx, "value"))
                                    .child(Input::new(&new_value_state)),
                            )
                    })
                    .on_ok({
                        let handle = handle_submit.clone();
                        move |_, window, cx| handle(window, cx)
                    })
                    .footer({
                        let handle = handle_submit.clone();
                        move |_, _, _, cx| {
                            let confirm_label = i18n_common(cx, "confirm");
                            let cancel_label = i18n_common(cx, "cancel");
                            vec![
                                // Submit button - validates and saves server configuration
                                Button::new("ok").primary().label(confirm_label).on_click({
                                    let handle = handle.clone();
                                    move |_, window, cx| {
                                        handle.clone()(window, cx);
                                    }
                                }),
                                // Cancel button - closes dialog without saving
                                Button::new("cancel").label(cancel_label).on_click(|_, window, cx| {
                                    window.close_dialog(cx);
                                }),
                            ]
                        }
                    })
            });
        });

        v_flex()
            .h_full()
            .w_full()
            .child(
                // Header row with column labels
                h_flex()
                    .w_full()
                    .px_2()
                    .py_1()
                    .child(
                        Label::new("#")
                            .text_align(TextAlign::Right)
                            .text_sm()
                            .text_color(text_color)
                            .w(px(INDEX_WIDTH_WITH_PADDING)),
                    )
                    .child(Label::new(value_label).pl_4().text_sm().text_color(text_color).flex_1())
                    .child(
                        Label::new(action_label)
                            .text_sm()
                            .text_color(text_color)
                            .w(px(ACTION_WIDTH_WITH_PADDING)),
                    ),
            )
            .child(
                // Scrollable list body with infinite scroll
                List::new(&self.list_state).flex_1(),
            )
            .child(
                // Footer with search and count indicator
                h_flex()
                    .w_full()
                    .p_2()
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new("add-value-btn")
                                    .icon(CustomIconName::FilePlusCorner)
                                    .tooltip(i18n_list_editor(cx, "add_value_tooltip"))
                                    .on_click(handle_add_value),
                            )
                            .child(
                                Input::new(&self.keyword_state)
                                    .w(px(KEYWORD_INPUT_WIDTH))
                                    .cleanable(true),
                            )
                            .flex_1(),
                    )
                    .child(
                        Label::new(format!("{} / {}", items_count, total_count))
                            .text_sm()
                            .text_color(text_color),
                    ),
            )
    }
}
