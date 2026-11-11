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

//! What a page keeps on the bridge for a server: the recycle bin and the
//! metrics history.

use super::*;

#[derive(Deserialize)]
pub(super) struct PutMetricsRequest {
    ts_ms: i64,
    /// The sample as the page holds it — JSON the bridge keeps unread.
    sample: Box<serde_json::value::RawValue>,
}

#[derive(Deserialize)]
pub(super) struct MetricsQuery {
    from_ms: i64,
    #[serde(default = "default_points")]
    max_points: usize,
}

fn default_points() -> usize {
    crate::metrics::MAX_POINTS
}

#[derive(Serialize)]
pub(super) struct MetricsResponse {
    samples: Vec<Box<serde_json::value::RawValue>>,
}

/// Keep a page's metrics sample: one a minute per server, whichever page
/// sent it (`metrics.rs`). Any account that may see the server sends them —
/// a read-only one's page beats like any other.
pub(super) async fn put_metrics(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(req): Json<PutMetricsRequest>,
) -> ApiResult<StatusCode> {
    let account = authorize(&state, &headers, &mut Origin::new(peer, &headers)).await?;
    let server = visible_server(&state.accounts, &id, &account)?;
    let kept = state
        .metrics
        .put(server.id, req.ts_ms, req.sample.get().to_string())
        .await;
    match kept {
        // Kept, or this minute already was: both are what the page wanted.
        Ok(_) => Ok(StatusCode::NO_CONTENT),
        Err(crate::metrics::PutError::Invalid(m)) => Err(ApiError::BadRequest(m)),
        Err(crate::metrics::PutError::OutOfTime) => Err(ApiError::BadRequest(
            "the sample's time is too far from the bridge's clock".to_string(),
        )),
        Err(crate::metrics::PutError::Io(m)) => {
            Err(ApiError::Upstream(format!("the history could not be written: {m}")))
        }
    }
}

/// The server's samples since `from_ms`, thinned to `max_points`.
pub(super) async fn metrics_since(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(query): Query<MetricsQuery>,
) -> ApiResult<Json<MetricsResponse>> {
    let account = authorize(&state, &headers, &mut Origin::new(peer, &headers)).await?;
    let server = visible_server(&state.accounts, &id, &account)?;
    let samples = state
        .metrics
        .since(server.id, query.from_ms, query.max_points)
        .await
        .map_err(|e| ApiError::Upstream(e.to_string()))?;
    Ok(Json(MetricsResponse { samples }))
}

#[derive(Deserialize)]
pub(super) struct PutTrashRequest {
    key: String,
    #[serde(default)]
    db: usize,
    #[serde(default)]
    pttl_ms: i64,
    /// The key's `DUMP`, base64.
    payload: String,
}

#[derive(Serialize)]
pub(super) struct TrashEntryBody {
    #[serde(flatten)]
    meta: crate::trash::Meta,
    /// The `DUMP`, base64 — on a fetch of one entry, never in a listing.
    #[serde(skip_serializing_if = "Option::is_none")]
    payload: Option<String>,
}

/// A key as an audit line names it: cut like a command's argument, since a
/// key name can be as long as a value.
fn audit_key(key: &str) -> String {
    audit::cut(vec![key.to_string()]).pop().unwrap_or_default()
}

