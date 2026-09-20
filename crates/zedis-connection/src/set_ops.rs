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

//! Set operations: a page of members, and the three writes the editor has.

#[cfg(target_family = "wasm")]
use crate::bridge::{BridgePipeline as _, BridgeQuery as _};
use crate::error::Error;
use crate::hash_fields::contains_pattern;
use crate::server_db::ServerDb;
use redis::{cmd, pipe};

type Result<T, E = Error> = std::result::Result<T, E>;

/// `SCARD`.
pub async fn set_card(at: &ServerDb, key: &str) -> Result<usize> {
    Ok(cmd("SCARD").arg(key).query_async(&mut at.connection().await?).await?)
}

/// One `SSCAN` round: the next cursor (0 when the scan is complete) and the
/// members, bytes kept as answered. `keyword` filters by substring.
pub async fn set_scan(
    at: &ServerDb,
    key: &str,
    keyword: Option<&str>,
    cursor: u64,
    count: usize,
) -> Result<(u64, Vec<Vec<u8>>)> {
    Ok(cmd("SSCAN")
        .arg(key)
        .arg(cursor)
        .arg("MATCH")
        .arg(contains_pattern(keyword))
        .arg("COUNT")
        .arg(count)
        .query_async(&mut at.connection().await?)
        .await?)
}

/// `SADD key member`; `false` when it was already a member.
pub async fn set_add(at: &ServerDb, key: &str, member: &[u8]) -> Result<bool> {
    let added: usize = cmd("SADD")
        .arg(key)
        .arg(member)
        .query_async(&mut at.connection().await?)
        .await?;
    Ok(added > 0)
}

/// `SREM key member…`; answers how many were members.
pub async fn set_remove(at: &ServerDb, key: &str, members: &[&[u8]]) -> Result<usize> {
    if members.is_empty() {
        return Ok(0);
    }
    let mut c = cmd("SREM");
    c.arg(key);
    for member in members {
        c.arg(*member);
    }
    Ok(c.query_async(&mut at.connection().await?).await?)
}

/// An edited member: the old one removed and the new one added, in one
/// pipeline. `false` when the new member was already in the set — the edit
/// then merged two members into one.
pub async fn set_replace_member(at: &ServerDb, key: &str, old: &[u8], new: &[u8]) -> Result<bool> {
    let (_removed, added): (usize, usize) = pipe()
        .cmd("SREM")
        .arg(key)
        .arg(old)
        .cmd("SADD")
        .arg(key)
        .arg(new)
        .query_async(&mut at.connection().await?)
        .await?;
    Ok(added > 0)
}
