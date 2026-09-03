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

//! Editor body dispatch: per-type sub-editor selection plus the
//! inline load-error / value-too-large / no-key panels and the root
//! `Render` impl. Split out of `editor.rs`.

use super::*;

impl ZedisEditor {
    /// Render the appropriate editor based on the key type
    /// Inline error panel shown when a value load failed (the key stays
    /// selected). Offers a Retry that re-fetches the current key.
    pub(super) fn render_load_error(&mut self, message: SharedString, cx: &mut Context<Self>) -> impl IntoElement {
        let title = i18n_editor(cx, "value_load_failed");
        let retry = i18n_editor(cx, "retry");
        let danger = cx.theme().danger;
        let muted = cx.theme().muted_foreground;
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap_2()
            .p_4()
            .child(Label::new(title).text_color(danger))
            .child(Label::new(message).text_sm().text_color(muted))
            .child(
                Button::new("value-reload-retry")
                    .outline()
                    .label(retry)
                    .on_click(cx.listener(|this, _, _window, cx| {
                        let key = this.server_state.read(cx).key();
                        if let Some(key) = key {
                            this.server_state.update(cx, |state, cx| state.reload_value(key, cx));
                        }
                    })),
            )
    }

    /// The card for a key type Zedis has no viewer for: what the server
    /// calls it, which modules it runs, and the value as its DUMP bytes when
    /// they were fetched — else why not (size unknown, above the DUMP cap,
    /// DUMP unavailable). Type-agnostic actions in the toolbar (rename, TTL,
    /// delete, copy to another server) work as for any key.
    pub(super) fn render_module_value(
        &mut self,
        id: ModuleTypeId,
        size: u64,
        has_dump: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let locale = cx.global::<ZedisGlobalStore>().read(cx).locale().to_string();
        let title: SharedString = t!("editor.module_no_viewer", type_name = id.name(), locale = &locale)
            .to_string()
            .into();
        let modules = self.server_state.read(cx).nodes_description().modules.clone();
        let loaded: Option<SharedString> = (!modules.is_empty()).then(|| {
            t!("editor.module_loaded_modules", modules = modules, locale = &locale)
                .to_string()
                .into()
        });
        let dump_usable = self.server_state.read(cx).can(Capability::ExportKeys);
        let why_not: Option<SharedString> = if has_dump {
            None
        } else if size == 0 {
            Some(i18n_editor(cx, "module_size_unknown"))
        } else if size > MAX_MODULE_DUMP_BYTES {
            Some(
                t!(
                    "editor.module_too_large",
                    size = format_size(size, DECIMAL),
                    limit = format_size(MAX_MODULE_DUMP_BYTES, DECIMAL),
                    locale = &locale
                )
                .to_string()
                .into(),
            )
        } else if !dump_usable {
            Some(i18n_editor(cx, "module_dump_unavailable"))
        } else {
            None
        };
        let muted = cx.theme().muted_foreground;
        let hint = i18n_editor(cx, "module_hint");
        let open_terminal = i18n_editor(cx, "module_open_terminal");
        let mut card = v_flex()
            .size_full()
            .gap_2()
            .p_4()
            .child(Label::new(title))
            .when_some(loaded, |this, text| {
                this.child(Label::new(text).text_sm().text_color(muted))
            })
            .child(Label::new(hint).text_sm().text_color(muted))
            .child(
                div().child(
                    Button::new("module-open-terminal")
                        .outline()
                        .label(open_terminal)
                        .on_click(cx.listener(|this, _, _window, cx| {
                            this.server_state.update(cx, |state, cx| state.toggle_terminal(cx));
                        })),
                ),
            );
        if has_dump {
            let editor = self
                .bytes_editor
                .get_or_insert_with(|| {
                    debug!("Creating new bytes editor for a module value");
                    cx.new(|cx| ZedisBytesEditor::new(self.server_state.clone(), window, cx))
                })
                .clone();
            card = card.child(div().flex_1().min_h_0().w_full().child(editor));
        } else if let Some(text) = why_not {
            card = card.child(Label::new(text).text_sm().text_color(muted));
        }
        card
    }

