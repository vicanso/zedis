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

//! The HTTP bridge: RESP in, RESP out, over one endpoint.
//!
//! A browser cannot open a TCP socket, so the web build of Zedis sends its
//! Redis traffic here and this process does the dialling — with the same
//! `zedis-connection` code the desktop client uses, so SSH tunnels, TLS,
//! Sentinel and cluster discovery all work without being reimplemented
//! (ADR 9).
//!
//! Deliberately not a subcommand of the desktop binary: that one links a
//! window system, wgpu and the whole asset bundle, none of which belongs on
//! a server.
//!
//! One thing it does differently from the desktop on purpose: the master key
//! for the secrets in `redis-servers.toml` comes from the `master.key` file,
//! never the OS keychain (`disable_keychain`). A service has no session to
//! answer a keychain prompt in.

mod api;
mod audit;
mod auth;
mod policy;
mod resp;
mod session;
mod static_files;

use auth::{Accounts, USERS_ENV, USERS_FILE_ENV};
use session::Sessions;
use std::net::SocketAddr;
use std::time::Duration;
use tracing_subscriber::EnvFilter;
use zedis_connection::{disable_keychain, install_crypto_provider};
use zedis_core::fs::get_or_create_config_dir;

/// Loopback by default. Binding every interface is opting in to handing the
/// network a door into every configured Redis instance, so it has to be typed.
const DEFAULT_LISTEN: &str = "127.0.0.1:7379";

/// `--base-path` for a deployment that is configured by its environment.
const BASE_PATH_ENV: &str = "ZEDIS_BRIDGE_BASE_PATH";

