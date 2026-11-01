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

//! What the client is: the server it reached (product, version, flavor, the
//! floors it meets), its nodes, and the slot map and topology the views draw.

use super::*;

impl RedisClient {
    pub fn nodes(&self) -> (usize, usize) {
        (self.master_nodes.len(), self.nodes.len())
    }

    pub fn version(&self) -> String {
        self.version.to_string()
    }

    /// The connect found `INFO` denied or missing: the version is unknown,
    /// the heartbeat probes with `PING`, and there are no `INFO` texts to
    /// fan out for.
    pub fn info_unavailable(&self) -> bool {
        self.info_unavailable
    }

    /// Whether a save may say `SET … KEEPTTL`: the server has the option
    /// ([`floors::SET_KEEPTTL`]) and keeps the key when it is used
    /// ([`floors::set_keepttl_is_safe`] — Garnet does not).
    pub fn keeps_ttl_on_set(&self) -> bool {
        self.supports(floors::SET_KEEPTTL) && floors::set_keepttl_is_safe(self.flavor)
    }

    /// Whether the server is a Valkey — what a flavor-dependent command
    /// shape (atomic slot migration) is chosen by.
    pub fn is_valkey(&self) -> bool {
        self.is_valkey
    }

    /// What the server is, as a person names it: `Valkey 9.0.0`, `Redis
    /// 8.6.1`, `Dragonfly 2.0.2` — the product's own version, not the one it
    /// claims for Redis's sake.
    pub fn product(&self) -> String {
        let version = if self.flavor_version.is_empty() {
            self.version()
        } else {
            self.flavor_version.clone()
        };
        format!("{} {version}", self.flavor.label()).trim_end().to_string()
    }

    pub fn databases(&self) -> usize {
        self.databases
    }

    pub fn access_mode(&self) -> AccessMode {
        self.access_mode
    }

    /// Returns the list of master node server configurations.
    pub fn master_servers(&self) -> Vec<RedisServer> {
        self.master_nodes.iter().map(|node| node.server.clone()).collect()
    }

    /// Every node's `host:port`, masters and replicas — the keys of the
    /// per-node connection pool, which the browser build does not have.
    #[cfg(not(target_family = "wasm"))]
    pub(crate) fn node_addrs(&self) -> Vec<String> {
        self.master_nodes
            .iter()
            .chain(&self.nodes)
            .map(|node| node.host_port())
            .collect()
    }

    pub fn nodes_description(&self) -> RedisClientDescription {
        let master_nodes: Vec<String> = self.master_nodes.iter().map(|node| node.host_port()).collect();
        let slave_nodes: Vec<String> = self
            .nodes
            .iter()
            .filter(|node| !master_nodes.contains(&node.host_port()))
            .map(|node| node.host_port().clone())
            .collect();
        let modules = self
            .modules
            .iter()
            .map(|(name, ver)| format!("{name}@{ver}"))
            .collect::<Vec<_>>()
            .join(", ");
        let topology = self.build_topology();
        let slot_map = self.build_slot_map();
        RedisClientDescription {
            is_valkey: self.is_valkey,
            flavor: self.flavor,
            flavor_version: self.flavor_version.clone(),
            server_type: format!("{:?}", self.server_type),
            master_nodes: master_nodes.join(","),
            slave_nodes: slave_nodes.join(","),
            modules,
            topology,
            slot_map,
            sentinel_master_names: self.sentinel_master_names.clone(),
        }
    }

