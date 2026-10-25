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

//! `CLIENT PAUSE` and filtered `CLIENT KILL` — the argument lists, composed
//! here so the Clients panel and the live tests spell them the same way.
//!
//! And the operations of the Clients panel themselves. `CLIENT LIST`, `PAUSE`,
//! `UNPAUSE` and a filtered `KILL` go to every master, because a client is
//! connected to one node and the panel shows them all; `KILL ID` goes to the
//! one node that listed that client, since an id means nothing anywhere else.

use crate::config::RedisServer;
use crate::error::Error;
use crate::server_db::ServerDb;
use redis::{Cmd, cmd};
use std::collections::HashSet;

type Result<T, E = Error> = std::result::Result<T, E>;

fn client_cmd(args: &[impl AsRef<str>]) -> Cmd {
    let mut command = cmd("CLIENT");
    for arg in args {
        command.arg(arg.as_ref());
    }
    command
}

/// `CLIENT LIST` from every master: the node, and its reply as the server
/// wrote it (one client per line, `key=value` fields).
pub async fn client_list(at: &ServerDb) -> Result<Vec<(RedisServer, String)>> {
    let (nodes, replies): (Vec<RedisServer>, Vec<String>) = at
        .client()
        .await?
        .query_async_masters(vec![client_cmd(&["LIST"])])
        .await?;
    Ok(nodes.into_iter().zip(replies).collect())
}

/// Whether a `CLIENT LIST` name is one this application gives its own
/// connections: `zedis:v…` ([`crate::client_name`]), or the bare `zedis` of
/// a build from before the version was added.
pub fn is_zedis_client_name(name: &str) -> bool {
    name == "zedis" || name.starts_with("zedis:")
}

/// The addresses of the clients in one node's `CLIENT LIST` reply that
/// [`is_zedis_client_name`] claims, as the server wrote them (`addr=…`).
fn zedis_client_addrs_in(listing: &str) -> impl Iterator<Item = &str> {
    listing.lines().filter_map(|line| {
        let mut addr = None;
        let mut name = None;
        for field in line.split_ascii_whitespace() {
            if let Some(value) = field.strip_prefix("addr=") {
                addr = Some(value);
            } else if let Some(value) = field.strip_prefix("name=") {
                name = Some(value);
            }
        }
        name.filter(|name| is_zedis_client_name(name)).and(addr)
    })
}

/// The connections Zedis itself holds on every master, as `(node, addr)`:
/// the node as `host:port`, the address as `CLIENT LIST` wrote it — which is
/// also how a `MONITOR` line names the client. The Live Monitor leaves these
/// out of its feed by default: the heartbeat's `INFO`, the key tree's
/// `SCAN`, this very listing. The match is on the name alone, so another
/// Zedis on another machine, and a bridge's connections, count as well: they
/// are Zedis's commands too, not the application under study's.
pub async fn zedis_client_addrs(at: &ServerDb) -> Result<HashSet<(String, String)>> {
    let mut own = HashSet::new();
    for (node, listing) in client_list(at).await? {
        let node = format!("{}:{}", node.host, node.port);
        own.extend(zedis_client_addrs_in(&listing).map(|addr| (node.clone(), addr.to_string())));
    }
    Ok(own)
}

/// `CLIENT KILL ID id` on `node` — the master whose `CLIENT LIST` had the id
/// (ids are per node, so it goes nowhere else). `false` when no such client
/// was connected any more: the filter form of `CLIENT KILL` answers with a
/// count, not with the old form's "No such client" error, so a client that
/// left between the listing and the click is not a failure — and not a kill
/// either.
///
/// Sent through the entry's own per-master path rather than a connection
/// dialed to `node`: that is what reaches a node through the bridge, where
/// the listed node is a label without an entry of its own and dialing it
/// failed every time.
pub async fn client_kill_id(at: &ServerDb, node: &RedisServer, id: &str) -> Result<bool> {
    let client = at.client().await?;
    let masters = client.master_servers();
    let Some(target) = masters
        .iter()
        .position(|master| master.host == node.host && master.port == node.port)
    else {
        return Err(Error::Invalid {
            message: format!("{}:{} is no longer a master of this server", node.host, node.port),
        });
    };
    let commands = (0..masters.len())
        .map(|ix| (ix == target).then(|| client_cmd(&["KILL", "ID", id])))
        .collect();
    let killed: Vec<Option<i64>> = client.query_async_masters_with_option(commands).await?;
    Ok(killed.get(target).copied().flatten().unwrap_or(0) > 0)
}

