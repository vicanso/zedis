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
    connection::{get_command_description, get_connection_manager, list_commands},
    db::get_cmd_history_manager,
    error::Error,
    helpers::{get_font_family, redis_value_to_string, starts_with_ignore_ascii_case},
    states::{ServerEvent, ZedisServerState},
};
use gpui::{Entity, SharedString, Subscription, Window, div, prelude::*};
use gpui_component::{
    ActiveTheme,
    highlighter::Language,
    input::{Input, InputEvent, InputState, MoveDown, MoveUp, Position},
    label::Label,
    v_flex,
};
use redis::cmd;

type Result<T, E = Error> = std::result::Result<T, E>;

const CMD_LABEL: &str = "$";
const CMD_CLEAR: &str = "clear";
const VERSION: &str = env!("CARGO_PKG_VERSION");

const ZEDIS_LOGO: &str = r#" __________ ____ ___ ____
|__  / ____|  _ \_ _/ ___|
  / /|  _| | | | | |\___ \    ZEDIS Native Redis GUI v{VERSION}
 / /_| |___| |_| | | ___) |
/____|_____|____/___|____/
"#;

pub struct ZedisTerminal {
    server_state: Entity<ZedisServerState>,
    cmd_output_state: Entity<InputState>,
    cmd_output_text: String,
    cmd_output_dirty: bool,
    cmd_input_state: Entity<InputState>,
    redis_commands: Vec<SharedString>,
    cmd_suggestions: Vec<String>,
    cmd_suggestion_index: Option<usize>,
    cmd_history_index: Option<usize>,
    should_focus_input: bool,
    _subscriptions: Vec<Subscription>,
}

impl ZedisTerminal {
    pub fn new(server_state: Entity<ZedisServerState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut subscriptions = Vec::new();

        let cmd_output_state = cx.new(|cx| {
            InputState::new(window, cx)
                .code_editor(Language::from_str("bash").name())
                .line_number(true)
                .searchable(true)
                .soft_wrap(true)
        });
        let cmd_input_state = cx.new(|cx| InputState::new(window, cx).auto_grow(1, 3));

        subscriptions.push(
            cx.subscribe_in(&cmd_input_state, window, |this, state, event, window, cx| match event {
                InputEvent::PressEnter { .. } => {
                    let cmd = state.read(cx).value();
                    let mut selected_cmd = "".to_string();
                    if let Some(index) = this.cmd_suggestion_index
                        && let Some(suggestion) = this.cmd_suggestions.get(index)
                        && !starts_with_ignore_ascii_case(cmd.as_str(), suggestion)
                    {
                        selected_cmd = suggestion.clone();
                    }
                    if !selected_cmd.is_empty() {
                        this.apply_suggestion(window, cx);
                        cx.stop_propagation();
                        return;
                    }
                    state.update(cx, |state, cx| {
                        state.set_value(SharedString::default(), window, cx);
                    });
                    this.cmd_suggestions.clear();
                    this.cmd_suggestion_index = None;
                    this.execute_command(cmd, cx);
                }
                InputEvent::Change => {
                    if this.cmd_history_index.is_some() {
                        return;
                    }
                    let value = state.read(cx).value().to_string();
                    if !value.is_empty()
                        && !value.contains(' ')
                        && let Some(last) = value.chars().last()
                        && let Some(index) = last.to_digit(10)
                        && index <= this.cmd_suggestions.len() as u32
                    {
                        this.cmd_suggestion_index = Some((index - 1) as usize);
                        this.apply_suggestion(window, cx);
                        return;
                    }
                    this.update_suggestions(value);
                    cx.notify();
                }
                _ => {}
            }),
        );

        subscriptions.push(
            cx.subscribe(&server_state, |this, _server_state, event, cx| match event {
                ServerEvent::ServerSelected(_) => {
                    this.reset_cmd_state(cx);
                }
                ServerEvent::ServerInfoUpdated => {
                    this.update_redis_commands(cx);
                }
                ServerEvent::TerminalToggled(toggled) => {
                    if *toggled {
                        this.should_focus_input = true;
                        cx.notify();
                    }
                }
                _ => {}
            }),
        );

        let mut this = Self {
            server_state,
            cmd_output_state,
            cmd_output_text: String::new(),
            cmd_output_dirty: false,
            cmd_input_state,
            redis_commands: Vec::new(),
            cmd_suggestions: Vec::new(),
            cmd_suggestion_index: None,
            cmd_history_index: None,
            should_focus_input: false,
            _subscriptions: subscriptions,
        };
        this.reset_cmd_state(cx);
        this.update_redis_commands(cx);
        this
    }

