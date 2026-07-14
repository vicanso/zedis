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

//! Per-parameter help text for the CONFIG editor.
//!
//! Pure data: the full official `redis.conf` description for ~146 server
//! parameters, in English (verbatim upstream) and Chinese (translated),
//! keyed by parameter name. Kept out of `config_editor.rs` so the UI file
//! stays navigable — this table is ~1.1k lines on its own.

pub(crate) fn config_doc(key: &str, zh: bool) -> Option<&'static str> {
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
        "lazyfree-lazy-eviction" => (
            r#"Redis has two primitives to delete keys. One is called DEL and is a blocking deletion of the object. It means that the server stops processing new commands in order to reclaim all the memory associated with an object in a synchronous way. If the key deleted is associated with a small object, the time needed in order to execute the DEL command is very small and comparable to most other O(1) or O(log_N) commands in Redis. However if the key is associated with an aggregated value containing millions of elements, the server can block for a long time (even seconds) in order to complete the operation.

For the above reasons Redis also offers non blocking deletion primitives such as UNLINK (non blocking DEL) and the ASYNC option of FLUSHALL and FLUSHDB commands, in order to reclaim memory in background. Those commands are executed in constant time. Another thread will incrementally free the object in the background as fast as possible.

DEL, UNLINK and ASYNC option of FLUSHALL and FLUSHDB are user-controlled. It's up to the design of the application to understand when it is a good idea to use one or the other. However the Redis server sometimes has to delete keys or flush the whole database as a side effect of other operations. Specifically Redis deletes objects independently of a user call in the following scenarios:

1) On eviction, because of the maxmemory and maxmemory policy configurations, in order to make room for new data, without going over the specified memory limit.
2) Because of expire: when a key with an associated time to live (see the EXPIRE command) must be deleted from memory.
3) Because of a side effect of a command that stores data on a key that may already exist. For example the RENAME command may delete the old key content when it is replaced with another one. Similarly SUNIONSTORE or SORT with STORE option may delete existing keys. The SET command itself removes any old content of the specified key in order to replace it with the specified string.
4) During replication, when a replica performs a full resynchronization with its master, the content of the whole database is removed in order to load the RDB file just transferred.

In all the above cases the default is to delete objects in a blocking way, like if DEL was called. However you can configure each case specifically in order to instead release memory in a non-blocking way like if UNLINK was called, using the following configuration directives."#,
            r#"Redis 有两种删除键的原语。一种是 DEL，它对对象执行阻塞式删除，意味着服务器会停止处理新命令，以同步的方式回收该对象占用的全部内存。如果被删除的键关联的是一个小对象，执行 DEL 所需的时间非常短，与 Redis 中大多数 O(1) 或 O(log_N) 命令相当。但如果该键关联的是一个包含数百万个元素的聚合类型值，服务器可能会阻塞很长时间（甚至数秒）才能完成该操作。

出于上述原因，Redis 还提供了非阻塞的删除原语，例如 UNLINK（非阻塞版的 DEL），以及 FLUSHALL 和 FLUSHDB 命令的 ASYNC 选项，用于在后台回收内存。这些命令以常数时间执行，由另一个线程在后台尽可能快地逐步释放该对象。

DEL、UNLINK 以及 FLUSHALL 和 FLUSHDB 的 ASYNC 选项都由用户控制，何时使用哪一种取决于应用的设计。然而，Redis 服务器有时会因其他操作的副作用而不得不删除键或清空整个数据库。具体来说，在以下场景中 Redis 会在没有用户调用的情况下独立删除对象：

1) 发生淘汰（eviction）时：由于 maxmemory 和 maxmemory-policy 的配置，为了在不超出指定内存上限的前提下为新数据腾出空间。
2) 因为过期：当一个设置了生存时间（参见 EXPIRE 命令）的键必须从内存中删除时。
3) 作为某个命令的副作用：该命令向一个可能已存在的键写入数据。例如 RENAME 命令在用新内容替换旧键时可能会删除旧键的内容；类似地，SUNIONSTORE 或带 STORE 选项的 SORT 也可能删除已存在的键。SET 命令本身也会移除指定键的旧内容，以便用新的字符串替换它。
4) 在复制过程中：当副本与其主节点执行全量重同步时，整个数据库的内容会被清除，以便加载刚刚传输过来的 RDB 文件。

在上述所有情况下，默认都是以阻塞方式删除对象，就像调用了 DEL 一样。不过你可以针对每一种情况分别配置，改为以非阻塞的方式释放内存，就像调用了 UNLINK 一样，具体使用下面这些配置指令。"#,
        ),
        "lazyfree-lazy-expire" => (
            r#"Redis has two primitives to delete keys. One is called DEL and is a blocking deletion of the object. It means that the server stops processing new commands in order to reclaim all the memory associated with an object in a synchronous way. If the key deleted is associated with a small object, the time needed in order to execute the DEL command is very small and comparable to most other O(1) or O(log_N) commands in Redis. However if the key is associated with an aggregated value containing millions of elements, the server can block for a long time (even seconds) in order to complete the operation.

For the above reasons Redis also offers non blocking deletion primitives such as UNLINK (non blocking DEL) and the ASYNC option of FLUSHALL and FLUSHDB commands, in order to reclaim memory in background. Those commands are executed in constant time. Another thread will incrementally free the object in the background as fast as possible.

DEL, UNLINK and ASYNC option of FLUSHALL and FLUSHDB are user-controlled. It's up to the design of the application to understand when it is a good idea to use one or the other. However the Redis server sometimes has to delete keys or flush the whole database as a side effect of other operations. Specifically Redis deletes objects independently of a user call in the following scenarios:

1) On eviction, because of the maxmemory and maxmemory policy configurations, in order to make room for new data, without going over the specified memory limit.
2) Because of expire: when a key with an associated time to live (see the EXPIRE command) must be deleted from memory.
3) Because of a side effect of a command that stores data on a key that may already exist. For example the RENAME command may delete the old key content when it is replaced with another one. Similarly SUNIONSTORE or SORT with STORE option may delete existing keys. The SET command itself removes any old content of the specified key in order to replace it with the specified string.
4) During replication, when a replica performs a full resynchronization with its master, the content of the whole database is removed in order to load the RDB file just transferred.

In all the above cases the default is to delete objects in a blocking way, like if DEL was called. However you can configure each case specifically in order to instead release memory in a non-blocking way like if UNLINK was called, using the following configuration directives."#,
            r#"Redis 有两种删除键的原语。一种是 DEL，它对对象执行阻塞式删除，意味着服务器会停止处理新命令，以同步的方式回收该对象占用的全部内存。如果被删除的键关联的是一个小对象，执行 DEL 所需的时间非常短，与 Redis 中大多数 O(1) 或 O(log_N) 命令相当。但如果该键关联的是一个包含数百万个元素的聚合类型值，服务器可能会阻塞很长时间（甚至数秒）才能完成该操作。

出于上述原因，Redis 还提供了非阻塞的删除原语，例如 UNLINK（非阻塞版的 DEL），以及 FLUSHALL 和 FLUSHDB 命令的 ASYNC 选项，用于在后台回收内存。这些命令以常数时间执行，由另一个线程在后台尽可能快地逐步释放该对象。

DEL、UNLINK 以及 FLUSHALL 和 FLUSHDB 的 ASYNC 选项都由用户控制，何时使用哪一种取决于应用的设计。然而，Redis 服务器有时会因其他操作的副作用而不得不删除键或清空整个数据库。具体来说，在以下场景中 Redis 会在没有用户调用的情况下独立删除对象：

1) 发生淘汰（eviction）时：由于 maxmemory 和 maxmemory-policy 的配置，为了在不超出指定内存上限的前提下为新数据腾出空间。
2) 因为过期：当一个设置了生存时间（参见 EXPIRE 命令）的键必须从内存中删除时。
3) 作为某个命令的副作用：该命令向一个可能已存在的键写入数据。例如 RENAME 命令在用新内容替换旧键时可能会删除旧键的内容；类似地，SUNIONSTORE 或带 STORE 选项的 SORT 也可能删除已存在的键。SET 命令本身也会移除指定键的旧内容，以便用新的字符串替换它。
4) 在复制过程中：当副本与其主节点执行全量重同步时，整个数据库的内容会被清除，以便加载刚刚传输过来的 RDB 文件。

