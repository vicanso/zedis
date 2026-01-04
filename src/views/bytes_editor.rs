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

use crate::helpers::get_font_family;
use crate::states::{DataFormat, RedisBytesValue, ServerEvent, ViewMode, ZedisGlobalStore, ZedisServerState};
use gpui::{App, Entity, Image, ObjectFit, SharedString, Subscription, Window, img, px};
use gpui::{div, hsla, prelude::*};
use gpui_component::highlighter::Language;
use gpui_component::input::{Input, InputEvent, InputState, TabSize};
use gpui_component::label::Label;
use gpui_component::list::{List, ListDelegate, ListItem, ListState};
use gpui_component::{ActiveTheme, IndexPath, h_flex};
use pretty_hex::HexConfig;
use pretty_hex::config_hex;
use std::sync::Arc;
use tracing::info;

// Constants for editor configuration
const DEFAULT_TAB_SIZE: usize = 2;
const DEFAULT_LANGUAGE: &str = "json";
const HEX_WIDTH_NARROW: usize = 16; // Bytes per line for narrow viewports
const HEX_WIDTH_MEDIUM: usize = 24; // Bytes per line for medium viewports
const HEX_WIDTH_WIDE: usize = 32; // Bytes per line for wide viewports
const VIEWPORT_WIDE: f32 = 1400.0; // Pixel width to switch hex display width
const VIEWPORT_MEDIUM: f32 = 1000.0; // Pixel width to switch hex display width

/// String value editor component for Redis String data type
///
/// Features:
/// - Code editor with syntax highlighting (JSON by default)
/// - Line numbers and indent guides
/// - Search functionality
/// - Soft wrap support
/// - Automatic hex display for binary data
/// - Tracks modification state
pub struct ZedisBytesEditor {
    /// Reference to server state for Redis operations
    server_state: Entity<ZedisServerState>,

    /// Flag indicating if the value has been modified from original
    value_modified: bool,

    /// State for hex viewer list
    hex_viewer_state: Option<Entity<ListState<HexViewerListDelegate>>>,

    /// Code editor state with input handling
    editor: Entity<InputState>,

    /// Whether to soft wrap the editor
    soft_wrap: bool,

    /// Whether the editor is readonly
    readonly: bool,

    /// Whether to update the editor
    should_update_editor: bool,

    /// Whether the soft wrap has been changed
    soft_wrap_changed: bool,

    /// The data to display in the editor
    data: ByteEditorData,

    /// Event subscriptions for reactive updates
    _subscriptions: Vec<Subscription>,
}

enum ByteEditorData {
    Image(Arc<Image>),
    Text(SharedString),
    Hex(HexViewerListDelegate),
}

impl ByteEditorData {
    fn to_string(&self) -> Option<SharedString> {
        match self {
            ByteEditorData::Text(value) => Some(value.clone()),
            _ => None,
        }
    }
}
/// Extract string value from Redis value, with hex fallback for binary data
///
/// If the value is a string, returns Text(SharedString).
/// If the value is binary data, formats it as a hex dump with appropriate width
/// based on viewport size and returns Hex(SharedString).
///
/// # Arguments
/// * `value` - Optional Redis value to extract string from
/// * `cx` - App context for viewport size calculation
///
/// # Returns
/// String representation (either original string or hex dump)
fn format_byte_editor_data(value: &Arc<RedisBytesValue>, cx: &App) -> ByteEditorData {
    if value.bytes.is_empty() {
        return ByteEditorData::Text(SharedString::default());
    }

    let create_hex_view = || {
        let width = cx
            .global::<ZedisGlobalStore>()
            .read(cx)
            .content_width()
            .unwrap_or_default();

        let hex_width = match width {
            w if w < px(VIEWPORT_MEDIUM) => HEX_WIDTH_NARROW,
            w if w < px(VIEWPORT_WIDE) => HEX_WIDTH_MEDIUM,
            _ => HEX_WIDTH_WIDE,
        };

        let cfg = HexConfig {
            title: false,
            width: hex_width,
            group: 0,
            ..Default::default()
        };

        let hex_data = config_hex(&value.bytes, cfg);
        ByteEditorData::Hex(HexViewerListDelegate::new(&hex_data))
    };

    match value.view_mode {
        ViewMode::Hex => create_hex_view(),

        ViewMode::Plain => {
            let text = String::from_utf8_lossy(&value.bytes).to_string().into();
            ByteEditorData::Text(text)
        }

        _ => {
            if value.is_image() {
                let format = match value.format {
                    DataFormat::Png => gpui::ImageFormat::Png,
                    DataFormat::Webp => gpui::ImageFormat::Webp,
                    DataFormat::Gif => gpui::ImageFormat::Gif,
                    DataFormat::Svg => gpui::ImageFormat::Svg,
                    _ => gpui::ImageFormat::Jpeg,
                };
                let data = Image::from_bytes(format, value.bytes.to_vec());
                return ByteEditorData::Image(Arc::new(data));
            }

            if let Some(text) = &value.text {
                return ByteEditorData::Text(text.clone());
            }

            create_hex_view()
        }
    }
}
#[derive(Clone)]
struct HexViewerListDelegate {
    items: Vec<(SharedString, SharedString, SharedString)>,
    selected_index: Option<IndexPath>,
}

