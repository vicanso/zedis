中文 | [English](./FEATURES.md) · [← 返回 README](../README_zh.md)

# Zedis —— 完整功能巡览

Zedis 自动检测（`ViewerMode::Auto`）并实时格式化你的数据。本页是详尽参考；概览见 [README](../README_zh.md)。

---

## 🚀 原生 & 快

### 原生 GPU 渲染
**每个像素都在 GPU 上绘制、虚拟滚动 `SCAN`，百万级键也保持 60+ FPS、极低内存。**

虚拟滚动结合 `SCAN` 迭代，无论 keyspace 多大都保持响应——滚动零延迟、标签页秒切换、内存占用极低。

### 跨平台
**macOS、Windows、Linux 均为原生体验，支持浅色 / 深色 / 跟随系统。**

在三大桌面平台上都有真正的原生质感。

---

## 🧠 智能数据查看器

### 自动解压缩
**透明解包 LZ4、SNAPPY、GZIP、ZSTD。**

压缩值就地解包，你看到的是真实内容而非二进制 blob。

### 丰富内容解码
**JSON/RedisJSON、Protobuf、MessagePack、Unix 时间戳、媒体 & Hex。**

**JSON & RedisJSON** 内置美化、语法高亮与最小化 `JSON.MERGE` 差异（RFC 7396）——**JSONPath**（`$.user.email`、`$.items[?(@.price > 100)]`）对普通 string 键也适用，无需模块。**MessagePack** 零配置反序列化，**Protobuf** 在你注册 `.proto` schema 后解码（按服务器以精确 / 前缀 / 后缀 / 正则匹配键名）；10/13 位 **Unix 时间戳** 预览为本地 + UTC；**图片**（`PNG/JPG/WEBP/SVG/GIF`）与完全可编辑的 **Hex** 视图一应俱全。

### 自定义脚本查看器
**通过外部 Shell 命令对任意值做自定义解码。**

配置含占位符（`{KEY}`、`{VALUE}`、`{HEX}`、`{RAW_FILE}`）的命令模板，Zedis 用 `sh -c` / `cmd /c` 执行并将 stdout 作为格式化结果，按服务器精确 / 前缀 / 后缀 / 正则匹配键名。

### 解码管线
**顺序明确且可预期——你配置的查看器永远优先。**

每个字符串值都会经过同一条管线,优先级从高到低:

1. **Protobuf 查看器**——匹配该键的已注册 `.proto` schema
2. **自定义脚本查看器**——匹配该键的已配置脚本
3. **原生格式检测**——MessagePack · GZIP · ZSTD · Snappy · Unix 时间戳 · 图片(`PNG/JPG/WEBP/SVG/GIF`)
4. **LZ4**(带长度前缀)——针对非 UTF-8 数据
5. **文本 / 美化 JSON** 兜底

查看器匹配但解码失败时会回退到原生处理,坏脚本不会让值显示空白。手动切换:**hex** 视图始终展示原始字节,小体积不透明字符串还提供 **bitmap** 视图开关。

---

## 🗂️ 类型 & 模块查看器

### 专项类型查看器
**不透明的值都会打开为专用的交互式查看器。**

**位图/Bitfield** 在 GPU 网格上绘制每一位（`SETBIT`/`BITCOUNT`/`BITFIELD`）；**HyperLogLog** 展示 `PFCOUNT` 基数；**向量集 + KNN**（Redis 8）经 `VSIM` 逐跳遍历 HNSW 图；**地理地图** 把 sorted set 画在无底图雷达上并支持 `GEOSEARCH`；**概率型**（RedisBloom：Bloom/Cuckoo/Count-Min/Top-K/t-digest）与**时间序列**（RedisTimeSeries `TS.INFO` + 分桶 `TS.RANGE` 图表）各有专卡。按 key 的类型 / 模块分发。

### 模块面板
**RediSearch（FT.*）与 Functions（Lua library）的专用面板。**

**RediSearch**：列出 / 查看索引，配 chip 运行 `FT.SEARCH` / `FT.AGGREGATE`，表单创建 / alter / drop。**Functions**（Redis 7+）：经 `FUNCTION LIST/LOAD/DELETE` 管理 library，带 tree-sitter Lua 编辑器、起步模板、直接 **`FCALL`** 调用，以及 `DUMP` / `RESTORE` / `FLUSH` / `STATS`。模块 / 版本不满足时自动隐藏。

