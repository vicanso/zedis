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

mod account_api;
mod accounts;
mod api;
mod audit;
mod auth;
mod handover;
mod import;
mod mcp;
mod metrics;
mod page_data;
mod policy;
mod presence;
mod redis_store;
mod resp;
mod session;
mod sql;
mod static_files;
mod store;
mod trash;
mod watch;
mod webhook;

use accounts::{Accounts, SETUP_TOKEN_ENV, SIGNUP_ENV, Setup, Signup, USERS_ENV, USERS_FILE_ENV};
use session::Sessions;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tracing_subscriber::EnvFilter;
use zedis_connection::{clear_expired_cache, disable_keychain, install_crypto_provider};
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

/// A setting's flag, else its environment variable — the one a container
/// sets, since arguments there replace the image's whole command line. Blank
/// is unset.
fn setting(flag_name: &str, env_name: &str) -> Option<String> {
    flag(flag_name)
        .or_else(|| std::env::var(env_name).ok())
        .filter(|v| !v.trim().is_empty())
}

/// Where the bridge keeps what it keeps (ADR 18): the database `--store`
/// names (PostgreSQL, MySQL, or Redis/Valkey), or the local directory.
async fn open_stores(data_dir: &std::path::Path) -> std::io::Result<store::Stores> {
    match store_url() {
        Some(url) if redis_store::is_redis_url(&url) => {
            let store = redis_store::RedisStore::connect(&url).await?;
            tracing::info!(
                store = sql::redacted(&url),
                prefix = store.prefix(),
                "the stores are in Redis"
            );
            Ok(store.stores())
        }
        Some(url) => {
            let store = sql::SqlStore::connect(&url).await?;
            tracing::info!(store = sql::redacted(&url), dialect = ?store.dialect(), "the stores are in a database");
            Ok(store.stores())
        }
        None => Ok(store::Stores::local(data_dir)),
    }
}

/// The database `--store` (or the environment) names, if any.
fn store_url() -> Option<String> {
    flag("--store")
        .or_else(|| std::env::var(sql::STORE_ENV).ok())
        .filter(|url| !url.trim().is_empty())
}

/// `zedis-bridge import-local`: a local directory's stores, copied once
/// into an empty database (ADR 18).
async fn import_local_command() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::args().any(|a| a == "--help" || a == "-h") {
        println!("zedis-bridge import-local --store <url> [--from <dir>] [--audit-file <file>]");
        println!();
        println!("Copies what a bridge keeps in its data directory (--from, else ZEDIS_CONFIG_DIR)");
        println!("— accounts, invitations, waiting requests, logins, the server list, scripts, the page's");
        println!("data, the recycle bin, the metrics history and a waiting setup token — into the empty");
        println!(
            "database --store ({}) names. It runs with that directory's master.key:",
            sql::STORE_ENV
        );
        println!("give every bridge on the database the same file afterwards. A database that");
        println!("already holds accounts, server entries or scripts is refused. --audit-file copies");
        println!("the lines of the file --audit-log wrote, too.");
        return Ok(());
    }
    // The directory's master key opens its secrets and seals the database's
    // check, so the directory is the configuration directory for this run.
    if let Some(from) = flag("--from") {
        zedis_core::fs::override_config_dir(std::path::PathBuf::from(from));
    }
    install_crypto_provider();
    disable_keychain();
    let from = get_or_create_config_dir()?;
    let url = store_url().ok_or_else(|| format!("import-local needs --store <url> or {}", sql::STORE_ENV))?;
    let audit_file = flag("--audit-file").map(std::path::PathBuf::from);
    let done = if redis_store::is_redis_url(&url) {
        let store = redis_store::RedisStore::connect(&url).await?;
        import::import_local(&from, audit_file.as_deref(), &store).await?
    } else {
        let store = sql::SqlStore::connect(&url).await?;
        import::import_local(&from, audit_file.as_deref(), &store).await?
    };
    println!("Copied {} into {}:", from.display(), sql::redacted(&url));
    println!("{done}");
    println!();
    println!(
        "Every bridge on this database now uses {}: give each the same file.",
        from.join("master.key").display()
    );
    Ok(())
}

