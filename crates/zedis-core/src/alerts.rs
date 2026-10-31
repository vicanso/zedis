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

//! Threshold alert rules and stateful sample evaluation.
//!
//! Pure data and logic (no GPUI, no i18n) so rules can be persisted in
//! `zedis-db` and unit-tested standalone. Alerts for a server are strictly
//! opt-in (off by default); callers check the per-server enable switch before
//! feeding samples to [`AlertEvaluator`].

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Metric inspected by an [`AlertRule`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AlertMetric {
    /// `used_memory / maxmemory * 100.0`. Evaluated only when `maxmemory > 0`.
    #[default]
    MemoryPercent,
    /// `blocked_clients` from `INFO clients`.
    BlockedClients,
    /// `connected_clients` from `INFO clients`.
    ConnectedClients,
    /// Heartbeat round-trip latency in milliseconds.
    LatencyMs,
    /// Rate of `evicted_keys` per second between consecutive samples.
    EvictedKeysPerSec,
    /// Maximum replica lag in megabytes (`lag_bytes / 1_048_576`).
    ReplicationLagMb,
    /// `instantaneous_ops_per_sec` from `INFO stats`.
    InstantaneousOps,
    /// `1.0` when the heartbeat marks the connection `Offline`, `0.0` otherwise.
    LinkOffline,
}

impl AlertMetric {
    /// Canonical display order for the alerts configuration dialog.
    pub const ALL: [AlertMetric; 8] = [
        AlertMetric::MemoryPercent,
        AlertMetric::BlockedClients,
        AlertMetric::ConnectedClients,
        AlertMetric::LatencyMs,
        AlertMetric::EvictedKeysPerSec,
        AlertMetric::ReplicationLagMb,
        AlertMetric::InstantaneousOps,
        AlertMetric::LinkOffline,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            AlertMetric::MemoryPercent => "memory_percent",
            AlertMetric::BlockedClients => "blocked_clients",
            AlertMetric::ConnectedClients => "connected_clients",
            AlertMetric::LatencyMs => "latency_ms",
            AlertMetric::EvictedKeysPerSec => "evicted_keys_per_sec",
            AlertMetric::ReplicationLagMb => "replication_lag_mb",
            AlertMetric::InstantaneousOps => "instantaneous_ops",
            AlertMetric::LinkOffline => "link_offline",
        }
    }

    /// Unit suffix shown next to the threshold input and in alert messages.
    pub fn unit(self) -> &'static str {
        match self {
            AlertMetric::MemoryPercent => "%",
            AlertMetric::BlockedClients | AlertMetric::ConnectedClients => "",
            AlertMetric::LatencyMs => "ms",
            AlertMetric::EvictedKeysPerSec | AlertMetric::InstantaneousOps => "/s",
            AlertMetric::ReplicationLagMb => "MB",
            AlertMetric::LinkOffline => "",
        }
    }

    /// Whether this metric takes a user-editable numeric threshold.
    /// `LinkOffline` is a boolean condition (`Offline` vs `Connected`).
    pub fn has_threshold(self) -> bool {
        !matches!(self, AlertMetric::LinkOffline)
    }

    /// Format an observed or threshold number for display.
    pub fn format_value(self, value: f64) -> String {
        match self {
            AlertMetric::MemoryPercent | AlertMetric::EvictedKeysPerSec | AlertMetric::ReplicationLagMb => {
                format!("{value:.1}{}", self.unit())
            }
            AlertMetric::BlockedClients
            | AlertMetric::ConnectedClients
            | AlertMetric::LatencyMs
            | AlertMetric::InstantaneousOps => {
                format!("{}{}", value.round() as i64, self.unit())
            }
            AlertMetric::LinkOffline => "offline".to_string(),
        }
    }
}

/// How an observed metric value is compared against a rule's threshold.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AlertComparator {
    /// Fires when `observed > threshold`.
    #[default]
    Above,
    /// Fires when `observed < threshold`.
    Below,
}

impl AlertComparator {
    pub fn symbol(self) -> &'static str {
        match self {
            AlertComparator::Above => ">",
            AlertComparator::Below => "<",
        }
    }

    pub fn matches(self, observed: f64, threshold: f64) -> bool {
        match self {
            AlertComparator::Above => observed > threshold,
            AlertComparator::Below => observed < threshold,
        }
    }
}

/// One threshold alert rule for a server.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AlertRule {
    pub metric: AlertMetric,
    pub enabled: bool,
    pub comparator: AlertComparator,
    pub threshold: f64,
    /// How long (in seconds) the breach must hold continuously before firing.
    /// `0` fires on the first breaching sample.
    pub sustained_secs: u64,
    /// Minimum seconds between two notifications for the same metric.
    pub cooldown_secs: u64,
}