### Redis Streams
**浏览、实时跟踪、管理消费者组，全程不离开 GUI。**

浏览流条目、**实时跟踪**新消息（`XREAD BLOCK`，环形缓冲）、经 `XINFO` 查看消费者组与待处理消息，并管理消费者组（`XGROUP CREATE` / `SETID` / `DESTROY`，销毁带确认）。

### Pub/Sub
**订阅频道、发布消息，带实时消息日志。**

模式订阅（`PSUBSCRIBE`）、`PUBLISH` 消息编辑器，收到的消息流入环形缓冲的 `(time, channel, message)` 表格——Redis 中与 Streams 并列的另一种消息机制。**分片**开关（Redis 7+）切换为 `SSUBSCRIBE` / `SPUBLISH`——集群模式下消息按频道哈希槽路由而非广播到所有节点，走独立的 RESP3 推送连接，故障转移后自动重新订阅。

---

## 📊 实时可观测性

内置 GPU 加速仪表盘，彻底改变你监控 Redis 实例的方式。

### 实时指标
**CPU、内存、网络 I/O 的实时图表——并保留 7 天历史。**

精美渲染的 GPU 加速时序图表。采样同时落盘（每分钟一条，保留 7 天），1h / 24h / 7d 时间范围即使重启应用也能回答"昨晚内存有没有涨"。

### 内存分析器 + 体检建议
**排查 BigKey、查看 TTL 分布、获取离线体检与可选 AI 建议。**

Top-N 表按 **大小 / 最热 / 最冷** 排序（按 `maxmemory-policy` 自动选 `OBJECT FREQ`/`IDLETIME`），并配 **TTL 直方图**。扫描一结束，**离线规则引擎** 即自动给出体检建议 —— 大 key、`volatile-*` 策略下无法淘汰的键、`noeviction`、内存碎片偏高、应合并为 Hash 的大量小 string、占用大部分内存的前缀 —— 零配置、零网络。也可一键将报告（只含 key *名称*、大小、TTL，绝不含 value）发送到任意 **OpenAI 兼容** 接口，用当前界面语言内联返回建议。

完全不想碰生产环境？**分析 RDB** 可离线解析本地备份文件 —— 流式解析器（支持到 Redis 7.4 的全部编码，值按长度跳过，多 GB 文件以 I/O 速度解析）喂给同一套表格、TTL 直方图和规则引擎，全程不向任何服务器发送命令。大小为 key 在文件中的序列化字节数：不等于在线内存，但用于大 key 与前缀排查的排序完全可信。

### 性能诊断
**慢日志 ↔ Latency、实时 MONITOR、客户端、命令统计。**

Performance 面板把**慢日志**与 `LATENCY` 事件交叉关联（±5 秒徽章一键跳到 `LATENCY HISTORY` 折线图），并把过滤后的视图导出为 **CSV/JSON**；外加关键字过滤的实时 `MONITOR`、客户端管理（`CLIENT LIST/KILL`）、以及来自 `INFO commandstats` 的每命令 **次/秒** 表——带汇总行、闲置/自身连接噪声过滤与导出。

### 按值搜索
**找出*值里包含*某段文本的 key（带护栏的采样扫描）。**

Redis 无法索引值，故这种 `O(keyspace)` 搜索带护栏运行：必填 key 前缀、1 万键 / 10 秒上限（可取消）、跳过超 1 MiB 的值——覆盖 string 值、hash 字段、list/set/zset 成员，每条命中标注命中位置。空态引导与结果侧 key 过滤让流程更清晰；结果是明确的**采样**，绝不号称完整。

### 集群健康与管理
**带复制延迟的拓扑树、slot 分布图、逐节点负载，以及重分片向导。**

以树状查看 Cluster/Sentinel 拓扑（master、slot 范围、replica、源于 `INFO replication` 的逐副本延迟），并可操作：`CLUSTER FAILOVER` / `FORGET` / `MEET` / `REPLICATE` 与 `SENTINEL FAILOVER` / `RESET` / `REMOVE`，每个写操作都过确认对话框、PROD 升级。另有三个页签深入细节：**Slots** 展示各 master 的 slot 范围与迁移中的 slot，**Load** 采样各 master 的内存 / OPS / 客户端数，**重分片**向导在 master 间迁移 slot —— 选择目标节点（源节点可选，在 Load 卡片上一键指定）、预览方案，再执行带确认保护的 `CLUSTER RESHARD`。仅在多节点部署出现。