    fn reset_cmd_state(&mut self, _cx: &mut Context<Self>) {
        self.cmd_output_text = ZEDIS_LOGO.replace("{VERSION}", VERSION);
        self.cmd_output_dirty = true;
    }

    fn update_redis_commands(&mut self, cx: &mut Context<Self>) {
        self.redis_commands = list_commands(self.server_state.read(cx).version());
    }

    fn update_suggestions(&mut self, input: String) {
        self.cmd_suggestions.clear();
        self.cmd_suggestion_index = None;
        if input.is_empty() {
            return;
        }
        let words: Vec<&str> = input.split_whitespace().collect();
        if words.is_empty() {
            return;
        }
        let max_words = words.len().min(3);
        for word_count in (1..=max_words).rev() {
            let cmd_input = words[..word_count].join(" ").to_uppercase();
            let matches: Vec<String> = self
                .redis_commands
                .iter()
                .filter(|cmd| cmd.as_str().starts_with(&cmd_input))
                .take(5)
                .map(|cmd| cmd.to_string())
                .collect();
            if !matches.is_empty() {
                self.cmd_suggestions = matches;
                self.cmd_suggestion_index = self.cmd_suggestions.iter().position(|cmd| cmd == &cmd_input);
                return;
            }
        }
    }

    fn apply_suggestion(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.cmd_suggestions.is_empty() {
            return;
        }
        let suggestion = if let Some(index) = self.cmd_suggestion_index {
            self.cmd_suggestions.get(index).cloned()
        } else {
            self.cmd_suggestions.first().cloned()
        };
        if let Some(cmd) = suggestion {
            self.cmd_input_state.update(cx, |state, cx| {
                state.set_value(SharedString::from(cmd), window, cx);
            });
            self.cmd_suggestions.clear();
            self.cmd_suggestion_index = None;
            cx.notify();
        }
    }

    fn handle_cmd_history(&mut self, is_up: bool, window: &mut Window, cx: &mut Context<Self>) {
        let server_id = self.server_state.read(cx).server_id();
        if server_id.is_empty() {
            return;
        }
        let offset: i32 = if is_up { 1 } else { -1 };
        let records = get_cmd_history_manager().records(server_id).unwrap_or_default();
        if records.is_empty() {
            return;
        }
        let mut index = if let Some(current) = self.cmd_history_index {
            if offset > 0 {
                current + 1
            } else if current == 0 {
                0
            } else {
                current - 1
            }
        } else if offset > 0 {
            0
        } else {
            records.len() - 1
        };
        index = index.min(records.len() - 1);
        if let Some(value) = records.get(index) {
            self.cmd_input_state.update(cx, |this, cx| {
                this.set_value(value.clone(), window, cx);
                this.set_cursor_position(Position::new(0, u32::MAX), window, cx);
            });
            self.cmd_history_index = Some(index);
        }
    }

    fn execute_command(&mut self, command: SharedString, cx: &mut Context<Self>) {
        if command.is_empty() {
            return;
        }
        if command == CMD_CLEAR {
            self.reset_cmd_state(cx);
            cx.notify();
            return;
        }
        let server_state = self.server_state.read(cx);
        let server_id = server_state.server_id().to_string();
        let db = server_state.db();
        cx.spawn(async move |handle, cx| {
            for line in command.lines() {
                let line = line.trim().to_string();
                let line_clone = line.clone();
                let server_id = server_id.clone();
                let task = cx.background_spawn(async move {
                    let Some(parts) = shlex::split(&line) else {
                        return Ok(SharedString::default());
                    };
                    if parts.is_empty() {
                        return Ok(SharedString::default());
                    }
                    let cmd_name = parts[0].clone();
                    let args = parts[1..].to_vec();
                    let mut conn = get_connection_manager().get_connection(&server_id, db).await?;
                    let data: redis::Value = cmd(&cmd_name).arg(&args).query_async(&mut conn).await?;
                    let _ = get_cmd_history_manager().add_record(server_id.as_str(), line.as_str());
                    Ok(redis_value_to_string(&data).into())
                });
                let result: Result<SharedString> = task.await;
                let content: SharedString = match result {
                    Ok(v) => v,
                    Err(e) => e.to_string().into(),
                };
                let _ = handle.update(cx, |this, cx| {
                    use std::fmt::Write;
                    let _ = writeln!(this.cmd_output_text, "{CMD_LABEL} {line_clone}");
                    let _ = writeln!(this.cmd_output_text, "{content}");
                    this.cmd_output_dirty = true;
                    cx.notify();
                });
            }
        })
        .detach();
    }
}