impl Default for AlertRule {
    fn default() -> Self {
        Self {
            metric: AlertMetric::MemoryPercent,
            enabled: true,
            comparator: AlertComparator::Above,
            threshold: 85.0,
            sustained_secs: 10,
            cooldown_secs: 300,
        }
    }
}

/// Default rule set for a server when the user enables alerts.
pub fn default_alert_rules() -> Vec<AlertRule> {
    vec![
        AlertRule {
            metric: AlertMetric::MemoryPercent,
            enabled: true,
            comparator: AlertComparator::Above,
            threshold: 85.0,
            sustained_secs: 10,
            cooldown_secs: 300,
        },
        AlertRule {
            metric: AlertMetric::BlockedClients,
            enabled: true,
            comparator: AlertComparator::Above,
            threshold: 10.0,
            sustained_secs: 10,
            cooldown_secs: 300,
        },
        AlertRule {
            metric: AlertMetric::ConnectedClients,
            enabled: false,
            comparator: AlertComparator::Above,
            threshold: 1000.0,
            sustained_secs: 10,
            cooldown_secs: 300,
        },
        AlertRule {
            metric: AlertMetric::LatencyMs,
            enabled: true,
            comparator: AlertComparator::Above,
            threshold: 200.0,
            sustained_secs: 10,
            cooldown_secs: 300,
        },
        AlertRule {
            metric: AlertMetric::EvictedKeysPerSec,
            enabled: true,
            comparator: AlertComparator::Above,
            threshold: 0.0,
            sustained_secs: 0,
            cooldown_secs: 300,
        },
        AlertRule {
            metric: AlertMetric::ReplicationLagMb,
            enabled: true,
            comparator: AlertComparator::Above,
            threshold: 10.0,
            sustained_secs: 10,
            cooldown_secs: 300,
        },
        AlertRule {
            metric: AlertMetric::InstantaneousOps,
            enabled: false,
            comparator: AlertComparator::Above,
            threshold: 50_000.0,
            sustained_secs: 10,
            cooldown_secs: 300,
        },
        AlertRule {
            metric: AlertMetric::LinkOffline,
            enabled: true,
            comparator: AlertComparator::Above,
            threshold: 0.5,
            sustained_secs: 0,
            cooldown_secs: 120,
        },
    ]
}

/// Merge persisted `stored` rules onto [`default_alert_rules`] in canonical
/// [`AlertMetric::ALL`] order so newly added metrics appear automatically and
/// missing rows never break the UI.
pub fn normalize_alert_rules(stored: &[AlertRule]) -> Vec<AlertRule> {
    let by_metric: HashMap<AlertMetric, &AlertRule> = stored.iter().map(|r| (r.metric, r)).collect();
    default_alert_rules()
        .into_iter()
        .map(|default_rule| {
            by_metric
                .get(&default_rule.metric)
                .map(|saved| (*saved).clone())
                .unwrap_or(default_rule)
        })
        .collect()
}

/// Point-in-time server observation fed into [`AlertEvaluator::evaluate`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AlertSample {
    pub timestamp_ms: i64,
    pub used_memory: u64,
    pub maxmemory: u64,
    pub connected_clients: u64,
    pub blocked_clients: u64,
    pub max_replica_lag_bytes: u64,
    pub evicted_keys: u64,
    pub instantaneous_ops_per_sec: u64,
    pub latency_ms: u64,
    pub link_offline: bool,
}

impl AlertSample {
    /// Extract the numeric value for `metric`, or `None` when the sample
    /// cannot answer it (e.g. `MemoryPercent` when `maxmemory == 0`, or
    /// `EvictedKeysPerSec` on the very first sample).
    pub fn value_for(&self, metric: AlertMetric, prev: Option<&AlertSample>) -> Option<f64> {
        if self.link_offline && metric != AlertMetric::LinkOffline {
            return None;
        }
        match metric {
            AlertMetric::MemoryPercent => {
                if self.maxmemory == 0 {
                    None
                } else {
                    Some((self.used_memory as f64 / self.maxmemory as f64) * 100.0)
                }
            }
            AlertMetric::BlockedClients => Some(self.blocked_clients as f64),
            AlertMetric::ConnectedClients => Some(self.connected_clients as f64),
            AlertMetric::LatencyMs => Some(self.latency_ms as f64),
            AlertMetric::EvictedKeysPerSec => {
                let prev = prev?;
                if prev.link_offline || self.timestamp_ms <= prev.timestamp_ms {
                    return None;
                }
                let delta_keys = self.evicted_keys.saturating_sub(prev.evicted_keys);
                let elapsed_secs = (self.timestamp_ms - prev.timestamp_ms) as f64 / 1000.0;
                if elapsed_secs <= 0.0 {
                    None
                } else {
                    Some(delta_keys as f64 / elapsed_secs)
                }
            }
            AlertMetric::ReplicationLagMb => Some(self.max_replica_lag_bytes as f64 / (1024.0 * 1024.0)),
            AlertMetric::InstantaneousOps => Some(self.instantaneous_ops_per_sec as f64),
            AlertMetric::LinkOffline => Some(if self.link_offline { 1.0 } else { 0.0 }),
        }
    }
}

