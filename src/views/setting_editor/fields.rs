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

//! What `ZedisSettingEditor::new` builds, a section at a time: each
//! section's fields from the settings as they are, and what saves them.

use super::*;

pub(super) struct AppearanceFields {
    pub(super) font_size_state: Entity<InputState>,
    pub(super) locale_select: Entity<ZedisSelect>,
    pub(super) ui_font_select: Entity<ZedisSelect>,
    pub(super) mono_font_select: Entity<ZedisSelect>,
    pub(super) ui_font_values: Vec<Option<String>>,
    pub(super) mono_font_values: Vec<Option<String>>,
    pub(super) ui_font: Option<String>,
    pub(super) mono_font: Option<String>,
}

pub(super) struct DateTimeFields {
    pub(super) time_zone_select: Entity<ZedisSelect>,
    pub(super) date_format_select: Entity<ZedisSelect>,
}

pub(super) struct KeyBehaviorFields {
    pub(super) max_key_tree_depth_state: Entity<InputState>,
    pub(super) key_scan_count_state: Entity<InputState>,
    pub(super) auto_expand_threshold_state: Entity<InputState>,
    pub(super) max_truncate_length_state: Entity<InputState>,
    pub(super) value_search_scan_cap_state: Entity<InputState>,
    pub(super) value_search_time_budget_state: Entity<InputState>,
    pub(super) value_search_max_matches_state: Entity<InputState>,
    pub(super) json_view_select: Entity<ZedisSelect>,
}

#[cfg(not(target_family = "wasm"))]
pub(super) struct RedisFields {
    pub(super) redis_connection_timeout_state: Entity<InputState>,
    pub(super) redis_response_timeout_state: Entity<InputState>,
}

#[cfg(not(target_family = "wasm"))]
pub(super) struct AiFields {
    pub(super) ai_base_url_state: Entity<InputState>,
    pub(super) ai_api_key_state: Entity<InputState>,
    pub(super) ai_model_state: Entity<InputState>,
}

#[cfg(not(target_family = "wasm"))]
pub(super) struct SystemFields {
    pub(super) http_proxy_state: Entity<InputState>,
    pub(super) config_dir_state: Entity<InputState>,
}

