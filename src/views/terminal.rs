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

use crate::helpers::saves_files;
#[cfg(not(target_family = "wasm"))]
use crate::helpers::{AiEndpoint, get_download_dir, get_or_create_config_dir, suggest_command, write_file_atomic};
#[cfg(target_family = "wasm")]
use crate::helpers::{media_type_for, web_download};
#[cfg(target_family = "wasm")]
use crate::views::export_filename_with;
use crate::{
    connection::{
        DangerKind, ExecReplies, ReplyFormat, ServerDb, TerminalReply, TerminalSession, classify_dangerous_line,
        command_doc_url, get_command_description, get_server, is_write_command, list_commands, requires_write_confirm,
    },
    error::Error,
    helpers::{TerminalAction, get_mono_font_family, humanize_keystroke, starts_with_ignore_ascii_case},
    states::{
        ServerEvent, WriteGate, ZedisGlobalStore, ZedisServerState, command_history, i18n_common, i18n_terminal,
        record_command, update_app_state_and_save_quiet,
    },
    views::{bridge_danger, confirm_dangerous_command},
};
#[cfg(not(target_family = "wasm"))]
use chrono::Local;
#[cfg(not(target_family = "wasm"))]
use gpui::Task;
use gpui::{App, ClipboardItem, Entity, SharedString, Subscription, Window, div, prelude::*, px};
use gpui_kit::component::{
    ActiveTheme, Icon, IconName, Selectable, Sizable, WindowExt,
    button::{Button, ButtonGroup, ButtonVariants},
    h_flex,
    highlighter::Language,
    input::{
        Copy, Editor, EditorState, Input, InputEvent, InputState, MoveDown, MoveUp, Position, SelectAll, Textarea,
        TextareaState,
    },
    label::Label,
    notification::Notification,
    v_flex,
};
use rust_i18n::t;
#[cfg(not(target_family = "wasm"))]
use std::io;
#[cfg(not(target_family = "wasm"))]
use std::path::PathBuf;
use tracing::error;
#[cfg(not(target_family = "wasm"))]
use tracing::info;
use web_time::Instant;

mod completion;
mod execution;
mod transcript;

use completion::ReverseSearchState;
pub(crate) use execution::{first_write_line, refusal, strip_redis_cli_prefix};
use transcript::{Block, render_transcript, reply_format_label, trim_output_scrollback};

type Result<T, E = Error> = std::result::Result<T, E>;

const CMD_LABEL: &str = "$";
const CMD_CLEAR: &str = "clear";
const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The banner a cleared terminal starts with: the logo, and the name under
/// it rather than beside it. Beside the logo's middle row it made a line of
/// sixty-odd columns, which at the smallest window — some forty-five
/// columns of terminal — wrapped through the drawing (#203).
const ZEDIS_LOGO: &str = r#" __________ ____ ___ ____
|__  / ____|  _ \_ _/ ___|
  / /|  _| | | | | |\___ \
 / /_| |___| |_| | | ___) |
/____|_____|____/___|____/
ZEDIS Native Redis GUI v{VERSION}
"#;

