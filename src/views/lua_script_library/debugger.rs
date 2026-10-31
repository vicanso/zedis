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

//! The Lua debugger page: one saved script stepped through with the Redis
//! Lua debugger (`SCRIPT DEBUG`), drawn in place of the library's list.
//!
//! The source is drawn line by line here rather than in an editor, so a
//! line number can be a breakpoint toggle and the paused line can be marked.
//! The session itself is `LuaDebugSession` (zedis-connection); this page asks
//! it for one step at a time and draws what came back: the paused line, the
//! locals there, the output so far, and the script's result at the end.
//!
//! Desktop only (`debugs_lua_scripts`): the session holds one connection for
//! its whole length, which a page talking to the bridge does not have.

use super::ZedisLuaScriptLibrary;
use crate::{
    connection::{
        Capability, DebugCommand, DebugEnd, DebugLine, DebugMode, DebugStep, LuaDebugSession, LuaLocal, ServerDb,
        max_keys_index,
    },
    db::LuaScript,
    error::Error,
    helpers::{get_mono_font_family, parse_lines},
    states::{ZedisServerState, escalate_dangerous_body, i18n_common, i18n_lua_scripts},
};
use gpui::{Entity, SharedString, Subscription, Task, WeakEntity, Window, div, prelude::*, px};
use gpui_kit::component::{
    ActiveTheme, Disableable, Icon, IconName, Sizable,
    button::{Button, ButtonVariants},
    checkbox::Checkbox,
    h_flex,
    input::{Input, InputEvent, InputState, Textarea, TextareaState},
    label::Label,
    scroll::ScrollableElement,
    v_flex,
};
use std::collections::BTreeSet;
use zedis_ui::ZedisDialog;

type Result<T, E = Error> = std::result::Result<T, E>;

/// Width of the line-number gutter, which is also the breakpoint toggle.
const GUTTER_WIDTH: f32 = 56.;
/// Width of the side column (inputs, locals, output).
const SIDE_WIDTH: f32 = 380.;
/// Output lines kept; a loop stepped through for a while says a lot.
const MAX_LOG_LINES: usize = 500;

/// What produced one output line, for its colour and tag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LogKind {
    /// What the user typed into the evaluate box.
    Input,
    Debug,
    RedisCall,
    RedisReply,
    RetVal,
    Error,
    Info,
}

impl LogKind {
    fn tag(self) -> &'static str {
        match self {
            LogKind::Input => ">",
            LogKind::Debug => "debug",
            LogKind::RedisCall => "redis",
            LogKind::RedisReply => "reply",
            LogKind::RetVal => "=",
            LogKind::Error => "error",
            LogKind::Info => "",
        }
    }
}

fn log_entry(line: DebugLine) -> (LogKind, SharedString) {
    let (kind, text) = match line {
        DebugLine::Debug(text) => (LogKind::Debug, text),
        DebugLine::RedisCall(text) => (LogKind::RedisCall, text),
        DebugLine::RedisReply(text) => (LogKind::RedisReply, text),
        DebugLine::RetVal(text) => (LogKind::RetVal, text),
        DebugLine::Error(text) => (LogKind::Error, text),
        DebugLine::Info(text) => (LogKind::Info, text),
    };
    (kind, text.into())
}

