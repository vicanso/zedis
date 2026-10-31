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

//! Per-server threshold alert configuration persisted in `alert_rules`.
//!
//! Alerts are **off by default** (`enabled: None` / `Some(false)`). No rule is
//! evaluated unless the user explicitly enables alerts for a server in the
//! connection settings dialog or in the Metrics Alerts dialog.

use super::{ALERT_RULES_TABLE, Result, get_database};
#[cfg(target_family = "wasm")]
use crate::mem_store::{ReadableDatabase, ReadableTable};
use dashmap::DashMap;
#[cfg(not(target_family = "wasm"))]
use redb::ReadableDatabase;
use serde::{Deserialize, Serialize};
use std::sync::LazyLock;
use zedis_core::alerts::{AlertRule, default_alert_rules, normalize_alert_rules};

const ENVELOPE_VERSION: u32 = 1;

fn default_version() -> u32 {
    ENVELOPE_VERSION
}

/// On-disk JSON envelope stored per `server_id`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ServerAlertConfig {
    #[serde(default = "default_version")]
    pub v: u32,
    /// Explicit per-server enable switch stored in local DB. When `None`,
    /// callers fall back to `RedisServer::alerts_enabled` (which also defaults
    /// to `false`).
    pub enabled: Option<bool>,
    pub rules: Vec<AlertRule>,
}

pub struct AlertRulesManager {
    cache: DashMap<String, ServerAlertConfig>,
}

impl AlertRulesManager {
    fn new() -> Self {
        Self { cache: DashMap::new() }
    }

    fn load_or_default(&self, server_id: &str) -> ServerAlertConfig {
        if let Some(cached) = self.cache.get(server_id) {
            return cached.clone();
        }
        let loaded = self.read_from_db(server_id).unwrap_or_default();
        self.cache.insert(server_id.to_string(), loaded.clone());
        loaded
    }

    fn read_from_db(&self, server_id: &str) -> Option<ServerAlertConfig> {
        let db = get_database().ok()?;
        let read_txn = db.begin_read().ok()?;
        let table = read_txn.open_table(ALERT_RULES_TABLE).ok()?;
        let guard = table.get(server_id).ok()??;
        let parsed: ServerAlertConfig = serde_json::from_slice(guard.value()).ok()?;
        (parsed.v == ENVELOPE_VERSION).then_some(parsed)
    }

    fn persist(&self, server_id: &str, config: &ServerAlertConfig) -> Result<()> {
        let Ok(db) = get_database() else {
            return Ok(());
        };
        let payload = serde_json::to_vec(config)?;
        let write_txn = db.begin_write()?;
        {
            let mut table = write_txn.open_table(ALERT_RULES_TABLE)?;
            table.insert(server_id, payload.as_slice())?;
        }
        write_txn.commit()?;
        Ok(())
    }

    /// Whether alerts are enabled for `server_id`. Strictly `false` by default
    /// unless opted in via `ServerAlertConfig::enabled` or `server_config_enabled`
    /// (`RedisServer::alerts_enabled`).
    pub fn is_enabled(&self, server_id: &str, server_config_enabled: Option<bool>) -> bool {
        if server_id.is_empty() {
            return false;
        }
        let config = self.load_or_default(server_id);
        config.enabled.or(server_config_enabled).unwrap_or(false)
    }

    /// Update only the per-server `enabled` switch (e.g. when saving the
    /// connection form), preserving any customized rules.
    pub fn set_enabled(&self, server_id: &str, enabled: bool) -> Result<()> {
        if server_id.is_empty() {
            return Ok(());
        }
        let mut config = self.load_or_default(server_id);
        config.v = ENVELOPE_VERSION;
        config.enabled = Some(enabled);
        if config.rules.is_empty() {
            config.rules = default_alert_rules();
        }
        self.cache.insert(server_id.to_string(), config.clone());
        self.persist(server_id, &config)
    }

    /// Canonical rule list for `server_id` (merged with defaults so all metrics
    /// are present in `AlertMetric::ALL` order).
    pub fn rules_for(&self, server_id: &str) -> Vec<AlertRule> {
        if server_id.is_empty() {
            return default_alert_rules();
        }
        let config = self.load_or_default(server_id);
        normalize_alert_rules(&config.rules)
    }

    /// Save both the per-server `enabled` switch and the customized `rules`.
    pub fn save_config(&self, server_id: &str, enabled: bool, rules: Vec<AlertRule>) -> Result<()> {
        if server_id.is_empty() {
            return Ok(());
        }
        let normalized = normalize_alert_rules(&rules);
        let config = ServerAlertConfig {
            v: ENVELOPE_VERSION,
            enabled: Some(enabled),
            rules: normalized,
        };
        self.cache.insert(server_id.to_string(), config.clone());
        self.persist(server_id, &config)
    }
}

static ALERT_RULES_MANAGER: LazyLock<AlertRulesManager> = LazyLock::new(AlertRulesManager::new);

pub fn get_alert_rules_manager() -> &'static AlertRulesManager {
    &ALERT_RULES_MANAGER
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::init_database_for_tests;
    use zedis_core::alerts::AlertMetric;
    use zedis_core::fs::override_config_dir;

    #[test]
    fn alerts_are_off_by_default_and_persist_when_opted_in() {
        override_config_dir(std::env::temp_dir().join(format!("zedis-test-config-{}", std::process::id())));
        init_database_for_tests();

        let mgr = get_alert_rules_manager();
        let server_id = "alert-test-srv";

        assert!(!mgr.is_enabled(server_id, None));
        assert!(!mgr.is_enabled(server_id, Some(false)));
        assert!(mgr.is_enabled(server_id, Some(true)));

        let mut rules = mgr.rules_for(server_id);
        assert_eq!(rules.len(), AlertMetric::ALL.len());
        rules[0].threshold = 92.5;

        mgr.save_config(server_id, true, rules).expect("save alert config");
        assert!(mgr.is_enabled(server_id, None));

        // Clear in-memory cache to verify on-disk roundtrip.
        mgr.cache.remove(server_id);
        assert!(mgr.is_enabled(server_id, None));
        let loaded_rules = mgr.rules_for(server_id);
        assert!((loaded_rules[0].threshold - 92.5).abs() < f64::EPSILON);

        mgr.set_enabled(server_id, false).expect("disable alerts");
        assert!(!mgr.is_enabled(server_id, Some(true)));
    }
}