impl HexViewerListDelegate {
    fn new(data: &str) -> Self {
        let items = data
            .split("\n")
            .flat_map(|item| {
                let (address, value) = item.split_once(":")?;
                let (hex_data, ascii_data) = value.trim_start().split_once("   ")?;
                Some((
                    address.to_uppercase().into(),
                    hex_data.to_string().into(),
                    ascii_data.to_string().into(),
                ))
            })
            .collect::<Vec<_>>();
        Self {
            items,
            selected_index: None,
        }
    }
}

impl ListDelegate for HexViewerListDelegate {
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
        let address_color = if cx.theme().is_dark() {
            hsla(0.108, 0.66, 0.69, 1.0)
        } else {
            hsla(0.0892, 0.9462, 0.4373, 1.0)
        };
        self.items.get(ix.row).map(|(address, hex_data, ascii_data)| {
            ListItem::new(ix).py_0().px_2().child(
                h_flex()
                    .child(Label::new(address.clone()).text_color(address_color).mr_4())
                    .child(
                        Label::new(hex_data.clone())
                            .text_color(cx.theme().muted_foreground)
                            .mr_6(),
                    )
                    .child(Label::new(ascii_data.clone())),
            )
        })
    }

    fn set_selected_index(&mut self, ix: Option<IndexPath>, _window: &mut Window, _cx: &mut Context<ListState<Self>>) {
        self.selected_index = ix;
    }
}

impl ZedisBytesEditor {
    /// Create a new string editor with code editing capabilities
    ///
    /// Initializes a code editor with:
    /// - JSON syntax highlighting by default
    /// - Line numbers and indent guides
    /// - Search functionality
    /// - Soft wrap for long lines
    /// - Automatic value updates when server state changes
    pub fn new(server_state: Entity<ZedisServerState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut subscriptions = Vec::new();

        // Subscribe to server state changes to update editor when value changes
        subscriptions.push(
            cx.subscribe(&server_state, |this, _server_state, event, cx| match event {
                ServerEvent::ValueLoaded(_) | ServerEvent::ValueModeViewUpdated(_) => {
                    this.update_editor_data(cx);
                    this.should_update_editor = true;
                }
                ServerEvent::ValueUpdated(_) => {
                    this.update_editor_data(cx);
                }
                ServerEvent::SoftWrapToggled(soft_wrap) => {
                    this.soft_wrap_changed = true;
                    this.soft_wrap = *soft_wrap;
                }
                _ => {}
            }),
        );

        let soft_wrap = server_state.read(cx).soft_wrap();

        // Configure code editor with JSON syntax highlighting
        let default_language = Language::from_str(DEFAULT_LANGUAGE);
        let editor = cx.new(|cx| {
            InputState::new(window, cx)
                .code_editor(default_language.name())
                .line_number(true)
                .indent_guides(true)
                .tab_size(TabSize {
                    tab_size: DEFAULT_TAB_SIZE,
                    hard_tabs: false,
                })
                .searchable(true)
                .soft_wrap(soft_wrap)
        });

        editor.update(cx, |this, cx| {
            this.focus(window, cx);
        });

        // Subscribe to editor changes to track modification state
        subscriptions.push(cx.subscribe(&editor, |this, _, event, cx| {
            if let InputEvent::Change = &event {
                let value = this.editor.read(cx).value();

                // Compare with original value to determine if modified
                let original = this.data.to_string().unwrap_or_default();

                this.value_modified = original != value.as_str();
                cx.notify();
            }
        }));

        info!("Creating new string editor view");

        let mut this = Self {
            value_modified: false,
            soft_wrap,
            soft_wrap_changed: false,
            data: ByteEditorData::Text(SharedString::default()),
            hex_viewer_state: None,
            editor,
            should_update_editor: true,
            server_state,
            readonly: false,
            _subscriptions: subscriptions,
        };
        this.update_editor_data(cx);
        this
    }

