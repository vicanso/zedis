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

//! The audit log: what went through this door, and by whom.
//!
//! One JSON object per line, appended to the file `--audit-log` names, for
//! the questions a shared gateway is asked afterwards: who signed in, who
//! was refused, who changed a server entry, who ran `CONFIG SET` or
//! `FLUSHDB` — and, with `--audit-writes`, who wrote which key by hand.
//!
//! What is kept is decided here once; the routes hand over facts they hold
//! anyway (ADR 11):
//!
//! - **Bridge events**: a login and a failed one (passwords are guessable,
//!   and a run of wrong ones shows nowhere else), a logout, a refusal by
//!   role.
//! - **Server entries**: added, edited — which settings changed and from
//!   what, which secrets changed and never to what, and a private entry
//!   made shared, which hands its credentials to every account — deleted.
//! - **Commands that administer the server** rather than its data
//!   (`zedis_connection::is_administration_command`), and every command a
//!   person had to confirm: what needed a confirmation needs a record, so
//!   the audit set contains the confirmation set by construction, and an
//!   entry with `require_confirm_writes` set logs every write it receives.
//! - **Data writes**, only with `--audit-writes`. The applications write
//!   more in a second than every GUI user in a day and none of it passes
//!   here, so this log cannot say who changed a key; it can say whether
//!   anyone did so by hand, which is the question actually asked. Off by
//!   default, with the per-entry setting above for the servers where
//!   by-hand matters.
//! - **Reads, never.** The key tree is a stream of `SCAN`s and every open
//!   tab a heartbeat of `INFO`s; a log that carried them would bury the rest.
//!
//! A line is written after the outcome is known, so it says what happened
//! rather than what was attempted, with `error` for an upstream failure.
//! Arguments are blanked of passwords (`zedis_connection::redact_secrets`)
//! and cut to a length — a `SET` carries a value, and an audit log is not a
//! backup. A line that cannot be written is reported and the request goes
//! on: refusing work when the disk is full would turn an audit failure into
//! an outage, and a bridge that stops is a bridge someone goes around.

use crate::policy::{Verdict, words};
use axum::http::HeaderMap;
use serde::Serialize;
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use zedis_connection::{
    ConfirmStrictness, DangerKind, RedisServer, is_administration_command, is_write_command, redact_secrets,
};

/// `--audit-log` for a deployment configured by its environment.
pub const LOG_ENV: &str = "ZEDIS_BRIDGE_AUDIT_LOG";
/// `--audit-writes` likewise; `1` / `true` switch it on.
pub const WRITES_ENV: &str = "ZEDIS_BRIDGE_AUDIT_WRITES";

/// An argument longer than this is cut: a value is not what the log is for.
const MAX_ARG_CHARS: usize = 128;
/// A command with more arguments than this keeps the first of them and a
/// count: a `DEL` of a folder names hundreds of keys.
const MAX_ARGS: usize = 24;

/// The log, or nothing. Cloned into the router state; `off()` is a
/// deployment without `--audit-log`, where every `record` is a no-op.
#[derive(Clone, Default)]
pub struct Audit(Option<Arc<Sink>>);

struct Sink {
    path: PathBuf,
    file: Mutex<File>,
    writes: bool,
}

impl Audit {
    pub fn off() -> Self {
        Self(None)
    }

    /// Open `path` for appending — created owner-only if it is new — and
    /// keep it. `writes` is `--audit-writes`.
    pub fn open(path: &Path, writes: bool) -> std::io::Result<Self> {
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        restrict(path);
        Ok(Self(Some(Arc::new(Sink {
            path: path.to_path_buf(),
            file: Mutex::new(file),
            writes,
        }))))
    }

    /// Whether plain data writes get a line too.
    pub fn logs_writes(&self) -> bool {
        self.0.as_ref().is_some_and(|sink| sink.writes)
    }

    /// Append one line. Never fails the caller: a line that could not be
    /// written is an error in the process log instead.
    pub fn record(&self, account: &str, origin: &Origin, event: Event) {
        let Some(sink) = &self.0 else {
            return;
        };
        let line = Line {
            ts: rfc3339_now(),
            account,
            peer: origin.peer.as_deref(),
            forwarded_for: origin.forwarded_for.as_deref(),
            auth: origin.auth,
            via: origin.via,
            event,
        };
        let mut bytes = match serde_json::to_vec(&line) {
            Ok(bytes) => bytes,
            Err(e) => {
                tracing::error!(error = %e, "audit line could not be serialised");
                return;
            }
        };
        bytes.push(b'\n');
        // A poisoned lock means another writer panicked mid-line; the file
        // is still the file, and losing every later line over it is worse.
        let mut file = sink.file.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Err(e) = file.write_all(&bytes) {
            tracing::error!(error = %e, path = %sink.path.display(), "audit line lost");
        }
    }
}