/// `CLIENT PAUSE timeout [WRITE|ALL]` on every master ([`pause_args`]).
pub async fn client_pause(at: &ServerDb, timeout_ms: u64, mode: PauseMode, mode_supported: bool) -> Result<()> {
    let command = client_cmd(&pause_args(timeout_ms, mode, mode_supported));
    let (_, _replies): (_, Vec<String>) = at.client().await?.query_async_masters(vec![command]).await?;
    Ok(())
}

/// `CLIENT UNPAUSE` on every master.
pub async fn client_unpause(at: &ServerDb) -> Result<()> {
    let (_, _replies): (_, Vec<String>) = at
        .client()
        .await?
        .query_async_masters(vec![client_cmd(&["UNPAUSE"])])
        .await?;
    Ok(())
}

/// Run composed `CLIENT KILL …` argument lists ([`kill_filter_commands`]) on
/// every master and sum the counts the servers answer with. It takes the
/// lists rather than the filter so that what runs is exactly what the confirm
/// dialog summarised ([`kill_filter_summary`]).
pub async fn client_kill_by(at: &ServerDb, commands: &[Vec<String>]) -> Result<i64> {
    let client = at.client().await?;
    let mut killed = 0;
    for args in commands {
        let (_, counts): (_, Vec<i64>) = client.query_async_masters(vec![client_cmd(args)]).await?;
        killed += counts.iter().sum::<i64>();
    }
    Ok(killed)
}

/// What `CLIENT PAUSE` holds back.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PauseMode {
    /// Only writes (and their replication) wait; reads go on. Redis 6.2+.
    Write,
    /// Every command waits — the only mode before 6.2.
    All,
}

impl PauseMode {
    pub fn as_str(self) -> &'static str {
        match self {
            PauseMode::Write => "WRITE",
            PauseMode::All => "ALL",
        }
    }
}

/// `CLIENT PAUSE timeout [mode]`: the mode word only when the server
/// takes one (see `floors::CLIENT_PAUSE_WRITE`); before that a bare
/// `PAUSE` is `ALL`.
pub fn pause_args(timeout_ms: u64, mode: PauseMode, mode_supported: bool) -> Vec<String> {
    let mut args = vec!["PAUSE".to_string(), timeout_ms.to_string()];
    if mode_supported {
        args.push(mode.as_str().to_string());
    }
    args
}

/// The filters of a `CLIENT KILL`. Every set filter must match (they AND),
/// except `ids`: Redis keeps only the last `ID` given, so each id is its
/// own command.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct KillFilter {
    pub ids: Vec<u64>,
    pub addr: Option<String>,
    pub laddr: Option<String>,
    pub user: Option<String>,
    /// `normal` / `master` / `replica` / `pubsub`.
    pub kind: Option<String>,
    /// Only connections older than this many seconds (Redis 7.2+).
    pub maxage_secs: Option<u64>,
    /// Leave the connection that sends the command alone.
    pub skipme: bool,
}

impl KillFilter {
    /// Whether anything narrows the kill: `SKIPME` alone would be every
    /// other client, which is never what a filter form means.
    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
            && self.addr.is_none()
            && self.laddr.is_none()
            && self.user.is_none()
            && self.kind.is_none()
            && self.maxage_secs.is_none()
    }
}

/// The `CLIENT …` argument lists a filter turns into — one per id, or one
/// with every other filter; empty for an empty filter.
pub fn kill_filter_commands(filter: &KillFilter) -> Vec<Vec<String>> {
    if filter.is_empty() {
        return Vec::new();
    }
    let mut common: Vec<String> = Vec::new();
    let mut pair = |name: &str, value: Option<&str>| {
        if let Some(value) = value.map(str::trim).filter(|v| !v.is_empty()) {
            common.push(name.to_string());
            common.push(value.to_string());
        }
    };
    pair("ADDR", filter.addr.as_deref());
    pair("LADDR", filter.laddr.as_deref());
    pair("USER", filter.user.as_deref());
    pair("TYPE", filter.kind.as_deref());
    let maxage = filter.maxage_secs.map(|s| s.to_string());
    pair("MAXAGE", maxage.as_deref());
    common.push("SKIPME".to_string());
    common.push(if filter.skipme { "yes" } else { "no" }.to_string());

    if filter.ids.is_empty() {
        let mut args = vec!["KILL".to_string()];
        args.extend(common);
        return vec![args];
    }
    filter
        .ids
        .iter()
        .map(|id| {
            let mut args = vec!["KILL".to_string(), "ID".to_string(), id.to_string()];
            args.extend(common.iter().cloned());
            args
        })
        .collect()
}

