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

use crate::views::secondary_window::{active_window_display, open_secondary_window};
use crate::{
    helpers::{DEFAULT_UI_FONT_SIZE, apply_fonts, get_or_create_config_dir, parse_duration},
    states::{
        ZedisGlobalStore, i18n_settings, update_app_state_and_save, update_app_state_and_save_debounced,
        update_app_state_and_save_quiet,
    },
};
use gpui::{
    App, Bounds, Entity, FontWeight, Subscription, TitlebarOptions, Window, WindowBounds, WindowOptions, prelude::*,
    px, size,
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

/// Build a font dropdown's `(labels, index→value, selected_index)`: a localized
/// default entry (value `None`, index 0) followed by the installed families. A
/// saved font missing from `fonts` (e.g. the config moved machines) is inserted
/// so it stays selectable and shown as current.
fn build_font_options(
    fonts: &[String],
    saved: &Option<String>,
    default_label: String,
) -> (Vec<String>, Vec<Option<String>>, Option<usize>) {
    let mut labels = vec![default_label];
    let mut values: Vec<Option<String>> = vec![None];
    for f in fonts {
        labels.push(f.clone());
        values.push(Some(f.clone()));
    }
    let selected = match saved {
        None => 0,
        Some(name) => match fonts.iter().position(|f| f == name) {
            Some(pos) => pos + 1,
            None => {
                labels.insert(1, name.clone());
                values.insert(1, Some(name.clone()));
                1
            }
        },
    };
    (labels, values, Some(selected))
}

pub struct ZedisSettingEditor {
    ui_font_select: Entity<ZedisSelect>,
    mono_font_select: Entity<ZedisSelect>,
    /// Index → value for each dropdown (index 0 = the "default" entry = `None`).
    ui_font_values: Vec<Option<String>>,
    mono_font_values: Vec<Option<String>>,
    /// Current selection, applied + persisted on change.
    ui_font: Option<String>,
    mono_font: Option<String>,
    max_key_tree_depth_state: Entity<InputState>,
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
    soft_delete: bool,
    sidebar_click_new_tab: bool,
    auto_update_check: bool,
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
        let auto_expand_threshold = store.auto_expand_threshold();
        let max_truncate_length = store.max_truncate_length();
        let redis_connection_timeout = store.redis_connection_timeout();
        let redis_response_timeout = store.redis_response_timeout();
        let key_scan_count = store.key_scan_count();
        let tray_enabled = store.tray_enabled();
        let show_key_tree_ttl = store.show_key_tree_ttl();
        let soft_delete = store.soft_delete();
        let sidebar_click_new_tab = store.sidebar_click_new_tab();
        let auto_update_check = store.auto_update_check();
        let font_rem = store.font_rem_px().unwrap_or(DEFAULT_UI_FONT_SIZE);
        let ui_font = store.ui_font_family();
        let mono_font = store.mono_font_family();
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

        // Continuous font size (rem px) via a slider, 12–22px. The state
        // updates per change so reads stay live, but the disk write and the
        // app-wide refresh are debounced until the thumb settles;
        // `cx.notify()` re-renders the row so the px readout tracks the
        // thumb live.
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
                    update_app_state_and_save_debounced(cx, "save_font_size", move |state, _| {
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

        Self {
            _subscriptions: subscriptions,
            ui_font_select,
            mono_font_select,
            ui_font_values,
            mono_font_values,
            ui_font,
            mono_font,
            key_scan_count_state,
            config_dir_state,
            auto_expand_threshold_state,
            max_truncate_length_state,
            max_key_tree_depth_state,
            redis_response_timeout_state,
            redis_connection_timeout_state,
            ai_base_url_state,
            ai_api_key_state,
            ai_model_state,
            tray_enabled,
            show_key_tree_ttl,
            soft_delete,
            sidebar_click_new_tab,
            auto_update_check,
            font_size_slider,
            locale_select,
        }
    }

    /// Apply the current font selections live (Theme + mono global) and persist
    /// them. Both are sent together so a change to one keeps the other's value.
    fn apply_and_save_fonts(&self, cx: &mut Context<Self>) {
        let ui = self.ui_font.clone();
        let mono = self.mono_font.clone();
        apply_fonts(cx, ui.as_deref(), mono.as_deref());
        update_app_state_and_save(cx, "save_fonts", move |state, _| {
            state.set_ui_font_family(ui.clone());
            state.set_mono_font_family(mono.clone());
        });
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
                // `min_w_0` lets the text column shrink below its content
                // width so a long description wraps instead of squeezing the
                // control column out of the row (default flex `min-width:
                // auto` — same overflow the font-size slider row hit).
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_0p5()
                    .child(Label::new(i18n_settings(cx, label_key)).text_sm())
                    .child(Label::new(i18n_settings(cx, &desc_key)).text_xs().text_color(muted)),
            )
            // Right-align the control column so small controls (Switch) sit
            // flush right; full-width Input/Select already fill the 200px box.
            // `flex_none` guarantees the 200px against a long description.
            .child(h_flex().w(px(200.)).flex_none().justify_end().child(input_element))
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
                .child(Self::render_setting_row(cx, "ui_font", self.ui_font_select.clone()))
                .child(Self::render_setting_row(cx, "mono_font", self.mono_font_select.clone()))
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
                    "soft_delete",
                    Switch::new("soft-delete")
                        .checked(self.soft_delete)
                        .on_click(cx.listener(|this, checked: &bool, _window, cx| {
                            this.soft_delete = *checked;
                            let enabled = *checked;
                            update_app_state_and_save(cx, "save_soft_delete", move |state, _| {
                                state.set_soft_delete(enabled);
                            });
                        })),
                ))
                .child(Self::render_setting_row(
                    cx,
                    "max_truncate_length",
                    Input::new(&self.max_truncate_length_state),
                ))
                // — Workspace Tabs —
                .child(Self::render_section_header(cx, "section_tabs", "section_tabs_desc"))
                .child(Self::render_setting_row(
                    cx,
                    "sidebar_click_new_tab",
                    Switch::new("sidebar-click-new-tab")
                        .checked(self.sidebar_click_new_tab)
                        .on_click(cx.listener(|this, checked: &bool, _window, cx| {
                            this.sidebar_click_new_tab = *checked;
                            let enabled = *checked;
                            update_app_state_and_save(cx, "save_sidebar_click_new_tab", move |state, _| {
                                state.set_sidebar_click_new_tab(enabled);
                            });
                        })),
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
                    "auto_update_check",
                    Switch::new("auto-update-check")
                        .checked(self.auto_update_check)
                        .on_click(cx.listener(|this, checked: &bool, _window, cx| {
                            this.auto_update_check = *checked;
                            let enabled = *checked;
                            update_app_state_and_save(cx, "save_auto_update_check", move |state, _| {
                                state.set_auto_update_check(enabled);
                            });
                        })),
                ))
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
    // Center on the monitor the main window is on (not always the primary).
    let display = active_window_display(cx);
    open_secondary_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(display, window_size, cx))),
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
