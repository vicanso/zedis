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

use crate::helpers::{
    JsonPathAction, JsonPathOutcome, bytes_to_hex_text, get_font_family, is_json_container, parse_hex_text,
    run_jsonpath,
};
use crate::states::{
    DataFormat, RedisBytesValue, ServerEvent, ViewMode, ZedisGlobalStore, ZedisServerState, i18n_editor,
};
use bytes::Bytes;
use gpui::{App, Entity, Image, ObjectFit, SharedString, Subscription, Window, img, px};
use gpui::{div, hsla, prelude::*};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::highlighter::Language;
use gpui_component::input::{CompletionProvider, Enter, Input, InputEvent, InputState, TabSize};
use gpui_component::label::Label;
use gpui_component::list::{List, ListDelegate, ListItem, ListState};
use gpui_component::{ActiveTheme, IconName, IndexPath, Sizable, h_flex, v_flex};
use pretty_hex::HexConfig;
use pretty_hex::config_hex;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use super::jsonpath_completion::{JsonDoc, JsonPathCompletionProvider};
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

    /// JSONPath query input (only relevant when `is_json_value` is true).
    jsonpath_input: Entity<InputState>,

    /// Lazily-parsed JSON document shared with the JSONPath completion
    /// provider. Fed (raw text only) by `update_editor_data`; the DOM
    /// is built on first completion and cached.
    jsonpath_doc: Rc<RefCell<JsonDoc>>,

    /// Last JSONPath outcome, populated after the user hits Enter / Run.
    /// `None` ⇒ no query has been issued yet on the current value.
    jsonpath_result: Option<JsonPathOutcome>,

    /// Whether the JSONPath query bar is expanded. Collapsed by default so
    /// the query UI doesn't permanently occupy a row above the value; a
    /// slim toggle in its place expands it on demand.
    jsonpath_open: bool,

    /// Read-only JSON viewer for the JSONPath result. Reused across queries
    /// so syntax highlighting / scroll position / search work like the main
    /// editor. Hidden until a query produces a non-trivial value.
    jsonpath_result_editor: Entity<InputState>,

    /// True when the current value is detected as parseable JSON. Controls
    /// whether the JSONPath query bar is rendered above the editor.
    is_json_value: bool,

    /// True when the editor is showing the value as an editable hex dump.
    /// Save path then decodes the text back to raw bytes (see
    /// `value_bytes_for_save`).
    is_hex_text: bool,

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
/// Pick a bytes-per-row that fits the current viewport. Narrow windows
/// wrap at 16 bytes (the `xxd` default), wider ones go up to 32 so the
/// user doesn't waste horizontal space. Shared by hex-dump rendering
/// and history-load injection so both use the same layout decision.
fn hex_bytes_per_row(cx: &App) -> usize {
    let width = cx
        .global::<ZedisGlobalStore>()
        .read(cx)
        .content_width()
        .unwrap_or_default();
    match width {
        w if w < px(VIEWPORT_MEDIUM) => HEX_WIDTH_NARROW,
        w if w < px(VIEWPORT_WIDE) => HEX_WIDTH_MEDIUM,
        _ => HEX_WIDTH_WIDE,
    }
}

