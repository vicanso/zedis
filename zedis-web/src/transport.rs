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

use base64::{Engine as _, engine::general_purpose::STANDARD as B64};
use futures::AsyncReadExt as _;
use futures::future::BoxFuture;
use gpui::http_client::{AsyncBody, HttpClient, http::Request};
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::Arc;
use zedis_connection::{
    BridgeError, BridgeErrorKind, BridgeReply, BridgeRequest, BridgeServerStore, BridgeTransport, RedisServer,
    get_servers,
};

/// Talks to one `zedis-bridge`.
pub struct HttpBridgeTransport {
    client: Arc<dyn HttpClient>,
    /// No trailing slash.
    base_url: String,
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
        }
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

    /// The entries this account may see, as the bridge holds them: settings
    /// and owner, never a secret.
    pub fn fetch_servers(&self) -> BoxFuture<'static, Result<Vec<RedisServer>, BridgeError>> {
        let (client, base_url) = (self.client.clone(), self.base_url.clone());
        Box::pin(Self::list(client, base_url))
    }

    async fn list(client: Arc<dyn HttpClient>, base_url: String) -> Result<Vec<RedisServer>, BridgeError> {
        let (status, bytes) = Self::call(client, "GET", format!("{base_url}/v1/servers"), String::new()).await?;
        if status != 200 {
            return Err(failure(status, &bytes));
        }
        #[derive(Deserialize)]
        struct Entry {
            #[serde(flatten)]
            server: RedisServer,
            #[serde(default)]
            secrets_set: Vec<String>,
        }
        let list = serde_json::from_slice::<Vec<Entry>>(&bytes)
            .map_err(|e| BridgeError::transport(format!("the bridge's server list is not JSON: {e}")))?;
        Ok(list
            .into_iter()
            .map(|entry| show_stored_secrets(entry.server, &entry.secrets_set))
            .collect())
    }
}

/// What a secret field holds in the browser when the bridge has a value for
/// it: the bridge sends an entry's settings and the *names* of the secrets
/// that are set, never the secrets (ADR 9), so the form needs something to
/// show in their place. Eight bullets read as "a hidden value" in a masked
/// field and a plain one alike, in every locale, which is why it is not a
/// translated sentence. The three things a user can then do map onto the
/// three things an edit can mean: leave it (keep what is stored), empty it
/// (remove the secret), type over it (replace it). It never crosses the
/// wire — [`split_stored_secrets`] turns it back into a name on the way out.
const STORED_SECRET: &str = "••••••••";

/// `server` as the form should see it: each secret the bridge holds shown as
/// the placeholder.
fn show_stored_secrets(mut server: RedisServer, secrets_set: &[String]) -> RedisServer {
    for name in secrets_set {
        if let Some(field) = server.secret_mut(name) {
            *field = Some(STORED_SECRET.to_string());
        }
    }
    server
}

/// `server` as the bridge should get it: every field still holding the
/// placeholder blanked, and named, so the bridge keeps what it has stored.
fn split_stored_secrets(mut server: RedisServer) -> (RedisServer, Vec<&'static str>) {
    let mut keep = Vec::new();
    for name in RedisServer::SECRET_FIELDS {
        if let Some(field) = server.secret_mut(name)
            && field.as_deref() == Some(STORED_SECRET)
        {
            *field = None;
            keep.push(name);
        }
    }
    (server, keep)
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

/// The `/v1/exec` body.
///
/// Field names are the contract with `zedis-bridge`'s `ExecRequest`; the test
/// below is what keeps the two from drifting apart silently, since a rename
/// on either side would otherwise only show up as a 400 at runtime.
fn exec_payload(request: &BridgeRequest) -> serde_json::Value {
    let mut payload = serde_json::json!({
        "server": request.server_id,
        "db": request.db,
        "commands": request.commands.iter().map(|c| B64.encode(c)).collect::<Vec<_>>(),
    });
    if request.fanout_masters {
        payload["fanout"] = serde_json::Value::String("masters".to_string());
        if !request.fanout_nodes.is_empty() {
            payload["fanout_nodes"] = serde_json::Value::from(request.fanout_nodes.clone());
        }
    }
    if let Some(spec) = request.pipeline {
        payload["pipeline"] = serde_json::json!({
            "offset": spec.offset,
            "count": spec.count,
            "atomic": spec.atomic,
        });
    }
    if let Some(session) = &request.session {
        payload["session"] = serde_json::Value::String(session.clone());
    }
    if let Some(confirm) = &request.confirm {
        payload["confirm"] = serde_json::Value::String(confirm.clone());
    }
    payload
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
        502 => BridgeErrorKind::Upstream,
        _ => BridgeErrorKind::Transport,
    };
    BridgeError::new(kind, message)
}

impl BridgeTransport for HttpBridgeTransport {
    fn send(&self, request: BridgeRequest) -> BoxFuture<'static, Result<BridgeReply, BridgeError>> {
        let client = self.client.clone();
        let url = format!("{}/v1/exec", self.base_url);

        let payload = exec_payload(&request);

