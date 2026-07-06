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

use crate::{
    assets::CustomIconName,
    connection::{DangerKind, get_connection_manager, get_server, get_servers},
    error::Error,
    helpers::{ConfigEditAction, get_mono_font_family},
    states::{
        ServerEvent, ServerView, ZedisGlobalStore, ZedisServerState, dialog_button_props, i18n_common,
        i18n_config_editor,
    },
    views::{ZedisCopyKeyDialog, confirm_dangerous_command},
};
use gpui::{App, Entity, FocusHandle, SharedString, Subscription, Window, div, prelude::*, px};
use gpui_component::{
    ActiveTheme, Icon, IconName, Sizable, WindowExt,
    button::{Button, ButtonVariants},
    checkbox::Checkbox,
    h_flex,
    input::{Input, InputEvent, InputState, NumberInput},
    label::Label,
    notification::Notification,
    spinner::Spinner,
    v_flex,
};
use redis::cmd;
use std::collections::{BTreeSet, HashMap};
use tracing::error;
use zedis_ui::{ZedisDialog, ZedisSelect, ZedisSelectEvent, help_popover};

type Result<T, E = Error> = std::result::Result<T, E>;

/// A computed `CONFIG GET *` diff between the active server and another:
/// only the parameters whose values differ, plus the two column labels.
struct ConfigDiff {
    local_label: SharedString,
    other_label: SharedString,
    /// `(parameter, local value, other value)`; a value absent on one side is
    /// an empty string.
    rows: Vec<(SharedString, SharedString, SharedString)>,
}

/// Editing control inferred for a config value. A curated catalog of known
/// enum parameters (by name) is consulted first; otherwise the type is guessed
/// from the current value: `yes`/`no` → checkbox, a parseable number → numeric
/// input, anything else → plain text.
#[derive(Clone, Copy, PartialEq)]
enum ConfigKind {
    /// Known enum parameter — pick from a fixed candidate list (a dropdown).
    Enum(&'static [&'static str]),
    Bool,
    Number,
    Text,
}

/// Candidate values for well-known enum config parameters. Curated and kept
/// version-independent (only options stable across Redis versions); the editor
/// always also offers the server's current value, so an option added in a newer
/// Redis is never lost. Parameters not listed fall back to value inference.
fn config_enum_options(key: &str) -> Option<&'static [&'static str]> {
    let opts: &[&str] = match key {
        "maxmemory-policy" => &[
            "noeviction",
            "allkeys-lru",
            "allkeys-lfu",
            "allkeys-random",
            "volatile-lru",
            "volatile-lfu",
            "volatile-random",
            "volatile-ttl",
        ],
        "appendfsync" => &["everysec", "always", "no"],
        "loglevel" => &["debug", "verbose", "notice", "warning", "nothing"],
        "tls-auth-clients" => &["no", "yes", "optional"],
        "repl-diskless-load" => &["disabled", "on-empty-db", "swapdb"],
        "sanitize-dump-payload" => &["no", "yes", "clients"],
        "propagation-error-behavior" => &["ignore", "panic", "panic-on-replicas"],
        "cluster-preferred-endpoint-type" => &["ip", "hostname", "unknown-endpoint"],
        "supervised" => &["no", "upstart", "systemd", "auto"],
        "oom-score-adj" => &["no", "yes", "relative", "absolute"],
        "acl-pubsub-default" => &["resetchannels", "allchannels"],
        "syslog-facility" => &[
            "user", "local0", "local1", "local2", "local3", "local4", "local5", "local6", "local7",
        ],
        // yes/no/local tri-state guards — not plain booleans.
        "enable-protected-configs" | "enable-debug-command" | "enable-module-command" => &["no", "yes", "local"],
        _ => return None,
    };
    Some(opts)
}

fn config_kind(key: &str, value: &str) -> ConfigKind {
    if let Some(opts) = config_enum_options(key) {
        return ConfigKind::Enum(opts);
    }
    match value {
        "yes" | "no" => ConfigKind::Bool,
        _ if !value.is_empty() && value.parse::<f64>().is_ok() => ConfigKind::Number,
        _ => ConfigKind::Text,
    }
}

/// A section the config parameters are grouped into. `label` is a fixed
/// technical string (a prefix pattern, shown mono); `desc_key` is an i18n key
/// (under `config_editor`) for the human description. A parameter joins the
/// first group any of its `prefixes` matches (`starts_with`); anything left
/// over falls into a synthetic "others" section.
struct ConfigGroup {
    label: &'static str,
    desc_key: &'static str,
    prefixes: &'static [&'static str],
}

/// Groups in display order. Prefixes are chosen specific enough to avoid
/// cross-group collisions (e.g. `set-max` not `set-`, so `set-proc-title`
/// doesn't land in data types).
/// Order: the parameters people most often view/tune first (memory,
/// persistence, network, security, replication, logging, latency), then the
/// situational/advanced groups (cluster, TLS, active defrag, data-type
/// tuning, scripting). The "others" bucket always renders after all of these.
const CONFIG_GROUPS: &[ConfigGroup] = &[
    ConfigGroup {
        label: "maxmemory-*",
        desc_key: "group_memory",
        prefixes: &["maxmemory"],
    },
    ConfigGroup {
        label: "aof-* / appendonly",
        desc_key: "group_aof",
        prefixes: &[
            "appendonly",
            "appendfsync",
            "appendfilename",
            "appenddirname",
            "aof",
            "auto-aof-rewrite",
            "no-appendfsync-on-rewrite",
        ],
    },
    ConfigGroup {
        label: "rdb-* / snapshot",
        desc_key: "group_rdb",
        prefixes: &[
            "save",
            "rdb",
            "dbfilename",
            "dir",
            "stop-writes-on-bgsave-error",
            "sanitize-dump-payload",
        ],
    },
    ConfigGroup {
        label: "client-* / network",
        desc_key: "group_network",
        prefixes: &[
            "maxclients",
            "timeout",
            "tcp",
            "client",
            "bind",
            "port",
            "unixsocket",
            "socket",
            "protected-mode",
        ],
    },
    ConfigGroup {
        label: "acl-* / auth",
        desc_key: "group_security",
        prefixes: &[
            "requirepass",
            "acl",
            "enable-protected",
            "enable-debug",
            "enable-module",
            "rename-command",
        ],
    },
    ConfigGroup {
        label: "repl-* / replica-*",
        desc_key: "group_replication",
        prefixes: &[
            "repl",
            "slave",
            "min-replicas",
            "min-slaves",
            "master",
            "propagation-error",
        ],
    },
    ConfigGroup {
        label: "log-* / syslog",
        desc_key: "group_logging",
        prefixes: &["logfile", "loglevel", "syslog", "crash-"],
    },
    ConfigGroup {
        label: "latency / slowlog",
        desc_key: "group_observability",
        prefixes: &["latency", "slowlog"],
    },
    ConfigGroup {
        label: "cluster-*",
        desc_key: "group_cluster",
        prefixes: &["cluster"],
    },
    ConfigGroup {
        label: "tls-*",
        desc_key: "group_tls",
        prefixes: &["tls"],
    },
    ConfigGroup {
        label: "active-defrag-*",
        desc_key: "group_defrag",
        prefixes: &["activedefrag", "active-defrag"],
    },
    ConfigGroup {
        label: "hash / list / set / zset",
        desc_key: "group_datatypes",
        prefixes: &[
            "hash-max",
            "list-max",
            "list-compress",
            "set-max",
            "zset-max",
            "stream-node-max",
            "hll-sparse",
            "activerehashing",
            "proto-max-bulk-len",
        ],
    },
    ConfigGroup {
        label: "lua / functions",
        desc_key: "group_scripting",
        prefixes: &["lua", "functions", "busy-reply-threshold"],
    },
];

/// The group index a config key belongs to, or `None` for the "others" bucket.
fn config_group_index(key: &str) -> Option<usize> {
    CONFIG_GROUPS
        .iter()
        .position(|g| g.prefixes.iter().any(|p| key.starts_with(p)))
}

