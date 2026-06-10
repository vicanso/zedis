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


---

## 🤔 为什么选择 Zedis？

厌倦了那些仅仅为了显示一个 JSON 字符串就吃掉几 GB 内存的 Electron Redis 客户端，或者在你不小心点击了一个包含 10 万个元素的键时直接卡死？我们也有同感。

**Zedis** 专为追求原生性能的开发者而生，从零开始打造。由 **GPUI**（[Zed Editor](https://zed.dev) 背后同款革命性渲染引擎）驱动，即便在浏览超大数据库时，Zedis 也能以极低的内存占用，带来流畅丝滑的 60+ FPS 原生体验。

## ✨ 核心特性

### 🚀 极速原生体验

<details><summary><b>GPU 渲染</b> —— 每个像素都在 GPU 上绘制，滚动零延迟、标签页秒切换。</summary>

即便浏览超大数据库，内存占用也极低。
</details>

<details><summary><b>虚拟列表</b> —— 从容浏览百万级键的实例，界面永不阻塞。</summary>

虚拟滚动结合 `SCAN` 迭代，无论 keyspace 多大都保持响应。
</details>

<details><summary><b>跨平台</b> —— macOS、Windows、Linux 均为原生体验，支持浅色 / 深色 / 跟随系统。</summary>

在三大桌面平台上都有真正的原生质感。
</details>

### 🧠 智能数据查看器

Zedis 自动检测（`ViewerMode::Auto`）并实时格式化你的数据。

<details><summary><b>自动解压缩</b> —— 透明解包 LZ4、SNAPPY、GZIP、ZSTD。</summary>

压缩值就地解包，你看到的是真实内容而非二进制 blob。
</details>

<details><summary><b>丰富内容解码</b> —— JSON/RedisJSON、Protobuf、MessagePack、Unix 时间戳、媒体 & Hex。</summary>

- **JSON & RedisJSON**：完整读写，内置美化与语法高亮。智能计算 RFC 7396 Merge Patch 差异，发送最小化的 `JSON.MERGE` 而非全量覆盖。**JSONPath 过滤同样适用于普通 string 类型 JSON**——用 `$.user.email` 或 `$.items[?(@.price > 100)]` 直接查询嵌套字段，无需 RedisJSON 模块。
- **Protobuf & MessagePack**：零配置二进制反序列化，输出可读的类 JSON 格式。
- **Unix 时间戳**：恰好 10 位（秒）或 13 位（毫秒）的 epoch 字符串自动识别，预览为本地时间 + UTC——原始值保持不变且可编辑。
- **媒体 & 十六进制**：原生预览图片（`PNG`、`JPG`、`WEBP`、`SVG`、`GIF`），二进制支持完全**可编辑**的 Hex 视图——粘贴 hex（自动忽略空白、逗号、`0x` 前缀）保存时解码回字节流。每行字节数随视口宽度自适应（16 / 24 / 32）。
</details>

<details><summary><b>自定义脚本查看器</b> —— 通过外部 Shell 命令对任意值做自定义解码。</summary>

配置含占位符（`{KEY}`、`{VALUE}`、`{HEX}`、`{RAW_FILE}`）的命令模板，Zedis 在 Unix/macOS 用 `sh -c`、Windows 用 `cmd /c` 执行，并将 stdout 作为格式化结果。适用于 base64、自定义二进制协议或 `$PATH` 中任意工具，按服务器配置精确 / 前缀 / 后缀 / 正则匹配键名。
</details>

<details><summary><b>Value 文件导出 / 导入</b> —— 把任意 string 值存成文件，或从文件加载回写。</summary>

在 key 栏的 `…` 菜单中，可把当前 string 值的原始字节保存为文件（按检测到的格式猜扩展名——`.png`、`.json`、`.gz`……），或选择文件用其字节覆盖当前值。二进制安全、保留 TTL（`SET … KEEPTTL`），并与普通保存一样记入本地写入历史。
</details>

<details><summary><b>Key 重命名</b> —— 在 key 栏重命名当前 key，带覆盖保护。</summary>

key 栏的 `…` 菜单打开重命名对话框（预填当前名）。底层用原子的 `RENAMENX`，绝不会静默覆盖另一个 key——若目标名已存在，会先弹"是否覆盖？"确认，确认后才执行 `RENAME`。编辑器停留在重命名后的 key、key 树自动刷新；值与 TTL 由服务端随之带走。
</details>

<details><summary><b>跨服务器复制 Key</b> —— 把某个 key（连值带 TTL）复制到另一台服务器或 db。</summary>

key 栏 `…` 菜单的"复制到…"选择目标服务器 + db（带覆盖开关），随后用源端 `DUMP` + 目标端 `RESTORE` 把该 key 送过去——值、编码、剩余 TTL 全部由服务端保留。把 key 推到测试环境很顺手。跨版本复制遵循 Redis `RESTORE` 的兼容规则。
</details>

<details><summary><b>Hash 字段级 TTL</b>（Redis 7.4+）—— 通过 HEXPIRE / HPERSIST 设置单字段过期。</summary>

为 Hash 中特定字段设置独立过期时间，无需为了让部分字段过期而重构数据模型。
</details>

<details><summary><b>Redis Streams</b> —— 浏览、实时跟踪、管理消费者组，全程不离开 GUI。</summary>

浏览流条目、**实时跟踪**新消息（`XREAD BLOCK`，环形缓冲，热流也不撑爆内存）、通过 `XINFO` 查看消费者组与待处理消息，并管理消费者组（`XGROUP CREATE` / `SETID` / `DESTROY`，销毁带确认保护）。
</details>

<details><summary><b>批量粘贴</b> —— 从 TSV / CSV 一次性向 Hash/List/Set/ZSet 添加大量条目。</summary>

粘贴 TSV 或 CSV（优先 Tab，回退逗号，逐单元格 trim），Zedis 按行走正常的 `HSET` / `RPUSH` / `SADD` / `ZADD` 路径写入。
</details>

<details><summary><b>Pub/Sub</b> —— 在 GUI 里订阅频道模式、实时收消息、发布消息。</summary>

内置订阅/发布界面，无需切换到 `redis-cli`。
</details>

<details><summary><b>本地写入历史</b> —— string 每次保存的最近 10 个版本，可 diff 可回滚。</summary>

按 key 在内存中保留，一键将旧版本载入编辑器。纯客户端（不占 Redis 存储），仅会话内有效，key 删除或切换服务器时自动清理。Restore 旁有**分裂 Diff 按钮**——主点击与上一版本并排 diff，下拉可挑任意旧版本。JSON 类型额外渲染 RFC 7396 merge patch，与 Save 实际发送的 `JSON.MERGE` 一致。
</details>

<details><summary><b>RediSearch 浏览器</b>（模块）—— 覆盖 FT.* 命令族的专用面板。</summary>

`FT._LIST` 列出索引、`FT.INFO` 查看 schema 与统计（含 indexing 进度与 `type mismatch` 失败计数，直接解释"有数据却 0 docs"）、原始 `FT.SEARCH` 配 `HIGHLIGHT` / `RETURN` / `LIMIT` chip，或切到 `FT.AGGREGATE` 跑单层 `GROUPBY` + `REDUCE`（`COUNT` / `SUM` / `AVG` / `QUANTILE` / `TOLIST` 等）。索引创建走结构化表单（HASH / JSON、prefix、每字段 SORTABLE / NOSTEM / NOINDEX），并支持 alter / drop。未加载模块时自动隐藏。
</details>

<details><summary><b>Functions 编辑器</b>（Redis 7+）—— 管理服务端 Lua library，带语法高亮。</summary>

通过 `FUNCTION LIST / LOAD / DELETE` 管理 library。卡片展示每个 library 的 engine、注册函数和 flags（`no-writes`、`allow-oom` 等）；点击展开只读 Lua 预览，带 tree-sitter 高亮。Edit 与"新建 library"共用一个 Lua 编辑器，带行号、缩进引导线、`REPLACE` 开关。Redis 6.x 及更早自动隐藏。
</details>

<details><summary><b>时间序列查看器</b>（RedisTimeSeries）—— TS.INFO 元数据 + 分桶的 TS.RANGE 图表。</summary>

选中 `TSDB-TYPE` key 打开专用图表——`TS.INFO` 展示采样总数 / 内存 / 保留期 / 分块数与 labels，`TS.RANGE`（服务端 `AVG` 聚合、按 ~240 点分桶，百万级采样也流畅）驱动 GPU 折线图，配 `15m / 1h / 6h / 24h / 7d / 全部` 切换。靠 key 是否存在自我门控。
</details>

<details><summary><b>概率型数据结构</b>（RedisBloom）—— Bloom / Cuckoo / Count-Min / Top-K / t-digest 查看器。</summary>

过去只能当二进制看的 key，现在打开专用只读查看器，展示各自的 `*.INFO` 统计（容量、大小、错误率……）。Top-K 还列出当前高频元素（`TOPK.LIST … WITHCOUNT`），t-digest 给出 min / max / p50 / p90 / p99（`TDIGEST.QUANTILE`）。按 key 的模块 TYPE 分发。
</details>

<details><summary><b>位图 / Bitfield</b> —— 在 GPU 网格上可视化字符串的每一位，支持 SETBIT / BITCOUNT / BITFIELD。</summary>

看起来像裸位图的 string——小（< 4 KB）、非文本、无法识别的二进制——会**自动**进入位图模式；更大的不透明二进制可从 key 栏的 `…` 菜单切入（文本 / JSON 及图片等可识别格式无此入口）。在无底图的 GPU 网格上绘制每一位——置位点亮，按 Redis 位序排列。悬浮读出位偏移，点击格子即用 `SETBIT` 翻转。统计行展示全量 `BITCOUNT` 与 `BITPOS`（首个 1 / 首个 0），下方薄输入框可运行原始 `BITFIELD` 子命令（如 `GET u8 0`）。网格为流畅做了上限，统计始终按整个 key 计算。
</details>

<details><summary><b>HyperLogLog</b> —— 为 HLL string key 提供 PFCOUNT 基数卡片。</summary>

HyperLogLog 以普通 `string` 存储，过去只能当二进制乱码看。Zedis 现在识别 `HYLL` 头部魔数，打开专用只读卡片，展示估算基数（`PFCOUNT`）、内部编码（稠密 / 稀疏）与表示大小（`STRLEN`），并带一个 `PFADD` 输入框，可加入新元素、实时观察基数变化。
</details>

<details><summary><b>向量集 + KNN</b>（Redis 8）—— 元数据 + 交互式最近邻检索。</summary>

原生 `vectorset` key 打开查看器——`VINFO` / `VCARD` / `VDIM` 元数据、`VRANDMEMBER` 样本，以及**交互式 KNN 检索**：输入（或点击）元素运行 `VSIM … WITHSCORES` 展示最近邻；点击某邻居以它为起点重新检索，逐跳遍历 HNSW 图。只读。
</details>

<details><summary><b>地理地图</b> —— 把地理类有序集合画在无底图的雷达画布上。</summary>

把任意 sorted set 切到 **地图** 模式，即可在暗色 GPU 渲染的 Web Mercator 画布上绘制成员——无地图瓦片、无网络请求。`GEOPOS` 解码 geohash、自适应包围盒取景，滚轮缩放 / 拖拽平移 / 悬浮（带实时坐标读数与联动高亮的侧边列表）让它成为快速排查 GEO 的"雷达"。输入中心经纬度 + 半径即可运行 `GEOSEARCH`，命中点高亮、其余淡出，并在画布上画出半径圆。为流畅做了上限，无效 / 非地理成员单独列出。
</details>

### 📊 实时可观测性

内置 GPU 加速仪表盘，彻底改变你监控 Redis 实例的方式。

<details><summary><b>实时指标</b> —— CPU、内存、网络 I/O 的实时图表。</summary>

精美渲染的 GPU 加速时序图表。
</details>

<details><summary><b>内存分析器 + AI 建议</b> —— 排查 BigKey、查看 TTL 分布、获取 AI 优化建议。</summary>

Top-N 表按 **大小 / 最热 / 最冷** 排序——根据 `maxmemory-policy` 自动选用 `OBJECT FREQ` 或 `OBJECT IDLETIME`。同一次 SCAN 还展示 **TTL 分布直方图**（`<1m / <1h / <1d / <7d / ≥7d / 无 TTL`）——一眼识别"凌晨 3 点集中淘汰"、查看 `PERSIST`（内存泄漏红旗）占比，并在 `ratio<1` 采样时读到估算的全量键数。点击 **AI 分析** 将报告转成 Markdown 提交到任意 **OpenAI 兼容** 接口（设置中填 Base URL + API Key，密钥加密存储），优化建议内联渲染——只发送 key 的*名称*、大小与 TTL，绝不发送 value。兼容 OpenAI、Claude（经 Anthropic 的 OpenAI 兼容端点 `https://api.anthropic.com/v1/`）等服务；建议用当前界面语言返回。
</details>

<details><summary><b>集群健康度</b> —— 以树状查看 Cluster / Sentinel 拓扑与复制延迟。</summary>

悬浮节点指示器即可查看各 master 及其 slot 范围、replica 按 master 分组，并附每个 replica 的复制延迟（字节 + 秒 + 连接状态），数据源于 `INFO replication`。
</details>

<details><summary><b>深度诊断</b> —— 慢日志 ↔ Latency 交叉关联、实时 MONITOR、客户端管理。</summary>

追踪慢日志、关键字过滤实时监控 `MONITOR`、管理活跃客户端（`CLIENT LIST/KILL`）。Performance 面板把慢日志与 `LATENCY` 事件交叉关联：每条慢命令带徽章标出 ±5 秒内最近的 fork/AOF/expire 事件，一键跳到该事件的 `LATENCY HISTORY` 折线图；反向徽章把慢日志收窄到该时间段。`latency-monitor-threshold` 关闭时面板内一键开启（默认 100 ms，PROD 走确认对话框）。
</details>

<details><summary><b>持久化管理</b> —— RDB/AOF 状态 + 一键 BGSAVE / BGREWRITEAOF。</summary>

独立面板持续读取 `INFO persistence`——上次 RDB 时间、距上次保存的写入数、AOF 体积与重写基线的膨胀比，以及按 fork 失败的告警 banner。`BGSAVE` / `BGREWRITEAOF` 一键触发（集群模式 fan-out 到所有 master），走确认对话框（PROD 升级措辞），fork 进行中或只读时按钮自动 disable。
</details>

<details><summary><b>Keyspace 通知订阅</b> —— 从 keyspace / keyevent 频道做实时键事件排查。</summary>

"刚刚是哪个客户端删了 user:42？"不用切 redis-cli 就有答案。channel 解析成 `(time, db, key, event, source)` 表格，事件动词按严重度上色，ring buffer 保留最近 1000 条；过滤器走纯客户端路径（事件类型多选 chip + key 子串）。`notify-keyspace-events` 为空时 banner 提供一键 "Enable (AKE)"——PROD 先走确认对话框。
</details>

### 🛡️ 企业级安全与效率

<details><summary><b>命令面板</b>（⌘K）—— 键盘优先的模糊搜索，覆盖服务器与各面板。</summary>

无需鼠标即可切换连接或跳转到任意面板（Metrics、性能、内存、Config、ACL、RediSearch、Functions、Lua 脚本、设置……）。方向键移动、Enter 执行、Esc 关闭。
</details>

<details><summary><b>服务器分组与排序</b> —— 可折叠的命名分组、组内重排、导出为 JSON 分享。</summary>

把连接整理进命名分组，组内卡片可重排，单个连接可导出为 JSON（默认剥离凭据，可选包含密钥用于个人备份）。折叠状态跨会话保留。
</details>

<details><summary><b>只读模式</b> —— 锁定连接，防止误操作写入。</summary>

避免在生产环境中误写数据。
</details>

<details><summary><b>ACL 用户管理</b>（Redis 6+）—— 覆盖完整 ACL 生命周期的 GUI。</summary>

列出用户、查看 flags / 命令 / key 模式 / 频道规则，并通过快捷预设工具栏（Full access / Read-only / Disabled）和可切换 chip（`+@read`、`-@dangerous` 等命令类别 + key/频道通配符）编辑。
</details>

<details><summary><b>连接安全</b> —— 环境标签 + 对生产升级措辞的确认对话框。</summary>

每个服务器可配置 tag（PROD / DEV / STAGING）与颜色，同步显示在侧栏与状态栏。危险命令（`FLUSHALL`、`FLUSHDB`、`CONFIG SET`、`SHUTDOWN`、`DEBUG`、`SCRIPT FLUSH`、`KEYS *`、批量 `DEL`…）执行前被拦截，对 PROD 标签服务器使用更严肃的确认文案。
</details>

<details><summary><b>数据导入 / 导出</b> —— 基于 DUMP/RESTORE，所有 key 类型二进制安全。</summary>

对任意 key 选择（多选 / 单 key / 整个文件夹前缀）导出为带 magic header + CRC32 的 framed 二进制文件，可在另一实例上 restore。
</details>

<details><summary><b>高级隧道</b> —— TLS/SSL（自定义 CA、客户端证书）与 SSH 隧道。</summary>

完整支持 TLS/SSL 和 SSH 隧道（密码、私钥、SSH Agent）。
</details>

<details><summary><b>集成 CLI 与 Workbench</b> —— 带补全的 redis-cli 终端 + 多行 Batch 模式。</summary>

按版本过滤的命令补全和内联参数/说明提示。一键切换 **Batch** 模式把单行 REPL 换成多行编辑器——每行一条命令，用 `⌘`/`Ctrl`+`Enter` 一次运行整段脚本（危险命令仍走确认对话框）。
</details>

<details><summary><b>命名空间树视图</b> —— 按 <code>:</code> 整理成嵌套树，每个 key 带 TTL chip。</summary>

右键文件夹可刷新内容或一键删除该前缀下所有键。每个 leaf key 旁有紧凑 TTL chip——绿色存活、红色 2 分钟内过期、灰色永久。
</details>

<details><summary><b>多选批量删除</b> —— 一次标记并删除多个键。</summary>

切换多选模式即可删除大量键，无需编写任何命令。
</details>

<details><summary><b>键收藏与搜索历史</b> —— 收藏常用键、回溯近期搜索。</summary>

收藏常用键以便快速访问，并从持久化历史面板回溯近期搜索。
</details>

<details><summary><b>键标签与私人备注</b> <i>(纯客户端)</i> —— 给键打颜色标签和备注，本地存储。</summary>

给任意键打颜色标签（红/橙/黄/绿/蓝/紫）和自由文本备注——全部存在本地 redb 文件，**完全不占用 Redis 存储**，永不离开本机。打了标签的行在 key tree 左侧有 4px 色条，悬停显示备注；编辑入口为右键 → "编辑标签与备注…"。每种颜色可作为筛选条件从 tree 的 ⋯ 菜单一键过滤；筛选**直接以本地 metadata 为来源**而非 SCAN 在线进度，所以每个 tagged key 立即可见。Save 后只刷新受影响的那一行。磁盘存储版本化，便于将来平滑迁移。
</details>

<details><summary><b>自动刷新</b> —— 为键目录配置定时刷新，适配高频变更实例。</summary>

配置自动刷新间隔，让树视图与实时实例保持同步。
</details>

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

欢迎提交 issue 或 PR 参与进来。提交 PR 即表示你同意我们的[贡献者许可协议（CLA）](./CLA.md)。

## 📄 许可证

Zedis 是根据 [Apache License 2.0](./LICENSE) 授权的开源软件。