pub struct ZedisTerminal {
    server_state: Entity<ZedisServerState>,
    cmd_output_state: Entity<EditorState>,
    /// What the output pane shows, as blocks (see [`TranscriptEntry`]).
    transcript: Vec<Block>,
    /// `transcript` rendered in `reply_format`; rebuilt in `render` while
    /// `cmd_output_dirty`, and what Copy / Save hand out.
    cmd_output_text: String,
    cmd_output_dirty: bool,
    /// How replies are drawn — the toolbar's Text / Table / JSON choice,
    /// remembered in the app state.
    reply_format: ReplyFormat,
    /// Commands queued since a `MULTI` on the terminal's connection, so the
    /// `EXEC` reply can be shown beside them. `None` outside a transaction.
    multi_queue: Option<Vec<String>>,
    cmd_input_state: Entity<TextareaState>,
    /// Multi-line "Workbench" editor: one command per line, run as a
    /// batch with Cmd/Ctrl+Enter. Reuses the same execute path as the
    /// single-line REPL (which already iterates `command.lines()`).
    batch_input_state: Entity<EditorState>,
    batch_mode: bool,
    /// Query field for the `Ctrl+R` reverse history search.
    search_input_state: Entity<InputState>,
    /// `Some` while the reverse-search overlay is active.
    reverse_search: Option<ReverseSearchState>,
    redis_commands: Vec<SharedString>,
    cmd_suggestions: Vec<String>,
    cmd_suggestion_index: Option<usize>,
    cmd_history_index: Option<usize>,
    should_focus_input: bool,
    /// In-flight `?` AI request; dropping it (server switch, panel
    /// teardown) cancels the foreground update.
    #[cfg(not(target_family = "wasm"))]
    ai_task: Option<Task<()>>,
    /// AI-suggested command waiting to be placed into the input box —
    /// consumed by `render`, which has the `Window` that `set_value`
    /// needs. The user reviews and hits Enter; nothing auto-executes.
    pending_ai_fill: Option<SharedString>,
    /// The terminal's own connection — never the pooled one the key tree
    /// scans on, so `SELECT` / `AUTH` / `CLIENT SETNAME` / `MULTI` typed
    /// here reach nothing else (see `TerminalSession`). Opened lazily by
    /// the first line and shared by every later one, replaced on a server
    /// or db switch, and forgotten after a link error so the next line
    /// reconnects.
    session: TerminalSession,
    /// Database the terminal's connection sits on after a `SELECT` typed
    /// here. Shown beside the prompt while it differs from the panel's db,
    /// so the divergence from the key tree is visible instead of silent.
    terminal_db: Option<usize>,
    _subscriptions: Vec<Subscription>,
}

impl ZedisTerminal {
    pub fn new(server_state: Entity<ZedisServerState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut subscriptions = Vec::new();

        let cmd_output_state = cx.new(|cx| {
            EditorState::new(window, cx)
                .language(Language::from_str("bash").name())
                .line_number(true)
                .searchable(true)
                .soft_wrap(true)
        });
        let cmd_input_state = cx.new(|cx| TextareaState::new(window, cx).auto_grow(1, 3));
        let batch_input_state = cx.new(|cx| {
            EditorState::new(window, cx)
                .language(Language::from_str("bash").name())
                .line_number(true)
                .soft_wrap(true)
                // Redis command lines, not shell code: an apostrophe or a
                // lone quote inside a value must not grow a partner, and with
                // no bash grammar compiled in nothing would tell the editor
                // it is inside a string.
                .auto_close(false)
        });
        let search_input_state = cx.new(|cx| InputState::new(window, cx));

        subscriptions.push(
            cx.subscribe_in(&cmd_input_state, window, |this, state, event, window, cx| match event {
                InputEvent::PressEnter { .. } => {
                    let cmd = state.read(cx).value();
                    let mut selected_cmd = "".to_string();
                    if let Some(index) = this.cmd_suggestion_index
                        && let Some(suggestion) = this.cmd_suggestions.get(index)
                        && !starts_with_ignore_ascii_case(cmd.trim_start(), suggestion)
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
                    this.execute_command(cmd, window, cx);
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
                        // 1-based: a `0` picks nothing (it underflowed to
                        // index -1, a panic in a debug build).
                        && (1..=this.cmd_suggestions.len() as u32).contains(&index)
                    {
                        this.cmd_suggestion_index = Some((index - 1) as usize);
                        this.apply_suggestion(window, cx);
                        return;
                    }
                    this.update_suggestions(value, cx);
                    cx.notify();
                }
                _ => {}
            }),
        );

        subscriptions.push(
            cx.subscribe(&server_state, |this, _server_state, event, cx| match event {
                // Fires for a db switch too (`select` resets the state either
                // way), so the connection is rebuilt on the new db.
                ServerEvent::ServerSelected(_) => {
                    this.drop_connection();
                    this.reset_cmd_state(cx);
                }
                ServerEvent::ServerInfoUpdated => {
                    this.update_redis_commands(cx);
                }
                ServerEvent::TerminalToggled(toggled) if *toggled => {
                    this.should_focus_input = true;
                    cx.notify();
                }
                _ => {}
            }),
        );

        // Cmd/Ctrl+Enter in the batch editor runs every line at once.
        subscriptions.push(
            cx.subscribe_in(&batch_input_state, window, |this, _state, event, window, cx| {
                if let InputEvent::PressEnter { secondary, .. } = event
                    && *secondary
                {
                    this.run_batch(window, cx);
                }
            }),
        );

        // Reverse-search query field: typing re-filters the history live;
        // Enter accepts the highlighted match into the command input.
        subscriptions.push(
            cx.subscribe_in(&search_input_state, window, |this, state, event, window, cx| {
                if this.reverse_search.is_none() {
                    return;
                }
                match event {
                    InputEvent::Change => {
                        let query = state.read(cx).value().to_string();
                        this.update_reverse_search_matches(&query, cx);
                        cx.notify();
                    }
                    InputEvent::PressEnter { .. } => this.accept_reverse_search(window, cx),
                    _ => {}
                }
            }),
        );

        let reply_format = cx.global::<ZedisGlobalStore>().read(cx).terminal_reply_format();
        let mut this = Self {
            server_state,
            cmd_output_state,
            transcript: Vec::new(),
            cmd_output_text: String::new(),
            cmd_output_dirty: false,
            reply_format,
            multi_queue: None,
            cmd_input_state,
            batch_input_state,
            batch_mode: false,
            search_input_state,
            reverse_search: None,
            redis_commands: Vec::new(),
            cmd_suggestions: Vec::new(),
            cmd_suggestion_index: None,
            cmd_history_index: None,
            should_focus_input: false,
            #[cfg(not(target_family = "wasm"))]
            ai_task: None,
            pending_ai_fill: None,
            session: TerminalSession::default(),
            terminal_db: None,
            _subscriptions: subscriptions,
        };
        this.reset_cmd_state(cx);
        this.update_redis_commands(cx);
        this
    }
}