/// Reference description for a well-known config parameter, shown in the
/// scrollable help popover behind the card's `?`. `(en, cn)` pairs: the
/// English is extracted from the bundled official redis.conf (reflowed to
/// Markdown); the Chinese is a faithful translation of it. Returns Chinese
/// when `zh` is set, English otherwise. Parameters without an entry show
/// no `?`.
fn config_doc(key: &str, zh: bool) -> Option<&'static str> {
    let (en, cn): (&str, &str) = match key {
        "maxmemory" => (
            r#"Set a memory usage limit to the specified amount of bytes. When the memory limit is reached Redis will try to remove keys according to the eviction policy selected (see maxmemory-policy).

If Redis can't remove keys according to the policy, or if the policy is set to 'noeviction', Redis will start to reply with errors to commands that would use more memory, like SET, LPUSH, and so on, and will continue to reply to read-only commands like GET.

This option is usually useful when using Redis as an LRU or LFU cache, or to set a hard memory limit for an instance (using the 'noeviction' policy).

WARNING: If you have replicas attached to an instance with maxmemory on, the size of the output buffers needed to feed the replicas are subtracted from the used memory count, so that network problems / resyncs will not trigger a loop where keys are evicted, and in turn the output buffer of replicas is full with DELs of keys evicted triggering the deletion of more keys, and so forth until the database is completely emptied.

In short... if you have replicas attached it is suggested that you set a lower limit for maxmemory so that there is some free RAM on the system for replica output buffers (but this is not needed if the policy is 'noeviction')."#,
            r#"将内存使用量限制在指定的字节数。达到内存上限后，Redis 会依据所选的淘汰策略（见 maxmemory-policy）尝试移除键。

如果无法按策略移除键，或策略被设为 'noeviction'，Redis 会对需要更多内存的命令（如 SET、LPUSH 等）返回错误，同时仍正常响应 GET 等只读命令。

该选项通常用于把 Redis 当作 LRU 或 LFU 缓存，或为实例设置一个硬性内存上限（配合 'noeviction' 策略）。

警告：当实例开启了 maxmemory 且挂有副本时，喂给副本的输出缓冲区大小会从已用内存中扣除，以免网络问题/重同步引发恶性循环——键被淘汰、副本输出缓冲区被淘汰键的 DEL 填满、进而触发更多键被删除，如此往复直至数据库被清空。

简言之：若挂有副本，建议把 maxmemory 设得低一些，给系统留出空闲内存供副本输出缓冲区使用（若策略为 'noeviction' 则无需如此）。"#,
        ),
        "maxmemory-policy" => (
            r#"MAXMEMORY POLICY: how Redis will select what to remove when maxmemory is reached. You can select one from the following behaviors:

volatile-lru -> Evict using approximated LRU, only keys with an expire set.
allkeys-lru -> Evict any key using approximated LRU.
volatile-lfu -> Evict using approximated LFU, only keys with an expire set.
allkeys-lfu -> Evict any key using approximated LFU.
volatile-lrm -> Evict using approximated LRM, only keys with an expire set.
allkeys-lrm -> Evict any key using approximated LRM.
volatile-random -> Remove a random key having an expire set.
allkeys-random -> Remove a random key, any key.
volatile-ttl -> Remove the key with the nearest expire time (minor TTL)
noeviction -> Don't evict anything, just return an error on write operations.

LRU means Least Recently Used LFU means Least Frequently Used LRM means Least Recently Modified (only write operations update the timestamp)

LRU, LFU, LRM and volatile-ttl are implemented using approximated randomized algorithms.

LRU vs LRM: Both use similar eviction logic based on access time, but:
- LRU updates the timestamp on both read (GET) and write (SET) operations
- LRM only updates the timestamp on write (SET, INCR, etc.) operations
This makes LRM useful when you want to evict keys that haven't been updated
recently, regardless of how often they are read.

Note: with any of the above policies, when there are no suitable keys for eviction, Redis will return an error on write operations that require more memory. These are usually commands that create new keys, add data or modify existing keys. A few examples are: SET, INCR, HSET, LPUSH, SUNIONSTORE, SORT (due to the STORE argument), and EXEC (if the transaction includes any command that requires memory).

The default is:"#,
            r#"MAXMEMORY 策略：达到 maxmemory 时 Redis 如何选择要移除的内容。可从以下行为中选择其一：

volatile-lru -> 仅对设置了过期时间的键，按近似 LRU 淘汰。
allkeys-lru -> 对任意键按近似 LRU 淘汰。
volatile-lfu -> 仅对设置了过期时间的键，按近似 LFU 淘汰。
allkeys-lfu -> 对任意键按近似 LFU 淘汰。
volatile-random -> 随机移除一个设置了过期时间的键。
allkeys-random -> 随机移除任意键。
volatile-ttl -> 移除过期时间最近（TTL 最小）的键。
noeviction -> 不淘汰任何键，对写操作直接返回错误。

LRU = 最近最少使用；LFU = 最不经常使用。

LRU、LFU 与 volatile-ttl 均采用近似的随机化算法实现。

注意：以上任一策略下，当没有合适的键可供淘汰时，Redis 会对需要更多内存的写操作返回错误。这类命令通常会创建新键、追加数据或修改已有键，例如：SET、INCR、HSET、LPUSH、SUNIONSTORE、SORT（因 STORE 参数）以及 EXEC（当事务中包含任何需要内存的命令时）。"#,
        ),
        "maxmemory-samples" => (
            r#"LRU, LFU and minimal TTL algorithms are not precise algorithms but approximated algorithms (in order to save memory), so you can tune it for speed or accuracy. By default Redis will check five keys and pick the one that was used least recently, you can change the sample size using the following configuration directive.

The default of 5 produces good enough results. 10 Approximates very closely true LRU but costs more CPU. 3 is faster but not very accurate. The maximum value that can be set is 64."#,
            r#"LRU、LFU 和最小 TTL 算法都不是精确算法，而是近似算法（为了节省内存），因此可以在速度和精度之间权衡。默认情况下 Redis 会检查五个键并挑出最近最少使用的那个，可用下面的配置项调整采样数量。

默认值 5 已能产生足够好的结果。设为 10 会非常接近真实 LRU，但更耗 CPU；设为 3 更快但不太准确。可设置的最大值为 64。"#,
        ),
        "maxmemory-eviction-tenacity" => (
            r#"Eviction processing is designed to function well with the default setting. If there is an unusually large amount of write traffic, this value may need to be increased.  Decreasing this value may reduce latency at the risk of eviction processing effectiveness   0 = minimum latency, 10 = default, 100 = process without regard to latency"#,
            r#"淘汰处理在默认设置下即可良好工作。若写入流量异常大，可能需要调高该值；调低则可能降低延迟，但会牺牲淘汰处理的效果。0 = 最低延迟，10 = 默认，100 = 不顾延迟地处理。"#,
        ),
        "maxmemory-clients" => (
            r#"In some scenarios client connections can hog up memory leading to OOM errors or data eviction. To avoid this we can cap the accumulated memory used by all client connections (all pubsub and normal clients). Once we reach that limit connections will be dropped by the server freeing up memory. The server will attempt to drop the connections using the most memory first. We call this mechanism "client eviction".

Client eviction is configured using the maxmemory-clients setting as follows: 0 - client eviction is disabled (default)

A memory value can be used for the client eviction threshold, for example:"#,
            r#"某些场景下，客户端连接会占用大量内存，导致 OOM 错误或数据被淘汰。为避免这一点，可对所有客户端连接（包括 pubsub 与普通客户端）累计使用的内存设上限。一旦达到上限，服务器会断开连接以释放内存，并优先断开占用内存最多的连接。这一机制称为「客户端淘汰」。

通过 maxmemory-clients 配置：0 表示关闭客户端淘汰（默认）。

阈值也可用内存值表示，例如 1g；或用百分比（如 5%）表示占最大内存的比例。"#,
        ),
        "appendonly" => (
            r#"By default Redis asynchronously dumps the dataset on disk. This mode is good enough in many applications, but an issue with the Redis process or a power outage may result into a few minutes of writes lost (depending on the configured save points).

The Append Only File is an alternative persistence mode that provides much better durability. For instance using the default data fsync policy (see later in the config file) Redis can lose just one second of writes in a dramatic event like a server power outage, or a single write if something wrong with the Redis process itself happens, but the operating system is still running correctly.

AOF and RDB persistence can be enabled at the same time without problems. If the AOF is enabled on startup Redis will load the AOF, that is the file with the better durability guarantees.

Note that changing this value in a config file of an existing database and restarting the server can lead to data loss. A conversion needs to be done by setting it via CONFIG command on a live server first."#,
            r#"默认情况下 Redis 会异步地把数据集转储到磁盘。这种模式对许多应用已经足够，但 Redis 进程出问题或断电时，可能会丢失几分钟的写入（取决于所配置的 save 点）。

Append Only File（AOF）是另一种持久化模式，能提供好得多的持久性。例如在默认的 fsync 策略下，即便发生服务器断电这类严重事件，Redis 也只会丢失一秒的写入；若只是 Redis 进程本身出错而操作系统仍正常，则最多丢失一次写入。

AOF 与 RDB 持久化可以同时开启而不冲突。若启动时 AOF 已开启，Redis 会加载 AOF，因为它的持久性保证更好。

注意：在已有数据库的配置文件中修改此值并重启，可能导致数据丢失。应先在运行中的实例上通过 CONFIG 命令切换来完成转换。"#,
        ),
        "appendfsync" => (
            r#"The fsync() call tells the Operating System to actually write data on disk instead of waiting for more data in the output buffer. Some OS will really flush data on disk, some other OS will just try to do it ASAP.

Redis supports three different modes:

no: don't fsync, just let the OS flush the data when it wants. Faster. always: fsync after every write to the append only log. Slow, Safest. everysec: fsync only one time every second. Compromise.

The default is "everysec", as that's usually the right compromise between speed and data safety. It's up to you to understand if you can relax this to "no" that will let the operating system flush the output buffer when it wants, for better performances (but if you can live with the idea of some data loss consider the default persistence mode that's snapshotting), or on the contrary, use "always" that's very slow but a bit safer than everysec.

If unsure, use "everysec"."#,
            r#"fsync() 调用会让操作系统真正把数据写入磁盘，而不是等待输出缓冲区里攒够更多数据。有些操作系统会立刻刷盘，有些则只是尽快尝试。

Redis 支持三种模式：

no：不做 fsync，让操作系统自行决定何时刷盘。更快。
always：每次写入 AOF 后都 fsync。慢，最安全。
everysec：每秒 fsync 一次。折中方案。

默认是 "everysec"，通常是速度与数据安全之间的恰当折中。你可以自行判断能否放宽到 "no"（由操作系统决定何时刷盘，性能更好，但要能接受一定数据丢失，否则不如考虑默认的快照持久化模式），或反过来用 "always"（很慢，但比 everysec 稍安全）。

拿不准就用 "everysec"。"#,
        ),
        "appendfilename" => (
            r#"The base name of the append only file.

Redis 7 and newer use a set of append-only files to persist the dataset and changes applied to it. There are two basic types of files in use:

- Base files, which are a snapshot representing the complete state of the
  dataset at the time the file was created. Base files can be either in
  the form of RDB (binary serialized) or AOF (textual commands).
- Incremental files, which contain additional commands that were applied
  to the dataset following the previous file.

In addition, manifest files are used to track the files and the order in which they were created and should be applied.

Append-only file names are created by Redis following a specific pattern. The file name's prefix is based on the 'appendfilename' configuration parameter, followed by additional information about the sequence and type.

For example, if appendfilename is set to appendonly.aof, the following file names could be derived:

- appendonly.aof.1.base.rdb as a base file.
- appendonly.aof.1.incr.aof, appendonly.aof.2.incr.aof as incremental files.
- appendonly.aof.manifest as a manifest file."#,
            r#"AOF 追加文件的基础名称。

Redis 7 及以后版本使用一组 AOF 文件来持久化数据集及其变更，主要有两类文件：

- 基准文件（base）：表示文件创建时数据集的完整状态快照，可以是 RDB（二进制序列化）或 AOF（文本命令）格式。
- 增量文件（incr）：包含在上一个文件之后应用到数据集的额外命令。

此外还有清单文件（manifest），用于记录这些文件及其创建/应用的顺序。

AOF 文件名按特定模式生成，前缀基于 'appendfilename' 配置，后接序号与类型等信息。例如当 appendfilename 为 appendonly.aof 时，可能派生出：appendonly.aof.1.base.rdb（基准文件）、appendonly.aof.1.incr.aof（增量文件）、appendonly.aof.manifest（清单文件）。"#,
        ),
        "appenddirname" => (
            r#"For convenience, Redis stores all persistent append-only files in a dedicated directory. The name of the directory is determined by the appenddirname configuration parameter."#,
            r#"为方便管理，Redis 把所有持久化的 AOF 文件存放在一个专用目录中。该目录名由 appenddirname 配置项决定。"#,
        ),
        "auto-aof-rewrite-percentage" => (
            r#"Automatic rewrite of the append only file. Redis is able to automatically rewrite the log file implicitly calling BGREWRITEAOF when the AOF log size grows by the specified percentage.

This is how it works: Redis remembers the size of the AOF file after the latest rewrite (if no rewrite has happened since the restart, the size of the AOF at startup is used).

This base size is compared to the current size. If the current size is bigger than the specified percentage, the rewrite is triggered. Also you need to specify a minimal size for the AOF file to be rewritten, this is useful to avoid rewriting the AOF file even if the percentage increase is reached but it is still pretty small.

Specify a percentage of zero in order to disable the automatic AOF rewrite feature."#,
            r#"自动重写 AOF 文件。当 AOF 日志相对上次重写后增长达到指定百分比时，Redis 能隐式调用 BGREWRITEAOF 自动重写。

工作方式：Redis 记住最近一次重写后 AOF 文件的大小（若自重启以来未发生重写，则用启动时的 AOF 大小）。将该基准大小与当前大小比较，若当前大小超出指定百分比即触发重写。同时还需指定 AOF 可被重写的最小体积，以避免虽达到增长百分比但文件仍很小时也去重写。

将百分比设为 0 可关闭自动 AOF 重写功能。"#,
        ),
        "no-appendfsync-on-rewrite" => (
            r#"When the AOF fsync policy is set to always or everysec, and a background saving process (a background save or AOF log background rewriting) is performing a lot of I/O against the disk, in some Linux configurations Redis may block too long on the fsync() call. Note that there is no fix for this currently, as even performing fsync in a different thread will block our synchronous write(2) call.

In order to mitigate this problem it's possible to use the following option that will prevent fsync() from being called in the main process while a BGSAVE or BGREWRITEAOF is in progress.

This means that while another child is saving, the durability of Redis is the same as "appendfsync no". In practical terms, this means that it is possible to lose up to 30 seconds of log in the worst scenario (with the default Linux settings).

If you have latency problems turn this to "yes". Otherwise leave it as "no" that is the safest pick from the point of view of durability."#,
            r#"当 AOF 的 fsync 策略为 always 或 everysec，且某个后台保存进程（后台 save 或 AOF 后台重写）正对磁盘进行大量 I/O 时，在某些 Linux 配置下 Redis 可能在 fsync() 上阻塞过久。注意目前对此并无根治办法，因为即便把 fsync 放到另一线程执行，仍会阻塞我们的同步 write(2) 调用。

为缓解该问题，可用下面这个选项，在 BGSAVE 或 BGREWRITEAOF 进行期间阻止主进程调用 fsync()。

这意味着当另一个子进程在保存时，Redis 的持久性等同于 "appendfsync no"。实际上，最坏情况下可能丢失多达 30 秒的日志（在默认 Linux 设置下）。

若有延迟问题就设为 "yes"；否则保持 "no"，从持久性角度看这是最安全的选择。"#,
        ),
        "aof-load-truncated" => (
            r#"An AOF file may be found to be truncated at the end during the Redis startup process, when the AOF data gets loaded back into memory. This may happen when the system where Redis is running crashes, especially when an ext4 filesystem is mounted without the data=ordered option (however this can't happen when Redis itself crashes or aborts but the operating system still works correctly).

Redis can either exit with an error when this happens, or load as much data as possible (the default now) and start if the AOF file is found to be truncated at the end. The following option controls this behavior.

If aof-load-truncated is set to yes, a truncated AOF file is loaded and the Redis server starts emitting a log to inform the user of the event. Otherwise if the option is set to no, the server aborts with an error and refuses to start. When the option is set to no, the user requires to fix the AOF file using the "redis-check-aof" utility before to restart the server.

Note that if the AOF file will be found to be corrupted in the middle the server will still exit with an error. This option only applies when Redis will try to read more data from the AOF file but not enough bytes will be found."#,
            r#"在 Redis 启动、把 AOF 数据加载回内存的过程中，可能发现 AOF 文件末尾被截断。这多发生在运行 Redis 的系统崩溃时，尤其是 ext4 文件系统未以 data=ordered 选项挂载的情况（不过若只是 Redis 自身崩溃或中止、而操作系统仍正常工作，则不会发生）。

遇到这种情况，Redis 可以直接报错退出，也可以尽量加载能读到的数据（现为默认）并在 AOF 末尾被截断时照常启动。下面的选项控制这一行为。

设为 yes 时，会加载被截断的 AOF，并输出一条日志告知用户；设为 no 时，服务器会报错中止、拒绝启动，此时需先用 "redis-check-aof" 工具修复 AOF 再重启。

注意：若 AOF 文件是在中间损坏，服务器仍会报错退出。本选项只适用于 Redis 试图从 AOF 读取更多数据但字节不足的情况。"#,
        ),
        "aof-use-rdb-preamble" => (
            r#"Redis can create append-only base files in either RDB or AOF formats. Using the RDB format is always faster and more efficient, and disabling it is only supported for backward compatibility purposes."#,
            r#"Redis 可以用 RDB 或 AOF 格式创建 AOF 基准文件。使用 RDB 格式总是更快、更高效；关闭它仅出于向后兼容目的才受支持。"#,
        ),
        "aof-timestamp-enabled" => (
            r#"Redis supports recording timestamp annotations in the AOF to support restoring the data from a specific point-in-time. However, using this capability changes the AOF format in a way that may not be compatible with existing AOF parsers."#,
            r#"Redis 支持在 AOF 中记录时间戳标注，以支持按指定时间点恢复数据。但启用该能力会改变 AOF 格式，可能与现有的 AOF 解析器不兼容。"#,
        ),
        "save" => (
            r#"Save the DB to disk.

Redis will save the DB if the given number of seconds elapsed and it surpassed the given number of write operations against the DB.

Snapshotting can be completely disabled with a single empty string argument as in following example:"#,
            r#"将数据库保存到磁盘。

当经过给定的秒数、且期间对数据库的写操作次数超过给定值时，Redis 会保存数据库。

用单个空字符串参数即可完全关闭快照。"#,
        ),
        "dbfilename" => (
            r#"The filename where to dump the DB"#,
            r#"转储数据库时使用的 RDB 文件名。"#,
        ),
        "dir" => (
            r#"The working directory.

The DB will be written inside this directory, with the filename specified above using the 'dbfilename' configuration directive.

The Append Only File will also be created inside this directory.

Note that you must specify a directory here, not a file name."#,
            r#"工作目录。

数据库会写入此目录，文件名由上面的 'dbfilename' 配置项指定。

AOF 文件也会创建在此目录内。

注意：这里必须指定一个目录，而不是文件名。"#,
        ),
        "rdbcompression" => (
            r#"Compress string objects using LZF when dump .rdb databases? By default compression is enabled as it's almost always a win. If you want to save some CPU in the saving child set it to 'no' but the dataset will likely be bigger if you have compressible values or keys."#,
            r#"转储 .rdb 数据库时是否用 LZF 压缩字符串对象？默认开启压缩，因为几乎总是划算。若想在保存子进程中省点 CPU，可设为 'no'，但当键值可压缩时数据集体积可能更大。"#,
        ),
        "rdbchecksum" => (
            r#"Since version 5 of RDB a CRC64 checksum is placed at the end of the file. This makes the format more resistant to corruption but there is a performance hit to pay (around 10%) when saving and loading RDB files, so you can disable it for maximum performances.

RDB files created with checksum disabled have a checksum of zero that will tell the loading code to skip the check."#,
            r#"从 RDB 第 5 版起，会在文件末尾放置 CRC64 校验和。这让格式更能抵御损坏，但保存和加载 RDB 时会有约 10% 的性能损耗，因此若追求极致性能可关闭它。

关闭校验和创建的 RDB 文件校验和为零，加载时会跳过检查。"#,
        ),
        "rdb-del-sync-files" => (
            r#"Remove RDB files used by replication in instances without persistence enabled. By default this option is disabled, however there are environments where for regulations or other security concerns, RDB files persisted on disk by masters in order to feed replicas, or stored on disk by replicas in order to load them for the initial synchronization, should be deleted ASAP. Note that this option ONLY WORKS in instances that have both AOF and RDB persistence disabled, otherwise is completely ignored.

An alternative (and sometimes better) way to obtain the same effect is to use diskless replication on both master and replicas instances. However in the case of replicas, diskless is not always an option."#,
            r#"删除未启用持久化的实例上用于复制的 RDB 文件。该选项默认关闭；但在某些环境下，出于合规或安全考虑，主库为喂给副本而落盘的 RDB、或副本为初始同步加载而落盘的 RDB，应尽快删除。注意：本选项仅在同时关闭 AOF 与 RDB 持久化的实例上生效，否则被完全忽略。

达成同样效果的另一种（有时更好的）方式，是在主库和副本上都使用无盘复制；不过对副本而言，无盘并非总是可行。"#,
        ),
        "stop-writes-on-bgsave-error" => (
            r#"By default Redis will stop accepting writes if RDB snapshots are enabled (at least one save point) and the latest background save failed. This will make the user aware (in a hard way) that data is not persisting on disk properly, otherwise chances are that no one will notice and some disaster will happen.

If the background saving process will start working again Redis will automatically allow writes again.

However if you have setup your proper monitoring of the Redis server and persistence, you may want to disable this feature so that Redis will continue to work as usual even if there are problems with disk, permissions, and so forth."#,
            r#"默认情况下，若启用了 RDB 快照（至少有一个 save 点）且最近一次后台保存失败，Redis 会停止接受写入。这会以「强硬」的方式让用户意识到数据未能正确落盘，否则很可能无人察觉、最终酿成灾难。

一旦后台保存进程恢复正常，Redis 会自动重新允许写入。

但如果你已经为 Redis 及其持久化配置了完善的监控，也可以关闭此特性，这样即便磁盘、权限等出问题，Redis 仍照常工作。"#,
        ),
        "sanitize-dump-payload" => (
            r#"Enables or disables full sanitization checks for ziplist and listpack etc when loading an RDB or RESTORE payload. This reduces the chances of a assertion or crash later on while processing commands. Options:   no         - Never perform full sanitization   yes        - Always perform full sanitization   clients    - Perform full sanitization only for user connections.                Excludes: RDB files, RESTORE commands received from the master                connection, and client connections which have the                skip-sanitize-payload ACL flag. The default should be 'clients' but since it currently affects cluster resharding via MIGRATE, it is temporarily set to 'no' by default."#,
            r#"在加载 RDB 或 RESTORE 载荷时，是否对 ziplist、listpack 等做完整的合法性校验。开启可降低后续处理命令时触发断言或崩溃的概率。可选值：
no — 从不做完整校验
yes — 总是做完整校验
clients — 仅对用户连接做完整校验（不含 RDB 文件、来自主库连接的 RESTORE，以及带 skip-sanitize-payload ACL 标志的客户端连接）。
默认本应为 'clients'，但由于当前会影响经 MIGRATE 的集群重分片，暂时默认设为 'no'。"#,
        ),
        "maxclients" => (
            r#"Set the max number of connected clients at the same time. By default this limit is set to 10000 clients, however if the Redis server is not able to configure the process file limit to allow for the specified limit the max number of allowed clients is set to the current file limit minus 32 (as Redis reserves a few file descriptors for internal uses).

Once the limit is reached Redis will close all the new connections sending an error 'max number of clients reached'.

IMPORTANT: When Redis Cluster is used, the max number of connections is also shared with the cluster bus: every node in the cluster will use two connections, one incoming and another outgoing. It is important to size the limit accordingly in case of very large clusters."#,
            r#"设置同时连接的客户端数量上限。默认上限为 10000；但若 Redis 无法把进程的文件描述符上限配到该值，则允许的最大客户端数会被设为当前文件上限减 32（Redis 会保留少量描述符供内部使用）。

达到上限后，Redis 会关闭所有新连接并返回错误 'max number of clients reached'。

重要：使用 Redis Cluster 时，最大连接数还要与集群总线共享——集群中每个节点会用两个连接（一进一出）。在超大集群中应据此调整上限。"#,
        ),
        "timeout" => (
            r#"Close the connection after a client is idle for N seconds (0 to disable)"#,
            r#"当客户端空闲 N 秒后关闭连接（0 表示关闭此功能）。"#,
        ),
        "tcp-keepalive" => (
            r#"TCP keepalive.

If non-zero, use SO_KEEPALIVE to send TCP ACKs to clients in absence of communication. This is useful for two reasons:

1) Detect dead peers. 2) Force network equipment in the middle to consider the connection to be    alive.

On Linux, the specified value (in seconds) is the period used to send ACKs. Note that to close the connection the double of the time is needed. On other kernels the period depends on the kernel configuration.

A reasonable value for this option is 300 seconds, which is the new Redis default starting with Redis 3.2.1."#,
            r#"TCP keepalive。

若非零，则使用 SO_KEEPALIVE，在无通信时向客户端发送 TCP ACK。这有两个好处：
1) 探测失联的对端；
2) 促使中间的网络设备认为连接仍然存活。