/// Owner-only on unix: the file names who did what, and the keys they did
/// it to. On Windows it inherits its directory's ACL, as the logins file does.
#[cfg(unix)]
fn restrict(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Err(e) = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)) {
        tracing::warn!(error = %e, path = %path.display(), "could not restrict the audit log");
    }
}

#[cfg(not(unix))]
fn restrict(_path: &Path) {}

/// Where a request came from: the socket's peer, and what a proxy in front
/// wrote in `X-Forwarded-For`. Kept apart, not merged: the header is
/// whatever the previous hop said, and only the deployment knows whether
/// that hop is its own proxy or the open internet. The line carries both
/// and the reader decides.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Origin {
    pub peer: Option<String>,
    pub forwarded_for: Option<String>,
    /// The peer's address as an address, for the trusted-proxy check.
    pub peer_ip: Option<IpAddr>,
    /// How the caller was identified when it was not by a password of its
    /// own: `proxy` for an identity the reverse proxy asserted. Set by
    /// `authorize`, written into every line of that request.
    pub auth: Option<&'static str>,
    /// Which door the request came through when it was not the page or a
    /// script at the API: `mcp` for a tool call by an AI assistant (ADR 15).
    /// Set by that handler, written into every line of that request.
    pub via: Option<&'static str>,
}

impl Origin {
    pub fn new(peer: SocketAddr, headers: &HeaderMap) -> Self {
        Self {
            peer: Some(peer.to_string()),
            peer_ip: Some(peer.ip()),
            auth: None,
            via: None,
            forwarded_for: headers
                .get("x-forwarded-for")
                .and_then(|v| v.to_str().ok())
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty()),
        }
    }
}

/// One line of the log.
#[derive(Serialize)]
struct Line<'a> {
    ts: String,
    account: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    peer: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    forwarded_for: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    auth: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    via: Option<&'static str>,
    #[serde(flatten)]
    event: Event,
}