    /// Build the structured slot map consumed by Topology's slot bar and
    /// reshard planner. Only populated in cluster mode.
    #[cfg(not(target_family = "wasm"))]
    fn build_slot_map(&self) -> ClusterSlotMap {
        if self.server_type != ServerType::Cluster {
            return ClusterSlotMap::default();
        }

        // Master order is the stable colour index source of truth.
        let masters: Vec<ClusterMasterSlotSummary> = self
            .master_nodes
            .iter()
            .enumerate()
            .filter_map(|(idx, m)| {
                let id = m.cluster_id.clone()?;
                let slot_count = m
                    .slots
                    .iter()
                    .map(|(lo, hi)| u32::from(hi.saturating_sub(*lo).saturating_add(1)))
                    .sum();
                Some(ClusterMasterSlotSummary {
                    node_id: id,
                    addr: m.host_port(),
                    slot_count,
                    color_index: idx,
                })
            })
            .collect();

        let id_to_addr: HashMap<String, String> = self
            .nodes
            .iter()
            .filter_map(|n| n.cluster_id.clone().map(|id| (id, n.host_port())))
            .collect();
        let id_to_color: HashMap<String, usize> =
            masters.iter().map(|m| (m.node_id.to_string(), m.color_index)).collect();

        // Expand every owned range into ClusterSlotRange segments, then
        // merge contiguous same-owner ranges so the bar isn't 16k pieces.
        let mut flat: Vec<(u16, u16, String, String, usize)> = Vec::new();
        for m in self.master_nodes.iter() {
            let Some(id) = m.cluster_id.as_ref() else {
                continue;
            };
            let color = id_to_color.get(id).copied().unwrap_or(0);
            let addr = m.host_port();
            for &(lo, hi) in &m.slots {
                flat.push((lo, hi, id.clone(), addr.clone(), color));
            }
        }
        flat.sort_by_key(|(lo, _, _, _, _)| *lo);

        let mut owners: Vec<ClusterSlotRange> = Vec::new();
        for (lo, hi, id, addr, color) in flat {
            if let Some(last) = owners.last_mut()
                && last.node_id.as_str() == id
                && last.end.saturating_add(1) == lo
            {
                last.end = hi;
                continue;
            }
            owners.push(ClusterSlotRange {
                start: lo,
                end: hi,
                node_id: id,
                addr,
                color_index: color,
            });
        }

        let assigned_slots = owners
            .iter()
            .map(|r| u32::from(r.end.saturating_sub(r.start).saturating_add(1)))
            .sum();

        // Pair migrating (source) with importing (target) by slot.
        // Either side alone is enough to surface the in-flight slot.
        #[derive(Default)]
        struct PairSides {
            source: Option<(String, String)>,
            target: Option<(String, String)>,
        }
        let mut by_slot: HashMap<u16, PairSides> = HashMap::new();
        for node in self.nodes.iter() {
            let Some(self_id) = node.cluster_id.as_ref() else {
                continue;
            };
            let self_addr = node.host_port();
            for m in &node.migrations {
                let entry = by_slot.entry(m.slot).or_default();
                match m.kind {
                    SlotMigrationKind::Migrating => {
                        entry.source = Some((self_id.clone(), self_addr.clone()));
                        // peer is target; fill target side if still empty
                        if entry.target.is_none() {
                            let t_addr = id_to_addr.get(&m.peer_id).cloned().unwrap_or_default();
                            entry.target = Some((m.peer_id.clone(), t_addr));
                        }
                    }
                    SlotMigrationKind::Importing => {
                        entry.target = Some((self_id.clone(), self_addr.clone()));
                        if entry.source.is_none() {
                            let s_addr = id_to_addr.get(&m.peer_id).cloned().unwrap_or_default();
                            entry.source = Some((m.peer_id.clone(), s_addr));
                        }
                    }
                }
            }
        }

        let mut migrations: Vec<ClusterMigrationEntry> = by_slot
            .into_iter()
            .map(|(slot, sides)| {
                let (source_id, source_addr) = sides.source.unwrap_or_default();
                let (target_id, target_addr) = sides.target.unwrap_or_default();
                ClusterMigrationEntry {
                    slot,
                    source_id,
                    source_addr,
                    target_id,
                    target_addr,
                }
            })
            .collect();
        migrations.sort_by_key(|m| m.slot);

        ClusterSlotMap {
            owners,
            migrations,
            masters,
            assigned_slots,
        }
    }

    #[cfg(target_family = "wasm")]
    fn build_slot_map(&self) -> ClusterSlotMap {
        ClusterSlotMap::default()
    }

