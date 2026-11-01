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

#[cfg(not(target_family = "wasm"))]
use super::export::dirs_default_directory;
#[cfg(not(target_family = "wasm"))]
use crate::helpers::{
    ensure_keybindings_file, export_local_data_json, get_or_create_config_dir, import_local_data_file,
    is_app_store_build, is_valid_proxy_setting, parse_duration, write_local_data_file,
};
#[cfg(not(target_family = "wasm"))]
use crate::states::update_app_state_and_save_quiet;
#[cfg(not(target_family = "wasm"))]
use crate::views::secondary_window::{active_window_display, open_secondary_window};
use crate::{
    connection::{DEFAULT_KEY_SCAN_COUNT, MAX_KEY_SCAN_COUNT, MIN_KEY_SCAN_COUNT},
    helpers::{
        DATE_FORMATS, DEFAULT_UI_FONT_SIZE, TimeZonePref, UI_FONT_SIZE_MAX, UI_FONT_SIZE_MIN, apply_fonts,
        date_format_sample, set_datetime_prefs,
    },
    states::{JsonView, ZedisGlobalStore, i18n_editor, i18n_settings, save_ui_locale, update_app_state_and_save},
};
#[cfg(not(target_family = "wasm"))]
use gpui::PathPromptOptions;
#[cfg(target_family = "wasm")]
use gpui::div;
use gpui::{AnyElement, App, Entity, FocusHandle, FontWeight, Subscription, Window, prelude::*, px};
#[cfg(not(target_family = "wasm"))]
use gpui::{Bounds, TitlebarOptions, WindowBounds, WindowOptions, size};
use gpui_kit::component::{
    ActiveTheme, WindowExt, h_flex,
    input::{Input, InputEvent, InputState, NumberInput, NumberInputEvent, StepAction},
    label::Label,
    list::ListItem,
    scroll::ScrollableElement,
    switch::Switch,
    v_flex,
};
#[cfg(not(target_family = "wasm"))]
use gpui_kit::component::{Sizable, button::Button, notification::Notification};
#[cfg(not(target_family = "wasm"))]
use rust_i18n::t;
#[cfg(not(target_family = "wasm"))]
use tracing::{error, warn};
#[cfg(target_family = "wasm")]
use zedis_ui::ZedisDialog;
use zedis_ui::{ZedisSelect, ZedisSelectEvent};

mod fields;

#[cfg(not(target_family = "wasm"))]
use fields::{AiFields, RedisFields, SystemFields};
use fields::{AppearanceFields, DateTimeFields, KeyBehaviorFields};

/// The Settings dialog of the browser build: as wide as the desktop's
/// window plus the dialog's own padding, as tall as that window when the
/// page has the room and never shorter than a few rows.
#[cfg(target_family = "wasm")]
const SETTINGS_DIALOG_WIDTH: f32 = 760.;

/// The recycle-bin row's wording: a local copy on the desktop…
#[cfg(not(target_family = "wasm"))]
const SOFT_DELETE_ROW: &str = "soft_delete";
/// …a copy on the bridge, which everyone who sees the server sees, in the
/// browser.
#[cfg(target_family = "wasm")]
const SOFT_DELETE_ROW: &str = "soft_delete_web";
#[cfg(target_family = "wasm")]
const SETTINGS_DIALOG_MAX_HEIGHT: f32 = 520.;
#[cfg(target_family = "wasm")]
const SETTINGS_DIALOG_MIN_HEIGHT: f32 = 280.;
/// What the dialog needs around its body: its title, its paddings and a
/// margin to the page's edges.
#[cfg(target_family = "wasm")]
const SETTINGS_DIALOG_CHROME: f32 = 160.;

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

/// Width of the section list down the left of the window.
const SECTION_NAV_WIDTH: f32 = 160.;

/// A typed value is applied only when it is already a whole number inside
/// the range, so a partial `"2"` (on the way to `"20"`) is ignored.
fn parse_font_rem_px(text: &str) -> Option<f32> {
    let n: i32 = text.trim().parse().ok()?;
    (UI_FONT_SIZE_MIN..=UI_FONT_SIZE_MAX)
        .contains(&(n as f32))
        .then_some(n as f32)
}

/// On blur: keep an in-range whole number, round a numeric out-of-range or
/// fractional value, otherwise restore `fallback` (the last saved size).
fn clamp_font_rem_px(text: &str, fallback: f32) -> f32 {
    if let Some(px) = parse_font_rem_px(text) {
        return px;
    }
    text.trim()
        .parse::<f32>()
        .ok()
        .map(|n| n.round().clamp(UI_FONT_SIZE_MIN, UI_FONT_SIZE_MAX))
        .unwrap_or(fallback)
}

fn persist_font_size(cx: &mut Context<ZedisSettingEditor>, px: f32) {
    update_app_state_and_save(cx, "save_font_size", move |app, _| {
        app.set_font_rem_px(Some(px));
    });
}