impl ZedisSettingEditor {
    /// The Appearance section: the font size, saved as it is typed; the
    /// language; the UI and monospace fonts.
    pub(super) fn appearance_fields(
        window: &mut Window,
        cx: &mut Context<Self>,
        subscriptions: &mut Vec<Subscription>,
    ) -> AppearanceFields {
        let store = cx.global::<ZedisGlobalStore>().read(cx);
        let font_rem = store.font_rem_px().unwrap_or(DEFAULT_UI_FONT_SIZE);
        let ui_font = store.ui_font_family();
        let mono_font = store.mono_font_family();
        let locale = store.locale().to_string();

        let font_size_state = cx.new(|cx| {
            let px = font_rem.clamp(UI_FONT_SIZE_MIN, UI_FONT_SIZE_MAX).round() as i32;
            InputState::new(window, cx)
                .placeholder(i18n_settings(cx, "font_size_placeholder"))
                .default_value(px.to_string())
                .step(1.0)
                .min(UI_FONT_SIZE_MIN as f64)
                .max(UI_FONT_SIZE_MAX as f64)
        });
        subscriptions.push(cx.subscribe_in(
            &font_size_state,
            window,
            |_view, state, event: &InputEvent, window, cx| match event {
                InputEvent::Change => {
                    if let Some(px) = parse_font_rem_px(state.read(cx).value().as_ref()) {
                        persist_font_size(cx, px);
                    }
                }
                InputEvent::Blur => {
                    let fallback = cx
                        .global::<ZedisGlobalStore>()
                        .read(cx)
                        .font_rem_px()
                        .unwrap_or(DEFAULT_UI_FONT_SIZE);
                    let px = clamp_font_rem_px(state.read(cx).value().as_ref(), fallback);
                    let shown = (px as i32).to_string();
                    if state.read(cx).value().as_ref() != shown.as_str() {
                        state.update(cx, |input, cx| input.set_value(shown, window, cx));
                    }
                    persist_font_size(cx, px);
                }
                _ => {}
            },
        ));
        let locale_select = cx.new(|cx| {
            ZedisSelect::new(
                LOCALES.iter().map(|(_, label)| label.to_string()).collect(),
                Some(locale_to_index(&locale)),
                window,
                cx,
            )
        });

        subscriptions.push(cx.subscribe_in(
            &locale_select,
            window,
            |_view, _select, event: &ZedisSelectEvent, _window, cx| {
                let ZedisSelectEvent::Change(index) = event;
                let locale = index_to_locale(*index);
                save_ui_locale(cx, locale);
            },
        ));
        // UI + monospace font pickers: searchable dropdowns of the installed
        // families (drop the `.`-prefixed internal ones), each led by a
        // "default" entry. Changing either applies + persists both.
        let all_fonts: Vec<String> = cx
            .text_system()
            .all_font_names()
            .into_iter()
            .filter(|n| !n.starts_with('.'))
            .collect();
        // The bundled JetBrains Mono is registered at runtime (`add_fonts`), so
        // CoreText's enumeration may not list it — offer it explicitly.
        let mut mono_fonts = all_fonts.clone();
        if !mono_fonts.iter().any(|f| f == "JetBrains Mono") {
            mono_fonts.push("JetBrains Mono".to_string());
            mono_fonts.sort_unstable();
        }
        let (ui_labels, ui_font_values, ui_index) = build_font_options(
            &all_fonts,
            &ui_font,
            i18n_settings(cx, "font_system_default").to_string(),
        );
        let (mono_labels, mono_font_values, mono_index) = build_font_options(
            &mono_fonts,
            &mono_font,
            i18n_settings(cx, "font_mono_default").to_string(),
        );
        let ui_font_select = cx.new(|cx| ZedisSelect::new_searchable(ui_labels, ui_index, window, cx));
        let mono_font_select = cx.new(|cx| ZedisSelect::new_searchable(mono_labels, mono_index, window, cx));
        subscriptions.push(
            cx.subscribe(&ui_font_select, |this, _sel, event: &ZedisSelectEvent, cx| {
                let ZedisSelectEvent::Change(index) = event;
                this.ui_font = this.ui_font_values.get(*index).cloned().flatten();
                this.apply_and_save_fonts(cx);
            }),
        );
        subscriptions.push(
            cx.subscribe(&mono_font_select, |this, _sel, event: &ZedisSelectEvent, cx| {
                let ZedisSelectEvent::Change(index) = event;
                this.mono_font = this.mono_font_values.get(*index).cloned().flatten();
                this.apply_and_save_fonts(cx);
            }),
        );
        AppearanceFields {
            font_size_state,
            locale_select,
            ui_font_select,
            mono_font_select,
            ui_font_values,
            mono_font_values,
            ui_font,
            mono_font,
        }
    }

