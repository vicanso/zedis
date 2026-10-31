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

//! The browser's carrier for `zedis-connection`'s [`BridgeTransport`].
//!
//! It lives here, not in `zedis-connection`, because that crate deliberately
//! has no HTTP client and no gpui (ADR 9). GPUI's own [`HttpClient`] is what
//! makes the split pay: under wasm it is the browser's `fetch`, and on the
//! host it is the native client — which is why this module is not itself
//! browser-only, and its tests run in an ordinary `cargo test`.
//!
//! It carries five of that crate's seams, one submodule each — `commands`
//! ([`BridgeTransport`]), `storage` ([`BridgeStorage`]), `servers`
//! ([`BridgeServerStore`]), `accounts` ([`BridgeAccounts`]) and `page_data`
//! ([`BridgePageData`]) — over the requests this file makes for all of them.

use base64::{Engine as _, engine::general_purpose::STANDARD as B64};
use futures::AsyncReadExt as _;
use futures::StreamExt as _;
use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use futures::future::BoxFuture;
use gpui::http_client::{AsyncBody, HttpClient, http::Request};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use zedis_connection::{
    BRIDGE_MAX_REQUEST_BYTES, BridgeAccount, BridgeAccountChange, BridgeAccountError, BridgeAccountErrorKind,
    BridgeAccountResult, BridgeAccounts, BridgeAuditPage, BridgeAuditQuery, BridgeClient, BridgeClients,
    BridgeDisposition, BridgeError, BridgeErrorKind, BridgeInvite, BridgeInviteKind, BridgeKeyNote, BridgeNewInvite,
    BridgeOrphans, BridgeOwnAccount, BridgeOwned, BridgeOwnedItem, BridgePageData, BridgePageRow, BridgePageSeed,
    BridgeReply, BridgeRequest, BridgeRole, BridgeScript, BridgeServerStore, BridgeSignup, BridgeStorage, BridgeToken,
    BridgeTransport, BridgeTrashEntry, DiagReportRow, KillReply, KillTarget, RedisServer, WatchBatch, WatchItem,
    WatchKind, WatchOpened, describe_commands, get_servers, set_account_read_only_on,
};

mod accounts;
mod commands;
mod page_data;
mod servers;
mod storage;

pub use page_data::{PageData, page_seed};

/// Talks to one `zedis-bridge`.
pub struct HttpBridgeTransport {
    client: Arc<dyn HttpClient>,
    /// No trailing slash.
    base_url: String,
    /// The page's data changes, sent one after another in the order they
    /// were made ([`BridgePageData`]). Started with the first one.
    page_writes: OnceLock<UnboundedSender<PageWrite>>,
    /// How many of those have not been answered yet.
    page_writes_pending: Arc<AtomicUsize>,
    /// Asks for an early read of what other accounts changed
    /// ([`BridgePageData::refresh_soon`]).
    page_data_wake: Wake,
    /// Asks for an early read of the server list: an account's deletion or
    /// a settlement handed entries over or shared them.
    server_list_wake: Wake,
}

/// A nudge to one of the page's loops, to run its round now: sent from
/// anywhere, heard by the loop that takes the listening end — once.
struct Wake {
    sender: UnboundedSender<()>,
    listener: Mutex<Option<UnboundedReceiver<()>>>,
}

impl Wake {
    fn new() -> Self {
        let (sender, listener) = unbounded();
        Self {
            sender,
            listener: Mutex::new(Some(listener)),
        }
    }

    fn take(&self) -> Option<UnboundedReceiver<()>> {
        self.listener.lock().unwrap_or_else(PoisonError::into_inner).take()
    }
}

/// One change of the page's data, as the request that carries it.
struct PageWrite {
    method: &'static str,
    path: String,
    body: String,
}

impl HttpBridgeTransport {
    /// No credential: the page signed in for a cookie before it started the
    /// application, and a same-origin fetch carries that on its own. (There
    /// used to be a bearer token here for the bridge's token mode, which is
    /// gone — ADR 9.)
    pub fn new(client: Arc<dyn HttpClient>, base_url: impl Into<String>) -> Self {
        Self {
            client,
            base_url: base_url.into().trim_end_matches('/').to_string(),
            page_writes: OnceLock::new(),
            page_writes_pending: Arc::default(),
            page_data_wake: Wake::new(),
            server_list_wake: Wake::new(),
        }
    }

    /// Where [`BridgePageData::refresh_soon`] is heard — once, by the page's
    /// refresh loop.
    pub fn take_refresh_requests(&self) -> Option<UnboundedReceiver<()>> {
        self.page_data_wake.take()
    }

    /// Where an early read of the server list is asked for — once, by the
    /// page's server list loop.
    pub fn take_server_list_requests(&self) -> Option<UnboundedReceiver<()>> {
        self.server_list_wake.take()
    }

    /// POST `body` to `url` and hand back the status and the raw response.
    async fn post(client: Arc<dyn HttpClient>, url: String, body: String) -> Result<(u16, Vec<u8>), BridgeError> {
        Self::call(client, "POST", url, body).await
    }

    /// Send one request and hand back the status and the raw response.
    ///
    /// Both are needed: the status says which kind of failure this is, and
    /// the body carries the detail the confirm dialog needs.
    async fn call(
        client: Arc<dyn HttpClient>,
        method: &'static str,
        url: String,
        body: String,
    ) -> Result<(u16, Vec<u8>), BridgeError> {
        // The bridge would only answer `413`, after the whole body crossed
        // the network to get it.
        if body.len() > BRIDGE_MAX_REQUEST_BYTES {
            return Err(too_large(body.len()));
        }
        let builder = Request::builder()
            .method(method)
            .uri(&url)
            .header("content-type", "application/json");
        let request = builder
            .body(AsyncBody::from(body))
            .map_err(|e| BridgeError::transport(format!("could not build the request: {e}")))?;
        let mut response = client
            .send(request)
            .await
            .map_err(|e| BridgeError::transport(format!("{url} is unreachable: {e}")))?;
        let status = response.status().as_u16();
        let mut bytes = Vec::new();
        response
            .body_mut()
            .read_to_end(&mut bytes)
            .await
            .map_err(|e| BridgeError::transport(format!("the bridge's reply could not be read: {e}")))?;
        if status == 401 {
            sign_in_again();
        }
        Ok((status, bytes))
    }
}

