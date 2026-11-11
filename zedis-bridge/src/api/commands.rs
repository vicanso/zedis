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

//! The one route that forwards commands (`/v1/exec`), the sessions a
//! terminal keeps, and the forwarding the MCP tools share.

use super::*;

#[derive(Deserialize)]
pub(super) struct SessionRequest {
    server: String,
    #[serde(default)]
    db: usize,
}

#[derive(Serialize)]
pub(super) struct SessionResponse {
    session: String,
}

pub(super) async fn open_session(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<SessionRequest>,
) -> ApiResult<Json<SessionResponse>> {
    let mut origin = Origin::new(peer, &headers);
    let account = authorize(&state, &headers, &mut origin).await?;
    visible_server(&state.accounts, &req.server, &account)?;
    note_server(&state, &origin, &req.server);
    let session = state
        .sessions
        .open(&account, &req.server, req.db)
        .await
        .map_err(|e| match e {
            SessionError::TooMany(msg) => ApiError::BadRequest(msg.to_string()),
            SessionError::Upstream(msg) => ApiError::Upstream(msg),
        })?;
    Ok(Json(SessionResponse { session }))
}

pub(super) async fn close_session(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(token): Path<String>,
) -> ApiResult<StatusCode> {
    let account = authorize(&state, &headers, &mut Origin::new(peer, &headers)).await?;
    state.sessions.close(&token, &account).await;
    // Idempotent: closing a session that has already expired is not an error.
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Default, Deserialize)]
pub(crate) struct PipelineSpecDto {
    offset: usize,
    count: usize,
    #[serde(default)]
    atomic: bool,
}

#[derive(Deserialize, Default)]
pub(crate) struct ExecRequest {
    server: String,
    #[serde(default)]
    db: usize,
    /// The packed commands, base64 of what `Cmd::get_packed_command()` made.
    /// One entry is a plain command; several are a pipeline.
    ///
    /// The page also sends `desc`, a readable label per command for its own
    /// network panel. It is deliberately not a field here: a label is the
    /// caller's claim about the commands, and the policy judges the commands.
    commands: Vec<String>,
    /// Present only for a pipeline, carrying the framing redis-rs asks a
    /// connection for. `MULTI`/`EXEC` are added here from `atomic`, exactly
    /// as redis-rs adds them when it packs a transaction, so the caller's
    /// `offset` still counts the replies it expects to skip.
    #[serde(default)]
    pipeline: Option<PipelineSpecDto>,
    /// Run the commands on every master of the server instead of on one
    /// connection. The bridge owns the topology, so it does the fan-out and
    /// the caller never holds a dialable node address.
    #[serde(default)]
    fanout: Option<String>,
    /// Which node each command is for, as a `host:port` label. Empty means
    /// every master, padding with the first command. When set, `commands[i]`
    /// is for `fanout_nodes[i]` and unlisted nodes are not asked.
    ///
    /// A label may repeat: the commands carrying it run as one pipeline on
    /// that node, in the order given. That is what the key tree's `TYPE` /
    /// `TTL` round needs, and it cannot be done as a single pipeline over one
    /// connection — redis-rs refuses a cluster pipeline whose commands span
    /// slots, which a page of scanned keys always does.
    #[serde(default)]
    fanout_nodes: Vec<String>,
    /// Pin the commands to a session's own connection.
    #[serde(default)]
    session: Option<String>,
    /// What the caller sends after being refused once.
    #[serde(default)]
    confirm: Option<String>,
}

impl ExecRequest {
    /// The request an MCP tool makes (`mcp.rs`), whose commands arrive
    /// already decoded — so no frames — for one routed connection, every
    /// master, or the masters `fanout_nodes` names, one command each.
    pub(crate) fn decoded(server: &str, db: usize, fanout_nodes: Vec<String>, every_master: bool) -> Self {
        Self {
            server: server.to_string(),
            db,
            fanout: (every_master || !fanout_nodes.is_empty()).then(|| "masters".to_string()),
            fanout_nodes,
            ..Default::default()
        }
    }
}

#[derive(Serialize)]
pub(super) struct ExecResponse {
    /// One base64 RESP frame per reply, for `parse_redis_value`.
    replies: Vec<String>,
    /// For a fan-out, the `host:port` of the node behind each reply. A label
    /// for the caller to print; the connection details stay here.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    nodes: Vec<String>,
}