impl Render for ZedisTerminal {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if std::mem::take(&mut self.cmd_output_dirty) {
            let mut rendered = render_transcript(&mut self.transcript, self.reply_format);
            trim_output_scrollback(&mut rendered);
            self.cmd_output_text = rendered;
            let text = SharedString::from(self.cmd_output_text.clone());
            self.cmd_output_state.update(cx, |state, cx| {
                state.set_value(text, window, cx);
                state.set_cursor_position(Position::new(u32::MAX, u32::MAX), window, cx);
            });
            self.should_focus_input = true;
        }
        // An AI suggestion arrived — put it into the input box for review
        // (deferred to render because `set_value` needs the `Window`).
        if let Some(command) = self.pending_ai_fill.take() {
            self.cmd_input_state.update(cx, |state, cx| {
                state.set_value(command, window, cx);
            });
            self.should_focus_input = true;
        }
        if std::mem::take(&mut self.should_focus_input) && self.reverse_search.is_none() {
            self.cmd_input_state.update(cx, |this, cx| this.focus(window, cx));
        }

        let font_family: SharedString = get_mono_font_family().into();

        let handle_cmd_arrow = |this: &mut Self, is_up: bool, window: &mut Window, cx: &mut Context<Self>| {
            // While the reverse-search overlay is open the arrows step through
            // its matches (↑ older, ↓ newer) instead of driving REPL
            // completion/history.
            if this.reverse_search.is_some() {
                this.step_reverse_search(is_up, cx);
                cx.stop_propagation();
                return;
            }
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
        // Captured (fires before the focused input) so Ctrl+R and Esc work
        // regardless of what the InputState binds them to. Ctrl+R opens the
        // reverse search and steps through matches; Esc closes it.
        let handle_capture_keys = cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
            let ks = &event.keystroke;
            if ks.modifiers.control && ks.key == "r" {
                this.enter_or_advance_reverse_search(window, cx);
                cx.stop_propagation();
            } else if this.reverse_search.is_some() && ks.key == "escape" {
                this.cancel_reverse_search(window, cx);
                cx.stop_propagation();
            }
        });

        let reply_format = self.reply_format;
        let toolbar_border = cx.theme().border;
        // Above the output: how replies are drawn, and what to do with the
        // whole transcript. Labels, not icons — three quiet commands beside
        // a segmented choice.
        let output_toolbar = h_flex()
            .w_full()
            .items_center()
            .justify_between()
            .px_2()
            .py_1()
            .border_b_1()
            .border_color(toolbar_border)
            .child(
                ButtonGroup::new("term-reply-format")
                    .compact()
                    .small()
                    .outline()
                    .children(ReplyFormat::ALL.into_iter().enumerate().map(|(ix, format)| {
                        Button::new(("term-reply-format", ix))
                            .label(reply_format_label(format, cx))
                            .selected(format == reply_format)
                    }))
                    .on_click(cx.listener(|this, clicks: &Vec<usize>, _window, cx| {
                        if let Some(format) = clicks.first().and_then(|ix| ReplyFormat::ALL.get(*ix)) {
                            this.set_reply_format(*format, cx);
                        }
                    })),
            )
            .child(
                h_flex()
                    .gap_1()
                    .child(
                        Button::new("term-copy-output")
                            .label(i18n_terminal(cx, "copy_output"))
                            .ghost()
                            .small()
                            .tooltip(i18n_terminal(cx, "copy_output_tooltip"))
                            .on_click(cx.listener(|this, _, window, cx| this.copy_output(window, cx))),
                    )
                    // A file in Downloads, revealed in the file manager.
                    .when(saves_files(), |this| {
                        this.child(
                            Button::new("term-save-output")
                                .label(i18n_terminal(cx, "save_output"))
                                .ghost()
                                .small()
                                .tooltip(i18n_terminal(cx, "save_output_tooltip"))
                                .on_click(cx.listener(|this, _, window, cx| this.save_output(window, cx))),
                        )
                    })
                    .child(
                        Button::new("term-clear-output")
                            .label(i18n_terminal(cx, "clear"))
                            .ghost()
                            .small()
                            .on_click(cx.listener(|this, _, _window, cx| {
                                this.reset_cmd_state(cx);
                                cx.notify();
                            })),
                    ),
            );

        v_flex()
            .w_full()
            .h_full()
            .on_action(cx.listener(|this, action: &TerminalAction, window, cx| this.handle_action(action, window, cx)))
            .child(output_toolbar)
            .child(
                div().flex_1().w_full().relative().child(
                    div().absolute().inset_0().size_full().overflow_hidden().child(
                        // Read-only rather than disabled: the pane keeps
                        // focus, selection, ⌘C and a right-click menu (a
                        // disabled input has none of those — issue #60).
                        Editor::new(&self.cmd_output_state)
                            .w_full()
                            .h_full()
                            .font_family(font_family.clone())
                            .readonly(true)
                            .appearance(false)
                            .bordered(false)
                            .context_menu(|menu, _window, cx| {
                                let menu = menu
                                    .menu(i18n_terminal(cx, "menu_copy"), Box::new(Copy))
                                    .menu(i18n_terminal(cx, "menu_select_all"), Box::new(SelectAll))
                                    .separator()
                                    .menu(i18n_terminal(cx, "menu_copy_output"), Box::new(TerminalAction::CopyAll));
                                let menu = if saves_files() {
                                    menu.menu(i18n_terminal(cx, "menu_save_output"), Box::new(TerminalAction::Save))
                                } else {
                                    menu
                                };
                                menu.separator()
                                    .menu(i18n_terminal(cx, "clear"), Box::new(TerminalAction::Clear))
                            }),
                    ),
                ),
            )
            .child({
                let batch_mode = self.batch_mode;
                let border = cx.theme().border;

                let muted = cx.theme().muted_foreground;
                let search_active = self.reverse_search.is_some();
                let current_match = self.current_reverse_match();
                // `[n]` after the prompt, redis-cli style, once a `SELECT`
                // typed here has parted the terminal from the key tree's db.
                let panel_db = self.server_state.read(cx).db();
                let prompt_db = self.terminal_db.filter(|db| *db != panel_db);

                // The bottom row: normally the single-line REPL (completion +
                // history + Batch toggle); while reverse-search is active it is
                // replaced in place by the `(reverse-i-search)` overlay. Built
                // unconditionally so the captured arrow/key handlers (and the
                // Ctrl+R capture that *opens* search) are always consumed.
                let row_body = if search_active {
                    // (reverse-i-search)`<query>`: <matched command>
                    let match_label = match &current_match {
                        Some(m) => Label::new(m.clone()).text_color(cx.theme().foreground),
                        None => Label::new(i18n_terminal(cx, "no_match")).text_color(muted),
                    };
                    h_flex()
                        .w_full()
                        .items_center()
                        .gap_1()
                        .px_2()
                        .py_1()
                        .child(
                            Label::new("(reverse-i-search)`")
                                .font_family(font_family.clone())
                                .text_color(muted),
                        )
                        .child(
                            div().min_w(px(60.)).child(
                                Input::new(&self.search_input_state)
                                    .font_family(font_family.clone())
                                    .appearance(false),
                            ),
                        )
                        .child(Label::new("`:").font_family(font_family.clone()).text_color(muted))
                        .child(match_label.font_family(font_family.clone()))
                        .child(div().flex_1())
                        .child(
                            Label::new(i18n_terminal(cx, "history_search_hint"))
                                .text_xs()
                                .text_color(muted),
                        )
                } else {
                    h_flex()
                        .w_full()
                        .items_center()
                        .gap_1()
                        // The prompt starts where the search row's text does
                        // (`px_2`): it sat against the pane's edge while the
                        // Batch button kept its own padding off the other.
                        .pl_2()
                        .child(
                            // `prefix` is an `Input` adornment and a
                            // `Textarea` has none, so the `>` marker sits
                            // beside the box instead of inside its frame —
                            // identical on screen, since the box draws no
                            // frame here anyway.
                            h_flex()
                                .flex_1()
                                .items_center()
                                .gap_1()
                                .child(Label::new(CMD_LABEL).text_color(cx.theme().yellow))
                                .when_some(prompt_db, |this, db| {
                                    this.child(
                                        Label::new(format!("[{db}]"))
                                            .font_family(font_family.clone())
                                            .text_color(muted),
                                    )
                                })
                                .child(
                                    Textarea::new(&self.cmd_input_state)
                                        .flex_1()
                                        .font_family(font_family.clone())
                                        .appearance(false),
                                ),
                        )
                        .child(
                            Button::new("term-mode-batch")
                                .label(i18n_terminal(cx, "batch"))
                                .ghost()
                                .small()
                                .on_click(cx.listener(|this, _, window, cx| this.toggle_batch_mode(window, cx))),
                        )
                };
                let repl_row = div()
                    .w_full()
                    .border_t_1()
                    .border_color(border)
                    .capture_action(handle_move_up)
                    .capture_action(handle_move_down)
                    .capture_key_down(handle_capture_keys)
                    .on_key_down(handle_other_keys)
                    .child(row_body);

                v_flex()
                    .w_full()
                    .when(batch_mode, |this| {
                        this.child(
                            div().w_full().h(px(180.)).border_t_1().border_color(border).child(
                                Editor::new(&self.batch_input_state)
                                    .w_full()
                                    .h_full()
                                    .font_family(font_family.clone())
                                    .appearance(false),
                            ),
                        )
                        .child(
                            h_flex()
                                .w_full()
                                .items_center()
                                .gap_2()
                                .px_2()
                                .py_1()
                                .border_t_1()
                                .border_color(border)
                                .child(
                                    Button::new("term-batch-run")
                                        .label(i18n_terminal(cx, "run"))
                                        .primary()
                                        .small()
                                        .on_click(cx.listener(|this, _, window, cx| this.run_batch(window, cx))),
                                )
                                .child(
                                    // The input's own `secondary` Enter: ⌘ on a
                                    // Mac keyboard, Ctrl elsewhere — one of the
                                    // two, not both spelled out.
                                    Label::new(humanize_keystroke("secondary-enter"))
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground),
                                )
                                .child(div().flex_1())
                                .child(
                                    Button::new("term-mode-repl").label("REPL").ghost().small().on_click(
                                        cx.listener(|this, _, window, cx| this.toggle_batch_mode(window, cx)),
                                    ),
                                ),
                        )
                    })
                    .when(!batch_mode, |this| {
                        this.when(!self.cmd_suggestions.is_empty(), |this| {
                            this.child(
                                div()
                                    .w_full()
                                    .bg(cx.theme().background)
                                    .border_t_1()
                                    .border_color(border)
                                    .p_1()
                                    .child(v_flex().gap_0p5().children(self.cmd_suggestions.iter().enumerate().map(
                                        |(idx, cmd)| {
                                            let is_selected = self.cmd_suggestion_index == Some(idx);
                                            let text = format!("{}: {cmd}", idx + 1);
                                            let (summary, syntax) = get_command_description(cmd).unwrap_or_default();
                                            let doc_url = command_doc_url(cmd);
                                            let make_label = |text: SharedString| {
                                                Label::new(text)
                                                    .font_family(font_family.clone())
                                                    .text_sm()
                                                    .text_color(cx.theme().muted_foreground)
                                            };
                                            h_flex()
                                                .px_2()
                                                .py_1()
                                                .rounded_sm()
                                                .items_start()
                                                .justify_between()
                                                .gap_2()
                                                .when(is_selected, |this| this.bg(cx.theme().selection))
                                                .child(
                                                    div()
                                                        .min_w_0()
                                                        .child(
                                                            Label::new(text)
                                                                .font_family(font_family.clone())
                                                                .text_color(cx.theme().foreground),
                                                        )
                                                        .child(make_label(syntax.into()))
                                                        .child(make_label(summary.into())),
                                                )
                                                .child(
                                                    // Hardcoded English like the rest of this panel.
                                                    Button::new(("term-cmd-doc", idx))
                                                        .icon(Icon::new(IconName::ExternalLink))
                                                        .ghost()
                                                        .small()
                                                        .tooltip(format!("{cmd} docs — {doc_url}"))
                                                        .on_click(move |_, _window, cx| cx.open_url(&doc_url)),
                                                )
                                        },
                                    ))),
                            )
                        })
                        .child(repl_row)
                    })
            })
    }
}