/// The login is gone — it lapsed, it was closed, or the account's password
/// changed — and the application is still running on it. Reloading is the way
/// back: the page asks `/v1/servers` before it starts anything, gets the same
/// 401, and shows the sign-in form. No loop in that, because the form is
/// plain HTML and nothing here runs until someone has signed in.
#[cfg(target_family = "wasm")]
fn sign_in_again() {
    if let Some(window) = web_sys::window() {
        let _ = window.location().reload();
    }
}

/// Off the browser there is no page to send anyone back to; the caller gets
/// the `Unauthorized` error like any other.
#[cfg(not(target_family = "wasm"))]
fn sign_in_again() {}

/// A request the bridge does not take, in the terms the person who made it
/// can act on: how large it was, and how large one may be.
fn too_large(bytes: usize) -> BridgeError {
    const MIB: f64 = 1024.0 * 1024.0;
    BridgeError::new(
        BridgeErrorKind::TooLarge,
        format!(
            "the request is {:.1} MB, more than the {:.0} MB the bridge accepts in one",
            bytes as f64 / MIB,
            BRIDGE_MAX_REQUEST_BYTES as f64 / MIB
        ),
    )
}

/// The error a non-200 stands for.
///
/// The status is the authority on the kind, because it is the one thing a
/// proxy in front of the bridge cannot quietly reshape into something else.
fn failure(status: u16, bytes: &[u8]) -> BridgeError {
    #[derive(Deserialize, Default)]
    struct Body {
        #[serde(default)]
        message: String,
        #[serde(default)]
        kind: Option<String>,
        #[serde(default)]
        strictness: Option<String>,
    }
    let body = serde_json::from_slice::<Body>(bytes).unwrap_or_default();
    let message = if body.message.is_empty() {
        format!("the bridge answered {status}")
    } else {
        body.message
    };
    let kind = match status {
        401 | 403 => BridgeErrorKind::Unauthorized,
        404 => BridgeErrorKind::UnknownServer,
        428 => BridgeErrorKind::ConfirmationRequired {
            danger_key: body.kind.unwrap_or_default(),
            type_name_required: body.strictness.as_deref() == Some("type_name"),
        },
        // A proxy in front with a lower limit than the bridge's own.
        413 => BridgeErrorKind::TooLarge,
        502 => BridgeErrorKind::Upstream,
        _ => BridgeErrorKind::Transport,
    };
    BridgeError::new(kind, message)
}

/// Start `work` and let it run unwatched: on the page's executor in the
/// browser…
#[cfg(target_family = "wasm")]
fn start_detached(work: impl std::future::Future<Output = ()> + 'static) {
    wasm_bindgen_futures::spawn_local(work);
}
/// …and on a thread of its own in a native build, which is this crate's
/// tests and nothing else.
#[cfg(not(target_family = "wasm"))]
fn start_detached(work: impl std::future::Future<Output = ()> + Send + 'static) {
    std::thread::spawn(move || futures::executor::block_on(work));
}