/// What happened. The variant name is the line's `event` field.
#[derive(Serialize, Debug)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    Login {
        remember: bool,
    },
    /// `account` is the name that was tried.
    LoginFailed,
    /// The reverse proxy signed in a name that is no account here;
    /// `account` is that name.
    NoAccount,
    Logout,
    /// A read-only account asked for something that changes the server list.
    Refused {
        action: &'static str,
    },
    Command(CommandLine),
    ServerAdded {
        server: ServerRef,
        shared: bool,
        settings: Map<String, Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    ServerUpdated {
        server: ServerRef,
        /// Setting → from, to. Empty for an edit that changed no setting
        /// (a reorder), which is still an edit.
        changed: BTreeMap<String, Change>,
        /// Which secrets were set, cleared or replaced. Never to what.
        #[serde(skip_serializing_if = "Vec::is_empty")]
        secrets_changed: Vec<&'static str>,
        /// Present when the entry went private → shared or back.
        #[serde(skip_serializing_if = "Option::is_none")]
        shared: Option<Change>,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    ServerDeleted {
        server: ServerRef,
    },
    /// The caller opened its write window on a locked entry (ADR 14).
    Unlocked {
        server: ServerRef,
        seconds: u64,
    },
    /// …and closed it before it ended.
    Locked {
        server: ServerRef,
    },
    /// A tool call at the MCP entry point (ADR 15) — every one, whatever it
    /// read. The other lines keep what a person did that mattered; this
    /// door admits a program acting for someone, and reads are the whole
    /// of what it does.
    Tool {
        tool: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        server: Option<ServerRef>,
        #[serde(skip_serializing_if = "Option::is_none")]
        db: Option<usize>,
        /// The arguments as the caller gave them; a `read_command`'s
        /// command redacted and cut like a command line's.
        arguments: Value,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
}

/// The entry a line is about — enough to find it, and the name a reader
/// knows it by even after it is deleted.
#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
pub struct ServerRef {
    pub id: String,
    pub name: String,
}

impl From<&RedisServer> for ServerRef {
    fn from(server: &RedisServer) -> Self {
        Self {
            id: server.id.clone(),
            name: server.name.clone(),
        }
    }
}

#[derive(Serialize, Debug, PartialEq, Eq)]
pub struct Change {
    pub from: Value,
    pub to: Value,
}

/// One command, or several with the same name and outcome in one batch —
/// a folder deleted key by key is one line saying `DEL` with a count, not
/// a line per key.
#[derive(Serialize, Debug, PartialEq, Eq)]
pub struct CommandLine {
    pub server: ServerRef,
    pub db: usize,
    pub command: String,
    /// Redacted and cut; the first command's, when several are collapsed.
    pub args: Vec<String>,
    #[serde(skip_serializing_if = "is_one")]
    pub count: usize,
    pub outcome: Outcome,
    /// The `danger.*` kind without its prefix, for a confirmed or refused
    /// command.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<&'static str>,
    /// How it was (or had to be) confirmed: `click` or `type_name`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confirm: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

fn is_one(count: &usize) -> bool {
    *count == 1
}

#[derive(Serialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// Forwarded, no confirmation involved: an administrative command, or a
    /// write with `--audit-writes`.
    Allowed,
    /// Forwarded after the caller confirmed.
    Confirmed,
    /// Refused: it needed a confirmation the caller did not give.
    ConfirmRequired,
    /// Refused: a read-only account.
    Denied,
}

/// The lines an exec request produces, from the verdict each of its commands
/// got. A command without a verdict of note (a read, or a write while
/// `--audit-writes` is off) produces none; commands with the same name and
/// outcome collapse into one line with a count.
pub fn command_lines<'a>(
    server: &RedisServer,
    db: usize,
    judged: impl IntoIterator<Item = (&'a Vec<Vec<u8>>, &'a Verdict)>,
    logs_writes: bool,
) -> Vec<CommandLine> {
    let mut lines: Vec<CommandLine> = Vec::new();
    for (args, verdict) in judged {
        let Some((name, rest)) = words(args) else {
            continue;
        };
        let name = name.to_ascii_uppercase();
        let (outcome, kind, confirm) = match verdict {
            Verdict::Deny => (Outcome::Denied, None, None),
            Verdict::Confirm { kind, strictness } => (Outcome::ConfirmRequired, Some(kind), Some(strictness)),
            Verdict::Confirmed { kind, strictness } => (Outcome::Confirmed, Some(kind), Some(strictness)),
            Verdict::Allow => {
                if is_administration_command(&name, &rest) || (logs_writes && is_write_command(&name)) {
                    (Outcome::Allowed, None, None)
                } else {
                    continue;
                }
            }
        };
        if let Some(line) = lines.iter_mut().find(|l| l.command == name && l.outcome == outcome) {
            line.count += 1;
            continue;
        }
        lines.push(CommandLine {
            server: ServerRef::from(server),
            db,
            command: name.clone(),
            args: cut(redact_secrets(&name, &rest)),
            count: 1,
            outcome,
            kind: kind.map(kind_name),
            confirm: confirm.map(|s| match s {
                ConfirmStrictness::Click => "click",
                ConfirmStrictness::TypeName => "type_name",
            }),
            error: None,
        });
    }
    lines
}

/// `danger.flushall` → `flushall`.
fn kind_name(kind: &DangerKind) -> &'static str {
    let key = kind.i18n_key();
    key.strip_prefix("danger.").unwrap_or(key)
}

/// Arguments as a line keeps them: each cut at [`MAX_ARG_CHARS`], and the
/// list at [`MAX_ARGS`], with what was dropped counted.
pub(crate) fn cut(args: Vec<String>) -> Vec<String> {
    let total = args.len();
    let mut out: Vec<String> = args
        .into_iter()
        .take(MAX_ARGS)
        .map(|arg| {
            let chars = arg.chars().count();
            if chars <= MAX_ARG_CHARS {
                arg
            } else {
                let head: String = arg.chars().take(MAX_ARG_CHARS).collect();
                format!("{head}…(+{} chars)", chars - MAX_ARG_CHARS)
            }
        })
        .collect();
    if total > MAX_ARGS {
        out.push(format!("…(+{} args)", total - MAX_ARGS));
    }
    out
}