    /// The Date & time section: the time zone and the date layout.
    pub(super) fn date_time_fields(
        window: &mut Window,
        cx: &mut Context<Self>,
        subscriptions: &mut Vec<Subscription>,
    ) -> DateTimeFields {
        let store = cx.global::<ZedisGlobalStore>().read(cx);
        let time_zone = store.time_zone();
        let date_format = store.date_format();

        // Time zone + date layout: both mirror into the process-wide slot
        // right away (`set_datetime_prefs`), so every open panel renders the
        // new choice on its next frame — no restart, no window reopen.
        let zone_labels: Vec<String> = TimeZonePref::ALL
            .iter()
            .map(|zone| {
                let key = match zone {
                    TimeZonePref::Local => "time_zone_local",
                    TimeZonePref::Utc => "time_zone_utc",
                };
                i18n_settings(cx, key).to_string()
            })
            .collect();
        let zone_index = TimeZonePref::ALL.iter().position(|zone| *zone == time_zone);
        let time_zone_select = cx.new(|cx| ZedisSelect::new(zone_labels, zone_index, window, cx));
        subscriptions.push(cx.subscribe_in(
            &time_zone_select,
            window,
            |_view, _select, event: &ZedisSelectEvent, _window, cx| {
                let ZedisSelectEvent::Change(index) = event;
                let zone = TimeZonePref::ALL.get(*index).copied().unwrap_or_default();
                update_app_state_and_save(cx, "save_time_zone", move |state, _| {
                    state.set_time_zone(zone);
                    set_datetime_prefs(zone, &state.date_format());
                });
            },
        ));
        // The select shows a fixed sample instant in each layout — the
        // sample explains a format better than a name would.
        let format_labels: Vec<String> = DATE_FORMATS.iter().map(date_format_sample).collect();
        let format_index = DATE_FORMATS.iter().position(|format| format.id == date_format);
        let date_format_select = cx.new(|cx| ZedisSelect::new(format_labels, format_index, window, cx));
        subscriptions.push(cx.subscribe_in(
            &date_format_select,
            window,
            |_view, _select, event: &ZedisSelectEvent, _window, cx| {
                let ZedisSelectEvent::Change(index) = event;
                let Some(format) = DATE_FORMATS.get(*index) else {
                    return;
                };
                update_app_state_and_save(cx, "save_date_format", move |state, _| {
                    state.set_date_format(format.id);
                    set_datetime_prefs(state.time_zone(), format.id);
                });
            },
        ));
        DateTimeFields {
            time_zone_select,
            date_format_select,
        }
    }