/// An alert that crossed its threshold and sustained duration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TriggeredAlert {
    pub metric: AlertMetric,
    pub comparator: AlertComparator,
    pub threshold: f64,
    pub observed: f64,
    pub triggered_at_ms: i64,
}

#[derive(Debug, Clone, Default)]
struct RuleState {
    breach_started_ms: Option<i64>,
    last_fired_ms: Option<i64>,
}

/// Stateful per-server evaluator tracking sustained durations and cooldowns.
#[derive(Debug, Clone, Default)]
pub struct AlertEvaluator {
    states: HashMap<AlertMetric, RuleState>,
    prev_sample: Option<AlertSample>,
    active_alerts: Vec<TriggeredAlert>,
}

impl AlertEvaluator {
    pub fn new() -> Self {
        Self::default()
    }

    /// Reset all state (e.g. on server switch or when alerts are disabled).
    pub fn reset(&mut self) {
        self.states.clear();
        self.prev_sample = None;
        self.active_alerts.clear();
    }

    /// Alerts whose condition is *currently* breaching past their sustained
    /// duration on the latest sample.
    pub fn active_alerts(&self) -> &[TriggeredAlert] {
        &self.active_alerts
    }

    /// Evaluate `sample` against `rules`. Returns only the alerts that should
    /// **fire a notification on this tick** (i.e. sustained duration met and
    /// cooldown elapsed), while [`Self::active_alerts`] holds all currently
    /// active breaches.
    pub fn evaluate(&mut self, rules: &[AlertRule], sample: AlertSample) -> Vec<TriggeredAlert> {
        let mut fired = Vec::new();
        let mut currently_active = Vec::new();
        let prev = self.prev_sample.as_ref();

        for rule in rules {
            if !rule.enabled {
                self.states.remove(&rule.metric);
                continue;
            }
            let Some(observed) = sample.value_for(rule.metric, prev) else {
                // If link went offline, non-link metrics are unknown rather
                // than cleared, except LinkOffline which evaluates above.
                if !sample.link_offline {
                    self.states.remove(&rule.metric);
                }
                continue;
            };

            if rule.comparator.matches(observed, rule.threshold) {
                let state = self.states.entry(rule.metric).or_default();
                let started_ms = *state.breach_started_ms.get_or_insert(sample.timestamp_ms);
                let sustained_ms = (rule.sustained_secs as i64).saturating_mul(1000);
                if sample.timestamp_ms.saturating_sub(started_ms) >= sustained_ms {
                    let alert = TriggeredAlert {
                        metric: rule.metric,
                        comparator: rule.comparator,
                        threshold: rule.threshold,
                        observed,
                        triggered_at_ms: sample.timestamp_ms,
                    };
                    currently_active.push(alert.clone());

                    let cooldown_ms = (rule.cooldown_secs as i64).saturating_mul(1000);
                    let cooldown_elapsed = state
                        .last_fired_ms
                        .is_none_or(|last| sample.timestamp_ms.saturating_sub(last) >= cooldown_ms);
                    if cooldown_elapsed {
                        state.last_fired_ms = Some(sample.timestamp_ms);
                        fired.push(alert);
                    }
                }
            } else if let Some(state) = self.states.get_mut(&rule.metric) {
                // Breach ended: clear `breach_started_ms` so the next breach
                // must sustain again, but keep `last_fired_ms` so flapping
                // around the threshold still respects the cooldown.
                state.breach_started_ms = None;
            }
        }

        if !sample.link_offline {
            self.prev_sample = Some(sample);
        }
        self.active_alerts = currently_active;
        fired
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn memory_rule(threshold: f64, sustained_secs: u64, cooldown_secs: u64) -> AlertRule {
        AlertRule {
            metric: AlertMetric::MemoryPercent,
            enabled: true,
            comparator: AlertComparator::Above,
            threshold,
            sustained_secs,
            cooldown_secs,
        }
    }

    #[test]
    fn memory_percent_requires_maxmemory_and_sustained_duration_and_cooldown() {
        let mut eval = AlertEvaluator::new();
        let rules = vec![memory_rule(80.0, 10, 60)];

        // maxmemory == 0: never fires even if used_memory is large.
        let s0 = AlertSample {
            timestamp_ms: 1_000,
            used_memory: 900,
            maxmemory: 0,
            ..Default::default()
        };
        assert!(eval.evaluate(&rules, s0).is_empty());
        assert!(eval.active_alerts().is_empty());

        // Breach starts at t=2s (90% > 80%), sustained_secs = 10s -> not yet fired.
        let s1 = AlertSample {
            timestamp_ms: 2_000,
            used_memory: 900,
            maxmemory: 1_000,
            ..Default::default()
        };
        assert!(eval.evaluate(&rules, s1).is_empty());
        assert!(eval.active_alerts().is_empty());

        // At t=11s (9s elapsed < 10s): still waiting.
        let s2 = AlertSample {
            timestamp_ms: 11_000,
            used_memory: 900,
            maxmemory: 1_000,
            ..Default::default()
        };
        assert!(eval.evaluate(&rules, s2).is_empty());

        // At t=12s (10s elapsed): fires once and becomes active!
        let s3 = AlertSample {
            timestamp_ms: 12_000,
            used_memory: 920,
            maxmemory: 1_000,
            ..Default::default()
        };
        let fired = eval.evaluate(&rules, s3);
        assert_eq!(fired.len(), 1);
        assert_eq!(fired[0].metric, AlertMetric::MemoryPercent);
        assert!((fired[0].observed - 92.0).abs() < 0.01);
        assert_eq!(eval.active_alerts().len(), 1);

        // At t=30s (still breaching, inside 60s cooldown): stays active, does not re-fire.
        let s4 = AlertSample {
            timestamp_ms: 30_000,
            used_memory: 930,
            maxmemory: 1_000,
            ..Default::default()
        };
        assert!(eval.evaluate(&rules, s4).is_empty());
        assert_eq!(eval.active_alerts().len(), 1);

        // Drops below threshold at t=40s: active alerts clear.
        let s5 = AlertSample {
            timestamp_ms: 40_000,
            used_memory: 500,
            maxmemory: 1_000,
            ..Default::default()
        };
        assert!(eval.evaluate(&rules, s5).is_empty());
        assert!(eval.active_alerts().is_empty());

        // Re-breaches at t=75s, sustained until t=85s (after 60s cooldown from t=12s): fires again.
        let s6 = AlertSample {
            timestamp_ms: 75_000,
            used_memory: 850,
            maxmemory: 1_000,
            ..Default::default()
        };
        assert!(eval.evaluate(&rules, s6).is_empty());
        let s7 = AlertSample {
            timestamp_ms: 85_000,
            used_memory: 860,
            maxmemory: 1_000,
            ..Default::default()
        };
        assert_eq!(eval.evaluate(&rules, s7).len(), 1);
    }

    #[test]
    fn evicted_keys_rate_and_link_offline_fire_immediately_when_sustained_zero() {
        let mut eval = AlertEvaluator::new();
        let rules = default_alert_rules();

        // First sample establishes baseline for evicted_keys.
        let s1 = AlertSample {
            timestamp_ms: 10_000,
            evicted_keys: 100,
            ..Default::default()
        };
        assert!(eval.evaluate(&rules, s1).is_empty());

        // Second sample 2s later has 106 evicted_keys (3.0/s > 0.0/s).
        let s2 = AlertSample {
            timestamp_ms: 12_000,
            evicted_keys: 106,
            ..Default::default()
        };
        let fired = eval.evaluate(&rules, s2);
        assert_eq!(fired.len(), 1);
        assert_eq!(fired[0].metric, AlertMetric::EvictedKeysPerSec);
        assert!((fired[0].observed - 3.0).abs() < 0.01);

        // Link goes offline at t=14s: LinkOffline fires immediately.
        let s3 = AlertSample {
            timestamp_ms: 14_000,
            link_offline: true,
            ..Default::default()
        };
        let fired_offline = eval.evaluate(&rules, s3);
        assert_eq!(fired_offline.len(), 1);
        assert_eq!(fired_offline[0].metric, AlertMetric::LinkOffline);
    }

    #[test]
    fn normalize_preserves_saved_overrides_in_canonical_order() {
        let custom = vec![AlertRule {
            metric: AlertMetric::LatencyMs,
            enabled: false,
            comparator: AlertComparator::Above,
            threshold: 500.0,
            sustained_secs: 30,
            cooldown_secs: 600,
        }];
        let normalized = normalize_alert_rules(&custom);
        assert_eq!(normalized.len(), AlertMetric::ALL.len());
        let latency = normalized
            .iter()
            .find(|r| r.metric == AlertMetric::LatencyMs)
            .expect("latency rule present");
        assert!(!latency.enabled);
        assert!((latency.threshold - 500.0).abs() < f64::EPSILON);
        assert_eq!(latency.sustained_secs, 30);
    }
}
