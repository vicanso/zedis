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

//! Terminal output transcript model, reply formatting, MULTI/EXEC block
//! bookkeeping, scrollback trimming, and copy/save output actions.

use super::*;

/// What one executed line produced: the reply — kept as a value, so the
/// output can be re-rendered in another format — and the db a successful
/// `SELECT` moved the terminal's connection to.
#[derive(Default)]
pub(super) struct LineOutcome {
    pub(super) reply: LineReply,
    pub(super) selected_db: Option<usize>,
}

/// The server's answer to one line, or the text shown in its place.
pub(super) enum LineReply {
    /// The reply, kept as the server sent it (with the command that
    /// produced it) so the output can be rendered again in another format.
    Value(TerminalReply),
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
pub(super) enum TranscriptEntry {
    /// Banner, notes, AI lines: verbatim.
    Text(String),
    Command {
        line: String,
        reply: LineReply,
    },
    /// A `MULTI … EXEC` block: the queued commands beside EXEC's replies.
    Exec {
        commands: Vec<String>,
        replies: ExecReplies,
    },
    /// Footer of a multi-line (Batch / pasted pipeline) run.
    BatchSummary {
        commands: usize,
        errors: usize,
        elapsed_ms: u128,
    },
    /// A `?` request in flight; replaced by its answer.
    #[cfg(not(target_family = "wasm"))]
    AiPending,
}

/// Entries kept; the oldest go first. The rendered text is capped again by
/// [`MAX_OUTPUT_CHARS`], so a few huge replies can't keep 1000 blocks alive.
pub(super) const MAX_TRANSCRIPT_ENTRIES: usize = 1_000;

/// One transcript entry and its text as last drawn, with the format it was
/// drawn in.
pub(super) struct Block {
    pub(super) entry: TranscriptEntry,
    pub(super) drawn: Option<(ReplyFormat, String)>,
}

impl From<TranscriptEntry> for Block {
    fn from(entry: TranscriptEntry) -> Self {
        Self { entry, drawn: None }
    }
}

/// The output pane's text for `blocks` in `format`. A block is drawn once
/// per format and kept: each line of output used to redraw every reply in
/// the transcript — up to a thousand, tables and JSON included.
pub(super) fn render_transcript(blocks: &mut [Block], format: ReplyFormat) -> String {
    for block in blocks.iter_mut() {
        if block.drawn.as_ref().is_none_or(|(drawn_in, _)| *drawn_in != format) {
            block.drawn = Some((format, render_entry(&block.entry, format)));
        }
    }
    blocks
        .iter()
        .filter_map(|block| block.drawn.as_ref().map(|(_, text)| text.as_str()))
        .collect()
}

/// One entry's text in `format`.
fn render_entry(entry: &TranscriptEntry, format: ReplyFormat) -> String {
    use std::fmt::Write;
    let mut out = String::new();
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
                LineReply::Value(reply) => {
                    let _ = writeln!(out, "{}", reply.render(format));
                }
                LineReply::Message(message) => {
                    let _ = writeln!(out, "{message}");
                }
            }
        }
        TranscriptEntry::Exec { commands, replies } => {
            let _ = writeln!(out, "{CMD_LABEL} EXEC");
            let _ = writeln!(out, "{}", replies.render(commands, format));
        }
        TranscriptEntry::BatchSummary {
            commands,
            errors,
            elapsed_ms,
        } => {
            let _ = writeln!(out, "── batch: {commands} commands · {errors} errors · {elapsed_ms} ms");
        }
        #[cfg(not(target_family = "wasm"))]
        TranscriptEntry::AiPending => {
            let _ = writeln!(out, "{AI_WAITING_MSG}");
        }
    }
    out
}

/// The reply-format switcher's label, in the UI language — the format
/// itself is the connection crate's, its name for a person is not.
pub(super) fn reply_format_label(format: ReplyFormat, cx: &App) -> SharedString {
    i18n_terminal(
        cx,
        match format {
            ReplyFormat::Text => "format_text",
            ReplyFormat::Table => "format_table",
            ReplyFormat::Json => "format_json",
        },
    )
}

