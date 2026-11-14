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

//! Command execution, blocking/streaming command refusals, write-gate /
//! danger-confirm checks, `?` AI command assistant, and Batch mode.

use super::transcript::{LineOutcome, LineReply, TranscriptEntry, transcript_entries_for};
use super::*;

/// Shown when a blocking command is typed in the terminal (see
/// [`is_blocking_command`]). Hardcoded English to match the rest of this
/// panel, which is not internationalized.
pub(super) const BLOCKING_REJECT_MSG: &str = "Blocking commands (BLPOP / BRPOP / BLMOVE / BRPOPLPUSH / BLMPOP / BZPOPMIN / BZPOPMAX / BZMPOP / XREAD BLOCK / XREADGROUP BLOCK / WAIT / WAITAOF) are not run here: they would park the terminal's connection until data arrives, and the response timeout would cut the wait short and leave the connection out of step with its replies. Use the live key tail or the Monitor view for blocking reads.";

/// Shown when a line would turn the terminal's connection into a stream it
/// does not read (see [`refusal`]): the terminal sends a command and reads
/// one reply. English like the message above, and for the same reason.
const SUBSCRIBE_REJECT_MSG: &str = "SUBSCRIBE / PSUBSCRIBE / SSUBSCRIBE are not run here: a subscribed connection only receives what is published, which a terminal that reads one reply per command never shows, and every later line would be refused until UNSUBSCRIBE.";
const MONITOR_REJECT_MSG: &str = "MONITOR is not run here: it turns the connection into a stream of every command the server receives, which a terminal that reads one reply per command cannot show.";
const REPLICATION_REJECT_MSG: &str =
    "SYNC / PSYNC are not run here: they turn the connection into a replication stream, not a reply.";

/// Why a parsed line is never sent, if it is one the terminal cannot run: a
/// command that parks the connection ([`is_blocking_command`]) or one that
/// turns it into a stream — a subscription, `MONITOR`, a replication
/// handshake. `SUBSCRIBE` used to be sent like any other line: it answered
/// `[subscribe, channel, 1]`, nothing published ever appeared, and the next
/// line was refused by the server until `UNSUBSCRIBE` (#191). Where the app
/// has a panel for the job, the reason names it.
pub(crate) fn refusal(cmd_name: &str, args: &[String]) -> Option<String> {
    if is_blocking_command(cmd_name, args) {
        return Some(BLOCKING_REJECT_MSG.to_string());
    }
    let (reason, instead) = match cmd_name.to_ascii_uppercase().as_str() {
        "SUBSCRIBE" | "PSUBSCRIBE" | "SSUBSCRIBE" => (
            SUBSCRIBE_REJECT_MSG,
            " Use Pubsub Mode instead (≡ › Observability › Pubsub Mode).",
        ),
        "MONITOR" => (
            MONITOR_REJECT_MSG,
            " Use Live Monitor instead (≡ › Observability › Live Monitor).",
        ),
        "SYNC" | "PSYNC" => (REPLICATION_REJECT_MSG, ""),
        _ => return None,
    };
    Some(format!("{reason}{instead}"))
}

/// The first line of `input` that is a write (`is_write_command` — whatever
/// the read-only allowlist does not name, so an unknown command counts), as
/// typed. A line that does not parse is left to the run, which reports it,
/// and so is one the terminal refuses ([`refusal`]): it is never sent, so
/// there is nothing to unlock or confirm for it.
pub(crate) fn first_write_line(input: &str) -> Option<String> {
    input.lines().map(str::trim).find_map(|line| {
        let parts = shlex::split(line)?;
        let (name, args) = parts.split_first()?;
        (is_write_command(name, args) && refusal(name, args).is_none()).then(|| line.to_string())
    })
}

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
pub(crate) fn strip_redis_cli_prefix(line: &str) -> &str {
    let trimmed = line.trim_start();
    match trimmed.split_once(char::is_whitespace) {
        Some((first, rest)) if first.eq_ignore_ascii_case("redis-cli") => rest.trim_start(),
        None if trimmed.eq_ignore_ascii_case("redis-cli") => "",
        _ => line,
    }
}

impl ZedisTerminal {
    /// Forget the terminal's connection; the next line opens a fresh one on
    /// the panel's db. A new session rather than emptying this one, so a
    /// line still in flight on the old connection can't write it back.
    pub(super) fn drop_connection(&mut self) {
        self.session = TerminalSession::default();
        self.terminal_db = None;
    }

