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
    connection::{
        DangerKind, RedisAsyncConn, ReplyFormat, classify_dangerous_line, command_doc_url, format_exec, format_reply,
        get_command_description, get_connection_manager, get_server, is_write_command, list_commands,
        requires_write_confirm,
    },
    db::get_cmd_history_manager,
    error::{ConnectionErrorKind, Error},
    helpers::{
        AiEndpoint, TerminalAction, get_download_dir, get_mono_font_family, get_or_create_config_dir,
        starts_with_ignore_ascii_case, suggest_command, write_file_atomic,
    },
    states::{ServerEvent, ZedisGlobalStore, ZedisServerState, update_app_state_and_save_quiet},
    views::confirm_dangerous_command,
};
use chrono::Local;
use gpui::{ClipboardItem, Entity, SharedString, Subscription, Task, Window, div, prelude::*, px};
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
use redis::{Value, cmd};
use smol::lock::Mutex;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;
use tracing::{error, info, warn};

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

/// Shown when a blocking command is typed in the terminal (see
/// [`is_blocking_command`]). Hardcoded English to match the rest of this
/// panel, which is not internationalized.
const BLOCKING_REJECT_MSG: &str = "Blocking commands (BLPOP / BRPOP / BLMOVE / BRPOPLPUSH / BLMPOP / BZPOPMIN / BZPOPMAX / BZMPOP / XREAD BLOCK / XREADGROUP BLOCK / WAIT / WAITAOF) are not run here: they would park the terminal's connection until data arrives, and the response timeout would cut the wait short and leave the connection out of step with its replies. Use the live key tail or the Monitor view for blocking reads.";

/// Whether a parsed command would park the Redis connection until data
/// arrives or it times out. The terminal has a connection of its own, so a
/// blocking command no longer stalls the key tree — but it would still hang
/// every later line until data arrives, and the response timeout would break
/// its semantics anyway. Refuse up front.
fn is_blocking_command(cmd_name: &str, args: &[String]) -> bool {
    const ALWAYS_BLOCKING: &[&str] = &[
        "BLPOP",
        "BRPOP",
        "BLMOVE",
        "BRPOPLPUSH",
        "BLMPOP",
        "BZPOPMIN",
        "BZPOPMAX",
        "BZMPOP",
        "WAIT",
        "WAITAOF",
    ];
    let verb = cmd_name.to_ascii_uppercase();
    if ALWAYS_BLOCKING.contains(&verb.as_str()) {
        return true;
    }
    // XREAD / XREADGROUP only block when given a BLOCK option.
    if verb == "XREAD" || verb == "XREADGROUP" {
        return args.iter().any(|a| a.eq_ignore_ascii_case("BLOCK"));
    }
    false
}

/// Commands pasted from docs or a shell history often carry a leading
/// `redis-cli`; drop that word so the rest of the line runs as a plain Redis
/// command. Any connection flags after it (`-h`, `-p`, …) are left untouched —
/// the terminal already talks to the selected server, and a pasted flag
/// failing loudly beats silently pretending to honor it.
fn strip_redis_cli_prefix(line: &str) -> &str {
    let trimmed = line.trim_start();
    match trimmed.split_once(char::is_whitespace) {
        Some((first, rest)) if first.eq_ignore_ascii_case("redis-cli") => rest.trim_start(),
        None if trimmed.eq_ignore_ascii_case("redis-cli") => "",
        _ => line,
    }
}

/// What one executed line produced: the reply — kept as a value, so the
/// output can be re-rendered in another format — and the db a successful
/// `SELECT` moved the terminal's connection to.
#[derive(Default)]
struct LineOutcome {
    reply: LineReply,
    selected_db: Option<usize>,
}

/// The server's answer to one line, or the text shown in its place.
enum LineReply {
    /// The reply, with the command that produced it: `format_reply` needs
    /// the command to tell a hash or `WITHSCORES` pair list from a plain
    /// list (RESP2 flattens both).
    Value {
        cmd: String,
        args: Vec<String>,
        value: Value,
    },
    /// An error, or a line refused before it reached the server — verbatim.
    Message(String),
}

impl Default for LineReply {
    fn default() -> Self {
        LineReply::Message(String::new())
    }
}

/// One block of the output pane. The transcript is kept structured rather
/// than as text so a format switch re-renders every reply, `EXEC` can be
/// laid out against the commands it ran, and the AI placeholder can be
/// found again while commands keep landing around it.
enum TranscriptEntry {
    /// Banner, notes, AI lines: verbatim.
    Text(String),
    Command {
        line: String,
        reply: LineReply,
    },
    /// A `MULTI … EXEC` block: the queued commands beside EXEC's replies.
    Exec {
        commands: Vec<String>,
        replies: Vec<Value>,
    },
    /// Footer of a multi-line (Batch / pasted pipeline) run.
    BatchSummary {
        commands: usize,
        errors: usize,
        elapsed_ms: u128,
    },
    /// A `?` request in flight; replaced by its answer.
    AiPending,
}