在 Linux 上，指定值（秒）是发送 ACK 的周期；注意真正关闭连接需要两倍的时间。在其它内核上，周期取决于内核配置。

合理的取值是 300 秒，这也是自 Redis 3.2.1 起的新默认值。"#,
        ),
        "tcp-backlog" => (
            r#"TCP listen() backlog.

In high requests-per-second environments you need a high backlog in order to avoid slow clients connection issues. Note that the Linux kernel will silently truncate it to the value of /proc/sys/net/core/somaxconn so make sure to raise both the value of somaxconn and tcp_max_syn_backlog in order to get the desired effect."#,
            r#"TCP listen() 的 backlog。

在高每秒请求数的环境中，需要较大的 backlog 以避免慢客户端带来的连接问题。注意 Linux 内核会悄悄将其截断到 /proc/sys/net/core/somaxconn 的值，因此请同时调高 somaxconn 和 tcp_max_syn_backlog 才能达到预期效果。"#,
        ),
        "bind" => (
            r#"By default, if no "bind" configuration directive is specified, Redis listens for connections from all available network interfaces on the host machine. It is possible to listen to just one or multiple selected interfaces using the "bind" configuration directive, followed by one or more IP addresses. Each address can be prefixed by "-", which means that redis will not fail to start if the address is not available. Being not available only refers to addresses that does not correspond to any network interface. Addresses that are already in use will always fail, and unsupported protocols will always BE silently skipped.

Examples:

~~~ WARNING ~~~ If the computer running Redis is directly exposed to the internet, binding to all the interfaces is dangerous and will expose the instance to everybody on the internet. So by default we uncomment the following bind directive, that will force Redis to listen only on the IPv4 and IPv6 (if available) loopback interface addresses (this means Redis will only be able to accept client connections from the same host that it is running on).

IF YOU ARE SURE YOU WANT YOUR INSTANCE TO LISTEN TO ALL THE INTERFACES COMMENT OUT THE FOLLOWING LINE.

You will also need to set a password unless you explicitly disable protected mode. ~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~"#,
            r#"默认情况下，若未指定 "bind" 配置，Redis 会监听宿主机上所有可用网络接口的连接。可用 "bind" 后接一个或多个 IP 地址，只监听选定的一个或多个接口。每个地址可加 "-" 前缀，表示该地址不可用时 Redis 也不会启动失败（「不可用」仅指不对应任何网络接口的地址；已被占用的地址总会失败，不支持的协议总会被静默跳过）。

警告：若运行 Redis 的机器直接暴露在公网上，绑定到所有接口很危险，会把实例暴露给互联网上的所有人。因此默认只监听 IPv4 和（若可用）IPv6 的回环地址，即只接受来自同一台主机的客户端连接。若确实要监听所有接口，请务必同时设置密码，或显式关闭保护模式。"#,
        ),
        "port" => (
            r#"Accept connections on the specified port, default is 6379 (IANA #815344). If port 0 is specified Redis will not listen on a TCP socket."#,
            r#"在指定端口接受连接，默认是 6379。若指定端口为 0，Redis 将不监听 TCP 套接字。"#,
        ),
        "protected-mode" => (
            r#"Protected mode is a layer of security protection, in order to avoid that Redis instances left open on the internet are accessed and exploited.

When protected mode is on and the default user has no password, the server only accepts local connections from the IPv4 address (127.0.0.1), IPv6 address (::1) or Unix domain sockets.

By default protected mode is enabled. You should disable it only if you are sure you want clients from other hosts to connect to Redis even if no authentication is configured."#,
            r#"保护模式是一层安全防护，用于避免暴露在公网上的 Redis 实例被访问和利用。

当保护模式开启且默认用户没有密码时，服务器只接受来自 IPv4 地址（127.0.0.1）、IPv6 地址（::1）或 Unix 域套接字的本地连接。

保护模式默认开启。只有当你确定要让其它主机的客户端在未配置认证的情况下也能连接时，才应关闭它。"#,
        ),
        "unixsocket" => (
            r#"Unix socket.

Specify the path for the Unix socket that will be used to listen for incoming connections. There is no default, so Redis will not listen on a unix socket when not specified."#,
            r#"Unix 套接字。

指定用于监听传入连接的 Unix 套接字路径。没有默认值，因此未指定时 Redis 不会监听 Unix 套接字。"#,
        ),
        "client-output-buffer-limit" => (
            r#"The client output buffer limits can be used to force disconnection of clients that are not reading data from the server fast enough for some reason (a common reason is that a Pub/Sub client can't consume messages as fast as the publisher can produce them).

The limit can be set differently for the three different classes of clients:

normal -> normal clients including MONITOR clients
replica -> replica clients
pubsub -> clients subscribed to at least one pubsub channel or pattern

The syntax of every client-output-buffer-limit directive is the following:

A client is immediately disconnected once the hard limit is reached, or if the soft limit is reached and remains reached for the specified number of seconds (continuously). So for instance if the hard limit is 32 megabytes and the soft limit is 16 megabytes / 10 seconds, the client will get disconnected immediately if the size of the output buffers reach 32 megabytes, but will also get disconnected if the client reaches 16 megabytes and continuously overcomes the limit for 10 seconds.

By default normal clients are not limited because they don't receive data without asking (in a push way), but just after a request, so only asynchronous clients may create a scenario where data is requested faster than it can read.

Instead there is a default limit for pubsub and replica clients, since subscribers and replicas receive data in a push fashion.

Note that it doesn't make sense to set the replica clients output buffer limit lower than the repl-backlog-size config (partial sync will succeed and then replica will get disconnected). Such a configuration is ignored (the size of repl-backlog-size will be used). This doesn't have memory consumption implications since the replica client will share the backlog buffers memory.

Both the hard or the soft limit can be disabled by setting them to zero."#,
            r#"客户端输出缓冲区上限可用于强制断开那些「读取服务器数据不够快」的客户端（常见原因是 Pub/Sub 客户端消费消息的速度跟不上发布者的生产速度）。

可为三类客户端分别设置上限：
normal -> 普通客户端（含 MONITOR 客户端）
replica -> 副本客户端
pubsub -> 至少订阅了一个 pubsub 频道或模式的客户端

每条指令的语法为：<class> <hard limit> <soft limit> <soft seconds>。一旦达到硬上限，或达到软上限并持续指定秒数，客户端会立即被断开。例如硬上限 32MB、软上限 16MB/10 秒时，输出缓冲区达到 32MB 会立即断开；若达到 16MB 并持续超限 10 秒也会被断开。

默认普通客户端不设限，因为它们不会被「推送」数据、只在请求后收到响应，只有异步客户端才可能出现请求快于读取的情形。而 pubsub 与副本客户端有默认上限，因为订阅者和副本是以推送方式接收数据的。

注意：把副本客户端的输出缓冲上限设得低于 repl-backlog-size 没有意义（部分同步会成功、随后副本又被断开），这类配置会被忽略（改用 repl-backlog-size 的大小）。这不会增加内存消耗，因为副本客户端会共享 backlog 缓冲区内存。

将硬上限或软上限设为零即可关闭对应限制。"#,
        ),
        "io-threads" => (
            r#"Redis is mostly single threaded, however there are certain threaded operations such as UNLINK, slow I/O accesses and other things that are performed on side threads.

Now it is also possible to handle Redis clients socket reads and writes in different I/O threads. Since especially writing is so slow, normally Redis users use pipelining in order to speed up the Redis performances per core, and spawn multiple instances in order to scale more. Using I/O threads it is possible to easily speedup several times Redis without resorting to pipelining nor sharding of the instance.

By default threading is disabled, we suggest enabling it only in machines that have at least 4 or more cores, leaving at least one spare core. We also recommend using threaded I/O only if you actually have performance problems, with Redis instances being able to use a quite big percentage of CPU time, otherwise there is no point in using this feature.

So for instance if you have a four cores boxes, try to use 3 I/O threads, if you have a 8 cores, try to use 7 threads. In order to enable I/O threads use the following configuration directive:"#,
            r#"Redis 大体上是单线程的，但也有一些线程化操作（如 UNLINK、慢速 I/O 访问等）在旁路线程中执行。

现在也可以用不同的 I/O 线程来处理客户端套接字的读写。由于写入尤其慢，通常用户会借助流水线（pipelining）来提升单核性能、并启动多个实例来扩展。使用 I/O 线程可以在不依赖流水线、也不对实例分片的前提下，轻松将 Redis 提速数倍。

默认关闭线程化。建议仅在至少 4 核以上、并能留出至少一个空闲核心的机器上启用；也建议仅在确有性能问题、Redis 实例能占用相当高比例 CPU 时才用，否则并无意义。

例如 4 核机器可尝试 3 个 I/O 线程，8 核可尝试 7 个。"#,
        ),
        "requirepass" => (
            r#"IMPORTANT NOTE: starting with Redis 6 "requirepass" is just a compatibility layer on top of the new ACL system. The option effect will be just setting the password for the default user. Clients will still authenticate using AUTH <password> as usually, or more explicitly with AUTH default <password> if they follow the new protocol: both will work.

The requirepass is not compatible with aclfile option and the ACL LOAD command, these will cause requirepass to be ignored."#,
            r#"重要提示：从 Redis 6 起，"requirepass" 只是新 ACL 系统之上的兼容层，其效果仅是为 default 用户设置密码。客户端仍照常用 AUTH <password> 认证；若遵循新协议，也可更明确地用 AUTH default <password>——两者都有效。

requirepass 与 aclfile 选项及 ACL LOAD 命令不兼容，后者会导致 requirepass 被忽略。"#,
        ),
        "aclfile" => (
            r#"Using an external ACL file

Instead of configuring users here in this file, it is possible to use a stand-alone file just listing users. The two methods cannot be mixed: if you configure users here and at the same time you activate the external ACL file, the server will refuse to start.

The format of the external ACL user file is exactly the same as the format that is used inside redis.conf to describe users."#,
            r#"使用外部 ACL 文件。

可以不在本配置文件中定义用户，而是用一个仅列出用户的独立文件。两种方式不能混用：若同时在此处配置用户并启用外部 ACL 文件，服务器会拒绝启动。

外部 ACL 用户文件的格式，与 redis.conf 中描述用户所用的格式完全相同。"#,
        ),
        "acllog-max-len" => (
            r#"ACL LOG

The ACL Log tracks failed commands and authentication events associated with ACLs. The ACL Log is useful to troubleshoot failed commands blocked by ACLs. The ACL Log is stored in memory. You can reclaim memory with ACL LOG RESET. Define the maximum entry length of the ACL Log below."#,
            r#"ACL LOG。

ACL 日志记录与 ACL 相关的失败命令和认证事件，便于排查被 ACL 拦截的命令。该日志存储在内存中，可用 ACL LOG RESET 回收内存。下面定义 ACL 日志的最大条目数。"#,
        ),
        "acl-pubsub-default" => (
            r#"New users are initialized with restrictive permissions by default, via the equivalent of this ACL rule 'off resetkeys -@all'. Starting with Redis 6.2, it is possible to manage access to Pub/Sub channels with ACL rules as well. The default Pub/Sub channels permission if new users is controlled by the

allchannels: grants access to all Pub/Sub channels resetchannels: revokes access to all Pub/Sub channels

From Redis 7.0, acl-pubsub-default defaults to 'resetchannels' permission."#,
            r#"新用户默认以受限权限初始化，等价于 ACL 规则 'off resetkeys -@all'。自 Redis 6.2 起，也可用 ACL 规则管理对 Pub/Sub 频道的访问。新用户默认的 Pub/Sub 频道权限由本项控制：

allchannels：授予对所有 Pub/Sub 频道的访问；
resetchannels：撤销对所有 Pub/Sub 频道的访问。

自 Redis 7.0 起，acl-pubsub-default 默认为 'resetchannels'。"#,
        ),
        "enable-protected-configs" => (
            r#"Redis uses default hardened security configuration directives to reduce the attack surface on innocent users. Therefore, several sensitive configuration directives are immutable, and some potentially-dangerous commands are blocked.

Configuration directives that control files that Redis writes to (e.g., 'dir' and 'dbfilename') and that aren't usually modified during runtime are protected by making them immutable.

Commands that can increase the attack surface of Redis and that aren't usually called by users are blocked by default.

These can be exposed to either all connections or just local ones by setting each of the configs listed below to either of these values:

no    - Block for any connection (remain immutable) yes   - Allow for any connection (no protection) local - Allow only for local connections. Ones originating from the         IPv4 address (127.0.0.1), IPv6 address (::1) or Unix domain sockets."#,
            r#"Redis 采用默认强化的安全配置来减小对无辜用户的攻击面。因此，若干敏感配置项被设为不可变，一些潜在危险的命令默认被屏蔽。

那些控制 Redis 写入文件、且通常不在运行时修改的配置项（如 'dir' 和 'dbfilename'）通过设为不可变来加以保护。

那些可能增大 Redis 攻击面、且通常不被用户调用的命令，默认被屏蔽。

可通过把下列各配置设为以下取值之一，把它们向所有连接或仅本地连接开放：
no — 对任何连接屏蔽（保持不可变）
yes — 对任何连接允许（无保护）
local — 仅允许本地连接，即来自 IPv4（127.0.0.1）、IPv6（::1）或 Unix 域套接字的连接。"#,
        ),
        "enable-debug-command" => (
            r#"Redis uses default hardened security configuration directives to reduce the attack surface on innocent users. Therefore, several sensitive configuration directives are immutable, and some potentially-dangerous commands are blocked.

Configuration directives that control files that Redis writes to (e.g., 'dir' and 'dbfilename') and that aren't usually modified during runtime are protected by making them immutable.

Commands that can increase the attack surface of Redis and that aren't usually called by users are blocked by default.

These can be exposed to either all connections or just local ones by setting each of the configs listed below to either of these values:

no    - Block for any connection (remain immutable) yes   - Allow for any connection (no protection) local - Allow only for local connections. Ones originating from the         IPv4 address (127.0.0.1), IPv6 address (::1) or Unix domain sockets.

enable-protected-configs no"#,
            r#"Redis 采用默认强化的安全配置来减小对无辜用户的攻击面。因此，若干敏感配置项被设为不可变，一些潜在危险的命令默认被屏蔽。

那些控制 Redis 写入文件、且通常不在运行时修改的配置项（如 'dir' 和 'dbfilename'）通过设为不可变来加以保护。

那些可能增大 Redis 攻击面、且通常不被用户调用的命令，默认被屏蔽。

可通过把下列各配置设为以下取值之一，把它们向所有连接或仅本地连接开放：
no — 对任何连接屏蔽（保持不可变）
yes — 对任何连接允许（无保护）
local — 仅允许本地连接，即来自 IPv4（127.0.0.1）、IPv6（::1）或 Unix 域套接字的连接。"#,
        ),
        "enable-module-command" => (
            r#"Redis uses default hardened security configuration directives to reduce the attack surface on innocent users. Therefore, several sensitive configuration directives are immutable, and some potentially-dangerous commands are blocked.

Configuration directives that control files that Redis writes to (e.g., 'dir' and 'dbfilename') and that aren't usually modified during runtime are protected by making them immutable.

Commands that can increase the attack surface of Redis and that aren't usually called by users are blocked by default.

These can be exposed to either all connections or just local ones by setting each of the configs listed below to either of these values:

no    - Block for any connection (remain immutable) yes   - Allow for any connection (no protection) local - Allow only for local connections. Ones originating from the         IPv4 address (127.0.0.1), IPv6 address (::1) or Unix domain sockets.

enable-protected-configs no enable-debug-command no"#,
            r#"Redis 采用默认强化的安全配置来减小对无辜用户的攻击面。因此，若干敏感配置项被设为不可变，一些潜在危险的命令默认被屏蔽。

那些控制 Redis 写入文件、且通常不在运行时修改的配置项（如 'dir' 和 'dbfilename'）通过设为不可变来加以保护。

那些可能增大 Redis 攻击面、且通常不被用户调用的命令，默认被屏蔽。

可通过把下列各配置设为以下取值之一，把它们向所有连接或仅本地连接开放：
no — 对任何连接屏蔽（保持不可变）
yes — 对任何连接允许（无保护）
local — 仅允许本地连接，即来自 IPv4（127.0.0.1）、IPv6（::1）或 Unix 域套接字的连接。"#,
        ),
        "replica-read-only" => (
            r#"You can configure a replica instance to accept writes or not. Writing against a replica instance may be useful to store some ephemeral data (because data written on a replica will be easily deleted after resync with the master) but may also cause problems if clients are writing to it because of a misconfiguration.

Since Redis 2.6 by default replicas are read-only.

Note: read only replicas are not designed to be exposed to untrusted clients on the internet. It's just a protection layer against misuse of the instance. Still a read only replica exports by default all the administrative commands such as CONFIG, DEBUG, and so forth. To a limited extent you can improve security of read only replicas using 'rename-command' to shadow all the administrative / dangerous commands."#,
            r#"可以配置副本实例是否接受写入。对副本写入可用于存放一些临时数据（因为写到副本上的数据在与主库重同步后会被轻易删除），但若因配置失误导致客户端向其写入，也可能引发问题。

自 Redis 2.6 起，副本默认只读。

注意：只读副本并非设计用来暴露给公网上不受信任的客户端，它只是防止误用实例的一层保护。即便如此，只读副本默认仍会导出 CONFIG、DEBUG 等所有管理命令。可在一定程度上用 'rename-command' 隐藏所有管理/危险命令，以提升只读副本的安全性。"#,
        ),
        "masterauth" => (
            r#"If the master is password protected (using the "requirepass" configuration directive below) it is possible to tell the replica to authenticate before starting the replication synchronization process, otherwise the master will refuse the replica request."#,
            r#"若主库设置了密码（通过下面的 "requirepass" 配置），可以让副本在开始复制同步流程前先进行认证，否则主库会拒绝副本的请求。"#,
        ),
        "masteruser" => (
            r#"If the master is password protected (using the "requirepass" configuration directive below) it is possible to tell the replica to authenticate before starting the replication synchronization process, otherwise the master will refuse the replica request.

masterauth <master-password>

However this is not enough if you are using Redis ACLs (for Redis version 6 or greater), and the default user is not capable of running the PSYNC command and/or other commands needed for replication. In this case it's better to configure a special user to use with replication, and specify the"#,
            r#"若主库设置了密码（通过 "requirepass" 配置），可让副本在开始复制同步前先认证，否则主库会拒绝其请求（配合 masterauth <master-password>）。

但在使用 Redis ACL（Redis 6 及以上）且 default 用户无法运行 PSYNC 等复制所需命令时，仅此还不够。此时最好配置一个专供复制使用的特殊用户，并用 masteruser 指定其用户名。"#,
        ),
        "repl-diskless-sync" => (
            r#"Replication SYNC strategy: disk or socket.

New replicas and reconnecting replicas that are not able to continue the replication process just receiving differences, need to do what is called a "full synchronization". An RDB file is transmitted from the master to the replicas.

The transmission can happen in two different ways:

1) Disk-backed: The Redis master creates a new process that writes the RDB                 file on disk. Later the file is transferred by the parent                 process to the replicas incrementally. 2) Diskless: The Redis master creates a new process that directly writes the              RDB file to replica sockets, without touching the disk at all.