/// Fold one executed line into the `MULTI` bookkeeping and produce its
/// transcript entries. `queue` is `Some` between a successful `MULTI` and
/// its `EXEC` / `DISCARD`; every line Redis answers `QUEUED` joins it, and
/// `EXEC`'s array reply is then laid out against those commands as one
/// [`TranscriptEntry::Exec`] block instead of a bare list. `EXEC` after a
/// `WATCH` conflict answers nil — said in words (`watch_aborted`, in the UI
/// language), since a bare `(nil)` reads as a missing key.
pub(super) fn transcript_entries_for(
    queue: &mut Option<Vec<String>>,
    line: String,
    reply: LineReply,
    watch_aborted: &str,
) -> Vec<TranscriptEntry> {
    // An `EXEC` or `DISCARD` the server refused as a whole (`EXECABORT`
    // after a queued syntax error, "EXEC without MULTI") still ends the
    // transaction; the terminal must not go on queueing into it.
    if matches!(reply, LineReply::Message(_))
        && line
            .split_whitespace()
            .next()
            .is_some_and(|word| word.eq_ignore_ascii_case("EXEC") || word.eq_ignore_ascii_case("DISCARD"))
    {
        *queue = None;
    }
    if let LineReply::Value(answer) = &reply {
        match answer.command().to_ascii_uppercase().as_str() {
            "MULTI" if answer.is_ok() => *queue = Some(Vec::new()),
            "DISCARD" => *queue = None,
            "EXEC" => {
                if let Some(commands) = queue.take() {
                    if let Some(replies) = answer.exec_replies() {
                        return vec![TranscriptEntry::Exec { commands, replies }];
                    }
                    if answer.is_nil() {
                        return vec![
                            TranscriptEntry::Command { line, reply },
                            TranscriptEntry::Text(watch_aborted.to_string()),
                        ];
                    }
                }
            }
            _ => {
                if let Some(queued) = queue.as_mut()
                    && answer.is_queued()
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
#[cfg(not(target_family = "wasm"))]
fn write_output_file(text: &str) -> io::Result<PathBuf> {
    let dir = get_download_dir()
        .or_else(|| get_or_create_config_dir().ok())
        .ok_or_else(|| io::Error::other("no directory to write the output to"))?;
    let path = dir.join(format!("zedis-terminal-{}.txt", Local::now().format("%Y%m%d-%H%M%S")));
    write_file_atomic(&path, text.as_bytes())?;
    Ok(path)
}

/// Scrollback cap for the rendered output text. The whole text is
/// re-rendered and handed to the read-only editor on every command, so
/// leaving it unbounded made a long session O(n²) in total output. ~200 KB
/// ≈ a few thousand lines.
const MAX_OUTPUT_CHARS: usize = 200_000;

/// Placeholder line shown in the output while a `?` AI request is in
/// flight; removed when the reply (or error) lands. Distinctive on
/// purpose so [`remove_last_line`] can't clip user output.
#[cfg(not(target_family = "wasm"))]
const AI_WAITING_MSG: &str = "AI> … thinking — this can take a few seconds; you can keep running commands meanwhile";

/// Drop the oldest output once the scrollback buffer exceeds
/// [`MAX_OUTPUT_CHARS`], cutting at a line boundary so the top stays clean.
pub(super) fn trim_output_scrollback(buf: &mut String) {
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

impl ZedisTerminal {
    pub(super) fn reset_cmd_state(&mut self, cx: &mut Context<Self>) {
        self.transcript.clear();
        self.multi_queue = None;
        self.push_entry(TranscriptEntry::Text(ZEDIS_LOGO.replace("{VERSION}", VERSION)));
        // In the language of the moment; a banner already in the transcript
        // is history, and Clear writes it again.
        self.push_entry(TranscriptEntry::Text(i18n_terminal(cx, "ai_hint").to_string()));
    }

    /// Append to the transcript, dropping the oldest blocks past the cap,
    /// and schedule a re-render of the output text.
    pub(super) fn push_entry(&mut self, entry: TranscriptEntry) {
        self.transcript.push(entry.into());
        if self.transcript.len() > MAX_TRANSCRIPT_ENTRIES {
            let excess = self.transcript.len() - MAX_TRANSCRIPT_ENTRIES;
            self.transcript.drain(..excess);
        }
        self.cmd_output_dirty = true;
    }

    /// Switch the reply rendering: every block re-renders, and the choice
    /// is remembered for the next session.
    pub(super) fn set_reply_format(&mut self, format: ReplyFormat, cx: &mut Context<Self>) {
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

    pub(super) fn copy_output(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(self.cmd_output_text.clone()));
        window.push_notification(Notification::success(i18n_terminal(cx, "output_copied")), cx);
    }

    #[cfg(not(target_family = "wasm"))]
    pub(super) fn save_output(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
        match write_output_file(&self.cmd_output_text) {
            Ok(path) => {
                info!(path = %path.display(), "terminal output saved");
                let message = t!(
                    "terminal.output_saved",
                    path = path.display().to_string(),
                    locale = locale
                );
                window.push_notification(Notification::success(message.to_string()), cx);
                cx.reveal_path(&path);
            }
            Err(e) => {
                error!(error = %e, "terminal output save failed");
                let message = t!("terminal.save_output_failed", error = e.to_string(), locale = locale);
                window.push_notification(Notification::error(message.to_string()), cx);
            }
        }
    }

    /// [`Self::save_output`] in a page: the same file, as a download.
    #[cfg(target_family = "wasm")]
    pub(super) fn save_output(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
        let name = export_filename_with("terminal", "txt");
        let saved = web_download()
            .ok_or_else(|| "no download in this page".to_string())
            .and_then(|download| download(&name, media_type_for(&name), self.cmd_output_text.as_bytes()));
        let notification = match saved {
            Ok(()) => Notification::success(t!("terminal.output_saved", path = name, locale = locale).to_string()),
            Err(e) => {
                error!(error = %e, "terminal output download failed");
                Notification::error(t!("terminal.save_output_failed", error = e, locale = locale).to_string())
            }
        };
        window.push_notification(notification, cx);
    }

    /// The output pane's right-click commands beyond the editor's own.
    pub(super) fn handle_action(&mut self, action: &TerminalAction, window: &mut Window, cx: &mut Context<Self>) {
        match action {
            TerminalAction::CopyAll => self.copy_output(window, cx),
            TerminalAction::Save => self.save_output(window, cx),
            TerminalAction::Clear => {
                self.reset_cmd_state(cx);
                cx.notify();
            }
        }
    }
}