/// The groups the settings are in, in the order the window lists them.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
enum SettingsSection {
    #[default]
    Appearance,
    DateTime,
    KeyBehavior,
    Tabs,
    #[cfg(not(target_family = "wasm"))]
    Redis,
    #[cfg(not(target_family = "wasm"))]
    Ai,
    #[cfg(not(target_family = "wasm"))]
    System,
    #[cfg(not(target_family = "wasm"))]
    LocalData,
}

impl SettingsSection {
    /// The i18n key of its title (`settings.` section); its description is
    /// the same key with `_desc`.
    fn title_key(self) -> &'static str {
        match self {
            Self::Appearance => "section_appearance",
            Self::DateTime => "section_datetime",
            Self::KeyBehavior => "section_key_behavior",
            Self::Tabs => "section_tabs",
            #[cfg(not(target_family = "wasm"))]
            Self::Redis => "section_redis",
            #[cfg(not(target_family = "wasm"))]
            Self::Ai => "section_ai",
            #[cfg(not(target_family = "wasm"))]
            Self::System => "section_system",
            #[cfg(not(target_family = "wasm"))]
            Self::LocalData => "section_local_data",
        }
    }
}

/// The rows of one section, picked out of the page's builder chain.
///
/// `render` still declares every section in one chain, top to bottom, as it
/// did when the page was one long column: `.section(…)` starts a section,
/// the `.child(…)` / `.when(…)` after it are its rows. This keeps the rows of
/// the `active` one and drops the rest, and remembers which sections went by
/// — on a target that leaves one out, the list on the left leaves it out too.
struct SectionRows {
    active: SettingsSection,
    current: Option<SettingsSection>,
    sections: Vec<SettingsSection>,
    rows: Vec<AnyElement>,
}

impl SectionRows {
    fn new(active: SettingsSection) -> Self {
        Self {
            active,
            current: None,
            sections: Vec::new(),
            rows: Vec::new(),
        }
    }

    /// Start `section`: the rows that follow are its own.
    fn section(mut self, cx: &Context<ZedisSettingEditor>, section: SettingsSection) -> Self {
        self.current = Some(section);
        self.sections.push(section);
        self.child(ZedisSettingEditor::render_section_header(cx, section))
    }
}

impl ParentElement for SectionRows {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        if self.current == Some(self.active) {
            self.rows.extend(elements);
        }
    }
}

impl FluentBuilder for SectionRows {}

/// Where the settings are kept, for the System section's read-only row.
#[cfg(not(target_family = "wasm"))]
fn config_dir_text() -> String {
    match get_or_create_config_dir() {
        Ok(dir) => dir.to_string_lossy().to_string(),
        Err(e) => {
            warn!(error = %e, "config directory unavailable");
            String::new()
        }
    }
}

/// Whether a proxy typed into Settings may be saved: only one `app_proxy`
/// can act on.
#[cfg(not(target_family = "wasm"))]
fn proxy_setting_accepted(text: &str) -> bool {
    is_valid_proxy_setting(text)
}

pub struct ZedisSettingEditor {
    /// The section whose settings are on screen.
    section: SettingsSection,
    /// Where the focus goes when the section changes. The field that held
    /// it leaves with its section, and a focus on nothing has no path to
    /// whatever contains this view: in the browser's dialog Escape then
    /// closed nothing, having never reached the dialog.
    focus_handle: FocusHandle,
    /// The language the lists and fields below were built in. Their
    /// entries and placeholders are strings made in `new` — the font
    /// lists' "default" entry, the time zones' names — so a language
    /// change, made in this very view, builds them again (see `render`).
    built_in: String,
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
    #[cfg(not(target_family = "wasm"))]
    config_dir_state: Entity<InputState>,
    key_scan_count_state: Entity<InputState>,
    value_search_scan_cap_state: Entity<InputState>,
    value_search_time_budget_state: Entity<InputState>,
    value_search_max_matches_state: Entity<InputState>,
    auto_expand_threshold_state: Entity<InputState>,
    #[cfg(not(target_family = "wasm"))]
    redis_connection_timeout_state: Entity<InputState>,
    #[cfg(not(target_family = "wasm"))]
    redis_response_timeout_state: Entity<InputState>,
    #[cfg(not(target_family = "wasm"))]
    ai_base_url_state: Entity<InputState>,
    #[cfg(not(target_family = "wasm"))]
    ai_api_key_state: Entity<InputState>,
    #[cfg(not(target_family = "wasm"))]
    ai_model_state: Entity<InputState>,
    #[cfg(not(target_family = "wasm"))]
    http_proxy_state: Entity<InputState>,
    #[cfg(not(target_family = "wasm"))]
    tray_enabled: bool,
    show_key_tree_ttl: bool,
    soft_delete: bool,
    sidebar_click_new_tab: bool,
    #[cfg(not(target_family = "wasm"))]
    auto_update_check: bool,
    #[cfg(not(target_family = "wasm"))]
    update_prerelease: bool,
    font_size_state: Entity<InputState>,
    locale_select: Entity<ZedisSelect>,
    time_zone_select: Entity<ZedisSelect>,
    date_format_select: Entity<ZedisSelect>,
    json_view_select: Entity<ZedisSelect>,
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
        #[cfg(not(target_family = "wasm"))]
        let tray_enabled = store.tray_enabled();
        let show_key_tree_ttl = store.show_key_tree_ttl();
        let soft_delete = store.soft_delete();
        let sidebar_click_new_tab = store.sidebar_click_new_tab();
        #[cfg(not(target_family = "wasm"))]
        let auto_update_check = store.auto_update_check();
        #[cfg(not(target_family = "wasm"))]
        let update_prerelease = store.update_prerelease();
        let mut subscriptions = Vec::new();
        let AppearanceFields {
            font_size_state,
            locale_select,
            ui_font_select,
            mono_font_select,
            ui_font_values,
            mono_font_values,
            ui_font,
            mono_font,
        } = Self::appearance_fields(window, cx, &mut subscriptions);
        let DateTimeFields {
            time_zone_select,
            date_format_select,
        } = Self::date_time_fields(window, cx, &mut subscriptions);
        let KeyBehaviorFields {
            max_key_tree_depth_state,
            key_scan_count_state,
            auto_expand_threshold_state,
            max_truncate_length_state,
            value_search_scan_cap_state,
            value_search_time_budget_state,
            value_search_max_matches_state,
            json_view_select,
        } = Self::key_behavior_fields(window, cx, &mut subscriptions);
        #[cfg(not(target_family = "wasm"))]
        let RedisFields {
            redis_connection_timeout_state,
            redis_response_timeout_state,
        } = Self::redis_fields(window, cx, &mut subscriptions);
        #[cfg(not(target_family = "wasm"))]
        let AiFields {
            ai_base_url_state,
            ai_api_key_state,
            ai_model_state,
        } = Self::ai_fields(window, cx, &mut subscriptions);
        #[cfg(not(target_family = "wasm"))]
        let SystemFields {
            http_proxy_state,
            config_dir_state,
        } = Self::system_fields(window, cx, &mut subscriptions);