/// Which commands each master is being asked for, by label.
///
/// One entry per master, holding the *indexes* of the commands aimed at it in
/// the order the caller sent them — order is load-bearing, because the caller
/// puts the replies back together by walking its own batches. A label that
/// names no master this process knows is dropped rather than guessed at: the
/// two sides discover the topology separately, and a reply attributed to the
/// wrong node would resume a scan from another node's cursor (ADR 9).
fn aim_at_nodes(master_labels: &[String], wanted: &[String]) -> Vec<Vec<usize>> {
    let mut aimed: Vec<Vec<usize>> = master_labels.iter().map(|_| Vec::new()).collect();
    for (index, label) in wanted.iter().enumerate() {
        if let Some(master) = master_labels.iter().position(|m| m == label) {
            aimed[master].push(index);
        }
    }
    aimed
}

pub(super) async fn exec(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<ExecRequest>,
) -> ApiResult<Json<ExecResponse>> {
    let mut origin = Origin::new(peer, &headers);
    let account = authorize(&state, &headers, &mut origin).await?;
    if req.commands.is_empty() {
        return Err(ApiError::BadRequest("no commands".to_string()));
    }

    let mut decoded = Vec::with_capacity(req.commands.len());
    for (index, frame) in req.commands.iter().enumerate() {
        let bytes = B64
            .decode(frame.as_bytes())
            .map_err(|e| ApiError::BadRequest(format!("command {index} is not base64: {e}")))?;
        decoded.push(resp::decode_command(&bytes).map_err(|e| ApiError::BadRequest(format!("command {index}: {e}")))?);
    }

    let server = visible_server(&state.accounts, &req.server, &account)?;
    note_server(&state, &origin, &server.id);
    // Not `authorize_write`: most of what reaches this route *is* a read, and
    // a read-only account has to keep running those. The role — on *this*
    // entry, since an account's rules may grant it read-only here and full
    // elsewhere — is carried down to `policy::check`, which judges each
    // command.
    let read_only = state.accounts.is_read_only_on(&account, &server);
    let unlocked = state.unlocks.active(&account, &server.id);
    // Every command in a batch is judged. Gating only the first would let a
    // pipeline smuggle a FLUSHALL in behind a GET.
    let verdicts: Vec<policy::Verdict> = decoded
        .iter()
        .map(|args| {
            policy::check(
                &server,
                args,
                req.confirm.as_deref(),
                read_only,
                unlocked,
                req.session.is_some(),
            )
        })
        .collect();
    for (args, verdict) in decoded.iter().zip(&verdicts) {
        let refusal = match verdict {
            policy::Verdict::Allow | policy::Verdict::Confirmed { .. } => continue,
            policy::Verdict::Deny => ApiError::Forbidden(READ_ONLY_REFUSAL.to_string()),
            policy::Verdict::Confirm { kind, strictness } => ApiError::ConfirmationRequired {
                kind: kind.i18n_key().to_string(),
                strictness: match strictness {
                    zedis_connection::ConfirmStrictness::Click => "click",
                    zedis_connection::ConfirmStrictness::TypeName => "type_name",
                },
            },
        };
        // The refused command alone: the batch went nowhere.
        for line in audit::command_lines(&server, req.db, [(args, verdict)], false) {
            state.audit.record(&account, &origin, Event::Command(line));
        }
        return Err(refusal);
    }

    // What the log keeps of this batch, decided before it goes out and
    // written after, with the outcome.
    let lines = audit::command_lines(
        &server,
        req.db,
        decoded.iter().zip(&verdicts),
        state.audit.logs_writes(),
    );
    let result = forward(&state, &account, &req, &decoded).await;
    let error = result.as_ref().err().map(ApiError::describe);
    for mut line in lines {
        line.error.clone_from(&error);
        state.audit.record(&account, &origin, Event::Command(line));
    }
    result.map(Json)
}

/// Send the judged commands of `req` and collect the replies as frames.
async fn forward(
    state: &AppState,
    account: &str,
    req: &ExecRequest,
    decoded: &[Vec<Vec<u8>>],
) -> ApiResult<ExecResponse> {
    let (values, nodes) = forward_values(state, account, req, decoded).await?;
    let replies = values
        .iter()
        .map(|value| {
            resp::encode_to_vec(value)
                .map(|bytes| B64.encode(bytes))
                .map_err(|e| ApiError::Upstream(e.to_string()))
        })
        .collect::<ApiResult<Vec<String>>>()?;
    Ok(ExecResponse { replies, nodes })
}