    /// The Key behavior section's fields: the tree's depth, scan and expansion,
    /// the truncation, the value search's guardrails, how a JSON value opens.
    pub(super) fn key_behavior_fields(
        window: &mut Window,
        cx: &mut Context<Self>,
        subscriptions: &mut Vec<Subscription>,
    ) -> KeyBehaviorFields {
        let store = cx.global::<ZedisGlobalStore>().read(cx);
        let max_key_tree_depth = store.max_key_tree_depth();
        let auto_expand_threshold = store.auto_expand_threshold();
        let max_truncate_length = store.max_truncate_length();
        let key_scan_count = store.key_scan_count();
        let value_search_scan_cap = store.value_search_scan_cap();
        let value_search_time_budget = store.value_search_time_budget_secs();
        let value_search_max_matches = store.value_search_max_matches();
        let json_view = store.json_view();

        let max_key_tree_depth_state = Self::create_input_state(
            window,
            cx,
            "max_key_tree_depth_placeholder",
            max_key_tree_depth.to_string(),
            None,
        );
        let key_scan_count_state = Self::create_input_state(
            window,
            cx,
            "key_scan_count_placeholder",
            key_scan_count.to_string(),
            Some(|s| s.parse::<usize>().is_ok()),
        );
        let value_search_scan_cap_state = Self::create_input_state(
            window,
            cx,
            "value_search_scan_cap_placeholder",
            value_search_scan_cap.to_string(),
            Some(|s| s.parse::<usize>().is_ok()),
        );
        let value_search_time_budget_state = Self::create_input_state(
            window,
            cx,
            "value_search_time_budget_placeholder",
            value_search_time_budget.to_string(),
            Some(|s| s.parse::<u64>().is_ok()),
        );
        let value_search_max_matches_state = Self::create_input_state(
            window,
            cx,
            "value_search_max_matches_placeholder",
            value_search_max_matches.to_string(),
            Some(|s| s.parse::<usize>().is_ok()),
        );
        let auto_expand_threshold_state = Self::create_input_state(
            window,
            cx,
            "auto_expand_threshold_placeholder",
            auto_expand_threshold.to_string(),
            Some(|s| s.parse::<usize>().is_ok()),
        );
        let max_truncate_length_state = Self::create_input_state(
            window,
            cx,
            "max_truncate_length_placeholder",
            max_truncate_length.to_string(),
            Some(|s| s.parse::<usize>().is_ok()),
        );
        subscriptions.push(Self::bind_blur_save(
            cx,
            &max_key_tree_depth_state,
            window,
            |text, cx| {
                let value = text.parse::<i64>().unwrap_or_default();
                update_app_state_and_save(cx, "save_max_key_tree_depth", move |state, _| {
                    state.set_max_key_tree_depth(value as usize);
                });
            },
        ));

        subscriptions.push(
            cx.subscribe_in(&max_key_tree_depth_state, window, |_view, state, event, window, cx| {
                let NumberInputEvent::Step(action) = event;
                let Ok(current_val) = state.read(cx).value().parse::<u16>() else {
                    return;
                };
                let new_val = match action {
                    StepAction::Increment => current_val.saturating_add(1),
                    StepAction::Decrement => current_val.saturating_sub(1),
                };
                if new_val != current_val {
                    state.update(cx, |input, cx| {
                        input.set_value(new_val.to_string(), window, cx);
                    });
                }
            }),
        );

        subscriptions.push(cx.subscribe_in(
            &key_scan_count_state,
            window,
            |_view, state, event: &InputEvent, window, cx| {
                if !matches!(event, InputEvent::Blur) {
                    return;
                }
                let text = state.read(cx).value();
                let text = text.trim();
                // Range lives in the setter; write the clamped (or default)
                // value back so the box matches what SCAN will use. Live
                // validate stays parse-only: a min of 1000 would reject the
                // prefixes of 1000 itself.
                let shown = if text.is_empty() {
                    update_app_state_and_save(cx, "save_key_scan_count", |state, _| {
                        state.set_key_scan_count(0);
                    });
                    DEFAULT_KEY_SCAN_COUNT
                } else if let Ok(parsed) = text.parse::<usize>() {
                    let value = parsed.clamp(MIN_KEY_SCAN_COUNT, MAX_KEY_SCAN_COUNT);
                    update_app_state_and_save(cx, "save_key_scan_count", move |state, _| {
                        state.set_key_scan_count(value);
                    });
                    value
                } else {
                    return;
                };
                let shown = shown.to_string();
                if state.read(cx).value().as_ref() != shown.as_str() {
                    state.update(cx, |input, cx| input.set_value(shown, window, cx));
                }
            },
        ));

        // Value-search guardrails. Cleared input → 0 → the setter stores
        // None and the default applies; range clamping lives in the
        // setters so every write path shares it.
        subscriptions.push(Self::bind_blur_save(
            cx,
            &value_search_scan_cap_state,
            window,
            |text, cx| {
                let value = text.trim().parse::<usize>().unwrap_or_default();
                update_app_state_and_save(cx, "save_value_search_scan_cap", move |state, _| {
                    state.set_value_search_scan_cap(value);
                });
            },
        ));
        subscriptions.push(Self::bind_blur_save(
            cx,
            &value_search_time_budget_state,
            window,
            |text, cx| {
                let value = text.trim().parse::<u64>().unwrap_or_default();
                update_app_state_and_save(cx, "save_value_search_time_budget", move |state, _| {
                    state.set_value_search_time_budget_secs(value);
                });
            },
        ));
        subscriptions.push(Self::bind_blur_save(
            cx,
            &value_search_max_matches_state,
            window,
            |text, cx| {
                let value = text.trim().parse::<usize>().unwrap_or_default();
                update_app_state_and_save(cx, "save_value_search_max_matches", move |state, _| {
                    state.set_value_search_max_matches(value);
                });
            },
        ));

        subscriptions.push(Self::bind_blur_save(
            cx,
            &auto_expand_threshold_state,
            window,
            |text, cx| {
                let text = text.trim();
                if text.is_empty() {
                    // Cleared input → reset to default.
                    update_app_state_and_save(cx, "save_auto_expand_threshold", |state, _| {
                        state.set_auto_expand_threshold(0);
                    });
                } else if let Ok(value) = text.parse::<usize>()
                    && value >= 100
                {
                    update_app_state_and_save(cx, "save_auto_expand_threshold", move |state, _| {
                        state.set_auto_expand_threshold(value);
                    });
                }
            },
        ));

        subscriptions.push(Self::bind_blur_save(
            cx,
            &max_truncate_length_state,
            window,
            |text, cx| {
                let text = text.trim();
                if text.is_empty() {
                    // Cleared input → reset to default.
                    update_app_state_and_save(cx, "save_max_truncate_length", |state, _| {
                        state.set_max_truncate_length(0);
                    });
                } else if let Ok(value) = text.parse::<usize>()
                    && value >= 10
                {
                    update_app_state_and_save(cx, "save_max_truncate_length", move |state, _| {
                        state.set_max_truncate_length(value);
                    });
                }
            },
        ));
        // How a JSON value opens. Read when a key is opened, so the next one
        // follows the choice.
        let json_view_labels: Vec<String> = JsonView::ALL
            .iter()
            .map(|view| {
                // The switch's own words, so the two read the same.
                let key = match view {
                    JsonView::Text => "json_view_text",
                    JsonView::Tree => "json_view_tree",
                };
                i18n_editor(cx, key).to_string()
            })
            .collect();
        let json_view_index = JsonView::ALL.iter().position(|view| *view == json_view);
        let json_view_select = cx.new(|cx| ZedisSelect::new(json_view_labels, json_view_index, window, cx));
        subscriptions.push(cx.subscribe_in(
            &json_view_select,
            window,
            |_view, _select, event: &ZedisSelectEvent, _window, cx| {
                let ZedisSelectEvent::Change(index) = event;
                let view = JsonView::ALL.get(*index).copied().unwrap_or_default();
                update_app_state_and_save(cx, "save_json_view", move |state, _| {
                    state.set_json_view(view);
                });
            },
        ));
        KeyBehaviorFields {
            max_key_tree_depth_state,
            key_scan_count_state,
            auto_expand_threshold_state,
            max_truncate_length_state,
            value_search_scan_cap_state,
            value_search_time_budget_state,
            value_search_max_matches_state,
            json_view_select,
        }
    }

