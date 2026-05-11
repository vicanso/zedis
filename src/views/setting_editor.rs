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
    states::{FontSize, ZedisGlobalStore, i18n_settings, update_app_state_and_save},
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
    tray_enabled: bool,
    show_key_tree_ttl: bool,
    font_size_select: Entity<ZedisSelect>,
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
            if let Ok(value) = text.parse::<usize>()
                && value >= 1000
            {
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
                if let Ok(value) = text.parse::<usize>()
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

        let font_size_index = match font_size {
            FontSize::Large => 0,
            FontSize::Medium => 1,
            FontSize::Small => 2,
        };
        let font_size_items = vec![
            i18n_settings(cx, "font_size_large").to_string(),
            i18n_settings(cx, "font_size_medium").to_string(),
            i18n_settings(cx, "font_size_small").to_string(),
        ];
        let font_size_select = cx.new(|cx| ZedisSelect::new(font_size_items, Some(font_size_index), window, cx));

        subscriptions.push(cx.subscribe_in(
            &font_size_select,
            window,
            |_view, _select, event: &ZedisSelectEvent, _window, cx| {
                let ZedisSelectEvent::Change(index) = event;
                let font_size = match *index {
                    0 => Some(FontSize::Large),
                    2 => Some(FontSize::Small),
                    _ => None,
                };
                update_app_state_and_save(cx, "save_font_size", move |state, _| {
                    state.set_font_size(font_size);
                });
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
            tray_enabled,
            show_key_tree_ttl,
            font_size_select,
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
        if let Some(font_size) = cx.global::<ZedisGlobalStore>().read(cx).font_size().to_pixels() {
            window.set_rem_size(font_size);
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
                .child(Self::render_setting_row(cx, "font_size", self.font_size_select.clone()))
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
