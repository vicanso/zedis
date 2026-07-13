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

    /// Inline panel shown when the oversized-value gate skipped the load:
    /// the probed size, the cap it exceeded, and an explicit bypass load.
    pub(super) fn render_value_too_large(&mut self, size: u64, cx: &mut Context<Self>) -> impl IntoElement {
        let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
        let title = i18n_editor(cx, "value_too_large");
        let message: SharedString = t!(
            "editor.value_too_large_message",
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
                                    if ed.read(cx).zset_is_geo != Some(true) {
                                        return vec![];
                                    }
                                    let w_map = weak.clone();
                                    // Icon-only button matching the footer's
                                    // Add/Bulk style; opens the GEO map view.
                                    vec![
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
                                    ]
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
    /// space. A calm muted icon + a one-line hint pointing at the key
    /// tree gives the pane a deliberate resting state instead.
    pub(super) fn render_no_key_selected(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap_2()
            .child(
                Icon::new(IconName::Inbox)
                    .with_size(px(44.))
                    .text_color(muted.alpha(0.55)),
            )
            .child(
                Label::new(i18n_editor(cx, "no_key_selected_title"))
                    .font_medium()
                    .text_color(cx.theme().foreground),
            )
            .child(
                Label::new(i18n_editor(cx, "no_key_selected_hint"))
                    .text_sm()
                    .text_color(muted),
            )
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