    /// The Redis connection section: the connection and response timeouts.
    #[cfg(not(target_family = "wasm"))]
    pub(super) fn redis_fields(
        window: &mut Window,
        cx: &mut Context<Self>,
        subscriptions: &mut Vec<Subscription>,
    ) -> RedisFields {
        let store = cx.global::<ZedisGlobalStore>().read(cx);
        let redis_connection_timeout = store.redis_connection_timeout();
        let redis_response_timeout = store.redis_response_timeout();

        let redis_connection_timeout_state = Self::create_input_state(
            window,
            cx,
            "redis_connection_timeout_placeholder",
            redis_connection_timeout,
            None,
        );
        let redis_response_timeout_state = Self::create_input_state(
            window,
            cx,
            "redis_response_timeout_placeholder",
            redis_response_timeout,
            None,
        );
        subscriptions.push(Self::bind_blur_save(
            cx,
            &redis_connection_timeout_state,
            window,
            |text, cx| {
                let duration = parse_duration(&text).ok();
                update_app_state_and_save(cx, "save_redis_connection_timeout", move |state, _| {
                    state.set_redis_connection_timeout(duration);
                });
            },
        ));

        subscriptions.push(Self::bind_blur_save(
            cx,
            &redis_response_timeout_state,
            window,
            |text, cx| {
                let duration = parse_duration(&text).ok();
                update_app_state_and_save(cx, "save_redis_response_timeout", move |state, _| {
                    state.set_redis_response_timeout(duration);
                });
            },
        ));
        RedisFields {
            redis_connection_timeout_state,
            redis_response_timeout_state,
        }
    }