/// Entries kept; the oldest go first. The rendered text is capped again by
/// [`MAX_OUTPUT_CHARS`], so a few huge replies can't keep 1000 blocks alive.
const MAX_TRANSCRIPT_ENTRIES: usize = 1_000;

/// The output pane's text for `entries` in `format`.
fn render_transcript(entries: &[TranscriptEntry], format: ReplyFormat) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    for entry in entries {
        match entry {
            TranscriptEntry::Text(text) => {
                out.push_str(text);
                if !text.ends_with('\n') {
                    out.push('\n');
                }
            }
            TranscriptEntry::Command { line, reply } => {
                let _ = writeln!(out, "{CMD_LABEL} {line}");
                match reply {
                    LineReply::Value { cmd, args, value } => {
                        let _ = writeln!(out, "{}", format_reply(cmd, args, value, format));
                    }
                    LineReply::Message(message) => {
                        let _ = writeln!(out, "{message}");
                    }
                }
            }
            TranscriptEntry::Exec { commands, replies } => {
                let _ = writeln!(out, "{CMD_LABEL} EXEC");
                let _ = writeln!(out, "{}", format_exec(commands, replies, format));
            }
            TranscriptEntry::BatchSummary {
                commands,
                errors,
                elapsed_ms,
            } => {
                let _ = writeln!(out, "── batch: {commands} commands · {errors} errors · {elapsed_ms} ms");
            }
            TranscriptEntry::AiPending => {
                let _ = writeln!(out, "{AI_WAITING_MSG}");
            }
        }
    }
    out
}

/// Fold one executed line into the `MULTI` bookkeeping and produce its
/// transcript entries. `queue` is `Some` between a successful `MULTI` and
/// its `EXEC` / `DISCARD`; every line Redis answers `QUEUED` joins it, and
/// `EXEC`'s array reply is then laid out against those commands as one
/// [`TranscriptEntry::Exec`] block instead of a bare list. `EXEC` after a
/// `WATCH` conflict answers nil — said in words, since a bare `(nil)` reads
/// as a missing key.
fn transcript_entries_for(queue: &mut Option<Vec<String>>, line: String, reply: LineReply) -> Vec<TranscriptEntry> {
    if let LineReply::Value { cmd, value, .. } = &reply {
        match cmd.to_ascii_uppercase().as_str() {
            "MULTI" if matches!(value, Value::Okay) => *queue = Some(Vec::new()),
            "DISCARD" => *queue = None,
            "EXEC" => match (queue.take(), value) {
                (Some(commands), Value::Array(replies)) => {
                    return vec![TranscriptEntry::Exec {
                        commands,
                        replies: replies.clone(),
                    }];
                }
                (Some(_), Value::Nil) => {
                    return vec![
                        TranscriptEntry::Command { line, reply },
                        TranscriptEntry::Text("(transaction aborted: a key under WATCH changed)".to_string()),
                    ];
                }
                _ => {}
            },
            _ => {
                if let Some(queued) = queue.as_mut()
                    && matches!(value, Value::SimpleString(s) if s == "QUEUED")
                {
                    queued.push(line.clone());
                }
            }
        }
    }
    vec![TranscriptEntry::Command { line, reply }]
}

/// Writes the output pane's text to `zedis-terminal-<stamp>.txt` in
/// Downloads (the config dir when there is none — App Store sandbox) and
/// returns its path.
fn write_output_file(text: &str) -> io::Result<PathBuf> {
    let dir = get_download_dir()
        .or_else(|| get_or_create_config_dir().ok())
        .ok_or_else(|| io::Error::other("no directory to write the output to"))?;
    let path = dir.join(format!("zedis-terminal-{}.txt", Local::now().format("%Y%m%d-%H%M%S")));
    write_file_atomic(&path, text.as_bytes())?;
    Ok(path)
}

/// The db a successful `SELECT <n>` moved the connection to, else `None`.
/// Only the plain one-argument form counts: anything else Redis accepted
/// was not a database switch.
fn selected_db(cmd_name: &str, args: &[String], reply: &redis::Value) -> Option<usize> {
    if !cmd_name.eq_ignore_ascii_case("SELECT") || !matches!(reply, redis::Value::Okay) {
        return None;
    }
    match args {
        [db] => db.parse().ok(),
        _ => None,
    }
}