/// Send the judged commands of `req` and collect the replies as values,
/// with the `host:port` of the node behind each one for a fan-out. The
/// page's route encodes them as frames; the MCP tools (`mcp.rs`) read them.
pub(crate) async fn forward_values(
    state: &AppState,
    account: &str,
    req: &ExecRequest,
    decoded: &[Vec<Vec<u8>>],
) -> ApiResult<(Vec<redis::Value>, Vec<String>)> {
    // Without a session — and always for a fan-out, which ignores one — the
    // commands run on a connection other callers share. What would change or
    // hold it is refused here, the one path both the exec route and the MCP
    // tools take, whatever the allowlist thinks of it (`policy::holds_connection`).
    if (req.session.is_none() || req.fanout.is_some())
        && let Some(args) = decoded.iter().find(|args| policy::holds_connection(args))
    {
        return Err(ApiError::BadRequest(format!(
            "{} changes or holds the connection it runs on, which other callers share; open a session for it",
            policy::command_name(args)
        )));
    }
    // Fan-out is its own path: it needs the client, not a connection, because
    // reaching every master is topology knowledge this process owns.
    if let Some(mode) = &req.fanout {
        if mode != "masters" {
            return Err(ApiError::BadRequest(format!("unknown fanout \"{mode}\"")));
        }
        if req.pipeline.is_some() {
            return Err(ApiError::BadRequest("a fan-out cannot also be a pipeline".to_string()));
        }
        let client = get_connection_manager()
            .get_client(&req.server, req.db)
            .await
            .map_err(|e| ApiError::Upstream(e.to_string()))?;
        let (servers, values): (Vec<zedis_connection::RedisServer>, Vec<redis::Value>) = if req.fanout_nodes.is_empty()
        {
            let cmds: Vec<redis::Cmd> = decoded.iter().map(|args| resp::command_from_args(args)).collect();
            // Reuses the desktop's own fan-out, so the padding rule that
            // repeats the first command across the remaining masters is
            // identical.
            client
                .query_async_masters(cmds)
                .await
                .map_err(|e| ApiError::Upstream(e.to_string()))?
        } else {
            if req.fanout_nodes.len() != decoded.len() {
                return Err(ApiError::BadRequest(
                    "fanout_nodes and commands must be the same length".to_string(),
                ));
            }
            // Re-aim the caller's labels onto *this* process's master list.
            // A label that is not here is dropped rather than guessed at: the
            // reply set says which nodes actually answered, and the caller
            // re-aligns on that.
            //
            // Everything goes out as a pipeline, including a batch of one —
            // redis-rs sends the same bytes either way, and one path is one
            // thing to get right.
            let masters = client.master_servers();
            let master_labels: Vec<String> = masters.iter().map(|s| format!("{}:{}", s.host, s.port)).collect();
            let aimed: Vec<Option<redis::Pipeline>> = aim_at_nodes(&master_labels, &req.fanout_nodes)
                .into_iter()
                .map(|indexes| {
                    (!indexes.is_empty()).then(|| {
                        let mut pipe = redis::pipe();
                        for index in indexes {
                            pipe.add_command(resp::command_from_args(&decoded[index]));
                        }
                        pipe
                    })
                })
                .collect();
            if aimed.iter().all(Option::is_none)
                && req.fanout_nodes.len() == 1
                && decoded.len() == 1
                && client.node_addrs().iter().any(|addr| addr == &req.fanout_nodes[0])
            {
                let target_addr = &req.fanout_nodes[0];
                let mut conn = zedis_connection::open_node_connection_cached(&req.server, target_addr)
                    .await
                    .map_err(|e| ApiError::Upstream(e.to_string()))?;
                let value = conn
                    .req_packed_command(&resp::command_from_args(&decoded[0]))
                    .await
                    .map_err(|e| ApiError::Upstream(e.to_string()))?;
                return Ok((vec![value], vec![target_addr.clone()]));
            }
            let replies: Vec<Option<Vec<redis::Value>>> = client
                .query_async_masters_pipelines(aimed)
                .await
                .map_err(|e| ApiError::Upstream(e.to_string()))?;
            // One label per reply frame, so a caller that sent several
            // commands to one node can put the answers back together.
            let mut answered = Vec::new();
            let mut values = Vec::new();
            for (server, reply) in masters.into_iter().zip(replies) {
                let Some(batch) = reply else { continue };
                for value in batch {
                    answered.push(server.clone());
                    values.push(value);
                }
            }
            (answered, values)
        };
        let nodes = servers.iter().map(|s| format!("{}:{}", s.host, s.port)).collect();
        return Ok((values, nodes));
    }

    let mut conn = match &req.session {
        Some(token) => state
            .sessions
            .take(token, account, &req.server, req.db)
            .await
            .ok_or_else(|| ApiError::BadRequest("unknown or mismatched session".to_string()))?,
        None => get_connection_manager()
            .get_connection(&req.server, req.db)
            .await
            .map_err(|e| ApiError::Upstream(e.to_string()))?,
    };

    let values = match &req.pipeline {
        None => {
            // One command. More without a pipeline would have been judged and
            // audited in full and then sent as only the first.
            if decoded.len() != 1 {
                return Err(ApiError::BadRequest(
                    "several commands are a pipeline; send them with its framing".to_string(),
                ));
            }
            let cmd = resp::command_from_args(&decoded[0]);
            vec![
                conn.req_packed_command(&cmd)
                    .await
                    .map_err(|e| ApiError::Upstream(e.to_string()))?,
            ]
        }
        Some(spec) => {
            // The framing is checked, not trusted: on a multiplexed connection
            // `count` is how replies are dealt out, so a count these commands
            // do not produce hands another caller's replies to this one, or
            // this one's to the next.
            let expected = PipelineSpec::for_pipeline(decoded.len(), spec.atomic);
            if (spec.offset, spec.count) != (expected.offset, expected.count) {
                return Err(ApiError::BadRequest(format!(
                    "a pipeline of {} commands{} is framed as offset {}, count {}",
                    decoded.len(),
                    if spec.atomic { " in a transaction" } else { "" },
                    expected.offset,
                    expected.count
                )));
            }
            let mut pipe = redis::Pipeline::with_capacity(decoded.len());
            if spec.atomic {
                pipe.atomic();
            }
            for args in decoded {
                pipe.add_command(resp::command_from_args(args));
            }
            conn.req_packed_commands(&pipe, spec.offset, spec.count)
                .await
                .map_err(|e| ApiError::Upstream(e.to_string()))?
        }
    };

    Ok((values, Vec::new()))
}

