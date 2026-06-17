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

use crate::views::secondary_window::open_secondary_window;
use crate::{
    helpers::{get_or_create_config_dir, parse_duration},
    states::{ZedisGlobalStore, i18n_settings, update_app_state_and_save},
};
use gpui::{
    App, Bounds, Entity, FontWeight, Subscription, TitlebarOptions, Window, WindowBounds, WindowOptions, div,
    prelude::*, px, size,
};
use gpui_component::{
    ActiveTheme, h_flex,
    input::{Input, InputEvent, InputState, NumberInput, NumberInputEvent, StepAction},
    label::Label,
    scroll::ScrollableElement,
    slider::{Slider, SliderEvent, SliderState, SliderValue},
    switch::Switch,
    v_flex,
};
use zedis_ui::{ZedisSelect, ZedisSelectEvent};

/// Locale codes in display order, matching the items passed to locale_select.
const LOCALES: &[(&str, &str)] = &[
    ("en", "English"),
    ("zh", "中文"),
    ("ru", "Русский"),
    ("ja", "日本語"),
    ("pt", "Português"),
    ("es", "Español"),
    ("de", "Deutsch"),
    ("fr", "Français"),
];

fn locale_to_index(locale: &str) -> usize {
    LOCALES.iter().position(|(code, _)| *code == locale).unwrap_or(0)
}

fn index_to_locale(index: usize) -> &'static str {
    LOCALES.get(index).map(|(code, _)| *code).unwrap_or("en")
}

pub struct ZedisSettingEditor {
    max_key_tree_depth_state: Entity<InputState>,
    key_separator_state: Entity<InputState>,
    max_truncate_length_state: Entity<InputState>,
    config_dir_state: Entity<InputState>,
    key_scan_count_state: Entity<InputState>,
    auto_expand_threshold_state: Entity<InputState>,
    redis_connection_timeout_state: Entity<InputState>,
    redis_response_timeout_state: Entity<InputState>,
    ai_base_url_state: Entity<InputState>,
    ai_api_key_state: Entity<InputState>,
    ai_model_state: Entity<InputState>,
    tray_enabled: bool,
    show_key_tree_ttl: bool,
    font_size_slider: Entity<SliderState>,
    locale_select: Entity<ZedisSelect>,
    _subscriptions: Vec<Subscription>,
}

impl ZedisSettingEditor {
    fn create_input_state(
        window: &mut Window,
        cx: &mut Context<Self>,
        placeholder_key: &str,
        default_val: String,
        validate: Option<fn(&str) -> bool>,
    ) -> Entity<InputState> {
        cx.new(|cx| {
            let mut state = InputState::new(window, cx)
                .placeholder(i18n_settings(cx, placeholder_key))
                .default_value(default_val);

            if let Some(v) = validate {
                state = state.validate(move |s, _| v(s));
            }
            state
        })
    }

    fn bind_blur_save<F>(
        cx: &mut Context<Self>,
        state: &Entity<InputState>,
        window: &Window,
        mut save_action: F,
    ) -> Subscription
    where
        F: FnMut(String, &mut Context<Self>) + 'static,
    {
        cx.subscribe_in(state, window, move |_view, state, event, _window, cx| {
            if let InputEvent::Blur = event {
                let text = state.read(cx).value();
                save_action(text.to_string(), cx);
            }
        })
    }

    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let store = cx.global::<ZedisGlobalStore>().read(cx);
        let max_key_tree_depth = store.max_key_tree_depth();
        let key_separator = store.key_separator().to_string();
        let auto_expand_threshold = store.auto_expand_threshold();
        let max_truncate_length = store.max_truncate_length();
        let redis_connection_timeout = store.redis_connection_timeout();
        let redis_response_timeout = store.redis_response_timeout();
        let key_scan_count = store.key_scan_count();
        let tray_enabled = store.tray_enabled();
        let show_key_tree_ttl = store.show_key_tree_ttl();
        let font_rem = store.font_rem_px().unwrap_or(16.0);
        let locale = store.locale().to_string();
        let ai_base_url = store.ai_base_url();
        let ai_api_key = store.ai_api_key();
        let ai_model = store.ai_model();

