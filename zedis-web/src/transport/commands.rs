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

//! Commands and what holds a connection: `/v1/exec`, the terminal's
//! sessions, the write lock's window, `/v1/watch`, the diagnosis and signing
//! out — [`BridgeTransport`] itself.

use super::*;

/// The `/v1/exec` body.
///
/// Field names are the contract with `zedis-bridge`'s `ExecRequest`; the test
/// below is what keeps the two from drifting apart silently, since a rename
/// on either side would otherwise only show up as a 400 at runtime. `desc` is
/// the one field outside that contract: a readable label per command
/// (`describe_commands`) for whoever reads the request in the browser's
/// network panel, where `commands` is only base64. The bridge ignores it.
pub(super) fn exec_payload(request: &BridgeRequest) -> serde_json::Value {
    let mut payload = serde_json::json!({
        "server": request.server_id,
        "db": request.db,
        "commands": request.commands.iter().map(|c| B64.encode(c)).collect::<Vec<_>>(),
        "desc": describe_commands(&request.commands),
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

    fn unlock_writes(&self, server_id: String, confirm: String) -> BoxFuture<'static, Result<(), BridgeError>> {
        let client = self.client.clone();
        let url = format!("{}/v1/servers/{server_id}/unlock", self.base_url);
        Box::pin(async move {
            let body = serde_json::json!({ "confirm": confirm }).to_string();
            let (status, bytes) = Self::post(client, url, body).await?;
            if status != 200 {
                return Err(failure(status, &bytes));
            }
            Ok(())
        })
    }

    fn lock_writes(&self, server_id: String) -> BoxFuture<'static, Result<(), BridgeError>> {
        let client = self.client.clone();
        let url = format!("{}/v1/servers/{server_id}/unlock", self.base_url);
        Box::pin(async move {
            let (status, bytes) = Self::call(client, "DELETE", url, String::new()).await?;
            if status != 204 && status != 200 {
                return Err(failure(status, &bytes));
            }
            Ok(())
        })
    }

    /// `POST /v1/logout`, then the page's own way back to the sign-in form.
    ///
    /// The bridge answers `204` whether or not it knew the login, and clears
    /// the cookie either way — the cookie is `HttpOnly`, so nothing here
    /// could have. Anything else leaves the login standing, and saying so is
    /// better than a reload that comes back signed in.
    fn sign_out(&self) -> BoxFuture<'static, Result<(), BridgeError>> {
        let client = self.client.clone();
        let url = format!("{}/v1/logout", self.base_url);
        Box::pin(async move {
            let (status, bytes) = Self::post(client, url, String::new()).await?;
            if status != 204 && status != 200 {
                return Err(failure(status, &bytes));
            }
            sign_in_again();
            Ok(())
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
    fn open_watch(
        &self,
        server_id: String,
        db: usize,
        kind: WatchKind,
        channels: Vec<String>,
        confirm: Option<String>,
    ) -> BoxFuture<'static, Result<WatchOpened, BridgeError>> {
        let client = self.client.clone();
        let url = format!("{}/v1/watch", self.base_url);
        Box::pin(async move {
            let mut body = serde_json::json!({
                "server": server_id,
                "db": db,
                "kind": kind.as_str(),
                "channels": channels,
            });
            if let Some(confirm) = confirm {
                body["confirm"] = serde_json::Value::String(confirm);
            }
            let body = body.to_string();
            let (status, bytes) = Self::post(client, url, body).await?;
            if status != 200 {
                return Err(failure(status, &bytes));
            }
            #[derive(Deserialize)]
            struct Failure {
                node: String,
                error: String,
            }
            #[derive(Deserialize)]
            struct Body {
                token: String,
                #[serde(default)]
                nodes: Vec<String>,
                #[serde(default)]
                failures: Vec<Failure>,
            }
            let body = serde_json::from_slice::<Body>(&bytes)
                .map_err(|e| BridgeError::transport(format!("the bridge's watch reply is not JSON: {e}")))?;
            Ok(WatchOpened {
                token: body.token,
                nodes: body.nodes,
                failures: body.failures.into_iter().map(|f| (f.node, f.error)).collect(),
            })
        })
    }

    fn poll_watch(&self, token: String) -> BoxFuture<'static, Result<WatchBatch, BridgeError>> {
        let client = self.client.clone();
        let url = format!("{}/v1/watch/{token}/poll", self.base_url);
        Box::pin(async move {
            let body = serde_json::json!({ "wait_ms": WATCH_WAIT_MS }).to_string();
            let (status, bytes) = Self::post(client, url, body).await?;
            if status != 200 {
                return Err(failure(status, &bytes));
            }
            watch_batch(&bytes)
        })
    }

    fn release_watch(&self, token: String) {
        let client = self.client.clone();
        let url = format!("{}/v1/watch/{token}", self.base_url);
        start_detached(async move {
            // Idempotent on the bridge, and nobody is waiting to hear.
            let _ = Self::call(client, "DELETE", url, String::new()).await;
        });
    }

    fn diagnose(&self, server_id: String) -> BoxFuture<'static, Result<Vec<DiagReportRow>, BridgeError>> {
        let client = self.client.clone();
        let url = format!("{}/v1/servers/{}/diagnose", self.base_url, segment(&server_id));
        Box::pin(async move {
            let (status, bytes) = Self::post(client, url, String::new()).await?;
            if status != 200 {
                return Err(failure(status, &bytes));
            }
            #[derive(Deserialize)]
            struct Body {
                stages: Vec<DiagReportRow>,
            }
            serde_json::from_slice::<Body>(&bytes)
                .map(|body| body.stages)
                .map_err(|e| BridgeError::transport(format!("the bridge's diagnosis is not JSON: {e}")))
        })
    }

    fn kill_running(
        &self,
        server_id: String,
        target: KillTarget,
    ) -> BoxFuture<'static, Result<Vec<KillReply>, BridgeError>> {
        let client = self.client.clone();
        let url = format!("{}/v1/servers/{}/kill-script", self.base_url, segment(&server_id));
        Box::pin(async move {
            let body = serde_json::json!({ "target": target }).to_string();
            let (status, bytes) = Self::post(client, url, body).await?;
            if status != 200 {
                return Err(failure(status, &bytes));
            }
            #[derive(Deserialize)]
            struct Body {
                replies: Vec<KillReply>,
            }
            serde_json::from_slice::<Body>(&bytes)
                .map(|body| body.replies)
                .map_err(|e| BridgeError::transport(format!("the bridge's kill-script reply is not JSON: {e}")))
        })
    }
}