/// What `id`'s recycle bin holds, newest first (`trash.rs`). Whoever may
/// see the entry may see what was deleted from it, and by whom.
pub(super) async fn list_trash(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult<Json<Vec<TrashEntryBody>>> {
    let account = authorize(&state, &headers, &mut Origin::new(peer, &headers)).await?;
    let server = visible_server(&state.accounts, &id, &account)?;
    let metas = state
        .trash
        .list(server.id)
        .await
        .map_err(|e| ApiError::Upstream(e.to_string()))?;
    Ok(Json(
        metas
            .into_iter()
            .map(|meta| TrashEntryBody { meta, payload: None })
            .collect(),
    ))
}

/// Keep a key the page is about to delete. The page took the `DUMP`
/// through `/v1/exec`; this stores the bytes. An account that may not write
/// to the entry could not delete from it either.
pub(super) async fn put_trash(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(req): Json<PutTrashRequest>,
) -> ApiResult<Json<TrashEntryBody>> {
    let mut origin = Origin::new(peer, &headers);
    let account = authorize_write(&state, &headers, &mut origin, "trash").await?;
    let server = visible_server(&state.accounts, &id, &account)?;
    refuse_read_only_on(&state, &account, &server, &origin, "trash")?;
    let payload = B64
        .decode(req.payload)
        .map_err(|e| ApiError::BadRequest(format!("the payload is not base64: {e}")))?;
    let too_large = || ApiError::BadRequest(format!("a value over {} bytes is not kept", crate::trash::MAX_PAYLOAD));
    if payload.len() > crate::trash::MAX_PAYLOAD {
        return Err(too_large());
    }
    // A value, so encrypted before it reaches a store (ADR 17).
    let size = payload.len() as u64;
    let payload =
        encrypt_bytes(&payload).map_err(|e| ApiError::Upstream(format!("the value could not be sealed: {e}")))?;
    let reference = ServerRef::from(&server);
    let meta = state
        .trash
        .put(
            server.id,
            NewEntry {
                key: req.key,
                db: req.db,
                pttl_ms: req.pttl_ms,
                deleted_by: account.clone(),
                payload,
                size,
                sealed: true,
            },
        )
        .await
        .map_err(|e| match e {
            PutError::TooLarge => too_large(),
            PutError::Io(e) => ApiError::Upstream(format!("the bin could not be written: {e}")),
        })?;
    if state.audit.logs_writes() {
        state.audit.record(
            &account,
            &origin,
            Event::TrashPut {
                server: reference,
                key: audit_key(&meta.key),
                db: meta.db,
            },
        );
    }
    Ok(Json(TrashEntryBody { meta, payload: None }))
}

/// One deleted key with its `DUMP`, for the page to `RESTORE` through
/// `/v1/exec`, where the account's role and the entry's lock decide;
/// `null` once it is gone (restored by someone else, or a day old).
pub(super) async fn get_trash(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path((id, entry)): Path<(String, String)>,
) -> ApiResult<Json<Option<TrashEntryBody>>> {
    let account = authorize(&state, &headers, &mut Origin::new(peer, &headers)).await?;
    let server = visible_server(&state.accounts, &id, &account)?;
    let found = state
        .trash
        .get(server.id, entry)
        .await
        .map_err(|e| ApiError::Upstream(e.to_string()))?;
    let Some((meta, payload)) = found else {
        return Ok(Json(None));
    };
    let payload = if meta.sealed {
        decrypt_bytes(&payload).map_err(|e| ApiError::Upstream(format!("the value could not be opened: {e}")))?
    } else {
        payload
    };
    Ok(Json(Some(TrashEntryBody {
        meta,
        payload: Some(B64.encode(payload)),
    })))
}

/// Forget one deleted key: restored, or not wanted.
pub(super) async fn remove_trash(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path((id, entry)): Path<(String, String)>,
) -> ApiResult<StatusCode> {
    let mut origin = Origin::new(peer, &headers);
    let account = authorize_write(&state, &headers, &mut origin, "trash").await?;
    let server = visible_server(&state.accounts, &id, &account)?;
    refuse_read_only_on(&state, &account, &server, &origin, "trash")?;
    let reference = ServerRef::from(&server);
    let removed = state
        .trash
        .remove(server.id, entry)
        .await
        .map_err(|e| ApiError::Upstream(e.to_string()))?;
    if let Some(meta) = removed {
        state.audit.record(
            &account,
            &origin,
            Event::TrashRemoved {
                server: reference,
                key: audit_key(&meta.key),
                db: meta.db,
            },
        );
    }
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod bin_tests {
    use super::*;
    use zedis_core::fs::override_config_dir;

    /// A value the page hands the bin is encrypted before it reaches the
    /// store and comes back whole; an entry kept before encryption is still
    /// read as it is; and taking one out is an audit line, while keeping
    /// one is only with `--audit-writes`, like the delete it follows.
    #[tokio::test]
    async fn the_bin_keeps_values_sealed_and_audits_what_leaves_it() {
        override_config_dir(std::env::temp_dir().join(format!("zedis-bridge-store-{}", std::process::id())));
        let dir = std::env::temp_dir().join(format!("zedis-bridge-bin-{}", Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).expect("dir");
        let server = RedisServer {
            id: "bin-srv".to_string(),
            name: "bin".to_string(),
            host: "127.0.0.1".to_string(),
            port: 6379,
            ..Default::default()
        };
        crate::store::LocalServers.put(server).await.expect("server");

        let trash = crate::trash::LocalTrash::new(dir.join("bridge-trash"));
        let audit_path = dir.join("audit.log");
        let state = |writes: bool| AppState {
            accounts: Accounts::from_toml("[[users]]\nname = \"ops\"\npassword = \"secret\"").expect("accounts"),
            sessions: Sessions::new(),
            logins: auth::Logins::new(),
            audit: Audit::open(&audit_path, writes).expect("audit"),
            trusted: None,
            unlocks: policy::Unlocks::new(),
            cookie: auth::CookiePolicy::new(false, ""),
            base_path: String::new(),
            web_root: None,
            mcp_calls: mcp::Limiter::new(),
            presence: Presence::new(),
            watches: Watches::new(),
            servers: Arc::new(crate::store::LocalServers),
            trash: Arc::new(trash.clone()),
            metrics: Arc::new(crate::metrics::LocalMetrics::new(dir.join("bridge-metrics"))),
            page_rows: Arc::new(crate::page_data::LocalPageRows::new(dir.join("rows"))),
            key_notes: Arc::new(crate::page_data::LocalKeyNotes::new(dir.join("notes"))),
            scripts: Arc::new(crate::page_data::LocalScripts::new(dir.join("scripts.json"))),
            throttle: auth::Throttle::new(),
            setup: Setup::in_dir(&dir),
            signup: Signup::Off,
            store_probe: None,
        };
        let peer: SocketAddr = "10.0.0.7:5000".parse().expect("address");
        let mut headers = HeaderMap::new();
        let basic = format!("Basic {}", B64.encode("ops:secret"));
        headers.insert("authorization", basic.parse().expect("header"));
        let value = b"\x00\x0bDUMP payload\xff".to_vec();
        let put = |state: AppState, key: &str| {
            put_trash(
                State(state),
                ConnectInfo(peer),
                headers.clone(),
                Path("bin-srv".to_string()),
                Json(PutTrashRequest {
                    key: key.to_string(),
                    db: 3,
                    pttl_ms: 0,
                    payload: B64.encode(&value),
                }),
            )
        };
        let audit_lines = || std::fs::read_to_string(&audit_path).unwrap_or_default();

        let Ok(Json(kept)) = put(state(false), "plain:off").await else {
            panic!("kept");
        };
        assert!(kept.meta.sealed);
        assert_eq!(
            kept.meta.size,
            value.len() as u64,
            "the value's size, not the sealed one"
        );
        let on_disk = std::fs::read(dir.join("bridge-trash/bin-srv").join(format!("{}.dump", kept.meta.id)))
            .expect("payload file");
        assert!(
            !on_disk.windows(7).any(|w| w == b"payload"),
            "the value is not on disk in the clear"
        );
        assert!(!audit_lines().contains("trash_put"), "not without --audit-writes");

        let Ok(Json(Some(read))) = get_trash(
            State(state(false)),
            ConnectInfo(peer),
            headers.clone(),
            Path(("bin-srv".to_string(), kept.meta.id.clone())),
        )
        .await
        else {
            panic!("read back");
        };
        assert_eq!(
            read.payload.map(|p| B64.decode(p).expect("base64")),
            Some(value.clone())
        );

        let Ok(Json(audited)) = put(state(true), "with:writes").await else {
            panic!("kept");
        };
        assert!(audit_lines().contains("\"event\":\"trash_put\""), "{}", audit_lines());

        // Kept before the bin encrypted: read as it is.
        let legacy = TrashStore::put(
            &trash,
            "bin-srv".to_string(),
            NewEntry {
                key: "legacy".to_string(),
                db: 0,
                pttl_ms: 0,
                deleted_by: "ops".to_string(),
                payload: value.clone(),
                size: value.len() as u64,
                sealed: false,
            },
        )
        .await
        .expect("legacy");
        let Ok(Json(Some(read))) = get_trash(
            State(state(false)),
            ConnectInfo(peer),
            headers.clone(),
            Path(("bin-srv".to_string(), legacy.id)),
        )
        .await
        else {
            panic!("legacy read back");
        };
        assert_eq!(
            read.payload.map(|p| B64.decode(p).expect("base64")),
            Some(value.clone())
        );

        let removed = remove_trash(
            State(state(false)),
            ConnectInfo(peer),
            headers.clone(),
            Path(("bin-srv".to_string(), audited.meta.id)),
        )
        .await;
        assert!(matches!(removed, Ok(StatusCode::NO_CONTENT)));
        let lines = audit_lines();
        let line = lines
            .lines()
            .find(|l| l.contains("\"event\":\"trash_removed\""))
            .expect("a removal is a line without --audit-writes");
        assert!(
            line.contains("\"key\":\"with:writes\"") && line.contains("\"db\":3"),
            "{line}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