#[cfg(test)]
mod tests {
    use super::execution::{BLOCKING_REJECT_MSG, first_write_line, refusal, strip_redis_cli_prefix};
    use super::transcript::{Block, LineReply, TranscriptEntry, transcript_entries_for};
    use super::{ReplyFormat, TerminalReply, VERSION, ZEDIS_LOGO};

    /// The banner fits the terminal of the smallest window, about forty-five
    /// columns: a wider line wraps through the logo (#203).
    #[test]
    fn the_banner_fits_the_narrowest_terminal() {
        let banner = ZEDIS_LOGO.replace("{VERSION}", VERSION);
        for line in banner.lines() {
            assert!(line.chars().count() <= 40, "{} columns: {line:?}", line.chars().count());
        }
        assert!(banner.contains(VERSION), "the banner names the version");
    }

    /// The line a read-only or locked connection holds the input back for.
    #[test]
    fn the_first_write_among_the_lines_is_found() {
        assert_eq!(first_write_line("GET a"), None);
        assert_eq!(first_write_line("  \n\n"), None);
        assert_eq!(first_write_line("SET a 1").as_deref(), Some("SET a 1"));
        // Any line of a batch, and whatever case it was typed in.
        assert_eq!(
            first_write_line("GET a\n  hset h f v  \nDEL a").as_deref(),
            Some("hset h f v")
        );
        // Reads that carry a write's name in an argument stay reads; a
        // container command is judged by its subcommand.
        assert_eq!(first_write_line("GET set\nCONFIG GET maxmemory"), None);
        assert_eq!(
            first_write_line("CONFIG SET maxmemory 1").as_deref(),
            Some("CONFIG SET maxmemory 1")
        );
        // Writes spelled like reads, a script, and a command nobody knows.
        for line in ["GETDEL a", "EVAL 'return 1' 0", "NOSUCHCMD x"] {
            assert_eq!(first_write_line(line).as_deref(), Some(line), "{line}");
        }
        // What a session needs in order to look around is not a write.
        assert_eq!(first_write_line("SELECT 1\nPING\nINFO memory\nSCAN 0"), None);
    }