/// `zedis-bridge setup-token`: a fresh setup token, for when every
/// administrator is lost (ADR 17). Written beside the bridge's data, where
/// the running bridge reads it at the next attempt, and printed once. Host
/// access is administrator access; nothing over HTTP can do this.
async fn setup_token_command() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::args().any(|a| a == "--help" || a == "-h") {
        println!("zedis-bridge setup-token [--store <url>]");
        println!();
        println!("Writes a one-time setup token to setup-token in the bridge's data directory");
        println!(
            "(ZEDIS_CONFIG_DIR) — or, with --store ({}), to the database the bridges",
            sql::STORE_ENV
        );
        println!("share — and prints it. On the bridge's page, Create the administrator with it:");
        println!("a new account, or an existing one by its password, becomes an administrator.");
        println!("The token is deleted once used.");
        return Ok(());
    }
    install_crypto_provider();
    disable_keychain();
    let setup = Setup::new(open_stores(&get_or_create_config_dir()?).await?.setup);
    let token = setup.mint().await?;
    println!("Setup token — shown once, also in {}:", setup.location());
    println!();
    println!("  {token}");
    println!();
    println!("Open the bridge's page and choose Create the administrator.");
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .init();

    if std::env::args().nth(1).as_deref() == Some("setup-token") {
        return setup_token_command().await;
    }
    if std::env::args().nth(1).as_deref() == Some("import-local") {
        return import_local_command().await;
    }

    if std::env::args().any(|a| a == "--help" || a == "-h") {
        print_help();
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
    let trusted = trusted_proxy(
        setting("--trusted-header", auth::TRUSTED_HEADER_ENV),
        setting("--trusted-proxy", auth::TRUSTED_PROXY_ENV),
    )?;
    // Requests for an account (ADR 17, amended 2026-10-18): off unless a
    // deployment opens that door, and a word it is not understood stops the
    // bridge rather than leaving it shut or open by surprise.
    let signup = Signup::from_setting(setting("--signup", SIGNUP_ENV).as_deref())?;
    if signup.is_open() {
        tracing::info!("anyone may ask for an account: administrators approve or reject each on the account page");
    }
    // What the bridge keeps, in the local directory (ADR 17). The lists are
    // read now rather than by the first request that asks: a file that
    // cannot be read says so at start.
    let data_dir = get_or_create_config_dir()?;
    let stores = open_stores(&data_dir).await?;
    let entries = stores.servers.list().await?.len();
    tracing::info!(entries, "server list loaded");
    let accounts = Accounts::load(stores.accounts.clone()).await?;
    let setup = Setup::new(stores.setup.clone());

    report_proxy(trusted.as_ref(), &accounts);

    let audit_writes = std::env::args().any(|a| a == "--audit-writes")
        || std::env::var(audit::WRITES_ENV).is_ok_and(|v| matches!(v.trim(), "1" | "true" | "yes"));
    let audit_setting = flag("--audit-log")
        .or_else(|| std::env::var(audit::LOG_ENV).ok())
        .map(|setting| setting.trim().to_string())
        .filter(|setting| !setting.is_empty());
    let audit = open_audit(audit_setting.as_deref(), audit_writes, stores.audit.clone())?;
    let audit_retention_days = flag("--audit-retention-days")
        .or_else(|| std::env::var(audit::RETENTION_DAYS_ENV).ok())
        .map(|days| days.trim().parse::<u32>())
        .transpose()
        .map_err(|e| format!("--audit-retention-days: {e}"))?
        .unwrap_or(audit::RETENTION_DAYS);
    let audit = with_audit_webhook(audit)?;

    import_configured_users(&accounts, &audit).await?;
    report_accounts(&accounts, &setup).await?;

    // Saved, so that restarting the bridge does not sign everybody out —
    // which, while they lived in memory, it did, every time.
    let logins = auth::Logins::load(stores.logins.clone(), &accounts).await;
    tracing::info!(live = logins.len(), "saved logins restored");
    let throttle = match stores.throttle.clone() {
        Some(shared) => auth::Throttle::shared(shared, store::process_id().to_string()),
        None => auth::Throttle::new(),
    };
    if let Some(versions) = stores.versions.clone() {
        let held = store::Held {
            servers: stores.servers.clone(),
            accounts: accounts.clone(),
            logins: logins.clone(),
            throttle: throttle.clone(),
        };
        tokio::spawn(store::keep_in_step(versions, held));
    }

    let sessions = Sessions::new();
    let unlocks = policy::Unlocks::new();
    let presence = presence::Presence::new();
    let watches = watch::Watches::new();
    tokio::spawn(
        Housekeeping {
            sessions: sessions.clone(),
            logins: logins.clone(),
            accounts: accounts.clone(),
            throttle: throttle.clone(),
            unlocks: unlocks.clone(),
            presence: presence.clone(),
            watches: watches.clone(),
            trash: stores.trash.clone(),
            metrics: stores.metrics.clone(),
            audit: audit.log().cloned(),
            audit_retention_days,
        }
        .run(),
    );

    let web_root = web_root()?;

    let listen = flag("--listen").unwrap_or_else(|| DEFAULT_LISTEN.to_string());
    let listener = tokio::net::TcpListener::bind(&listen).await?;
    tracing::info!(addr = %listener.local_addr()?, base_path = %format!("{base_path}/"), "zedis-bridge listening");

    let app = api::router(api::AppState {
        accounts,
        sessions,
        logins,
        audit,
        trusted,
        unlocks,
        cookie: auth::CookiePolicy::new(secure_cookie, &base_path),
        base_path,
        web_root,
        mcp_calls: mcp::Limiter::new(),
        presence,
        watches,
        servers: stores.servers,
        trash: stores.trash,
        metrics: stores.metrics,
        page_rows: stores.page_rows,
        key_notes: stores.key_notes,
        scripts: stores.scripts,
        throttle,
        setup,
        signup,
        store_probe: stores.versions.clone(),
    });
    // With the peer's address, which the audit log records beside whatever
    // a proxy wrote in `X-Forwarded-For`.
    axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>()).await?;
    Ok(())
}

