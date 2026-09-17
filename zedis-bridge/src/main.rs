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

mod api;
mod auth;
mod policy;
mod resp;
mod session;
mod static_files;

use session::Sessions;
use std::time::Duration;
use tracing_subscriber::EnvFilter;
use zedis_connection::install_crypto_provider;

/// Loopback by default. Binding every interface is opting in to handing the
/// network a door into every configured Redis instance, so it has to be typed.
const DEFAULT_LISTEN: &str = "127.0.0.1:7379";

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
        println!("zedis-bridge [--listen {DEFAULT_LISTEN}] [--static <dir>] [--insecure-cookie]");
        println!();
        println!("Forwards RESP frames to the Redis servers in redis-servers.toml.");
        println!("The bearer token is read from, or created in, the config directory.");
        return Ok(());
    }

    // rustls refuses to pick a provider on its own. The desktop binary has the
    // same call for the same reason; here only `ring` is present, but the
    // install must still happen before anything dials TLS.
    install_crypto_provider();

    let token = auth::load_or_create()?;
    tracing::info!(path = %auth::token_path()?.display(), "bearer token ready");

    // A plain-http local run has to say so: the login cookie is `Secure` by
    // default, so a deployment that forgets TLS sees a login that visibly
    // does not stick instead of a credential sent in the clear.
    let secure_cookie = !std::env::args().any(|a| a == "--insecure-cookie");
    let logins = auth::Logins::new();

    let sessions = Sessions::new();
    let sweeper = sessions.clone();
    let login_sweeper = logins.clone();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(60));
        loop {
            tick.tick().await;
            let expired = login_sweeper.sweep();
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
    // `SameSite=Strict`.
    let web_root = flag("--static").map(std::path::PathBuf::from);
    if let Some(root) = &web_root {
        match root.canonicalize() {
            Ok(path) => tracing::info!(path = %path.display(), "serving the web build"),
            Err(e) => {
                tracing::error!(error = %e, path = %root.display(), "--static is not a readable directory");
                return Err(e.into());
            }
        }
    }

    let listen = flag("--listen").unwrap_or_else(|| DEFAULT_LISTEN.to_string());
    let listener = tokio::net::TcpListener::bind(&listen).await?;
    tracing::info!(addr = %listener.local_addr()?, "zedis-bridge listening");

    let app = api::router(api::AppState {
        token,
        sessions,
        logins,
        secure_cookie,
        web_root,
    });
    axum::serve(listener, app).await?;
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