With disk-backed replication, while the RDB file is generated, more replicas can be queued and served with the RDB file as soon as the current child producing the RDB file finishes its work. With diskless replication instead once the transfer starts, new replicas arriving will be queued and a new transfer will start when the current one terminates.

When diskless replication is used, the master waits a configurable amount of time (in seconds) before starting the transfer in the hope that multiple replicas will arrive and the transfer can be parallelized.

With slow disks and fast (large bandwidth) networks, diskless replication works better."#,
            r#"复制 SYNC 策略：磁盘还是套接字。

新副本、以及无法仅靠接收差异继续复制的重连副本，需要进行所谓的「全量同步」——由主库向副本传输一个 RDB 文件。传输可通过两种方式进行：
1) 基于磁盘：主库新建一个进程把 RDB 文件写到磁盘，随后父进程再把该文件增量地传给副本。
2) 无盘：主库新建一个进程直接把 RDB 写到副本的套接字，完全不碰磁盘。

基于磁盘复制时，在生成 RDB 期间可以排队更多副本，等当前生成 RDB 的子进程完成后一起用该文件服务它们。无盘复制则不同：一旦传输开始，新到达的副本会被排队，等当前传输结束后再启动新一轮。

使用无盘复制时，主库会等待一段可配置的时间（秒）再开始传输，以期多个副本到齐、把传输并行化。

在磁盘慢、网络快（大带宽）的场景下，无盘复制表现更好。"#,
        ),
        "repl-diskless-sync-delay" => (
            r#"When diskless replication is enabled, it is possible to configure the delay the server waits in order to spawn the child that transfers the RDB via socket to the replicas.

This is important since once the transfer starts, it is not possible to serve new replicas arriving, that will be queued for the next RDB transfer, so the server waits a delay in order to let more replicas arrive.

The delay is specified in seconds, and by default is 5 seconds. To disable it entirely just set it to 0 seconds and the transfer will start ASAP."#,
            r#"启用无盘复制时，可以配置服务器在派生「通过套接字传输 RDB」的子进程之前等待的延迟。

这很重要，因为一旦传输开始，就无法再服务新到达的副本，它们会被排入下一轮 RDB 传输，所以服务器先等一段时间以便更多副本到齐。

延迟以秒计，默认 5 秒。设为 0 即可完全关闭，传输会尽快开始。"#,
        ),
        "repl-diskless-load" => (
            r#"Replica can load the RDB it reads from the replication link directly from the socket, or store the RDB to a file and read that file after it was completely received from the master.

In many cases the disk is slower than the network, and storing and loading the RDB file may increase replication time (and even increase the master's Copy on Write memory and replica buffers). However, when parsing the RDB file directly from the socket, in order to avoid data loss it's only safe to flush the current dataset when the new dataset is fully loaded in memory, resulting in higher memory usage. For this reason we have the following options:

"disabled"    - Don't use diskless load (store the rdb file to the disk first) "swapdb"      - Keep current db contents in RAM while parsing the data directly                 from the socket. Replicas in this mode can keep serving current                 dataset while replication is in progress, except for cases where                 they can't recognize master as having a data set from same                 replication history.                 Note that this requires sufficient memory, if you don't have it,                 you risk an OOM kill. "flushdb"     - Always flush the entire dataset before diskless load.                 Note that if the diskless load fails, the replica will lose all                 existing data. "on-empty-db" - Use diskless load only when current dataset is empty. This is                 safer and avoid having old and new dataset loaded side by side                 during replication."#,
            r#"副本可以直接从复制链路的套接字加载读到的 RDB，也可以先把 RDB 存成文件、待完整收到后再从该文件加载。

很多情况下磁盘比网络慢，先存后读会拉长复制时间（甚至增加主库的写时复制内存和副本缓冲）。但若直接从套接字解析 RDB，为避免数据丢失，只能在新数据集完全载入内存后才清空当前数据集，因而内存占用更高。为此提供以下选项：

"disabled" — 不使用无盘加载（先把 RDB 存到磁盘）。
"swapdb" — 在直接从套接字解析数据的同时，把当前数据库内容保留在内存中。此模式下副本在复制进行时仍能服务现有数据集，除非无法确认主库与自己属于同一复制历史。注意这需要足够内存，否则有被 OOM kill 的风险。
"on-empty-db" — 仅当当前数据集为空时才使用无盘加载。这更安全，避免复制期间新旧数据集并存。"#,
        ),
        "repl-backlog-size" => (
            r#"Set the replication backlog size. The backlog is a buffer that accumulates replica data when replicas are disconnected for some time, so that when a replica wants to reconnect again, often a full resync is not needed, but a partial resync is enough, just passing the portion of data the replica missed while disconnected.

The bigger the replication backlog, the longer the replica can endure the disconnect and later be able to perform a partial resynchronization.

The backlog is only allocated if there is at least one replica connected."#,
            r#"设置复制积压缓冲区（backlog）的大小。backlog 是一个缓冲区，在副本断开一段时间时累积其数据，这样副本重连时往往无需全量重同步，只需部分重同步、补上断开期间缺失的那段数据即可。

backlog 越大，副本能承受的断开时间越长，之后仍可进行部分重同步。

只有在至少有一个副本连接时才会分配 backlog。"#,
        ),
        "repl-backlog-ttl" => (
            r#"After a master has no connected replicas for some time, the backlog will be freed. The following option configures the amount of seconds that need to elapse, starting from the time the last replica disconnected, for the backlog buffer to be freed.

Note that replicas never free the backlog for timeout, since they may be promoted to masters later, and should be able to correctly "partially resynchronize" with other replicas: hence they should always accumulate backlog.

A value of 0 means to never release the backlog."#,
            r#"当主库在一段时间内没有已连接的副本后，backlog 会被释放。下面的选项配置：从最后一个副本断开算起，需经过多少秒才释放 backlog 缓冲区。

注意：副本永远不会因超时释放 backlog，因为它们日后可能被提升为主库，需要能正确地与其它副本「部分重同步」，因此应始终累积 backlog。

值为 0 表示永不释放 backlog。"#,
        ),
        "repl-timeout" => (
            r#"The following option sets the replication timeout for:

1) Bulk transfer I/O during SYNC, from the point of view of replica. 2) Master timeout from the point of view of replicas (data, pings). 3) Replica timeout from the point of view of masters (REPLCONF ACK pings).

It is important to make sure that this value is greater than the value specified for repl-ping-replica-period otherwise a timeout will be detected every time there is low traffic between the master and the replica. The default value is 60 seconds."#,
            r#"下面的选项设置复制超时，适用于：
1) 从副本视角看，SYNC 期间的批量传输 I/O；
2) 从副本视角看的主库超时（数据、ping）；
3) 从主库视角看的副本超时（REPLCONF ACK ping）。

务必让此值大于 repl-ping-replica-period，否则在主从间流量较低时每次都会被判为超时。默认值 60 秒。"#,
        ),
        "repl-ping-replica-period" => (
            r#"Master send PINGs to its replicas in a predefined interval. It's possible to change this interval with the repl-ping-replica-period option. The default value is 10 seconds."#,
            r#"主库按预定间隔向其副本发送 PING。可用 repl-ping-replica-period 选项修改该间隔。默认值 10 秒。"#,
        ),
        "replica-serve-stale-data" => (
            r#"When a replica loses its connection with the master, or when the replication is still in progress, the replica can act in two different ways:

1) if replica-serve-stale-data is set to 'yes' (the default) the replica will    still reply to client requests, possibly with out of date data, or the    data set may just be empty if this is the first synchronization.

2) If replica-serve-stale-data is set to 'no' the replica will reply with error    "MASTERDOWN Link with MASTER is down and replica-serve-stale-data is set to 'no'"    to all data access commands, excluding commands such as:    INFO, REPLICAOF, AUTH, SHUTDOWN, REPLCONF, ROLE, CONFIG, SUBSCRIBE,    UNSUBSCRIBE, PSUBSCRIBE, PUNSUBSCRIBE, PUBLISH, PUBSUB, COMMAND, POST,    HOST and LATENCY."#,
            r#"当副本与主库失联，或复制仍在进行时，副本可有两种表现：

1) 若 replica-serve-stale-data 设为 'yes'（默认），副本仍会响应客户端请求，数据可能已过期；若这是首次同步，数据集也可能为空。

