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

//! What a connect learned about the server, and what the feature probe and
//! refused commands taught since: its version, its nodes, its databases,
//! its modules, the commands it offers, and a Sentinel's masters.

use super::*;

#[derive(Debug, Clone, Copy, Default)]
pub(super) struct ModuleFlags {
    pub rejson: bool,
    pub search: bool,
    pub timeseries: bool,
    pub bf: bool,
}

#[derive(Debug, Clone, Default)]
pub(super) struct ServerFacts {
    /// Redis server version string
    version: SharedString,

    /// Number of Redis nodes (master, replica) for cluster info
    nodes: (usize, usize),

    /// Description of the nodes
    nodes_description: Arc<RedisClientDescription>,

    /// Total number of databases
    databases: usize,

    /// Whether the server supports ReJSON module
    supports_rejson: bool,

    /// Whether the server has the RediSearch module loaded. Gates the
    /// "Search" entry in the Tools dropdown so unrelated servers don't
    /// surface a useless menu item.
    supports_search: bool,

    /// Whether the server has RedisTimeSeries loaded.
    supports_timeseries: bool,

    /// Whether the server has RedisBloom (`bf`) loaded.
    supports_bf: bool,

    /// Which commands this server offers the connected user — probed in the
    /// background after connect, cached per server (shared across dbs), and
    /// refined by runtime `NOPERM` / `unknown command` replies. Drives panel
    /// placeholders, disabled menu entries and per-section chips.
    features: Arc<ServerFeatures>,

    /// Sentinel only: the monitored masters as the sentinels describe them
    /// (quorum, timing, down flags) — see `server/sentinel.rs`.
    #[cfg(not(target_family = "wasm"))]
    sentinel_masters: Vec<SentinelMaster>,
}

impl ServerFacts {
    /// What a server switch forgets: the version and the topology. The
    /// databases, the modules and the feature matrix stay until `select`
    /// and its connect set them again.
    pub(super) fn reset(&mut self) {
        self.version = SharedString::default();
        self.nodes = (0, 0);
        self.nodes_description = Arc::new(RedisClientDescription::default());
        #[cfg(not(target_family = "wasm"))]
        self.sentinel_masters.clear();
    }

    /// The nodes a connect or a topology reload found.
    pub(super) fn set_topology(&mut self, nodes: (usize, usize), description: RedisClientDescription) {
        self.nodes = nodes;
        self.nodes_description = Arc::new(description);
    }

    /// What else a connect learned.
    pub(super) fn note_connect(&mut self, version: SharedString, databases: usize, modules: ModuleFlags) {
        self.version = version;
        self.databases = databases;
        self.supports_rejson = modules.rejson;
        self.supports_search = modules.search;
        self.supports_timeseries = modules.timeseries;
        self.supports_bf = modules.bf;
    }

    pub(super) fn set_features(&mut self, features: Arc<ServerFeatures>) {
        self.features = features;
    }

    /// The probed feature matrix (optimistic until probed).
    pub(super) fn feature_matrix(&self) -> &ServerFeatures {
        &self.features
    }

    pub(super) fn features(&self) -> Arc<ServerFeatures> {
        self.features.clone()
    }

    /// Why `cap` is blocked on the server side, if it is.
    pub(super) fn blocked_by(&self, cap: Capability) -> Option<(ServerCommand, CommandStatus)> {
        cap.blocked_by(&self.features)
    }

    /// The first command `view` cannot function without that this server
    /// lacks.
    pub(super) fn panel_block(&self, view: ServerView) -> Option<(ServerCommand, CommandStatus)> {
        self.features.first_unusable(view.required_commands())
    }

    /// Why `command` is unusable here, if it is.
    pub(super) fn command_block(&self, command: ServerCommand) -> Option<CommandStatus> {
        let status = self.features.status(command);
        (!status.is_usable()).then_some(status)
    }

    pub(super) fn nodes(&self) -> (usize, usize) {
        self.nodes
    }

    pub(super) fn nodes_description(&self) -> Arc<RedisClientDescription> {
        self.nodes_description.clone()
    }

    pub(super) fn version(&self) -> &str {
        &self.version
    }

    /// The one version gate — see [`ZedisServerState::supports`].
    pub(super) fn supports(&self, floor: Floor) -> bool {
        use semver::Version;
        Version::parse(self.version.as_ref())
            .map(|v| floor.met_by(self.nodes_description.is_valkey, &v))
            .unwrap_or(false)
    }

    pub(super) fn databases(&self) -> usize {
        self.databases
    }

    pub(super) fn supports_rejson(&self) -> bool {
        self.supports_rejson
    }

    pub(super) fn supports_search(&self) -> bool {
        self.supports_search
    }

    pub(super) fn supports_timeseries(&self) -> bool {
        self.supports_timeseries
    }

    pub(super) fn supports_bf(&self) -> bool {
        self.supports_bf
    }

    #[cfg(not(target_family = "wasm"))]
    pub(super) fn sentinel_masters(&self) -> &[SentinelMaster] {
        &self.sentinel_masters
    }

    #[cfg(not(target_family = "wasm"))]
    pub(super) fn set_sentinel_masters(&mut self, masters: Vec<SentinelMaster>) {
        self.sentinel_masters = masters;
    }
}