    /// The transcript's text in `format`, drawn the way the view draws it.
    fn render_transcript(entries: Vec<TranscriptEntry>, format: ReplyFormat) -> String {
        let mut blocks: Vec<Block> = entries.into_iter().map(Block::from).collect();
        super::render_transcript(&mut blocks, format)
    }

    /// A reply from its RESP encoding — the view holds replies it cannot
    /// look inside, and a test has no more access than the view does.
    fn reply(cmd: &str, args: &[&str], resp: &[u8]) -> LineReply {
        let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        LineReply::Value(TerminalReply::from_resp(cmd, &args, resp).expect("valid RESP"))
    }

    #[test]
    fn transcript_re_renders_in_every_format() {
        let entries = vec![
            TranscriptEntry::Text("banner".to_string()),
            TranscriptEntry::Command {
                line: "HGETALL h".to_string(),
                reply: reply("HGETALL", &["h"], b"*2\r\n$1\r\na\r\n$1\r\n1\r\n"),
            },
            TranscriptEntry::Command {
                line: "GET missing".to_string(),
                reply: LineReply::Message("(error) boom".to_string()),
            },
        ];
        // One set of blocks through every format and back: each switch
        // redraws what was drawn in another.
        let mut blocks: Vec<Block> = entries.into_iter().map(Block::from).collect();
        let text = "banner\n$ HGETALL h\n[a, 1]\n$ GET missing\n(error) boom\n";
        assert_eq!(super::render_transcript(&mut blocks, ReplyFormat::Text), text);
        assert_eq!(
            super::render_transcript(&mut blocks, ReplyFormat::Table),
            "banner\n$ HGETALL h\nfield │ value\n──────┼──────\na     │ 1\n$ GET missing\n(error) boom\n"
        );
        assert_eq!(
            super::render_transcript(&mut blocks, ReplyFormat::Json),
            "banner\n$ HGETALL h\n{\n  \"a\": \"1\"\n}\n$ GET missing\n(error) boom\n"
        );
        assert_eq!(super::render_transcript(&mut blocks, ReplyFormat::Text), text);
    }