2) 若设为 'no'，副本会对所有数据访问命令返回错误 "MASTERDOWN Link with MASTER is down and replica-serve-stale-data is set to 'no'"，但以下命令除外：INFO、REPLICAOF、AUTH、SHUTDOWN、REPLCONF、ROLE、CONFIG、SUBSCRIBE、UNSUBSCRIBE、PSUBSCRIBE、PUNSUBSCRIBE、PUBLISH、PUBSUB、COMMAND、POST、HOST 和 LATENCY。"#,
        ),
        "replica-priority" => (
            r#"The replica priority is an integer number published by Redis in the INFO output. It is used by Redis Sentinel in order to select a replica to promote into a master if the master is no longer working correctly.

A replica with a low priority number is considered better for promotion, so for instance if there are three replicas with priority 10, 100, 25 Sentinel will pick the one with priority 10, that is the lowest.

However a special priority of 0 marks the replica as not able to perform the role of master, so a replica with priority of 0 will never be selected by Redis Sentinel for promotion.

By default the priority is 100."#,
            r#"副本优先级是 Redis 在 INFO 输出中公布的一个整数。当主库不再正常工作时，Redis Sentinel 用它来选择要提升为主库的副本。

优先级数值越小越适合被提升。例如三个副本优先级分别为 10、100、25 时，Sentinel 会选优先级 10（最小）的那个。

特殊值 0 表示该副本无法担任主库角色，因此优先级为 0 的副本永远不会被 Sentinel 选中提升。

默认优先级为 100。"#,
        ),
        "min-replicas-to-write" => (
            r#"It is possible for a master to stop accepting writes if there are less than N replicas connected, having a lag less or equal than M seconds.

The N replicas need to be in "online" state.

The lag in seconds, that must be <= the specified value, is calculated from the last ping received from the replica, that is usually sent every second.

This option does not GUARANTEE that N replicas will accept the write, but will limit the window of exposure for lost writes in case not enough replicas are available, to the specified number of seconds.

For example to require at least 3 replicas with a lag <= 10 seconds use:"#,
            r#"当连接的副本少于 N 个、或这些副本的滞后超过 M 秒时，主库可以停止接受写入。

这 N 个副本需处于「online」状态。

以秒计的滞后（须 <= 指定值）从最近一次收到该副本的 ping 算起，ping 通常每秒发送一次。

本选项并不保证一定有 N 个副本接受写入，而是把「副本不足时丢失写入」的暴露窗口限制在指定秒数内。

例如要求至少 3 个副本、滞后 <= 10 秒，可设 min-replicas-to-write 3 与 min-replicas-max-lag 10。"#,
        ),
        "min-replicas-max-lag" => (
            r#"It is possible for a master to stop accepting writes if there are less than N replicas connected, having a lag less or equal than M seconds.

The N replicas need to be in "online" state.

The lag in seconds, that must be <= the specified value, is calculated from the last ping received from the replica, that is usually sent every second.

This option does not GUARANTEE that N replicas will accept the write, but will limit the window of exposure for lost writes in case not enough replicas are available, to the specified number of seconds.

For example to require at least 3 replicas with a lag <= 10 seconds use:

min-replicas-to-write 3"#,
            r#"当连接的副本少于 N 个、或这些副本的滞后超过 M 秒时，主库可以停止接受写入。

这 N 个副本需处于「online」状态。

以秒计的滞后（须 <= 指定值）从最近一次收到该副本的 ping 算起，ping 通常每秒发送一次。

本选项并不保证一定有 N 个副本接受写入，而是把「副本不足时丢失写入」的暴露窗口限制在指定秒数内。

例如要求至少 3 个副本、滞后 <= 10 秒，可设 min-replicas-to-write 3 与 min-replicas-max-lag 10。"#,
        ),
        "propagation-error-behavior" => (
            r#"The propagation error behavior controls how Redis will behave when it is unable to handle a command being processed in the replication stream from a master or processed while reading from an AOF file. Errors that occur during propagation are unexpected, and can cause data inconsistency. However, there are edge cases in earlier versions of Redis where it was possible for the server to replicate or persist commands that would fail on future versions. For this reason the default behavior is to ignore such errors and continue processing commands.

If an application wants to ensure there is no data divergence, this configuration should be set to 'panic' instead. The value can also be set to 'panic-on-replicas' to only panic when a replica encounters an error on the replication stream. One of these two panic values will become the default value in the future once there are sufficient safety mechanisms in place to prevent false positive crashes."#,
            r#"传播错误行为控制当 Redis 无法处理「来自主库复制流中、或从 AOF 文件读取时」正在处理的某条命令时的表现。传播期间发生的错误是意外的，可能导致数据不一致。但在较早的 Redis 版本中存在一些边界情况：服务器可能复制或持久化了在后续版本会失败的命令。为此，默认行为是忽略这类错误并继续处理命令。

若应用希望确保不出现数据分歧，应把此项设为 'panic'；也可设为 'panic-on-replicas'，仅当副本在复制流上遇到错误时才 panic。待有足够的安全机制防止误报崩溃后，这两个 panic 值之一将来会成为默认值。"#,
        ),
        "loglevel" => (
            r#"Specify the server verbosity level. This can be one of: debug (a lot of information, useful for development/testing) verbose (many rarely useful info, but not a mess like the debug level) notice (moderately verbose, what you want in production probably) warning (only very important / critical messages are logged) nothing (nothing is logged)"#,
            r#"指定服务器的日志详细程度，可为以下之一：
debug（信息量很大，适合开发/测试）
verbose（很多很少用到的信息，但不像 debug 那样杂乱）
notice（适度详细，通常适合生产环境）
warning（只记录非常重要/关键的消息）
nothing（不记录任何日志）"#,
        ),
        "logfile" => (
            r#"Specify the log file name. Also the empty string can be used to force Redis to log on the standard output. Note that if you use standard output for logging but daemonize, logs will be sent to /dev/null"#,
            r#"指定日志文件名。也可用空字符串强制 Redis 输出到标准输出。注意：若用标准输出记日志但又以守护进程方式运行，日志会被送往 /dev/null。"#,
        ),
        "syslog-enabled" => (
            r#"To enable logging to the system logger, just set 'syslog-enabled' to yes, and optionally update the other syslog parameters to suit your needs."#,
            r#"要启用记录到系统日志（syslog），把 'syslog-enabled' 设为 yes，并按需更新其它 syslog 参数即可。"#,
        ),
        "syslog-ident" => (
            r#"Specify the syslog identity."#,
            r#"指定 syslog 的 identity（标识）。"#,
        ),
        "syslog-facility" => (
            r#"Specify the syslog facility. Must be USER or between LOCAL0-LOCAL7."#,
            r#"指定 syslog 的 facility。必须是 USER，或介于 LOCAL0-LOCAL7 之间。"#,
        ),
        "crash-log-enabled" => (
            r#"To disable the built in crash log, which will possibly produce cleaner core dumps when they are needed, uncomment the following:"#,
            r#"要关闭内置的崩溃日志（在需要时可能产生更干净的 core dump），取消注释此项即可。"#,
        ),
        "slowlog-log-slower-than" => (
            r#"The following time is expressed in microseconds, so 1000000 is equivalent to one second. Note that a negative number disables the slow log, while a value of zero forces the logging of every command."#,
            r#"下面的时间以微秒表示，因此 1000000 等于一秒。注意：负数会关闭慢日志，而值为零则强制记录每一条命令。"#,
        ),
        "slowlog-max-len" => (
            r#"There is no limit to this length. Just be aware that it will consume memory. You can reclaim memory used by the slow log with SLOWLOG RESET."#,
            r#"此长度没有上限，但要注意它会消耗内存。可用 SLOWLOG RESET 回收慢日志占用的内存。"#,
        ),
        "latency-monitor-threshold" => (
            r#"The Redis latency monitoring subsystem samples different operations at runtime in order to collect data related to possible sources of latency of a Redis instance.

Via the LATENCY command this information is available to the user that can print graphs and obtain reports.

The system only logs operations that were performed in a time equal or greater than the amount of milliseconds specified via the to zero, the latency monitor is turned off.

By default latency monitoring is disabled since it is mostly not needed if you don't have latency issues, and collecting data has a performance impact, that while very small, can be measured under big load. Latency monitoring can easily be enabled at runtime using the command "CONFIG SET latency-monitor-threshold <milliseconds>" if needed."#,
            r#"Redis 延迟监控子系统在运行时对不同操作采样，以收集与实例延迟可能来源相关的数据。

通过 LATENCY 命令，用户可获取这些信息，打印图表并生成报告。

系统只记录耗时大于等于指定毫秒数的操作；将阈值设为零则关闭延迟监控。

默认关闭延迟监控，因为在没有延迟问题时通常并不需要，而采集数据会有性能开销——虽然很小，但在大负载下可测。需要时可在运行时用 "CONFIG SET latency-monitor-threshold <milliseconds>" 轻松开启。"#,
        ),
        "latency-tracking" => (
            r#"The Redis extended latency monitoring tracks the per command latencies and enables exporting the percentile distribution via the INFO latencystats command, and cumulative latency distributions (histograms) via the LATENCY command.

By default, the extended latency monitoring is enabled since the overhead of keeping track of the command latency is very small."#,
            r#"Redis 扩展延迟监控会跟踪每条命令的延迟，并支持通过 INFO latencystats 命令导出分位分布，以及通过 LATENCY 命令导出累积延迟分布（直方图）。

默认开启扩展延迟监控，因为跟踪命令延迟的开销非常小。"#,
        ),
        "cluster-enabled" => (
            r#"Normal Redis instances can't be part of a Redis Cluster; only nodes that are started as cluster nodes can. In order to start a Redis instance as a cluster node enable the cluster support uncommenting the following:"#,
            r#"普通 Redis 实例不能加入 Redis Cluster，只有以集群节点方式启动的节点才可以。要以集群节点方式启动一个 Redis 实例，取消注释此项以开启集群支持。"#,
        ),
        "cluster-node-timeout" => (
            r#"Cluster node timeout is the amount of milliseconds a node must be unreachable for it to be considered in failure state. Most other internal time limits are a multiple of the node timeout."#,
            r#"集群节点超时，是指一个节点须持续多少毫秒不可达才被视为处于失败状态。其它多数内部时间限制都是该节点超时的倍数。"#,
        ),
        "cluster-config-file" => (
            r#"Every cluster node has a cluster configuration file. This file is not intended to be edited by hand. It is created and updated by Redis nodes. Every Redis Cluster node requires a different cluster configuration file. Make sure that instances running in the same system do not have overlapping cluster configuration file names."#,
            r#"每个集群节点都有一个集群配置文件。该文件不应手工编辑，由 Redis 节点创建和更新。每个 Redis Cluster 节点都需要一个不同的集群配置文件；请确保同一系统上运行的实例其集群配置文件名不重叠。"#,
        ),
        "cluster-replica-validity-factor" => (
            r#"A replica of a failing master will avoid to start a failover if its data looks too old.

There is no simple way for a replica to actually have an exact measure of its "data age", so the following two checks are performed:

1) If there are multiple replicas able to failover, they exchange messages    in order to try to give an advantage to the replica with the best    replication offset (more data from the master processed).    Replicas will try to get their rank by offset, and apply to the start    of the failover a delay proportional to their rank.

2) Every single replica computes the time of the last interaction with    its master. This can be the last ping or command received (if the master    is still in the "connected" state), or the time that elapsed since the    disconnection with the master (if the replication link is currently down).    If the last interaction is too old, the replica will not try to failover    at all.

The point "2" can be tuned by user. Specifically a replica will not perform the failover if, since the last interaction with the master, the time elapsed is greater than:

  (node-timeout * cluster-replica-validity-factor) + repl-ping-replica-period

So for example if node-timeout is 30 seconds, and the cluster-replica-validity-factor is 10, and assuming a default repl-ping-replica-period of 10 seconds, the replica will not try to failover if it was not able to talk with the master for longer than 310 seconds.

A large cluster-replica-validity-factor may allow replicas with too old data to failover a master, while a too small value may prevent the cluster from being able to elect a replica at all.

For maximum availability, it is possible to set the cluster-replica-validity-factor to a value of 0, which means, that replicas will always try to failover the master regardless of the last time they interacted with the master. (However they'll always try to apply a delay proportional to their offset rank).

Zero is the only value able to guarantee that when all the partitions heal the cluster will always be able to continue."#,
            r#"当主库故障时，若其副本的数据看起来太旧，副本会避免发起故障转移。

副本没有简单办法精确衡量自己的「数据年龄」，因此会进行以下两项检查：

1) 若有多个副本有能力进行故障转移，它们会交换消息，试图让复制偏移量最优（即从主库处理了更多数据）的副本占优。副本会按偏移量排名，并在发起故障转移前应用一个与其排名成正比的延迟。

2) 每个副本会计算自己与主库最后一次交互的时间——可以是最近收到的 ping 或命令（若主库仍处于「connected」状态），也可以是与主库断开以来经过的时间（若复制链路当前断开）。若最后一次交互过于久远，副本将完全不尝试故障转移。

用户可调节第 2 点。具体来说，若自最后一次与主库交互以来经过的时间大于 (node-timeout * cluster-replica-validity-factor) + repl-ping-replica-period，副本就不会进行故障转移。例如 node-timeout 为 30 秒、cluster-replica-validity-factor 为 10、假定 repl-ping-replica-period 为默认 10 秒，则副本在超过 310 秒无法与主库通信时不会尝试故障转移。

该因子过大，可能让数据过旧的副本也去接管主库；过小则可能导致集群根本选不出副本。