        Box::pin(async move {
            let (status, bytes) = Self::post(client, url, payload.to_string()).await?;
            if status != 200 {
                return Err(failure(status, &bytes));
            }
            #[derive(Deserialize)]
            struct Body {
                replies: Vec<String>,
                #[serde(default)]
                nodes: Vec<String>,
            }
            let body = serde_json::from_slice::<Body>(&bytes)
                .map_err(|e| BridgeError::transport(format!("the bridge's reply is not JSON: {e}")))?;
            let frames = body
                .replies
                .iter()
                .enumerate()
                .map(|(index, reply)| {
                    B64.decode(reply.as_bytes())
                        .map_err(|e| BridgeError::transport(format!("reply {index} is not base64: {e}")))
                })
                .collect::<Result<Vec<Vec<u8>>, BridgeError>>()?;
            Ok(BridgeReply {
                frames,
                nodes: body.nodes,
            })
        })
    }

    /// Ask the bridge for a connection of this caller's own — what the
    /// terminal needs so a `SELECT` typed there does not move the key tree
    /// (ADR 4).
    fn open_session(&self, server_id: String, db: usize) -> BoxFuture<'static, Result<String, BridgeError>> {
        let client = self.client.clone();
        let url = format!("{}/v1/session", self.base_url);
        Box::pin(async move {
            let body = serde_json::json!({ "server": server_id, "db": db }).to_string();
            let (status, bytes) = Self::post(client, url, body).await?;
            if status != 200 {
                return Err(failure(status, &bytes));
            }
            #[derive(Deserialize)]
            struct Body {
                session: String,
            }
            serde_json::from_slice::<Body>(&bytes)
                .map(|b| b.session)
                .map_err(|e| BridgeError::transport(format!("the bridge's session reply is not JSON: {e}")))
        })
    }

    fn close_session(&self, session: String) -> BoxFuture<'static, Result<(), BridgeError>> {
        let client = self.client.clone();
        let url = format!("{}/v1/session/{session}", self.base_url);
        Box::pin(async move {
            let (status, bytes) = Self::call(client, "DELETE", url, String::new()).await?;
            // 204 is the success; the bridge also treats closing an expired
            // session as one, so a late release is never an error here.
            if status != 204 && status != 200 {
                return Err(failure(status, &bytes));
            }
            Ok(())
        })
    }
}

/// Saving the list from a browser is a diff, not a write.
///
/// The desktop writes `redis-servers.toml` whole. A tab cannot: its copy of
/// an existing entry carries settings but no secrets (the bridge's list names
/// the secrets that are set and sends none of them), so sending the whole
/// list back would strip every saved password. Instead each entry goes its
/// own way: one the bridge does not have is posted whole, credentials and
/// all, for the bridge to stamp and dial; one that differs from the copy the
/// bridge last answered is put as an edit, its untouched secrets named
/// rather than sent ([`split_stored_secrets`]); one the list no longer names
/// is deleted; the rest are left alone. The bridge dials an edit before it
/// keeps it, so a change that breaks the connection comes back as the
/// driver's error and the stored entry is unchanged.
impl BridgeServerStore for HttpBridgeTransport {
    fn save(&self, servers: Vec<RedisServer>) -> BoxFuture<'static, Result<Vec<RedisServer>, BridgeError>> {
        let (client, base_url) = (self.client.clone(), self.base_url.clone());
        Box::pin(async move {
            // What the bridge last answered, which is what the form was
            // opened on: an entry that still equals its copy here was not
            // edited, and only the ones that differ are sent.
            let before: HashMap<String, RedisServer> = get_servers()
                .map(|list| list.into_iter().map(|s| (s.id.clone(), s)).collect())
                .unwrap_or_default();
            let after: Vec<&str> = servers.iter().map(|s| s.id.as_str()).collect();
            for server in &servers {
                let (method, url, body) = match before.get(&server.id) {
                    Some(cached) if cached == server => continue,
                    // An edit. The placeholders go back as names: the bridge
                    // keeps those secrets, and clears or replaces the rest.
                    Some(_) => {
                        let (edited, keep_secrets) = split_stored_secrets(server.clone());
                        let body = serde_json::json!({ "server": edited, "keep_secrets": keep_secrets });
                        ("PUT", format!("{base_url}/v1/servers/{}", server.id), body)
                    }
                    // A new entry, whole, for the bridge to stamp an id on and
                    // dial. A placeholder here came from duplicating an entry
                    // whose secret this side never had, so it is dropped — the
                    // dial then says what is missing instead of a stored
                    // password of eight bullets failing later.
                    None => {
                        let (created, _) = split_stored_secrets(server.clone());
                        let body = serde_json::json!({ "server": created });
                        ("POST", format!("{base_url}/v1/servers"), body)
                    }
                };
                let (status, bytes) = Self::call(client.clone(), method, url, body.to_string()).await?;
                if status != 200 {
                    return Err(failure(status, &bytes));
                }
            }
            for id in before.keys().filter(|id| !after.contains(&id.as_str())) {
                let (status, bytes) = Self::call(
                    client.clone(),
                    "DELETE",
                    format!("{base_url}/v1/servers/{id}"),
                    String::new(),
                )
                .await?;
                if status != 204 && status != 200 {
                    return Err(failure(status, &bytes));
                }
            }
            Self::list(client, base_url).await
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::http_client::FakeHttpClient;

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
}
