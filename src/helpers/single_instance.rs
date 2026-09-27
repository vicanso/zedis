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

//! One running Zedis per profile.
//!
//! The local database can only be opened by one process, so a second launch
//! used to end at the "database locked" recovery window. Now the running
//! instance listens on a loopback port whose number and a one-time token are
//! written to `<config_dir>/instance.json`; a second launch reads that file,
//! hands over its arguments (a `redis://` link, or nothing — "bring the
//! window up") and exits. The token keeps another local user or process from
//! steering the app; a stale file (crash, reboot) simply fails to connect,
//! or connects to something that never answers `OK`, and the new process
//! becomes the instance.

use crate::helpers::channel::{Receiver, Sender, unbounded};
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;
use tracing::{info, warn};
use uuid::Uuid;
use zedis_core::fs::{get_or_create_config_dir, write_file_atomic};

pub const INSTANCE_FILE: &str = "instance.json";
const CONNECT_TIMEOUT: Duration = Duration::from_millis(500);
const IO_TIMEOUT: Duration = Duration::from_secs(2);
/// A request is two short lines; anything longer is not ours.
const MAX_REQUEST_BYTES: u64 = 64 * 1024;

/// What a second launch hands to the running instance.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct InstanceMessage {
    /// Connection links (`redis://`, `valkeys://`, …) from the command line.
    pub urls: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct InstanceRecord {
    port: u16,
    token: String,
    pid: u32,
}

/// The listening side, handed to `launch` so it can route messages onto the
/// foreground.
pub struct InstanceServer {
    listener: TcpListener,
    token: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstanceRole {
    /// This process runs the app. The listener, when one could be set up,
    /// waits in [`take_instance_server`] for `launch` — the app still runs
    /// without one, a second launch just is not forwarded.
    Primary,
    /// The running instance took the message; this process should exit.
    Forwarded,
}

/// The listener between [`claim_instance`] (before the app exists) and
/// `launch` (which may run from the database recovery window instead of
/// `main`, so it cannot be threaded through as an argument).
static PENDING_SERVER: Mutex<Option<InstanceServer>> = Mutex::new(None);

pub fn take_instance_server() -> Option<InstanceServer> {
    PENDING_SERVER.lock().unwrap_or_else(|e| e.into_inner()).take()
}

/// Messages for the running app, from the hand-off thread and from the OS
/// URL callback alike (both fire off the foreground); `launch` drains the
/// queue on the foreground once the window exists — a link that arrives
/// during startup waits here.
static INBOX: OnceLock<(Sender<InstanceMessage>, Receiver<InstanceMessage>)> = OnceLock::new();

fn inbox() -> &'static (Sender<InstanceMessage>, Receiver<InstanceMessage>) {
    INBOX.get_or_init(unbounded)
}

pub fn post_instance_message(message: InstanceMessage) {
    // Unbounded, so this never blocks the caller's thread.
    let _ = inbox().0.send_blocking(message);
}

pub fn instance_messages() -> Receiver<InstanceMessage> {
    inbox().1.clone()
}

fn instance_file_path() -> Option<PathBuf> {
    match get_or_create_config_dir() {
        Ok(dir) => Some(dir.join(INSTANCE_FILE)),
        Err(e) => {
            warn!(error = %e, "config dir unavailable; single-instance hand-off disabled");
            None
        }
    }
}

fn read_record(path: &Path) -> Option<InstanceRecord> {
    let bytes = std::fs::read(path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Forward `message` to a running instance if there is one, else become it.
pub fn claim_instance(message: &InstanceMessage) -> InstanceRole {
    let Some(path) = instance_file_path() else {
        return InstanceRole::Primary;
    };
    if let Some(record) = read_record(&path) {
        let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, record.port));
        // A listener that took the connection but did not answer is an
        // instance still starting — it binds before it serves. Taking over
        // then pointed the record at this launch, and once it was gone at a
        // dead port while the first ran on: every later launch started a
        // new instance. So it is asked again; only a refused connection (no
        // one there) is a stale record at once.
        let mut attempt = 0;
        loop {
            match forward(addr, &record.token, message) {
                Forward::Delivered => {
                    info!(
                        port = record.port,
                        pid = record.pid,
                        "handed off to the running instance"
                    );
                    return InstanceRole::Forwarded;
                }
                Forward::Unanswered if attempt < FORWARD_RETRIES => attempt += 1,
                Forward::Unanswered | Forward::Refused => break,
            }
        }
        info!(
            port = record.port,
            pid = record.pid,
            "stale instance record; taking over"
        );
    }
    let listener = match TcpListener::bind((Ipv4Addr::LOCALHOST, 0)) {
        Ok(listener) => listener,
        Err(e) => {
            warn!(error = %e, "loopback listener failed; single-instance hand-off disabled");
            return InstanceRole::Primary;
        }
    };
    let port = match listener.local_addr() {
        Ok(addr) => addr.port(),
        Err(e) => {
            warn!(error = %e, "loopback listener has no address; single-instance hand-off disabled");
            return InstanceRole::Primary;
        }
    };
    let token = Uuid::now_v7().simple().to_string();
    let record = InstanceRecord {
        port,
        token: token.clone(),
        pid: std::process::id(),
    };
    match serde_json::to_vec(&record)
        .map_err(std::io::Error::other)
        .and_then(|bytes| write_file_atomic(&path, &bytes))
    {
        Ok(()) => {
            PENDING_SERVER
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .replace(InstanceServer { listener, token });
            InstanceRole::Primary
        }
        Err(e) => {
            warn!(error = %e, file = %path.display(), "instance record not written; single-instance hand-off disabled");
            InstanceRole::Primary
        }
    }
}