为追求最高可用性，可设为 0，表示副本无论上次何时与主库交互都会尝试故障转移（但仍会应用与其偏移排名成正比的延迟）。0 是唯一能保证「所有分区愈合后集群总能继续工作」的取值。"#,
        ),
        "cluster-migration-barrier" => (
            r#"Cluster replicas are able to migrate to orphaned masters, that are masters that are left without working replicas. This improves the cluster ability to resist to failures as otherwise an orphaned master can't be failed over in case of failure if it has no working replicas.

Replicas migrate to orphaned masters only if there are still at least a given number of other working replicas for their old master. This number is the "migration barrier". A migration barrier of 1 means that a replica will migrate only if there is at least 1 other working replica for its master and so forth. It usually reflects the number of replicas you want for every master in your cluster.

Default is 1 (replicas migrate only if their masters remain with at least one replica). To disable migration just set it to a very large value or set cluster-allow-replica-migration to 'no'. A value of 0 can be set but is useful only for debugging and dangerous in production."#,
            r#"集群副本能够迁移到「孤立主库」——即失去所有可用副本的主库。这提升了集群抵御故障的能力，否则孤立主库一旦故障便无法被接管。

副本只有在其原主库仍保有至少给定数量的其它可用副本时，才会迁移到孤立主库。这个数量即「迁移屏障」。屏障为 1 表示：只有当某副本的主库还至少有 1 个其它可用副本时，它才会迁移，以此类推。它通常反映你希望集群中每个主库拥有的副本数。

默认 1（仅当迁移后原主库仍至少留有一个副本时才迁移）。要关闭迁移，可将其设为极大值，或把 cluster-allow-replica-migration 设为 'no'。可设为 0，但仅用于调试，在生产中很危险。"#,
        ),
        "cluster-require-full-coverage" => (
            r#"By default Redis Cluster nodes stop accepting queries if they detect there is at least a hash slot uncovered (no available node is serving it). This way if the cluster is partially down (for example a range of hash slots are no longer covered) all the cluster becomes, eventually, unavailable. It automatically returns available as soon as all the slots are covered again.

However sometimes you want the subset of the cluster which is working, to continue to accept queries for the part of the key space that is still covered. In order to do so, just set the cluster-require-full-coverage option to no."#,
            r#"默认情况下，Redis Cluster 节点一旦发现至少有一个哈希槽未被覆盖（无可用节点服务它），便停止接受查询。这样当集群部分宕机（例如某段哈希槽不再被覆盖）时，整个集群最终变为不可用；一旦所有槽重新被覆盖，又会自动恢复可用。

但有时你希望「仍在工作的那部分集群」继续为仍被覆盖的那部分键空间提供查询服务。要这样做，把 cluster-require-full-coverage 设为 no 即可。"#,
        ),
        "cluster-allow-reads-when-down" => (
            r#"This option, when set to yes, allows nodes to serve read traffic while the cluster is in a down state, as long as it believes it owns the slots.

This is useful for two cases.  The first case is for when an application doesn't require consistency of data during node failures or network partitions. One example of this is a cache, where as long as the node has the data it should be able to serve it.

The second use case is for configurations that don't meet the recommended three shards but want to enable cluster mode and scale later. A master outage in a 1 or 2 shard configuration causes a read/write outage to the entire cluster without this option set, with it set there is only a write outage. Without a quorum of masters, slot ownership will not change automatically."#,
            r#"此选项设为 yes 时，允许节点在集群处于 down 状态时仍服务读流量，只要它认为自己拥有相应的槽。

这在两种情况下有用。其一：应用在节点故障或网络分区期间不要求数据一致性，例如缓存——只要节点还有数据就应能提供。

其二：用于未满足推荐的三分片、但想先开启集群模式、日后再扩展的配置。在 1 或 2 分片配置中，若不设此项，一个主库故障会导致整个集群读写都中断；设了此项则只中断写。注意：没有多数主库（quorum）时，槽的归属不会自动变更。"#,
        ),
        "cluster-preferred-endpoint-type" => (
            r#"Clusters can advertise how clients should connect to them using either their IP address, a user defined hostname, or by declaring they have no endpoint. Which endpoint is shown as the preferred endpoint is set by using the cluster-preferred-endpoint-type config with values 'ip', 'hostname', or 'unknown-endpoint'. This value controls how the endpoint returned for MOVED/ASKING requests as well as the first field of CLUSTER SLOTS. If the preferred endpoint type is set to hostname, but no announced hostname is set, a '?' will be returned instead.

When a cluster advertises itself as having an unknown endpoint, it's indicating that the server doesn't know how clients can reach the cluster. This can happen in certain networking situations where there are multiple possible routes to the node, and the server doesn't know which one the client took. In this case, the server is expecting the client to reach out on the same endpoint it used for making the last request, but use the port provided in the response."#,
            r#"集群可以用 IP 地址、用户自定义主机名，或声明自己「没有端点」，来告知客户端如何连接。首选端点类型由 cluster-preferred-endpoint-type 配置，取值为 'ip'、'hostname' 或 'unknown-endpoint'。此值控制 MOVED/ASKING 请求返回的端点，以及 CLUSTER SLOTS 的第一个字段。若首选类型设为 hostname 但未设置对外公布的主机名，则会返回 '?'。

当集群把自己公布为「未知端点」时，表示服务器不知道客户端能通过哪条路径连到集群。这可能发生在存在多条可达路由、服务器不知客户端走了哪条的网络情形下。此时服务器期望客户端沿用它上次请求所用的端点，但改用响应中给出的端口。"#,
        ),
        "tls-port" => (
            r#"By default, TLS/SSL is disabled. To enable it, the "tls-port" configuration directive can be used to define TLS-listening ports. To enable TLS on the default port, use:

port 0"#,
            r#"默认关闭 TLS/SSL。要开启，可用 "tls-port" 配置项定义 TLS 监听端口。若要在默认端口上启用 TLS，把普通端口关掉（port 0）并设置 tls-port。"#,
        ),
        "tls-cert-file" => (
            r#"Configure a X.509 certificate and private key to use for authenticating the server to connected clients, masters or cluster peers.  These files should be PEM formatted."#,
            r#"配置一份 X.509 证书和私钥，用于向已连接的客户端、主库或集群对端验证服务器身份。这些文件应为 PEM 格式（配合 tls-cert-file 与 tls-key-file）。"#,
        ),
        "tls-key-file" => (
            r#"Configure a X.509 certificate and private key to use for authenticating the server to connected clients, masters or cluster peers.  These files should be PEM formatted.

tls-cert-file redis.crt"#,
            r#"配置一份 X.509 证书和私钥，用于向已连接的客户端、主库或集群对端验证服务器身份。这些文件应为 PEM 格式（配合 tls-cert-file 与 tls-key-file）。"#,
        ),
        "tls-ca-cert-file" => (
            r#"Configure a CA certificate(s) bundle or directory to authenticate TLS/SSL clients and peers.  Redis requires an explicit configuration of at least one of these, and will not implicitly use the system wide configuration."#,
            r#"配置一个 CA 证书包或目录，用于认证 TLS/SSL 客户端与对端。Redis 要求至少显式配置其一，且不会隐式使用系统级配置。"#,
        ),
        "tls-auth-clients" => (
            r#"By default, clients (including replica servers) on a TLS port are required to authenticate using valid client side certificates.

If "no" is specified, client certificates are not required and not accepted. If "optional" is specified, client certificates are accepted and must be valid if provided, but are not required."#,
            r#"默认情况下，TLS 端口上的客户端（包括副本服务器）必须使用有效的客户端证书进行认证。

若设为 "no"，则不要求也不接受客户端证书；若设为 "optional"，则接受客户端证书、提供时必须有效，但并非必需。"#,
        ),
        "tls-protocols" => (
            r#"By default, only TLSv1.2 and TLSv1.3 are enabled and it is highly recommended that older formally deprecated versions are kept disabled to reduce the attack surface. You can explicitly specify TLS versions to support. Allowed values are case insensitive and include "TLSv1", "TLSv1.1", "TLSv1.2", "TLSv1.3" (OpenSSL >= 1.1.1) or any combination. To enable only TLSv1.2 and TLSv1.3, use:"#,
            r#"默认只启用 TLSv1.2 和 TLSv1.3，强烈建议保持已被正式弃用的旧版本处于关闭状态，以减小攻击面。你可以显式指定要支持的 TLS 版本，取值不区分大小写，包括 "TLSv1"、"TLSv1.1"、"TLSv1.2"、"TLSv1.3"（OpenSSL >= 1.1.1）或其任意组合。例如只启用 TLSv1.2 和 TLSv1.3。"#,
        ),
        "tls-ciphers" => (
            r#"Configure allowed ciphers.  See the ciphers(1ssl) manpage for more information about the syntax of this string.

Note: this configuration applies only to <= TLSv1.2."#,
            r#"配置允许的加密套件。其字符串语法详见 ciphers(1ssl) 手册页。

注意：此配置仅适用于 <= TLSv1.2。"#,
        ),
        "tls-replication" => (
            r#"By default, a Redis replica does not attempt to establish a TLS connection with its master.

Use the following directive to enable TLS on replication links."#,
            r#"默认情况下，Redis 副本不会尝试与其主库建立 TLS 连接。使用此指令可在复制链路上启用 TLS。"#,
        ),
        "tls-cluster" => (
            r#"By default, the Redis Cluster bus uses a plain TCP connection. To enable TLS for the bus protocol, use the following directive:"#,
            r#"默认情况下，Redis Cluster 总线使用普通 TCP 连接。使用此指令可为总线协议启用 TLS。"#,
        ),
        "activedefrag" => (
            r#"Active defragmentation is disabled by default"#,
            r#"主动碎片整理，默认关闭。开启后，Redis 服务器可在线压紧内存中小块分配/释放留下的空隙，从而回收内存。碎片是每个分配器都会自然产生的现象（用 Jemalloc 时相对较少），也与特定负载有关。"#,
        ),
        "active-defrag-ignore-bytes" => (
            r#"Minimum amount of fragmentation waste to start active defrag"#,
            r#"启动主动碎片整理所需的最小碎片浪费量（字节）。浪费的字节数至少达到该值才开始整理。"#,
        ),
        "active-defrag-threshold-lower" => (
            r#"Minimum percentage of fragmentation to start active defrag"#,
            r#"启动主动碎片整理所需的最小碎片率（百分比）。"#,
        ),
        "active-defrag-threshold-upper" => (
            r#"Maximum percentage of fragmentation at which we use maximum effort"#,
            r#"碎片率达到该百分比时，以最大力度进行整理。"#,
        ),
        "active-defrag-cycle-min" => (
            r#"Minimal effort for defrag in CPU percentage, to be used when the lower threshold is reached"#,
            r#"碎片整理的最小力度（CPU 百分比），在达到下限阈值时使用。"#,
        ),
        "active-defrag-cycle-max" => (
            r#"Maximal effort for defrag in CPU percentage, to be used when the upper threshold is reached"#,
            r#"碎片整理的最大力度（CPU 百分比），在达到上限阈值时使用。"#,
        ),
        "active-defrag-max-scan-fields" => (
            r#"Maximum number of set/hash/zset/list fields that will be processed from the main dictionary scan"#,
            r#"在主字典扫描中，单次处理的 set/hash/zset/list 字段的最大数量。"#,
        ),
        "hash-max-listpack-entries" => (
            r#"Hashes are encoded using a memory efficient data structure when they have a small number of entries, and the biggest entry does not exceed a given threshold. These thresholds can be configured using the following directives."#,
            r#"当 hash 的条目数较少、且最大条目不超过给定阈值时，会采用内存高效的数据结构（listpack）编码。这些阈值可用相关配置项设置。"#,
        ),
        "list-max-listpack-size" => (
            r#"Lists are also encoded in a special way to save a lot of space. The number of entries allowed per internal list node can be specified as a fixed maximum size or a maximum number of elements. For a fixed maximum size, use -5 through -1, meaning: -5: max size: 64 Kb  <-- not recommended for normal workloads -4: max size: 32 Kb  <-- not recommended -3: max size: 16 Kb  <-- probably not recommended -2: max size: 8 Kb   <-- good -1: max size: 4 Kb   <-- good Positive numbers mean store up to _exactly_ that number of elements per list node. The highest performing option is usually -2 (8 Kb size) or -1 (4 Kb size), but if your use case is unique, adjust the settings as necessary."#,
            r#"list 也以特殊方式编码以大量节省空间。每个内部 list 节点允许的条目数，可用「固定最大大小」或「最大元素个数」来指定。固定最大大小用 -5 到 -1 表示：
-5：最大 64 Kb  <-- 常规负载不推荐
-4：最大 32 Kb  <-- 不推荐
-3：最大 16 Kb  <-- 可能不推荐
-2：最大 8 Kb   <-- 好
-1：最大 4 Kb   <-- 好
正数表示每个 list 节点最多存储「恰好」这么多个元素。性能最好的通常是 -2（8 Kb）或 -1（4 Kb）；若你的场景特殊，可按需调整。"#,
        ),
        "list-compress-depth" => (
            r#"Lists may also be compressed.
Compress depth is the number of quicklist ziplist nodes from *each* side of
the list to *exclude* from compression.  The head and tail of the list
are always uncompressed for fast push/pop operations.  Settings are:
0: disable all list compression
1: depth 1 means "don't start compressing until after 1 node into the list,
   going from either the head or tail"
   So: [head]->node->node->...->node->[tail]
   [head], [tail] will always be uncompressed; inner nodes will compress.
2: [head]->[next]->node->node->...->node->[prev]->[tail]
   2 here means: don't compress head or head->next or tail->prev or tail,
   but compress all nodes between them.
3: [head]->[next]->[next]->node->node->...->node->[prev]->[prev]->[tail]
etc."#,
            r#"list 也可以被压缩。
compress depth 指从 list 两端各排除多少个 quicklist ziplist 节点不做压缩。为了快速的 push/pop，list 的头和尾始终不压缩。取值：
0：关闭所有 list 压缩。
1：深度 1 表示「从头或从尾进入 1 个节点后才开始压缩」。
   即：[head]->node->node->...->node->[tail]
   [head]、[tail] 始终不压缩，内部节点会被压缩。
2：[head]->[next]->node->...->node->[prev]->[tail]
   即不压缩 head、head->next、tail->prev、tail，但压缩它们之间的所有节点。
3：以此类推。"#,
        ),
        "set-max-intset-entries" => (
            r#"Sets have a special encoding when a set is composed of just strings that happen to be integers in radix 10 in the range of 64 bit signed integers. The following configuration setting sets the limit in the size of the set in order to use this special memory saving encoding."#,
            r#"当一个 set 完全由「10 进制、且落在 64 位有符号整数范围内」的字符串组成时，会采用特殊编码（intset）。下面的配置设定使用这种省内存编码的 set 大小上限。"#,
        ),
        "set-max-listpack-entries" => (
            r#"Sets containing non-integer values are also encoded using a memory efficient data structure when they have a small number of entries, and the biggest entry does not exceed a given threshold. These thresholds can be configured using the following directives."#,
            r#"当 set 含非整数值、条目数较少、且最大条目不超过给定阈值时，也会采用内存高效的数据结构（listpack）编码。这些阈值可用相关配置项设置。"#,
        ),
        "zset-max-listpack-entries" => (
            r#"Similarly to hashes and lists, sorted sets are also specially encoded in order to save a lot of space. This encoding is only used when the length and elements of a sorted set are below the following limits:"#,
            r#"与 hash 和 list 类似，sorted set 也会被特殊编码以大量节省空间。这种编码仅在 sorted set 的长度和元素大小都低于下列上限时使用。"#,
        ),
        "stream-node-max-bytes" => (
            r#"Streams macro node max size / items. The stream data structure is a radix tree of big nodes that encode multiple items inside. Using this configuration it is possible to configure how big a single node can be in bytes, and the maximum number of items it may contain before switching to a new node when appending new stream entries. If any of the following settings are set to zero, the limit is ignored, so for instance it is possible to set just a max entries limit by setting max-bytes to 0 and max-entries to the desired value."#,
            r#"stream 宏节点的最大大小/条目数。stream 数据结构是由「大节点」组成的基数树，每个节点内部编码多个条目。通过此配置可设定单个节点的最大字节数，以及追加新 stream 条目时切换到新节点前一个节点可容纳的最大条目数。若下列任一设置为零，则忽略对应上限；例如可把 max-bytes 设为 0、只用 max-entries 来限制条目数。"#,
        ),
        "hll-sparse-max-bytes" => (
            r#"HyperLogLog sparse representation bytes limit. The limit includes the 16 bytes header. When a HyperLogLog using the sparse representation crosses this limit, it is converted into the dense representation.

A value greater than 16000 is totally useless, since at that point the dense representation is more memory efficient.

The suggested value is ~ 3000 in order to have the benefits of the space efficient encoding without slowing down too much PFADD, which is O(N) with the sparse encoding. The value can be raised to ~ 10000 when CPU is not a concern, but space is, and the data set is composed of many HyperLogLogs with cardinality in the 0 - 15000 range."#,
            r#"HyperLogLog 稀疏表示的字节上限，含 16 字节头部。当稀疏表示的 HyperLogLog 超过此上限时，会转换为稠密表示。

超过 16000 完全没有意义，因为到那时稠密表示更省内存。

建议取值约 3000，以在获得省空间编码好处的同时不至于让 PFADD 太慢（稀疏编码下 PFADD 为 O(N)）。当不在意 CPU、但在意空间、且数据集由许多基数在 0–15000 之间的 HyperLogLog 组成时，可提高到约 10000。"#,
        ),
        "activerehashing" => (
            r#"Active rehashing uses 1 millisecond every 100 milliseconds of CPU time in order to help rehashing the main Redis hash table (the one mapping top-level keys to values). The hash table implementation Redis uses (see dict.c) performs a lazy rehashing: the more operation you run into a hash table that is rehashing, the more rehashing "steps" are performed, so if the server is idle the rehashing is never complete and some more memory is used by the hash table.

The default is to use this millisecond 10 times every second in order to actively rehash the main dictionaries, freeing memory when possible.

If unsure: use "activerehashing no" if you have hard latency requirements and it is not a good thing in your environment that Redis can reply from time to time to queries with 2 milliseconds delay.

use "activerehashing yes" if you don't have such hard requirements but want to free memory asap when possible."#,
            r#"主动 rehash 每 100 毫秒 CPU 时间中用 1 毫秒，来帮助对 Redis 主哈希表（把顶层键映射到值的那张表）进行 rehash。Redis 使用的哈希表实现（见 dict.c）采用惰性 rehash：对正在 rehash 的表操作越多，执行的 rehash「步」就越多；因此若服务器空闲，rehash 可能永远不会完成，哈希表也会多占一些内存。

默认每秒用这 1 毫秒共 10 次来主动 rehash 主字典，尽可能释放内存。

拿不准时：若你有严苛的延迟要求、且无法接受 Redis 偶尔以 2 毫秒延迟响应查询，就用 "activerehashing no"；若没有这种严苛要求、又希望尽快释放内存，就用 "activerehashing yes"。"#,
        ),
        "proto-max-bulk-len" => (
            r#"In the Redis protocol, bulk requests, that are, elements representing single strings, are normally limited to 512 mb. However you can change this limit here, but must be 1mb or greater"#,
            r#"在 Redis 协议中，bulk 请求（即表示单个字符串的元素）通常限制为 512 mb。你可以在此修改该上限，但必须为 1mb 或更大。"#,
        ),
        "lua-time-limit" => (
            r#"Maximum time in milliseconds for EVAL scripts, functions and in some cases modules' commands before Redis can start processing or rejecting other clients.

If the maximum execution time is reached Redis will start to reply to most commands with a BUSY error.

In this state Redis will only allow a handful of commands to be executed. For instance, SCRIPT KILL, FUNCTION KILL, SHUTDOWN NOSAVE and possibly some module specific 'allow-busy' commands.

SCRIPT KILL and FUNCTION KILL will only be able to stop a script that did not yet call any write commands, so SHUTDOWN NOSAVE may be the only way to stop the server in the case a write command was already issued by the script when the user doesn't want to wait for the natural termination of the script.

The default is 5 seconds. It is possible to set it to 0 or a negative value to disable this mechanism (uninterrupted execution). Note that in the past this config had a different name, which is now an alias, so both of these do the same:"#,
            r#"EVAL 脚本、函数、以及某些情况下模块命令的最大执行时间（毫秒），超过后 Redis 才会开始处理或拒绝其它客户端。

达到最大执行时间后，Redis 会对大多数命令返回 BUSY 错误。

此状态下 Redis 只允许执行少数几个命令，例如 SCRIPT KILL、FUNCTION KILL、SHUTDOWN NOSAVE，以及可能的某些模块特定的 'allow-busy' 命令。

SCRIPT KILL 和 FUNCTION KILL 只能停止尚未调用任何写命令的脚本；因此当脚本已发出写命令、而用户又不想等它自然结束时，SHUTDOWN NOSAVE 可能是停止服务器的唯一办法。

默认 5 秒。可设为 0 或负值以关闭此机制（不可中断地执行）。注意此配置过去叫另一个名字（现为别名），因此 lua-time-limit 与 busy-reply-threshold 二者作用相同。"#,
        ),
        "busy-reply-threshold" => (
            r#"Maximum time in milliseconds for EVAL scripts, functions and in some cases modules' commands before Redis can start processing or rejecting other clients.

If the maximum execution time is reached Redis will start to reply to most commands with a BUSY error.

In this state Redis will only allow a handful of commands to be executed. For instance, SCRIPT KILL, FUNCTION KILL, SHUTDOWN NOSAVE and possibly some module specific 'allow-busy' commands.

SCRIPT KILL and FUNCTION KILL will only be able to stop a script that did not yet call any write commands, so SHUTDOWN NOSAVE may be the only way to stop the server in the case a write command was already issued by the script when the user doesn't want to wait for the natural termination of the script.

The default is 5 seconds. It is possible to set it to 0 or a negative value to disable this mechanism (uninterrupted execution). Note that in the past this config had a different name, which is now an alias, so both of these do the same: lua-time-limit 5000"#,
            r#"EVAL 脚本、函数、以及某些情况下模块命令的最大执行时间（毫秒），超过后 Redis 才会开始处理或拒绝其它客户端。

达到最大执行时间后，Redis 会对大多数命令返回 BUSY 错误。

此状态下 Redis 只允许执行少数几个命令，例如 SCRIPT KILL、FUNCTION KILL、SHUTDOWN NOSAVE，以及可能的某些模块特定的 'allow-busy' 命令。

SCRIPT KILL 和 FUNCTION KILL 只能停止尚未调用任何写命令的脚本；因此当脚本已发出写命令、而用户又不想等它自然结束时，SHUTDOWN NOSAVE 可能是停止服务器的唯一办法。

默认 5 秒。可设为 0 或负值以关闭此机制（不可中断地执行）。注意此配置过去叫另一个名字（现为别名），因此 lua-time-limit 与 busy-reply-threshold 二者作用相同。"#,
        ),
        "auto-aof-rewrite-min-size" => (
            r#"Automatic rewrite of the append only file. Redis is able to automatically rewrite the log file implicitly calling BGREWRITEAOF when the AOF log size grows by the specified percentage.

This is how it works: Redis remembers the size of the AOF file after the latest rewrite (if no rewrite has happened since the restart, the size of the AOF at startup is used).

This base size is compared to the current size. If the current size is bigger than the specified percentage, the rewrite is triggered. Also you need to specify a minimal size for the AOF file to be rewritten, this is useful to avoid rewriting the AOF file even if the percentage increase is reached but it is still pretty small.

Specify a percentage of zero in order to disable the automatic AOF rewrite feature."#,
            r#"自动重写 AOF 文件。当 AOF 日志相对上次重写后增长达到指定百分比时，Redis 能隐式调用 BGREWRITEAOF 自动重写。

工作方式：Redis 记住最近一次重写后 AOF 文件的大小（若自重启以来未发生重写，则用启动时的 AOF 大小）。将该基准大小与当前大小比较，若当前大小超出指定百分比即触发重写。同时还需指定 AOF 可被重写的最小体积，以避免虽达到增长百分比但文件仍很小时也去重写。

将百分比设为 0 可关闭自动 AOF 重写功能。"#,
        ),
        "hash-max-listpack-value" => (
            r#"Hashes are encoded using a memory efficient data structure when they have a small number of entries, and the biggest entry does not exceed a given threshold. These thresholds can be configured using the following directives."#,
            r#"当 hash 的条目数较少、且最大条目不超过给定阈值时，会采用内存高效的数据结构（listpack）编码。这些阈值可用相关配置项设置。"#,
        ),
        "set-max-listpack-value" => (
            r#"Sets containing non-integer values are also encoded using a memory efficient data structure when they have a small number of entries, and the biggest entry does not exceed a given threshold. These thresholds can be configured using the following directives."#,
            r#"当 set 含非整数值、条目数较少、且最大条目不超过给定阈值时，也会采用内存高效的数据结构（listpack）编码。这些阈值可用相关配置项设置。"#,
        ),
        "zset-max-listpack-value" => (
            r#"Similarly to hashes and lists, sorted sets are also specially encoded in order to save a lot of space. This encoding is only used when the length and elements of a sorted set are below the following limits:"#,
            r#"与 hash 和 list 类似，sorted set 也会被特殊编码以大量节省空间。这种编码仅在 sorted set 的长度和元素大小都低于下列上限时使用。"#,
        ),
        "stream-node-max-entries" => (
            r#"Streams macro node max size / items. The stream data structure is a radix tree of big nodes that encode multiple items inside. Using this configuration it is possible to configure how big a single node can be in bytes, and the maximum number of items it may contain before switching to a new node when appending new stream entries. If any of the following settings are set to zero, the limit is ignored, so for instance it is possible to set just a max entries limit by setting max-bytes to 0 and max-entries to the desired value."#,
            r#"stream 宏节点的最大大小/条目数。stream 数据结构是由「大节点」组成的基数树，每个节点内部编码多个条目。通过此配置可设定单个节点的最大字节数，以及追加新 stream 条目时切换到新节点前一个节点可容纳的最大条目数。若下列任一设置为零，则忽略对应上限；例如可把 max-bytes 设为 0、只用 max-entries 来限制条目数。"#,
        ),
        "rdb-key-save-delay" => (
            r#"The amount of microseconds to delay saving each key when writing the RDB. Throttles the fork's copy-on-write; useful to reduce latency spikes on large datasets. 0 disables the delay."#,
            r#"写 RDB 时，每保存一个键延迟的微秒数。用于抑制 fork 的写时复制（copy-on-write），有助于减少大数据集上的延迟尖峰。0 表示不延迟。"#,
        ),
        "io-threads-do-reads" => (
            r#"When I/O threads are enabled, also offload reading and protocol parsing of client requests to the I/O threads, not just writing replies. Usually only worth enabling together with a higher io-threads count."#,
            r#"启用 I/O 线程时，也把客户端请求的读取和协议解析交给 I/O 线程处理，而不仅是写回复。通常只有在配合较高的 io-threads 数量时才值得开启。"#,
        ),
        _ => return None,
    };
    Some(if zh { cn } else { en })
}