/// `--help`.
fn print_help() {
    println!(
        "zedis-bridge [--listen {DEFAULT_LISTEN}] [--base-path /prefix] [--static <dir>] \
         [--insecure-cookie] [--store <url>] [--audit-log <file>] [--audit-writes] \
         [--audit-webhook <url> [--audit-webhook-format <name>] [--audit-webhook-events <list>]] \
         [--trusted-header <name> --trusted-proxy <cidr,…>] [--signup approval]"
    );
    println!();
    println!("POST /v1/mcp is a Model Context Protocol server for an AI assistant (Claude");
    println!("Code, Cursor, …): a read-only account's token (issued on its account page,");
    println!("sent as `Authorization: Bearer …`) or HTTP Basic, and tools that list the");
    println!("servers, scan keys, describe one key, read INFO and SLOWLOG and run any other");
    println!("read-only command. A full account is refused there, every call is one audit");
    println!(
        "line, and an account may make {} calls a minute.",
        mcp::CALLS_PER_MINUTE
    );
    println!();
    println!("Serves the Zedis web build compiled into this binary, and forwards its RESP");
    println!("frames to the Redis servers in redis-servers.toml. --static <dir> serves that");
    println!("directory as the page instead.");
    println!("--base-path /zedis (or {BASE_PATH_ENV}) mounts the page and the API under that");
    println!("path, for a host name shared with other applications; the reverse proxy then");
    println!("forwards /zedis/ with the prefix kept.");
    println!("Accounts are registered, never configured. The first is the administrator: while");
    println!("there is none, the bridge prints a setup token in its log and writes it to");
    println!("setup-token in its data directory ({SETUP_TOKEN_ENV} chooses one in advance),");
    println!("and the page offers Create the administrator. After that an administrator");
    println!("invites each account — a role (administrator, full or read-only), the servers it");
    println!("sees, an expiry — and sends the link. `zedis-bridge setup-token` writes a new");
    println!("setup token when every administrator is lost. A deployment that still sets");
    println!("{USERS_ENV} or --users-file ({USERS_FILE_ENV}) has those accounts imported once,");
    println!("into an empty store, as full or read-only — never as administrator.");
    println!("--signup approval ({SIGNUP_ENV}) also lets anyone ask for an account from the sign-in");
    println!("page: a request signs nobody in until an administrator approves it, with a role and");
    println!("server rules, on the account page — or rejects it. Off unless set; a request waits a");
    println!(
        "week, at most {} wait at once, and every request is an audit line.",
        accounts::MAX_SIGNUPS
    );
    println!("The page asks for the username and password, scripts send HTTP Basic. A server");
    println!("entry is private to the account that added it unless it is marked shared.");
    println!("Secrets are encrypted with the master.key file there, never the OS keychain.");
    println!(
        "--store <url> ({}, preferred: the URL carries a password) keeps the accounts, logins,",
        sql::STORE_ENV
    );
    println!("server list, recycle bin, metrics and the page's data in PostgreSQL 14+, MySQL");
    println!("8.0.19+ or Redis/Valkey (postgres://…, mysql://…, or redis://…[?prefix=zedis])");
    println!("instead of that directory, so several bridges can share them: route each browser");
    println!("to one of them by its login cookie, and give every bridge the same master.key.");
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
    println!(
        "--audit-webhook <url> ({}) pushes those lines to a chat as they happen:",
        webhook::URL_ENV
    );
    println!("a Feishu / Lark, DingTalk, WeCom or Slack bot (told apart by its host, or named");
    println!(
        "with --audit-webhook-format json|feishu|dingtalk|wecom|slack, {}), or any",
        webhook::FORMAT_ENV
    );
    println!("other URL, which gets them as JSON. Every few seconds, one message; a signing");
    println!(
        "secret goes in {} (Feishu, DingTalk). By default all but sign-ins,",
        webhook::SECRET_ENV
    );
    println!(
        "sign-outs and MCP calls; --audit-webhook-events ({}) takes a list of",
        webhook::EVENTS_ENV
    );
    println!(
        "{}, or all. Plain data writes are never pushed.",
        webhook::EVENTS.join(", ")
    );
    println!(
        "--trusted-header Remote-User ({}) believes the identity a reverse proxy",
        auth::TRUSTED_HEADER_ENV
    );
    println!("(oauth2-proxy, Authelia, …) writes into that header — only on connections from",);
    println!(
        "--trusted-proxy 10.0.0.0/8,… ({}); both or neither. The name must be an",
        auth::TRUSTED_PROXY_ENV
    );
    println!("account here; an invitation marked for the proxy registers that name, with no");
    println!("password. An account's server rules — \"prod-*:ro\", \"staging\", \"id:…\" — are");
    println!("the shared entries it sees (by name glob or id) and where it is read-only");
    println!("(\":ro\"; where rules disagree, write wins); none is every shared entry. Its own");
    println!("entries an account always sees and writes.");
}