### 持久化与键事件
**RDB/AOF 状态 + 一键保存，外加实时键事件排查。**

持久化面板读取 `INFO persistence`（上次保存、AOF 膨胀、fork 失败），一键 `BGSAVE` / `BGREWRITEAOF`（PROD 升级），集群下逐节点显示状态行，并有 **Policy & 路径** 卡片展示 `CONFIG GET` 读到的 `save` 规则与 AOF 配置。Keyspace 通知把 keyspace/keyevent 频道解析成可过滤的 `(time, db, key, event, source)` 表格——"刚刚是哪个客户端删了 user:42？"——并提供 `notify-keyspace-events` 一键预设、暂停与导出。

### 原始 INFO 浏览器
**`INFO everything` 全部字段，单个可过滤表格。**

结构化面板覆盖常用字段；这个页面负责长尾——`errorstats`、`latencystats` 延迟分位、fork/COW 开销、`sync_full` 计数、uptime——不用再开终端。过滤同时匹配分段、字段与值；集群模式下列出所有主节点，分段列带节点地址，过滤某个字段即可跨节点对比。只读，旧版本服务器自动降级为 `INFO all` / 普通 `INFO`。

### CONFIG 编辑器
**带类型与内联参数文档的 `CONFIG GET/SET` 编辑器。**

运行时参数以类型化编辑器呈现而非裸字符串，常用参数带内联的本地化帮助文档——每个旋钮是干什么的，就写在你改它的地方。写入经 `CONFIG SET` 执行，走 PROD 升级的确认对话框。

---

## 🔑 Keys & 数据管理

### Key 组织
**带 TTL chip 的命名空间树、收藏、纯客户端标签与备注。**

键按 `:` 整理成嵌套树，带紧凑 TTL chip（绿色存活 / 红色将过期 / 灰色永久）。收藏常用键、回溯搜索历史，并打颜色**标签与备注**——存于本地 redb 文件，**完全不占 Redis 存储**，永不离开本机。

### Key 编辑与历史
**重命名、字段级 TTL、文件导入导出、批量粘贴、版本历史。**

原子**重命名**（`RENAMENX`，带覆盖保护）、字段级 **Hash TTL**（`HEXPIRE`/`HPERSIST`，Redis 7.4+）、**Value 文件导出 / 导入**（二进制安全、`KEEPTTL`）、TSV/CSV **批量粘贴**到 Hash/List/Set/ZSet，以及纯客户端的**最近 10 版本**写入历史，可 diff 可一键回滚。删除 key 时会先把 `DUMP` 载荷存入**本地回收站**（保留 24 小时，工具菜单可恢复、TTL 原样保留；设置中可关闭）——生产环境手滑删 key 不再是不可挽回的事故。超大的 String/JSON 值绝不盲目加载：编辑器先显示大小，点击**仍要加载**才真正拉取。

### 批量 Key 操作
**多选删除、批量 TTL、DUMP/RESTORE 导入导出、自动刷新。**

多选一次删除大量键；对整批选择或前缀设置 / 移除 TTL（集群安全、PROD 升级）；把任意选择导出为带 magic header + CRC32 的 framed 二进制文件并在他处 restore；并为高频变更实例自动刷新树视图。

### 跨服务器工具
**在两台服务器间复制 / 对比 key、或对比完整配置。**

**复制** key（连值带 TTL，`DUMP`/`RESTORE`）、**对比** string key 与对端同名 key（并排 diff）、或**对比**两台的 `CONFIG GET *`（斑马线表格只列差异）。专为排查"prod 和 staging 为何不一致"。

---

## 🔐 安全 & 隐私

### 隐私优先
**数据与凭据都留在本机，绝不外传。**

标签、备注、收藏与搜索历史存于**本地 redb 文件** —— 完全不占 Redis 存储，也不发往任何地方。连接密钥**加密存储**，以 JSON 分享连接时**默认剥离凭据**。可选的 AI 分析只发送 key 的**名称、大小与 TTL —— 绝不含 value**，且只发往**你自己配置**的 OpenAI 兼容接口。自定义脚本查看器经你本机的 Shell 本地运行。**无遥测、无账号、无云端。**