    /// Inline panel shown when the oversized-value gate skipped the load:
    /// the probed size, the cap it exceeded, and an explicit bypass load.
    pub(super) fn render_value_too_large(&mut self, size: u64, cx: &mut Context<Self>) -> impl IntoElement {
        let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
        let title = i18n_editor(cx, "value_too_large");
        // A module value is fetched with DUMP, which stalls the *server*,
        // not just this UI — say so before the bypass.
        let is_module = self
            .server_state
            .read(cx)
            .value()
            .is_some_and(|v| matches!(v.key_type(), KeyType::Module(_)));
        let message_key = if is_module {
            "editor.module_value_too_large_message"
        } else {
            "editor.value_too_large_message"
        };
        let message: SharedString = t!(
            message_key,
            size = format_size(size, DECIMAL),
            limit = format_size(MAX_INLINE_VALUE_SIZE, DECIMAL),
            locale = locale
        )
        .to_string()
        .into();
        let load_anyway = i18n_editor(cx, "load_anyway");
        let muted = cx.theme().muted_foreground;
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap_2()
            .p_4()
            .child(Label::new(title))
            .child(Label::new(message).text_sm().text_color(muted))
            .child(
                Button::new("value-load-anyway")
                    .outline()
                    .label(load_anyway)
                    .on_click(cx.listener(|this, _, _window, cx| {
                        let key = this.server_state.read(cx).key();
                        if let Some(key) = key {
                            this.server_state
                                .update(cx, |state, cx| state.load_value_ignore_size_limit(key, cx));
                        }
                    })),
            )
    }

    pub(super) fn render_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // A failed value load keeps the key selected — surface the error
        // inline (with a Retry) instead of a blank/looks-empty sub-editor.
        if let Some(message) = self.server_state.read(cx).value().and_then(|v| v.failure_message()) {
            return self.render_load_error(message, cx).into_any_element();
        }
        // An oversized value was skipped by the size gate — offer an
        // explicit "load anyway" instead of a blank editor.
        if let Some(size) = self.server_state.read(cx).value().and_then(|v| v.too_large_size()) {
            return self.render_value_too_large(size, cx).into_any_element();
        }
        let Some(value) = self.server_state.read(cx).value() else {
            self.reset_editors(KeyType::Unknown);
            return div().into_any_element();
        };

        // Don't render anything if key type is unknown and still loading
        if value.key_type == KeyType::Unknown && value.is_busy() {
            return div().into_any_element();
        }

        // A module type without a viewer: the module card — type name,
        // loaded modules, and the DUMP bytes (read-only) when they were
        // fetched. The bytes editor is kept: it renders the hex.
        if let KeyType::Module(id) = value.key_type() {
            let size = value.size;
            let has_dump = value.bytes_value().is_some();
            self.reset_editors(KeyType::String);
            return self
                .render_module_value(id, size, has_dump, window, cx)
                .into_any_element();
        }

        // HyperLogLog sketches live in string keys; detect them from the
        // already-loaded bytes (the "HYLL" magic) so the dispatch can show
        // the dedicated read-only card instead of the raw bytes editor.
        let is_hll = value.key_type() == KeyType::String
            && value.bytes_value().is_some_and(|b| looks_like_hll(b.bytes.as_ref()));
        // Bitmap view: only for genuinely opaque (`DataFormat::Bytes`) non-HLL
        // string keys — anything the format pipeline decoded (Protobuf,
        // MessagePack, JSON, timestamps, compressed, …) keeps its own viewer.
        // Then the user's explicit override, else the small-binary heuristic.
        let bitmap_view = value.key_type() == KeyType::String
            && !is_hll
            && value.bytes_value().is_some_and(|b| {
                matches!(b.format, DataFormat::Bytes)
                    && self
                        .bitmap_override
                        .unwrap_or_else(|| looks_like_bitmap(b.bytes.as_ref()))
            });

