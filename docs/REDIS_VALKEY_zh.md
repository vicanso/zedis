中文 | [English](./REDIS_VALKEY.md) · [← 返回 README](../README_zh.md)

# Redis 与 Valkey

Zedis 如实对待这两者：协议是同一套，版本线却有两条，从 7.2.4 分叉之后各自加了不同的功能。Valkey 在这里不是"兼容模式"：每个依赖服务器版本的功能，能不能用都按 Redis 和 Valkey 各自的版本分别判断（`crates/zedis-connection/src/floors.rs`），所以 Valkey 不会收到它那个版本没有的命令，Valkey 在比 Redis 更低的版本号上就有的功能也从那个版本起启用，而 Valkey 提供的功能只要适合放进图形界面，都有对应的面板或操作 —— 下表就是两者的全部差异，2026-09-26 对照 Redis 7.4 / 8.0 / 8.10 与 Valkey 8.0 / 8.1 / 9.0 / 9.1 的 `COMMAND LIST` 和 `COMMAND DOCS` 逐条核对过。剩下的只有 GUI 用不上的命令（`CLIENT CAPA`、`CLIENT IMPORT-SOURCE`、`DELIFEQ`、`MSETEX`、`CLUSTERSCAN`；Redis 那边的 `DELEX`、`HIMPORT`、`BACKUP` 之类），两边都不会发。集成测试在每次改动时跑 Redis 6.2 / 7.2 / 8.0、Valkey 8.0 / 9.0、`redis-stack` 和 `valkey-bundle`（Valkey 9.1 带全部模块）—— Valkey 固定用每条版本线的*首个*版本，因为版本判断如果写错，错就出在这里。

| 功能 | Redis | Valkey |
|---|---|---|
| `COMMANDLOG` —— 慢命令、大请求、大回复三种日志 | — | 8.1 |
| 原子槽迁移（Valkey 是 `CLUSTER MIGRATESLOTS`，Redis 是 `CLUSTER MIGRATION`—— 同一个 Reshard 页，背后是各自服务器的命令） | 8.4 | 9.0 |
| `CLUSTER SLOT-STATS`（每个槽的键数、CPU、网络） | 8.2 | 8.0 |
| 集群模式下的多数据库 | — | 9.0 |
| Hash 字段 TTL（`HEXPIRE`、`HSETEX`、`HTTL`…） | 7.4 | 9.0 |
| `SET … IFEQ` | 8.4 | 8.1 |
| `CLIENT KILL … MAXAGE` | 7.4 | 8.0 |
| `SCRIPT SHOW` —— 慢日志里 `EVALSHA` 背后的脚本源码 | — | 8.0 |
| 每个节点的可用区（`availability-zone`，拓扑页显示） | — | 8.1 |
| `BGSAVE CANCEL` —— 进行中的快照旁边有取消按钮 | — | 8.1 |
| 状态栏显示客户端暂停状态（`INFO clients` 的 `paused_actions`） | 只显示 Zedis 自己发起的 `CLIENT PAUSE` | 8.1 —— 无论谁暂停的都能看到 |
| 每类事件的平均延迟（`LATENCY LATEST` 的 sum / count） | 用 `LATENCY HISTORY` 最近 160 个样本取均值 | 8.1 —— 自上次重置以来的精确值 |
| `INFO keysizes` 直方图（内存分析） | 8.0 | — |
| `HOTKEYS` 热点跟踪 | 8.6 | — |
| Stream `XACKDEL` / `XDELEX` 及引用策略 | 8.2 | — |
| `XNACK` | 8.8 | — |
| Vector set | 8.0 | — |
| `allkeys-lrm` / `volatile-lrm` 淘汰策略 | 8.6 | — |
| JSON | RedisJSON | valkey-json |
| 搜索 | RediSearch | valkey-search —— 有 `FT.CREATE` / `SEARCH` / `AGGREGATE` / `INFO`；没有 `TAGVALS`、`SPELLCHECK`、`EXPLAIN`、`PROFILE` 和 `DROPINDEX DD`，面板会提示不可用。它的 `FT.AGGREGATE` 只对 `LOAD` 进工作集的字段分组（SORTABLE 在那里不是捷径），所以 Zedis 在 Valkey 上会先 LOAD 分组和归约用到的字段，RediSearch 上则不发 |
| 概率结构 | RedisBloom —— BF、CF、CMS、TOPK、TDIGEST | valkey-bloom —— BF |
| 时间序列 | RedisTimeSeries | — |

服务器没有的命令不会导致崩溃：需要它的面板会说明，其它一切照常。会让服务器崩溃的命令也不会发出：Redis 8.0–8.2.6 和 Valkey 8.0 在带 `CLIENT NO-TOUCH` 的客户端唤醒另一个阻塞客户端时会在 `lookupKey()` 里段错误，所以 Zedis 在这些版本上不设置该标志，代价只是内存分析里热度一列略欠精确。

**两者之间的复制。** `DUMP` 载荷带着写出它的服务器的 RDB 版本号，而两条线各编各的 —— Redis 7.4 写 12、Redis 8.10 写 15，Valkey 8 写 11、Valkey 9 写 80 —— 所以除了 Valkey 8 → Redis 之外，`RESTORE` 在每个方向上都会拒绝。遇到这种拒绝，Zedis 会改为从源端按类型重建（string、hash、list、set、sorted set、保留条目 id 的 stream、JSON），TTL 一并带过去，于是 Redis 与 Valkey 之间的迁移和单键复制两个方向都能成功，日志会标出哪些键是这样过去的。带不过去的会明确指出：Bloom filter、时间序列、vector set 没有通用的读取方式，stream 的消费组和 hash 的字段级 TTL 不属于值本身。`.zdis` 文件里存的是 `DUMP` 载荷，在另一种服务器上无法重建 —— 要用文件在两者之间搬键，请导出为 JSON。