/// Identities from a reverse proxy: both settings or neither, because a
/// header without the addresses it is believed from is a header anyone
/// can write (`auth::TrustedProxy`). An account with no password is one
/// only the proxy can sign in, so without a proxy it is a mistake.
fn trusted_proxy(
    header: Option<String>,
    networks: Option<String>,
) -> Result<Option<auth::TrustedProxy>, Box<dyn std::error::Error>> {
    let trusted = match (header, networks) {
        (Some(header), Some(networks)) => Some(auth::TrustedProxy::new(&header, &networks)?),
        (None, None) => None,
        (Some(_), None) => {
            return Err(
                "--trusted-header needs --trusted-proxy: without the proxy's addresses, \
                        anyone could write the header"
                    .into(),
            );
        }
        (None, Some(_)) => {
            return Err("--trusted-proxy needs --trusted-header: which header carries the identity?".into());
        }
    };
    Ok(trusted)
}

/// Whom the reverse proxy signs in, or — with no proxy — the accounts that
/// cannot sign in at all.
fn report_proxy(trusted: Option<&auth::TrustedProxy>, accounts: &Accounts) {
    let passwordless = accounts.passwordless();
    match trusted {
        Some(proxy) => tracing::info!(
            header = proxy.header(),
            networks = %proxy.networks(),
            proxy_only_accounts = passwordless.len(),
            "identities from the reverse proxy are trusted"
        ),
        // Registered behind a proxy that is no longer configured: they are
        // still accounts, and an administrator can reset them a password.
        None if !passwordless.is_empty() => tracing::warn!(
            accounts = passwordless.join(", "),
            "accounts without a password cannot sign in without --trusted-header"
        ),
        None => {}
    }
}