/// Whether an error means the terminal's connection is gone and the next
/// line must reopen it. Mirrors what the pool does with its own client
/// (`note_link_error`): a dropped link, refused connect or broken tunnel
/// discards the connection; a response timeout does not — the multiplexed
/// connection stays in step after one, and a dead link surfaces as a
/// network error on the next line anyway.
fn drops_link(err: &Error) -> bool {
    use ConnectionErrorKind as K;
    matches!(err.connection_kind(), K::Network | K::Tls | K::Tunnel)
}

/// The terminal's connection, opened on first use. Cloning a
/// `RedisAsyncConn` shares the underlying socket, so every line — and every
/// later batch — sees the same connection state: the db a `SELECT` picked,
/// a `MULTI` still open.
async fn terminal_connection(
    slot: &Mutex<Option<RedisAsyncConn>>,
    server_id: &str,
    db: usize,
) -> Result<RedisAsyncConn> {
    let mut slot = slot.lock().await;
    if let Some(conn) = slot.as_ref() {
        return Ok(conn.clone());
    }
    let conn = get_connection_manager()
        .open_dedicated_connection(server_id, db)
        .await?;
    *slot = Some(conn.clone());
    Ok(conn)
}

/// The saved command history for `server_id`; an unreadable local
/// database reads as empty, with the cause in the log rather than a
/// silently amnesiac ↑ key.
fn command_history(server_id: &str) -> Vec<String> {
    get_cmd_history_manager().records(server_id).unwrap_or_else(|e| {
        warn!(error = %e, "command history unavailable");
        Vec::new()
    })
}

/// Scrollback cap for the rendered output text. The whole text is
/// re-rendered and handed to the read-only editor on every command, so
/// leaving it unbounded made a long session O(n²) in total output. ~200 KB
/// ≈ a few thousand lines.
const MAX_OUTPUT_CHARS: usize = 200_000;

/// Placeholder line shown in the output while a `?` AI request is in
/// flight; removed when the reply (or error) lands. Distinctive on
/// purpose so [`remove_last_line`] can't clip user output.
const AI_WAITING_MSG: &str = "AI> … thinking — this can take a few seconds; you can keep running commands meanwhile";

/// Drop the oldest output once the scrollback buffer exceeds
/// [`MAX_OUTPUT_CHARS`], cutting at a line boundary so the top stays clean.
fn trim_output_scrollback(buf: &mut String) {
    if buf.len() <= MAX_OUTPUT_CHARS {
        return;
    }
    let target = buf.len() - MAX_OUTPUT_CHARS;
    // `\n` is ASCII (always a valid char boundary), so draining up to and
    // including it keeps `buf` a valid `String`.
    if let Some(i) = buf.as_bytes()[target..].iter().position(|&b| b == b'\n') {
        buf.drain(..target + i + 1);
    }
}

/// Active `Ctrl+R` reverse-history-search session. `matches` is the command
/// history filtered by the current query, newest-first; `index` is the match
/// currently shown, advanced (toward older) by repeated `Ctrl+R`.
struct ReverseSearchState {
    matches: Vec<SharedString>,
    index: usize,
}