        Self {
            section: SettingsSection::default(),
            focus_handle: cx.focus_handle(),
            built_in: cx.global::<ZedisGlobalStore>().read(cx).locale().to_string(),
            _subscriptions: subscriptions,
            ui_font_select,
            mono_font_select,
            ui_font_values,
            mono_font_values,
            ui_font,
            mono_font,
            key_scan_count_state,
            value_search_scan_cap_state,
            value_search_time_budget_state,
            value_search_max_matches_state,
            #[cfg(not(target_family = "wasm"))]
            config_dir_state,
            auto_expand_threshold_state,
            max_truncate_length_state,
            max_key_tree_depth_state,
            #[cfg(not(target_family = "wasm"))]
            redis_response_timeout_state,
            #[cfg(not(target_family = "wasm"))]
            redis_connection_timeout_state,
            #[cfg(not(target_family = "wasm"))]
            ai_base_url_state,
            #[cfg(not(target_family = "wasm"))]
            ai_api_key_state,
            #[cfg(not(target_family = "wasm"))]
            ai_model_state,
            #[cfg(not(target_family = "wasm"))]
            http_proxy_state,
            #[cfg(not(target_family = "wasm"))]
            tray_enabled,
            show_key_tree_ttl,
            soft_delete,
            sidebar_click_new_tab,
            #[cfg(not(target_family = "wasm"))]
            auto_update_check,
            #[cfg(not(target_family = "wasm"))]
            update_prerelease,
            font_size_state,
            locale_select,
            time_zone_select,
            date_format_select,
            json_view_select,
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

    /// The recycle-bin switch.
    fn render_soft_delete_row(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let switch = Switch::new("soft-delete")
            .checked(self.soft_delete)
            .on_click(cx.listener(|this, checked: &bool, _window, cx| {
                this.soft_delete = *checked;
                let enabled = *checked;
                update_app_state_and_save(cx, "save_soft_delete", move |state, _| {
                    state.set_soft_delete(enabled);
                });
            }));
        Some(Self::render_setting_row(cx, SOFT_DELETE_ROW, switch).into_any_element())
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
                // auto`).
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

    fn render_section_header(cx: &Context<Self>, section: SettingsSection) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let title_key = section.title_key();
        v_flex()
            .w_full()
            .gap_1()
            .pt_5()
            .pb_4()
            .child(
                Label::new(i18n_settings(cx, title_key))
                    .text_sm()
                    .font_weight(FontWeight::BOLD),
            )
            .child(
                Label::new(i18n_settings(cx, &format!("{title_key}_desc")))
                    .text_xs()
                    .text_color(muted),
            )
    }

    /// The window: the sections down the left, the selected one's settings
    /// on the right. All of them used to be one column five screens long.
    fn render_sections(&self, page: SectionRows, cx: &mut Context<Self>) -> impl IntoElement {
        let active = page.active;
        let nav = v_flex()
            .flex_none()
            .w(px(SECTION_NAV_WIDTH))
            .h_full()
            .p_2()
            .gap_0p5()
            .border_r_1()
            .border_color(cx.theme().border)
            .children(page.sections.iter().map(|&section| {
                ListItem::new(section.title_key())
                    .selected(section == active)
                    .child(Label::new(i18n_settings(cx, section.title_key())).text_sm())
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.section = section;
                        this.focus_handle.focus(window, cx);
                        cx.notify();
                    }))
            }));
        h_flex()
            .track_focus(&self.focus_handle)
            .size_full()
            .items_start()
            .child(nav)
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .px_6()
                    .pb_4()
                    .children(page.rows)
                    .overflow_y_scrollbar(),
            )
    }
}