#[cfg(test)]
mod fanout_tests {
    use super::aim_at_nodes;

    fn labels(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn one_command_per_node_is_the_ordinary_case() {
        let masters = labels(&["a:1", "b:2", "c:3"]);
        assert_eq!(
            aim_at_nodes(&masters, &labels(&["a:1", "b:2", "c:3"])),
            vec![vec![0], vec![1], vec![2]]
        );
    }

    #[test]
    fn a_repeated_label_becomes_one_batch_in_the_order_it_was_sent() {
        // What the key tree's TYPE round needs: every key a node returned
        // from SCAN goes back to that node, together.
        let masters = labels(&["a:1", "b:2"]);
        assert_eq!(
            aim_at_nodes(&masters, &labels(&["a:1", "b:2", "a:1", "a:1"])),
            vec![vec![0, 2, 3], vec![1]],
            "order within a batch is what lets the caller re-pair replies with keys"
        );
    }

    #[test]
    fn a_master_nobody_asked_for_gets_nothing() {
        let masters = labels(&["a:1", "b:2", "c:3"]);
        assert_eq!(
            aim_at_nodes(&masters, &labels(&["c:3", "c:3"])),
            vec![vec![], vec![], vec![0, 1]]
        );
    }

    #[test]
    fn a_label_this_process_does_not_know_is_dropped_not_guessed_at() {
        // The caller's topology moved between its discovery and ours. The
        // reply set says which nodes answered; inventing a home for this one
        // would resume a scan from the wrong cursor.
        let masters = labels(&["a:1", "b:2"]);
        assert_eq!(
            aim_at_nodes(&masters, &labels(&["a:1", "gone:9", "b:2"])),
            vec![vec![0], vec![2]]
        );
    }

    #[test]
    fn no_masters_means_nothing_to_aim_at() {
        assert!(aim_at_nodes(&[], &labels(&["a:1"])).is_empty());
    }
}

#[cfg(test)]
mod exec_request_tests {
    use super::ExecRequest;

    #[test]
    fn the_page_s_readable_labels_are_accepted_and_ignored() {
        // `desc` is not a field: a body that carries it must still parse, or
        // every request from the page would be a 400.
        let body = serde_json::json!({
            "server": "srv",
            "db": 2,
            "commands": ["KjENCiQ0DQpQSU5HDQo="],
            "desc": ["PING"],
        });
        let request: ExecRequest = serde_json::from_value(body).expect("a labelled body parses");
        assert_eq!(request.server, "srv");
        assert_eq!(request.db, 2);
        assert_eq!(request.commands.len(), 1);
    }
}