    #[test]
    fn multi_exec_becomes_one_block_of_commands_and_replies() {
        // The sentence a WATCH conflict is said in; the view hands it in,
        // in the UI language.
        const ABORTED: &str = "(aborted under WATCH)";
        let mut queue = None;
        let queued = |line: &str| reply(line.split(' ').next().unwrap_or_default(), &[], b"+QUEUED\r\n");

        let entries = transcript_entries_for(&mut queue, "MULTI".into(), reply("MULTI", &[], b"+OK\r\n"), ABORTED);
        assert!(matches!(entries.as_slice(), [TranscriptEntry::Command { .. }]));
        assert_eq!(queue.as_deref(), Some(&[][..]));

        transcript_entries_for(&mut queue, "SET a 1".into(), queued("SET a 1"), ABORTED);
        transcript_entries_for(&mut queue, "INCR a".into(), queued("INCR a"), ABORTED);
        assert_eq!(queue.as_deref().map(<[String]>::len), Some(2));

        let exec = reply("EXEC", &[], b"*2\r\n+OK\r\n:2\r\n");
        let entries = transcript_entries_for(&mut queue, "EXEC".into(), exec, ABORTED);
        assert!(queue.is_none(), "EXEC closes the transaction");
        let rendered = render_transcript(entries, ReplyFormat::Text);
        assert_eq!(
            rendered,
            "$ EXEC\n# │ command │ reply\n──┼─────────┼──────\n1 │ SET a 1 │ OK\n2 │ INCR a  │ 2\n"
        );

        // A WATCH conflict: EXEC answers nil, which is said in words.
        let mut queue = Some(vec!["SET a 1".to_string()]);
        let entries = transcript_entries_for(&mut queue, "EXEC".into(), reply("EXEC", &[], b"*-1\r\n"), ABORTED);
        assert!(queue.is_none());
        assert!(render_transcript(entries, ReplyFormat::Text).contains(ABORTED));

        // DISCARD drops the queue; EXEC outside a MULTI is a plain command.
        let mut queue = Some(vec!["SET a 1".to_string()]);
        transcript_entries_for(&mut queue, "DISCARD".into(), reply("DISCARD", &[], b"+OK\r\n"), ABORTED);
        assert!(queue.is_none());
        let entries = transcript_entries_for(&mut queue, "EXEC".into(), reply("EXEC", &[], b"*-1\r\n"), ABORTED);
        assert!(matches!(entries.as_slice(), [TranscriptEntry::Command { .. }]));
    }