/// An entry's settings as a line carries them: what an export keeps — no
/// secrets, no certificates — less the owner, which the line states as
/// `shared` instead, and the id, which it states in `server` (the export
/// blanks it rather than dropping it).
pub fn settings(server: &RedisServer) -> Map<String, Value> {
    let mut map = server
        .to_export_json(false)
        .ok()
        .and_then(|text| serde_json::from_str::<Map<String, Value>>(&text).ok())
        .unwrap_or_default();
    map.remove("owner");
    map.remove("id");
    map
}

/// Every setting that differs between `before` and `after`, from → to.
pub fn changed(before: &RedisServer, after: &RedisServer) -> BTreeMap<String, Change> {
    let (before, after) = (settings(before), settings(after));
    let keys: std::collections::BTreeSet<&String> = before.keys().chain(after.keys()).collect();
    keys.into_iter()
        .filter(|key| before.get(*key) != after.get(*key))
        .map(|key| {
            (
                key.clone(),
                Change {
                    from: before.get(key).cloned().unwrap_or(Value::Null),
                    to: after.get(key).cloned().unwrap_or(Value::Null),
                },
            )
        })
        .collect()
}

/// The names of the secrets whose value differs — set, cleared or replaced.
pub fn secrets_changed(before: &RedisServer, after: &RedisServer) -> Vec<&'static str> {
    let (mut before, mut after) = (before.clone(), after.clone());
    RedisServer::SECRET_FIELDS
        .into_iter()
        .filter(|name| {
            let was = before.secret_mut(name).and_then(|s| s.take()).filter(|s| !s.is_empty());
            let now = after.secret_mut(name).and_then(|s| s.take()).filter(|s| !s.is_empty());
            was != now
        })
        .collect()
}

/// Shared is the absence of an owner (`api::visible_to` reads it the same way).
pub fn is_shared(server: &RedisServer) -> bool {
    server.owner.as_deref().is_none_or(str::is_empty)
}

fn rfc3339_now() -> String {
    let since = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    rfc3339(since.as_secs(), since.subsec_millis())
}