impl Render for ZedisSettingEditor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(rem) = cx.global::<ZedisGlobalStore>().read(cx).font_rem_px() {
            window.set_rem_size(rem);
        }
        // The language changed under what `new` built: build it again, on
        // the section the user is looking at. The focus was in the list the
        // change was made from, which goes with the old build.
        if cx.global::<ZedisGlobalStore>().read(cx).locale() != self.built_in {
            let section = self.section;
            *self = Self::new(window, cx);
            self.section = section;
            self.focus_handle.focus(window, cx);
        }

        let page = SectionRows::new(self.section)
            // — Appearance —
            .section(cx, SettingsSection::Appearance)
            .child(Self::render_setting_row(
                cx,
                "font_size",
                NumberInput::new(&self.font_size_state),
            ))
            .child(Self::render_setting_row(cx, "ui_font", self.ui_font_select.clone()))
            .child(Self::render_setting_row(cx, "mono_font", self.mono_font_select.clone()))
            .child(Self::render_setting_row(cx, "lang", self.locale_select.clone()))
            // — Date & time —
            .section(cx, SettingsSection::DateTime)
            .child(Self::render_setting_row(cx, "time_zone", self.time_zone_select.clone()))
            .child(Self::render_setting_row(
                cx,
                "date_format",
                self.date_format_select.clone(),
            ))
            // — Key Behavior —
            .section(cx, SettingsSection::KeyBehavior)
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
            .child(Self::render_setting_row(cx, "json_view", self.json_view_select.clone()))
            .children(self.render_soft_delete_row(cx))
            .child(Self::render_setting_row(
                cx,
                "max_truncate_length",
                Input::new(&self.max_truncate_length_state),
            ))
            .child(Self::render_setting_row(
                cx,
                "value_search_scan_cap",
                Input::new(&self.value_search_scan_cap_state),
            ))
            .child(Self::render_setting_row(
                cx,
                "value_search_time_budget",
                Input::new(&self.value_search_time_budget_state),
            ))
            .child(Self::render_setting_row(
                cx,
                "value_search_max_matches",
                Input::new(&self.value_search_max_matches_state),
            ))
            // — Workspace Tabs —
            .section(cx, SettingsSection::Tabs)
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
            ));
        // The four sections below are the installed application's: how it
        // dials Redis (in the browser the bridge dials, with its own
        // timeouts), the assistant's HTTP endpoint (the page has no HTTP
        // client for it), the tray / updater / proxy / config directory, and
        // the backup file. A page lists none of them — a section that is
        // never started is not in the list on the left either.
        #[cfg(not(target_family = "wasm"))]
        let page = page
            // — Redis Connection —
            .section(cx, SettingsSection::Redis)
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
            .section(cx, SettingsSection::Ai)
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
            .section(cx, SettingsSection::System)
            .child(Self::render_setting_row(
                cx,
                "http_proxy",
                Input::new(&self.http_proxy_state),
            ))
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
            // App Store builds update via the App Store — the toggle would
            // control a check that `check_for_updates` refuses to run, so
            // hide it alongside the menu entries.
            .when(!is_app_store_build(), |this| {
                this.child(Self::render_setting_row(
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
                    "update_prerelease",
                    Switch::new("update-prerelease")
                        .checked(self.update_prerelease)
                        .on_click(cx.listener(|this, checked: &bool, _window, cx| {
                            this.update_prerelease = *checked;
                            let enabled = *checked;
                            update_app_state_and_save(cx, "save_update_prerelease", move |state, _| {
                                state.set_update_prerelease(enabled);
                            });
                        })),
                ))
            })
            .child(Self::render_setting_row(
                cx,
                "config_dir",
                Input::new(&self.config_dir_state).disabled(true),
            ))
            // Linux registers the scheme through the desktop entry and
            // Windows through the installer; only macOS has a runtime
            // hook (Launch Services) worth a button.
            .when(cfg!(target_os = "macos"), |this| {
                this.child(Self::render_setting_row(
                    cx,
                    "url_scheme",
                    Button::new("register-url-scheme")
                        .small()
                        .outline()
                        .label(i18n_settings(cx, "url_scheme_button"))
                        .on_click(cx.listener(|this, _, window, cx| this.register_url_scheme(window, cx))),
                ))
            })
            .child(Self::render_setting_row(
                cx,
                "keybindings_file",
                Button::new("edit-keybindings")
                    .small()
                    .outline()
                    .label(i18n_settings(cx, "keybindings_edit"))
                    .on_click(cx.listener(|_this, _, window, cx| Self::open_keybindings_file(window, cx))),
            ))
            // — Local data —
            .section(cx, SettingsSection::LocalData)
            .child(Self::render_setting_row(
                cx,
                "local_data_export",
                Button::new("export-local-data")
                    .small()
                    .outline()
                    .label(i18n_settings(cx, "local_data_export_button"))
                    .on_click(cx.listener(|this, _, window, cx| this.export_local_data(window, cx))),
            ))
            .child(Self::render_setting_row(
                cx,
                "local_data_import",
                Button::new("import-local-data")
                    .small()
                    .outline()
                    .label(i18n_settings(cx, "local_data_import_button"))
                    .on_click(cx.listener(|this, _, window, cx| this.import_local_data(window, cx))),
            ));
        self.render_sections(page, cx)
    }
}