/// A path segment as the URL carries it: everything but the unreserved
/// characters escaped. An account's name may hold `#`, `?` or `%`, which
/// would otherwise end the path or be read as an escape of its own.
fn segment(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(byte));
        } else {
            let _ = write!(out, "%{byte:02X}");
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::accounts::account_failure;
    use super::commands::{WATCH_WAIT_MS, exec_payload, watch_batch};
    use super::servers::{STORED_SECRET, show_stored_secrets, split_stored_secrets};
    use super::storage::TrashBody;
    use super::*;
    use gpui::http_client::FakeHttpClient;
    use zedis_connection::{KillOutcome, set_servers_cache};

    /// One request a fake bridge was sent: method, address, body.
    type Sent = (String, String, String);

    /// Every request a fake bridge was sent, and the transport that sends
    /// them to it.
    fn transport_recording(status: u16, body: &'static str) -> (HttpBridgeTransport, Arc<Mutex<Vec<Sent>>>) {
        let seen: Arc<Mutex<Vec<Sent>>> = Arc::new(Mutex::new(Vec::new()));
        let captured = seen.clone();
        let client = FakeHttpClient::create(move |mut req| {
            let captured = captured.clone();
            async move {
                let mut sent = Vec::new();
                req.body_mut().read_to_end(&mut sent).await?;
                captured.lock().expect("lock").push((
                    req.method().to_string(),
                    req.uri().to_string(),
                    String::from_utf8_lossy(&sent).into_owned(),
                ));
                Ok(gpui::http_client::http::Response::builder()
                    .status(status)
                    .body(AsyncBody::from(body.to_string()))?)
            }
        });
        (HttpBridgeTransport::new(client, "http://bridge:7379/zedis/"), seen)
    }

    #[test]
    fn the_signed_in_account_is_read_with_its_tokens_and_the_proxy() {
        let (transport, seen) = transport_recording(
            200,
            r#"{"name":"ai","role":"read_only","disabled":false,"has_password":true,"servers":["prod-*:ro"],
                "tokens":[{"name":"laptop","expires_at":1800000000,"created_at":1700000000}],
                "created_at":1690000000,"proxy":true}"#,
        );
        let own = futures::executor::block_on(transport.account()).expect("account");
        assert_eq!(own.account.name, "ai");
        assert_eq!(own.account.role, BridgeRole::ReadOnly);
        assert_eq!(own.account.servers, Some(vec!["prod-*:ro".to_string()]));
        assert_eq!(own.account.tokens[0].expires_at, Some(1_800_000_000));
        assert!(own.proxy);
        let seen = seen.lock().expect("lock");
        assert_eq!(seen[0].0, "GET");
        assert_eq!(seen[0].1, "http://bridge:7379/zedis/v1/account");
    }

    /// A name is what someone typed: `#` or `?` in it must not end the path,
    /// and a token is revoked on the account it belongs to.
    #[test]
    fn a_name_in_a_path_is_escaped() {
        let (transport, seen) = transport_recording(204, "");
        futures::executor::block_on(transport.revoke_token(Some("ops#1".to_string()), "a b?".to_string()))
            .expect("revoked");
        futures::executor::block_on(transport.revoke_token(None, "laptop".to_string())).expect("revoked");
        let seen = seen.lock().expect("lock");
        assert_eq!(seen[0].0, "DELETE");
        assert_eq!(
            seen[0].1,
            "http://bridge:7379/zedis/v1/admin/accounts/ops%231/tokens/a%20b%3F"
        );
        assert_eq!(seen[1].1, "http://bridge:7379/zedis/v1/account/tokens/laptop");
    }

    /// Absent leaves a setting alone and `null` clears the rules; the bridge
    /// reads the two differently, so the body has to keep them apart.
    #[test]
    fn an_account_change_sends_only_what_changes() {
        let account = r#"{"name":"bob","role":"full","disabled":true,"locked":true,"has_password":true,"servers":null,"tokens":[],"created_at":1}"#;
        let (transport, seen) = transport_recording(200, account);
        let change = BridgeAccountChange {
            role: None,
            servers: Some(None),
            disabled: Some(true),
            locked: Some(true),
        };
        let updated =
            futures::executor::block_on(transport.update_account("bob".to_string(), change)).expect("updated");
        assert!(updated.disabled && updated.locked);
        let seen = seen.lock().expect("lock");
        assert_eq!(seen[0].0, "PATCH");
        let sent: serde_json::Value = serde_json::from_str(&seen[0].2).expect("json");
        assert_eq!(
            sent,
            serde_json::json!({ "servers": null, "disabled": true, "locked": true })
        );
    }

    #[test]
    fn invitations_are_read_by_their_kind() {
        let (transport, _) = transport_recording(
            200,
            r#"[{"id":"i1","kind":"register","role":"full","servers":null,"proxy":true,"expires_at":9,"created_by":"alice","created_at":1},
                {"id":"i2","kind":"reset","account":"bob","expires_at":9,"created_by":"alice","created_at":1}]"#,
        );
        let invites = futures::executor::block_on(transport.invites()).expect("invites");
        assert_eq!(
            invites[0].kind,
            BridgeInviteKind::Register {
                role: BridgeRole::Full,
                servers: None,
                proxy: true,
            }
        );
        assert_eq!(
            invites[1].kind,
            BridgeInviteKind::Reset {
                account: "bob".to_string()
            }
        );
        assert_eq!(transport.invite_link("abc"), "http://bridge:7379/zedis/#invite=abc");
    }

    #[test]
    fn the_page_data_is_read_whole() {
        let seed = page_seed(
            r#"{"account":"ai","read_only":true,
                "rows":[{"table":"favorites","key":"srv:0","value":"[\"k\"]"}],
                "key_notes":[{"server":"srv","key":"user:1","tag":"red","note":"","by":"alice","at":5}],
                "scripts":[{"id":"s1","shared":true,"owner":null,"updated_by":"alice","updated_at":5,
                            "script":{"name":"top","code":"return 1"}}]}"#,
        )
        .expect("seed");
        assert_eq!(seed.account, "ai");
        assert!(seed.read_only);
        assert!(seed.rows_kept, "a bridge that does not say keeps the rows");
        assert_eq!(seed.rows[0].value, "[\"k\"]");
        assert_eq!(seed.key_notes[0].tag.as_deref(), Some("red"));
        assert_eq!(seed.key_notes[0].by, "alice");
        assert!(seed.scripts[0].shared);
        let script: serde_json::Value = serde_json::from_str(&seed.scripts[0].json).expect("script json");
        assert_eq!(script["name"], "top");
        assert!(page_seed("not json").is_err());
        let locked = page_seed(r#"{"account":"demo","rows_kept":false}"#).expect("seed");
        assert!(!locked.rows_kept && locked.rows.is_empty());
    }

    /// Changes go out one after another, in the order they were made, each
    /// to its route — a key's name in the body, never in the path.
    #[test]
    fn page_data_changes_are_sent_in_order() {
        let (transport, seen) = transport_recording(204, "");
        transport.put_row(
            "favorites".to_string(),
            "srv:0".to_string(),
            Some("[\"a\"]".to_string()),
        );
        transport.put_row("favorites".to_string(), "srv:0".to_string(), None);
        transport.put_key_note(
            "srv".to_string(),
            "user:1/x?y".to_string(),
            Some("red".to_string()),
            String::new(),
        );
        transport.put_script(BridgeScript {
            id: "s 1".to_string(),
            json: r#"{"name":"top"}"#.to_string(),
            shared: true,
        });
        transport.delete_script("s 1".to_string());
        for _ in 0..200 {
            if seen.lock().expect("lock").len() == 5 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let seen = seen.lock().expect("lock");
        let asked: Vec<(&str, &str)> = seen.iter().map(|(m, u, _)| (m.as_str(), u.as_str())).collect();
        assert_eq!(
            asked,
            vec![
                ("PUT", "http://bridge:7379/zedis/v1/page-data/rows"),
                ("PUT", "http://bridge:7379/zedis/v1/page-data/rows"),
                ("PUT", "http://bridge:7379/zedis/v1/servers/srv/key-notes"),
                ("PUT", "http://bridge:7379/zedis/v1/scripts/s%201"),
                ("DELETE", "http://bridge:7379/zedis/v1/scripts/s%201"),
            ]
        );
        let row: serde_json::Value = serde_json::from_str(&seen[1].2).expect("json");
        assert_eq!(row["value"], serde_json::Value::Null, "null forgets the row");
        let note: serde_json::Value = serde_json::from_str(&seen[2].2).expect("json");
        assert_eq!(note["key"], "user:1/x?y");
        let script: serde_json::Value = serde_json::from_str(&seen[3].2).expect("json");
        assert_eq!(script["script"]["name"], "top");
        assert_eq!(script["shared"], true);
    }

    /// Deleting an account says what becomes of what it kept to itself, in
    /// the shape the bridge reads.
    #[test]
    fn a_deletion_says_where_the_account_s_own_go() {
        let (transport, seen) = transport_recording(204, "");
        futures::executor::block_on(
            transport.delete_account("bob".to_string(), BridgeDisposition::HandTo("alice".to_string())),
        )
        .expect("deleted");
        futures::executor::block_on(transport.settle_orphans("ghost".to_string(), BridgeDisposition::Share))
            .expect("settled");
        let seen = seen.lock().expect("lock");
        assert_eq!(seen[0].0, "DELETE");
        let body: serde_json::Value = serde_json::from_str(&seen[0].2).expect("json");
        assert_eq!(
            body,
            serde_json::json!({ "private": { "action": "hand_to", "account": "alice" } })
        );
        assert_eq!(
            (seen[1].0.as_str(), seen[1].1.as_str()),
            ("POST", "http://bridge:7379/zedis/v1/admin/orphans/ghost")
        );
        let body: serde_json::Value = serde_json::from_str(&seen[1].2).expect("json");
        assert_eq!(body, serde_json::json!({ "private": { "action": "share" } }));
    }

    /// What a deleted account kept may be this page's now, so the server
    /// list is read again as soon as the bridge says it was settled — and
    /// not when it refused.
    #[test]
    fn a_settled_handover_reads_the_server_list_again() {
        let (transport, _) = transport_recording(204, "");
        let mut requests = transport.take_server_list_requests().expect("the listener, once");
        assert!(transport.take_server_list_requests().is_none());
        futures::executor::block_on(transport.delete_account("bob".to_string(), BridgeDisposition::Share))
            .expect("deleted");
        assert_eq!(requests.try_recv().ok(), Some(()));
        futures::executor::block_on(transport.settle_orphans("ghost".to_string(), BridgeDisposition::Delete))
            .expect("settled");
        assert_eq!(requests.try_recv().ok(), Some(()));

        let (transport, _) = transport_recording(409, r#"{"error":"the last administrator"}"#);
        let mut requests = transport.take_server_list_requests().expect("the listener, once");
        let refused =
            futures::executor::block_on(transport.delete_account("alice".to_string(), BridgeDisposition::Share));
        assert!(refused.is_err());
        assert!(requests.try_recv().is_err(), "nothing changed, nothing to read");
    }

    /// The audit is asked for with each narrowing encoded, and its lines
    /// come back as the objects the bridge kept.
    #[test]
    fn the_audit_is_asked_for_narrowed_and_read_as_lines() {
        let (transport, seen) = transport_recording(
            200,
            r#"{"lines":[{"event":"logout","account":"a b"}],"next":"41","stopped_early":false}"#,
        );
        let page = futures::executor::block_on(transport.audit(BridgeAuditQuery {
            account: Some("a b".to_string()),
            event: Some("logout".to_string()),
            server: Some("srv 1".to_string()),
            since_ms: Some(1_000),
            before: Some("42".to_string()),
        }))
        .expect("page");
        assert_eq!(page.next.as_deref(), Some("41"));
        let line: serde_json::Value = serde_json::from_str(&page.lines[0]).expect("json");
        assert_eq!(line["account"], "a b");
        let seen = seen.lock().expect("lock");
        assert_eq!(
            (seen[0].0.as_str(), seen[0].1.as_str()),
            (
                "GET",
                "http://bridge:7379/zedis/v1/admin/audit?account=a%20b&event=logout&server=srv%201&since_ms=1000&before=42"
            )
        );
    }

    /// Who is online is read with the servers each client is on, by name.
    #[test]
    fn who_is_online_is_read_with_the_servers_each_client_is_on() {
        let (transport, seen) = transport_recording(
            200,
            r#"{"process":"4c1f","online":1,"window_secs":300,"clients":[{"account":"bob","kind":"token",
               "token":"laptop","peer":"10.0.0.9","last_seen":"x","seen_at":1700000000,"idle_secs":3,
               "servers":[{"id":"s1","name":"prod"},{"id":"s2","name":"staging"}]}]}"#,
        );
        let online = futures::executor::block_on(transport.clients()).expect("clients");
        assert_eq!(seen.lock().expect("lock")[0].1, "http://bridge:7379/zedis/v1/clients");
        assert_eq!((online.process.as_str(), online.window_secs), ("4c1f", 300));
        let bob = &online.clients[0];
        assert_eq!(bob.token.as_deref(), Some("laptop"));
        assert_eq!(bob.seen_at, 1_700_000_000);
        assert_eq!(bob.servers, ["prod", "staging"]);
    }

    #[test]
    fn the_requests_are_read_and_answered() {
        let (transport, _) = transport_recording(
            200,
            r#"[{"id":"r1","name":"dana","note":"on call","peer":"10.0.0.9","requested_at":5,"expires_at":9,"name_taken":true}]"#,
        );
        let waiting = futures::executor::block_on(transport.signups()).expect("requests");
        assert_eq!(
            waiting,
            vec![BridgeSignup {
                id: "r1".to_string(),
                name: "dana".to_string(),
                note: "on call".to_string(),
                peer: Some("10.0.0.9".to_string()),
                requested_at: 5,
                expires_at: 9,
                name_taken: true,
            }]
        );

        let (transport, seen) = transport_recording(200, r#"{"name":"dana","role":"full"}"#);
        let made = futures::executor::block_on(transport.approve_signup(
            "r 1".to_string(),
            BridgeRole::Full,
            Some(vec!["staging".to_string()]),
        ))
        .expect("approved");
        assert_eq!((made.name.as_str(), made.role), ("dana", BridgeRole::Full));
        let seen = seen.lock().expect("lock");
        assert_eq!(
            (seen[0].0.as_str(), seen[0].1.as_str()),
            ("POST", "http://bridge:7379/zedis/v1/admin/signups/r%201/approve")
        );
        let body: serde_json::Value = serde_json::from_str(&seen[0].2).expect("json");
        assert_eq!(body, serde_json::json!({ "role": "full", "servers": ["staging"] }));
    }

    #[test]
    fn what_an_account_owns_is_read_by_kind() {
        let (transport, _) = transport_recording(
            200,
            r#"{"servers":[{"id":"s1","name":"prod"}],"scripts":[{"id":"x","name":"top"}]}"#,
        );
        let owned = futures::executor::block_on(transport.owned("bob".to_string())).expect("owned");
        assert_eq!(owned.servers[0].name, "prod");
        assert_eq!(owned.scripts[0].id, "x");
        assert!(!owned.is_empty());
    }

    /// A refresh asks with the tag it last had: the same answer is a 304
    /// and nothing to apply, a new one comes with its tag.
    #[test]
    fn a_refresh_is_nothing_when_the_bridge_says_unchanged() {
        let asked: Arc<Mutex<Vec<Option<String>>>> = Arc::default();
        let captured = asked.clone();
        let client = FakeHttpClient::create(move |req| {
            let captured = captured.clone();
            async move {
                let tag = req
                    .headers()
                    .get("if-none-match")
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_string);
                captured.lock().expect("lock").push(tag.clone());
                let response = gpui::http_client::http::Response::builder().header("etag", "\"v2\"");
                Ok(match tag.as_deref() {
                    Some("\"v2\"") => response.status(304).body(AsyncBody::empty())?,
                    _ => response
                        .status(200)
                        .body(AsyncBody::from(r#"{"account":"ai"}"#.to_string()))?,
                })
            }
        });
        let transport = HttpBridgeTransport::new(client, "http://bridge:7379");
        let fresh = futures::executor::block_on(transport.fetch_page_data(None)).expect("answer");
        let fresh = fresh.expect("a new answer");
        assert_eq!(fresh.json, r#"{"account":"ai"}"#);
        assert_eq!(fresh.etag.as_deref(), Some("\"v2\""));
        let same = futures::executor::block_on(transport.fetch_page_data(fresh.etag)).expect("answer");
        assert!(same.is_none(), "unchanged: nothing to apply");
        assert_eq!(asked.lock().expect("lock").len(), 2);
    }

    /// A change on its way to the bridge is pending until it is answered —
    /// what keeps a refresh from putting back what the change replaces.
    #[test]
    fn a_change_is_pending_until_the_bridge_answers() {
        let (transport, seen) = transport_recording(204, "");
        assert!(!transport.writes_pending());
        transport.put_row("favorites".to_string(), "srv:0".to_string(), Some("[]".to_string()));
        for _ in 0..200 {
            if !transport.writes_pending() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(!transport.writes_pending(), "answered");
        assert_eq!(seen.lock().expect("lock").len(), 1);
        transport.refresh_soon();
        let mut requests = transport.take_refresh_requests().expect("the listener, once");
        assert!(transport.take_refresh_requests().is_none());
        assert_eq!(requests.try_recv().ok(), Some(()));
    }

    #[test]
    fn a_refused_account_request_says_why_by_its_status() {
        for (status, kind) in [
            (400, BridgeAccountErrorKind::Invalid),
            (403, BridgeAccountErrorKind::Refused),
            (404, BridgeAccountErrorKind::NotFound),
            (409, BridgeAccountErrorKind::Conflict),
            (429, BridgeAccountErrorKind::TooManyAttempts),
            (502, BridgeAccountErrorKind::Failed),
        ] {
            let error = account_failure(
                status,
                br#"{"message":"the bridge would be left without an administrator"}"#,
            );
            assert_eq!(error.kind, kind);
            assert_eq!(error.message, "the bridge would be left without an administrator");
        }
    }

    #[test]
    fn a_stored_secret_is_shown_as_a_placeholder_and_sent_back_as_a_name() {
        let listed = RedisServer {
            id: "srv".to_string(),
            host: "10.0.0.5".to_string(),
            ..Default::default()
        };
        let shown = show_stored_secrets(listed, &["password".to_string(), "ssh_key".to_string()]);
        assert_eq!(shown.password.as_deref(), Some(STORED_SECRET));
        assert_eq!(shown.ssh_key.as_deref(), Some(STORED_SECRET));
        assert_eq!(
            shown.sentinel_password, None,
            "not set on the bridge, so nothing to show"
        );

        // Untouched: both go back as names, and no placeholder crosses the wire.
        let (sent, keep) = split_stored_secrets(shown.clone());
        assert_eq!(keep, vec!["password", "ssh_key"]);
        assert_eq!((sent.password, sent.ssh_key), (None, None));
        assert_eq!(sent.host, "10.0.0.5");
    }

    #[test]
    fn typing_over_a_placeholder_replaces_the_secret_and_emptying_it_clears_it() {
        let shown = show_stored_secrets(
            RedisServer::default(),
            &["password".to_string(), "ssh_password".to_string()],
        );
        let edited = RedisServer {
            password: Some("a-new-password".to_string()),
            ssh_password: None,
            ..shown
        };
        let (sent, keep) = split_stored_secrets(edited);
        assert!(keep.is_empty(), "neither is kept: one is replaced, one is cleared");
        assert_eq!(sent.password.as_deref(), Some("a-new-password"));
        assert_eq!(sent.ssh_password, None);
    }

    #[test]
    fn a_name_the_bridge_sends_that_is_not_a_secret_is_ignored() {
        let shown = show_stored_secrets(RedisServer::default(), &["host".to_string()]);
        assert_eq!(shown, RedisServer::default());
    }
    use std::sync::Mutex;

    use zedis_connection::PipelineSpec;

    fn request() -> BridgeRequest {
        BridgeRequest {
            server_id: "srv".to_string(),
            db: 3,
            session: None,
            commands: vec![b"*1\r\n$4\r\nPING\r\n".to_vec()],
            pipeline: None,
            fanout_masters: false,
            fanout_nodes: Vec::new(),
            confirm: None,
        }
    }

    #[test]
    fn a_plain_command_sends_only_what_the_bridge_needs() {
        let body = exec_payload(&request());
        assert_eq!(body["server"], "srv");
        assert_eq!(body["db"], 3);
        assert_eq!(body["commands"].as_array().expect("commands").len(), 1);
        assert_eq!(body["desc"], serde_json::json!(["PING"]));
        // Absent, not null: the server reads these with `#[serde(default)]`.
        assert!(body.get("pipeline").is_none());
        assert!(body.get("fanout").is_none());
        assert!(body.get("session").is_none());
        assert!(body.get("confirm").is_none());
    }

    #[test]
    fn a_pipeline_sends_its_framing() {
        let mut req = request();
        req.pipeline = Some(PipelineSpec {
            offset: 3,
            count: 1,
            atomic: true,
        });
        let body = exec_payload(&req);
        assert_eq!(body["pipeline"]["offset"], 3);
        assert_eq!(body["pipeline"]["count"], 1);
        assert_eq!(body["pipeline"]["atomic"], true);
    }

    #[test]
    fn a_session_and_a_confirmation_ride_along_when_set() {
        let mut req = request();
        req.session = Some("tok".to_string());
        req.confirm = Some("yes".to_string());
        let body = exec_payload(&req);
        assert_eq!(body["session"], "tok");
        assert_eq!(body["confirm"], "yes");
    }

    /// Drive the real `send()` against a scripted client: this is the only
    /// place the request building, the status handling and the body parsing
    /// are exercised together, and it needs no network to do it.
    fn transport_answering(status: u16, body: &'static str) -> (HttpBridgeTransport, Arc<Mutex<Option<String>>>) {
        let seen: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let captured = seen.clone();
        let client = FakeHttpClient::create(move |mut req| {
            let captured = captured.clone();
            async move {
                let mut sent = Vec::new();
                req.body_mut().read_to_end(&mut sent).await?;
                *captured.lock().expect("lock") = Some(String::from_utf8_lossy(&sent).into_owned());
                Ok(gpui::http_client::http::Response::builder()
                    .status(status)
                    .body(AsyncBody::from(body.to_string()))?)
            }
        });
        (HttpBridgeTransport::new(client, "http://bridge:7379/"), seen)
    }

    /// Signing out is the bridge's `POST /v1/logout`: the login is forgotten
    /// on the server, not just dropped by the page. Any other answer leaves
    /// it standing, and is an error rather than a reload that would come
    /// back signed in.
    #[test]
    fn signing_out_posts_to_the_logout_route_and_fails_when_the_bridge_does() {
        let asked = Arc::new(Mutex::new(None));
        let captured = asked.clone();
        let client = FakeHttpClient::create(move |req| {
            let captured = captured.clone();
            async move {
                *captured.lock().expect("lock") = Some((req.method().to_string(), req.uri().to_string()));
                Ok(gpui::http_client::http::Response::builder()
                    .status(204)
                    .body(AsyncBody::empty())?)
            }
        });
        let transport = HttpBridgeTransport::new(client, "http://bridge:7379/zedis/");
        futures::executor::block_on(transport.sign_out()).expect("signed out");
        assert_eq!(
            asked.lock().expect("lock").clone(),
            Some(("POST".to_string(), "http://bridge:7379/zedis/v1/logout".to_string())),
            "under the bridge's base path, like every other route"
        );

        let (transport, _) = transport_answering(502, r#"{"error":"bad gateway"}"#);
        let err = futures::executor::block_on(transport.sign_out()).expect_err("the login is still there");
        assert_eq!(err.kind, BridgeErrorKind::Upstream);
    }

    #[test]
    fn a_successful_round_trip_returns_the_decoded_frames() {
        // base64 of `+PONG\r\n`
        let (transport, seen) = transport_answering(200, r#"{"replies":["K1BPTkcNCg=="]}"#);
        let reply = futures::executor::block_on(transport.send(request())).expect("frames");
        assert_eq!(reply.frames, vec![b"+PONG\r\n".to_vec()]);

        let sent: serde_json::Value =
            serde_json::from_str(seen.lock().expect("lock").as_deref().expect("a body was sent")).expect("json");
        assert_eq!(sent["server"], "srv");
        assert_eq!(sent["db"], 3);
    }

    #[test]
    fn a_refused_call_becomes_a_typed_error_not_a_parse_failure() {
        let (transport, _) = transport_answering(
            428,
            r#"{"message":"needs it","kind":"danger.flushall","strictness":"type_name"}"#,
        );
        let err = futures::executor::block_on(transport.send(request())).expect_err("must fail");
        assert_eq!(
            err.kind,
            BridgeErrorKind::ConfirmationRequired {
                danger_key: "danger.flushall".to_string(),
                type_name_required: true,
            }
        );
    }

    #[test]
    fn a_reply_that_is_not_base64_is_reported_as_transport_trouble() {
        let (transport, _) = transport_answering(200, r#"{"replies":["!!!not base64!!!"]}"#);
        let err = futures::executor::block_on(transport.send(request())).expect_err("must fail");
        assert_eq!(err.kind, BridgeErrorKind::Transport);
    }

    #[test]
    fn a_fan_out_is_asked_for_by_name_and_brings_labels_back() {
        let mut req = request();
        req.fanout_masters = true;
        let body = exec_payload(&req);
        assert_eq!(body["fanout"], "masters");
        // Absent unless the caller aims at specific nodes.
        assert!(body.get("fanout_nodes").is_none());

        let mut aimed = request();
        aimed.fanout_masters = true;
        aimed.fanout_nodes = vec!["10.0.0.1:6379".to_string()];
        assert_eq!(exec_payload(&aimed)["fanout_nodes"][0], "10.0.0.1:6379");

        let (transport, _) = transport_answering(200, r#"{"replies":["K09LDQo="],"nodes":["10.0.0.1:6379"]}"#);
        let reply = futures::executor::block_on(transport.send(req)).expect("reply");
        assert_eq!(reply.nodes, vec!["10.0.0.1:6379"]);
    }

    #[test]
    fn the_status_decides_the_kind() {
        assert_eq!(failure(401, b"{}").kind, BridgeErrorKind::Unauthorized);
        assert_eq!(failure(404, b"{}").kind, BridgeErrorKind::UnknownServer);
        assert_eq!(failure(502, b"{}").kind, BridgeErrorKind::Upstream);
        assert_eq!(failure(500, b"{}").kind, BridgeErrorKind::Transport);
        assert_eq!(failure(413, b"").kind, BridgeErrorKind::TooLarge);
    }

    /// A diagnosis is the bridge's to run: the page posts to the entry's
    /// route and reads every stage back.
    #[test]
    fn a_diagnosis_is_asked_of_the_entry_and_read_back_stage_by_stage() {
        let asked = Arc::new(Mutex::new(None));
        let captured = asked.clone();
        let client = FakeHttpClient::create(move |req| {
            let captured = captured.clone();
            async move {
                *captured.lock().expect("lock") = Some((req.method().to_string(), req.uri().to_string()));
                let body = r#"{"stages":[{"stage":"dns","status":"skipped","elapsed_ms":0},
                    {"stage":"tcp","status":"failed","error":"refused","hint":"tcp_refused","elapsed_ms":3}]}"#;
                Ok(gpui::http_client::http::Response::builder()
                    .status(200)
                    .body(AsyncBody::from(body.to_string()))?)
            }
        });
        let transport = HttpBridgeTransport::new(client, "http://bridge:7379/");
        let rows = futures::executor::block_on(transport.diagnose("srv 1".to_string())).expect("rows");
        assert_eq!(
            asked.lock().expect("lock").clone(),
            Some((
                "POST".to_string(),
                "http://bridge:7379/v1/servers/srv%201/diagnose".to_string()
            ))
        );
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1].stage, zedis_connection::DiagStage::Tcp);
        assert_eq!(rows[1].outcome().error.as_deref(), Some("refused"));
    }

    #[test]
    fn kill_running_posts_target_and_parses_node_outcomes() {
        let (transport, seen) = transport_recording(
            200,
            r#"{"replies":[{"node":"127.0.0.1:6379","outcome":{"status":"killed"}}]}"#,
        );
        let replies =
            futures::executor::block_on(transport.kill_running("srv 1".to_string(), KillTarget::Function)).expect("ok");
        assert_eq!(
            replies,
            vec![KillReply {
                node: "127.0.0.1:6379".to_string(),
                outcome: KillOutcome::Killed,
            }]
        );
        let seen = seen.lock().expect("lock");
        assert_eq!(seen[0].0, "POST");
        assert_eq!(seen[0].1, "http://bridge:7379/zedis/v1/servers/srv%201/kill-script");
        assert_eq!(seen[0].2, r#"{"target":"function"}"#);
    }

    /// A value the bridge would refuse is not sent: the page says how large
    /// it was instead of shipping megabytes to be told `413` — and says it
    /// as what it is, not as trouble with the link.
    #[test]
    fn a_request_over_the_limit_is_refused_before_it_is_sent() {
        let (transport, seen) = transport_answering(200, r#"{"replies":["K09LDQo="]}"#);
        let mut big = request();
        big.commands = vec![vec![b'x'; zedis_connection::BRIDGE_MAX_VALUE_BYTES + 128 * 1024]];
        let err = futures::executor::block_on(transport.send(big)).expect_err("too large");
        assert_eq!(err.kind, BridgeErrorKind::TooLarge);
        assert!(err.message.contains("2 MB"), "{}", err.message);
        assert!(seen.lock().expect("lock").is_none(), "nothing went out");

        let mut fits = request();
        fits.commands = vec![vec![b'x'; zedis_connection::BRIDGE_MAX_VALUE_BYTES]];
        futures::executor::block_on(transport.send(fits)).expect("a value at the limit goes out");
    }

    #[test]
    fn a_refusal_carries_what_the_confirm_dialog_needs() {
        let body =
            br#"{"error":"confirmation_required","message":"needs it","kind":"danger.flushall","strictness":"click"}"#;
        let err = failure(428, body);
        assert_eq!(
            err.kind,
            BridgeErrorKind::ConfirmationRequired {
                danger_key: "danger.flushall".to_string(),
                type_name_required: false,
            }
        );
        assert_eq!(err.message, "needs it");

        let strict = br#"{"message":"m","kind":"danger.flushdb","strictness":"type_name"}"#;
        assert_eq!(
            failure(428, strict).kind,
            BridgeErrorKind::ConfirmationRequired {
                danger_key: "danger.flushdb".to_string(),
                type_name_required: true,
            }
        );
    }

    #[test]
    fn an_unreadable_body_still_produces_a_usable_error() {
        let err = failure(503, b"<html>gateway</html>");
        assert_eq!(err.kind, BridgeErrorKind::Transport);
        assert!(err.message.contains("503"), "got {}", err.message);
    }

    #[test]
    fn the_base_url_loses_its_trailing_slashes() {
        // Built without a client so the URL join is checked on its own; the
        // transport never has to guess whether to add a separator.
        let joined = "http://host:7379///".trim_end_matches('/').to_string();
        assert_eq!(format!("{joined}/v1/exec"), "http://host:7379/v1/exec");
    }

    /// A poll's reply: a message's payload comes back as the bytes that
    /// were published, a line as the node wrote it, and the end is said.
    #[test]
    fn a_poll_reply_is_read_back_into_messages_and_lines() {
        let batch = watch_batch(
            br#"{"items":[{"channel":"news","payload":"aGVsbG8g5L2g5aW9"},{"node":"10.0.0.5:6379","line":"1.0 [0 x] \"GET\" \"k\""}],"dropped":3,"closed":"time limit"}"#,
        )
        .expect("batch");
        assert_eq!(
            batch.items,
            [
                WatchItem::Message {
                    channel: "news".to_string(),
                    payload: "hello 你好".as_bytes().to_vec(),
                },
                WatchItem::Line {
                    node: "10.0.0.5:6379".to_string(),
                    line: r#"1.0 [0 x] "GET" "k""#.to_string(),
                },
            ]
        );
        assert_eq!((batch.dropped, batch.closed.as_deref()), (3, Some("time limit")));
        assert_eq!(
            watch_batch(br#"{"items":[],"dropped":0}"#).expect("empty"),
            WatchBatch::default()
        );
        assert!(
            watch_batch(br#"{"items":[{"channel":"c","payload":"!!"}]}"#).is_err(),
            "not base64"
        );
    }

    /// A bin entry: listed without its payload, fetched with it.
    #[test]
    fn a_bin_entry_is_read_with_or_without_its_payload() {
        let listed: TrashBody = serde_json::from_str(
            r#"{"id":"a1","key":"k","db":2,"pttl_ms":-1,"deleted_at_ms":7,"deleted_by":"alice","size":3}"#,
        )
        .expect("listed");
        let entry = listed.into_entry().expect("entry");
        assert_eq!(
            (entry.id.as_str(), entry.db, entry.deleted_by.as_str()),
            ("a1", 2, "alice")
        );
        assert!(entry.payload.is_empty());
        let fetched: TrashBody = serde_json::from_str(r#"{"id":"a1","key":"k","payload":"AAEC"}"#).expect("fetched");
        assert_eq!(fetched.into_entry().expect("entry").payload, [0, 1, 2]);
    }

    /// The bridge's address for each request a fake bridge was sent, and
    /// its body read as JSON (`Null` for none).
    fn routes(seen: &Arc<Mutex<Vec<Sent>>>) -> Vec<(String, String, serde_json::Value)> {
        seen.lock()
            .expect("lock")
            .iter()
            .map(|(method, url, body)| {
                let path = url.trim_start_matches("http://bridge:7379/zedis").to_string();
                let body = serde_json::from_str(body).unwrap_or(serde_json::Value::Null);
                (method.clone(), path, body)
            })
            .collect()
    }

    /// Saving the list sends each entry its own way: one the bridge does not
    /// have is posted whole, one that differs from the copy last answered is
    /// put with its untouched secrets named and not sent, one the list no
    /// longer names is deleted, and one that did not change is not sent at
    /// all — and then the list is read again.
    #[test]
    fn the_list_is_saved_entry_by_entry() {
        let entry = |id: &str, port: u16| RedisServer {
            id: id.to_string(),
            name: id.to_string(),
            host: "redis.internal".to_string(),
            port,
            ..Default::default()
        };
        set_servers_cache(vec![entry("same", 1), entry("edited", 2), entry("gone", 3)]);
        let (transport, seen) = transport_recording(200, "[]");
        let mut edited = entry("edited", 20);
        edited.password = Some(STORED_SECRET.to_string());
        let saved = futures::executor::block_on(transport.save(vec![entry("same", 1), edited, entry("new", 4)]));
        assert!(saved.expect("saved").is_empty(), "the list as the bridge read it back");

        let routes = routes(&seen);
        let paths: Vec<(&str, &str)> = routes.iter().map(|(m, p, _)| (m.as_str(), p.as_str())).collect();
        assert_eq!(
            paths,
            [
                ("PUT", "/v1/servers/edited"),
                ("POST", "/v1/servers"),
                ("DELETE", "/v1/servers/gone"),
                ("GET", "/v1/servers"),
            ]
        );
        let put = &routes[0].2;
        assert_eq!(put["server"]["port"], 20);
        assert_eq!(put["keep_secrets"], serde_json::json!(["password"]));
        assert!(put["server"]["password"].is_null(), "a kept secret is named, not sent");
        assert_eq!(routes[1].2["server"]["id"], "new");
    }

    /// The recycle bin and the Metrics history are asked for under their
    /// server: a deleted key's payload goes as base64, a sample as the JSON
    /// it is, and the history comes back one sample at a time.
    #[test]
    fn the_bin_and_the_metrics_are_kept_under_their_server() {
        let (transport, seen) = transport_recording(200, r#"{"samples":[{"used_memory":1}]}"#);
        let entry = BridgeTrashEntry {
            key: "k".to_string(),
            db: 2,
            pttl_ms: -1,
            payload: vec![0, 1, 2],
            ..BridgeTrashEntry::default()
        };
        futures::executor::block_on(async {
            transport.trash_put("s1".into(), entry).await.expect("put");
            let _ = transport.trash_list("s1".into()).await;
            let _ = transport.trash_get("s1".into(), "a1".into()).await;
            transport.trash_remove("s1".into(), "a1".into()).await.expect("remove");
            transport
                .metrics_put("s1".into(), 7, br#"{"x":1}"#.to_vec())
                .await
                .expect("sample");
            let history = transport.metrics_since("s1".into(), 5, 100).await.expect("history");
            assert_eq!(history, [br#"{"used_memory":1}"#.to_vec()]);
        });

        let routes = routes(&seen);
        let paths: Vec<(&str, &str)> = routes.iter().map(|(m, p, _)| (m.as_str(), p.as_str())).collect();
        assert_eq!(
            paths,
            [
                ("POST", "/v1/servers/s1/trash"),
                ("GET", "/v1/servers/s1/trash"),
                ("GET", "/v1/servers/s1/trash/a1"),
                ("DELETE", "/v1/servers/s1/trash/a1"),
                ("POST", "/v1/servers/s1/metrics"),
                ("GET", "/v1/servers/s1/metrics?from_ms=5&max_points=100"),
            ]
        );
        assert_eq!(
            routes[0].2,
            serde_json::json!({ "key": "k", "db": 2, "pttl_ms": -1, "payload": "AAEC" })
        );
        assert_eq!(routes[4].2, serde_json::json!({ "ts_ms": 7, "sample": { "x": 1 } }));
    }

    /// A session, the write lock's window and a watch each have their
    /// route; a watch's confirmation rides along, and a released watch is
    /// deleted without anyone waiting for the answer.
    #[test]
    fn a_session_the_write_window_and_a_watch_are_asked_for_by_their_routes() {
        let (transport, seen) = transport_recording(200, r#"{"session":"x","token":"t","items":[]}"#);
        futures::executor::block_on(async {
            assert_eq!(transport.open_session("s1".into(), 3).await.expect("session"), "x");
            transport.close_session("x".into()).await.expect("closed");
            transport
                .unlock_writes("s1".into(), "prod".into())
                .await
                .expect("unlocked");
            transport.lock_writes("s1".into()).await.expect("locked");
            let opened = transport
                .open_watch(
                    "s1".into(),
                    0,
                    WatchKind::Monitor,
                    vec!["news".into()],
                    Some("prod".into()),
                )
                .await
                .expect("watch");
            assert_eq!(opened.token, "t");
            transport.poll_watch("t".into()).await.expect("poll");
        });
        transport.release_watch("t".into());
        for _ in 0..200 {
            if seen.lock().expect("lock").len() == 7 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        let routes = routes(&seen);
        let paths: Vec<(&str, &str)> = routes.iter().map(|(m, p, _)| (m.as_str(), p.as_str())).collect();
        assert_eq!(
            paths,
            [
                ("POST", "/v1/session"),
                ("DELETE", "/v1/session/x"),
                ("POST", "/v1/servers/s1/unlock"),
                ("DELETE", "/v1/servers/s1/unlock"),
                ("POST", "/v1/watch"),
                ("POST", "/v1/watch/t/poll"),
                ("DELETE", "/v1/watch/t"),
            ]
        );
        assert_eq!(routes[0].2, serde_json::json!({ "server": "s1", "db": 3 }));
        assert_eq!(routes[2].2, serde_json::json!({ "confirm": "prod" }));
        assert_eq!(
            routes[4].2,
            serde_json::json!({
                "server": "s1", "db": 0, "kind": "monitor", "channels": ["news"], "confirm": "prod"
            })
        );
        assert_eq!(routes[5].2, serde_json::json!({ "wait_ms": WATCH_WAIT_MS }));
    }
}