fn format_byte_editor_data(value: &Arc<RedisBytesValue>, cx: &App) -> ByteEditorData {
    if value.bytes.is_empty() {
        return ByteEditorData::Text(value.text.clone().unwrap_or_default());
    }

    let create_hex_view = || {
        let cfg = HexConfig {
            title: false,
            width: hex_bytes_per_row(cx),
            group: 0,
            ..Default::default()
        };

        let hex_data = config_hex(&value.bytes, cfg);
        ByteEditorData::Hex(HexViewerListDelegate::new(&hex_data))
    };

    match value.view_mode {
        ViewMode::Hex => ByteEditorData::Text(bytes_to_hex_text(&value.bytes, hex_bytes_per_row(cx)).into()),

        ViewMode::Plain => {
            let text = SharedString::new(String::from_utf8_lossy(&value.bytes));
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
        subscriptions.push(cx.subscribe_in(
            &server_state,
            window,
            |this, _server_state, event, window, cx| match event {
                ServerEvent::ValueLoaded | ServerEvent::ValueModeViewUpdated => {
                    this.update_editor_data(cx);
                    this.should_update_editor = true;
                }
                ServerEvent::ValueUpdated => {
                    this.update_editor_data(cx);
                }
                ServerEvent::KeySelected(_) => {
                    // A different key invalidates the current JSONPath
                    // query — clear the input (and any stale result) so
                    // it doesn't carry over to an unrelated value.
                    this.jsonpath_result = None;
                    this.jsonpath_input.update(cx, |state, cx| {
                        state.set_value(SharedString::default(), window, cx);
                    });
                }
                ServerEvent::SoftWrapToggled(soft_wrap) => {
                    this.soft_wrap_changed = true;
                    this.soft_wrap = *soft_wrap;
                }
                _ => {}
            },
        ));

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

        let readonly = server_state.read(cx).readonly();
        info!("Creating new string editor view");

        // JSONPath query bar — single-line input that re-evaluates on Enter.
        // We keep it as a separate InputState so it doesn't compete with the
        // main editor's focus or undo stack.
        let jsonpath_doc: Rc<RefCell<JsonDoc>> = Rc::new(RefCell::new(JsonDoc::default()));
        // `clean_on_escape`: when the completion menu is closed, Esc
        // clears the query input. If the menu is open, `InputState`'s
        // escape handler closes the menu first and returns, so Esc
        // dismisses the menu before it ever clears the text.
        let jsonpath_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(i18n_editor(cx, "jsonpath_placeholder"))
                .clean_on_escape()
        });
        {
            // Context-aware key autocomplete. The provider shares the
            // lazily-parsed document with the editor via `jsonpath_doc`.
            let provider: Rc<dyn CompletionProvider> = Rc::new(JsonPathCompletionProvider::new(jsonpath_doc.clone()));
            jsonpath_input.update(cx, |state, _| {
                state.lsp.completion_provider = Some(provider);
            });
        }
        subscriptions.push(cx.subscribe_in(&jsonpath_input, window, |this, _, event, window, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                this.run_jsonpath_query(window, cx);
            }
        }));

        // Read-only JSON editor for displaying query results. Lazily filled
        // by `run_jsonpath_query`. Same code-editor configuration as the main
        // editor so the result is syntax-highlighted, scrollable, and searchable.
        // `auto_grow(2, 13)` lets the editor size to its content up to ~220px,
        // then internal scrolling takes over — short results don't waste space.
        let jsonpath_result_editor = cx.new(|cx| {
            InputState::new(window, cx)
                .code_editor(default_language.name())
                .line_number(true)
                .searchable(true)
                .soft_wrap(true)
                .auto_grow(2, 13)
        });

        let mut this = Self {
            value_modified: false,
            soft_wrap,
            soft_wrap_changed: false,
            data: ByteEditorData::Text(SharedString::default()),
            hex_viewer_state: None,
            editor,
            should_update_editor: true,
            server_state,
            readonly,
            jsonpath_input,
            jsonpath_doc,
            jsonpath_result: None,
            jsonpath_open: false,
            jsonpath_result_editor,
            is_json_value: false,
            is_hex_text: false,
            _subscriptions: subscriptions,
        };
        this.update_editor_data(cx);
        this
    }

    /// Evaluate the current JSONPath input against the current editor value.
    /// Results are stashed in `self.jsonpath_result` and re-rendered next
    /// frame. Empty paths reset the result instead of erroring.
    fn run_jsonpath_query(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let path = self.jsonpath_input.read(cx).value().to_string();
        if path.trim().is_empty() {
            self.jsonpath_result = None;
            cx.notify();
            return;
        }
        let raw = self.data.to_string().unwrap_or_default();
        let outcome = run_jsonpath(raw.as_str(), path.trim());
        // Push the matched payload into the read-only result editor when
        // there's something multi-line to scroll through. Status outcomes
        // (NoMatch / errors) render as a single label, not the editor.
        let editor_text = match &outcome {
            JsonPathOutcome::Single(s) | JsonPathOutcome::Multiple(s) => Some(SharedString::from(s.clone())),
            _ => None,
        };
        if let Some(text) = editor_text {
            self.jsonpath_result_editor.update(cx, |state, cx| {
                state.set_value(text, window, cx);
            });
        }
        self.jsonpath_result = Some(outcome);
        cx.notify();
    }

    /// Update editor data when server state changes
    ///
    /// Skips update if value is currently loading to prevent flickering.
    /// Resets the modification flag after updating to the new value.
    fn update_editor_data(&mut self, cx: &mut Context<Self>) {
        // Prevent editor flickering by skipping value updates while loading
        let server_state = self.server_state.read(cx);
        let value = server_state.value();
        if value.map(|value| value.is_loading()).unwrap_or(false) {
            return;
        }

        // Reset modification flag since we're loading a new value
        self.value_modified = false;
        let readonly = server_state.readonly();

        let redis_bytes_value = value.and_then(|v| v.bytes_value());

        // Hex text mode is determined by the *requested* view mode, not the
        // resulting `ByteEditorData` (which is `Text` in both Plain and Hex
        // modes after the recent refactor). Capture it before formatting.
        let hex_mode = redis_bytes_value
            .as_ref()
            .map(|v| matches!(v.view_mode, ViewMode::Hex))
            .unwrap_or(false);

        if let Some(redis_bytes_value) = &redis_bytes_value {
            // Hex mode is always editable — even for binary keys — because
            // we round-trip through hex text on save.
            self.readonly = readonly || (!hex_mode && !redis_bytes_value.is_utf8_text());
            self.data = format_byte_editor_data(redis_bytes_value, cx);
        } else {
            self.data = ByteEditorData::Text(SharedString::default());
        }

        if !matches!(self.data, ByteEditorData::Hex(_)) {
            self.hex_viewer_state = None;
        }

        self.is_hex_text = hex_mode;

        // JSON detection is meaningless on hex-encoded text — skip it.
        self.is_json_value = if hex_mode {
            false
        } else {
            match &redis_bytes_value {
                Some(v) if v.format == DataFormat::Json => true,
                _ => match &self.data {
                    ByteEditorData::Text(t) => is_json_container(t.as_ref()),
                    _ => false,
                },
            }
        };
        // Clear stale results when switching to a different value.
        self.jsonpath_result = None;

        // Feed the autocomplete provider: only the raw text, and only
        // when the value is JSON-shaped (or RedisJSON). Parsing the DOM
        // stays lazy inside the provider, so this keeps the
        // "no DOM at detection" cost profile.
        let json_text = if self.is_json_value {
            match &self.data {
                ByteEditorData::Text(t) => Some(t.clone()),
                _ => None,
            }
        } else {
            None
        };
        self.jsonpath_doc.borrow_mut().set_raw(json_text);
    }

    /// When the editor is in hex-text mode, decode the user's input back to
    /// raw bytes for the save path. Returns:
    ///   * `None` ⇒ not in hex mode, caller should use the text save path.
    ///   * `Some(Ok(bytes))` ⇒ parsed successfully, send via `update_value_bytes`.
    ///   * `Some(Err(msg))` ⇒ invalid hex, caller should surface the error.
    pub fn value_bytes_for_save(&self, cx: &mut Context<Self>) -> Option<Result<Vec<u8>, String>> {
        if !self.is_hex_text {
            return None;
        }
        let text = self.editor.read(cx).value();
        Some(parse_hex_text(text.as_ref()))
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

    /// Replace the editor's current text with `bytes`, rendered according to
    /// the active view mode. Used by the value-history rollback UI: the user
    /// picks an old version, we surface it in the editor, and the existing
    /// Save flow takes over from there.
    ///
    /// In hex mode we round-trip through `bytes_to_hex_text` so non-UTF8
    /// payloads stay losslessly editable. In plain/auto modes we use
    /// `String::from_utf8_lossy` — this is safe because the history button
    /// is only offered when the editor is writable, which already excludes
    /// the binary-and-not-hex case (see `update_editor_data`).
    ///
    /// Marks the editor as modified so the Save button enables itself.
    pub fn load_bytes_into_editor(&mut self, bytes: Bytes, window: &mut Window, cx: &mut Context<Self>) {
        if self.readonly {
            return;
        }
        let text: SharedString = if self.is_hex_text {
            bytes_to_hex_text(&bytes, hex_bytes_per_row(cx)).into()
        } else {
            String::from_utf8_lossy(&bytes).to_string().into()
        };
        self.editor.update(cx, |state, cx| {
            state.set_value(text, window, cx);
        });
        self.value_modified = true;
        cx.notify();
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
                let editor = Input::new(&self.editor)
                    .flex_1()
                    .bordered(false)
                    // 由于gpui-component调整之后，
                    // 设置disabled之后文字颜色模糊，因此暂时禁用
                    // TODO 后续如果有优化更调整
                    // .disabled(self.readonly)
                    .appearance(false)
                    .p_0()
                    .w_full()
                    .font_family(get_font_family())
                    .focus_bordered(false);
                if !self.is_json_value {
                    return editor.h_full().into_any_element();
                }
                v_flex()
                    .size_full()
                    // Collapsed by default: a slim toggle row instead of the
                    // full query bar, returning the vertical space to the
                    // value. Expanding shows the bar (and any prior result).
                    .child(if self.jsonpath_open {
                        self.render_jsonpath_bar(cx).into_any_element()
                    } else {
                        self.render_jsonpath_toggle(cx).into_any_element()
                    })
                    .when(self.jsonpath_open, |this| {
                        this.when_some(self.jsonpath_result.clone(), |this, outcome| {
                            this.child(self.render_jsonpath_result(outcome, cx))
                        })
                    })
                    .child(editor)
                    .into_any_element()
            }
        }
    }
}