        let max_key_tree_depth_state = Self::create_input_state(
            window,
            cx,
            "max_key_tree_depth_placeholder",
            max_key_tree_depth.to_string(),
            None,
        );
        let key_separator_state =
            Self::create_input_state(window, cx, "key_separator_placeholder", key_separator, None);
        let key_scan_count_state = Self::create_input_state(
            window,
            cx,
            "key_scan_count_placeholder",
            key_scan_count.to_string(),
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

        let ai_base_url_state = Self::create_input_state(window, cx, "ai_base_url_placeholder", ai_base_url, None);
        let ai_api_key_state = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(i18n_settings(cx, "ai_api_key_placeholder"))
                .default_value(ai_api_key)
                .masked(true)
        });
        let ai_model_state = Self::create_input_state(window, cx, "ai_model_placeholder", ai_model, None);

        let config_dir = get_or_create_config_dir().unwrap_or_default();

        let mut subscriptions = Vec::new();
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

        subscriptions.push(Self::bind_blur_save(cx, &key_separator_state, window, |text, cx| {
            update_app_state_and_save(cx, "save_key_separator", move |state, _| {
                state.set_key_separator(text);
            });
        }));

        subscriptions.push(Self::bind_blur_save(cx, &key_scan_count_state, window, |text, cx| {
            let text = text.trim();
            if text.is_empty() {
                // Cleared input → reset to default (set_key_scan_count maps 0 → None).
                update_app_state_and_save(cx, "save_key_scan_count", |state, _| {
                    state.set_key_scan_count(0);
                });
            } else if let Ok(value) = text.parse::<usize>() {
                // Clamp so a tiny/huge "Per Scan" can't break paging; down to 10
                // so the first-page load can actually be small.
                let value = value.clamp(10, 100_000);
                update_app_state_and_save(cx, "save_key_scan_count", move |state, _| {
                    state.set_key_scan_count(value);
                });
            }
        }));

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

        let config_dir_state =
            cx.new(|cx| InputState::new(window, cx).default_value(config_dir.to_string_lossy().to_string()));

        subscriptions.push(Self::bind_blur_save(cx, &ai_base_url_state, window, |text, cx| {
            update_app_state_and_save(cx, "save_ai_base_url", move |state, _| {
                state.set_ai_base_url(text);
            });
        }));
        subscriptions.push(Self::bind_blur_save(cx, &ai_api_key_state, window, |text, cx| {
            update_app_state_and_save(cx, "save_ai_api_key", move |state, _| {
                state.set_ai_api_key(text);
            });
        }));
        subscriptions.push(Self::bind_blur_save(cx, &ai_model_state, window, |text, cx| {
            update_app_state_and_save(cx, "save_ai_model", move |state, _| {
                state.set_ai_model(text);
            });
        }));

        // Continuous font size (rem px) via a slider, 12–22px. The save is
        // debounced; `cx.notify()` re-renders the row so the px readout tracks
        // the thumb live.
        let font_size_slider = cx.new(|_| {
            SliderState::new()
                .min(12.0)
                .max(20.0)
                .step(1.0)
                .default_value(font_rem.clamp(12.0, 20.0))
        });
        subscriptions.push(cx.subscribe_in(
            &font_size_slider,
            window,
            |_view, _slider, event: &SliderEvent, _window, cx| {
                if let SliderEvent::Change(SliderValue::Single(rem)) = event {
                    let rem = *rem;
                    update_app_state_and_save(cx, "save_font_size", move |state, _| {
                        state.set_font_rem_px(Some(rem));
                    });
                    cx.notify();
                }
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
                update_app_state_and_save(cx, "save_locale", move |state, _| {
                    state.set_locale(locale.to_string());
                });
            },
        ));

        Self {
            _subscriptions: subscriptions,
            key_scan_count_state,
            config_dir_state,
            auto_expand_threshold_state,
            max_truncate_length_state,
            key_separator_state,
            max_key_tree_depth_state,
            redis_response_timeout_state,
            redis_connection_timeout_state,
            ai_base_url_state,
            ai_api_key_state,
            ai_model_state,
            tray_enabled,
            show_key_tree_ttl,
            font_size_slider,
            locale_select,
        }
    }

    fn render_setting_row(cx: &Context<Self>, label_key: &str, input_element: impl IntoElement) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let desc_key = format!("{label_key}_desc");
        h_flex()
            .w_full()
            .justify_between()
            .items_center()
            .gap_8()
            .py_2()
            .child(
                v_flex()
                    .flex_1()
                    .gap_0p5()
                    .child(Label::new(i18n_settings(cx, label_key)).text_sm())
                    .child(Label::new(i18n_settings(cx, &desc_key)).text_xs().text_color(muted)),
            )
            .child(div().w(px(200.)).child(input_element))
    }

    fn render_section_header(cx: &Context<Self>, title_key: &str, desc_key: &str) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        v_flex()
            .w_full()
            .gap_1()
            .pt_8()
            .pb_4()
            .child(
                Label::new(i18n_settings(cx, title_key))
                    .text_sm()
                    .font_weight(FontWeight::BOLD),
            )
            .child(Label::new(i18n_settings(cx, desc_key)).text_xs().text_color(muted))
    }
}

