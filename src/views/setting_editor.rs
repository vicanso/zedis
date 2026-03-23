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
    helpers::{get_or_create_config_dir, parse_duration},
    states::{FontSize, ZedisGlobalStore, i18n_settings, update_app_state_and_save},
};
use gpui::{Entity, Subscription, Window, prelude::*, px};
use gpui_component::{
    checkbox::Checkbox,
    form::{Field, field, v_form},
    input::{Input, InputEvent, InputState, NumberInput, NumberInputEvent, StepAction},
    label::Label,
    radio::RadioGroup,
    v_flex,
};

pub struct ZedisSettingEditor {
    max_key_tree_depth_state: Entity<InputState>,
    key_separator_state: Entity<InputState>,
    max_truncate_length_state: Entity<InputState>,
    config_dir_state: Entity<InputState>,
    key_scan_count_state: Entity<InputState>,
    auto_expand_threshold_state: Entity<InputState>,
    redis_connection_timeout_state: Entity<InputState>,
    redis_response_timeout_state: Entity<InputState>,
    tray_enabled: bool,
    font_size: FontSize,
    locale: String,
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
        let font_size = store.font_size();
        let locale = store.locale().to_string();
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

        // Redis Connection Timeout
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
        // Redis Response Timeout
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

        // Key Separator
        subscriptions.push(Self::bind_blur_save(cx, &key_separator_state, window, |text, cx| {
            update_app_state_and_save(cx, "save_key_separator", move |state, _| {
                state.set_key_separator(text);
            });
        }));

        // Key Scan Count
        subscriptions.push(Self::bind_blur_save(cx, &key_scan_count_state, window, |text, cx| {
            if let Ok(value) = text.parse::<usize>()
                && value >= 1000
            {
                update_app_state_and_save(cx, "save_key_scan_count", move |state, _| {
                    state.set_key_scan_count(value);
                });
            }
        }));
        // Auto Expand Threshold
        subscriptions.push(Self::bind_blur_save(
            cx,
            &auto_expand_threshold_state,
            window,
            |text, cx| {
                if let Ok(value) = text.parse::<usize>()
                    && value >= 100
                {
                    update_app_state_and_save(cx, "save_auto_expand_threshold", move |state, _| {
                        state.set_auto_expand_threshold(value);
                    });
                }
            },
        ));

        // Max Truncate Length
        subscriptions.push(Self::bind_blur_save(
            cx,
            &max_truncate_length_state,
            window,
            |text, cx| {
                if let Ok(value) = text.parse::<usize>()
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
            tray_enabled,
            font_size,
            locale,
        }
    }
    fn render_field(cx: &Context<Self>, label_key: &str, input_element: impl IntoElement) -> Field {
        field().label(i18n_settings(cx, label_key)).child(input_element)
    }
}

impl Render for ZedisSettingEditor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let cols = if window.viewport_size().width < px(800.) { 1 } else { 2 };

        let font_size_index = match self.font_size {
            FontSize::Large => 0,
            FontSize::Medium => 1,
            FontSize::Small => 2,
        };
        let locale_index = match self.locale.as_str() {
            "zh" => 0,
            "en" => 1,
            "ja" => 2,
            "ru" => 3,
            "pt" => 4,
            "de" => 5,
            "fr" => 6,
            "es" => 7,
            _ => 1,
        };

        v_flex()
            .p_5()
            .child(Label::new(i18n_settings(cx, "title")).text_3xl().mb_2())
            .child(
                v_form()
                    .flex_1()
                    .columns(cols)
                    .child(
                        field().label(i18n_settings(cx, "font_size")).child(
                            RadioGroup::horizontal("font-size-group")
                                .mt(px(8.))
                                .children(vec![
                                    i18n_settings(cx, "font_size_large"),
                                    i18n_settings(cx, "font_size_medium"),
                                    i18n_settings(cx, "font_size_small"),
                                ])
                                .selected_index(Some(font_size_index))
                                .on_click(cx.listener(|this, index: &usize, _window, cx| {
                                    let font_size = match *index {
                                        0 => Some(FontSize::Large),
                                        2 => Some(FontSize::Small),
                                        _ => None,
                                    };
                                    this.font_size = font_size.unwrap_or(FontSize::Medium);
                                    update_app_state_and_save(cx, "save_font_size", move |state, _| {
                                        state.set_font_size(font_size);
                                    });
                                })),
                        ),
                    )
                    .child(
                        field().col_span(cols as u16).label(i18n_settings(cx, "lang")).child(
                            RadioGroup::horizontal("locale-group")
                                .mt(px(8.))
                                .children(vec![
                                    "English",
                                    "中文",
                                    "Русский",
                                    "日本語",
                                    "Português",
                                    "Español",
                                    "Deutsch",
                                    "Français",
                                ])
                                .selected_index(Some(locale_index))
                                .on_click(cx.listener(|this, index: &usize, _window, cx| {
                                    let locale = match *index {
                                        1 => "zh",
                                        2 => "ru",
                                        3 => "ja",
                                        4 => "pt",
                                        5 => "es",
                                        6 => "de",
                                        7 => "fr",
                                        _ => "en",
                                    };
                                    this.locale = locale.to_string();
                                    update_app_state_and_save(cx, "save_locale", move |state, _| {
                                        state.set_locale(locale.to_string());
                                    });
                                })),
                        ),
                    )
                    .child(Self::render_field(
                        cx,
                        "max_key_tree_depth",
                        NumberInput::new(&self.max_key_tree_depth_state),
                    ))
                    .child(Self::render_field(
                        cx,
                        "key_separator",
                        Input::new(&self.key_separator_state),
                    ))
                    .child(Self::render_field(
                        cx,
                        "key_scan_count",
                        Input::new(&self.key_scan_count_state),
                    ))
                    .child(Self::render_field(
                        cx,
                        "auto_expand_threshold",
                        Input::new(&self.auto_expand_threshold_state),
                    ))
                    .child(Self::render_field(
                        cx,
                        "max_truncate_length",
                        Input::new(&self.max_truncate_length_state),
                    ))
                    .child(Self::render_field(
                        cx,
                        "redis_connection_timeout",
                        Input::new(&self.redis_connection_timeout_state),
                    ))
                    .child(Self::render_field(
                        cx,
                        "redis_response_timeout",
                        Input::new(&self.redis_response_timeout_state),
                    ))
                    .when(cfg!(not(target_os = "linux")), |this| {
                        this.child(
                            field().label(i18n_settings(cx, "tray_enabled")).child(
                                Checkbox::new("tray-enabled")
                                    .label(i18n_settings(cx, "tray_enabled_label"))
                                    .checked(self.tray_enabled)
                                    .mt(px(7.))
                                    .on_click(cx.listener(|this, checked: &bool, _window, cx| {
                                        this.tray_enabled = *checked;
                                        let enabled = *checked;
                                        update_app_state_and_save(cx, "save_tray_enabled", move |state, _| {
                                            state.set_tray_enabled(enabled);
                                        });
                                    })),
                            ),
                        )
                    })
                    .child(
                        field()
                            .col_span(cols as u16)
                            .label(i18n_settings(cx, "config_dir"))
                            .child(Input::new(&self.config_dir_state).disabled(true)),
                    ),
            )
    }
}