        match value.key_type() {
            KeyType::List => {
                self.reset_editors(KeyType::List);
                let editor = self.list_editor.get_or_insert_with(|| {
                    debug!("Creating new list editor");
                    cx.new(|cx| ZedisListEditor::new(self.server_state.clone(), window, cx))
                });
                editor.clone().into_any_element()
            }
            KeyType::Set => {
                self.reset_editors(KeyType::Set);
                let editor = self.set_editor.get_or_insert_with(|| {
                    debug!("Creating new set editor");
                    cx.new(|cx| ZedisSetEditor::new(self.server_state.clone(), window, cx))
                });
                editor.clone().into_any_element()
            }
            KeyType::Zset => {
                self.reset_editors(KeyType::Zset);
                // Map mode only when a GEOPOS probe confirmed GEO data.
                let map_mode = self.zset_map_mode && self.zset_is_geo == Some(true);

                if map_mode {
                    let map = self.geo_map.get_or_insert_with(|| {
                        debug!("Creating new geo map");
                        let map = cx.new(|cx| ZedisGeoMap::new(self.server_state.clone(), window, cx));
                        // Switch back to the table when the map's toggle fires.
                        cx.subscribe(&map, |this, _, _event: &GeoMapEvent, cx| {
                            if this.zset_map_mode {
                                this.zset_map_mode = false;
                                cx.notify();
                            }
                        })
                        .detach();
                        map
                    });
                    map.clone().into_any_element()
                } else {
                    let editor = self.zset_editor.get_or_insert_with(|| {
                        debug!("Creating new zset editor");
                        let editor = cx.new(|cx| ZedisZsetEditor::new(self.server_state.clone(), window, cx));
                        // Inject the Table/Map toggle into the table footer, beside
                        // the keyword filter. Self-gates on the GEO probe so it only
                        // appears for geospatial sorted sets.
                        let weak = cx.weak_entity();
                        editor.update(cx, |ze, cx| {
                            ze.set_action_button_factory(
                                Box::new(move |_window, cx| {
                                    let Some(ed) = weak.upgrade() else {
                                        return vec![];
                                    };
                                    let mut buttons = Vec::new();
                                    // Score order: the chevron shows the
                                    // current walk (ZRANGE up, ZREVRANGE
                                    // down), a click flips it.
                                    let server_state = ed.read(cx).server_state.clone();
                                    if let Some(order) = server_state.read(cx).zset_sort_order() {
                                        let (icon, tooltip) = match order {
                                            SortOrder::Asc => {
                                                (IconName::ChevronUp, i18n_zset_editor(cx, "sort_desc_tooltip"))
                                            }
                                            SortOrder::Desc => {
                                                (IconName::ChevronDown, i18n_zset_editor(cx, "sort_asc_tooltip"))
                                            }
                                        };
                                        buttons.push(
                                            Button::new("zset-sort-order").icon(icon).tooltip(tooltip).on_click(
                                                move |_, _, cx| {
                                                    server_state.update(cx, |state, cx| {
                                                        state.set_zset_sort_order(order.toggled(), cx);
                                                    });
                                                },
                                            ),
                                        );
                                    }
                                    if ed.read(cx).zset_is_geo == Some(true) {
                                        let w_map = weak.clone();
                                        // Icon-only button matching the footer's
                                        // Add/Bulk style; opens the GEO map view.
                                        buttons.push(
                                            Button::new("zset-view-map")
                                                .icon(IconName::Map)
                                                .tooltip(i18n_geo_map(cx, "map"))
                                                .on_click(move |_, _, cx| {
                                                    let _ = w_map.update(cx, |e, cx| {
                                                        if !e.zset_map_mode {
                                                            e.zset_map_mode = true;
                                                            cx.notify();
                                                        }
                                                    });
                                                }),
                                        );
                                    }
                                    buttons
                                }),
                                cx,
                            );
                        });
                        editor
                    });
                    editor.clone().into_any_element()
                }
            }
            KeyType::Hash => {
                self.reset_editors(KeyType::Hash);
                let editor = self.hash_editor.get_or_insert_with(|| {
                    debug!("Creating new hash editor");
                    cx.new(|cx| ZedisHashEditor::new(self.server_state.clone(), window, cx))
                });
                editor.clone().into_any_element()
            }
            KeyType::Stream => {
                self.reset_editors(KeyType::Stream);
                let editor = self.stream_editor.get_or_insert_with(|| {
                    debug!("Creating new stream editor");
                    cx.new(|cx| ZedisStreamEditor::new(self.server_state.clone(), window, cx))
                });
                editor.clone().into_any_element()
            }
            KeyType::Channel => {
                self.reset_editors(KeyType::Channel);
                let editor = self.pubsub_editor.get_or_insert_with(|| {
                    debug!("Creating new pubsub editor");
                    cx.new(|cx| ZedisPubsubEditor::new(self.server_state.clone(), window, cx))
                });
                editor.clone().into_any_element()
            }
            KeyType::TimeSeries => {
                self.reset_editors(KeyType::TimeSeries);
                let editor = self.timeseries_editor.get_or_insert_with(|| {
                    debug!("Creating new time series editor");
                    cx.new(|cx| ZedisTimeSeriesEditor::new(self.server_state.clone(), window, cx))
                });
                editor.clone().into_any_element()
            }
            KeyType::Probabilistic(kind) => {
                self.reset_editors(KeyType::Probabilistic(kind));
                let editor = self.probabilistic_editor.get_or_insert_with(|| {
                    debug!("Creating new probabilistic editor");
                    cx.new(|cx| ZedisProbabilisticEditor::new(self.server_state.clone(), kind, window, cx))
                });
                editor.clone().into_any_element()
            }
            KeyType::Vectorset => {
                self.reset_editors(KeyType::Vectorset);
                let editor = self.vector_set_editor.get_or_insert_with(|| {
                    debug!("Creating new vector set editor");
                    cx.new(|cx| ZedisVectorSetEditor::new(self.server_state.clone(), window, cx))
                });
                editor.clone().into_any_element()
            }
            _ => {
                // Default to bytes editor for String type and other types
                self.reset_editors(KeyType::String);

                // A string key holding an HLL sketch gets the dedicated
                // read-only card instead of the (binary-garbage) bytes view.
                if is_hll {
                    let _ = self.bytes_editor.take();
                    let editor = self.hll_editor.get_or_insert_with(|| {
                        debug!("Creating new HLL editor");
                        cx.new(|cx| ZedisHllEditor::new(self.server_state.clone(), window, cx))
                    });
                    return editor.clone().into_any_element();
                }
                let _ = self.hll_editor.take();

                // Bitmap view: chosen by the heuristic or an explicit toggle
                // (computed above as `bitmap_view`).
                if bitmap_view {
                    let readonly = self.readonly;
                    let editor = self.bitmap_editor.get_or_insert_with(|| {
                        debug!("Creating new bitmap editor");
                        let editor =
                            cx.new(|cx| ZedisBitmapEditor::new(self.server_state.clone(), readonly, window, cx));
                        // The "Raw" toggle pins the raw bytes view for this key.
                        cx.subscribe(&editor, |this, _, _event: &BitmapEvent, cx| {
                            this.bitmap_override = Some(false);
                            cx.notify();
                        })
                        .detach();
                        editor
                    });
                    return editor.clone().into_any_element();
                }
                let _ = self.bitmap_editor.take();

                let editor = self
                    .bytes_editor
                    .get_or_insert_with(|| {
                        debug!("Creating new bytes editor");
                        cx.new(|cx| ZedisBytesEditor::new(self.server_state.clone(), window, cx))
                    })
                    .clone();
                // Swap the bytes editor out for the side-by-side diff
                // view when a session is open. The bytes editor itself
                // stays alive in `self.bytes_editor` so closing the
                // diff returns the user to it without losing pending
                // edits.
                if let Some(diff_view) = self.diff_view.as_ref() {
                    diff_view.clone().into_any_element()
                } else {
                    editor.into_any_element()
                }
            }
        }
    }

    /// Centered empty state for the right panel when no key is selected.
    ///
    /// The editor pane is otherwise a blank rectangle on first connect —
    /// before the user clicks anything in the tree — which reads as dead
    /// space. Layout:
    /// 1. Calm hero (icon + title + hint)
    /// 2. Quick-action buttons for the most common no-key flows
    /// 3. One guide card: keyboard shortcuts, key-tree tips, and
    ///    status-bar orientation
    ///
    /// Shortcut descriptions reuse the `shortcuts.` locale section so
    /// the ⌘/ overlay and this list can never drift apart in wording.
    pub(super) fn render_no_key_selected(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let fg = theme.foreground;
        let chip_bg = theme.muted;
        let border = theme.border;
        let radius = theme.radius;
        let card_bg = card_background(cx);
        let mono = get_mono_font_family();

        // Only actions that work with nothing selected — key-bound ones
        // (save / TTL / rename / delete) would be misleading here. Two
        // columns: find/browse on the left, act on the right.
        const FIND_HINTS: [(&str, &str); 4] = [
            ("cmd-f", "search"),
            ("cmd-k", "command_palette"),
            ("cmd-p", "recent_keys"),
            ("cmd-shift-f", "multi_search"),
        ];
        const ACT_HINTS: [(&str, &str); 4] = [
            ("cmd-n", "new_key"),
            ("cmd-r", "reload_keys"),
            ("cmd-j", "terminal"),
            ("cmd-/", "keyboard_shortcuts"),
        ];

        // Chip-first rows: fixed min width on the keystroke so labels
        // start on one vertical line inside each column.
        let kbd_chip = |keystroke: &str| {
            div()
                .min_w(px(52.))
                .px_1p5()
                .py_0p5()
                .rounded(radius)
                .bg(chip_bg)
                .border_1()
                .border_color(border)
                .flex()
                .items_center()
                .justify_center()
                .child(
                    Label::new(humanize_keystroke(keystroke))
                        .text_xs()
                        .font_family(mono.clone())
                        .text_color(fg.alpha(0.9)),
                )
        };
        let hint_column = |cx: &mut Context<Self>, hints: &[(&str, &str)]| {
            let mut column = v_flex().flex_1().min_w(px(168.)).gap_1p5();
            for (keystroke, desc_key) in hints {
                column = column.child(
                    h_flex()
                        .items_center()
                        .gap_2()
                        .child(kbd_chip(keystroke))
                        .child(Label::new(i18n_shortcuts(cx, desc_key)).text_sm().text_color(muted)),
                );
            }
            column
        };

        let tip_row = |icon: Icon, text: SharedString| {
            h_flex()
                .items_start()
                .gap_2()
                .child(
                    div()
                        .w(px(16.))
                        .pt_0p5()
                        .flex()
                        .justify_center()
                        .child(icon.with_size(px(13.)).text_color(muted.alpha(0.85))),
                )
                .child(Label::new(text).text_xs().text_color(muted).whitespace_normal())
        };

        // Quick actions: one click to the flows people reach for before
        // they've picked a key. Labels reuse the shortcuts locale so
        // wording matches the list below and the ⌘/ overlay.
        let quick_actions = h_flex()
            .mt_3()
            .gap_2()
            .flex_wrap()
            .justify_center()
            .child(
                Button::new("empty-new-key")
                    .outline()
                    .small()
                    .icon(IconName::Plus)
                    .label(i18n_shortcuts(cx, "new_key"))
                    .tooltip(humanize_keystroke("cmd-n"))
                    .on_click(cx.listener(|this, _, _window, cx| {
                        this.server_state
                            .update(cx, |state, cx| state.emit_editor_action(EditorAction::Create, cx));
                    })),
            )
            .child(
                Button::new("empty-search")
                    .outline()
                    .small()
                    .icon(IconName::Search)
                    .label(i18n_shortcuts(cx, "search"))
                    .tooltip(humanize_keystroke("cmd-f"))
                    .on_click(cx.listener(|_this, _, window, cx| {
                        window.dispatch_action(Box::new(EditorAction::Search), cx);
                    })),
            )
            .child(
                Button::new("empty-terminal")
                    .outline()
                    .small()
                    .icon(IconName::SquareTerminal)
                    .label(i18n_shortcuts(cx, "terminal"))
                    .tooltip(humanize_keystroke("cmd-j"))
                    .on_click(cx.listener(|this, _, _window, cx| {
                        this.server_state.update(cx, |state, cx| state.toggle_terminal(cx));
                    })),
            )
            .child(
                Button::new("empty-multi-search")
                    .outline()
                    .small()
                    .icon(IconName::Globe)
                    .label(i18n_shortcuts(cx, "multi_search"))
                    .tooltip(humanize_keystroke("cmd-shift-f"))
                    .on_click(cx.listener(|_this, _, window, cx| {
                        window.dispatch_action(Box::new(MultiSearchAction::Toggle), cx);
                    })),
            );

        // One card for discoverability blocks — shared width, border, and
        // hairline dividers keep hierarchy without floating sections.
        let guide = v_flex()
            .mt_4()
            .w(px(480.))
            .rounded(theme.radius_lg)
            .border_1()
            .border_color(border)
            .bg(card_bg)
            .overflow_hidden()
            .child(
                v_flex()
                    .px_4()
                    .pt_3()
                    .pb_3()
                    .gap_2p5()
                    .child(
                        Label::new(i18n_shortcuts(cx, "title"))
                            .text_xs()
                            .font_medium()
                            .text_color(muted),
                    )
                    .child(
                        h_flex()
                            .w_full()
                            .flex_wrap()
                            .gap_x_6()
                            .gap_y_2()
                            .child(hint_column(cx, &FIND_HINTS))
                            .child(hint_column(cx, &ACT_HINTS)),
                    ),
            )
            .child(
                v_flex()
                    .px_4()
                    .py_3()
                    .gap_1p5()
                    .border_t_1()
                    .border_color(border)
                    .child(
                        Label::new(i18n_editor(cx, "tree_tips_title"))
                            .text_xs()
                            .font_medium()
                            .text_color(muted),
                    )
                    .child(tip_row(
                        Icon::new(IconName::FolderOpen),
                        i18n_editor(cx, "tree_tip_select"),
                    ))
                    .child(tip_row(Icon::new(IconName::Menu), i18n_editor(cx, "tree_tip_context")))
                    .child(tip_row(
                        Icon::new(IconName::Settings2),
                        i18n_editor(cx, "tree_tip_filters"),
                    ))
                    .child(tip_row(
                        Icon::new(CustomIconName::SquareCheck),
                        i18n_editor(cx, "tree_tip_multi"),
                    )),
            )
            .child(
                v_flex()
                    .px_4()
                    .py_3()
                    .gap_1p5()
                    .border_t_1()
                    .border_color(border)
                    .child(
                        Label::new(i18n_editor(cx, "status_bar_hints_title"))
                            .text_xs()
                            .font_medium()
                            .text_color(muted),
                    )
                    .child(tip_row(
                        Icon::new(IconName::Menu),
                        i18n_editor(cx, "status_bar_hint_tools"),
                    ))
                    .child(tip_row(
                        Icon::new(CustomIconName::Activity),
                        i18n_editor(cx, "status_bar_hint_metrics"),
                    ))
                    .child(tip_row(
                        Icon::new(CustomIconName::Link),
                        i18n_editor(cx, "status_bar_hint_connection"),
                    )),
            );

        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            // Optical centering: with the taller card the geometric
            // center reads a little low — bias the whole block upward.
            .pb(px(32.))
            .px_6()
            .gap_2()
            .child(
                Icon::new(IconName::Inbox)
                    .with_size(px(40.))
                    .text_color(muted.alpha(0.5)),
            )
            .child(
                Label::new(i18n_editor(cx, "no_key_selected_title"))
                    .font_medium()
                    .text_color(fg),
            )
            .child(
                Label::new(i18n_editor(cx, "no_key_selected_hint"))
                    .text_sm()
                    .text_color(muted)
                    .text_center(),
            )
            .child(quick_actions)
            .child(guide)
    }
}