    /// The AI section: the endpoint, its key and the model, saved without a
    /// window refresh.
    #[cfg(not(target_family = "wasm"))]
    pub(super) fn ai_fields(
        window: &mut Window,
        cx: &mut Context<Self>,
        subscriptions: &mut Vec<Subscription>,
    ) -> AiFields {
        let store = cx.global::<ZedisGlobalStore>().read(cx);
        let ai_base_url = store.ai_base_url();
        let ai_api_key = store.ai_api_key();
        let ai_model = store.ai_model();

        let ai_base_url_state = Self::create_input_state(window, cx, "ai_base_url_placeholder", ai_base_url, None);
        let ai_api_key_state = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(i18n_settings(cx, "ai_api_key_placeholder"))
                .default_value(ai_api_key)
                .masked(true)
        });
        let ai_model_state = Self::create_input_state(window, cx, "ai_model_placeholder", ai_model, None);
        // AI credentials have no visual output — persist without the
        // app-wide window refresh.
        subscriptions.push(Self::bind_blur_save(cx, &ai_base_url_state, window, |text, cx| {
            update_app_state_and_save_quiet(cx, "save_ai_base_url", move |state, _| {
                state.set_ai_base_url(text);
            });
        }));
        subscriptions.push(Self::bind_blur_save(cx, &ai_api_key_state, window, |text, cx| {
            update_app_state_and_save_quiet(cx, "save_ai_api_key", move |state, _| {
                state.set_ai_api_key(text);
            });
        }));
        subscriptions.push(Self::bind_blur_save(cx, &ai_model_state, window, |text, cx| {
            update_app_state_and_save_quiet(cx, "save_ai_model", move |state, _| {
                state.set_ai_model(text);
            });
        }));
        AiFields {
            ai_base_url_state,
            ai_api_key_state,
            ai_model_state,
        }
    }

    /// The System section's fields: the HTTP proxy, and the configuration
    /// directory to read.
    #[cfg(not(target_family = "wasm"))]
    pub(super) fn system_fields(
        window: &mut Window,
        cx: &mut Context<Self>,
        subscriptions: &mut Vec<Subscription>,
    ) -> SystemFields {
        let store = cx.global::<ZedisGlobalStore>().read(cx);
        let http_proxy = store.http_proxy();

        // Empty = follow env/OS system proxy; "none" = always direct; else a
        // proxy URI. No live validator: `InputState::validate` rejects any
        // keystroke whose *resulting* text fails, and URI validity isn't
        // prefix-closed — "http:/" is invalid, so the second slash of
        // "http://" could never be typed. Validation happens on blur instead.
        let http_proxy_state = Self::create_input_state(window, cx, "http_proxy_placeholder", http_proxy, None);
        let config_dir = config_dir_text();
        let config_dir_state = cx.new(|cx| InputState::new(window, cx).default_value(config_dir));
        subscriptions.push(
            cx.subscribe_in(&http_proxy_state, window, |_view, state, event, window, cx| {
                if let InputEvent::Blur = event {
                    let text = state.read(cx).value().trim().to_string();
                    if proxy_setting_accepted(&text) {
                        update_app_state_and_save_quiet(cx, "save_http_proxy", move |state, _| {
                            state.set_http_proxy(text);
                        });
                    } else {
                        // Never persist a value `app_proxy` can't act on; put
                        // the last saved value back so the box always shows
                        // what is actually in effect.
                        let saved = cx.global::<ZedisGlobalStore>().read(cx).http_proxy();
                        state.update(cx, |input, cx| input.set_value(saved, window, cx));
                    }
                }
            }),
        );
        SystemFields {
            http_proxy_state,
            config_dir_state,
        }
    }
}
