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

//! Where the app is: the top-level [`Route`] and, inside a server, the
//! [`ServerView`] panel — with their names, icons and the commands a panel
//! cannot do without.

use super::*;

/// Top-level navigation target — the runtime single source of truth for
/// "where am I", including the active connection: a connection-scoped page is
/// only representable together with its `(id, db)`. App-scoped pages
/// (`Home` / `Settings` / `Protos` / `Scripts`) stand alone.
#[derive(Debug, Clone, Default, PartialEq)]
pub enum Route {
    #[default]
    Home,
    Settings,
    Protos,
    Scripts,
    Server {
        id: SharedString,
        db: usize,
        view: ServerView,
    },
}

/// A connection-scoped page, rendered against the active `selected_server`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ServerView {
    #[default]
    Editor,
    Metrics,
    Slowlog,
    MemoryAnalysis,
    Clients,
    Monitor,
    Config,
    Acl,
    Search,
    Functions,
    LuaScripts,
    Persistence,
    KeyspaceNotifications,
    Topology,
    ServerLoad,
    /// `HOTKEYS` tracking (Redis 8.6) — top keys by CPU time / network bytes.
    Hotkeys,
    ValueSearch,
    /// Raw `INFO everything` browser — every field, filterable, for the
    /// long tail the structured panels don't surface.
    ServerInfo,
    /// `TS.MRANGE` explorer — many time series at once, selected by label
    /// rather than by key. The single-key chart answers "what did this do";
    /// this answers "what did all of these do", which is the question a
    /// label-organised series set exists for.
    TimeSeriesExplorer,
}

impl Route {
    /// Stable lowercase name used for persistence (and, later, deep links).
    pub fn as_str(&self) -> &'static str {
        match self {
            Route::Home => "home",
            Route::Settings => "settings",
            Route::Protos => "protos",
            Route::Scripts => "scripts",
            Route::Server { view, .. } => view.as_str(),
        }
    }
    /// Parse an app-level route name (case-insensitive). Connection-scoped
    /// names go through `ServerView::from_name` instead — they can't stand
    /// alone as a `Route` without an `(id, db)`.
    pub fn app_from_name(s: &str) -> Option<Route> {
        Some(match s.trim().to_ascii_lowercase().as_str() {
            "home" => Route::Home,
            "settings" => Route::Settings,
            "protos" => Route::Protos,
            "scripts" => Route::Scripts,
            _ => return None,
        })
    }
    /// The connection-scoped view, if this is a server route.
    pub fn server_view(&self) -> Option<ServerView> {
        match self {
            Route::Server { view, .. } => Some(*view),
            _ => None,
        }
    }
    /// The `(id, db)` this route renders against, if it is a server route.
    pub fn server(&self) -> Option<(SharedString, usize)> {
        match self {
            Route::Server { id, db, .. } => Some((id.clone(), *db)),
            _ => None,
        }
    }
    pub fn is_server(&self) -> bool {
        matches!(self, Route::Server { .. })
    }
}

impl ServerView {
    /// Stable lowercase name (matches the lowercased legacy variant name).
    pub fn as_str(&self) -> &'static str {
        match self {
            ServerView::Editor => "editor",
            ServerView::Metrics => "metrics",
            ServerView::Slowlog => "slowlog",
            ServerView::MemoryAnalysis => "memoryanalysis",
            ServerView::Clients => "clients",
            ServerView::Monitor => "monitor",
            ServerView::Config => "config",
            ServerView::Acl => "acl",
            ServerView::Search => "search",
            ServerView::Functions => "functions",
            ServerView::LuaScripts => "luascripts",
            ServerView::Persistence => "persistence",
            ServerView::KeyspaceNotifications => "keyspacenotifications",
            ServerView::Topology => "topology",
            ServerView::ServerLoad => "serverload",
            ServerView::Hotkeys => "hotkeys",
            ServerView::ValueSearch => "valuesearch",
            ServerView::ServerInfo => "serverinfo",
            ServerView::TimeSeriesExplorer => "timeseriesexplorer",
        }
    }
    /// The probed commands this panel cannot function without — when one is
    /// missing or denied on the server, the route renders a placeholder
    /// instead of the panel. Panels that still have something to offer
    /// without the server (the memory analyzer's offline RDB mode, the local
    /// Lua library) or that are gated elsewhere (Search by module, Topology
    /// by server type) list nothing here and degrade section by section.
    pub const fn required_commands(self) -> &'static [ServerCommand] {
        match self {
            ServerView::Metrics | ServerView::Persistence | ServerView::ServerLoad | ServerView::ServerInfo => {
                &[ServerCommand::Info]
            }
            ServerView::Slowlog => &[ServerCommand::SlowlogGet],
            ServerView::Hotkeys => &[ServerCommand::HotkeysGet],
            ServerView::Clients => &[ServerCommand::ClientList],
            ServerView::Monitor => &[ServerCommand::Monitor],
            ServerView::Config => &[ServerCommand::ConfigGet],
            ServerView::Acl => &[ServerCommand::AclList],
            ServerView::Functions => &[ServerCommand::FunctionList],
            ServerView::KeyspaceNotifications => &[ServerCommand::Subscribe],
            ServerView::ValueSearch => &[ServerCommand::Scan],
            ServerView::TimeSeriesExplorer => &[ServerCommand::TsMRange],
            ServerView::Editor
            | ServerView::MemoryAnalysis
            | ServerView::Search
            | ServerView::LuaScripts
            | ServerView::Topology => &[],
        }
    }

    /// Parse a connection-scoped view name (expects an already-lowercased str).
    pub fn from_name(s: &str) -> Option<ServerView> {
        Some(match s {
            "editor" => ServerView::Editor,
            "metrics" => ServerView::Metrics,
            "slowlog" => ServerView::Slowlog,
            "memoryanalysis" => ServerView::MemoryAnalysis,
            "clients" => ServerView::Clients,
            "monitor" => ServerView::Monitor,
            "config" => ServerView::Config,
            "acl" => ServerView::Acl,
            "search" => ServerView::Search,
            "functions" => ServerView::Functions,
            "luascripts" => ServerView::LuaScripts,
            "persistence" => ServerView::Persistence,
            "keyspacenotifications" => ServerView::KeyspaceNotifications,
            "topology" => ServerView::Topology,
            "serverload" => ServerView::ServerLoad,
            "hotkeys" => ServerView::Hotkeys,
            "valuesearch" => ServerView::ValueSearch,
            "serverinfo" => ServerView::ServerInfo,
            "timeseriesexplorer" => ServerView::TimeSeriesExplorer,
            _ => return None,
        })
    }
}