    /// What the terminal cannot run is refused with its reason, and the
    /// reason names the panel that does the job. A subscription used to be
    /// sent: nothing published ever showed, and the connection refused
    /// every later line (#191).
    #[test]
    fn a_line_that_would_take_the_connection_over_is_refused() {
        let args = |words: &[&str]| words.iter().map(|word| word.to_string()).collect::<Vec<_>>();
        for name in ["SUBSCRIBE", "psubscribe", "SSubscribe"] {
            let reason = refusal(name, &args(&["news"])).expect("refused");
            assert!(reason.contains("Pubsub Mode"), "{name}: {reason}");
        }
        let monitor = refusal("monitor", &[]).expect("refused");
        assert!(monitor.contains("Live Monitor"), "{monitor}");
        assert!(refusal("PSYNC", &args(&["?", "-1"])).is_some());
        assert!(refusal("sync", &[]).is_some());
        assert_eq!(
            refusal("BLPOP", &args(&["queue", "0"])).as_deref(),
            Some(BLOCKING_REJECT_MSG)
        );
        assert!(refusal("XREAD", &args(&["BLOCK", "0", "STREAMS", "s", "$"])).is_some());

        // Everything around them still runs: publishing, asking who
        // listens, leaving a subscription that was never entered.
        for (name, rest) in [
            ("PUBLISH", vec!["news", "hi"]),
            ("PUBSUB", vec!["CHANNELS"]),
            ("UNSUBSCRIBE", vec![]),
            ("XREAD", vec!["STREAMS", "s", "0"]),
            ("GET", vec!["k"]),
        ] {
            assert_eq!(refusal(name, &args(&rest)), None, "{name}");
        }
    }

    /// A refused line is never sent, so it is not the write a locked
    /// connection asks to unlock for; a real write beside it still is.
    #[test]
    fn a_refused_line_is_not_a_write_to_unlock_for() {
        assert_eq!(first_write_line("SUBSCRIBE news\nGET k"), None);
        assert_eq!(first_write_line("BLPOP queue 0"), None);
        assert_eq!(first_write_line("SUBSCRIBE news\nSET k v").as_deref(), Some("SET k v"));
    }

    #[test]
    fn strips_leading_redis_cli() {
        assert_eq!(strip_redis_cli_prefix("redis-cli GET foo"), "GET foo");
        assert_eq!(strip_redis_cli_prefix("  REDIS-CLI  set k v"), "set k v");
        assert_eq!(strip_redis_cli_prefix("redis-cli"), "");
        // Only a whole leading word counts; embedded mentions stay untouched.
        assert_eq!(strip_redis_cli_prefix("get redis-cli"), "get redis-cli");
        assert_eq!(strip_redis_cli_prefix("redis-cli-tool run"), "redis-cli-tool run");
        assert_eq!(strip_redis_cli_prefix("GET foo"), "GET foo");
    }
}