/// What the desktop-only rows do: files, the OS default editor, Launch
/// Services. None of it is in the browser build, where their sections are
/// not listed (see `render`).
#[cfg(not(target_family = "wasm"))]
impl ZedisSettingEditor {
    fn notify_error(window: &mut Window, cx: &mut App, key: &str, error: &str) {
        let locale = cx.global::<ZedisGlobalStore>().read(cx).locale().to_string();
        let message = t!(key, error = error, locale = &locale).to_string();
        window.push_notification(Notification::error(message), cx);
    }

    /// Create `keybindings.toml` from the template when it does not exist
    /// yet and hand it to the OS default editor. Changes bind on the next
    /// launch (the row's description says so).
    fn open_keybindings_file(window: &mut Window, cx: &mut App) {
        match ensure_keybindings_file() {
            Ok(path) => cx.open_with_system(&path),
            Err(e) => {
                error!(error = %e, "keybindings file could not be created");
                Self::notify_error(window, cx, "settings.keybindings_failed", &e.to_string());
            }
        }
    }

    /// Make Zedis the handler for `redis://` / `rediss://` links (Launch
    /// Services on macOS). The bundle already declares the schemes; this
    /// only matters when another client claimed them.
    fn register_url_scheme(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let tasks = [cx.register_url_scheme("redis"), cx.register_url_scheme("rediss")];
        cx.spawn_in(window, async move |this, cx| {
            let mut failure = None;
            for task in tasks {
                if let Err(e) = task.await {
                    failure = Some(e.to_string());
                }
            }
            let _ = this.update_in(cx, |_this, window, cx| match failure {
                None => {
                    let message = i18n_settings(cx, "url_scheme_registered");
                    window.push_notification(Notification::success(message), cx);
                }
                Some(error) => {
                    error!(error = %error, "url scheme registration failed");
                    Self::notify_error(window, cx, "settings.url_scheme_failed", &error);
                }
            });
        })
        .detach();
    }

    fn export_local_data(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (name, json) = match export_local_data_json() {
            Ok(v) => v,
            Err(e) => {
                error!(error = %e, "local data export failed");
                Self::notify_error(window, cx, "settings.local_data_failed", &e.to_string());
                return;
            }
        };
        let receiver = cx.prompt_for_new_path(&dirs_default_directory(), Some(&name));
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(path))) = receiver.await else {
                return;
            };
            let result = cx
                .background_spawn(async move { write_local_data_file(&path, &json).map(|()| path) })
                .await;
            let _ = this.update_in(cx, |_this, window, cx| match result {
                Ok(path) => {
                    let locale = cx.global::<ZedisGlobalStore>().read(cx).locale().to_string();
                    let message = t!(
                        "settings.local_data_exported",
                        path = path.display().to_string(),
                        locale = &locale
                    )
                    .to_string();
                    window.push_notification(Notification::success(message), cx);
                    cx.reveal_path(&path);
                }
                Err(e) => {
                    error!(error = %e, "local data export failed");
                    Self::notify_error(window, cx, "settings.local_data_failed", &e.to_string());
                }
            });
        })
        .detach();
    }

    /// Pick a backup file and merge it into the store. The picker is
    /// async; the merge itself is a few redb writes and runs on the
    /// foreground so the managers' caches stay coherent.
    fn import_local_data(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: None,
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = receiver.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            let _ = this.update_in(cx, |_this, window, cx| match import_local_data_file(&path) {
                Ok(summary) => {
                    let locale = cx.global::<ZedisGlobalStore>().read(cx).locale().to_string();
                    let message = t!(
                        "settings.local_data_imported",
                        tags = summary.key_metadata,
                        favorites = summary.favorites,
                        viewers = summary.script_viewers,
                        scripts = summary.lua_scripts,
                        protos = summary.protos,
                        locale = &locale
                    )
                    .to_string();
                    if summary.skipped > 0 {
                        window.push_notification(Notification::warning(message), cx);
                    } else {
                        window.push_notification(Notification::success(message), cx);
                    }
                }
                Err(e) => {
                    error!(error = %e, path = %path.display(), "local data import failed");
                    Self::notify_error(window, cx, "settings.local_data_failed", &e.to_string());
                }
            });
        })
        .detach();
    }
}

/// Open Settings from the main window.
///
/// On the desktop it is a window of its own ([`open_settings_window`]).
#[cfg(not(target_family = "wasm"))]
pub fn open_settings(_window: &mut Window, cx: &mut App) {
    open_settings_window(cx);
}