/// How long a watch's poll may wait on the bridge for something to arrive:
/// under the bridge's own cap, so the bridge is what decides.
pub(super) const WATCH_WAIT_MS: u64 = 15_000;

/// A poll's reply: Pub/Sub messages carry their payload in base64.
pub(super) fn watch_batch(bytes: &[u8]) -> Result<WatchBatch, BridgeError> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Item {
        Message { channel: String, payload: String },
        Line { node: String, line: String },
    }
    #[derive(Deserialize)]
    struct Body {
        #[serde(default)]
        items: Vec<Item>,
        #[serde(default)]
        dropped: u64,
        #[serde(default)]
        closed: Option<String>,
    }
    let body = serde_json::from_slice::<Body>(bytes)
        .map_err(|e| BridgeError::transport(format!("the bridge's poll reply is not JSON: {e}")))?;
    let mut items = Vec::with_capacity(body.items.len());
    for item in body.items {
        items.push(match item {
            Item::Message { channel, payload } => WatchItem::Message {
                channel,
                payload: B64
                    .decode(payload)
                    .map_err(|e| BridgeError::transport(format!("a message's payload is not base64: {e}")))?,
            },
            Item::Line { node, line } => WatchItem::Line { node, line },
        });
    }
    Ok(WatchBatch {
        items,
        dropped: body.dropped,
        closed: body.closed,
    })
}