impl Render for ZedisSettingEditor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Apply the *persisted* font size, not the live slider value: this
        // window scales by rem and the slider sets rem, so driving rem straight
        // from the slider rescales the slider itself mid-drag — the thumb drifts
        // and resizes (e.g. looked centred at 15px). The debounced store keeps
        // the control stable while dragging.
        if let Some(rem) = cx.global::<ZedisGlobalStore>().read(cx).font_rem_px() {
            window.set_rem_size(rem);
        }

        v_flex().size_full().overflow_y_scrollbar().px_6().child(
            v_flex()
                .w_full()
                .mx_auto()
                // — Appearance —
                .child(Self::render_section_header(
                    cx,
                    "section_appearance",
                    "section_appearance_desc",
                ))
                .child(Self::render_setting_row(cx, "font_size", {
                    let muted = cx.theme().muted_foreground;
                    let rem = match self.font_size_slider.read(cx).value() {
                        SliderValue::Single(v) | SliderValue::Range(v, _) => v,
                    };
                    // Fill the row's shared 200px control column (render_setting_row
                    // wraps every input in a `w(px(200.))` div). `min_w_0` lets the
                    // flex_1 slider shrink below its content width so the px readout
                    // beside it stays visible — the original overflow was flex_1
                    // with the default `min-width: auto`.
                    // gap_4 (1rem) leaves room for the thumb, which overhangs the
                    // track end by ~half its width (size_4) at the max; flex_none
                    // keeps the px readout from being squeezed.
                    h_flex()
                        .w_full()
                        .gap_4()
                        .items_center()
                        .child(Slider::new(&self.font_size_slider).flex_1().min_w_0())
                        .child(
                            Label::new(format!("{}px", rem as i32))
                                .flex_none()
                                .text_sm()
                                .text_color(muted),
                        )
                }))
                .child(Self::render_setting_row(cx, "lang", self.locale_select.clone()))
                // — Key Behavior —
                .child(Self::render_section_header(
                    cx,
                    "section_key_behavior",
                    "section_key_behavior_desc",
                ))
                .child(Self::render_setting_row(
                    cx,
                    "max_key_tree_depth",
                    NumberInput::new(&self.max_key_tree_depth_state),
                ))
                .child(Self::render_setting_row(
                    cx,
                    "key_separator",
                    Input::new(&self.key_separator_state),
                ))
                .child(Self::render_setting_row(
                    cx,
                    "key_scan_count",
                    Input::new(&self.key_scan_count_state),
                ))
                .child(Self::render_setting_row(
                    cx,
                    "auto_expand_threshold",
                    Input::new(&self.auto_expand_threshold_state),
                ))
                .child(Self::render_setting_row(
                    cx,
                    "show_key_tree_ttl",
                    Switch::new("show-key-tree-ttl")
                        .checked(self.show_key_tree_ttl)
                        .on_click(cx.listener(|this, checked: &bool, _window, cx| {
                            this.show_key_tree_ttl = *checked;
                            let enabled = *checked;
                            update_app_state_and_save(cx, "save_show_key_tree_ttl", move |state, _| {
                                state.set_show_key_tree_ttl(enabled);
                            });
                        })),
                ))
                .child(Self::render_setting_row(
                    cx,
                    "max_truncate_length",
                    Input::new(&self.max_truncate_length_state),
                ))
                // — Redis Connection —
                .child(Self::render_section_header(cx, "section_redis", "section_redis_desc"))
                .child(Self::render_setting_row(
                    cx,
                    "redis_connection_timeout",
                    Input::new(&self.redis_connection_timeout_state),
                ))
                .child(Self::render_setting_row(
                    cx,
                    "redis_response_timeout",
                    Input::new(&self.redis_response_timeout_state),
                ))
                // — AI Analysis —
                .child(Self::render_section_header(cx, "section_ai", "section_ai_desc"))
                .child(Self::render_setting_row(
                    cx,
                    "ai_base_url",
                    Input::new(&self.ai_base_url_state),
                ))
                .child(Self::render_setting_row(
                    cx,
                    "ai_api_key",
                    Input::new(&self.ai_api_key_state).mask_toggle(),
                ))
                .child(Self::render_setting_row(
                    cx,
                    "ai_model",
                    Input::new(&self.ai_model_state),
                ))
                // — System —
                .child(Self::render_section_header(cx, "section_system", "section_system_desc"))
                .when(cfg!(not(target_os = "linux")), |this| {
                    this.child(Self::render_setting_row(
                        cx,
                        "tray_enabled",
                        Switch::new("tray-enabled")
                            .checked(self.tray_enabled)
                            .on_click(cx.listener(|this, checked: &bool, _window, cx| {
                                this.tray_enabled = *checked;
                                let enabled = *checked;
                                update_app_state_and_save(cx, "save_tray_enabled", move |state, _| {
                                    state.set_tray_enabled(enabled);
                                });
                            })),
                    ))
                })
                .child(Self::render_setting_row(
                    cx,
                    "config_dir",
                    Input::new(&self.config_dir_state).disabled(true),
                )),
        )
    }
}

pub fn open_settings_window(cx: &mut App) {
    let window_size = size(px(700.), px(560.));
    let title = i18n_settings(cx, "title");
    open_secondary_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(None, window_size, cx))),
            titlebar: Some(TitlebarOptions {
                title: Some(title),
                ..Default::default()
            }),
            is_resizable: false,
            focus: true,
            ..Default::default()
        },
        cx,
        |window, cx| cx.new(|cx| ZedisSettingEditor::new(window, cx)),
    );
}