pub struct ZedisConfigEditor {
    server_state: Entity<ZedisServerState>,
    /// Tracked on the root so a config edit can grab focus (see the edit
    /// handler), letting the `Esc` capture handler cancel the edit before the
    /// keystroke bubbles up to the global "back" binding.
    focus_handle: FocusHandle,
    configs: Vec<(SharedString, SharedString)>,
    filter_state: Entity<InputState>,
    filter: String,
    editing_key: Option<SharedString>,
    edit_state: Entity<InputState>,
    /// Numeric-only input used when editing a value that parses as a number.
    number_state: Entity<InputState>,
    /// Checkbox state used when editing a `yes`/`no` value.
    editing_bool: bool,
    /// Current selection used when editing a known enum value (dropdown).
    editing_enum: SharedString,
    /// Searchable-style dropdown (`ZedisSelect`, matching the settings UI) and
    /// its change subscription for the active enum edit; `None` otherwise. A
    /// stateful view entity, so it's created on entering an enum edit rather
    /// than inline per render.
    enum_select: Option<(Entity<ZedisSelect>, Subscription)>,
    loading: bool,
    /// True while a cross-server compare fetches the target's `CONFIG GET *`,
    /// so the header shows the same spinner as the initial load (the fetch can
    /// be slow against a remote / cluster target).
    comparing: bool,
    /// Set when the `CONFIG GET *` load fails, so the body shows the error
    /// instead of a misleading empty "no data" panel.
    error: Option<SharedString>,
    pending_notification: Option<Notification>,
    /// Active cross-server config comparison (`None` = normal editor view).
    diff: Option<ConfigDiff>,
    _subscriptions: Vec<Subscription>,
}

impl ZedisConfigEditor {
    pub fn new(server_state: Entity<ZedisServerState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let filter_state = cx.new(|cx| InputState::new(window, cx).placeholder("Filter by key..."));
        let edit_state = cx.new(|cx| InputState::new(window, cx));
        // Numeric values use a NumberInput (with ↑/↓ steppers) for a numeric
        // feel, but the input is NOT pattern-restricted — many "numeric-looking"
        // configs legitimately take spaces / units / multiple segments (e.g.
        // `save 3600 1 300 100`, `maxmemory 100mb`), so hard-blocking keystrokes
        // would trap the user. The steppers safely no-op on non-numeric values.
        let number_state = cx.new(|cx| InputState::new(window, cx));
        let mut subscriptions = Vec::new();

        subscriptions.push(cx.subscribe(&filter_state, |this, state, event, cx| {
            if matches!(event, InputEvent::Change) {
                this.filter = state.read(cx).value().to_string();
                cx.notify();
            }
        }));

        subscriptions.push(cx.subscribe(&server_state, |this, _server_state, event, cx| {
            if matches!(event, ServerEvent::ServerSelected(_)) {
                this.editing_key = None;
                this.enum_select = None;
                this.configs.clear();
                this.load_configs(cx);
            }
        }));

        let mut this = Self {
            server_state,
            focus_handle: cx.focus_handle(),
            configs: Vec::new(),
            filter_state,
            filter: String::new(),
            editing_key: None,
            edit_state,
            number_state,
            editing_bool: false,
            editing_enum: SharedString::default(),
            enum_select: None,
            loading: false,
            comparing: false,
            error: None,
            pending_notification: None,
            diff: None,
            _subscriptions: subscriptions,
        };
        this.load_configs(cx);
        this
    }