在上述所有情况下，默认都是以阻塞方式删除对象，就像调用了 DEL 一样。不过你可以针对每一种情况分别配置，改为以非阻塞的方式释放内存，就像调用了 UNLINK 一样，具体使用下面这些配置指令。"#,
        ),
        "lazyfree-lazy-server-del" => (
            r#"Redis has two primitives to delete keys. One is called DEL and is a blocking deletion of the object. It means that the server stops processing new commands in order to reclaim all the memory associated with an object in a synchronous way. If the key deleted is associated with a small object, the time needed in order to execute the DEL command is very small and comparable to most other O(1) or O(log_N) commands in Redis. However if the key is associated with an aggregated value containing millions of elements, the server can block for a long time (even seconds) in order to complete the operation.

For the above reasons Redis also offers non blocking deletion primitives such as UNLINK (non blocking DEL) and the ASYNC option of FLUSHALL and FLUSHDB commands, in order to reclaim memory in background. Those commands are executed in constant time. Another thread will incrementally free the object in the background as fast as possible.

DEL, UNLINK and ASYNC option of FLUSHALL and FLUSHDB are user-controlled. It's up to the design of the application to understand when it is a good idea to use one or the other. However the Redis server sometimes has to delete keys or flush the whole database as a side effect of other operations. Specifically Redis deletes objects independently of a user call in the following scenarios:

1) On eviction, because of the maxmemory and maxmemory policy configurations, in order to make room for new data, without going over the specified memory limit.
2) Because of expire: when a key with an associated time to live (see the EXPIRE command) must be deleted from memory.
3) Because of a side effect of a command that stores data on a key that may already exist. For example the RENAME command may delete the old key content when it is replaced with another one. Similarly SUNIONSTORE or SORT with STORE option may delete existing keys. The SET command itself removes any old content of the specified key in order to replace it with the specified string.
4) During replication, when a replica performs a full resynchronization with its master, the content of the whole database is removed in order to load the RDB file just transferred.

In all the above cases the default is to delete objects in a blocking way, like if DEL was called. However you can configure each case specifically in order to instead release memory in a non-blocking way like if UNLINK was called, using the following configuration directives."#,
            r#"Redis 有两种删除键的原语。一种是 DEL，它对对象执行阻塞式删除，意味着服务器会停止处理新命令，以同步的方式回收该对象占用的全部内存。如果被删除的键关联的是一个小对象，执行 DEL 所需的时间非常短，与 Redis 中大多数 O(1) 或 O(log_N) 命令相当。但如果该键关联的是一个包含数百万个元素的聚合类型值，服务器可能会阻塞很长时间（甚至数秒）才能完成该操作。

出于上述原因，Redis 还提供了非阻塞的删除原语，例如 UNLINK（非阻塞版的 DEL），以及 FLUSHALL 和 FLUSHDB 命令的 ASYNC 选项，用于在后台回收内存。这些命令以常数时间执行，由另一个线程在后台尽可能快地逐步释放该对象。

DEL、UNLINK 以及 FLUSHALL 和 FLUSHDB 的 ASYNC 选项都由用户控制，何时使用哪一种取决于应用的设计。然而，Redis 服务器有时会因其他操作的副作用而不得不删除键或清空整个数据库。具体来说，在以下场景中 Redis 会在没有用户调用的情况下独立删除对象：

1) 发生淘汰（eviction）时：由于 maxmemory 和 maxmemory-policy 的配置，为了在不超出指定内存上限的前提下为新数据腾出空间。
2) 因为过期：当一个设置了生存时间（参见 EXPIRE 命令）的键必须从内存中删除时。
3) 作为某个命令的副作用：该命令向一个可能已存在的键写入数据。例如 RENAME 命令在用新内容替换旧键时可能会删除旧键的内容；类似地，SUNIONSTORE 或带 STORE 选项的 SORT 也可能删除已存在的键。SET 命令本身也会移除指定键的旧内容，以便用新的字符串替换它。
4) 在复制过程中：当副本与其主节点执行全量重同步时，整个数据库的内容会被清除，以便加载刚刚传输过来的 RDB 文件。

在上述所有情况下，默认都是以阻塞方式删除对象，就像调用了 DEL 一样。不过你可以针对每一种情况分别配置，改为以非阻塞的方式释放内存，就像调用了 UNLINK 一样，具体使用下面这些配置指令。"#,
        ),
        "lazyfree-lazy-user-del" => (
            r#"It is also possible, for the case when to replace the user code DEL calls with UNLINK calls is not easy, to modify the default behavior of the DEL command to act exactly like UNLINK, using the following configuration directive."#,
            r#"如果难以把用户代码中的 DEL 调用替换成 UNLINK 调用，也可以通过下面这条配置指令，把 DEL 命令的默认行为修改为与 UNLINK 完全一致。"#,
        ),
        "lazyfree-lazy-user-flush" => (
            r#"FLUSHDB, FLUSHALL, SCRIPT FLUSH and FUNCTION FLUSH support both asynchronous and synchronous deletion, which can be controlled by passing the [SYNC|ASYNC] flags into the commands. When neither flag is passed, this directive will be used to determine if the data should be deleted asynchronously."#,
            r#"FLUSHDB、FLUSHALL、SCRIPT FLUSH 和 FUNCTION FLUSH 同时支持异步和同步删除，可通过在命令中传入 [SYNC|ASYNC] 标志来控制。当两个标志都未传入时，将由本指令决定是否以异步方式删除数据。"#,
        ),
        "replica-lazy-flush" => (
            r#"Redis has two primitives to delete keys. One is called DEL and is a blocking deletion of the object. It means that the server stops processing new commands in order to reclaim all the memory associated with an object in a synchronous way. If the key deleted is associated with a small object, the time needed in order to execute the DEL command is very small and comparable to most other O(1) or O(log_N) commands in Redis. However if the key is associated with an aggregated value containing millions of elements, the server can block for a long time (even seconds) in order to complete the operation.

For the above reasons Redis also offers non blocking deletion primitives such as UNLINK (non blocking DEL) and the ASYNC option of FLUSHALL and FLUSHDB commands, in order to reclaim memory in background. Those commands are executed in constant time. Another thread will incrementally free the object in the background as fast as possible.

DEL, UNLINK and ASYNC option of FLUSHALL and FLUSHDB are user-controlled. It's up to the design of the application to understand when it is a good idea to use one or the other. However the Redis server sometimes has to delete keys or flush the whole database as a side effect of other operations. Specifically Redis deletes objects independently of a user call in the following scenarios:

1) On eviction, because of the maxmemory and maxmemory policy configurations, in order to make room for new data, without going over the specified memory limit.
2) Because of expire: when a key with an associated time to live (see the EXPIRE command) must be deleted from memory.
3) Because of a side effect of a command that stores data on a key that may already exist. For example the RENAME command may delete the old key content when it is replaced with another one. Similarly SUNIONSTORE or SORT with STORE option may delete existing keys. The SET command itself removes any old content of the specified key in order to replace it with the specified string.
4) During replication, when a replica performs a full resynchronization with its master, the content of the whole database is removed in order to load the RDB file just transferred.

In all the above cases the default is to delete objects in a blocking way, like if DEL was called. However you can configure each case specifically in order to instead release memory in a non-blocking way like if UNLINK was called, using the following configuration directives."#,
            r#"Redis 有两种删除键的原语。一种是 DEL，它对对象执行阻塞式删除，意味着服务器会停止处理新命令，以同步的方式回收该对象占用的全部内存。如果被删除的键关联的是一个小对象，执行 DEL 所需的时间非常短，与 Redis 中大多数 O(1) 或 O(log_N) 命令相当。但如果该键关联的是一个包含数百万个元素的聚合类型值，服务器可能会阻塞很长时间（甚至数秒）才能完成该操作。

出于上述原因，Redis 还提供了非阻塞的删除原语，例如 UNLINK（非阻塞版的 DEL），以及 FLUSHALL 和 FLUSHDB 命令的 ASYNC 选项，用于在后台回收内存。这些命令以常数时间执行，由另一个线程在后台尽可能快地逐步释放该对象。