impl ZedisBytesEditor {
    fn render_jsonpath_bar(&self, cx: &Context<Self>) -> impl IntoElement {
        h_flex()
            .w_full()
            .gap_2()
            .px_2()
            .py_1()
            .border_b_1()
            .border_color(cx.theme().border)
            .items_center()
            // `tab` accepts the highlighted completion (the menu auto-
            // selects the first item). gpui-component binds `tab` →
            // focus-next as an *action* at the Root, dispatched before
            // any key-down listener, so a capture handler can't win —
            // we override it with a deeper context-scoped keybinding
            // (`JsonPathBar`) routed here. We forward `Enter` to the
            // input's context menu, which returns `true` only when a
            // completion menu is actually open; if it isn't, we
            // propagate so `tab` keeps its normal focus movement.
            .key_context("JsonPathBar")
            .on_action(cx.listener(|this, _: &JsonPathAction, window, cx| {
                let accepted = this.jsonpath_input.update(cx, |state, cx| {
                    state.handle_action_for_context_menu(
                        Box::new(Enter {
                            secondary: false,
                            shift: false,
                        }),
                        window,
                        cx,
                    )
                });
                if accepted {
                    cx.stop_propagation();
                }
            }))
            // Collapse chevron — hides the bar again, back to the slim toggle.
            .child(
                Button::new("jsonpath-collapse")
                    .ghost()
                    .xsmall()
                    .icon(IconName::ChevronDown)
                    .on_click(cx.listener(|this, _, _window, cx| {
                        this.jsonpath_open = false;
                        cx.notify();
                    })),
            )
            .child(
                Label::new(i18n_editor(cx, "jsonpath_label"))
                    .text_xs()
                    .text_color(cx.theme().muted_foreground),
            )
            .child(
                Input::new(&self.jsonpath_input)
                    .small()
                    .flex_1()
                    .font_family(get_font_family()),
            )
            .child(
                Button::new("jsonpath-run")
                    .small()
                    .outline()
                    .label(i18n_editor(cx, "jsonpath_run"))
                    .on_click(cx.listener(|this, _, window, cx| this.run_jsonpath_query(window, cx))),
            )
    }