    /// Update editor data when server state changes
    ///
    /// Skips update if value is currently loading to prevent flickering.
    /// Resets the modification flag after updating to the new value.
    fn update_editor_data(&mut self, cx: &mut Context<Self>) {
        // Prevent editor flickering by skipping value updates while loading
        if self
            .server_state
            .read(cx)
            .value()
            .map(|value| value.is_loading())
            .unwrap_or(false)
        {
            return;
        }

        let server_state = self.server_state.clone();

        // Reset modification flag since we're loading a new value
        self.value_modified = false;

        let redis_bytes_value = server_state.read(cx).value().and_then(|v| v.bytes_value());
        if let Some(redis_bytes_value) = &redis_bytes_value {
            self.readonly = !redis_bytes_value.is_utf8_text();
            self.data = format_byte_editor_data(redis_bytes_value, cx);
        } else {
            self.data = ByteEditorData::Text(SharedString::default());
        }

        if !matches!(self.data, ByteEditorData::Hex(_)) {
            self.hex_viewer_state = None;
        }
    }

    /// Check if the current editor value differs from the original Redis value
    pub fn is_value_modified(&self) -> bool {
        self.value_modified
    }

    /// Check if the editor is readonly
    pub fn is_readonly(&self) -> bool {
        self.readonly
    }

    /// Get the current editor value
    pub fn value(&self, cx: &mut Context<Self>) -> SharedString {
        self.editor.read(cx).value()
    }
}

impl Render for ZedisBytesEditor {
    /// Main render method - displays code editor with monospace font
    ///
    /// Renders a full-width, full-height code editor with:
    /// - No borders for seamless integration
    /// - Monospace font for code readability
    /// - Customizable font size
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.soft_wrap_changed {
            self.editor.update(cx, |this, cx| {
                this.set_soft_wrap(self.soft_wrap, window, cx);
            });
            self.soft_wrap_changed = false;
        }
        match &self.data {
            ByteEditorData::Image(value) => div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .overflow_hidden()
                .child(img(value.clone()).object_fit(ObjectFit::Contain).flex_shrink_0())
                .into_any_element(),
            ByteEditorData::Hex(value) => {
                let state = self
                    .hex_viewer_state
                    .get_or_insert_with(|| cx.new(|cx| ListState::new(value.clone(), window, cx)))
                    .clone();
                List::new(&state).font_family(get_font_family()).into_any_element()
            }
            _ => {
                if self.should_update_editor {
                    self.should_update_editor = false;
                    let value = self.data.to_string().unwrap_or_default();
                    self.editor.update(cx, move |this, cx| {
                        this.set_value(value, window, cx);
                    });
                }
                Input::new(&self.editor)
                    .flex_1()
                    .bordered(false)
                    .disabled(self.readonly)
                    .appearance(false)
                    .p_0()
                    .w_full()
                    .h_full()
                    .font_family(get_font_family())
                    .focus_bordered(false)
                    .into_any_element()
            }
        }
    }
}