    /// Build the structured topology consumed by the status-bar tooltip.
    ///
    /// Cluster mode groups replicas under their master via `master_cluster_id`
    /// and annotates the master with its slot ranges. Sentinel groups by
    /// `master_name`. Standalone returns an empty list so the caller falls
    /// back to its existing flat summary.
    fn build_topology(&self) -> Vec<TopologyMaster> {
        let role_marker = |role: &NodeRole| -> String {
            match role {
                NodeRole::Master => "●",
                NodeRole::Slave => "↳",
                NodeRole::Fail => "✗",
                #[cfg(not(target_family = "wasm"))]
                NodeRole::Unknown => "?",
            }
            .into()
        };
        // A failed node keeps its role flag, so the cross comes from its
        // health; a merely suspected one keeps its role glyph and is
        // counted apart by the page.
        let marker = |role: &NodeRole, health: NodeHealth| -> String {
            if health == NodeHealth::Failing {
                "✗".into()
            } else {
                role_marker(role)
            }
        };
        let format_slots = |slots: &[(u16, u16)]| -> String {
            if slots.is_empty() {
                return String::new();
            }
            let parts: Vec<String> = slots
                .iter()
                .map(|(lo, hi)| if lo == hi { lo.to_string() } else { format!("{lo}-{hi}") })
                .collect();
            format!("slots {}", parts.join(","))
        };

        let mut out: Vec<TopologyMaster> = Vec::new();

        match self.server_type {
            ServerType::Cluster => {
                for master in self.master_nodes.iter() {
                    let entry = TopologyEntry {
                        addr: master.host_port(),
                        role_marker: marker(&master.role, master.health),
                        health: master.health,
                        annotation: format_slots(&master.slots),
                        node_id: master.cluster_id.clone().unwrap_or_default(),
                        master_name: String::new(),
                    };
                    let replicas: Vec<TopologyEntry> = if let Some(master_id) = master.cluster_id.as_ref() {
                        self.nodes
                            .iter()
                            .filter(|n| {
                                n.master_cluster_id.as_ref() == Some(master_id)
                                    && (n.role == NodeRole::Slave || n.role == NodeRole::Fail)
                            })
                            .map(|replica| TopologyEntry {
                                addr: replica.host_port(),
                                role_marker: marker(&replica.role, replica.health),
                                health: replica.health,
                                annotation: String::new(),
                                node_id: replica.cluster_id.clone().unwrap_or_default(),
                                master_name: String::new(),
                            })
                            .collect()
                    } else {
                        Vec::new()
                    };
                    out.push(TopologyMaster {
                        master: entry,
                        replicas,
                    });
                }
            }
            ServerType::Sentinel => {
                for master in self.master_nodes.iter() {
                    let label = master.master_name.as_deref().unwrap_or("");
                    // node_id is left empty in sentinel mode — CLUSTER FORGET /
                    // REPLICATE don't apply. master_name is populated only on
                    // the master row (replicas don't directly receive
                    // SENTINEL ops, which all target by master name).
                    let entry = TopologyEntry {
                        addr: master.host_port(),
                        role_marker: marker(&master.role, master.health),
                        health: master.health,
                        annotation: if label.is_empty() {
                            String::new()
                        } else {
                            format!("({label})")
                        },
                        node_id: String::new(),
                        master_name: label.to_string(),
                    };
                    let replicas: Vec<TopologyEntry> = self
                        .nodes
                        .iter()
                        .filter(|n| n.role == NodeRole::Slave && n.master_name.as_deref() == Some(label))
                        .map(|replica| TopologyEntry {
                            addr: replica.host_port(),
                            role_marker: marker(&replica.role, replica.health),
                            health: replica.health,
                            annotation: String::new(),
                            node_id: String::new(),
                            master_name: String::new(),
                        })
                        .collect();
                    out.push(TopologyMaster {
                        master: entry,
                        replicas,
                    });
                }
            }
            ServerType::Standalone => {
                // No replicas surfaced from a single Standalone connection — let
                // the caller fall back to its existing summary.
            }
        }

        out
    }

    /// Checks if the client is a cluster client.
    /// # Returns
    /// * `bool` - True if the client is a cluster client, false otherwise.
    pub fn is_cluster(&self) -> bool {
        self.server_type == ServerType::Cluster
    }

    /// The one version gate: both flavors decided by a [`Floor`] from
    /// `floors.rs` (Valkey's version numbers run ahead of Redis's, so a
    /// bare floor written for Redis is wrong on Valkey).
    pub fn supports(&self, floor: Floor) -> bool {
        floor.met_by(self.is_valkey, &self.version)
    }

    /// `SET … IFEQ` compare-and-set — see [`floors::SET_IFEQ`].
    pub fn supports_set_ifeq(&self) -> bool {
        self.supports(floors::SET_IFEQ)
    }

    /// `INFO keysizes` — see [`floors::INFO_KEYSIZES`].
    pub fn supports_info_keysizes(&self) -> bool {
        self.supports(floors::INFO_KEYSIZES)
    }

    /// Whether a JSON module is loaded: RedisJSON lists itself as `ReJSON`,
    /// valkey-json as `json`. Both answer the same `JSON.*` commands and
    /// type their keys `ReJSON-RL`, so everything past this test is shared.
    pub fn supports_rejson(&self) -> bool {
        self.modules.iter().any(|(name, _)| name == "ReJSON" || name == "json")
    }

    /// Whether a search module is loaded. RediSearch (every 1.x → 2.x) and
    /// valkey-search both list themselves as `search`; what differs is the
    /// `FT.*` surface behind the name — valkey-search has `FT.CREATE` /
    /// `FT.SEARCH` / `FT.AGGREGATE` / `FT.INFO` / `FT._LIST` / `FT.DROPINDEX`
    /// and none of `TAGVALS`, `SPELLCHECK`, `ALTER`, `EXPLAIN`, `PROFILE`,
    /// which the panel learns at run time the way it learns any missing
    /// command. Drives the visibility of the Search entry in the Tools menu.
    pub fn supports_search(&self) -> bool {
        self.modules.iter().any(|(name, _)| name == "search")
    }

    /// Returns the number of master nodes.
    /// # Returns
    /// * `usize` - The number of master nodes.
    pub fn count_masters(&self) -> Result<usize> {
        Ok(self.master_nodes.len())
    }
}
