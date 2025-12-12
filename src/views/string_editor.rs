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

use crate::helpers::get_font_family;
use crate::states::ServerEvent;
use crate::states::{RedisValue, ZedisServerState};
use gpui::AnyWindowHandle;
use gpui::Entity;
use gpui::SharedString;
use gpui::Subscription;
use gpui::Window;
use gpui::prelude::*;
use gpui::px;
use gpui_component::highlighter::Language;
use gpui_component::input::InputEvent;
use gpui_component::input::TabSize;
use gpui_component::input::{Input, InputState};
use pretty_hex::HexConfig;
use pretty_hex::config_hex;
use tracing::info;

// Constants for editor configuration
const DEFAULT_TAB_SIZE: usize = 4;
const DEFAULT_LANGUAGE: &str = "json";
const EDITOR_FONT_SIZE: f32 = 12.0;
const HEX_WIDTH_NARROW: usize = 16; // Bytes per line for narrow viewports
const HEX_WIDTH_WIDE: usize = 32; // Bytes per line for wide viewports
const VIEWPORT_BREAKPOINT: f32 = 1400.0; // Pixel width to switch hex display width

/// String value editor component for Redis String data type
///
/// Features:
/// - Code editor with syntax highlighting (JSON by default)
/// - Line numbers and indent guides
/// - Search functionality
/// - Soft wrap support
/// - Automatic hex display for binary data
/// - Tracks modification state
pub struct ZedisStringEditor {
    /// Reference to server state for Redis operations
    server_state: Entity<ZedisServerState>,

    /// Flag indicating if the value has been modified from original
    value_modified: bool,

    /// Code editor state with input handling
    editor: Entity<InputState>,

    /// Window handle for cross-window updates
    window_handle: AnyWindowHandle,

    /// Whether to soft wrap the editor
    soft_wrap: bool,

    /// Whether the soft wrap has been changed
    soft_wrap_changed: bool,

    /// Event subscriptions for reactive updates
    _subscriptions: Vec<Subscription>,
}

/// Extract string value from Redis value, with hex fallback for binary data
///
/// If the value is a string, returns it directly.
/// If the value is binary data, formats it as a hex dump with appropriate width
/// based on viewport size.
///
/// # Arguments
/// * `window` - Window reference for viewport size calculation
/// * `value` - Optional Redis value to extract string from
///
/// # Returns
/// String representation (either original string or hex dump)
fn get_string_value(window: &Window, value: Option<&RedisValue>) -> SharedString {
    let Some(value) = value else {
        return String::new().into();
    };

    let mut string_value = value.string_value().unwrap_or_default();

    // If string is empty but we have binary data, display as hex
    if string_value.is_empty()
        && let Some(data) = value.bytes_value()
    {
        // Adjust hex width based on viewport size
        let width = window.viewport_size().width;
        let hex_width = match width {
            width if width < px(VIEWPORT_BREAKPOINT) => HEX_WIDTH_NARROW,
            _ => HEX_WIDTH_WIDE,
        };

        // Configure hex dump format
        let cfg = HexConfig {
            title: false,
            width: hex_width,
            group: 0,
            ..Default::default()
        };
        string_value = config_hex(&data, cfg).into()
    }

    string_value
}

impl ZedisStringEditor {
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
                ServerEvent::ValueLoaded(_) => {
                    this.update_editor_value(cx);
                }
                ServerEvent::SoftWrapToggled(soft_wrap) => {
                    this.soft_wrap_changed = true;
                    this.soft_wrap = *soft_wrap;
                }
                _ => {}
            }),
        );

        // Get initial value (string or hex dump)
        let value = get_string_value(window, server_state.read(cx).value());
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
                .default_value(value)
        });

        // Subscribe to editor changes to track modification state
        subscriptions.push(cx.subscribe(&editor, |this, _, event, cx| {
            if let InputEvent::Change = &event {
                let value = this.editor.read(cx).value();
                let redis_value = this.server_state.read(cx).value();

                // Compare with original value to determine if modified
                let original = redis_value.and_then(|r| r.string_value()).map_or("".into(), |v| v);

                this.value_modified = original != value.as_str();
                cx.notify();
            }
        }));

        info!("Creating new string editor view");

        Self {
            value_modified: false,
            soft_wrap,
            soft_wrap_changed: false,
            editor,
            window_handle: window.window_handle(),
            server_state,
            _subscriptions: subscriptions,
        }
    }

    /// Update editor value when server state changes
    ///
    /// Skips update if value is currently loading to prevent flickering.
    /// Resets the modification flag after updating to the new value.
    fn update_editor_value(&mut self, cx: &mut Context<Self>) {
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

        let window_handle = self.window_handle;
        let server_state = self.server_state.clone();

        // Reset modification flag since we're loading a new value
        self.value_modified = false;

        // Update editor with new value (requires window handle for hex width calculation)
        let _ = window_handle.update(cx, move |_, window, cx| {
            self.editor.update(cx, move |this, cx| {
                let value = server_state.read(cx).value();
                this.set_value(get_string_value(window, value), window, cx);
                cx.notify();
            });
        });
    }

    /// Check if the current editor value differs from the original Redis value
    pub fn is_value_modified(&self) -> bool {
        self.value_modified
    }

    /// Get the current editor value
    pub fn value(&self, cx: &mut Context<Self>) -> SharedString {
        self.editor.read(cx).value()
    }
}

impl Render for ZedisStringEditor {
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
        Input::new(&self.editor)
            .flex_1()
            .bordered(false)
            .p_0()
            .w_full()
            .h_full()
            .font_family(get_font_family())
            .text_size(px(EDITOR_FONT_SIZE))
            .focus_bordered(false)
            .into_any_element()
    }
}