DEL、UNLINK 以及 FLUSHALL 和 FLUSHDB 的 ASYNC 选项都由用户控制，何时使用哪一种取决于应用的设计。然而，Redis 服务器有时会因其他操作的副作用而不得不删除键或清空整个数据库。具体来说，在以下场景中 Redis 会在没有用户调用的情况下独立删除对象：

1) 发生淘汰（eviction）时：由于 maxmemory 和 maxmemory-policy 的配置，为了在不超出指定内存上限的前提下为新数据腾出空间。
2) 因为过期：当一个设置了生存时间（参见 EXPIRE 命令）的键必须从内存中删除时。
3) 作为某个命令的副作用：该命令向一个可能已存在的键写入数据。例如 RENAME 命令在用新内容替换旧键时可能会删除旧键的内容；类似地，SUNIONSTORE 或带 STORE 选项的 SORT 也可能删除已存在的键。SET 命令本身也会移除指定键的旧内容，以便用新的字符串替换它。
4) 在复制过程中：当副本与其主节点执行全量重同步时，整个数据库的内容会被清除，以便加载刚刚传输过来的 RDB 文件。

在上述所有情况下，默认都是以阻塞方式删除对象，就像调用了 DEL 一样。不过你可以针对每一种情况分别配置，改为以非阻塞的方式释放内存，就像调用了 UNLINK 一样，具体使用下面这些配置指令。"#,
        ),
        "notify-keyspace-events" => (
            r#"Redis can notify Pub/Sub clients about events happening in the key space. This feature is documented at https://redis.io/docs/latest/develop/use/keyspace-notifications/

For instance if keyspace events notification is enabled, and a client performs a DEL operation on key "foo" stored in the Database 0, two messages will be published via Pub/Sub:

PUBLISH __keyspace@0__:foo del
PUBLISH __keyevent@0__:del foo

It is possible to select the events that Redis will notify among a set of classes. Every class is identified by a single character:

K     Keyspace events, published with __keyspace@<db>__ prefix.
E     Keyevent events, published with __keyevent@<db>__ prefix.
g     Generic commands (non-type specific) like DEL, EXPIRE, RENAME, ...
$     String commands
l     List commands
s     Set commands
h     Hash commands
z     Sorted set commands
x     Expired events (events generated every time a key expires)
e     Evicted events (events generated when a key is evicted for maxmemory)
n     New key events (Note: not included in the 'A' class)
t     Stream commands
d     Module key type events
m     Key-miss events (Note: It is not included in the 'A' class)
o     Overwritten events generated every time a key is overwritten.
      (Note: not included in the 'A' class)
c     Type-changed events generated every time a key's type changes
      (Note: not included in the 'A' class)
A     Alias for g$lshzxetd, so that the "AKE" string means all the events
      except key-miss, new key, overwritten and type-changed.

The "notify-keyspace-events" takes as argument a string that is composed of zero or multiple characters. The empty string means that notifications are disabled.

Example: to enable list and generic events, from the point of view of the event name, use:

notify-keyspace-events Elg

Example 2: to get the stream of the expired keys subscribing to channel name __keyevent@0__:expired use:

notify-keyspace-events Ex

By default all notifications are disabled because most users don't need this feature and the feature has some overhead. Note that if you don't specify at least one of K or E, no events will be delivered."#,
            r#"Redis 可以把键空间中发生的事件通知给 Pub/Sub 客户端。该特性的文档见 https://redis.io/docs/latest/develop/use/keyspace-notifications/

例如，如果启用了键空间事件通知，某个客户端对存储在 Database 0 中的键「foo」执行了 DEL 操作，则会通过 Pub/Sub 发布两条消息：

PUBLISH __keyspace@0__:foo del
PUBLISH __keyevent@0__:del foo

可以从一组事件类别中选择希望 Redis 通知的事件，每个类别由一个字符标识：

K     键空间（Keyspace）事件，以 __keyspace@<db>__ 为前缀发布。
E     键事件（Keyevent）事件，以 __keyevent@<db>__ 为前缀发布。
g     通用命令（与类型无关），如 DEL、EXPIRE、RENAME 等。
$     String 命令
l     List 命令
s     Set 命令
h     Hash 命令
z     Sorted set 命令
x     过期事件（每当一个键过期时产生）
e     淘汰事件（因 maxmemory 而淘汰某个键时产生）
n     新键事件（注意：不包含在 'A' 类别中）
t     Stream 命令
d     模块（Module）键类型事件
m     键未命中（key-miss）事件（注意：不包含在 'A' 类别中）
o     覆写事件，每当一个键被覆写时产生。
      （注意：不包含在 'A' 类别中）
c     类型变更事件，每当一个键的类型发生变化时产生。
      （注意：不包含在 'A' 类别中）
A     g$lshzxetd 的别名，因此字符串「AKE」表示除 key-miss、新键、覆写和类型变更之外的所有事件。

「notify-keyspace-events」的参数是一个由零个或多个字符组成的字符串。空字符串表示禁用通知。

示例：从事件名的角度启用 list 与通用事件，使用：

notify-keyspace-events Elg

示例 2：订阅频道 __keyevent@0__:expired 以获取过期键的事件流，使用：

notify-keyspace-events Ex

默认情况下所有通知都是关闭的，因为大多数用户并不需要该特性，且它有一定开销。注意，如果不至少指定 K 或 E 中的一个，将不会有任何事件被投递。"#,
        ),
        "tracking-table-max-keys" => (
            r#"Redis implements server assisted support for client side caching of values. This is implemented using an invalidation table that remembers, using a radix key indexed by key name, what clients have which keys. In turn this is used in order to send invalidation messages to clients. Please check this page to understand more about the feature:

  https://redis.io/docs/latest/develop/use/client-side-caching/

When tracking is enabled for a client, all the read only queries are assumed to be cached: this will force Redis to store information in the invalidation table. When keys are modified, such information is flushed away, and invalidation messages are sent to the clients. However if the workload is heavily dominated by reads, Redis could use more and more memory in order to track the keys fetched by many clients.

For this reason it is possible to configure a maximum fill value for the invalidation table. By default it is set to 1M of keys, and once this limit is reached, Redis will start to evict keys in the invalidation table even if they were not modified, just to reclaim memory: this will in turn force the clients to invalidate the cached values. Basically the table maximum size is a trade off between the memory you want to spend server side to track information about who cached what, and the ability of clients to retain cached objects in memory.

If you set the value to 0, it means there are no limits, and Redis will retain as many keys as needed in the invalidation table. In the "stats" INFO section, you can find information about the number of keys in the invalidation table at every given moment.

Note: when key tracking is used in broadcasting mode, no memory is used in the server side so this setting is useless."#,
            r#"Redis 实现了服务端辅助的客户端缓存（client side caching）支持。其实现方式是使用一张失效表（invalidation table），通过以键名索引的基数树记录哪些客户端缓存了哪些键，进而用它向客户端发送失效消息。关于该特性的更多信息请查看：

  https://redis.io/docs/latest/develop/use/client-side-caching/

当为某个客户端启用 tracking 后，所有只读查询都会被认为已被缓存：这会迫使 Redis 在失效表中保存相关信息。当这些键被修改时，这些信息会被清除，并向客户端发送失效消息。然而，如果工作负载以读为主，Redis 可能会为了追踪众多客户端所读取的键而占用越来越多的内存。

因此，可以为失效表配置一个最大填充值。默认设置为 1M 个键，一旦达到该上限，Redis 就会开始淘汰失效表中的键（即便它们并未被修改），只是为了回收内存：这反过来会迫使客户端让其缓存的值失效。本质上，该表的最大大小是在服务端为记录「谁缓存了什么」所花费的内存，与客户端在内存中保留缓存对象的能力之间的一种权衡。

如果将该值设为 0，则表示没有限制，Redis 会在失效表中保留所需数量的键。在 INFO 的「stats」小节中，可以查看任意时刻失效表中键的数量。

注意：当键追踪以广播（broadcasting）模式使用时，服务端不会占用内存，因此该配置没有作用。"#,
        ),
        "client-query-buffer-limit" => (
            r#"Client query buffers accumulate new commands. They are limited to a fixed amount by default in order to avoid that a protocol desynchronization (for instance due to a bug in the client) will lead to unbound memory usage in the query buffer. However you can configure it here if you have very special needs, such as a command with huge argument, or huge multi/exec requests or alike."#,
            r#"客户端查询缓冲区用于累积新到达的命令。默认情况下其大小被限制为一个固定值，以避免协议不同步（例如客户端存在 bug）导致查询缓冲区无限制地占用内存。不过，如果你有非常特殊的需求，例如某个命令带有超大参数、或者存在超大的 multi/exec 请求之类的情况，可以在这里进行配置。"#,
        ),
        "databases" => (
            r#"Set the number of databases. The default database is DB 0, you can select a different one on a per-connection basis using SELECT <dbid> where dbid is a number between 0 and 'databases'-1"#,
            r#"设置数据库的数量。默认数据库是 DB 0，你可以按连接使用 SELECT <dbid> 选择其他数据库，其中 dbid 是介于 0 和 'databases'-1 之间的数字。"#,
        ),
        "hz" => (
            r#"Redis calls an internal function to perform many background tasks, like closing connections of clients in timeout, purging expired keys that are never requested, and so forth.

Not all tasks are performed with the same frequency, but Redis checks for tasks to perform according to the specified "hz" value.

By default "hz" is set to 10. Raising the value will use more CPU when Redis is idle, but at the same time will make Redis more responsive when there are many keys expiring at the same time, and timeouts may be handled with more precision.

The range is between 1 and 500, however a value over 100 is usually not a good idea. Most users should use the default of 10 and raise this up to 100 only in environments where very low latency is required."#,
            r#"Redis 会调用一个内部函数来执行许多后台任务，例如关闭超时客户端的连接、清理那些从未被访问的过期键等等。

并非所有任务都以相同的频率执行，但 Redis 会按照所设定的「hz」值来检查需要执行的任务。

「hz」默认值为 10。调高该值会让 Redis 在空闲时消耗更多 CPU，但同时也会让 Redis 在大量键同时过期时反应更迅速，超时的处理也更精确。

取值范围为 1 到 500，不过超过 100 通常不是个好主意。大多数用户应使用默认值 10，只有在对延迟要求极低的环境中才把它调高到 100。"#,
        ),
        "dynamic-hz" => (
            r#"Normally it is useful to have an HZ value which is proportional to the number of clients connected. This is useful in order, for instance, to avoid too many clients are processed for each background task invocation in order to avoid latency spikes.

Since the default HZ value by default is conservatively set to 10, Redis offers, and enables by default, the ability to use an adaptive HZ value which will temporarily raise when there are many connected clients.

When dynamic HZ is enabled, the actual configured HZ will be used as a baseline, but multiples of the configured HZ value will be actually used as needed once more clients are connected. In this way an idle instance will use very little CPU time while a busy instance will be more responsive."#,
            r#"通常，让 HZ 值与已连接客户端的数量成正比是有好处的。例如，这样可以避免每次后台任务调用要处理过多客户端，从而避免延迟尖峰。

由于默认的 HZ 值被保守地设为 10，Redis 提供并默认启用了自适应 HZ 的能力：当连接的客户端很多时，HZ 会被临时调高。

启用动态 HZ 后，配置的 HZ 会作为基准值，随着客户端增多，实际使用的会是该配置值的若干倍。这样一来，空闲实例只消耗极少的 CPU 时间，而繁忙实例则更具响应性。"#,
        ),
        "active-expire-effort" => (
            r#"Redis reclaims expired keys in two ways: upon access when those keys are found to be expired, and also in background, in what is called the "active expire key". The key space is slowly and interactively scanned looking for expired keys to reclaim, so that it is possible to free memory of keys that are expired and will never be accessed again in a short time.

The default effort of the expire cycle will try to avoid having more than ten percent of expired keys still in memory, and will try to avoid consuming more than 25% of total memory and to add latency to the system. However it is possible to increase the expire "effort" that is normally set to "1", to a greater value, up to the value "10". At its maximum value the system will use more CPU, longer cycles (and technically may introduce more latency), and will tolerate less already expired keys still present in the system. It's a tradeoff between memory, CPU and latency."#,
            r#"Redis 通过两种方式回收过期键：一是在访问时发现键已过期，二是在后台进行，即所谓的「active expire key」（主动过期）。Redis 会缓慢地、交替地扫描键空间，寻找可回收的过期键，从而释放那些已过期且短期内不会再被访问的键所占的内存。

过期周期的默认「effort」会尽量避免内存中残留超过百分之十的过期键，同时尽量避免占用超过 25% 的总内存以及给系统增加延迟。不过，可以把通常设为「1」的过期「effort」调大，最高可到「10」。取最大值时，系统会消耗更多 CPU、使用更长的周期（严格来说可能引入更多延迟），但对系统中残留的已过期键的容忍度更低。这是在内存、CPU 和延迟之间的权衡。"#,
        ),
        "lfu-log-factor" => (
            r#"Redis LFU eviction (see maxmemory setting) can be tuned. However it is a good idea to start with the default settings and only change them after investigating how to improve the performances and how the keys LFU change over time, which is possible to inspect via the OBJECT FREQ command.

There are two tunable parameters in the Redis LFU implementation: the counter logarithm factor and the counter decay time. It is important to understand what the two parameters mean before changing them.

The LFU counter is just 8 bits per key, it's maximum value is 255, so Redis uses a probabilistic increment with logarithmic behavior. Given the value of the old counter, when a key is accessed, the counter is incremented in this way:

1. A random number R between 0 and 1 is extracted.
2. A probability P is calculated as 1/(old_value*lfu_log_factor+1).
3. The counter is incremented only if R < P.

The default lfu-log-factor is 10. This is a table of how the frequency counter changes with a different number of accesses with different logarithmic factors:

+--------+------------+------------+------------+------------+------------+
| factor | 100 hits   | 1000 hits  | 100K hits  | 1M hits    | 10M hits   |
+--------+------------+------------+------------+------------+------------+
| 0      | 104        | 255        | 255        | 255        | 255        |
+--------+------------+------------+------------+------------+------------+
| 1      | 18         | 49         | 255        | 255        | 255        |
+--------+------------+------------+------------+------------+------------+
| 10     | 10         | 18         | 142        | 255        | 255        |
+--------+------------+------------+------------+------------+------------+
| 100    | 8          | 11         | 49         | 143        | 255        |
+--------+------------+------------+------------+------------+------------+

NOTE: The above table was obtained by running the following commands:

  redis-benchmark -n 1000000 incr foo
  redis-cli object freq foo

NOTE 2: The counter initial value is 5 in order to give new objects a chance to accumulate hits.

The counter decay time is the time, in minutes, that must elapse in order for the key counter to be decremented.

The default value for the lfu-decay-time is 1. A special value of 0 means we will never decay the counter."#,
            r#"Redis 的 LFU 淘汰策略（参见 maxmemory 设置）是可以调优的。不过建议先从默认设置开始，只有在研究清楚如何提升性能、以及键的 LFU 值随时间如何变化之后再做调整；后者可以通过 OBJECT FREQ 命令来观察。

Redis 的 LFU 实现有两个可调参数：计数器的对数因子（counter logarithm factor）和计数器的衰减时间（counter decay time）。修改之前，务必先理解这两个参数的含义。

LFU 计数器每个键只占 8 位，最大值为 255，因此 Redis 采用具有对数特性的概率式递增。给定旧的计数器值，当某个键被访问时，计数器按如下方式递增：

1. 抽取一个 0 到 1 之间的随机数 R。
2. 按 1/(old_value*lfu_log_factor+1) 计算出概率 P。
3. 只有当 R < P 时才递增计数器。

lfu-log-factor 默认为 10。下表展示了在不同对数因子下，频率计数器随访问次数变化的情况：

+--------+------------+------------+------------+------------+------------+
| factor | 100 hits   | 1000 hits  | 100K hits  | 1M hits    | 10M hits   |
+--------+------------+------------+------------+------------+------------+
| 0      | 104        | 255        | 255        | 255        | 255        |
+--------+------------+------------+------------+------------+------------+
| 1      | 18         | 49         | 255        | 255        | 255        |
+--------+------------+------------+------------+------------+------------+
| 10     | 10         | 18         | 142        | 255        | 255        |
+--------+------------+------------+------------+------------+------------+
| 100    | 8          | 11         | 49         | 143        | 255        |
+--------+------------+------------+------------+------------+------------+

注意：上表是通过运行以下命令得到的：

  redis-benchmark -n 1000000 incr foo
  redis-cli object freq foo

注意 2：计数器的初始值为 5，目的是给新对象一个积累命中的机会。

计数器衰减时间是指键的计数器被递减前必须经过的时间，单位为分钟。

lfu-decay-time 的默认值为 1。特殊值 0 表示永不衰减计数器。"#,
        ),
        "lfu-decay-time" => (
            r#"Redis LFU eviction (see maxmemory setting) can be tuned. However it is a good idea to start with the default settings and only change them after investigating how to improve the performances and how the keys LFU change over time, which is possible to inspect via the OBJECT FREQ command.

There are two tunable parameters in the Redis LFU implementation: the counter logarithm factor and the counter decay time. It is important to understand what the two parameters mean before changing them.

The LFU counter is just 8 bits per key, it's maximum value is 255, so Redis uses a probabilistic increment with logarithmic behavior. Given the value of the old counter, when a key is accessed, the counter is incremented in this way:

1. A random number R between 0 and 1 is extracted.
2. A probability P is calculated as 1/(old_value*lfu_log_factor+1).
3. The counter is incremented only if R < P.

The default lfu-log-factor is 10. This is a table of how the frequency counter changes with a different number of accesses with different logarithmic factors:

+--------+------------+------------+------------+------------+------------+
| factor | 100 hits   | 1000 hits  | 100K hits  | 1M hits    | 10M hits   |
+--------+------------+------------+------------+------------+------------+
| 0      | 104        | 255        | 255        | 255        | 255        |
+--------+------------+------------+------------+------------+------------+
| 1      | 18         | 49         | 255        | 255        | 255        |
+--------+------------+------------+------------+------------+------------+
| 10     | 10         | 18         | 142        | 255        | 255        |
+--------+------------+------------+------------+------------+------------+
| 100    | 8          | 11         | 49         | 143        | 255        |
+--------+------------+------------+------------+------------+------------+

NOTE: The above table was obtained by running the following commands:

  redis-benchmark -n 1000000 incr foo
  redis-cli object freq foo

NOTE 2: The counter initial value is 5 in order to give new objects a chance to accumulate hits.

The counter decay time is the time, in minutes, that must elapse in order for the key counter to be decremented.

The default value for the lfu-decay-time is 1. A special value of 0 means we will never decay the counter."#,
            r#"Redis 的 LFU 淘汰策略（参见 maxmemory 设置）是可以调优的。不过建议先从默认设置开始，只有在研究清楚如何提升性能、以及键的 LFU 值随时间如何变化之后再做调整；后者可以通过 OBJECT FREQ 命令来观察。

Redis 的 LFU 实现有两个可调参数：计数器的对数因子（counter logarithm factor）和计数器的衰减时间（counter decay time）。修改之前，务必先理解这两个参数的含义。

LFU 计数器每个键只占 8 位，最大值为 255，因此 Redis 采用具有对数特性的概率式递增。给定旧的计数器值，当某个键被访问时，计数器按如下方式递增：

1. 抽取一个 0 到 1 之间的随机数 R。
2. 按 1/(old_value*lfu_log_factor+1) 计算出概率 P。
3. 只有当 R < P 时才递增计数器。

lfu-log-factor 默认为 10。下表展示了在不同对数因子下，频率计数器随访问次数变化的情况：

+--------+------------+------------+------------+------------+------------+
| factor | 100 hits   | 1000 hits  | 100K hits  | 1M hits    | 10M hits   |
+--------+------------+------------+------------+------------+------------+
| 0      | 104        | 255        | 255        | 255        | 255        |
+--------+------------+------------+------------+------------+------------+
| 1      | 18         | 49         | 255        | 255        | 255        |
+--------+------------+------------+------------+------------+------------+
| 10     | 10         | 18         | 142        | 255        | 255        |
+--------+------------+------------+------------+------------+------------+
| 100    | 8          | 11         | 49         | 143        | 255        |
+--------+------------+------------+------------+------------+------------+

注意：上表是通过运行以下命令得到的：

  redis-benchmark -n 1000000 incr foo
  redis-cli object freq foo

注意 2：计数器的初始值为 5，目的是给新对象一个积累命中的机会。

计数器衰减时间是指键的计数器被递减前必须经过的时间，单位为分钟。

lfu-decay-time 的默认值为 1。特殊值 0 表示永不衰减计数器。"#,
        ),
        "aof-rewrite-incremental-fsync" => (
            r#"When a child rewrites the AOF file, if the following option is enabled the file will be fsync-ed every 4 MB of data generated. This is useful in order to commit the file to the disk more incrementally and avoid big latency spikes."#,
            r#"当子进程重写 AOF 文件时，若启用该选项，则每生成 4 MB 数据就会对文件执行一次 fsync。这有助于更增量地把文件落盘，避免出现大的延迟尖峰。"#,
        ),
        "rdb-save-incremental-fsync" => (
            r#"When redis saves RDB file, if the following option is enabled the file will be fsync-ed every 4 MB of data generated. This is useful in order to commit the file to the disk more incrementally and avoid big latency spikes."#,
            r#"当 Redis 保存 RDB 文件时，若启用该选项，则每生成 4 MB 数据就会对文件执行一次 fsync。这有助于更增量地把文件落盘，避免出现大的延迟尖峰。"#,
        ),
        "disable-thp" => (
            r#"Usually the kernel Transparent Huge Pages control is set to "madvise" or "never" by default (/sys/kernel/mm/transparent_hugepage/enabled), in which case this config has no effect. On systems in which it is set to "always", redis will attempt to disable it specifically for the redis process in order to avoid latency problems specifically with fork(2) and CoW. If for some reason you prefer to keep it enabled, you can set this config to "no" and the kernel global to "always"."#,
            r#"内核的透明大页（Transparent Huge Pages）开关通常默认为「madvise」或「never」（见 /sys/kernel/mm/transparent_hugepage/enabled），此时该配置不起作用。在被设为「always」的系统上，Redis 会尝试仅针对 Redis 进程禁用它，以避免 fork(2) 和写时复制（CoW）带来的延迟问题。如果出于某些原因你希望保持其启用，可以把该配置设为「no」，并把内核全局设置保持为「always」。"#,
        ),
        "jemalloc-bg-thread" => (
            r#"Jemalloc background thread for purging will be enabled by default."#,
            r#"用于内存回收（purging）的 jemalloc 后台线程默认启用。"#,
        ),
        "hide-user-data-from-log" => (
            r#"Avoid logging personal identifiable information (PII) into the server log file. When enabled, Redis omits user data (such as command arguments and key or value contents) from log messages and crash reports, keeping only the non-sensitive parts. Defaults to no."#,
            r#"避免把个人可识别信息（PII）写入服务器日志文件。启用后，Redis 会在日志消息和崩溃报告中省略用户数据（例如命令参数以及键或值的内容），只保留不敏感的部分。默认值为 no。"#,
        ),
        "repl-disable-tcp-nodelay" => (
            r#"Disable TCP_NODELAY on the replica socket after SYNC?

If you select "yes" Redis will use a smaller number of TCP packets and less bandwidth to send data to replicas. But this can add a delay for the data to appear on the replica side, up to 40 milliseconds with Linux kernels using a default configuration.

If you select "no" the delay for data to appear on the replica side will be reduced but more bandwidth will be used for replication.

By default we optimize for low latency, but in very high traffic conditions or when the master and replicas are many hops away, turning this to "yes" may be a good idea."#,
            r#"SYNC 之后是否在副本连接的 socket 上禁用 TCP_NODELAY？

如果设置为 yes，Redis 会用更少的 TCP 包、更少的带宽把数据发送给副本。但这会增加数据出现在副本端的延迟，在使用默认配置的 Linux 内核上最多可达 40 毫秒。

如果设置为 no，数据出现在副本端的延迟会降低，但复制会占用更多带宽。

默认情况下我们优先保证低延迟；但在流量非常高，或者主节点与副本之间网络跳数很多的情况下，把它设为 yes 可能是个好主意。"#,
        ),
        "repl-diskless-sync-max-replicas" => (
            r#"When diskless replication is enabled with a delay, it is possible to let the replication start before the maximum delay is reached if the maximum number of replicas expected have connected. Default of 0 means that the maximum is not defined and Redis will wait the full delay."#,
            r#"当启用了带延迟的无盘复制（diskless replication）时，如果预期的最大副本数量已经全部连接上来，就可以在达到最大延迟之前提前开始复制。默认值 0 表示不设定该上限，Redis 会等满整个延迟时间。"#,
        ),
        "replica-ignore-maxmemory" => (
            r#"Starting from Redis 5, by default a replica will ignore its maxmemory setting (unless it is promoted to master after a failover or manually). It means that the eviction of keys will be just handled by the master, sending the DEL commands to the replica as keys evict in the master side.

This behavior ensures that masters and replicas stay consistent, and is usually what you want, however if your replica is writable, or you want the replica to have a different memory setting, and you are sure all the writes performed to the replica are idempotent, then you may change this default (but be sure to understand what you are doing).

Note that since the replica by default does not evict, it may end using more memory than the one set via maxmemory (there are certain buffers that may be larger on the replica, or data structures may sometimes take more memory and so forth). So make sure you monitor your replicas and make sure they have enough memory to never hit a real out-of-memory condition before the master hits the configured maxmemory setting."#,
            r#"从 Redis 5 开始，副本默认会忽略自身的 maxmemory 设置（除非它在故障转移后或被手动提升为主节点）。这意味着键的淘汰完全由主节点处理：主节点在淘汰键时会把 DEL 命令发送给副本。

这种行为保证了主从之间的一致性，通常也正是你想要的。但如果你的副本是可写的，或者你希望副本使用不同的内存设置，并且你确定所有对副本的写入都是幂等的，那么可以修改这个默认值（但请务必清楚自己在做什么）。

请注意，由于副本默认不做淘汰，它最终占用的内存可能超过 maxmemory 所设定的值（副本上的某些缓冲区可能更大，数据结构有时也会占用更多内存等等）。因此请监控你的副本，确保它们有足够的内存，不会在主节点达到配置的 maxmemory 之前就先触发真正的内存不足（OOM）。"#,
        ),
        "replicaof" => (
            r#"Master-Replica replication. Use replicaof to make a Redis instance a copy of another Redis server. A few things to understand ASAP about Redis replication.

  +------------------+      +---------------+
  |      Master      | ---> |    Replica    |
  | (receive writes) |      |  (exact copy) |
  +------------------+      +---------------+

1) Redis replication is asynchronous, but you can configure a master to stop accepting writes if it appears to be not connected with at least a given number of replicas.
2) Redis replicas are able to perform a partial resynchronization with the master if the replication link is lost for a relatively small amount of time. You may want to configure the replication backlog size (see the next sections of this file) with a sensible value depending on your needs.
3) Replication is automatic and does not need user intervention. After a network partition replicas automatically try to reconnect to masters and resynchronize with them."#,
            r#"主从（Master-Replica）复制。使用 replicaof 可以让一个 Redis 实例成为另一个 Redis 服务器的副本。关于 Redis 复制，有几点需要尽快了解。

  +------------------+      +---------------+
  |      Master      | ---> |    Replica    |
  | (receive writes) |      |  (exact copy) |
  +------------------+      +---------------+