/// `secs` since the epoch as `YYYY-MM-DDTHH:MM:SS.mmmZ`, without a date
/// crate: the bridge's dependency list is short on purpose, and this is the
/// one place it needs a calendar (Howard Hinnant's civil-from-days).
fn rfc3339(secs: u64, millis: u32) -> String {
    let days = (secs / 86_400) as i64;
    let second_of_day = secs % 86_400;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era = (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{millis:03}Z",
        second_of_day / 3600,
        (second_of_day % 3600) / 60,
        second_of_day % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use zedis_connection::DangerKind;

    fn args(parts: &[&str]) -> Vec<Vec<u8>> {
        parts.iter().map(|p| p.as_bytes().to_vec()).collect()
    }

    fn server() -> RedisServer {
        RedisServer {
            id: "srv-1".to_string(),
            name: "prod".to_string(),
            host: "10.0.0.5".to_string(),
            port: 6379,
            password: Some("hunter2".to_string()),
            ..Default::default()
        }
    }

    fn lines(judged: &[(Vec<Vec<u8>>, Verdict)], writes: bool) -> Vec<CommandLine> {
        command_lines(&server(), 2, judged.iter().map(|(a, v)| (a, v)), writes)
    }

    #[test]
    fn the_calendar_is_right_on_the_days_that_catch_a_wrong_one() {
        assert_eq!(rfc3339(0, 0), "1970-01-01T00:00:00.000Z");
        assert_eq!(rfc3339(946_684_799, 999), "1999-12-31T23:59:59.999Z");
        assert_eq!(rfc3339(946_684_800, 0), "2000-01-01T00:00:00.000Z");
        // A leap day in a century year that is a leap year.
        assert_eq!(rfc3339(951_782_400, 0), "2000-02-29T00:00:00.000Z");
        assert_eq!(rfc3339(951_868_800, 0), "2000-03-01T00:00:00.000Z");
        assert_eq!(rfc3339(1_790_294_400, 5), "2026-09-25T00:00:00.005Z");
        assert_eq!(rfc3339(1_790_294_400 + 3_661, 0), "2026-09-25T01:01:01.000Z");
    }

    #[test]
    fn an_administrative_command_is_kept_and_a_read_is_not() {
        let kept = lines(
            &[
                (args(&["GET", "k"]), Verdict::Allow),
                (args(&["CONFIG", "SET", "maxmemory", "1gb"]), Verdict::Allow),
                (args(&["SCAN", "0"]), Verdict::Allow),
            ],
            false,
        );
        assert_eq!(kept.len(), 1, "{kept:?}");
        assert_eq!(kept[0].command, "CONFIG");
        assert_eq!(kept[0].args, vec!["SET", "maxmemory", "1gb"]);
        assert_eq!(kept[0].outcome, Outcome::Allowed);
        assert_eq!(kept[0].db, 2);
        assert_eq!(kept[0].server.name, "prod");
    }

    #[test]
    fn a_data_write_is_kept_only_when_asked_for() {
        let write = [(args(&["SET", "k", "v"]), Verdict::Allow)];
        assert!(lines(&write, false).is_empty(), "writes are off by default");
        let kept = lines(&write, true);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].command, "SET");
        // Even then a read stays out.
        assert!(lines(&[(args(&["GET", "k"]), Verdict::Allow)], true).is_empty());
    }

    #[test]
    fn a_confirmed_or_refused_command_is_always_kept_with_how() {
        let confirmed = Verdict::Confirmed {
            kind: DangerKind::FlushDb,
            strictness: ConfirmStrictness::TypeName,
        };
        let kept = lines(&[(args(&["FLUSHDB"]), confirmed)], false);
        assert_eq!(
            (kept[0].outcome, kept[0].kind, kept[0].confirm),
            (Outcome::Confirmed, Some("flushdb"), Some("type_name"))
        );

        let required = Verdict::Confirm {
            kind: DangerKind::GenericWrite,
            strictness: ConfirmStrictness::Click,
        };
        // A plain write with a verdict is kept even with writes off: what
        // needed a confirmation needs a record.
        let kept = lines(&[(args(&["SET", "k", "v"]), required)], false);
        assert_eq!(
            (kept[0].outcome, kept[0].kind, kept[0].confirm),
            (Outcome::ConfirmRequired, Some("generic_write"), Some("click"))
        );

        let kept = lines(&[(args(&["DEL", "k"]), Verdict::Deny)], false);
        assert_eq!((kept[0].outcome, kept[0].kind), (Outcome::Denied, None));
    }

    #[test]
    fn a_batch_of_the_same_command_is_one_line_with_a_count() {
        let judged: Vec<_> = (0..300)
            .map(|i| (args(&["UNLINK", &format!("user:{i}")]), Verdict::Allow))
            .collect();
        let kept = lines(&judged, true);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].count, 300);
        assert_eq!(kept[0].args, vec!["user:0"], "the first command's arguments");
        // Different outcomes do not merge.
        let mixed = [
            (args(&["DEL", "a"]), Verdict::Allow),
            (
                args(&["DEL", "b"]),
                Verdict::Confirmed {
                    kind: DangerKind::GenericWrite,
                    strictness: ConfirmStrictness::Click,
                },
            ),
        ];
        assert_eq!(lines(&mixed, true).len(), 2);
    }

    #[test]
    fn arguments_are_redacted_and_cut() {
        let kept = lines(&[(args(&["AUTH", "hunter2"]), Verdict::Allow)], false);
        assert_eq!(kept[0].args, vec!["***"]);

        let long = "v".repeat(500);
        let kept = lines(&[(args(&["SET", "k", &long]), Verdict::Allow)], true);
        assert!(kept[0].args[1].starts_with(&"v".repeat(MAX_ARG_CHARS)));
        assert!(kept[0].args[1].ends_with("…(+372 chars)"), "{}", kept[0].args[1]);

        let keys: Vec<String> = (0..40).map(|i| format!("k{i}")).collect();
        let mut many = vec!["DEL".to_string()];
        many.extend(keys);
        let many: Vec<Vec<u8>> = many.into_iter().map(String::into_bytes).collect();
        let kept = lines(&[(many, Verdict::Allow)], true);
        assert_eq!(kept[0].args.len(), MAX_ARGS + 1);
        assert_eq!(kept[0].args[MAX_ARGS], "…(+16 args)");
    }

    #[test]
    fn a_line_is_one_json_object_with_the_event_flattened_in() {
        let path = std::env::temp_dir().join(format!("zedis-audit-{}.log", uuid::Uuid::new_v4()));
        let audit = Audit::open(&path, true).expect("open");
        assert!(audit.logs_writes());
        let origin = Origin {
            peer: Some("10.0.0.7:5000".to_string()),
            auth: Some("proxy"),
            ..Default::default()
        };
        audit.record("alice", &origin, Event::Login { remember: true });
        audit.record(
            "bob",
            &Origin::default(),
            Event::Command(CommandLine {
                server: ServerRef::from(&server()),
                db: 0,
                command: "FLUSHDB".to_string(),
                args: vec![],
                count: 1,
                outcome: Outcome::Confirmed,
                kind: Some("flushdb"),
                confirm: Some("click"),
                error: Some("upstream said no".to_string()),
            }),
        );
        let text = std::fs::read_to_string(&path).expect("read back");
        let _ = std::fs::remove_file(&path);
        let lines: Vec<Value> = text
            .lines()
            .map(|l| serde_json::from_str(l).expect("one json object per line"))
            .collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["event"], "login");
        assert_eq!(lines[0]["account"], "alice");
        assert_eq!(lines[0]["peer"], "10.0.0.7:5000");
        assert_eq!(lines[0]["remember"], true);
        assert_eq!(lines[0]["auth"], "proxy");
        assert!(lines[0].get("forwarded_for").is_none());
        assert!(lines[0]["ts"].as_str().is_some_and(|ts| ts.ends_with('Z')));
        assert_eq!(lines[1]["event"], "command");
        assert_eq!(lines[1]["command"], "FLUSHDB");
        assert_eq!(lines[1]["server"]["name"], "prod");
        assert_eq!(lines[1]["outcome"], "confirmed");
        assert_eq!(lines[1]["error"], "upstream said no");
        assert!(lines[1].get("count").is_none(), "a count of one is not written");
        assert!(lines[1].get("peer").is_none() && lines[1].get("auth").is_none());

        // Off: nothing is written, nothing is opened.
        let off = Audit::off();
        assert!(!off.logs_writes());
        off.record("x", &Origin::default(), Event::Logout);
    }

    #[test]
    fn an_entry_line_carries_settings_and_never_a_secret() {
        let map = settings(&server());
        assert_eq!(map["host"], "10.0.0.5");
        assert!(map.get("password").is_none() && map.get("owner").is_none() && map.get("id").is_none());
        let text = serde_json::to_string(&map).expect("json");
        assert!(!text.contains("hunter2"));
    }

    #[test]
    fn an_edit_is_the_settings_that_changed_and_the_names_of_the_secrets_that_did() {
        let before = server();
        let mut after = server();
        after.port = 6380;
        after.tls = Some(true);
        after.password = Some("new".to_string());
        after.ssh_key = Some("-----BEGIN".to_string());
        let diff = changed(&before, &after);
        assert_eq!(diff.len(), 2, "{diff:?}");
        assert_eq!(
            diff["port"],
            Change {
                from: 6379.into(),
                to: 6380.into()
            }
        );
        assert_eq!(
            diff["tls"],
            Change {
                from: Value::Null,
                to: true.into()
            }
        );
        assert_eq!(secrets_changed(&before, &after), vec!["password", "ssh_key"]);
        assert!(changed(&before, &before).is_empty());
        assert!(secrets_changed(&before, &before).is_empty());
        // Cleared counts; an empty string is a cleared one.
        let mut cleared = server();
        cleared.password = Some(String::new());
        assert_eq!(secrets_changed(&before, &cleared), vec!["password"]);
    }

    #[test]
    fn shared_is_the_absence_of_an_owner() {
        let mut s = server();
        assert!(is_shared(&s));
        s.owner = Some(String::new());
        assert!(is_shared(&s));
        s.owner = Some("alice".to_string());
        assert!(!is_shared(&s));
    }

    #[test]
    fn the_origin_keeps_the_peer_and_the_header_apart() {
        let peer: SocketAddr = "10.0.0.7:5000".parse().expect("addr");
        let mut headers = HeaderMap::new();
        assert_eq!(
            Origin::new(peer, &headers),
            Origin {
                peer: Some("10.0.0.7:5000".to_string()),
                peer_ip: Some(peer.ip()),
                ..Default::default()
            }
        );
        headers.insert("x-forwarded-for", " 203.0.113.9, 10.0.0.1 ".parse().expect("value"));
        assert_eq!(
            Origin::new(peer, &headers).forwarded_for.as_deref(),
            Some("203.0.113.9, 10.0.0.1")
        );
    }
}