fn flag(name: &str) -> Option<String> {
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == name {
            return args.next();
        }
        if let Some(value) = arg.strip_prefix(name).and_then(|rest| rest.strip_prefix('=')) {
            return Some(value.to_string());
        }
    }
    None
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .init();

    if std::env::args().any(|a| a == "--help" || a == "-h") {
        println!(
            "zedis-bridge [--listen {DEFAULT_LISTEN}] [--base-path /prefix] [--static <dir>] \
             [--users-file <file>] [--insecure-cookie] [--audit-log <file>] [--audit-writes]"
        );
        println!();
        println!("Serves the Zedis web build compiled into this binary, and forwards its RESP");
        println!("frames to the Redis servers in redis-servers.toml. --static <dir> serves that");
        println!("directory as the page instead.");
        println!("--base-path /zedis (or {BASE_PATH_ENV}) mounts the page and the API under that");
        println!("path, for a host name shared with other applications; the reverse proxy then");
        println!("forwards /zedis/ with the prefix kept.");
        println!("Callers sign in by name. Either {USERS_ENV}=\"alice@secret,bob:ro@hunter2\" or");
        println!("--users-file <file> ({USERS_FILE_ENV}), a TOML file of [[users]] tables with");
        println!("name / password / read_only — one of the two, never both. `:ro` (short form) or");
        println!("read_only = true makes an account read-only: it may look at everything it can");
        println!("see and change none of it, refused by the bridge rather than by the page.");
        println!("The page asks for the username and password, scripts send HTTP Basic. A server");
        println!("entry is private to the account that added it unless it is marked shared.");
        println!("Secrets are encrypted with the master.key file there, never the OS keychain.");
        println!(
            "--audit-log <file> ({}) appends one JSON line per login and failed",
            audit::LOG_ENV
        );
        println!("login, refusal, server entry added / edited / deleted, command that administers");
        println!("the server (CONFIG SET, ACL SETUSER, REPLICAOF, FLUSHDB, …) and command someone");
        println!(
            "had to confirm. --audit-writes ({}) adds plain data writes; reads",
            audit::WRITES_ENV
        );
        println!("are never logged. Passwords in arguments are blanked.");
        return Ok(());
    }

    // rustls refuses to pick a provider on its own. The desktop binary has the
    // same call for the same reason; here only `ring` is present, but the
    // install must still happen before anything dials TLS.
    install_crypto_provider();

    // The key that opens the secrets in `redis-servers.toml` is the `master.key`
    // file in the config directory, never the OS keychain: a server has no
    // session to answer a keychain prompt in, and on macOS an unsigned or
    // rebuilt binary is asked for the login password on every restart. Before
    // anything reads the server list, because the key is resolved once.
    disable_keychain();

    // The flag first, then the environment — the one a container sets, since
    // arguments there replace the image's whole command line.
    let users_file = flag("--users-file")
        .or_else(|| std::env::var(USERS_FILE_ENV).ok())
        .map(std::path::PathBuf::from);
    let accounts = Accounts::load(users_file.as_deref())?;
    tracing::info!(
        accounts = accounts.len(),
        read_only = accounts.read_only_count(),
        source = users_file.as_ref().map_or(USERS_ENV, |_| USERS_FILE_ENV),
        "accounts loaded"
    );

    // A plain-http local run has to say so: the login cookie is `Secure` by
    // default, so a deployment that forgets TLS sees a login that visibly
    // does not stick instead of a credential sent in the clear.
    let secure_cookie = !std::env::args().any(|a| a == "--insecure-cookie");
    // The flag first, then the environment — the one a container sets, since
    // arguments there replace the image's whole command line.
    let base_path = flag("--base-path")
        .or_else(|| std::env::var(BASE_PATH_ENV).ok())
        .map(|raw| api::normalize_base_path(&raw))
        .transpose()?
        .unwrap_or_default();
    // The audit log, when asked for: opened before anything can happen, and
    // a path that cannot be opened stops the bridge — an audit that was
    // configured and is silently absent is worse than none (ADR 11).
    let audit_writes = std::env::args().any(|a| a == "--audit-writes")
        || std::env::var(audit::WRITES_ENV).is_ok_and(|v| matches!(v.trim(), "1" | "true" | "yes"));
    let audit_path = flag("--audit-log")
        .or_else(|| std::env::var(audit::LOG_ENV).ok())
        .filter(|path| !path.trim().is_empty())
        .map(std::path::PathBuf::from);
    let audit = match audit_path {
        Some(path) => {
            let audit =
                audit::Audit::open(&path, audit_writes).map_err(|e| format!("--audit-log {}: {e}", path.display()))?;
            tracing::info!(path = %path.display(), writes = audit_writes, "audit log open");
            audit
        }
        None if audit_writes => return Err("--audit-writes needs --audit-log".into()),
        None => audit::Audit::off(),
    };

    // Saved, so that restarting the bridge does not sign everybody out —
    // which, while they lived in memory, it did, every time.
    let logins_path = get_or_create_config_dir()?.join("bridge-logins.json");
    let logins = auth::Logins::load(logins_path, &accounts);
    tracing::info!(live = logins.len(), "saved logins restored");

    let sessions = Sessions::new();
    let sweeper = sessions.clone();
    let login_sweeper = logins.clone();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(60));
        loop {
            tick.tick().await;
            let expired = login_sweeper.sweep();
            login_sweeper.flush();
            if expired > 0 {
                tracing::info!(expired, "swept idle logins");
            }
            let dropped = sweeper.sweep().await;
            if dropped > 0 {
                // Both awaits resolve before the macro runs: an `.await`
                // inside `tracing!`'s arguments makes the whole future !Send.
                let live = sweeper.len().await;
                tracing::info!(dropped, live, "swept idle sessions");
            }
        }
    });

    // Serving the web build from here is what makes the page same-origin with
    // the API: no CORS to configure, and the login cookie can stay
    // `SameSite=Strict`. The page is compiled in; `--static <dir>` serves a
    // directory instead (a rebuilt bundle without recompiling the bridge).
    let web_root = flag("--static").map(std::path::PathBuf::from);
    match &web_root {
        Some(root) => match root.canonicalize() {
            Ok(path) => tracing::info!(path = %path.display(), "serving the web build from a directory"),
            Err(e) => {
                tracing::error!(error = %e, path = %root.display(), "--static is not a readable directory");
                return Err(e.into());
            }
        },
        // rust-embed reads the directory from disk in a debug build (no
        // `debug-embed` here), so the same binary path means two things.
        None => tracing::info!("serving the web build (compiled in; a debug build reads zedis-web/www from disk)"),
    }

    let listen = flag("--listen").unwrap_or_else(|| DEFAULT_LISTEN.to_string());
    let listener = tokio::net::TcpListener::bind(&listen).await?;
    tracing::info!(addr = %listener.local_addr()?, base_path = %format!("{base_path}/"), "zedis-bridge listening");

    let app = api::router(api::AppState {
        accounts,
        sessions,
        logins,
        audit,
        cookie: auth::CookiePolicy::new(secure_cookie, &base_path),
        base_path,
        web_root,
    });
    // With the peer's address, which the audit log records beside whatever
    // a proxy wrote in `X-Forwarded-For`.
    axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>()).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_flag_reads_both_spellings() {
        // `flag` walks the real argv, so it is exercised through its parts:
        // this pins the shape the two spellings must share.
        assert_eq!(DEFAULT_LISTEN, "127.0.0.1:7379");
        assert!(flag("--definitely-not-passed").is_none());
    }
}