### 连接安全
**环境标签 + 对生产升级措辞的确认对话框。**

为每台服务器选择预设环境 —— **Dev / UAT / Prod** —— 以颜色 chip 显示在侧栏与状态栏；标题栏同时显示当前 **db**，高风险连接时附轻量 **Prod** 徽章。并可把任意连接锁为**只读**。破坏性操作（`FLUSHALL`、`CONFIG SET`、`SHUTDOWN`、`KEYS *`、批量 `DEL`、key/服务器删除、`XGROUP DESTROY`、cluster 操作…）执行前拦截，对 **Prod** 服务器使用更严肃的确认文案。

### ACL 用户管理（Redis 6+）
**覆盖完整 ACL 生命周期的 GUI。**

列出用户，查看 flags / 命令 / key 模式 / 频道规则，并通过快捷预设（Full / Read-only / Disabled）和可切换 chip（命令类别 + 通配符）编辑。

### 安全连接与分组
**TLS/SSL 与 SSH 隧道，配可命名、可分享的服务器分组。**

完整 **TLS/SSL**（自定义 CA、客户端证书）与 **SSH 隧道**（密码、私钥、Agent）。连接失败时，服务器表单的**诊断**按钮可运行分阶段诊断 —— DNS → TCP → SSH 认证 → SSH 隧道 → TLS → AUTH → PING —— 精确定位出错的层级并给出针对性修复提示，不再只抛一个晦涩的错误。把连接整理进可命名、可折叠的**分组**并重排。任意勾选连接导出为 JSON（默认剥离凭据）—— 或设置口令，生成紧凑、可跨机的**加密分享码**（`ZEDIS1.…`，Argon2id + AES-256-GCM），只有该口令能解开；导入对话框会识别分享码并提示输入口令。也可粘贴 `redis://` 连接串或 **Redis Insight** 的数据库导出一键迁入 —— 多个数据库一次到位。

---

## ⌨️ 效率

### 工作区标签页
**多个连接并排工作 —— Cmd/Ctrl+点击服务器即可在新标签页打开。**

每个标签页持有独立的连接与视图状态（键树展开、滚动、选中在切换后不丢失）。标签条仅在两个及以上标签页时出现（上限 8 个）：点击（或 **⌘1–8**）切换，中键或 × 关闭，拖拽排序，右键提供关闭 / 关闭其他 / 关闭右侧。后台标签页的心跳降为每 30 秒刷新一次，标签页列表会在下次启动时恢复，后台标签页在首次点击时才惰性建连。

### 多数据库键搜索
**⌘⇧F —— 一次在多个连接里找一个 key。**

浮层式搜索面板，范围三选一：所有已打开标签页的连接、某个服务器分组、或逐个勾选的服务器集合（支持全选；范围与每服务器扫描上限都会记住）。廉价的精确查找先行、即时呈现；完全无精确命中时自动执行带上限的 `SCAN`，有精确命中时扫描则收在按钮后面——快答案永远不被慢答案拖住。点击命中直接跳到对应服务器、db 和 key。

### 命令面板与快捷键
**⌘K 模糊导航 + ⌘/ 快捷键速查。**

**⌘K** 模糊搜索服务器、各面板与当前连接已加载的键（方向键移动、Enter 执行、Esc 关闭）；**⌘/** 打开只读的分组浮层，按平台符号列出所有快捷键。

### 集成 CLI 与 Workbench
**带补全的 redis-cli 终端 + 多行 Batch 模式。**

按版本过滤的命令补全与内联参数 / 说明提示。一键 **Batch** 模式把 REPL 换成多行编辑器——每行一条命令，用 `⌘`/`Ctrl`+`Enter` 运行（危险命令仍走确认对话框）。

### Lua 脚本库
**保存、复用、以 EVALSHA 运行 Lua 脚本，带命中率统计。**

本地保存的具名 Lua 脚本库（源码 + 预算 SHA1），带起步模板，一键 **EVALSHA 优先** 执行，预填 `KEYS` / `ARGS` 默认值便于一键重跑，并记录终身命中 / 未命中计数以发现总被刷出 Redis 缓存的脚本——外加缓存控制（**预热** = 仅 `SCRIPT LOAD` 不执行，及带确认保护的 `SCRIPT FLUSH`）与脚本库导入导出。（与 **Functions** 不同——后者管理服务端 `FUNCTION` library。）