pub struct ZedisTerminal {
    server_state: Entity<ZedisServerState>,
    cmd_output_state: Entity<EditorState>,
    /// What the output pane shows, as blocks (see [`TranscriptEntry`]).
    transcript: Vec<TranscriptEntry>,
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
    ai_task: Option<Task<()>>,
    /// AI-suggested command waiting to be placed into the input box —
    /// consumed by `render`, which has the `Window` that `set_value`
    /// needs. The user reviews and hits Enter; nothing auto-executes.
    pending_ai_fill: Option<SharedString>,
    /// The terminal's own connection — never the pooled one the key tree
    /// scans on, so `SELECT` / `AUTH` / `CLIENT SETNAME` / `MULTI` typed
    /// here reach nothing else (see
    /// `ConnectionManager::open_dedicated_connection`). Opened lazily by
    /// the first line and shared by every later one, replaced on a server
    /// or db switch, and cleared after a link error so the next line
    /// reconnects. Behind an async lock because each line runs on its own
    /// background task.
    conn: Arc<Mutex<Option<RedisAsyncConn>>>,
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
            ai_task: None,
            pending_ai_fill: None,
            conn: Arc::new(Mutex::new(None)),
            terminal_db: None,
            _subscriptions: subscriptions,
        };
        this.reset_cmd_state(cx);
        this.update_redis_commands(cx);
        this
    }

    /// Forget the terminal's connection; the next line opens a fresh one on
    /// the panel's db. A new `Arc` rather than `take()`, so a line still in
    /// flight on the old connection can't write it back into the slot.
    fn drop_connection(&mut self) {
        self.conn = Arc::new(Mutex::new(None));
        self.terminal_db = None;
    }

    fn reset_cmd_state(&mut self, _cx: &mut Context<Self>) {
        self.transcript.clear();
        self.multi_queue = None;
        self.push_entry(TranscriptEntry::Text(ZEDIS_LOGO.replace("{VERSION}", VERSION)));
        // Hardcoded English like the rest of this panel (not i18n'd).
        self.push_entry(TranscriptEntry::Text(
            "Type \"? <question>\" to ask AI for a command (endpoint configured in Settings).".to_string(),
        ));
    }

    /// Append to the transcript, dropping the oldest blocks past the cap,
    /// and schedule a re-render of the output text.
    fn push_entry(&mut self, entry: TranscriptEntry) {
        self.transcript.push(entry);
        if self.transcript.len() > MAX_TRANSCRIPT_ENTRIES {
            let excess = self.transcript.len() - MAX_TRANSCRIPT_ENTRIES;
            self.transcript.drain(..excess);
        }
        self.cmd_output_dirty = true;
    }

    /// Switch the reply rendering: every block re-renders, and the choice
    /// is remembered for the next session.
    fn set_reply_format(&mut self, format: ReplyFormat, cx: &mut Context<Self>) {
        if self.reply_format == format {
            return;
        }
        self.reply_format = format;
        self.cmd_output_dirty = true;
        update_app_state_and_save_quiet(cx, "save_terminal_reply_format", move |state, _| {
            state.set_terminal_reply_format(format);
        });
        cx.notify();
    }

    fn copy_output(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(self.cmd_output_text.clone()));
        window.push_notification(Notification::success("Output copied"), cx);
    }

    fn save_output(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match write_output_file(&self.cmd_output_text) {
            Ok(path) => {
                info!(path = %path.display(), "terminal output saved");
                window.push_notification(Notification::success(format!("Output saved to {}", path.display())), cx);
                cx.reveal_path(&path);
            }
            Err(e) => {
                error!(error = %e, "terminal output save failed");
                window.push_notification(Notification::error(format!("Saving the output failed: {e}")), cx);
            }
        }
    }

    /// The output pane's right-click commands beyond the editor's own.
    fn handle_action(&mut self, action: &TerminalAction, window: &mut Window, cx: &mut Context<Self>) {
        match action {
            TerminalAction::CopyAll => self.copy_output(window, cx),
            TerminalAction::Save => self.save_output(window, cx),
            TerminalAction::Clear => {
                self.reset_cmd_state(cx);
                cx.notify();
            }
        }
    }

    fn update_redis_commands(&mut self, cx: &mut Context<Self>) {
        self.redis_commands = list_commands(self.server_state.read(cx).version())
            .into_iter()
            .map(Into::into)
            .collect();
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
                // Move the caret to the end of the completed command so the
                // user can keep typing args (mirrors history navigation).
                state.set_cursor_position(Position::new(0, u32::MAX), window, cx);
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
        let records = command_history(server_id);
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

    /// History records newest-first, filtered by `query` (case-insensitive
    /// substring; an empty query keeps everything).
    fn reverse_search_matches(&self, query: &str, cx: &Context<Self>) -> Vec<SharedString> {
        let server_id = self.server_state.read(cx).server_id();
        if server_id.is_empty() {
            return Vec::new();
        }
        let records = command_history(server_id);
        let q = query.to_lowercase();
        records
            .into_iter()
            .rev()
            .filter(|r| q.is_empty() || r.to_lowercase().contains(&q))
            .map(Into::into)
            .collect()
    }

    /// `Ctrl+R`: open the reverse-search overlay on the first press, then
    /// step to the next (older) match on each subsequent press.
    fn enter_or_advance_reverse_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.reverse_search.is_some() {
            self.step_reverse_search(true, cx);
            return;
        }
        let matches = self.reverse_search_matches("", cx);
        self.reverse_search = Some(ReverseSearchState { matches, index: 0 });
        // The command-completion dropdown and history cursor belong to the
        // REPL input; clear them so the overlay starts clean.
        self.cmd_suggestions.clear();
        self.cmd_suggestion_index = None;
        self.cmd_history_index = None;
        self.search_input_state.update(cx, |state, cx| {
            state.set_value(SharedString::default(), window, cx);
            state.focus(window, cx);
        });
        cx.notify();
    }

    /// Move the highlighted match: `older` walks toward earlier history (↑ or
    /// a repeated Ctrl+R), otherwise toward more recent (↓). Clamped at both
    /// ends; no-op when not searching.
    fn step_reverse_search(&mut self, older: bool, cx: &mut Context<Self>) {
        if let Some(state) = &mut self.reverse_search {
            let next = if older {
                (state.index + 1).min(state.matches.len().saturating_sub(1))
            } else {
                state.index.saturating_sub(1)
            };
            if next != state.index {
                state.index = next;
                cx.notify();
            }
        }
    }

    /// Re-filter for a changed query, snapping back to the newest match.
    fn update_reverse_search_matches(&mut self, query: &str, cx: &mut Context<Self>) {
        let matches = self.reverse_search_matches(query, cx);
        if let Some(state) = &mut self.reverse_search {
            state.matches = matches;
            state.index = 0;
        }
    }

    fn current_reverse_match(&self) -> Option<SharedString> {
        let state = self.reverse_search.as_ref()?;
        state.matches.get(state.index).cloned()
    }

    /// Accept the highlighted match into the command input for review — it is
    /// deliberately not auto-run, so the danger-command confirm still gates the
    /// eventual Enter. No match ⇒ just close the overlay.
    fn accept_reverse_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let matched = self.current_reverse_match();
        self.reverse_search = None;
        self.cmd_input_state.update(cx, |state, cx| {
            if let Some(matched) = matched {
                state.set_value(matched, window, cx);
                state.set_cursor_position(Position::new(0, u32::MAX), window, cx);
            }
            state.focus(window, cx);
        });
        cx.notify();
    }

    /// `Esc`: close the overlay, leaving the command input untouched.
    fn cancel_reverse_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.reverse_search = None;
        self.cmd_input_state.update(cx, |state, cx| state.focus(window, cx));
        cx.notify();
    }

    fn execute_command(&mut self, command: SharedString, window: &mut Window, cx: &mut Context<Self>) {
        if command.is_empty() {
            return;
        }
        // Clear the output pane like a terminal would — tolerant of case and
        // surrounding whitespace so `CLEAR` / `clear ` work too (they'd
        // otherwise be sent to Redis as an unknown command).
        if command.trim().eq_ignore_ascii_case(CMD_CLEAR) {
            self.reset_cmd_state(cx);
            cx.notify();
            return;
        }
        // `? <question>` — AI command assistant: the answer lands in the
        // input box for review, never on the connection.
        if let Some(question) = command.trim().strip_prefix('?') {
            self.ask_ai(question.trim().to_string(), cx);
            return;
        }
        // Strip any `redis-cli` prefix up front so the danger classifier, the
        // executor, and the recorded history all see the real command.
        let command: SharedString = if command.lines().any(|line| strip_redis_cli_prefix(line) != line) {
            command
                .lines()
                .map(strip_redis_cli_prefix)
                .collect::<Vec<_>>()
                .join("\n")
                .into()
        } else {
            command
        };
        let server_id = self.server_state.read(cx).server_id().to_string();

        // Look for the first line that needs a confirm. If any line trips the
        // classifier (or the server requires confirm-on-write and the line is
        // a write), gate the whole multi-line input behind one dialog.
        if let Ok(server) = get_server(&server_id) {
            let confirm_writes = requires_write_confirm(&server);
            let mut blocking: Option<(String, DangerKind)> = None;
            for raw_line in command.lines() {
                let line = raw_line.trim();
                if line.is_empty() {
                    continue;
                }
                if let Some(kind) = classify_dangerous_line(line) {
                    blocking = Some((line.to_string(), kind));
                    break;
                }
                if confirm_writes
                    && let Some(parts) = shlex::split(line)
                    && let Some(cmd_name) = parts.first()
                    && is_write_command(cmd_name)
                {
                    blocking = Some((line.to_string(), DangerKind::GenericWrite));
                    break;
                }
            }
            if let Some((line, kind)) = blocking {
                let entity = cx.entity().downgrade();
                let command_for_run = command.clone();
                confirm_dangerous_command(&server, &kind, Some(&line), window, cx, move |_, cx| {
                    let Some(this) = entity.upgrade() else { return };
                    this.update(cx, |this, cx| this.run_command_lines(command_for_run.clone(), cx));
                });
                return;
            }
        }
        self.run_command_lines(command, cx);
    }

    /// `? <question>` handler: ask the configured AI endpoint for the
    /// matching Redis command. The suggestion is placed into the input box
    /// (via `pending_ai_fill`) and its explanation echoed to the output —
    /// execution stays with the user, so the danger-confirm and read-only
    /// gates apply unchanged when they hit Enter.
    ///
    /// Privacy: only the question plus server *metadata* (version,
    /// deployment type, modules, current db) are sent — never key values.
    fn ask_ai(&mut self, question: String, cx: &mut Context<Self>) {
        if question.is_empty() {
            return;
        }
        let store = cx.global::<ZedisGlobalStore>().read(cx);
        if !store.ai_configured() {
            self.push_entry(TranscriptEntry::Text(format!(
                "? {question}\nAI endpoint is not configured. Set the base URL in Settings first (an API key only if the endpoint needs one)."
            )));
            cx.notify();
            return;
        }
        let endpoint = AiEndpoint {
            base_url: store.ai_base_url(),
            api_key: store.ai_api_key(),
            model: store.ai_model(),
        };
        let locale = store.locale().to_string();

        let state = self.server_state.read(cx);
        let description = state.nodes_description();
        let server_context = format!(
            "Redis version {}; deployment: {}; modules: [{}]; current db: {}",
            state.version(),
            description.server_type.as_str(),
            description.modules,
            self.terminal_db.unwrap_or(state.db()),
        );

        // A second `?` while one is pending replaces the task (its
        // completion never runs) — clear the previous placeholder so it
        // can't linger in the scrollback forever.
        self.transcript
            .retain(|entry| !matches!(entry, TranscriptEntry::AiPending));
        self.push_entry(TranscriptEntry::Text(format!("? {question}")));
        self.push_entry(TranscriptEntry::AiPending);
        cx.notify();

        self.ai_task = Some(cx.spawn(async move |handle, cx| {
            // Blocking ureq call — keep it on the background pool.
            let result = cx
                .background_spawn(async move { suggest_command(&endpoint, &question, &server_context, &locale) })
                .await;
            let _ = handle.update(cx, |this, cx| {
                // The reply (or error) replaces the waiting placeholder.
                this.transcript
                    .retain(|entry| !matches!(entry, TranscriptEntry::AiPending));
                match result {
                    Ok(reply) => {
                        for command in &reply.commands {
                            this.push_entry(TranscriptEntry::Text(format!("AI> {command}")));
                        }
                        if !reply.explanation.is_empty() {
                            this.push_entry(TranscriptEntry::Text(reply.explanation.clone()));
                        }
                        // Single command goes straight to the input box for
                        // review; a multi-command answer stays in the output
                        // (the REPL input is one line — use Batch to run all).
                        if let Some(first) = reply.commands.first()
                            && reply.commands.len() == 1
                        {
                            this.pending_ai_fill = Some(first.clone().into());
                        }
                    }
                    Err(e) => {
                        this.push_entry(TranscriptEntry::Text(format!("AI error: {e}")));
                    }
                }
                this.cmd_output_dirty = true;
                cx.notify();
            });
        }));
    }

    fn run_command_lines(&mut self, command: SharedString, cx: &mut Context<Self>) {
        let server_state = self.server_state.read(cx);
        let server_id = server_state.server_id().to_string();
        let db = server_state.db();
        let conn_slot = self.conn.clone();
        let lines: Vec<String> = command
            .lines()
            .map(|line| line.trim().to_string())
            .filter(|line| !line.is_empty())
            .collect();
        // A pasted pipeline or a Batch run: the lines stream in one by one
        // as before, then a footer sums them up.
        let batch = lines.len() > 1;
        cx.spawn(async move |handle, cx| {
            let started = Instant::now();
            let mut errors = 0usize;
            let total = lines.len();
            for line in lines {
                let line_clone = line.clone();
                let server_id = server_id.clone();
                let conn_slot = conn_slot.clone();
                let task = cx.background_spawn(async move {
                    let Some(parts) = shlex::split(&line) else {
                        return Ok(LineOutcome::default());
                    };
                    if parts.is_empty() {
                        return Ok(LineOutcome::default());
                    }
                    let cmd_name = parts[0].clone();
                    let args = parts[1..].to_vec();
                    // Refuse blocking commands before touching the connection —
                    // they would park it until data arrives.
                    if is_blocking_command(&cmd_name, &args) {
                        return Ok(LineOutcome {
                            reply: LineReply::Message(BLOCKING_REJECT_MSG.to_string()),
                            selected_db: None,
                        });
                    }
                    let mut conn = terminal_connection(&conn_slot, &server_id, db).await?;
                    let data: redis::Value = match cmd(&cmd_name).arg(&args).query_async(&mut conn).await {
                        Ok(data) => data,
                        Err(e) => {
                            let e = Error::from(e);
                            // A dead link is forgotten here, so the next line
                            // reconnects instead of failing the same way.
                            if drops_link(&e) {
                                conn_slot.lock().await.take();
                            }
                            return Err(e);
                        }
                    };
                    let _ = get_cmd_history_manager().add_record(server_id.as_str(), line.as_str());
                    let selected_db = selected_db(&cmd_name, &args, &data);
                    Ok(LineOutcome {
                        reply: LineReply::Value {
                            cmd: cmd_name,
                            args,
                            value: data,
                        },
                        selected_db,
                    })
                });
                let result: Result<LineOutcome> = task.await;
                let (reply, selected_db, link_dropped, failed) = match result {
                    Ok(outcome) => (outcome.reply, outcome.selected_db, false, false),
                    Err(e) => (LineReply::Message(e.to_string()), None, drops_link(&e), true),
                };
                if failed {
                    errors += 1;
                }
                let _ = handle.update(cx, |this, cx| {
                    for entry in transcript_entries_for(&mut this.multi_queue, line_clone, reply) {
                        this.push_entry(entry);
                    }
                    if let Some(db) = selected_db {
                        this.terminal_db = Some(db);
                    }
                    // The reconnect lands on the panel's db again — and a
                    // transaction open on the old connection is gone with it.
                    if link_dropped {
                        this.terminal_db = None;
                        this.multi_queue = None;
                    }
                    cx.notify();
                });
            }
            if batch {
                let _ = handle.update(cx, |this, cx| {
                    this.push_entry(TranscriptEntry::BatchSummary {
                        commands: total,
                        errors,
                        elapsed_ms: started.elapsed().as_millis(),
                    });
                    cx.notify();
                });
            }
        })
        .detach();
    }

    fn toggle_batch_mode(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.batch_mode = !self.batch_mode;
        if self.batch_mode {
            self.batch_input_state.update(cx, |state, cx| state.focus(window, cx));
        } else {
            self.cmd_input_state.update(cx, |state, cx| state.focus(window, cx));
        }
        cx.notify();
    }

    /// Run every line of the batch editor as one batch (same path the
    /// single-line REPL uses, which already iterates `command.lines()`).
    fn run_batch(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let value = self.batch_input_state.read(cx).value();
        if value.trim().is_empty() {
            return;
        }
        self.execute_command(value.clone(), window, cx);
        // secondary-Enter inserts a trailing newline before emitting the
        // event; strip it (keeping the rest) so repeated runs don't pile
        // up blank lines, while the script stays put for editing/re-running.
        let trimmed = SharedString::from(value.trim_end().to_string());
        self.batch_input_state
            .update(cx, |state, cx| state.set_value(trimmed, window, cx));
    }
}

