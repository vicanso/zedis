中文 | [English](./README.md)

<h1 align="center">Zedis</h1>

<p align="center">
  <strong>一个使用 Rust 🦀 和 GPUI ⚡️ 构建的高性能、GPU 加速的 Redis 客户端</strong>
</p>

<p align="center">
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-Apache--2.0-blue.svg" alt="License"></a>
  <a href="https://x.com/tree0507"><img src="https://img.shields.io/twitter/follow/tree0507?style=social" alt="Twitter Follow"></a>
  <img src="https://img.shields.io/github/downloads/vicanso/zedis/total" alt="Downloads">
  <a href="https://www.blazingly.fast"><img src="https://www.blazingly.fast/api/badge.svg?repo=vicanso%2Fzedis" alt="blazingly fast"></a>
</p>

<p align="center">
  <video src="https://github.com/user-attachments/assets/217cc0a7-cc7e-40d0-ac7e-1ec61c36a02b" autoplay loop muted playsinline width="100%"></video>
</p>


# Zedis

一个使用 **Rust** 🦀 和 **GPUI** ⚡️ 构建的高性能、GPU 加速的 Redis 客户端

[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)
![GitHub Downloads (all assets, all releases)](https://img.shields.io/github/downloads/vicanso/zedis/total)
[![blazingly fast](https://www.blazingly.fast/api/badge.svg?repo=vicanso%2Fzedis)](https://www.blazingly.fast)

<video src="https://github.com/user-attachments/assets/217cc0a7-cc7e-40d0-ac7e-1ec61c36a02b" autoplay loop muted playsinline width="100%"></video>

---

## 🤔 为什么选择 Zedis？

厌倦了那些仅仅为了显示一个 JSON 字符串就吃掉几 GB 内存的 Electron Redis 客户端，或者在你不小心点击了一个包含 10 万个元素的键时直接卡死？我们也有同感。

**Zedis** 专为追求原生性能的开发者而生，从零开始打造。由 **GPUI**（[Zed Editor](https://zed.dev) 背后同款革命性渲染引擎）驱动，即便在浏览超大数据库时，Zedis 也能以极低的内存占用，带来流畅丝滑的 60+ FPS 原生体验。

## ✨ 核心特性

### 🚀 极速原生体验
- **GPU 渲染**：每一个像素都在 GPU 上绘制，滚动零延迟，标签页秒切换。
- **虚拟列表**：从容浏览百万级键数量的实例。虚拟滚动结合 `SCAN` 迭代，确保界面永不阻塞。
- **跨平台**：在 **macOS**、**Windows** 和 **Linux** 上均有真正的原生体验，完整支持浅色、深色和跟随系统主题。

### 🧠 智能数据查看器
告别手动解码。Zedis 自动检测（`ViewerMode::Auto`）并实时格式化你的数据：
- **自动解压缩**：透明解包 `LZ4`、`SNAPPY`、`GZIP` 和 `ZSTD` 压缩数据。
- **丰富内容解码**：
  - **JSON & RedisJSON**：完整读写支持，内置美化输出与语法高亮。智能计算 RFC 7396 Merge Patch 差异，发送最小化的 `JSON.MERGE` 命令，而非全量覆盖写入。**JSONPath 过滤同样适用于普通 string 类型的 JSON**——用 `$.user.email` 或 `$.items[?(@.price > 100)]` 直接查询嵌套字段，无需 RedisJSON 模块。
  - **Protobuf & MessagePack**：零配置二进制反序列化，输出为可读的类 JSON 格式。
  - **媒体 & 十六进制**：原生预览图片（`PNG`、`JPG`、`WEBP`、`SVG`、`GIF`），二进制数据支持完全**可编辑**的 Hex 视图——粘贴 hex 文本（自动忽略空白、逗号、`0x` 前缀）保存时自动解码回字节流。每行字节数根据视口宽度自适应（16 / 24 / 32）。
- **自定义脚本查看器**：通过外部 Shell 命令对任意 Redis 值进行自定义解码。配置包含占位符（`{KEY}`、`{VALUE}`、`{HEX}`、`{RAW_FILE}`）的命令模板，Zedis 将在 Unix/macOS 上通过 `sh -c`、在 Windows 上通过 `cmd /c` 执行该命令，并将标准输出作为格式化结果显示。适用于 base64、自定义二进制协议或 `$PATH` 中的任意工具，支持按服务器配置精确匹配、前缀、后缀或正则表达式的键名匹配规则。
- **Hash 字段级 TTL**（Redis 7.4+）：通过 `HEXPIRE` / `HPERSIST` 为 Hash 中的特定字段设置独立过期时间，无需为了让部分字段过期而重新设计数据模型。
- **Redis Streams**：完整 Stream 支持——浏览流条目、**实时跟踪**新消息（`XREAD BLOCK`，环形缓冲，热流也不会撑爆内存）、通过 `XINFO` 查看消费者组与待处理消息（Pending Entries），并管理消费者组（`XGROUP CREATE` / `SETID` / `DESTROY`，销毁带确认保护）——全程无需离开 GUI。
- **批量粘贴**：一次性向 Hash / List / Set / ZSet 添加大量条目——粘贴 TSV 或 CSV（优先 Tab，回退逗号，逐单元格 trim），Zedis 会按行走正常的 `HSET` / `RPUSH` / `SADD` / `ZADD` 路径写入。
- **Pub/Sub**：内置订阅/发布界面，支持订阅频道模式、实时接收消息，并可直接在 GUI 中发布消息，无需切换到 `redis-cli`。
- **本地写入历史**：string 类型每次保存时自动在内存中保留最近 10 个历史版本（按 key 隔离），一键即可将旧版本载入编辑器，预览或回滚后再保存。纯客户端记录（不占用 Redis 存储），仅会话内有效，key 被删除或切换服务器时自动清理。Restore 旁还有一个**分裂 Diff 按钮**——主点击直接与上一版本并排 diff、下拉可挑选任意旧版本。JSON 类型在差异面板下方额外渲染 RFC 7396 merge patch 文档,与 Save 流程实际发送的 `JSON.MERGE` 内容一模一样。
- **RediSearch 浏览器**（模块）：专用面板覆盖 `FT.*` 命令族——`FT._LIST` 列出索引、`FT.INFO` 查看 schema 与统计（含 indexing 进度与 `type mismatch` 失败计数，直接解释"明明有数据却 0 docs"的原因）、原始 `FT.SEARCH` 配合 `HIGHLIGHT` / `RETURN` / `LIMIT` chip，或切换到 `FT.AGGREGATE` 跑单层 `GROUPBY` + `REDUCE`（`COUNT` / `SUM` / `AVG` / `QUANTILE` / `TOLIST` 等）。索引创建走结构化表单（HASH / JSON、prefix、每字段 SORTABLE / NOSTEM / NOINDEX 切换），同时支持 alter / drop，全部不需离开 app。未加载 RediSearch 模块的服务器自动隐藏入口。
- **Functions 编辑器**（Redis 7+）：通过 `FUNCTION LIST / LOAD / DELETE` 管理服务端 Lua library。卡片一眼看到每个 library 的 engine、注册的函数和 flags（`no-writes`、`allow-oom` 等）；点击展开内联只读 Lua 预览，带完整 tree-sitter 语法高亮。Edit 与"新建 library"共用同一个 Lua 编辑器，带行号、缩进引导线、`REPLACE` 开关，确保迭代安全。Redis 6.x 及更早版本自动隐藏入口。
- **时间序列查看器**（RedisTimeSeries 模块）：选中 `TSDB-TYPE` 类型的 key 会打开专用图表——`TS.INFO` 展示采样总数 / 内存 / 保留期 / 分块数与 labels，`TS.RANGE`（服务端 `AVG` 聚合、按 ~240 个点分桶，百万级采样也能流畅响应）驱动 GPU 渲染折线图，配 `15m / 1h / 6h / 24h / 7d / 全部` 范围切换。无需单独的模块门控——key 只有在加载 `timeseries` 模块时才会被识别为该类型，查看器靠 key 是否存在自我门控。
- **概率型数据结构**（RedisBloom 模块）：Bloom 过滤器、Cuckoo 过滤器、Count-Min Sketch、Top-K、t-digest——过去只能当二进制看——现在会打开专用的只读查看器。每种都展示自己的 `*.INFO` 统计（容量、大小、错误率……）；Top-K 还会列出当前的高频元素（`TOPK.LIST … WITHCOUNT`），t-digest 额外给出 min / max / p50 / p90 / p99（`TDIGEST.QUANTILE`）。按 key 的模块 TYPE 分发，只在加载了 RedisBloom 时出现。
- **向量集 + KNN**（Redis 8）：原生 `vectorset` key（过去无法查看）会打开专用查看器——`VINFO` / `VCARD` / `VDIM` 元数据、`VRANDMEMBER` 元素样本，以及**交互式 KNN 检索**：输入（或点击）一个元素即可运行 `VSIM … WITHSCORES`，按相似度得分展示其最近邻；点击某个邻居会以它为起点重新检索，从而逐跳遍历 HNSW 图。只读。

### 📊 实时可观测性
内置 GPU 加速仪表盘，彻底改变你监控 Redis 实例的方式。
- **实时指标**：精美渲染的 CPU、内存和网络 I/O 实时图表。
- **内存分析器**：可视化排查 **BigKey**，优化存储效率，预防 OOM。Top-N 表支持按 **大小 / 最热 / 最冷** 排序——根据服务端 `maxmemory-policy` 自动选用 `OBJECT FREQ`（LFU）或 `OBJECT IDLETIME`（LRU），一键定位真正值得缓存（或淘汰）的 key。同一次 SCAN 还会在同一视图中（无需切换 Tab）同步展示 **TTL 分布直方图**（`<1m / <1h / <1d / <7d / ≥7d / 无 TTL`）——一眼识别"凌晨 3 点集中淘汰"陷阱、查看 `PERSIST`（内存泄漏红旗）占比，并在 ratio<1 采样时直接读到估算的全量键数。
- **集群健康度**：状态栏节点指示器悬浮即可查看树状拓扑——各 master 及其 slot 范围、replica 按 master 分组展示，并附带每个 replica 的复制延迟（字节 + 秒 + 连接状态），数据源于 `INFO replication`。Cluster 与 Sentinel 部署均支持。
- **深度诊断**：追踪慢日志，通过关键字过滤实时监控 `MONITOR` 流，并通过直观的 GUI 管理活跃客户端（`CLIENT LIST/KILL`）。Performance 面板把慢日志与 `LATENCY` 事件交叉关联：每条慢命令上自带徽章，标出 ±5 秒内最近的 fork/AOF/expire 事件，一键跳到该事件的 GPU 折线图（`LATENCY HISTORY`）；反向徽章则在每条 Latency 事件上显示窗口内的慢日志条数，点击把慢日志收窄到该时间段。`latency-monitor-threshold` 关闭时面板内一键开启（默认 100 ms，PROD 标签走标准确认对话框）。
- **持久化管理**：独立面板持续读取 `INFO persistence`——上次 RDB 快照时间、距上次保存的写入数、AOF 当前体积与重写基线的膨胀比，以及按 fork 失败维度的告警 banner。`BGSAVE` / `BGREWRITEAOF` 一键触发（集群模式下自动 fan-out 到所有 master），走标准确认对话框（PROD 标签自动升级措辞），fork 进行中或连接处于只读时按钮自动 disable 并显示已运行时长。
- **Keyspace 通知订阅**：一键挂载 `__keyspace@*__:*` 与 `__keyevent@*__:*` 做实时键事件排查——"刚刚是哪个客户端删了 user:42？"不用切到 redis-cli 就有答案。channel 名解析成 `(time, db, key, event, source)` 五列表格，事件动词按严重度上色，ring buffer 保留最近 1000 条；过滤器走纯客户端路径（事件类型多选 chip + key 子串过滤），切换不会重连订阅。`notify-keyspace-events` 为空时面板顶部出现黄色 banner，内嵌 "Enable (AKE)" 按钮一键开启——PROD 标签的服务器会先走标准确认对话框再下发 CONFIG SET。

### 🛡️ 企业级安全与效率
- **命令面板**（`⌘K`）：键盘优先的模糊搜索，覆盖服务器与导航命令——无需鼠标即可切换连接或跳转到任意面板（Metrics、性能、内存、Config、ACL、RediSearch、Functions、Lua 脚本、设置……）。方向键移动、Enter 执行、Esc 关闭。
- **服务器分组与排序**：把连接整理进可折叠的命名分组，组内卡片可重排，单个连接可导出为 JSON 分享（默认剥离凭据，可选包含密钥用于个人备份）。折叠状态跨会话保留。
- **只读模式**：锁定连接，防止在生产环境中误操作写入数据。
- **ACL 用户管理**（Redis 6+）：完整覆盖 `ACL` 生命周期——列出用户、查看 flags / 命令 / key 模式 / 频道规则，并通过快捷预设工具栏（Full access / Read-only / Disabled）以及可切换的 chip（`+@read`、`-@dangerous` 等命令类别 + key/频道通配符）进行编辑。
- **连接安全**：每个服务器可配置 tag（PROD / DEV / STAGING）与颜色，标识同步显示在侧栏与状态栏。危险命令（`FLUSHALL`、`FLUSHDB`、`CONFIG SET`、`SHUTDOWN`、`DEBUG`、`SCRIPT FLUSH`、`KEYS *`、批量 `DEL`…）在执行前会被拦截，对 PROD 标签的服务器使用更严肃的确认文案。
- **数据导入 / 导出**：对任意 key 选择（多选 / 单 key / 整个文件夹前缀）导出为带 magic header + CRC32 的 framed 二进制文件，可在另一实例上 restore——基于 `DUMP` / `RESTORE`，所有 key 类型二进制安全。
- **高级隧道**：完整支持 TLS/SSL（自定义 CA、客户端证书）和 SSH 隧道（密码、私钥、SSH Agent）。
- **集成 CLI**：内置 `redis-cli` 终端，无需离开应用即可使用命令行。
- **命名空间树视图**：自动将以冒号（`:`）分隔的键整理为嵌套目录树，右键菜单支持刷新指定文件夹或一键删除该前缀下的所有键。每个 leaf key 旁附带紧凑的 TTL chip——绿色表示存活 TTL，红色表示 2 分钟内即将过期，灰色表示永久 key。
- **多选批量删除**：切换多选模式，一次性标记并删除多个键，无需编写任何命令。
- **键收藏与搜索历史**：收藏常用键以便随时快速访问，并可从持久化历史面板中回溯近期搜索记录。
- **键标签与私人备注**（纯客户端）：给任意键打颜色标签（红/橙/黄/绿/蓝/紫）和自由文本备注——所有数据都存在本地 redb 文件里，**完全不占用 Redis 存储**，永远不会离开本机。打了标签的行在 key tree 左侧出现 4px 色条，鼠标悬停可显示备注 tooltip；编辑入口为右键 → "编辑标签与备注…"。每种颜色都可作为筛选条件从 tree 的 ⋯ 菜单一键过滤（只在已经标过键时出现）；筛选时**直接以本地 metadata 为 keys 列表来源**，而非依赖 SCAN 在线进度——所以每个 tagged key 立即可见，无需等待 SCAN 扫到对应 cursor 窗口，也不会因 scan_count 预算耗尽而漏掉本应出现的 key。Save 后只刷新被影响的那一行（非全树重建），除非过滤激活且新颜色改变了该行的可见性。磁盘存储采用 `v: 1` 版本化 envelope，将来扩展多标签 / 按标签备注时可平滑迁移。
- **自动刷新**：为键目录配置自动刷新间隔，实时同步高频变更的 Redis 实例数据。

---

## 📦 安装

准备好感受极速体验了吗？通过你喜欢的包管理器安装 Zedis：

### macOS
推荐通过 Homebrew 安装：

```bash
brew install --cask zedis
```

### Windows

```bash
scoop bucket add extras
scoop install zedis
```

### Linux (Arch)

```bash
yay -S zedis-bin
```

### Cargo（跨平台源码编译）

```bash
cargo install --locked zedis-gui
```

---

## 🤝 参与贡献

我们希望将 Zedis 打造成终极 Redis 客户端，非常欢迎你的参与！无论是新增功能、翻译界面还是修复 Bug，一切贡献都受到欢迎。

请阅读我们的[贡献指南](https://www.google.com/search?q=CONTRIBUTING.md)开始参与。提交 PR 即表示你同意我们的[贡献者许可协议（CLA）](https://www.google.com/search?q=CLA.md)。

## 📄 许可证

Zedis 是根据 [Apache License 2.0](./LICENSE) 授权的开源软件。