/// Remove the record on a clean quit, so the next launch skips the probe.
pub fn release_instance() {
    if let Some(path) = instance_file_path() {
        let _ = std::fs::remove_file(path);
    }
}

/// `true` only when the peer answered `OK` — anything else (nobody
/// listening, a foreign service on a reused port, a token mismatch) means
/// the caller must run the app itself.
/// How a hand-off went.
enum Forward {
    Delivered,
    /// Nothing is listening: the record is stale.
    Refused,
    /// Something took the connection and gave no `OK` in time.
    Unanswered,
}

/// Times an unanswered hand-off is tried again before this launch takes
/// over — each waits up to [`IO_TIMEOUT`] for the starting instance.
const FORWARD_RETRIES: usize = 3;

fn forward(addr: SocketAddr, token: &str, message: &InstanceMessage) -> Forward {
    let Ok(mut stream) = TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT) else {
        return Forward::Refused;
    };
    let _ = stream.set_read_timeout(Some(IO_TIMEOUT));
    let _ = stream.set_write_timeout(Some(IO_TIMEOUT));
    let Ok(body) = serde_json::to_string(message) else {
        return Forward::Unanswered;
    };
    if stream.write_all(format!("{token}\n{body}\n").as_bytes()).is_err() {
        return Forward::Unanswered;
    }
    let mut reply = String::new();
    let mut reader = BufReader::new(stream.take(MAX_REQUEST_BYTES));
    if reader.read_line(&mut reply).is_ok() && reply.trim() == "OK" {
        Forward::Delivered
    } else {
        Forward::Unanswered
    }
}

impl InstanceServer {
    /// Accept hand-offs on a background thread for the life of the process,
    /// calling `on_message` (from that thread) for each authenticated one.
    pub fn serve(self, on_message: impl Fn(InstanceMessage) + Send + 'static) {
        let spawned = std::thread::Builder::new()
            .name("zedis-instance".to_string())
            .spawn(move || {
                for stream in self.listener.incoming() {
                    let Ok(stream) = stream else {
                        continue;
                    };
                    if let Some(message) = Self::handle(stream, &self.token) {
                        on_message(message);
                    }
                }
            });
        if let Err(e) = spawned {
            warn!(error = %e, "instance listener thread failed to start");
        }
    }

    fn handle(stream: TcpStream, token: &str) -> Option<InstanceMessage> {
        let _ = stream.set_read_timeout(Some(IO_TIMEOUT));
        let _ = stream.set_write_timeout(Some(IO_TIMEOUT));
        let mut reader = BufReader::new(stream.take(MAX_REQUEST_BYTES));
        let mut line = String::new();
        reader.read_line(&mut line).ok()?;
        if line.trim() != token {
            return None;
        }
        line.clear();
        reader.read_line(&mut line).ok()?;
        let message: InstanceMessage = serde_json::from_str(line.trim()).ok()?;
        let mut stream = reader.into_inner().into_inner();
        stream.write_all(b"OK\n").ok()?;
        Some(message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn a_second_launch_is_forwarded_only_with_the_right_token() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind");
        let addr = listener.local_addr().expect("addr");
        let (tx, rx) = mpsc::channel();
        InstanceServer {
            listener,
            token: "secret".to_string(),
        }
        .serve(move |message| {
            let _ = tx.send(message);
        });
        let message = InstanceMessage {
            urls: vec!["redis://localhost:6379/2".to_string()],
        };
        assert!(matches!(forward(addr, "secret", &message), Forward::Delivered));
        assert_eq!(rx.recv_timeout(Duration::from_secs(5)).expect("delivered"), message);
        assert!(matches!(forward(addr, "wrong", &message), Forward::Unanswered));
        assert!(rx.recv_timeout(Duration::from_millis(200)).is_err());
    }

    #[test]
    fn a_dead_port_means_no_running_instance() {
        let probe = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind");
        let addr = probe.local_addr().expect("addr");
        drop(probe);
        assert!(matches!(
            forward(addr, "secret", &InstanceMessage::default()),
            Forward::Refused
        ));
    }

    /// A listener that is bound but not serving yet — an instance still
    /// starting — takes the connection and answers nothing: that is not
    /// a stale record.
    #[test]
    fn a_bound_listener_that_does_not_serve_is_unanswered_not_refused() {
        let starting = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind");
        let addr = starting.local_addr().expect("addr");
        assert!(matches!(
            forward(addr, "secret", &InstanceMessage::default()),
            Forward::Unanswered
        ));
        drop(starting);
    }
}
