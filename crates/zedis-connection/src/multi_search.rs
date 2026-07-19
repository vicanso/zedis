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

//! Cross-server key search backing the multi-database search palette.
//!
//! Two read-only passes over a caller-selected set of `(server_id, db)`
//! targets:
//!
//! - [`multi_search_exact`] — a single `TYPE` probe per target ("none"
//!   answers double as the existence check), surfacing exact key hits.
//! - [`multi_search_scan`] — a capped, pattern-wrapped `SCAN` per target
//!   (contains-match unless the query already carries glob characters).
//!
//! Targets are queried **concurrently** and failures stay **per-server**
//! (`error` on that target's result) so one unreachable instance can't
//! blank the whole panel. Both passes ride the pooled clients — no
//! dedicated connections.

use crate::manager::get_connection_manager;
use futures::future::join_all;

/// One key hit on one server.
#[derive(Debug, Clone)]
pub struct MultiSearchHit {
    pub key: String,
    /// Redis `TYPE` name (`string` / `hash` / …).
    pub key_type: String,
}

/// Everything one target contributed to a search round.
#[derive(Debug, Clone)]
pub struct MultiSearchServerResult {
    pub server_id: String,
    pub db: usize,
    pub hits: Vec<MultiSearchHit>,
    /// Connection / command failure for this target, verbatim.
    pub error: Option<String>,
    /// SCAN stopped at the per-server cap — more keys may match.
    pub truncated: bool,
}

impl MultiSearchServerResult {
    fn empty(server_id: String, db: usize) -> Self {
        Self {
            server_id,
            db,
            hits: Vec::new(),
            error: None,
            truncated: false,
        }
    }
}

/// Exact lookup: one `TYPE` round trip per target; a non-"none" answer is
/// a hit (and carries the type for the result row's badge).
pub async fn multi_search_exact(targets: Vec<(String, usize)>, key: String) -> Vec<MultiSearchServerResult> {
    let tasks = targets.into_iter().map(|(server_id, db)| {
        let key = key.clone();
        async move {
            let mut out = MultiSearchServerResult::empty(server_id, db);
            match get_connection_manager().get_client(&out.server_id, db).await {
                Ok(client) => match client.key_type(&key).await {
                    Ok(key_type) if key_type != "none" => out.hits.push(MultiSearchHit { key, key_type }),
                    Ok(_) => {}
                    Err(e) => out.error = Some(e.to_string()),
                },
                Err(e) => out.error = Some(e.to_string()),
            }
            out
        }
    });
    join_all(tasks).await
}

/// Build the `MATCH` pattern for a query: explicit glob characters are
/// respected as-is (power users can type `user:?x*`); a plain fragment
/// becomes a contains-match.
fn scan_pattern(query: &str) -> String {
    if query.contains(['*', '?', '[']) {
        query.to_string()
    } else {
        format!("*{query}*")
    }
}

/// Upper bound on keys *examined* per server per search (≈ pages ×
/// `COUNT`). `per_server_cap` only bounds collected **results** — with a
/// sparse pattern on a huge keyspace the result cap alone would let the
/// loop traverse the entire keyspace looking for matches. Mirrors the
/// value-search guardrail philosophy: results are an explicit sample.
const SCAN_EXAMINE_BUDGET: u64 = 50_000;

/// `COUNT` hint per `SCAN` round — how many keys each round trip examines,
/// **decoupled** from `per_server_cap` (which bounds collected *results*).
/// The cap used to double as the COUNT, so the common default cap of 10
/// issued `SCAN … COUNT 10` and a sparse pattern needed ~5 000 round trips
/// to spend the examine budget below — latency-bound and visibly slow.
///
/// 1000 is the conventional `SCAN COUNT` ceiling: it spends the 50 000
/// budget in ~50 round trips (was ~5 000), and each `SCAN` still examines
/// only ~1000 buckets server-side — sub-millisecond even on a busy instance.
/// Going higher buys diminishing round-trip savings (~50 → ~25 at 2 000)
/// while each call's O(COUNT) bucket scan starts to add measurable latency
/// for other clients on a loaded production Redis, which this palette may
/// well be pointed at. MATCH/TYPE filtering happens within the batch, so a
/// large COUNT never inflates the payload.
const SCAN_PAGE_COUNT: u64 = 1000;

/// Capped `SCAN` per target. Pages until `per_server_cap` **results** are
/// collected, the examine budget is spent, or the (per-master) cursors are
/// exhausted; `truncated` marks either early stop. Types come for free
/// from the key-tree `scan` pipeline.
pub async fn multi_search_scan(
    targets: Vec<(String, usize)>,
    query: String,
    per_server_cap: usize,
) -> Vec<MultiSearchServerResult> {
    let pattern = scan_pattern(&query);
    let tasks = targets.into_iter().map(|(server_id, db)| {
        let pattern = pattern.clone();
        async move {
            let mut out = MultiSearchServerResult::empty(server_id, db);
            let client = match get_connection_manager().get_client(&out.server_id, db).await {
                Ok(c) => c,
                Err(e) => {
                    out.error = Some(e.to_string());
                    return out;
                }
            };
            let mut cursors = None;
            let mut examined: u64 = 0;
            loop {
                match client.scan(cursors, &pattern, SCAN_PAGE_COUNT, false, None).await {
                    Ok((next, page)) => {
                        // COUNT is a per-cursor hint; on a cluster each of N
                        // masters examines ~COUNT keys per round.
                        examined += SCAN_PAGE_COUNT * next.len().max(1) as u64;
                        for (key, key_type, _ttl) in page {
                            if out.hits.len() >= per_server_cap {
                                out.truncated = true;
                                break;
                            }
                            out.hits.push(MultiSearchHit { key, key_type });
                        }
                        if examined >= SCAN_EXAMINE_BUDGET && !next.iter().all(|&c| c == 0) {
                            out.truncated = true;
                        }
                        if out.truncated || next.iter().all(|&c| c == 0) {
                            break;
                        }
                        cursors = Some(next);
                    }
                    Err(e) => {
                        out.error = Some(e.to_string());
                        break;
                    }
                }
            }
            out
        }
    });
    join_all(tasks).await
}

#[cfg(test)]
mod tests {
    use super::scan_pattern;

    #[test]
    fn plain_fragment_becomes_contains_match() {
        assert_eq!(scan_pattern("user"), "*user*");
    }

    #[test]
    fn explicit_glob_is_respected() {
        assert_eq!(scan_pattern("user:*"), "user:*");
        assert_eq!(scan_pattern("se?sion"), "se?sion");
        assert_eq!(scan_pattern("k[12]"), "k[12]");
    }
}