/// In the browser it is a dialog over the page: a tab is one canvas, the
/// browser backend refuses a second window, and the Settings item used to
/// open nothing at all there. Same view, minus the sections that are about
/// the installed application (see `render`).
#[cfg(target_family = "wasm")]
pub fn open_settings(window: &mut Window, cx: &mut App) {
    // One at a time: the shortcut still reaches here with a dialog up, and a
    // second copy would stack on the first.
    if window.has_active_dialog(cx) {
        return;
    }
    // A dialog's body has to be given a height to scroll in; the page's own,
    // less room for the dialog's title and margins, up to the window's size
    // on the desktop.
    let height = (window.viewport_size().height - px(SETTINGS_DIALOG_CHROME))
        .min(px(SETTINGS_DIALOG_MAX_HEIGHT))
        .max(px(SETTINGS_DIALOG_MIN_HEIGHT));
    let view = cx.new(|cx| ZedisSettingEditor::new(window, cx));
    ZedisDialog::new(i18n_settings(cx, "title"))
        .w(px(SETTINGS_DIALOG_WIDTH))
        .child(move || div().w_full().h(height).child(view.clone()))
        .open(window, cx);
}

/// The Settings window of the desktop app (the tray opens it too, with no
/// window of its own to open it from).
#[cfg(not(target_family = "wasm"))]
pub fn open_settings_window(cx: &mut App) {
    let window_size = size(px(700.), px(560.));
    // Center on the monitor the main window is on (not always the primary).
    let display = active_window_display(cx);
    open_secondary_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(display, window_size, cx))),
            titlebar: Some(TitlebarOptions::default()),
            // Resizable down to where a row still has its text and its
            // control side by side.
            window_min_size: Some(size(px(700.), px(480.))),
            focus: true,
            ..Default::default()
        },
        // The lookup, not its answer: the language is changed in this
        // very window, and its title has to change with the page.
        |cx| i18n_settings(cx, "title"),
        cx,
        |window, cx| cx.new(|cx| ZedisSettingEditor::new(window, cx)),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::states::ZedisAppState;
    use crate::test_support::init_ui;
    use gpui::TestAppContext;
    use gpui_kit::component::Root;
    use gpui_kit::test::TestWindowExt;

    /// The desktop saves any proxy its own HTTP can use, and clearing the
    /// field; what it cannot act on is refused rather than kept.
    #[test]
    fn the_desktop_saves_a_proxy_it_can_use() {
        assert!(proxy_setting_accepted(""));
        assert!(proxy_setting_accepted("none"));
        assert!(proxy_setting_accepted("http://127.0.0.1:8080"));
        assert!(!proxy_setting_accepted("http:/"));
    }

    /// The page is built in one language — the lists' entries and the
    /// fields' placeholders are made in `new` — so a language change builds
    /// it again, and leaves the user on the section they were reading. The
    /// font lists went on saying "System default" over a German page.
    #[gpui::test]
    fn a_language_change_builds_the_page_again_on_the_same_section(cx: &mut TestAppContext) {
        init_ui(cx);
        let mut view = None;
        let handle = cx.open_window(size(px(700.), px(560.)), |window, cx| {
            let editor = cx.new(|cx| ZedisSettingEditor::new(window, cx));
            view = Some(editor.clone());
            Root::new(editor, window, cx)
        });
        let view = view.expect("the window built the page");
        let first_build = cx
            .update_window(handle.into(), |_, window, cx| {
                window.render_frame(cx);
                view.update(cx, |view, _| view.section = SettingsSection::System);
                view.read(cx).ui_font_select.entity_id()
            })
            .expect("the window is open");

        let other = if cx.update(|cx| cx.global::<ZedisGlobalStore>().read(cx).locale() == "de") {
            "en"
        } else {
            "de"
        };
        cx.update(|cx| {
            let store = cx.global::<ZedisGlobalStore>().clone();
            store.update(cx, |state, _| state.set_locale(other.to_string()));
            cx.refresh_windows();
        });
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            let view = view.read(cx);
            assert_eq!(view.built_in, other);
            assert_ne!(view.ui_font_select.entity_id(), first_build, "the lists are new ones");
            assert_eq!(view.section, SettingsSection::System);
        })
        .expect("the window is open");
    }

    fn open_settings(cx: &mut TestAppContext) -> (gpui::AnyWindowHandle, Entity<ZedisSettingEditor>) {
        let mut view = None;
        let handle = cx.open_window(size(px(700.), px(560.)), |window, cx| {
            let editor = cx.new(|cx| ZedisSettingEditor::new(window, cx));
            view = Some(editor.clone());
            Root::new(editor, window, cx)
        });
        (handle.into(), view.expect("the window built the page"))
    }

    type Field = fn(&ZedisSettingEditor) -> Entity<InputState>;

    /// Put `text` in a field and leave it, as a person does: the text
    /// changes, then the field loses the focus.
    fn enter(
        cx: &mut TestAppContext,
        handle: gpui::AnyWindowHandle,
        view: &Entity<ZedisSettingEditor>,
        field: Field,
        text: &str,
    ) {
        cx.update_window(handle, |_, window, cx| {
            let state = field(view.read(cx));
            state.update(cx, |input, cx| {
                input.set_value(text.to_string(), window, cx);
                cx.emit(InputEvent::Blur);
            });
        })
        .expect("the window is open");
        cx.run_until_parked();
    }

    fn shown(cx: &mut TestAppContext, view: &Entity<ZedisSettingEditor>, field: Field) -> String {
        cx.read(|cx| field(view.read(cx)).read(cx).value().to_string())
    }

    fn stored<T>(cx: &mut TestAppContext, read: impl FnOnce(&ZedisAppState) -> T) -> T {
        cx.read(|cx| read(cx.global::<ZedisGlobalStore>().read(cx)))
    }

    /// What a setting reads after `change`, from the defaults: the setter's
    /// own clamping, so a test asks only that the field reached it.
    fn after<T>(change: impl FnOnce(&mut ZedisAppState), read: impl FnOnce(&ZedisAppState) -> T) -> T {
        let mut state = ZedisAppState::default();
        change(&mut state);
        read(&state)
    }

    /// Every field starts from its setting and saves what it is left with,
    /// through its setter and its own rule: the scan count shown as the
    /// count the scan will use, a threshold under its floor not kept, a
    /// proxy that cannot be used put back to the one in effect, a font size
    /// set right on leaving.
    #[gpui::test]
    fn every_field_starts_from_its_setting_and_saves_when_left(cx: &mut TestAppContext) {
        init_ui(cx);
        let (handle, view) = open_settings(cx);
        let depth: Field = |v| v.max_key_tree_depth_state.clone();
        let scan_count: Field = |v| v.key_scan_count_state.clone();
        let connect: Field = |v| v.redis_connection_timeout_state.clone();
        assert_eq!(
            shown(cx, &view, depth),
            stored(cx, |s| s.max_key_tree_depth().to_string())
        );
        assert_eq!(
            shown(cx, &view, scan_count),
            stored(cx, |s| s.key_scan_count().to_string())
        );
        assert_eq!(shown(cx, &view, connect), stored(cx, |s| s.redis_connection_timeout()));

        enter(cx, handle, &view, depth, "7");
        assert_eq!(stored(cx, |s| s.max_key_tree_depth()), 7);

        enter(cx, handle, &view, scan_count, "1");
        assert_eq!(stored(cx, |s| s.key_scan_count()), MIN_KEY_SCAN_COUNT);
        assert_eq!(shown(cx, &view, scan_count), MIN_KEY_SCAN_COUNT.to_string());
        enter(cx, handle, &view, scan_count, "");
        assert_eq!(shown(cx, &view, scan_count), DEFAULT_KEY_SCAN_COUNT.to_string());
        assert_eq!(
            stored(cx, |s| s.key_scan_count()),
            after(|s| s.set_key_scan_count(0), |s| s.key_scan_count())
        );

        enter(cx, handle, &view, |v| v.value_search_scan_cap_state.clone(), "1234");
        assert_eq!(
            stored(cx, |s| s.value_search_scan_cap()),
            after(|s| s.set_value_search_scan_cap(1234), |s| s.value_search_scan_cap())
        );
        enter(cx, handle, &view, |v| v.value_search_time_budget_state.clone(), "9");
        assert_eq!(
            stored(cx, |s| s.value_search_time_budget_secs()),
            after(
                |s| s.set_value_search_time_budget_secs(9),
                |s| s.value_search_time_budget_secs()
            )
        );
        enter(cx, handle, &view, |v| v.value_search_max_matches_state.clone(), "77");
        assert_eq!(
            stored(cx, |s| s.value_search_max_matches()),
            after(|s| s.set_value_search_max_matches(77), |s| s.value_search_max_matches())
        );

        let expand: Field = |v| v.auto_expand_threshold_state.clone();
        let before = stored(cx, |s| s.auto_expand_threshold());
        enter(cx, handle, &view, expand, "50");
        assert_eq!(stored(cx, |s| s.auto_expand_threshold()), before, "under its floor");
        enter(cx, handle, &view, expand, "500");
        assert_eq!(stored(cx, |s| s.auto_expand_threshold()), 500);

        let truncate: Field = |v| v.max_truncate_length_state.clone();
        let before = stored(cx, |s| s.max_truncate_length());
        enter(cx, handle, &view, truncate, "5");
        assert_eq!(stored(cx, |s| s.max_truncate_length()), before, "under its floor");
        enter(cx, handle, &view, truncate, "80");
        assert_eq!(stored(cx, |s| s.max_truncate_length()), 80);

        enter(cx, handle, &view, connect, "15s");
        assert_eq!(
            stored(cx, |s| s.redis_connection_timeout()),
            after(
                |s| s.set_redis_connection_timeout(parse_duration("15s").ok()),
                |s| s.redis_connection_timeout()
            )
        );
        enter(cx, handle, &view, |v| v.redis_response_timeout_state.clone(), "20s");
        assert_eq!(
            stored(cx, |s| s.redis_response_timeout()),
            after(
                |s| s.set_redis_response_timeout(parse_duration("20s").ok()),
                |s| s.redis_response_timeout()
            )
        );

        enter(
            cx,
            handle,
            &view,
            |v| v.ai_base_url_state.clone(),
            "https://ai.example/v1",
        );
        enter(cx, handle, &view, |v| v.ai_api_key_state.clone(), "sk-test");
        enter(cx, handle, &view, |v| v.ai_model_state.clone(), "a-model");
        assert_eq!(
            stored(cx, |s| (s.ai_base_url(), s.ai_api_key(), s.ai_model())),
            (
                "https://ai.example/v1".to_string(),
                "sk-test".to_string(),
                "a-model".to_string()
            )
        );

        let proxy: Field = |v| v.http_proxy_state.clone();
        enter(cx, handle, &view, proxy, "http:/");
        assert_eq!(stored(cx, |s| s.http_proxy()), "");
        assert_eq!(shown(cx, &view, proxy), "", "put back to the one in effect");
        enter(cx, handle, &view, proxy, "http://127.0.0.1:8080");
        assert_eq!(stored(cx, |s| s.http_proxy()), "http://127.0.0.1:8080");

        let font_size: Field = |v| v.font_size_state.clone();
        enter(cx, handle, &view, font_size, "25");
        assert_eq!(shown(cx, &view, font_size), "20");
        assert_eq!(stored(cx, |s| s.font_rem_px()), Some(20.));
    }

    /// The depth steps by one, and every list saves what is picked in it.
    #[gpui::test]
    fn each_list_saves_what_is_picked(cx: &mut TestAppContext) {
        init_ui(cx);
        let (handle, view) = open_settings(cx);
        let pick = |cx: &mut TestAppContext, select: fn(&ZedisSettingEditor) -> Entity<ZedisSelect>, index: usize| {
            cx.update(|cx| {
                select(view.read(cx)).update(cx, |_, cx| cx.emit(ZedisSelectEvent::Change(index)));
            });
            cx.run_until_parked();
        };

        let depth = stored(cx, |s| s.max_key_tree_depth());
        cx.update_window(handle, |_, _, cx| {
            view.read(cx)
                .max_key_tree_depth_state
                .clone()
                .update(cx, |_, cx| cx.emit(NumberInputEvent::Step(StepAction::Increment)));
        })
        .expect("the window is open");
        cx.run_until_parked();
        assert_eq!(
            cx.read(|cx| view.read(cx).max_key_tree_depth_state.read(cx).value().to_string()),
            (depth + 1).to_string()
        );

        let utc = TimeZonePref::ALL
            .iter()
            .position(|zone| *zone == TimeZonePref::Utc)
            .expect("UTC is offered");
        pick(cx, |v| v.time_zone_select.clone(), utc);
        assert_eq!(stored(cx, |s| s.time_zone()), TimeZonePref::Utc);
        pick(cx, |v| v.date_format_select.clone(), 1);
        assert_eq!(stored(cx, |s| s.date_format()), DATE_FORMATS[1].id);
        let tree = JsonView::ALL
            .iter()
            .position(|view| *view == JsonView::Tree)
            .expect("the tree is offered");
        pick(cx, |v| v.json_view_select.clone(), tree);
        assert_eq!(stored(cx, |s| s.json_view()), JsonView::Tree);
        if let Some(Some(font)) = cx.read(|cx| view.read(cx).ui_font_values.get(1).cloned()) {
            pick(cx, |v| v.ui_font_select.clone(), 1);
            assert_eq!(stored(cx, |s| s.ui_font_family()), Some(font));
        }
        // Last: a language builds the page again.
        pick(cx, |v| v.locale_select.clone(), 1);
        assert_eq!(stored(cx, |s| s.locale().to_string()), LOCALES[1].0);
    }

    #[test]
    fn parse_font_rem_px_accepts_whole_pixels_in_range() {
        assert_eq!(parse_font_rem_px("14"), Some(14.0));
        assert_eq!(parse_font_rem_px(" 12 "), Some(12.0));
        assert_eq!(parse_font_rem_px("20"), Some(20.0));
        assert_eq!(parse_font_rem_px("2"), None);
        assert_eq!(parse_font_rem_px("21"), None);
        assert_eq!(parse_font_rem_px("12.5"), None);
        assert_eq!(parse_font_rem_px(""), None);
        assert_eq!(parse_font_rem_px("abc"), None);
    }

    #[test]
    fn clamp_font_rem_px_rounds_numeric_and_falls_back() {
        assert_eq!(clamp_font_rem_px("14", 16.0), 14.0);
        assert_eq!(clamp_font_rem_px("25", 16.0), 20.0);
        assert_eq!(clamp_font_rem_px("11", 16.0), 12.0);
        assert_eq!(clamp_font_rem_px("12.6", 16.0), 13.0);
        assert_eq!(clamp_font_rem_px("", 16.0), 16.0);
        assert_eq!(clamp_font_rem_px("abc", 16.0), 16.0);
    }
}