impl Render for ZedisTerminal {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if std::mem::take(&mut self.cmd_output_dirty) {
            let text = SharedString::from(self.cmd_output_text.clone());
            self.cmd_output_state.update(cx, |state, cx| {
                state.set_value(text, window, cx);
                state.set_cursor_position(Position::new(u32::MAX, u32::MAX), window, cx);
            });
            self.should_focus_input = true;
        }
        if std::mem::take(&mut self.should_focus_input) {
            self.cmd_input_state.update(cx, |this, cx| this.focus(window, cx));
        }

        let font_family: SharedString = get_font_family().into();

        let handle_cmd_arrow = |this: &mut Self, is_up: bool, window: &mut Window, cx: &mut Context<Self>| {
            let input = this.cmd_input_state.read(cx).value();
            if input.is_empty() || this.cmd_history_index.is_some() {
                this.handle_cmd_history(is_up, window, cx);
                cx.stop_propagation();
                return;
            }
            if this.cmd_suggestions.is_empty() {
                return;
            }
            let max = this.cmd_suggestions.len() - 1;
            let new_index = if is_up {
                if let Some(current) = this.cmd_suggestion_index {
                    if current > 0 { current - 1 } else { max }
                } else {
                    max
                }
            } else if let Some(current) = this.cmd_suggestion_index {
                (current + 1).min(max)
            } else {
                0
            };
            this.cmd_suggestion_index = Some(new_index);
            if let Some(cmd) = this.cmd_suggestions.get(new_index) {
                let cmd: SharedString = cmd.clone().into();
                this.cmd_input_state.update(cx, |state, cx| {
                    state.set_value(cmd, window, cx);
                    state.set_cursor_position(Position::new(0, u32::MAX), window, cx);
                });
            }
            cx.notify();
            cx.stop_propagation();
        };

        let handle_move_up = cx.listener(move |this, _: &MoveUp, window, cx| {
            handle_cmd_arrow(this, true, window, cx);
        });
        let handle_move_down = cx.listener(move |this, _: &MoveDown, window, cx| {
            handle_cmd_arrow(this, false, window, cx);
        });
        let handle_other_keys = cx.listener(|this, _: &gpui::KeyDownEvent, _window, _cx| {
            this.cmd_history_index = None;
        });

        v_flex()
            .w_full()
            .h_full()
            .child(
                div().flex_1().w_full().relative().child(
                    div().absolute().inset_0().size_full().overflow_hidden().child(
                        Input::new(&self.cmd_output_state)
                            .w_full()
                            .h_full()
                            .font_family(font_family.clone())
                            .disabled(true)
                            .appearance(false)
                            .bordered(false)
                            .focus_bordered(false),
                    ),
                ),
            )
            .child(
                v_flex()
                    .w_full()
                    .when(!self.cmd_suggestions.is_empty(), |this| {
                        this.child(
                            div()
                                .w_full()
                                .bg(cx.theme().background)
                                .border_t_1()
                                .border_color(cx.theme().border)
                                .p_1()
                                .child(v_flex().gap_0p5().children(self.cmd_suggestions.iter().enumerate().map(
                                    |(idx, cmd)| {
                                        let is_selected = self.cmd_suggestion_index == Some(idx);
                                        let text = format!("{}: {cmd}", idx + 1);
                                        let (summary, syntax) = get_command_description(cmd).unwrap_or_default();
                                        let make_label = |text: SharedString| {
                                            Label::new(text)
                                                .font_family(font_family.clone())
                                                .text_sm()
                                                .text_color(cx.theme().muted_foreground)
                                        };
                                        div()
                                            .px_2()
                                            .py_1()
                                            .rounded_sm()
                                            .when(is_selected, |this| this.bg(cx.theme().selection))
                                            .child(
                                                Label::new(text)
                                                    .font_family(font_family.clone())
                                                    .text_color(cx.theme().foreground),
                                            )
                                            .child(make_label(syntax))
                                            .child(make_label(summary))
                                    },
                                ))),
                        )
                    })
                    .child(
                        div()
                            .w_full()
                            .border_t_1()
                            .border_color(cx.theme().border)
                            .capture_action(handle_move_up)
                            .capture_action(handle_move_down)
                            .on_key_down(handle_other_keys)
                            .child(
                                Input::new(&self.cmd_input_state)
                                    .font_family(font_family)
                                    .prefix(Label::new(CMD_LABEL).text_color(cx.theme().yellow))
                                    .appearance(false),
                            ),
                    ),
            )
    }
}