    /// Slim collapsed-state row that stands in for the JSONPath bar: a single
    /// ghost toggle that expands the full query bar on click. Keeps the query
    /// affordance discoverable without spending a full input row at rest.
    fn render_jsonpath_toggle(&self, cx: &Context<Self>) -> impl IntoElement {
        h_flex()
            .w_full()
            .px_2()
            .py_0p5()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                Button::new("jsonpath-expand")
                    .ghost()
                    .xsmall()
                    .icon(IconName::ChevronRight)
                    .label(i18n_editor(cx, "jsonpath_label"))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.jsonpath_open = true;
                        // Expanding is intent to query — drop the caret
                        // straight into the path input so the user can type.
                        this.jsonpath_input.update(cx, |state, cx| state.focus(window, cx));
                        cx.notify();
                    })),
            )
    }

    /// Render the JSONPath result. Two shapes:
    ///  * For Single/Multiple ⇒ embed the read-only code editor so the user
    ///    can scroll, search and copy from large JSON payloads.
    ///  * For status outcomes (NoMatch / errors) ⇒ a single-line label is
    ///    plenty.
    fn render_jsonpath_result(&self, outcome: JsonPathOutcome, cx: &Context<Self>) -> gpui::AnyElement {
        let container = div()
            .w_full()
            .px_2()
            .py_1()
            .border_b_1()
            .border_color(cx.theme().border);

        match outcome {
            JsonPathOutcome::Single(_) | JsonPathOutcome::Multiple(_) => container
                .child(
                    Input::new(&self.jsonpath_result_editor)
                        .bordered(false)
                        .appearance(false)
                        .focus_bordered(false)
                        .p_0()
                        .w_full()
                        .font_family(get_font_family()),
                )
                .into_any_element(),
            other => {
                let (text, color) = match other {
                    JsonPathOutcome::NotJson => (i18n_editor(cx, "jsonpath_not_json"), cx.theme().red),
                    JsonPathOutcome::InvalidPath(msg) => {
                        (SharedString::from(format!("Path error: {msg}")), cx.theme().red)
                    }
                    JsonPathOutcome::NoMatch => (i18n_editor(cx, "jsonpath_no_match"), cx.theme().muted_foreground),
                    _ => unreachable!("Single/Multiple handled in the matching arm above"),
                };
                container
                    .bg(cx.theme().muted.opacity(0.4))
                    .child(
                        Label::new(text)
                            .font_family(get_font_family())
                            .text_xs()
                            .text_color(color)
                            .whitespace_normal(),
                    )
                    .into_any_element()
            }
        }
    }
}
