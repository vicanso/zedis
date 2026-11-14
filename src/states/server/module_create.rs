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

//! Creation of RedisTimeSeries (`TS.CREATE`) and RedisBloom (`BF.RESERVE`,
//! `CF.RESERVE`) keys from the Add Key dialog.

use super::{KeyType, ServerEvent, ServerTask, ZedisServerState};
use crate::connection::{BfReserve, CfReserve, ProbKind, TsCreate, bf_reserve, cf_reserve, expire_key, ts_create};
use crate::error::Error;
use gpui::{Context, SharedString};
use std::sync::Arc;

/// Typed creation payload for a module key.
#[derive(Debug, Clone, PartialEq)]
pub enum ModuleKeyCreateSpec {
    TimeSeries(TsCreate),
    BloomFilter(BfReserve),
    CuckooFilter(CfReserve),
}

impl ModuleKeyCreateSpec {
    pub fn key_type(&self) -> KeyType {
        match self {
            ModuleKeyCreateSpec::TimeSeries(_) => KeyType::TimeSeries,
            ModuleKeyCreateSpec::BloomFilter(_) => KeyType::Probabilistic(ProbKind::Bloom),
            ModuleKeyCreateSpec::CuckooFilter(_) => KeyType::Probabilistic(ProbKind::Cuckoo),
        }
    }
}

fn parse_optional_ttl_secs(ttl: &str) -> Result<Option<u64>, Error> {
    let trimmed = ttl.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    if let Ok(secs) = trimmed.parse::<u64>() {
        return Ok(Some(secs));
    }
    let duration = humantime::parse_duration(trimmed).map_err(|e| Error::Invalid { message: e.to_string() })?;
    Ok(Some(duration.as_secs()))
}

impl ZedisServerState {
    pub fn add_module_key(
        &mut self,
        key: SharedString,
        ttl: SharedString,
        spec: ModuleKeyCreateSpec,
        cx: &mut Context<Self>,
    ) {
        let key: SharedString = key.trim().to_string().into();
        if key.is_empty() {
            return;
        }
        let at = self.at();
        let key_type = spec.key_type();
        let key_clone = key.clone();
        let cached_ttl: i64 = parse_optional_ttl_secs(&ttl)
            .ok()
            .flatten()
            .map_or(-1, |secs| secs as i64);

        self.spawn_with_arg(
            ServerTask::AddKey,
            key.clone(),
            move || async move {
                let ttl_secs = parse_optional_ttl_secs(&ttl)?;
                match &spec {
                    ModuleKeyCreateSpec::TimeSeries(ts) => ts_create(&at, key.as_str(), ts).await?,
                    ModuleKeyCreateSpec::BloomFilter(bf) => bf_reserve(&at, key.as_str(), bf).await?,
                    ModuleKeyCreateSpec::CuckooFilter(cf) => cf_reserve(&at, key.as_str(), cf).await?,
                }
                if let Some(secs) = ttl_secs.filter(|s| *s > 0) {
                    expire_key(&at, key.as_str(), secs).await?;
                }
                Ok(())
            },
            move |this, result, cx| {
                if result.is_ok() {
                    this.scan.keys.insert(key_clone.clone(), key_type);
                    Arc::make_mut(&mut this.scan.key_ttls).insert(key_clone.clone(), cached_ttl);
                    this.scan.renew_tree_id();
                    this.select_key(key_clone, cx);
                    cx.emit(ServerEvent::KeyTreeUpdated);
                    this.refresh_dbsize_now(cx);
                }
                cx.notify();
            },
            cx,
        );
    }
}