/// `CLIENT KILL …` spelled out, one line per command, for a confirm prompt.
pub fn kill_filter_summary(commands: &[Vec<String>]) -> String {
    commands
        .iter()
        .map(|args| format!("CLIENT {}", args.join(" ")))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(args: &[String]) -> String {
        args.join(" ")
    }

    /// Our own connections are told apart by the name `CLIENT SETNAME` gave
    /// them — this build's, an older build's bare `zedis` — and nothing
    /// else: not a client whose name merely contains the word, not one
    /// without a name.
    #[test]
    fn our_connections_are_found_by_their_name_in_a_client_listing() {
        let listing = concat!(
            "id=3 addr=127.0.0.1:51166 laddr=127.0.0.1:6379 fd=8 name=zedis:v0.15.0 age=10 idle=0 cmd=info\n",
            "id=4 addr=127.0.0.1:51170 laddr=127.0.0.1:6379 fd=9 name=zedis age=8 idle=1 cmd=scan\n",
            "id=5 addr=[::1]:51180 laddr=[::1]:6379 fd=10 name= age=7 idle=2 cmd=get\n",
            "id=6 addr=10.0.0.9:40000 laddr=10.0.0.1:6379 fd=11 name=my-zedis-app age=5 idle=0 cmd=set\n",
            "id=7 addr=10.0.0.9:40001 fd=12 age=5 idle=0 cmd=set\n",
        );
        let own: Vec<&str> = zedis_client_addrs_in(listing).collect();
        assert_eq!(own, ["127.0.0.1:51166", "127.0.0.1:51170"]);
        assert!(is_zedis_client_name(crate::client_name()), "this build's own name");
        assert!(!is_zedis_client_name("zedisx") && !is_zedis_client_name(""));
    }

    #[test]
    fn pause_takes_a_mode_only_where_the_server_does() {
        assert_eq!(words(&pause_args(5000, PauseMode::Write, true)), "PAUSE 5000 WRITE");
        assert_eq!(words(&pause_args(250, PauseMode::All, true)), "PAUSE 250 ALL");
        assert_eq!(words(&pause_args(250, PauseMode::Write, false)), "PAUSE 250");
    }

    #[test]
    fn ids_become_one_command_each_and_the_rest_combine() {
        let filter = KillFilter {
            ids: vec![7, 42],
            user: Some("app".into()),
            skipme: true,
            ..Default::default()
        };
        let commands = kill_filter_commands(&filter);
        assert_eq!(
            commands.iter().map(|c| words(c)).collect::<Vec<_>>(),
            vec!["KILL ID 7 USER app SKIPME yes", "KILL ID 42 USER app SKIPME yes"]
        );

        let filter = KillFilter {
            addr: Some(" 10.0.0.9:50000 ".into()),
            laddr: Some("".into()),
            kind: Some("pubsub".into()),
            maxage_secs: Some(3600),
            skipme: false,
            ..Default::default()
        };
        let commands = kill_filter_commands(&filter);
        assert_eq!(
            commands.iter().map(|c| words(c)).collect::<Vec<_>>(),
            vec!["KILL ADDR 10.0.0.9:50000 TYPE pubsub MAXAGE 3600 SKIPME no"]
        );
        assert_eq!(
            kill_filter_summary(&commands),
            "CLIENT KILL ADDR 10.0.0.9:50000 TYPE pubsub MAXAGE 3600 SKIPME no"
        );
    }

    #[test]
    fn an_empty_filter_is_no_command_at_all() {
        let filter = KillFilter {
            skipme: true,
            ..Default::default()
        };
        assert!(filter.is_empty());
        assert!(kill_filter_commands(&filter).is_empty());
    }
}