impl Render for ZedisTerminal {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if std::mem::take(&mut self.cmd_output_dirty) {
            let mut rendered = render_transcript(&self.transcript, self.reply_format);
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
                            .label(format.label())
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
                            .label("Copy")
                            .ghost()
                            .small()
                            .tooltip("Copy the whole output")
                            .on_click(cx.listener(|this, _, window, cx| this.copy_output(window, cx))),
                    )
                    .child(
                        Button::new("term-save-output")
                            .label("Save")
                            .ghost()
                            .small()
                            .tooltip("Save the output as a text file in Downloads")
                            .on_click(cx.listener(|this, _, window, cx| this.save_output(window, cx))),
                    )
                    .child(
                        Button::new("term-clear-output")
                            .label("Clear")
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
                            .context_menu(|menu, _window, _cx| {
                                menu.menu("Copy", Box::new(Copy))
                                    .menu("Select All", Box::new(SelectAll))
                                    .separator()
                                    .menu("Copy Output", Box::new(TerminalAction::CopyAll))
                                    .menu("Save Output…", Box::new(TerminalAction::Save))
                                    .separator()
                                    .menu("Clear", Box::new(TerminalAction::Clear))
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
                        None => Label::new("(no match)").text_color(muted),
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
                            Label::new("↑/↓ or Ctrl+R · Enter accept · Esc cancel")
                                .text_xs()
                                .text_color(muted),
                        )
                } else {
                    h_flex()
                        .w_full()
                        .items_center()
                        .gap_1()
                        .pr_1()
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
                                .label("Batch")
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
                                        .label("Run")
                                        .primary()
                                        .small()
                                        .on_click(cx.listener(|this, _, window, cx| this.run_batch(window, cx))),
                                )
                                .child(
                                    Label::new("⌘/Ctrl+Enter")
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
    use super::{
        LineReply, ReplyFormat, TranscriptEntry, render_transcript, selected_db, strip_redis_cli_prefix,
        transcript_entries_for,
    };
    use redis::Value;

    fn bulk(s: &str) -> Value {
        Value::BulkString(s.as_bytes().to_vec())
    }
    fn reply(cmd: &str, args: &[&str], value: Value) -> LineReply {
        LineReply::Value {
            cmd: cmd.to_string(),
            args: args.iter().map(|s| s.to_string()).collect(),
            value,
        }
    }

    #[test]
    fn selected_db_reads_only_a_successful_plain_select() {
        let args = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(selected_db("SELECT", &args(&["3"]), &Value::Okay), Some(3));
        assert_eq!(selected_db("select", &args(&["0"]), &Value::Okay), Some(0));
        // The server refused it (out of range, cluster mode) — nothing moved.
        assert_eq!(selected_db("SELECT", &args(&["99"]), &Value::Nil), None);
        // Not a SELECT, or not the one-argument form.
        assert_eq!(selected_db("GET", &args(&["3"]), &Value::Okay), None);
        assert_eq!(selected_db("SELECT", &args(&[]), &Value::Okay), None);
        assert_eq!(selected_db("SELECT", &args(&["3", "x"]), &Value::Okay), None);
        assert_eq!(selected_db("SELECT", &args(&["three"]), &Value::Okay), None);
    }

    #[test]
    fn transcript_re_renders_in_every_format() {
        let entries = vec![
            TranscriptEntry::Text("banner".to_string()),
            TranscriptEntry::Command {
                line: "HGETALL h".to_string(),
                reply: reply("HGETALL", &["h"], Value::Array(vec![bulk("a"), bulk("1")])),
            },
            TranscriptEntry::Command {
                line: "GET missing".to_string(),
                reply: LineReply::Message("(error) boom".to_string()),
            },
        ];
        assert_eq!(
            render_transcript(&entries, ReplyFormat::Text),
            "banner\n$ HGETALL h\n[a, 1]\n$ GET missing\n(error) boom\n"
        );
        assert_eq!(
            render_transcript(&entries, ReplyFormat::Table),
            "banner\n$ HGETALL h\nfield │ value\n──────┼──────\na     │ 1\n$ GET missing\n(error) boom\n"
        );
        assert_eq!(
            render_transcript(&entries, ReplyFormat::Json),
            "banner\n$ HGETALL h\n{\n  \"a\": \"1\"\n}\n$ GET missing\n(error) boom\n"
        );
    }

    #[test]
    fn multi_exec_becomes_one_block_of_commands_and_replies() {
        let mut queue = None;
        let queued = |line: &str| {
            reply(
                line.split(' ').next().unwrap_or_default(),
                &[],
                Value::SimpleString("QUEUED".into()),
            )
        };

        let entries = transcript_entries_for(&mut queue, "MULTI".into(), reply("MULTI", &[], Value::Okay));
        assert!(matches!(entries.as_slice(), [TranscriptEntry::Command { .. }]));
        assert_eq!(queue.as_deref(), Some(&[][..]));

        transcript_entries_for(&mut queue, "SET a 1".into(), queued("SET a 1"));
        transcript_entries_for(&mut queue, "INCR a".into(), queued("INCR a"));
        assert_eq!(queue.as_deref().map(<[String]>::len), Some(2));

        let exec = Value::Array(vec![Value::Okay, Value::Int(2)]);
        let entries = transcript_entries_for(&mut queue, "EXEC".into(), reply("EXEC", &[], exec));
        assert!(queue.is_none(), "EXEC closes the transaction");
        let rendered = render_transcript(&entries, ReplyFormat::Text);
        assert_eq!(
            rendered,
            "$ EXEC\n# │ command │ reply\n──┼─────────┼──────\n1 │ SET a 1 │ OK\n2 │ INCR a  │ 2\n"
        );

        // A WATCH conflict: EXEC answers nil, which is said in words.
        let mut queue = Some(vec!["SET a 1".to_string()]);
        let entries = transcript_entries_for(&mut queue, "EXEC".into(), reply("EXEC", &[], Value::Nil));
        assert!(queue.is_none());
        assert!(render_transcript(&entries, ReplyFormat::Text).contains("key under WATCH changed"));

        // DISCARD drops the queue; EXEC outside a MULTI is a plain command.
        let mut queue = Some(vec!["SET a 1".to_string()]);
        transcript_entries_for(&mut queue, "DISCARD".into(), reply("DISCARD", &[], Value::Okay));
        assert!(queue.is_none());
        let entries = transcript_entries_for(&mut queue, "EXEC".into(), reply("EXEC", &[], Value::Nil));
        assert!(matches!(entries.as_slice(), [TranscriptEntry::Command { .. }]));
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