1) Redis 复制是异步的，但你可以配置主节点：当它与至少给定数量的副本失去连接时，停止接受写入。
2) 如果复制链路中断的时间相对较短，Redis 副本能够与主节点执行部分重同步（partial resynchronization）。你可以根据需要，为复制积压缓冲区（replication backlog）配置一个合理的大小（参见本文件后续章节）。
3) 复制是自动进行的，无需用户干预。网络分区结束后，副本会自动尝试重新连接主节点并与之重新同步。"#,
        ),
        "replica-announce-ip" => (
            r#"A Redis master is able to list the address and port of the attached replicas in different ways. For example the "INFO replication" section offers this information, which is used, among other tools, by Redis Sentinel in order to discover replica instances. Another place where this info is available is in the output of the "ROLE" command of a master.

The listed IP address and port normally reported by a replica is obtained in the following way:

  IP: The address is auto detected by checking the peer address of the socket used by the replica to connect with the master.

  Port: The port is communicated by the replica during the replication handshake, and is normally the port that the replica is using to listen for connections.

However when port forwarding or Network Address Translation (NAT) is used, the replica may actually be reachable via different IP and port pairs. The following two options can be used by a replica in order to report to its master a specific set of IP and port, so that both INFO and ROLE will report those values.

There is no need to use both the options if you need to override just the port or the IP address."#,
            r#"Redis 主节点可以通过多种方式列出已连接副本的地址和端口。例如「INFO replication」这一段就提供了这些信息，Redis Sentinel 等工具正是借此来发现副本实例的。另一个可以获取该信息的地方是主节点上「ROLE」命令的输出。

副本通常上报的 IP 地址和端口是按以下方式获得的：

  IP：通过检查副本连接主节点所用 socket 的对端地址自动探测得出。

  Port：由副本在复制握手过程中告知主节点，通常就是副本用来监听连接的端口。

然而当使用端口转发或网络地址转换（NAT）时，副本实际上可能要通过不同的 IP 与端口组合才能访问。副本可以使用下面这两个选项，向主节点上报指定的 IP 和端口，这样 INFO 和 ROLE 就会报告这些值。

如果你只需要覆盖端口或 IP 地址其中之一，则不必同时使用这两个选项。"#,
        ),
        "replica-announce-port" => (
            r#"A Redis master is able to list the address and port of the attached replicas in different ways. For example the "INFO replication" section offers this information, which is used, among other tools, by Redis Sentinel in order to discover replica instances. Another place where this info is available is in the output of the "ROLE" command of a master.

The listed IP address and port normally reported by a replica is obtained in the following way:

  IP: The address is auto detected by checking the peer address of the socket used by the replica to connect with the master.

  Port: The port is communicated by the replica during the replication handshake, and is normally the port that the replica is using to listen for connections.

However when port forwarding or Network Address Translation (NAT) is used, the replica may actually be reachable via different IP and port pairs. The following two options can be used by a replica in order to report to its master a specific set of IP and port, so that both INFO and ROLE will report those values.

There is no need to use both the options if you need to override just the port or the IP address."#,
            r#"Redis 主节点可以通过多种方式列出已连接副本的地址和端口。例如「INFO replication」这一段就提供了这些信息，Redis Sentinel 等工具正是借此来发现副本实例的。另一个可以获取该信息的地方是主节点上「ROLE」命令的输出。

副本通常上报的 IP 地址和端口是按以下方式获得的：

  IP：通过检查副本连接主节点所用 socket 的对端地址自动探测得出。

  Port：由副本在复制握手过程中告知主节点，通常就是副本用来监听连接的端口。

然而当使用端口转发或网络地址转换（NAT）时，副本实际上可能要通过不同的 IP 与端口组合才能访问。副本可以使用下面这两个选项，向主节点上报指定的 IP 和端口，这样 INFO 和 ROLE 就会报告这些值。

如果你只需要覆盖端口或 IP 地址其中之一，则不必同时使用这两个选项。"#,
        ),
        "shutdown-timeout" => (
            r#"Maximum time to wait for replicas when shutting down, in seconds.

During shut down, a grace period allows any lagging replicas to catch up with the latest replication offset before the master exists. This period can prevent data loss, especially for deployments without configured disk backups.

The 'shutdown-timeout' value is the grace period's duration in seconds. It is only applicable when the instance has replicas. To disable the feature, set the value to 0."#,
            r#"关闭时等待副本的最长时间，单位为秒。

在关闭过程中，会有一段宽限期，让落后的副本在主节点退出前追上最新的复制偏移量。这段时间可以避免数据丢失，对于未配置磁盘备份的部署尤其重要。

「shutdown-timeout」的取值就是这段宽限期的秒数。它仅在实例拥有副本时才生效。将该值设为 0 可禁用此特性。"#,
        ),
        "shutdown-on-sigint" => (
            r#"When Redis receives a SIGINT or SIGTERM, shutdown is initiated and by default an RDB snapshot is written to disk in a blocking operation if save points are configured.
The options used on signaled shutdown can include the following values:
default:  Saves RDB snapshot only if save points are configured.
          Waits for lagging replicas to catch up.
save:     Forces a DB saving operation even if no save points are configured.
nosave:   Prevents DB saving operation even if one or more save points are configured.
now:      Skips waiting for lagging replicas.
force:    Ignores any errors that would normally prevent the server from exiting.

Any combination of values is allowed as long as "save" and "nosave" are not set simultaneously.
Example: "nosave force now""#,
            r#"当 Redis 收到 SIGINT 或 SIGTERM 时，会启动关闭流程；如果配置了 save 存盘点，默认会以阻塞方式把一份 RDB 快照写入磁盘。
信号关闭时可使用的选项包括以下取值：
default：仅在配置了 save 存盘点时才保存 RDB 快照。
         并等待落后的副本追上进度。
save：   即使没有配置 save 存盘点，也强制执行一次存盘操作。
nosave： 即使配置了一个或多个 save 存盘点，也不执行存盘操作。
now：    跳过对落后副本的等待。
force：  忽略那些通常会阻止服务器退出的错误。

只要不同时设置 save 和 nosave，任意取值组合都是允许的。
例如：「nosave force now」"#,
        ),
        "shutdown-on-sigterm" => (
            r#"When Redis receives a SIGINT or SIGTERM, shutdown is initiated and by default an RDB snapshot is written to disk in a blocking operation if save points are configured.
The options used on signaled shutdown can include the following values:
default:  Saves RDB snapshot only if save points are configured.
          Waits for lagging replicas to catch up.
save:     Forces a DB saving operation even if no save points are configured.
nosave:   Prevents DB saving operation even if one or more save points are configured.
now:      Skips waiting for lagging replicas.
force:    Ignores any errors that would normally prevent the server from exiting.

Any combination of values is allowed as long as "save" and "nosave" are not set simultaneously.
Example: "nosave force now""#,
            r#"当 Redis 收到 SIGINT 或 SIGTERM 时，会启动关闭流程；如果配置了 save 存盘点，默认会以阻塞方式把一份 RDB 快照写入磁盘。
信号关闭时可使用的选项包括以下取值：
default：仅在配置了 save 存盘点时才保存 RDB 快照。
         并等待落后的副本追上进度。
save：   即使没有配置 save 存盘点，也强制执行一次存盘操作。
nosave： 即使配置了一个或多个 save 存盘点，也不执行存盘操作。
now：    跳过对落后副本的等待。
force：  忽略那些通常会阻止服务器退出的错误。

只要不同时设置 save 和 nosave，任意取值组合都是允许的。
例如：「nosave force now」"#,
        ),
        "oom-score-adj" => (
            r#"On Linux, it is possible to hint the kernel OOM killer on what processes should be killed first when out of memory.

Enabling this feature makes Redis actively control the oom_score_adj value for all its processes, depending on their role. The default scores will attempt to have background child processes killed before all others, and replicas killed before masters.

Redis supports these options:

no:       Don't make changes to oom-score-adj (default).
yes:      Alias to 'relative' see below.
absolute: Values in oom-score-adj-values are written as is to the kernel.
relative: Values are used relative to the initial value of oom_score_adj when the server starts and are then clamped to a range of -1000 to 1000. Because typically the initial value is 0, they will often match the absolute values."#,
            r#"在 Linux 上，可以向内核的 OOM killer 提示：内存耗尽时应优先杀死哪些进程。

启用该功能后，Redis 会根据各进程的角色主动控制它们的 oom_score_adj 值。默认的评分策略会让后台子进程先于其他进程被杀死，并让副本先于主节点被杀死。

Redis 支持以下取值：

no：       不修改 oom-score-adj（默认）。
yes：      「relative」的别名，见下文。
absolute： oom-score-adj-values 中的值原样写入内核。
relative： 这些值相对于服务器启动时 oom_score_adj 的初始值来使用，并会被限制在 -1000 到 1000 的范围内。由于初始值通常为 0，它们往往与 absolute 的结果一致。"#,
        ),
        "oom-score-adj-values" => (
            r#"When oom-score-adj is used, this directive controls the specific values used for master, replica and background child processes. Values range -2000 to 2000 (higher means more likely to be killed).

Unprivileged processes (not root, and without CAP_SYS_RESOURCE capabilities) can freely increase their value, but not decrease it below its initial settings. This means that setting oom-score-adj to 'relative' and setting the oom-score-adj-values to positive values will always succeed."#,
            r#"当启用 oom-score-adj 时，该指令控制用于主节点、副本和后台子进程的具体数值。取值范围为 -2000 到 2000（数值越大越容易被杀死）。

非特权进程（非 root，且不具备 CAP_SYS_RESOURCE 能力）可以自由调高自己的值，但不能将其降到初始设置以下。这意味着将 oom-score-adj 设为「relative」并把 oom-score-adj-values 设为正值总是能成功。"#,
        ),
        "daemonize" => (
            r#"By default Redis does not run as a daemon. Use 'yes' if you need it. Note that Redis will write a pid file in /var/run/redis.pid when daemonized. When Redis is supervised by upstart or systemd, this parameter has no impact."#,
            r#"默认情况下 Redis 不以守护进程方式运行。如有需要请设为 yes。注意，以守护进程方式运行时，Redis 会在 /var/run/redis.pid 写入 pid 文件。当 Redis 由 upstart 或 systemd 托管时，该参数不起作用。"#,
        ),
        "pidfile" => (
            r#"If a pid file is specified, Redis writes it where specified at startup and removes it at exit.

When the server runs non daemonized, no pid file is created if none is specified in the configuration. When the server is daemonized, the pid file is used even if not specified, defaulting to '/var/run/redis.pid'.

Creating a pid file is best effort: if Redis is not able to create it nothing bad happens, the server will start and run normally.

Note that on modern Linux systems '/run/redis.pid' is more conforming and should be used instead."#,
            r#"如果指定了 pid 文件，Redis 会在启动时将其写入指定位置，并在退出时删除它。

当服务器不以守护进程方式运行时，若配置中未指定 pid 文件，则不会创建 pid 文件。当服务器以守护进程方式运行时，即使未指定也会使用 pid 文件，默认为「/var/run/redis.pid」。

创建 pid 文件是尽力而为的：如果 Redis 无法创建它也不会有什么问题，服务器仍会正常启动并运行。

注意，在现代 Linux 系统上「/run/redis.pid」更符合规范，应改用该路径。"#,
        ),
        "supervised" => (
            r#"If you run Redis from upstart or systemd, Redis can interact with your supervision tree. Options:

supervised no      - no supervision interaction
supervised upstart - signal upstart by putting Redis into SIGSTOP mode; requires 'expect stop' in your upstart job config
supervised systemd - signal systemd by writing READY=1 to $NOTIFY_SOCKET on startup, and updating Redis status on a regular basis.
supervised auto    - detect upstart or systemd method based on UPSTART_JOB or NOTIFY_SOCKET environment variables

Note: these supervision methods only signal 'process is ready.' They do not enable continuous pings back to your supervisor.

The default is 'no'. To run under upstart/systemd, you can simply use: supervised auto"#,
            r#"如果通过 upstart 或 systemd 启动 Redis，Redis 可以与你的监管树（supervision tree）交互。可选值：

supervised no      - 不与监管系统交互
supervised upstart - 通过让 Redis 进入 SIGSTOP 模式来通知 upstart；需要在 upstart 任务配置中写入「expect stop」
supervised systemd - 启动时向 $NOTIFY_SOCKET 写入 READY=1 来通知 systemd，并定期更新 Redis 状态。
supervised auto    - 根据 UPSTART_JOB 或 NOTIFY_SOCKET 环境变量自动检测使用 upstart 还是 systemd 方式

注意：这些监管方式只会发出「进程已就绪」的信号，并不会持续向监管进程发送心跳。

默认值为 no。若要在 upstart/systemd 下运行，只需设置：supervised auto"#,
        ),
        "cluster-announce-ip" => (
            r#"In certain deployments, Redis Cluster nodes address discovery fails, because addresses are NAT-ted or because ports are forwarded (the typical case is Docker and other containers).

In order to make Redis Cluster working in such environments, a static configuration where each node knows its public address is needed. The following four options are used for this scope, and are:

* cluster-announce-ip
* cluster-announce-port
* cluster-announce-tls-port
* cluster-announce-bus-port

Each instructs the node about its address, client ports (for connections without and with TLS) and cluster message bus port. The information is then published in the header of the bus packets so that other nodes will be able to correctly map the address of the node publishing the information.

If tls-cluster is set to yes and cluster-announce-tls-port is omitted or set to zero, then cluster-announce-port refers to the TLS port. Note also that cluster-announce-tls-port has no effect if tls-cluster is set to no.

If the above options are not used, the normal Redis Cluster auto-detection will be used instead.

Note that when remapped, the bus port may not be at the fixed offset of clients port + 10000, so you can specify any port and bus-port depending on how they get remapped. If the bus-port is not set, a fixed offset of 10000 will be used as usual."#,
            r#"在某些部署环境中，Redis Cluster 节点的地址发现会失败，因为地址经过了 NAT 转换，或者端口被转发（典型场景是 Docker 和其他容器）。

为了让 Redis Cluster 在这类环境中正常工作，需要一份静态配置，让每个节点都知道自己的公开地址。为此提供了以下四个选项：

* cluster-announce-ip
* cluster-announce-port
* cluster-announce-tls-port
* cluster-announce-bus-port

它们分别告知节点自己的地址、客户端端口（分别对应非 TLS 与 TLS 连接）以及集群消息总线端口。这些信息随后会发布在总线数据包的头部，以便其他节点能够正确映射发布该信息的节点地址。

如果 tls-cluster 设为 yes，而 cluster-announce-tls-port 未设置或设为 0，那么 cluster-announce-port 指的就是 TLS 端口。另外注意，若 tls-cluster 设为 no，则 cluster-announce-tls-port 不起作用。

如果不使用上述选项，则会沿用 Redis Cluster 常规的自动探测机制。

注意，端口被重映射后，总线端口未必仍是「客户端端口 + 10000」这一固定偏移，因此你可以根据实际的映射情况任意指定 port 与 bus-port。如果未设置 bus-port，仍会照常使用 10000 的固定偏移。"#,
        ),
        "cluster-announce-port" => (
            r#"In certain deployments, Redis Cluster nodes address discovery fails, because addresses are NAT-ted or because ports are forwarded (the typical case is Docker and other containers).

In order to make Redis Cluster working in such environments, a static configuration where each node knows its public address is needed. The following four options are used for this scope, and are:

* cluster-announce-ip
* cluster-announce-port
* cluster-announce-tls-port
* cluster-announce-bus-port

Each instructs the node about its address, client ports (for connections without and with TLS) and cluster message bus port. The information is then published in the header of the bus packets so that other nodes will be able to correctly map the address of the node publishing the information.

If tls-cluster is set to yes and cluster-announce-tls-port is omitted or set to zero, then cluster-announce-port refers to the TLS port. Note also that cluster-announce-tls-port has no effect if tls-cluster is set to no.

If the above options are not used, the normal Redis Cluster auto-detection will be used instead.

Note that when remapped, the bus port may not be at the fixed offset of clients port + 10000, so you can specify any port and bus-port depending on how they get remapped. If the bus-port is not set, a fixed offset of 10000 will be used as usual."#,
            r#"在某些部署环境中，Redis Cluster 节点的地址发现会失败，因为地址经过了 NAT 转换，或者端口被转发（典型场景是 Docker 和其他容器）。

为了让 Redis Cluster 在这类环境中正常工作，需要一份静态配置，让每个节点都知道自己的公开地址。为此提供了以下四个选项：

* cluster-announce-ip
* cluster-announce-port
* cluster-announce-tls-port
* cluster-announce-bus-port

它们分别告知节点自己的地址、客户端端口（分别对应非 TLS 与 TLS 连接）以及集群消息总线端口。这些信息随后会发布在总线数据包的头部，以便其他节点能够正确映射发布该信息的节点地址。

如果 tls-cluster 设为 yes，而 cluster-announce-tls-port 未设置或设为 0，那么 cluster-announce-port 指的就是 TLS 端口。另外注意，若 tls-cluster 设为 no，则 cluster-announce-tls-port 不起作用。

如果不使用上述选项，则会沿用 Redis Cluster 常规的自动探测机制。

注意，端口被重映射后，总线端口未必仍是「客户端端口 + 10000」这一固定偏移，因此你可以根据实际的映射情况任意指定 port 与 bus-port。如果未设置 bus-port，仍会照常使用 10000 的固定偏移。"#,
        ),
        "cluster-announce-bus-port" => (
            r#"In certain deployments, Redis Cluster nodes address discovery fails, because addresses are NAT-ted or because ports are forwarded (the typical case is Docker and other containers).

In order to make Redis Cluster working in such environments, a static configuration where each node knows its public address is needed. The following four options are used for this scope, and are:

* cluster-announce-ip
* cluster-announce-port
* cluster-announce-tls-port
* cluster-announce-bus-port

Each instructs the node about its address, client ports (for connections without and with TLS) and cluster message bus port. The information is then published in the header of the bus packets so that other nodes will be able to correctly map the address of the node publishing the information.

If tls-cluster is set to yes and cluster-announce-tls-port is omitted or set to zero, then cluster-announce-port refers to the TLS port. Note also that cluster-announce-tls-port has no effect if tls-cluster is set to no.

If the above options are not used, the normal Redis Cluster auto-detection will be used instead.

Note that when remapped, the bus port may not be at the fixed offset of clients port + 10000, so you can specify any port and bus-port depending on how they get remapped. If the bus-port is not set, a fixed offset of 10000 will be used as usual."#,
            r#"在某些部署环境中，Redis Cluster 节点的地址发现会失败，因为地址经过了 NAT 转换，或者端口被转发（典型场景是 Docker 和其他容器）。

为了让 Redis Cluster 在这类环境中正常工作，需要一份静态配置，让每个节点都知道自己的公开地址。为此提供了以下四个选项：

* cluster-announce-ip
* cluster-announce-port
* cluster-announce-tls-port
* cluster-announce-bus-port

它们分别告知节点自己的地址、客户端端口（分别对应非 TLS 与 TLS 连接）以及集群消息总线端口。这些信息随后会发布在总线数据包的头部，以便其他节点能够正确映射发布该信息的节点地址。

如果 tls-cluster 设为 yes，而 cluster-announce-tls-port 未设置或设为 0，那么 cluster-announce-port 指的就是 TLS 端口。另外注意，若 tls-cluster 设为 no，则 cluster-announce-tls-port 不起作用。

如果不使用上述选项，则会沿用 Redis Cluster 常规的自动探测机制。

注意，端口被重映射后，总线端口未必仍是「客户端端口 + 10000」这一固定偏移，因此你可以根据实际的映射情况任意指定 port 与 bus-port。如果未设置 bus-port，仍会照常使用 10000 的固定偏移。"#,
        ),
        "cluster-allow-replica-migration" => (
            r#"Turning off this option allows to use less automatic cluster configuration. It both disables migration to orphaned masters and migration from masters that became empty.

Default is 'yes' (allow automatic migrations)."#,
            r#"关闭该选项可以减少集群的自动配置行为。它会同时禁止副本迁移到孤立的主节点，以及从已变空的主节点迁出。

默认值为 yes（允许自动迁移）。"#,
        ),
        "cluster-replica-no-failover" => (
            r#"This option, when set to yes, prevents replicas from trying to failover its master during master failures. However the replica can still perform a manual failover, if forced to do so.

This is useful in different scenarios, especially in the case of multiple data center operations, where we want one side to never be promoted if not in the case of a total DC failure."#,
            r#"该选项设为 yes 时，会阻止副本在主节点故障期间尝试对其主节点执行故障转移。不过，如果被强制要求，副本仍然可以执行手动故障转移。

这在多种场景下都很有用，尤其是多数据中心运维的场景：我们希望某一侧永远不被提升为主节点，除非整个数据中心发生故障。"#,
        ),
        _ => return None,
    };
    Some(if zh { cn } else { en })
}