pub(super) struct LuaDebugger {
    library: WeakEntity<ZedisLuaScriptLibrary>,
    server_state: Entity<ZedisServerState>,
    name: SharedString,
    code: String,
    lines: Vec<SharedString>,
    breakpoints: BTreeSet<u32>,
    keys: Entity<TextareaState>,
    args: Entity<TextareaState>,
    expr: Entity<InputState>,
    sync: bool,
    session: Option<LuaDebugSession>,
    /// The line the script is paused before, while it is.
    current_line: Option<u32>,
    stop_reason: Option<SharedString>,
    locals: Vec<LuaLocal>,
    log: Vec<(LogKind, SharedString)>,
    end: Option<DebugEnd>,
    busy: bool,
    error: Option<SharedString>,
    _task: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl LuaDebugger {
    pub(super) fn new(
        library: WeakEntity<ZedisLuaScriptLibrary>,
        server_state: Entity<ZedisServerState>,
        script: &LuaScript,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let keys_default = script.default_keys.join("\n");
        let args_default = script.default_args.join("\n");
        let keys = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(2, 5)
                .placeholder(i18n_lua_scripts(cx, "keys_placeholder"))
                .default_value(keys_default)
        });
        let args = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(2, 5)
                .placeholder(i18n_lua_scripts(cx, "args_placeholder"))
                .default_value(args_default)
        });
        let expr = cx.new(|cx| InputState::new(window, cx).placeholder(i18n_lua_scripts(cx, "debug_eval_placeholder")));
        let subscriptions = vec![cx.subscribe_in(&expr, window, |this, _state, event, window, cx| {
            if let InputEvent::PressEnter { .. } = event {
                this.evaluate(window, cx);
            }
        })];
        Self {
            library,
            server_state,
            name: script.name.clone().into(),
            lines: script
                .code
                .lines()
                .map(|line| SharedString::from(line.replace('\t', "    ")))
                .collect(),
            code: script.code.clone(),
            breakpoints: BTreeSet::new(),
            keys,
            args,
            expr,
            sync: false,
            session: None,
            current_line: None,
            stop_reason: None,
            locals: Vec::new(),
            log: Vec::new(),
            end: None,
            busy: false,
            error: None,
            _task: None,
            _subscriptions: subscriptions,
        }
    }

    fn is_live(&self) -> bool {
        self.session.as_ref().is_some_and(LuaDebugSession::is_live)
    }

    /// Start (or start over): asks first in sync mode, which keeps the
    /// script's writes and blocks the server while it is paused.
    fn start(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || !self.server_state.read(cx).can(Capability::EvalScript) {
            return;
        }
        if !self.sync {
            self.begin(cx);
            return;
        }
        let entity = cx.entity().downgrade();
        let server_id = self.server_state.read(cx).server_id().to_string();
        let message = escalate_dangerous_body(cx, &server_id, i18n_lua_scripts(cx, "debug_sync_message"));
        ZedisDialog::new_alert(i18n_lua_scripts(cx, "debug_sync_title"), message)
            .danger()
            .ok_text(i18n_lua_scripts(cx, "debug_sync_confirm"))
            .cancel_text(i18n_common(cx, "cancel"))
            .on_ok(move |_, _window, cx| {
                if let Some(this) = entity.upgrade() {
                    this.update(cx, |this, cx| this.begin(cx));
                }
                true
            })
            .open(window, cx);
    }

    fn begin(&mut self, cx: &mut Context<Self>) {
        let state = self.server_state.read(cx);
        let server_id = state.server_id().to_string();
        let db = state.db();
        if server_id.is_empty() {
            self.error = Some(i18n_lua_scripts(cx, "no_server"));
            cx.notify();
            return;
        }
        let keys = parse_lines(&self.keys.read(cx).value());
        let args = parse_lines(&self.args.read(cx).value());
        let needed = max_keys_index(&self.code);
        if needed > keys.len() {
            self.error = Some(
                format!(
                    "{}: need {needed} KEYS, got {}",
                    i18n_lua_scripts(cx, "keys_count_mismatch"),
                    keys.len()
                )
                .into(),
            );
            cx.notify();
            return;
        }
        // A previous session still paused is let go: dropping it closes its
        // connection, which aborts that script.
        self.session = None;
        self.current_line = None;
        self.stop_reason = None;
        self.locals.clear();
        self.log.clear();
        self.end = None;
        self.error = None;
        self.busy = true;
        let code = self.code.clone();
        let breakpoints: Vec<u32> = self.breakpoints.iter().copied().collect();
        let mode = if self.sync { DebugMode::Sync } else { DebugMode::Forked };
        self._task = Some(cx.spawn(async move |handle, cx| {
            let task = cx.background_spawn(async move {
                let at = ServerDb::new(&*server_id, db);
                LuaDebugSession::start(&at, &code, &keys, &args, &breakpoints, mode).await
            });
            let result: Result<(LuaDebugSession, DebugStep)> = task.await.map_err(Into::into);
            let _ = handle.update(cx, |this, cx| {
                this.busy = false;
                match result {
                    Ok((session, step)) => {
                        this.session = Some(session);
                        this.apply(step);
                    }
                    Err(e) => this.error = Some(e.to_string().into()),
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn send(&mut self, command: DebugCommand, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let Some(session) = self.session.clone().filter(LuaDebugSession::is_live) else {
            return;
        };
        self.busy = true;
        self.error = None;
        self._task = Some(cx.spawn(async move |handle, cx| {
            let task = cx.background_spawn(async move { session.send(&command).await });
            let result: Result<DebugStep> = task.await.map_err(Into::into);
            let _ = handle.update(cx, |this, cx| {
                this.busy = false;
                match result {
                    Ok(step) => this.apply(step),
                    Err(e) => {
                        // The connection is gone with whatever was paused on it.
                        this.session = None;
                        this.current_line = None;
                        this.locals.clear();
                        this.error = Some(e.to_string().into());
                    }
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// Take in what one command led to.
    fn apply(&mut self, step: DebugStep) {
        self.log.extend(step.log.into_iter().map(log_entry));
        if self.log.len() > MAX_LOG_LINES {
            let excess = self.log.len() - MAX_LOG_LINES;
            self.log.drain(..excess);
        }
        if let Some(line) = step.stopped_at {
            self.current_line = Some(line);
            self.stop_reason = step.stop_reason.map(SharedString::from);
        }
        if let Some(end) = step.end {
            self.session = None;
            self.current_line = None;
            self.stop_reason = None;
            self.locals.clear();
            self.end = Some(end);
        } else {
            self.locals = step.locals;
        }
    }

    fn toggle_breakpoint(&mut self, line: u32, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let command = if self.breakpoints.remove(&line) {
            DebugCommand::Unbreak(line)
        } else {
            self.breakpoints.insert(line);
            DebugCommand::Break(line)
        };
        if self.is_live() {
            self.send(command, cx);
        } else {
            cx.notify();
        }
    }

    fn clear_breakpoints(&mut self, cx: &mut Context<Self>) {
        if self.busy || self.breakpoints.is_empty() {
            return;
        }
        self.breakpoints.clear();
        if self.is_live() {
            self.send(DebugCommand::ClearBreaks, cx);
        } else {
            cx.notify();
        }
    }

    fn evaluate(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(text) = self.take_expression(window, cx) else {
            return;
        };
        self.send(DebugCommand::Eval(text), cx);
    }

    fn run_redis(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(text) = self.take_expression(window, cx) else {
            return;
        };
        // Quotes as the terminal reads them; an unbalanced one falls back
        // to plain words rather than refusing the line.
        let words = shlex::split(&text).unwrap_or_else(|| text.split_whitespace().map(str::to_string).collect());
        if words.is_empty() {
            return;
        }
        self.send(DebugCommand::Redis(words), cx);
    }

    /// The evaluate box's text, echoed into the output and cleared — `None`
    /// when there is nothing to send or nothing paused to send it to.
    fn take_expression(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<String> {
        if self.busy || !self.is_live() {
            return None;
        }
        let text = self.expr.read(cx).value().trim().to_string();
        if text.is_empty() {
            return None;
        }
        self.log.push((LogKind::Input, text.clone().into()));
        self.expr
            .update(cx, |state, cx| state.set_value(SharedString::default(), window, cx));
        Some(text)
    }

    /// Back to the library. A paused script is aborted first, so a sync
    /// session stops blocking the server now rather than when the socket is
    /// noticed closed.
    fn close(&mut self, cx: &mut Context<Self>) {
        if let Some(session) = self.session.take().filter(LuaDebugSession::is_live) {
            cx.background_spawn(async move {
                let _ = session.send(&DebugCommand::Abort).await;
            })
            .detach();
        }
        let _ = self.library.update(cx, |library, cx| {
            library.debugger = None;
            cx.notify();
        });
    }
}

impl Render for LuaDebugger {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let error_banner = self.error.clone().map(|e| {
            div()
                .px_3()
                .py_2()
                .bg(cx.theme().red.opacity(0.15))
                .child(Label::new(e).text_color(cx.theme().red).text_xs())
        });
        v_flex()
            .flex_1()
            .min_h_0()
            .w_full()
            .child(self.render_toolbar(cx))
            .children(error_banner)
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .items_start()
                    .child(self.render_source(cx))
                    .child(self.render_side(cx)),
            )
    }
}

impl LuaDebugger {
    fn render_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let live = self.is_live();
        let busy = self.busy;
        let paused = live && !busy;
        let can_run = self.server_state.read(cx).can(Capability::EvalScript);
        let muted = cx.theme().muted_foreground;
        let (status, status_color) = if busy {
            (i18n_lua_scripts(cx, "debug_running"), cx.theme().yellow)
        } else if let Some(line) = self.current_line.filter(|_| live) {
            let reason = self
                .stop_reason
                .as_ref()
                .map(|reason| format!(" · {reason}"))
                .unwrap_or_default();
            (
                format!("{} {line}{reason}", i18n_lua_scripts(cx, "debug_paused_at")).into(),
                cx.theme().blue,
            )
        } else if let Some(end) = &self.end {
            if end.error {
                (i18n_lua_scripts(cx, "debug_failed"), cx.theme().red)
            } else {
                (i18n_lua_scripts(cx, "debug_finished"), cx.theme().green)
            }
        } else {
            (i18n_lua_scripts(cx, "debug_idle"), muted)
        };
        let started = live || self.end.is_some();
        h_flex()
            .items_center()
            .gap_2()
            .px_4()
            .py_2()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                Button::new("lua-dbg-close")
                    .ghost()
                    .small()
                    .icon(IconName::ArrowLeft)
                    .tooltip(i18n_lua_scripts(cx, "debug_close"))
                    .on_click(cx.listener(|this, _, _w, cx| this.close(cx))),
            )
            .child(
                Label::new(i18n_lua_scripts(cx, "debug_title"))
                    .text_sm()
                    .text_color(muted),
            )
            .child(
                Label::new(self.name.clone())
                    .text_sm()
                    .text_color(cx.theme().foreground),
            )
            .child(
                div()
                    .px_2()
                    .rounded_sm()
                    .bg(status_color.opacity(0.15))
                    .child(Label::new(status).text_xs().text_color(status_color)),
            )
            .child(div().flex_1())
            .child(
                Button::new("lua-dbg-start")
                    .primary()
                    .small()
                    .icon(IconName::Play)
                    .loading(busy && !live)
                    .label(if started {
                        i18n_lua_scripts(cx, "debug_restart")
                    } else {
                        i18n_lua_scripts(cx, "debug_start")
                    })
                    .disabled(busy || !can_run)
                    .on_click(cx.listener(|this, _, window, cx| this.start(window, cx))),
            )
            .child(
                Button::new("lua-dbg-step")
                    .outline()
                    .small()
                    .icon(IconName::ArrowDown)
                    .label(i18n_lua_scripts(cx, "debug_step"))
                    .disabled(!paused)
                    .on_click(cx.listener(|this, _, _w, cx| this.send(DebugCommand::Step, cx))),
            )
            .child(
                Button::new("lua-dbg-continue")
                    .outline()
                    .small()
                    .icon(IconName::ChevronRight)
                    .label(i18n_lua_scripts(cx, "debug_continue"))
                    .disabled(!paused)
                    .on_click(cx.listener(|this, _, _w, cx| this.send(DebugCommand::Continue, cx))),
            )
            .child(
                Button::new("lua-dbg-abort")
                    .outline()
                    .small()
                    .icon(IconName::Square)
                    .label(i18n_lua_scripts(cx, "debug_abort"))
                    .disabled(!paused)
                    .on_click(cx.listener(|this, _, _w, cx| this.send(DebugCommand::Abort, cx))),
            )
            .child(
                Button::new("lua-dbg-clear")
                    .ghost()
                    .small()
                    .icon(IconName::Delete)
                    .tooltip(i18n_lua_scripts(cx, "debug_clear_breakpoints"))
                    .disabled(busy || self.breakpoints.is_empty())
                    .on_click(cx.listener(|this, _, _w, cx| this.clear_breakpoints(cx))),
            )
    }

    fn render_source(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let foreground = cx.theme().foreground;
        let current_bg = cx.theme().yellow.opacity(0.22);
        let red = cx.theme().red;
        let live = self.is_live();
        let mono = get_mono_font_family();
        let rows: Vec<gpui::AnyElement> = self
            .lines
            .iter()
            .enumerate()
            .map(|(index, text)| {
                let line = index as u32 + 1;
                let is_current = live && self.current_line == Some(line);
                let has_break = self.breakpoints.contains(&line);
                h_flex()
                    .w_full()
                    .items_center()
                    .when(is_current, |row| row.bg(current_bg))
                    .child(
                        h_flex()
                            .id(("lua-dbg-line", line))
                            .flex_none()
                            .w(px(GUTTER_WIDTH))
                            .h_full()
                            .gap_1()
                            .pr_2()
                            .items_center()
                            .justify_end()
                            .cursor_pointer()
                            .hover(|style| style.bg(cx.theme().muted.opacity(0.5)))
                            .on_click(cx.listener(move |this, _, _w, cx| this.toggle_breakpoint(line, cx)))
                            .child(div().size(px(8.)).rounded_full().when(has_break, |dot| dot.bg(red)))
                            .child(
                                Label::new(SharedString::from(line.to_string()))
                                    .font_family(mono.clone())
                                    .text_xs()
                                    .text_color(if is_current { foreground } else { muted }),
                            ),
                    )
                    .child(
                        div().flex_1().min_w_0().pl_2().child(
                            Label::new(if text.is_empty() {
                                SharedString::from(" ")
                            } else {
                                text.clone()
                            })
                            .font_family(mono.clone())
                            .text_xs()
                            .text_color(foreground),
                        ),
                    )
                    .into_any_element()
            })
            .collect();
        v_flex()
            .flex_1()
            .min_w_0()
            .h_full()
            .border_r_1()
            .border_color(cx.theme().border)
            .child(
                div().px_3().py_1().border_b_1().border_color(cx.theme().border).child(
                    Label::new(i18n_lua_scripts(cx, "debug_breakpoint_hint"))
                        .text_xs()
                        .text_color(muted),
                ),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .overflow_y_scrollbar()
                    .child(v_flex().w_full().py_1().children(rows)),
            )
    }

    fn render_side(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let foreground = cx.theme().foreground;
        let live = self.is_live();
        let paused = live && !self.busy;
        let mono = get_mono_font_family();
        let section = |title: SharedString| Label::new(title).text_xs().text_color(muted);

        // KEYS / ARGV and the mode, while nothing is paused.
        let inputs = (!live).then(|| {
            v_flex()
                .gap_2()
                .child(section(i18n_lua_scripts(cx, "keys_label")))
                .child(Textarea::new(&self.keys).appearance(true).text_sm())
                .child(section(i18n_lua_scripts(cx, "args_label")))
                .child(Textarea::new(&self.args).appearance(true).text_sm())
                .child(
                    Checkbox::new("lua-dbg-sync")
                        .checked(self.sync)
                        .label(i18n_lua_scripts(cx, "debug_sync"))
                        .tooltip(i18n_lua_scripts(cx, "debug_sync_tooltip"))
                        .disabled(self.busy)
                        .on_click(cx.listener(|this, checked: &bool, _w, cx| {
                            this.sync = *checked;
                            cx.notify();
                        })),
                )
        });
        let mode_note = if self.sync {
            i18n_lua_scripts(cx, "debug_sync_note")
        } else {
            i18n_lua_scripts(cx, "debug_forked_note")
        };

        let locals: gpui::AnyElement = if self.locals.is_empty() {
            Label::new(i18n_lua_scripts(cx, "debug_no_locals"))
                .text_xs()
                .text_color(muted)
                .into_any_element()
        } else {
            v_flex()
                .gap_0p5()
                .children(self.locals.iter().map(|local| {
                    h_flex()
                        .gap_2()
                        .items_start()
                        .child(
                            Label::new(SharedString::from(local.name.clone()))
                                .font_family(mono.clone())
                                .text_xs()
                                .text_color(cx.theme().blue),
                        )
                        .child(
                            Label::new(SharedString::from(local.value.clone()))
                                .font_family(mono.clone())
                                .text_xs()
                                .whitespace_normal()
                                .text_color(foreground),
                        )
                }))
                .into_any_element()
        };

        let output: gpui::AnyElement = if self.log.is_empty() {
            Label::new(i18n_lua_scripts(cx, "debug_no_output"))
                .text_xs()
                .text_color(muted)
                .into_any_element()
        } else {
            v_flex()
                .gap_0p5()
                .children(self.log.iter().map(|(kind, text)| {
                    let color = match kind {
                        LogKind::Error => cx.theme().red,
                        LogKind::RetVal | LogKind::RedisReply => cx.theme().green,
                        LogKind::Input | LogKind::RedisCall => cx.theme().blue,
                        LogKind::Debug => cx.theme().yellow,
                        LogKind::Info => muted,
                    };
                    h_flex()
                        .gap_2()
                        .items_start()
                        .when(!kind.tag().is_empty(), |row| {
                            row.child(
                                Label::new(kind.tag())
                                    .font_family(mono.clone())
                                    .text_xs()
                                    .text_color(color),
                            )
                        })
                        .child(
                            Label::new(text.clone())
                                .font_family(mono.clone())
                                .text_xs()
                                .whitespace_normal()
                                .text_color(foreground),
                        )
                }))
                .into_any_element()
        };

        let result = self.end.as_ref().map(|end| {
            let color = if end.error { cx.theme().red } else { cx.theme().green };
            v_flex()
                .gap_1()
                .child(section(i18n_lua_scripts(cx, "result_label")))
                .child(
                    div().px_2().py_1().rounded_sm().bg(color.opacity(0.12)).child(
                        Label::new(SharedString::from(end.result.clone()))
                            .font_family(mono.clone())
                            .text_xs()
                            .whitespace_normal()
                            .text_color(foreground),
                    ),
                )
        });

        v_flex()
            .flex_none()
            .w(px(SIDE_WIDTH))
            .h_full()
            .child(
                div().flex_1().min_h_0().w_full().overflow_y_scrollbar().child(
                    v_flex()
                        .gap_3()
                        .p_3()
                        .w_full()
                        .children(inputs)
                        .child(Label::new(mode_note).text_xs().text_color(muted))
                        .children(result)
                        .child(
                            v_flex()
                                .gap_1()
                                .child(section(i18n_lua_scripts(cx, "debug_locals")))
                                .child(locals),
                        )
                        .child(
                            v_flex()
                                .gap_1()
                                .child(section(i18n_lua_scripts(cx, "debug_output")))
                                .child(output),
                        ),
                ),
            )
            .child(
                v_flex()
                    .gap_2()
                    .p_3()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .child(Input::new(&self.expr).appearance(true).small().disabled(!paused))
                    .child(
                        h_flex()
                            .gap_2()
                            .justify_end()
                            .child(
                                Button::new("lua-dbg-eval")
                                    .outline()
                                    .small()
                                    .label(i18n_lua_scripts(cx, "debug_eval"))
                                    .disabled(!paused)
                                    .on_click(cx.listener(|this, _, window, cx| this.evaluate(window, cx))),
                            )
                            .child(
                                Button::new("lua-dbg-redis")
                                    .outline()
                                    .small()
                                    .icon(Icon::new(IconName::SquareTerminal))
                                    .label(i18n_lua_scripts(cx, "debug_run_redis"))
                                    .disabled(!paused)
                                    .on_click(cx.listener(|this, _, window, cx| this.run_redis(window, cx))),
                            ),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::init_ui;
    use gpui::{AppContext, TestAppContext, size};
    use gpui_kit::test::TestWindowExt;

    fn script() -> LuaScript {
        LuaScript {
            name: "incr_with_ttl".to_string(),
            code: "local n = redis.call('INCR', KEYS[1])\n\tif n == 1 then\nend\nreturn n".to_string(),
            default_keys: vec!["counter".to_string()],
            ..LuaScript::default()
        }
    }

    /// The page opened on a library, as the Debug button opens it.
    fn open(
        cx: &mut TestAppContext,
    ) -> (
        gpui::AnyWindowHandle,
        Entity<ZedisLuaScriptLibrary>,
        Entity<LuaDebugger>,
    ) {
        init_ui(cx);
        let server_state = cx.new(|_| ZedisServerState::default());
        let mut built = None;
        let handle = cx.open_window(size(px(1000.), px(700.)), |window, cx| {
            let library = cx.new(|cx| ZedisLuaScriptLibrary::new(server_state.clone(), window, cx));
            let debugger =
                cx.new(|cx| LuaDebugger::new(library.downgrade(), server_state.clone(), &script(), window, cx));
            library.update(cx, |library, _| library.debugger = Some(debugger.clone().into()));
            built = Some((library.clone(), debugger));
            gpui_kit::component::Root::new(library, window, cx)
        });
        let (library, debugger) = built.expect("the window built its views");
        (handle.into(), library, debugger)
    }

    #[gpui::test]
    fn the_source_is_drawn_line_by_line_and_a_line_number_toggles_a_breakpoint(cx: &mut TestAppContext) {
        let (handle, _library, debugger) = open(cx);
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            let debugger = debugger.read(cx);
            assert_eq!(debugger.lines.len(), 4);
            assert_eq!(
                debugger.lines[1].as_ref(),
                "    if n == 1 then",
                "a tab is drawn as spaces"
            );
        })
        .expect("the window is open");

        // Nothing is paused: the toggles only change what the next start sets.
        debugger.update(cx, |debugger, cx| {
            debugger.toggle_breakpoint(2, cx);
            debugger.toggle_breakpoint(4, cx);
            debugger.toggle_breakpoint(2, cx);
        });
        debugger.read_with(cx, |debugger, _| {
            assert_eq!(debugger.breakpoints.iter().copied().collect::<Vec<_>>(), [4]);
            assert!(!debugger.busy, "no session, nothing sent");
        });
        debugger.update(cx, |debugger, cx| debugger.clear_breakpoints(cx));
        debugger.read_with(cx, |debugger, _| assert!(debugger.breakpoints.is_empty()));
    }

    #[gpui::test]
    fn a_step_moves_the_paused_line_and_the_end_shows_the_result(cx: &mut TestAppContext) {
        let (handle, _library, debugger) = open(cx);
        debugger.update(cx, |debugger, _| {
            debugger.apply(DebugStep {
                stopped_at: Some(2),
                stop_reason: Some("step over".to_string()),
                locals: vec![LuaLocal {
                    name: "n".to_string(),
                    value: "1".to_string(),
                }],
                ..DebugStep::default()
            });
            // An evaluation says nothing about the line: it stays where it was.
            debugger.apply(DebugStep {
                log: vec![DebugLine::RetVal("2".to_string())],
                locals: vec![LuaLocal {
                    name: "n".to_string(),
                    value: "1".to_string(),
                }],
                ..DebugStep::default()
            });
            assert_eq!(debugger.current_line, Some(2));
            assert_eq!(debugger.locals.len(), 1);
            assert_eq!(debugger.log, [(LogKind::RetVal, SharedString::from("2"))]);

            debugger.apply(DebugStep {
                end: Some(DebugEnd {
                    result: "(integer) 1".to_string(),
                    error: false,
                }),
                ..DebugStep::default()
            });
            assert_eq!(debugger.current_line, None);
            assert!(debugger.locals.is_empty());
            assert!(debugger.end.as_ref().is_some_and(|end| !end.error));
        });
        cx.update_window(handle, |_, window, cx| window.render_frame(cx))
            .expect("the finished page draws");
    }

    #[gpui::test]
    fn the_output_keeps_its_last_lines(cx: &mut TestAppContext) {
        let (_handle, _library, debugger) = open(cx);
        debugger.update(cx, |debugger, _| {
            debugger.apply(DebugStep {
                log: (0..MAX_LOG_LINES + 20)
                    .map(|i| DebugLine::Debug(i.to_string()))
                    .collect(),
                ..DebugStep::default()
            });
            assert_eq!(debugger.log.len(), MAX_LOG_LINES);
            assert_eq!(debugger.log[0].1.as_ref(), "20", "the oldest lines go first");
        });
    }

    /// Start with no server selected says so instead of dialing nothing.
    #[gpui::test]
    fn start_without_a_server_says_so(cx: &mut TestAppContext) {
        let (handle, _library, debugger) = open(cx);
        cx.update_window(handle, |_, window, cx| {
            debugger.update(cx, |debugger, cx| debugger.start(window, cx));
        })
        .expect("the window is open");
        debugger.read_with(cx, |debugger, _| {
            assert!(debugger.session.is_none() && !debugger.busy);
            assert!(debugger.error.is_some(), "the page says why nothing started");
        });
    }

    /// Closing hands the page back to the library's list.
    #[gpui::test]
    fn closing_returns_to_the_library(cx: &mut TestAppContext) {
        let (handle, library, debugger) = open(cx);
        debugger.update(cx, |debugger, cx| debugger.close(cx));
        library.read_with(cx, |library, _| assert!(library.debugger.is_none()));
        cx.update_window(handle, |_, window, cx| window.render_frame(cx))
            .expect("the list draws again");
    }
}