/// The audit log, when asked for: opened before anything can happen, and
/// a path that cannot be opened stops the bridge — an audit that was
/// configured and is silently absent is worse than none (ADR 11).
///
/// With a database the audit is on and kept there unless told otherwise
/// (ADR 18): a path still means a file, and `off` means none.
fn open_audit(
    audit_setting: Option<&str>,
    audit_writes: bool,
    database: Option<Arc<dyn store::AuditLog>>,
) -> Result<audit::Audit, Box<dyn std::error::Error>> {
    let audit = match (audit_setting, database) {
        (Some(audit::OFF), _) if audit_writes => return Err("--audit-writes needs an audit; --audit-log is off".into()),
        (Some(audit::OFF), _) => audit::Audit::off(),
        (Some(path), shared) => {
            let path = std::path::PathBuf::from(path);
            let audit =
                audit::Audit::open(&path, audit_writes).map_err(|e| format!("--audit-log {}: {e}", path.display()))?;
            if shared.is_some() {
                tracing::warn!(
                    path = %path.display(),
                    "the audit goes to a file instead of the database: each bridge on it keeps its own"
                );
            }
            tracing::info!(path = %path.display(), writes = audit_writes, "audit log open");
            audit
        }
        (None, Some(log)) => {
            tracing::info!(writes = audit_writes, "audit kept in the database");
            audit::Audit::to(log, audit_writes)
        }
        (None, None) if audit_writes => return Err("--audit-writes needs --audit-log".into()),
        (None, None) => audit::Audit::off(),
    };
    Ok(audit)
}

/// The webhook, when asked for, checked here too: a URL or a list that
/// is wrong stops the bridge rather than pushing nothing all day.
fn with_audit_webhook(audit: audit::Audit) -> Result<audit::Audit, Box<dyn std::error::Error>> {
    let webhook_url = flag("--audit-webhook")
        .or_else(|| std::env::var(webhook::URL_ENV).ok())
        .filter(|url| !url.trim().is_empty());
    let audit = match webhook_url {
        Some(url) => {
            let config = webhook::WebhookConfig::new(
                &url,
                flag("--audit-webhook-format")
                    .or_else(|| std::env::var(webhook::FORMAT_ENV).ok())
                    .as_deref(),
                flag("--audit-webhook-secret").or_else(|| std::env::var(webhook::SECRET_ENV).ok()),
                flag("--audit-webhook-events")
                    .or_else(|| std::env::var(webhook::EVENTS_ENV).ok())
                    .as_deref(),
            )
            .map_err(|e| format!("--audit-webhook: {e}"))?;
            // The host, never the URL: the bot's key is in it.
            tracing::info!(
                host = config.host(),
                format = config.format().name(),
                events = %config.events().join(","),
                signed = config.signed(),
                "audit webhook on"
            );
            audit.with_webhook(webhook::Webhook::start(config))
        }
        None => audit,
    };
    Ok(audit)
}