    pub(super) fn execute_command(&mut self, command: SharedString, window: &mut Window, cx: &mut Context<Self>) {
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

        // A write meets what every other write on this connection meets. The
        // editors grey their buttons out; a typed command has none, and used
        // to go straight to the server past a lock the status bar showed as
        // closed. One write among the lines holds the whole input back.
        let gate = self.server_state.read(cx).write_gate();
        if gate != WriteGate::Open
            && let Some(line) = first_write_line(&command)
        {
            match gate {
                WriteGate::Locked => self.ask_to_unlock(command, line, window, cx),
                _ => {
                    let reason = i18n_common(cx, "disable_in_readonly").to_string();
                    self.push_entry(TranscriptEntry::Command {
                        line,
                        reply: LineReply::Message(reason),
                    });
                    cx.notify();
                }
            }
            return;
        }

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
                if let Some(kind) = shlex::split(line)
                    .and_then(|parts| parts.first().and_then(|cmd_name| bridge_danger(&server, cmd_name)))
                {
                    blocking = Some((line.to_string(), kind));
                    break;
                }
                if confirm_writes
                    && let Some(parts) = shlex::split(line)
                    && let Some(cmd_name) = parts.first()
                    && is_write_command(cmd_name, &parts[1..])
                    && refusal(cmd_name, &parts[1..]).is_none()
                {
                    blocking = Some((line.to_string(), DangerKind::GenericWrite));
                    break;
                }
            }
            if let Some((line, kind)) = blocking {
                let entity = cx.entity().downgrade();
                let command_for_run = command.clone();
                let db = self.current_db(cx);
                confirm_dangerous_command(&server, db, &kind, Some(&line), window, cx, move |_, cx| {
                    let Some(this) = entity.upgrade() else { return };
                    this.update(cx, |this, cx| this.run_command_lines(command_for_run.clone(), true, cx));
                });
                return;
            }
        }
        self.run_command_lines(command, false, cx);
    }

    /// The database this terminal is on: its own after a `SELECT`, else the
    /// workspace's.
    fn current_db(&self, cx: &App) -> usize {
        self.terminal_db.unwrap_or_else(|| self.server_state.read(cx).db())
    }

    /// A write on a write-locked entry: the lock's own question (by name on
    /// production), and on a yes the window opens and the input is run from
    /// the top — so a line that is also dangerous is still asked about.
    fn ask_to_unlock(&mut self, command: SharedString, line: String, window: &mut Window, cx: &mut Context<Self>) {
        let Ok(server) = get_server(self.server_state.read(cx).server_id()) else {
            return;
        };
        let server_state = self.server_state.clone();
        let entity = cx.entity().downgrade();
        confirm_dangerous_command(
            &server,
            self.current_db(cx),
            &DangerKind::WriteLocked,
            Some(&line),
            window,
            cx,
            move |window, cx| {
                server_state.update(cx, |state, cx| state.unlock_writes(cx));
                let entity = entity.clone();
                let command = command.clone();
                // After this dialog has closed: the run may open one of
                // its own, and closing would take that one instead.
                window.defer(cx, move |window, cx| {
                    let Some(this) = entity.upgrade() else { return };
                    this.update(cx, |this, cx| this.execute_command(command, window, cx));
                });
            },
        );
    }

    /// `? <question>` handler: ask the configured AI endpoint for the
    /// matching Redis command. The suggestion is placed into the input box
    /// (via `pending_ai_fill`) and its explanation echoed to the output —
    /// execution stays with the user, so the danger-confirm and read-only
    /// gates apply unchanged when they hit Enter.
    ///
    /// Privacy: only the question plus server *metadata* (version,
    /// deployment type, modules, current db) are sent — never key values.
    #[cfg(not(target_family = "wasm"))]
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
            self.current_db(cx),
        );

        // A second `?` while one is pending replaces the task (its
        // completion never runs) — clear the previous placeholder so it
        // can't linger in the scrollback forever.
        self.transcript
            .retain(|block| !matches!(block.entry, TranscriptEntry::AiPending));
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
                    .retain(|block| !matches!(block.entry, TranscriptEntry::AiPending));
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

    /// The assistant is a blocking HTTP call to a configured endpoint,
    /// and the browser build carries no HTTP client of its own (ADR 9).
    #[cfg(target_family = "wasm")]
    fn ask_ai(&mut self, question: String, cx: &mut Context<Self>) {
        self.push_entry(TranscriptEntry::Text(format!(
            "? {question}\nThe AI assistant is not available in the browser build."
        )));
        cx.notify();
    }

    /// `confirmed` is whether the dialog above was shown and answered: the
    /// lines then run with the answer, which is what the bridge in the
    /// browser needs to hear (`ServerDb::confirmed`).
    fn run_command_lines(&mut self, command: SharedString, confirmed: bool, cx: &mut Context<Self>) {
        let server_state = self.server_state.read(cx);
        let server_id = server_state.server_id().to_string();
        let at = if confirmed {
            server_state.at_confirmed()
        } else {
            ServerDb::new(server_id.as_str(), server_state.db())
        };
        let session = self.session.clone();
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
                let at = at.clone();
                let session = session.clone();
                let task = cx.background_spawn(async move {
                    let Some(parts) = shlex::split(&line) else {
                        return Ok(LineOutcome::default());
                    };
                    if parts.is_empty() {
                        return Ok(LineOutcome::default());
                    }
                    let cmd_name = parts[0].clone();
                    let args = parts[1..].to_vec();
                    // Refuse, before touching the connection, what would park it
                    // until data arrives or turn it into a stream.
                    if let Some(reason) = refusal(&cmd_name, &args) {
                        return Ok(LineOutcome {
                            reply: LineReply::Message(reason),
                            selected_db: None,
                        });
                    }
                    // A dead link is forgotten by the session, so the next
                    // line reconnects instead of failing the same way.
                    let reply = session.run(&at, &cmd_name, &args).await?;
                    record_command(server_id.as_str(), line.as_str());
                    let selected_db = reply.selected_db();
                    Ok(LineOutcome {
                        reply: LineReply::Value(reply),
                        selected_db,
                    })
                });
                let result: Result<LineOutcome> = task.await;
                let (reply, selected_db, link_dropped, failed) = match result {
                    Ok(outcome) => (outcome.reply, outcome.selected_db, false, false),
                    Err(e) => (
                        LineReply::Message(e.to_string()),
                        None,
                        TerminalSession::drops_link(e.connection_kind()),
                        true,
                    ),
                };
                if failed {
                    errors += 1;
                }
                let _ = handle.update(cx, |this, cx| {
                    let watch_aborted = i18n_terminal(cx, "watch_aborted");
                    for entry in transcript_entries_for(&mut this.multi_queue, line_clone, reply, &watch_aborted) {
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

    pub(super) fn toggle_batch_mode(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
    pub(super) fn run_batch(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