    fn load_configs(&mut self, cx: &mut Context<Self>) {
        if self.loading {
            return;
        }
        let server_state = self.server_state.read(cx);
        let server_id = server_state.server_id().to_string();
        let db = server_state.db();
        // No active connection yet — e.g. a restored `config` route recreated
        // this view before `ServerSelected` wired up the server. Querying with
        // an empty id would hit `get_server("")` → "Redis config not found".
        // The ServerSelected subscription reloads once the connection is ready.
        if server_id.is_empty() {
            return;
        }
        self.loading = true;
        cx.spawn(async move |handle, cx| {
            let task = cx.background_spawn(async move {
                let mut conn = get_connection_manager().get_connection(&server_id, db).await?;
                let map: HashMap<String, String> = cmd("CONFIG").arg("GET").arg("*").query_async(&mut conn).await?;
                let mut configs: Vec<(SharedString, SharedString)> =
                    map.into_iter().map(|(k, v)| (k.into(), v.into())).collect();
                configs.sort_unstable_by(|a, b| a.0.cmp(&b.0));
                Ok(configs)
            });
            let result: Result<Vec<(SharedString, SharedString)>> = task.await;
            let _ = handle.update(cx, |this, cx| {
                this.loading = false;
                match result {
                    Ok(configs) => {
                        this.configs = configs;
                        this.error = None;
                    }
                    Err(e) => {
                        error!(error = %e, "load configs failed");
                        this.error = Some(e.to_string().into());
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn save_config(&mut self, key: SharedString, value: SharedString, cx: &mut Context<Self>) {
        let server_state = self.server_state.read(cx);
        let server_id = server_state.server_id().to_string();
        let db = server_state.db();
        cx.spawn(async move |handle, cx| {
            let key_clone = key.clone();
            let value_clone = value.clone();
            let task = cx.background_spawn(async move {
                let mut conn = get_connection_manager().get_connection(&server_id, db).await?;
                let _: () = cmd("CONFIG")
                    .arg("SET")
                    .arg(key.as_str())
                    .arg(value.as_str())
                    .query_async(&mut conn)
                    .await?;
                Ok(())
            });
            let result: Result<()> = task.await;
            let _ = handle.update(cx, |this, cx| {
                this.editing_key = None;
                this.enum_select = None;
                match result {
                    Ok(()) => {
                        if let Some(entry) = this.configs.iter_mut().find(|(k, _)| k == &key_clone) {
                            entry.1 = value_clone;
                        }
                        this.pending_notification = Some(Notification::success(i18n_config_editor(cx, "save_success")));
                    }
                    Err(e) => {
                        let msg: SharedString = format!("{}: {}", i18n_config_editor(cx, "save_failed"), e).into();
                        this.pending_notification = Some(Notification::error(msg));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Open the server picker (copy dialog reused as a server / db picker) and,
    /// on OK, compare this server's `CONFIG GET *` against the chosen one.
    fn open_compare_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let server_state = self.server_state.read(cx);
        let source_id = server_state.server_id().to_string();
        let source_db = server_state.db();
        if get_servers().map(|s| s.is_empty()).unwrap_or(true) {
            return;
        }
        let view = cx.new(|cx| ZedisCopyKeyDialog::new(source_id.into(), source_db, false, window, cx));
        let view_child = view.clone();
        let view_ok = view.clone();
        let editor = cx.entity().downgrade();
        ZedisDialog::new(i18n_config_editor(cx, "compare_title"))
            .w(px(460.))
            .ok_text(i18n_config_editor(cx, "compare_title"))
            .cancel_text(i18n_common(cx, "cancel"))
            .button_props(
                dialog_button_props(cx)
                    .ok_text(i18n_config_editor(cx, "compare_title"))
                    .cancel_text(i18n_common(cx, "cancel")),
            )
            .child(move || view_child.clone())
            .on_ok(move |_, _window, cx| {
                let Some(target_id) = view_ok.read(cx).target_server_id() else {
                    return false;
                };
                let target_db = view_ok.read(cx).target_db(cx);
                if let Some(editor) = editor.upgrade() {
                    editor.update(cx, |this, cx| this.run_compare(target_id, target_db, cx));
                }
                true
            })
            .open(window, cx);
    }

    /// Fetch `CONFIG GET *` from the target and store the differing parameters.
    fn run_compare(&mut self, target_id: SharedString, target_db: usize, cx: &mut Context<Self>) {
        // Show the header spinner immediately; the target fetch below is async.
        self.comparing = true;
        cx.notify();
        let local = self.configs.clone();
        let local_id = self.server_state.read(cx).server_id().to_string();
        let local_label: SharedString = get_server(&local_id)
            .map(|s| s.name.into())
            .unwrap_or_else(|_| local_id.clone().into());
        let other_label: SharedString = get_server(&target_id)
            .map(|s| s.name.into())
            .unwrap_or_else(|_| target_id.clone());
        cx.spawn(async move |handle, cx| {
            let task = cx.background_spawn(async move {
                let mut conn = get_connection_manager().get_connection(&target_id, target_db).await?;
                let map: HashMap<String, String> = cmd("CONFIG").arg("GET").arg("*").query_async(&mut conn).await?;
                Ok::<HashMap<String, String>, Error>(map)
            });
            let result = task.await;
            let _ = handle.update(cx, |this, cx| {
                this.comparing = false;
                match result {
                    Ok(other_map) => {
                        let mut local_map: HashMap<String, String> =
                            local.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
                        let mut keys: BTreeSet<String> = local_map.keys().cloned().collect();
                        keys.extend(other_map.keys().cloned());
                        let mut rows: Vec<(SharedString, SharedString, SharedString)> = Vec::new();
                        for key in keys {
                            let lv = local_map.remove(&key).unwrap_or_default();
                            let ov = other_map.get(&key).cloned().unwrap_or_default();
                            if lv != ov {
                                rows.push((key.into(), lv.into(), ov.into()));
                            }
                        }
                        this.diff = Some(ConfigDiff {
                            local_label,
                            other_label,
                            rows,
                        });
                        cx.notify();
                    }
                    Err(e) => {
                        let msg: SharedString = format!("{}: {e}", i18n_config_editor(cx, "compare_failed")).into();
                        this.pending_notification = Some(Notification::error(msg));
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }

    fn close_diff(&mut self, cx: &mut Context<Self>) {
        self.diff = None;
        cx.notify();
    }

    /// Build the enum-edit dropdown (`ZedisSelect`, matching the settings UI)
    /// for `value` among `options`. The server's current value is always
    /// included so an option from a newer Redis isn't lost. The subscription
    /// mirrors the selection into `editing_enum` for the eventual save.
    fn build_enum_select(
        &mut self,
        options: &[&str],
        value: &SharedString,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut opts: Vec<SharedString> = options.iter().map(|s| SharedString::from(*s)).collect();
        if !opts.iter().any(|o| o.as_ref() == value.as_ref()) {
            opts.insert(0, value.clone());
        }
        let selected = opts.iter().position(|o| o.as_ref() == value.as_ref());
        let items: Vec<String> = opts.iter().map(|o| o.to_string()).collect();
        self.editing_enum = value.clone();
        let select = cx.new(|cx| ZedisSelect::new(items, selected, window, cx));
        let opts_for_sub = opts;
        let subscription = cx.subscribe(&select, move |this, _sel, event: &ZedisSelectEvent, cx| {
            let ZedisSelectEvent::Change(index) = event;
            if let Some(v) = opts_for_sub.get(*index) {
                this.editing_enum = v.clone();
                cx.notify();
            }
        });
        self.enum_select = Some((select, subscription));
    }

    /// One config parameter as a card: key + edit pencil + value in display
    /// mode; a highlighted card with the editor control + Save/Cancel while
    /// editing. Theme colors are copied to `Copy` locals before any
    /// `cx.listener` closure (borrowing `cx` inside one won't compile).
    fn render_card(
        &self,
        key: SharedString,
        value: SharedString,
        font_family: &SharedString,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let border = cx.theme().border;
        let muted = cx.theme().muted_foreground;
        let fg = cx.theme().foreground;
        let primary = cx.theme().primary;
        let green = cx.theme().green;
        let card_bg = cx.theme().secondary;
        let radius = cx.theme().radius;
        let kind = config_kind(&key, &value);

        if self.editing_key.as_ref() == Some(&key) {
            let save_key = key.clone();
            let editor = match kind {
                ConfigKind::Enum(_) => self
                    .enum_select
                    .as_ref()
                    .map(|(s, _)| s.clone().into_any_element())
                    .unwrap_or_else(|| div().into_any_element()),
                ConfigKind::Bool => Checkbox::new("config-bool-edit")
                    .checked(self.editing_bool)
                    .label(if self.editing_bool { "yes" } else { "no" })
                    .on_click(cx.listener(|this, checked: &bool, _w, cx| {
                        this.editing_bool = *checked;
                        cx.notify();
                    }))
                    .into_any_element(),
                ConfigKind::Number => NumberInput::new(&self.number_state).w_full().into_any_element(),
                ConfigKind::Text => Input::new(&self.edit_state)
                    .w_full()
                    .font_family(font_family.clone())
                    .appearance(true)
                    .into_any_element(),
            };
            v_flex()
                .flex_1()
                .min_w_0()
                .gap_2()
                .p_3()
                .border_1()
                .border_color(primary)
                .rounded(radius)
                .child(
                    h_flex()
                        .items_center()
                        .justify_between()
                        .gap_2()
                        .child(
                            div().min_w_0().overflow_hidden().child(
                                Label::new(key.clone())
                                    .text_sm()
                                    .text_color(muted)
                                    .font_family(font_family.clone()),
                            ),
                        )
                        .child(
                            Label::new(i18n_config_editor(cx, "editing"))
                                .text_xs()
                                .text_color(primary),
                        ),
                )
                .child(editor)
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            Button::new("config-save")
                                .small()
                                .primary()
                                .flex_1()
                                .label(i18n_common(cx, "save"))
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    let v: SharedString = match kind {
                                        ConfigKind::Enum(_) => this.editing_enum.clone(),
                                        ConfigKind::Bool => if this.editing_bool { "yes" } else { "no" }.into(),
                                        ConfigKind::Number => this.number_state.read(cx).value(),
                                        ConfigKind::Text => this.edit_state.read(cx).value(),
                                    };
                                    let key = save_key.clone();
                                    let server_id = this.server_state.read(cx).server_id().to_string();
                                    let line = format!("CONFIG SET {} {}", key, v);
                                    let entity = cx.entity().downgrade();
                                    let value_for_run = v.clone();
                                    let key_for_run = key.clone();
                                    let run = move |_: &mut Window, cx: &mut App| {
                                        let Some(this) = entity.upgrade() else { return };
                                        let key = key_for_run.clone();
                                        let value = value_for_run.clone();
                                        this.update(cx, |this, cx| this.save_config(key, value, cx));
                                    };
                                    if let Ok(server) = get_server(&server_id) {
                                        confirm_dangerous_command(
                                            &server,
                                            &DangerKind::ConfigSet,
                                            Some(&line),
                                            window,
                                            cx,
                                            run,
                                        );
                                    } else {
                                        this.save_config(key, v, cx);
                                    }
                                })),
                        )
                        .child(
                            Button::new("config-cancel")
                                .small()
                                .ghost()
                                .flex_1()
                                .label(i18n_common(cx, "cancel"))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.editing_key = None;
                                    this.enum_select = None;
                                    cx.notify();
                                })),
                        ),
                )
                .into_any_element()
        } else {
            let edit_key = key.clone();
            let edit_value = value.clone();
            // Official redis.conf description for the `?` help popover, in the
            // app language (Chinese when the locale starts "zh"); `None` → no
            // `?` shown.
            let zh = cx.global::<ZedisGlobalStore>().read(cx).locale().starts_with("zh");
            let doc = config_doc(&key, zh);
            let value_el = if matches!(kind, ConfigKind::Bool) {
                let on = value.as_ref() == "yes";
                h_flex()
                    .items_center()
                    .gap_1p5()
                    .child(div().size_2().rounded_full().bg(if on { green } else { muted }))
                    .child(
                        Label::new(value.clone())
                            .text_sm()
                            .text_color(fg)
                            .font_family(font_family.clone()),
                    )
                    .into_any_element()
            } else {
                Label::new(value.clone())
                    .text_sm()
                    .text_ellipsis()
                    .text_color(fg)
                    .font_family(font_family.clone())
                    .into_any_element()
            };
            div()
                .id(SharedString::from(format!("config-card-{key}")))
                .flex_1()
                .min_w_0()
                .min_h(px(72.))
                .p_3()
                .border_1()
                .border_color(border)
                .rounded(radius)
                .bg(card_bg)
                .hover(|this| this.border_color(primary))
                .child(
                    h_flex()
                        .items_start()
                        .justify_between()
                        .gap_2()
                        .child(
                            div().min_w_0().overflow_hidden().child(
                                Label::new(key.clone())
                                    .text_sm()
                                    .text_color(muted)
                                    .font_family(font_family.clone()),
                            ),
                        )
                        .child(
                            h_flex()
                                .flex_none()
                                .items_center()
                                .gap_1()
                                // Per-parameter help: a `?` that opens a
                                // scrollable popover with the full official
                                // redis.conf description; shown only when one
                                // exists for this key.
                                .when_some(doc, |this, doc| {
                                    this.child(help_popover(SharedString::from(format!("config-help-{key}")), doc))
                                })
                                .child(
                                    Button::new(SharedString::from(format!("config-edit-{key}")))
                                        .xsmall()
                                        .ghost()
                                        .icon(Icon::new(CustomIconName::FilePenLine))
                                        .tooltip(i18n_config_editor(cx, "edit_tooltip"))
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            this.editing_key = Some(edit_key.clone());
                                            let kind = config_kind(&edit_key, &edit_value);
                                            match kind {
                                                ConfigKind::Enum(options) => {
                                                    this.build_enum_select(options, &edit_value, window, cx)
                                                }
                                                ConfigKind::Bool => this.editing_bool = edit_value.as_ref() == "yes",
                                                // Focus the input so the user can type at once — and so the
                                                // Esc-to-cancel capture handler is on the focus path.
                                                ConfigKind::Number => this.number_state.update(cx, |state, cx| {
                                                    state.set_value(edit_value.clone(), window, cx);
                                                    state.focus(window, cx);
                                                }),
                                                ConfigKind::Text => this.edit_state.update(cx, |state, cx| {
                                                    state.set_value(edit_value.clone(), window, cx);
                                                    state.focus(window, cx);
                                                }),
                                            }
                                            // Bool / enum have no text input to focus; focus the editor root
                                            // so Esc still reaches the capture handler above.
                                            if matches!(kind, ConfigKind::Bool | ConfigKind::Enum(_)) {
                                                this.focus_handle.focus(window, cx);
                                            }
                                            cx.notify();
                                        })),
                                ),
                        ),
                )
                .child(div().mt_2().overflow_hidden().child(value_el))
                .into_any_element()
        }
    }

    /// A titled section: a header (accent bar + mono label + description +
    /// count) over a responsive grid of parameter cards.
    fn render_group(
        &self,
        label: SharedString,
        desc: SharedString,
        configs: &[(SharedString, SharedString)],
        cols: u16,
        font_family: &SharedString,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let border = cx.theme().border;
        let muted = cx.theme().muted_foreground;
        let fg = cx.theme().foreground;
        let primary = cx.theme().primary;

        let mut cards: Vec<gpui::AnyElement> = Vec::with_capacity(configs.len());
        for (k, v) in configs {
            cards.push(self.render_card(k.clone(), v.clone(), font_family, cx));
        }

        v_flex()
            .w_full()
            .gap_3()
            .child(
                h_flex()
                    .items_center()
                    .gap_2()
                    .pb_2()
                    .border_b_1()
                    .border_color(border)
                    .child(div().w(px(3.)).h(px(14.)).rounded_sm().bg(primary))
                    .child(
                        Label::new(label)
                            .font_family(font_family.clone())
                            .font_weight(gpui::FontWeight::BOLD)
                            .text_color(fg),
                    )
                    .child(Label::new(desc).text_xs().text_color(muted))
                    .child(div().flex_1())
                    .child(
                        div()
                            .px_2()
                            .rounded_full()
                            .border_1()
                            .border_color(border)
                            .child(Label::new(configs.len().to_string()).text_xs().text_color(muted)),
                    ),
            )
            .child(div().grid().grid_cols(cols).items_start().gap_3().children(cards))
            .into_any_element()
    }
}

impl Render for ZedisConfigEditor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(notification) = self.pending_notification.take() {
            window.push_notification(notification, cx);
        }

        let font_family: SharedString = get_mono_font_family().into();
        let filter = self.filter.to_lowercase();

        let filtered: Vec<(SharedString, SharedString)> = self
            .configs
            .iter()
            .filter(|(k, _)| filter.is_empty() || k.to_lowercase().contains(&filter))
            .cloned()
            .collect();

        // `stripe_bg` is still used by the cross-server diff view below.
        let stripe_bg = cx.theme().table_even;

        // Group the filtered configs into ordered sections; anything not
        // matching a known group falls into the synthetic "others" section.
        let mut buckets: Vec<Vec<(SharedString, SharedString)>> = vec![Vec::new(); CONFIG_GROUPS.len()];
        let mut others: Vec<(SharedString, SharedString)> = Vec::new();
        for (k, v) in filtered {
            match config_group_index(&k) {
                Some(i) => buckets[i].push((k, v)),
                None => others.push((k, v)),
            }
        }
        // Responsive card-grid column count via the content-width proxy.
        let cols: u16 = cx
            .global::<ZedisGlobalStore>()
            .read(cx)
            .content_width()
            .map(|w| {
                let w = w.as_f32();
                if w > 1200. {
                    4
                } else if w > 900. {
                    3
                } else if w > 600. {
                    2
                } else {
                    1
                }
            })
            .unwrap_or(1);

        v_flex()
            .size_full()
            .overflow_hidden()
            .track_focus(&self.focus_handle)
            // While editing, declare the `ConfigEdit` key context so `escape`
            // maps to Cancel — deeper than the workspace's back binding, so it
            // wins; with no edit active there's no context and `escape` returns
            // to the editor as usual. Requires the editor to hold focus (the
            // edit handler focuses the input or this root).
            .when(self.editing_key.is_some(), |this| this.key_context("ConfigEdit"))
            .on_action(cx.listener(|this, _: &ConfigEditAction, _window, cx| {
                this.editing_key = None;
                this.enum_select = None;
                cx.notify();
            }))
            .child(
                h_flex()
                    .px_3()
                    .py_2()
                    .gap_2()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .items_center()
                    .child(
                        Button::new("config-back")
                            .ghost()
                            .small()
                            .icon(IconName::ArrowLeft)
                            .tooltip(i18n_common(cx, "back_to_editor"))
                            .on_click(|_, _w, cx| {
                                cx.update_global::<ZedisGlobalStore, ()>(|store, cx| {
                                    store.update(cx, |state, cx| state.go_to_view(ServerView::Editor, cx));
                                });
                            }),
                    )
                    .child(
                        Label::new(i18n_config_editor(cx, "title"))
                            .text_sm()
                            .font_family(font_family.clone()),
                    )
                    // Inline loading indicator in the header — more noticeable
                    // than a centered body spinner while `CONFIG GET *` is slow
                    // (covers both the initial load and a cross-server compare).
                    .when(self.loading || self.comparing, |this| {
                        this.child(
                            h_flex()
                                .items_center()
                                .gap_1p5()
                                .child(Spinner::new().with_size(px(14.)).color(cx.theme().muted_foreground))
                                .child(
                                    Label::new(i18n_common(cx, "loading"))
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground),
                                ),
                        )
                    })
                    .child(div().flex_1())
                    .child(
                        Button::new("config-reload")
                            .small()
                            .ghost()
                            .icon(Icon::new(CustomIconName::RotateCw))
                            .tooltip(i18n_common(cx, "reload"))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.editing_key = None;
                                this.enum_select = None;
                                this.load_configs(cx);
                            })),
                    )
                    .child(
                        Button::new("config-compare")
                            .small()
                            .ghost()
                            .icon(Icon::new(CustomIconName::GitCompareArrows))
                            .tooltip(i18n_config_editor(cx, "compare_tooltip"))
                            .on_click(cx.listener(|this, _, window, cx| this.open_compare_dialog(window, cx))),
                    )
                    .when(self.diff.is_some(), |this| {
                        this.child(
                            Button::new("config-exit-diff")
                                .small()
                                .ghost()
                                .label(i18n_config_editor(cx, "exit_diff"))
                                .on_click(cx.listener(|this, _, _, cx| this.close_diff(cx))),
                        )
                    })
                    .child(div().w(px(200.0)).child(Input::new(&self.filter_state).small())),
            )
            .child(if let Some(diff) = &self.diff {
                let border = cx.theme().border;
                let muted = cx.theme().muted_foreground;
                let fg = cx.theme().foreground;
                let hover_bg = cx.theme().table_hover;
                let filter = self.filter.to_lowercase();
                let filtered_diff: Vec<&(SharedString, SharedString, SharedString)> = diff
                    .rows
                    .iter()
                    .filter(|(k, _, _)| filter.is_empty() || k.to_lowercase().contains(&filter))
                    .collect();
                if filtered_diff.is_empty() {
                    div()
                        .flex_1()
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(Label::new(i18n_config_editor(cx, "no_diff")).text_color(muted))
                        .into_any_element()
                } else {
                    let header_row = h_flex()
                        .w_full()
                        .px_3()
                        .py_1()
                        .gap_2()
                        .border_b_1()
                        .border_color(border)
                        .child(
                            div()
                                .w(px(280.0))
                                .flex_none()
                                .child(Label::new(i18n_config_editor(cx, "param")).text_xs().text_color(muted)),
                        )
                        .child(
                            div()
                                .flex_1()
                                .child(Label::new(diff.local_label.clone()).text_xs().text_color(muted)),
                        )
                        .child(
                            div()
                                .flex_1()
                                .child(Label::new(diff.other_label.clone()).text_xs().text_color(muted)),
                        );
                    let diff_rows = filtered_diff.into_iter().enumerate().map(move |(i, (key, lv, ov))| {
                        let is_stripe = i % 2 != 0;
                        h_flex()
                            .id(("config-diff-row", i))
                            .w_full()
                            .px_3()
                            .py_1()
                            .gap_2()
                            .border_b_1()
                            .border_color(border)
                            .when(is_stripe, |this| this.bg(stripe_bg))
                            .hover(move |this| this.bg(hover_bg))
                            .child(
                                div()
                                    .w(px(280.0))
                                    .flex_none()
                                    .child(Label::new(key.clone()).text_sm().text_color(fg)),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .overflow_hidden()
                                    .child(Label::new(lv.clone()).text_sm().text_ellipsis().text_color(muted)),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .overflow_hidden()
                                    .child(Label::new(ov.clone()).text_sm().text_ellipsis().text_color(fg)),
                            )
                    });
                    div()
                        .id("config-diff-body")
                        .flex_1()
                        .w_full()
                        .min_h_0()
                        .overflow_y_scroll()
                        .child(header_row)
                        .children(diff_rows)
                        .into_any_element()
                }
            } else if self.configs.is_empty() && !self.loading {
                div()
                    .flex_1()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(if let Some(err) = &self.error {
                        Label::new(err.clone()).text_color(cx.theme().red)
                    } else {
                        Label::new(i18n_config_editor(cx, "no_data")).text_color(cx.theme().muted_foreground)
                    })
                    .into_any_element()
            } else {
                let mut sections = v_flex().w_full().gap_6().px_4().py_3();
                for (i, group) in CONFIG_GROUPS.iter().enumerate() {
                    if buckets[i].is_empty() {
                        continue;
                    }
                    sections = sections.child(self.render_group(
                        SharedString::from(group.label),
                        i18n_config_editor(cx, group.desc_key),
                        &buckets[i],
                        cols,
                        &font_family,
                        cx,
                    ));
                }
                if !others.is_empty() {
                    sections = sections.child(self.render_group(
                        i18n_config_editor(cx, "group_others"),
                        i18n_config_editor(cx, "group_others_desc"),
                        &others,
                        cols,
                        &font_family,
                        cx,
                    ));
                }
                div()
                    .id("config-editor-body")
                    .flex_1()
                    .w_full()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(sections)
                    .into_any_element()
            })
    }
}

#[cfg(test)]
mod tests {
    use super::{ConfigKind, config_enum_options, config_kind};

    #[test]
    fn config_kind_infers_from_value() {
        // yes/no → checkbox.
        assert!(matches!(config_kind("appendonly", "yes"), ConfigKind::Bool));
        assert!(matches!(config_kind("appendonly", "no"), ConfigKind::Bool));
        // Parseable number → numeric input.
        assert!(matches!(config_kind("maxmemory", "0"), ConfigKind::Number));
        assert!(matches!(config_kind("databases", "16"), ConfigKind::Number));
        // Paths / multi-segment / empty → plain text.
        assert!(matches!(config_kind("dir", "./"), ConfigKind::Text));
        assert!(matches!(config_kind("save", "3600 1 300 100"), ConfigKind::Text));
        assert!(matches!(config_kind("requirepass", ""), ConfigKind::Text));
    }

    #[test]
    fn config_kind_catalog_wins_over_value() {
        // Known enum params resolve to Enum even when their value would
        // otherwise infer as Text (or a number).
        assert!(matches!(
            config_kind("maxmemory-policy", "noeviction"),
            ConfigKind::Enum(_)
        ));
        assert!(matches!(config_kind("loglevel", "notice"), ConfigKind::Enum(_)));
    }

    #[test]
    fn config_enum_options_lookup() {
        // Unknown keys fall through (→ value inference).
        assert!(config_enum_options("definitely-not-a-config").is_none());

        let fsync = config_enum_options("appendfsync").expect("appendfsync is a known enum");
        assert_eq!(fsync, &["everysec", "always", "no"]);

        // loglevel includes the easy-to-miss `nothing`.
        let loglevel = config_enum_options("loglevel").expect("loglevel is a known enum");
        assert!(loglevel.contains(&"nothing"));

        // tls-auth-clients was added after the initial catalog.
        let tls = config_enum_options("tls-auth-clients").expect("tls-auth-clients is a known enum");
        assert_eq!(tls, &["no", "yes", "optional"]);

        // All 8 standard eviction policies.
        assert_eq!(config_enum_options("maxmemory-policy").map(|o| o.len()), Some(8));
    }
}