/// The accounts a deployment configured before registration, imported
/// once into an empty store — after the audit is open, since each is a
/// line — and never read again.
async fn import_configured_users(accounts: &Accounts, audit: &audit::Audit) -> Result<(), Box<dyn std::error::Error>> {
    let users_file = flag("--users-file")
        .or_else(|| std::env::var(USERS_FILE_ENV).ok())
        .map(std::path::PathBuf::from);
    if let Some(records) = accounts::import_records(users_file.as_deref())? {
        let names: Vec<(String, &'static str)> = records.iter().map(|r| (r.name.clone(), r.role.name())).collect();
        match accounts.import(records).await? {
            0 => tracing::warn!(
                accounts = accounts.len(),
                "{USERS_ENV} / {USERS_FILE_ENV} are ignored: accounts are registered now, and the store has some"
            ),
            imported => {
                for (name, role) in &names {
                    audit.record(
                        name,
                        &audit::Origin::default(),
                        audit::Event::AccountImported {
                            target: name.clone(),
                            role,
                        },
                    );
                }
                tracing::info!(
                    imported,
                    "accounts imported from the configured users; remove that setting now"
                );
            }
        }
    }
    Ok(())
}

/// How many accounts there are, and — while none is an administrator — how
/// one is seated.
async fn report_accounts(accounts: &Accounts, setup: &Setup) -> Result<(), Box<dyn std::error::Error>> {
    tracing::info!(
        accounts = accounts.len(),
        administrators = accounts.admin_count(),
        read_only = accounts.read_only_count(),
        with_server_rules = accounts.with_server_rules(),
        mcp_tokens = accounts.mcp_token_count(),
        "accounts loaded"
    );
    // No administrator: the setup token seats one (ADR 17).
    if accounts.admin_count() == 0 {
        if std::env::var(SETUP_TOKEN_ENV).is_ok_and(|t| !t.trim().is_empty()) {
            tracing::warn!("no administrator yet: open the page and Create the administrator with {SETUP_TOKEN_ENV}");
        } else {
            match setup.for_start().await? {
                Some(token) => tracing::warn!(
                    kept = setup.location(),
                    "no administrator yet: open the page and Create the administrator with the setup token {token}"
                ),
                // Another process kept one first, and printed it.
                None => tracing::warn!(
                    kept = setup.location(),
                    "no administrator yet: a setup token is waiting (in the log of the bridge that kept it; \
                     `zedis-bridge setup-token` keeps a new one)"
                ),
            }
        }
    }
    Ok(())
}

/// What the minute's sweep keeps in order: each a handle onto what the
/// routes hold too.
struct Housekeeping {
    sessions: Sessions,
    logins: auth::Logins,
    accounts: Accounts,
    throttle: auth::Throttle,
    unlocks: policy::Unlocks,
    presence: presence::Presence,
    watches: watch::Watches,
    trash: Arc<dyn store::TrashStore>,
    metrics: Arc<dyn store::MetricsStore>,
    audit: Option<Arc<dyn store::AuditLog>>,
    audit_retention_days: u32,
}

impl Housekeeping {
    /// Once a minute, for the life of the process.
    async fn run(self) {
        let Self {
            sessions: sweeper,
            logins: login_sweeper,
            accounts: account_sweeper,
            throttle: throttle_sweeper,
            unlocks: unlock_sweeper,
            presence: presence_sweeper,
            watches: watch_sweeper,
            trash: trash_sweeper,
            metrics: metrics_pruner,
            audit: audit_pruner,
            audit_retention_days,
        } = self;
        let mut tick = tokio::time::interval(Duration::from_secs(60));
        loop {
            tick.tick().await;
            let expired = login_sweeper.sweep(&account_sweeper);
            login_sweeper.flush().await;
            if expired > 0 {
                tracing::info!(expired, "swept idle logins");
            }
            throttle_sweeper.sweep().await;
            match account_sweeper.purge_expired_invites().await {
                Ok(0) => {}
                Ok(purged) => tracing::info!(purged, "purged expired invitations"),
                Err(e) => tracing::warn!(error = %e, "the expired invitations could not be purged"),
            }
            match account_sweeper.purge_expired_signups().await {
                Ok(0) => {}
                Ok(purged) => tracing::info!(purged, "purged requests for an account nobody answered"),
                Err(e) => tracing::warn!(error = %e, "the unanswered requests could not be purged"),
            }
            // Five quiet minutes: each client that went is a line, with how
            // many are left.
            let (gone, online) = presence_sweeper.sweep();
            for (account, kind) in gone {
                tracing::info!(account, kind, online, "client offline");
            }
            let ended = unlock_sweeper.sweep();
            if ended > 0 {
                tracing::info!(ended, "swept ended write windows");
            }
            match trash_sweeper.purge_expired().await {
                Ok(0) => {}
                Ok(purged) => tracing::info!(purged, "purged day-old recycle bin entries"),
                Err(e) => tracing::warn!(error = %e, "the recycle bin purge did not finish"),
            }
            if audit_retention_days > 0
                && let Some(log) = &audit_pruner
            {
                let kept_ms = i64::from(audit_retention_days) * 86_400_000;
                match log.prune(metrics::now_ms() - kept_ms).await {
                    Ok(0) => {}
                    Ok(pruned) => tracing::info!(pruned, days = audit_retention_days, "pruned old audit lines"),
                    Err(e) => tracing::warn!(error = %e, "the old audit lines could not be pruned"),
                }
            }
            if let Ok(pruned) = metrics_pruner.prune().await
                && pruned > 0
            {
                tracing::info!(pruned, "pruned week-old metrics");
            }
            let ended = watch_sweeper.sweep();
            if ended > 0 {
                tracing::info!(ended, live = watch_sweeper.len(), "swept idle and expired watches");
            }
            let dropped = sweeper.sweep().await;
            if dropped > 0 {
                // Both awaits resolve before the macro runs: an `.await`
                // inside `tracing!`'s arguments makes the whole future !Send.
                let live = sweeper.len().await;
                tracing::info!(dropped, live, "swept idle sessions");
            }
            // The pooled clients and their sockets, idle past their five
            // minutes. An expired entry is only ever *taken out* here: the
            // cache stops handing it out by itself, but it goes on holding
            // the connection open until something removes it, and the
            // desktop's housekeeping tick is the GUI's — this process had
            // none. Every server and database anyone had looked at once kept
            // its connections until the bridge was restarted (#176).
            clear_expired_cache();
        }
    }
}

/// Serving the web build from here is what makes the page same-origin with
/// the API: no CORS to configure, and the login cookie can stay
/// `SameSite=Strict`. The page is compiled in; `--static <dir>` serves a
/// directory instead (a rebuilt bundle without recompiling the bridge).
fn web_root() -> Result<Option<std::path::PathBuf>, Box<dyn std::error::Error>> {
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
    Ok(web_root)
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

    #[test]
    fn a_trusted_header_needs_the_proxies_it_is_believed_from() {
        let proxy = trusted_proxy(Some("Remote-User".to_string()), Some("10.0.0.0/8".to_string()))
            .expect("both settings")
            .expect("a proxy");
        assert_eq!(proxy.header(), "remote-user");
        assert!(trusted_proxy(None, None).expect("neither setting").is_none());

        // Either alone stops the bridge: a header believed from anywhere is
        // one anyone can write.
        let header_alone =
            trusted_proxy(Some("Remote-User".to_string()), None).expect_err("a header without its proxies");
        assert!(header_alone.to_string().contains("--trusted-proxy"), "{header_alone}");
        let proxies_alone = trusted_proxy(None, Some("10.0.0.0/8".to_string())).expect_err("proxies without a header");
        assert!(
            proxies_alone.to_string().contains("--trusted-header"),
            "{proxies_alone}"
        );
    }

    #[test]
    fn the_audit_goes_where_the_settings_say_and_a_wrong_one_stops_the_bridge() {
        let dir = std::env::temp_dir().join(format!("zedis-bridge-audit-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).expect("scratch directory");
        let database: Arc<dyn store::AuditLog> =
            Arc::new(audit::LocalAuditLog::open(&dir.join("database.log")).expect("a stand-in for the database"));

        // Nothing asked for and no database: no audit, and writes have none
        // to go to.
        assert!(open_audit(None, false, None).expect("no audit").log().is_none());
        let Err(no_log) = open_audit(None, true, None) else {
            panic!("--audit-writes without an audit started");
        };
        assert!(no_log.to_string().contains("--audit-log"), "{no_log}");

        // A database keeps it unless told otherwise.
        let kept = open_audit(None, true, Some(database.clone())).expect("the database's audit");
        assert!(kept.log().is_some_and(|log| Arc::ptr_eq(log, &database)));
        assert!(kept.logs_writes());

        // `off` is off, database or not, and writes beside it contradict it.
        let off = open_audit(Some(audit::OFF), false, Some(database.clone())).expect("off");
        assert!(off.log().is_none());
        let Err(off_with_writes) = open_audit(Some(audit::OFF), true, Some(database.clone())) else {
            panic!("--audit-writes with the audit off started");
        };
        assert!(off_with_writes.to_string().contains("off"), "{off_with_writes}");

        // A path is a file, even beside a database.
        let path = dir.join("audit.log");
        let file = open_audit(Some(&path.to_string_lossy()), false, Some(database.clone())).expect("a file");
        assert!(file.log().is_some_and(|log| !Arc::ptr_eq(log, &database)));
        assert!(!file.logs_writes());
        assert!(path.exists());

        // One that cannot be opened stops the bridge, rather than auditing
        // nothing.
        let unopenable = path.join("audit.log");
        let Err(refused) = open_audit(Some(&unopenable.to_string_lossy()), false, None) else {
            panic!("an audit that cannot be opened started");
        };
        assert!(refused.to_string().starts_with("--audit-log "), "{refused}");

        std::fs::remove_dir_all(&dir).expect("remove the scratch directory");
    }
}