impl Render for ZedisEditor {
    /// Main render method - displays key info bar and appropriate editor
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let server_state = self.server_state.read(cx);
        let is_channel_mode = server_state.is_channel_mode();
        let no_key_selected = !is_channel_mode && server_state.key().is_none();

        // Right after connecting (no key clicked yet) the pane would
        // otherwise be blank — show a centered empty-state hint instead.
        if no_key_selected {
            return self.render_no_key_selected(cx).into_any_element();
        }
        if let Some(true) = self.should_enter_ttl_edit_mode.take() {
            self.enter_ttl_edit_mode(window, cx);
        }
        if std::mem::take(&mut self.pending_rename_dialog) {
            self.open_rename_dialog(window, cx);
        }
        if let Some((old, new)) = self.pending_overwrite_confirm.take() {
            self.open_overwrite_confirm(old, new, window, cx);
        }
        if let Some((key, draft)) = self.pending_save_conflict.take() {
            self.open_save_conflict_dialog(key, draft, window, cx);
        }

        v_flex()
            .w_full()
            .h_full()
            .when(!is_channel_mode, |this| this.child(self.render_select_key(cx)))
            // The key bar above is fixed-height; the editor body must be a
            // bounded flex item (`flex_1` + `min_h_0`) so children that scroll
            // internally — e.g. the side-by-side diff view's panes — have a
            // definite parent height to shrink against. Without this the diff
            // panes grow to their content height and never show a scrollbar.
            .child(div().flex_1().min_h_0().child(self.render_editor(window, cx)))
            .on_action(cx.listener(move |this, event: &EditorAction, window, cx| match event {
                EditorAction::Save => {
                    this.save(window, cx);
                }
                EditorAction::AutoRefresh(interval) => {
                    this.start_auto_refresh(Some(*interval as u64), cx);
                }
                EditorAction::LoadHistory(idx) => {
                    this.load_history_entry(*idx, window, cx);
                }
                EditorAction::DiffHistory(idx) => {
                    this.open_diff_session(*idx, cx);
                }
                EditorAction::ExportValue => {
                    this.export_value_to_file(cx);
                }
                EditorAction::ImportValue => {
                    this.import_value_from_file(cx);
                }
                EditorAction::ViewBitmap => {
                    this.bitmap_override = Some(true);
                    cx.notify();
                }
                EditorAction::Delete => {
                    this.delete_key(window, cx);
                }
                EditorAction::Rename => {
                    this.open_rename_dialog(window, cx);
                }
                EditorAction::CopyTo => {
                    this.open_copy_dialog(window, cx);
                }
                EditorAction::DiffWithServer => {
                    this.open_diff_with_server_dialog(window, cx);
                }
                _ => {
                    cx.propagate();
                }
            }))
            .into_any_element()
    }
}
use crate::connection::Capability;
use crate::states::{MAX_MODULE_DUMP_BYTES, ModuleTypeId};
use crate::states::{SortOrder, i18n_zset_editor};
