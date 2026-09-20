中文 | [English](./README.md)

<h1 align="center">Zedis</h1>

<p align="center">
  <strong>能打开你那个百万级 key 的库而不转圈的 Redis 客户端 —— 原生、GPU 加速,由 Rust 🦀 和 GPUI ⚡️ 驱动</strong>
</p>

<p align="center">
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-Apache--2.0-blue.svg" alt="License"></a>
  <a href="https://x.com/tree_xie"><img src="https://img.shields.io/twitter/follow/tree_xie?style=social" alt="Twitter Follow"></a>
  <img src="https://img.shields.io/github/downloads/vicanso/zedis/total" alt="Downloads">
  <a href="https://www.blazingly.fast"><img src="https://www.blazingly.fast/api/badge.svg?repo=vicanso%2Fzedis" alt="blazingly fast"></a>
</p>

<p align="center">
  <video src="https://github.com/user-attachments/assets/d7007ecf-bbfd-4e68-bbaf-437091f711e7" autoplay loop muted playsinline width="100%"></video>
</p>

---

## 🤔 为什么选择 Zedis？

厌倦了那些仅仅为了显示一个 JSON 字符串就吃掉几 GB 内存、点开一个 10 万元素的键就直接卡死、集群模式操作处处别扭、把压缩和二进制 value 显示成一堆乱码的 Electron Redis 客户端？我们也有同感。

**Zedis** 专为追求原生性能的开发者而生，从零开始打造。由 **GPUI**（[Zed Editor](https://zed.dev) 背后同款渲染引擎）驱动，即便在浏览超大数据库时，Zedis 也能以极低的内存占用，带来流畅丝滑的 60+ FPS 原生体验。

## ✨ 亮点

- 🦀 **原生，而非 Electron** —— 每个像素都在 GPU 上绘制、虚拟滚动 `SCAN`，百万级键也保持 60+ FPS、极低内存。
- 🧠 **看得懂你的数据** —— 自动解压并解码 JSON/JSONPath、Protobuf、MessagePack、Java / PHP / pickle 序列化、BSON、JWT、Base64、URL 编码、时间戳、图片与 Hex，并为每种 Redis 类型和模块提供专用查看器。
- 📊 **实时可观测** —— 实时指标、内存分析器（离线 + AI 建议、服务端 key 大小直方图）、热点 Key 跟踪（`HOTKEYS`）、集群每 slot 统计、慢日志 ↔ Latency、`MONITOR`、按值搜索。
- 🔐 **隐私优先且安全** —— 元数据只存本地文件、密钥用每机唯一密钥加密存储、破坏性操作对生产环境升级确认措辞。
- 🌐 **连接一切** —— TLS/SSL、SSH 隧道（含带口令的加密密钥）、Cluster/Sentinel、从 Redis Insight / ARDM / Tiny RDM 导入，以及 8 种界面语言。
- ⌨️ **为重度用户而生** —— ⌘K 命令面板、带补全的 redis-cli、表格 / JSON 回复视图 + AI 命令助手、Batch 模式、跨服务器复制/对比。
- 🕸️ **浏览器里也能用** —— 同一套代码编译成 WebAssembly，一个约 26 MB 的 Docker 镜像即可自托管（见 [Web 版](#-web-版自托管)）。

> ### 🔄 已经在用 Redis Insight?
> **粘贴它导出的数据库配置,所有连接一次迁入** —— 不用一个个重填地址、端口和密码。花大约一分钟,就能拿你真实的连接试试 Zedis,快不快自己判断。

## 📸 截图

<!--
  方案 A —— 图片托管在 GitHub:把每张截图拖进任意 issue/PR 评论(或 release),
  得到 https://github.com/user-attachments/assets/... 链接,然后替换下面的
  REPLACE-* 占位符。点击缩略图打开原图;width="260" 让三列网格约占一屏。
  建议截图(按重要性):
    1. key-browser     —— 命名空间树 + JSON / 语法高亮的值编辑器
    2. memory-analyzer —— Top-N 表 + TTL 直方图 + 体检建议
    3. live-metrics    —— 实时 GPU 图表(CPU / 内存 / 网络)
    4. geo-map         —— sorted set 画在雷达上
    5. vector-set      —— 向量集 + KNN(VSIM)结果
    6. command-palette —— ⌘K 命令面板
-->

<table>
  <tr>
    <td><a href="https://github.com/user-attachments/assets/c06e4d80-7607-4d6c-807e-2a62a2ee556f"><img src="https://github.com/user-attachments/assets/c06e4d80-7607-4d6c-807e-2a62a2ee556f" width="260" alt="键浏览与数据查看"></a></td>
    <td><a href="https://github.com/user-attachments/assets/88091f50-ec77-41d5-acda-047a835079f8"><img src="https://github.com/user-attachments/assets/88091f50-ec77-41d5-acda-047a835079f8" width="260" alt="内存分析器"></a></td>
    <td><a href="https://github.com/user-attachments/assets/d5801a8c-da94-461b-83b6-6c9b70e2007d"><img src="https://github.com/user-attachments/assets/d5801a8c-da94-461b-83b6-6c9b70e2007d" width="260" alt="实时指标"></a></td>
  </tr>
  <tr>
    <td align="center"><sub>键浏览与数据查看</sub></td>
    <td align="center"><sub>内存分析器</sub></td>
    <td align="center"><sub>实时指标</sub></td>
  </tr>
  <tr>
    <td><a href="https://github.com/user-attachments/assets/2525cec9-5dd6-4049-9ea9-60fcb4cc249f"><img src="https://github.com/user-attachments/assets/2525cec9-5dd6-4049-9ea9-60fcb4cc249f" width="260" alt="地理地图"></a></td>
    <td><a href="https://github.com/user-attachments/assets/b4733051-2965-40ff-9d49-6bb909551513"><img src="https://github.com/user-attachments/assets/b4733051-2965-40ff-9d49-6bb909551513" width="260" alt="向量集 + KNN"></a></td>
    <td><a href="https://github.com/user-attachments/assets/4335c12b-cbca-467e-abd9-7b50ffd568c5"><img src="https://github.com/user-attachments/assets/4335c12b-cbca-467e-abd9-7b50ffd568c5" width="260" alt="命令面板"></a></td>
  </tr>
  <tr>
    <td align="center"><sub>地理地图</sub></td>
    <td align="center"><sub>向量集 + KNN</sub></td>
    <td align="center"><sub>命令面板(⌘K)</sub></td>
  </tr>
</table>

## 🧩 功能一览

| 领域 | 包含内容 |
| --- | --- |
| 🚀 **原生 & 快** | GPU 渲染 · 虚拟滚动 `SCAN`，百万键 60+ FPS · macOS / Windows / Linux · 浅色 / 深色 / 跟随系统 + 6 套内置主题 · 界面与等宽字体可自选 |
| 🧠 **智能数据查看器** | 自动解压(LZ4 / Snappy / GZIP / ZSTD)· JSON & RedisJSON + JSONPath · Protobuf · MessagePack · Java / PHP / pickle 序列化 · BSON · JWT · Base64 · URL 编码 · 时间戳 · 图片 · Hex · 自定义脚本——Hash / List / Set / ZSet 的元素同样适用，编辑的仍是存储的字节 |
| 🗂️ **类型 & 模块查看器** | 位图（`BITOP`）· HyperLogLog（`PFMERGE`）· 向量集(KNN)· 地理地图（`GEOADD` / `GEODIST`，半径 + 矩形搜索）· Bloom / Cuckoo / Count-Min / Top-K · 时间序列（`TS.ADD` / `TS.ALTER` / 聚合规则，以及基于 `TS.MRANGE` 的多序列浏览器）· Streams(实时跟踪、`XSETID`、消费者管理)· Pub/Sub(含分片)· RediSearch（索引大小、`FT.TAGVALS` 取值、`FT.SPELLCHECK` 拼写建议）· Functions |
| 📊 **可观测性** | 实时指标 + 7 天历史（可导出 CSV）· `MEMORY DOCTOR` / `MEMORY STATS` 与 `LATENCY DOCTOR` 报告 · 内存分析（在线扫描或离线 RDB 文件）· 类型/编码占比 · 逐级下钻前缀 + AI 建议 · 慢日志 ↔ Latency（含 Valkey `COMMANDLOG` 大请求 / 大回复）· `MONITOR` · 按值搜索 · 集群健康、重分片 / 槽位修复 / 再平衡 · 主从复制（`REPLICAOF` / `FAILOVER`）· 持久化 & 键事件 · 带类型的 CONFIG 编辑器（含 `CONFIG REWRITE`） · 原始 INFO 浏览器 |
| 🔑 **Keys & 数据** | 带 TTL chip 的命名空间树 · 分页加载的 Hash / List / Set / ZSet 编辑器（`HSCAN`/`SSCAN`/`ZSCAN`）· 多选批量删除 · 类型原生操作（`LTRIM` / `LPOP` / `ZINCRBY` / `ZPOPMIN` / `HINCRBY` / `INCRBY` / `APPEND` / `GETEX`）· ZSet 分数区间筛选（`ZRANGEBYSCORE`）· 标签 / 备注 / 收藏 · 重命名 · 字段级 TTL · 绝对到期时刻（`EXPIREAT`）· 键栏显示存储编码 / 空闲时间 · 版本历史 · 集合的会话变更记录与结构化 diff · 值编辑器查找替换 · JSON 树视图与路径级操作（`JSON.SET` / `JSON.DEL` / `JSON.NUMINCRBY` / `JSON.TOGGLE` / `JSON.ARRAPPEND` / `JSON.STRAPPEND` / `JSON.CLEAR`，普通字符串里的 JSON 在本地应用）· 保存前 JSON 校验、格式化与压缩 · 本地回收站(24h)· 文件导入导出 · 批量操作(Tools 导出、前缀过滤、二进制 / JSON / CSV)· 跨服务器复制 & 对比——单键或整个前缀：`DUMP`/`RESTORE` 直传批量复制，以及两库对比 |
| 🔐 **安全 & 隐私** | 环境标签 + PROD 升级确认 · 只读锁 · ACL 编辑（安全日志、DRYRUN 权限测试、`ACL GENPASS` 生成密码、aclfile 保存/载入）· TLS/SSL & SSH · 分阶段连接诊断 · 断线自愈并跟随 Sentinel/Cluster 故障转移 · 每机密钥加密 · 纯本地、无遥测 |
| 🧭 **受限服务端** | 连接后自动探测能力：代理（Twemproxy / Codis / Envoy）、云托管（ElastiCache / Azure / Tair）和 Redis 兼容服务端（Valkey / Dragonfly / KeyDB / Kvrocks）上，依赖缺失命令的面板与按钮会灰显并*说明原因*（`CONFIG GET` 不支持、`SLOWLOG` 无权限）而不是报错 · 没有 `SCAN` 时键编辑器仍可按键名打开 · 完整命令矩阵在 工具 → 服务端能力 中查看 |
| ⌨️ **效率** | 多连接工作区标签页 · ⌘K 面板 · ⌘P 最近打开的键 · ⌘⇧F 多数据库键搜索 · ⌘/ 快捷键速查 · 自定义快捷键（`keybindings.toml`）· ⌘+/− 缩放 · 单实例 + `redis://` 链接 · redis-cli 带补全、按服务器的历史与 `Ctrl+R` 反向搜索 · AI 命令助手（终端内 `?`）· 多行 Batch 模式 · Lua 脚本库 · 可关闭的更新检查（下载带校验和验证，可含预发布 / nightly）· 时区与日期格式 · 本地数据备份（标签、收藏、脚本）· 可选系统托盘（macOS / Windows）· 应用自身请求可走 HTTP / SOCKS5 代理 · 滚动文件日志 · 导出诊断包（日志、崩溃报告、脱敏配置、连接状态打成一个 zip） |

> 🔐 **连接密钥存放位置：** 密码与 SSH 私钥用每台机器**唯一的随机密钥**加密 —— macOS 存 **钥匙串(Keychain)**、Windows 存 **凭据管理器**、**Linux** 存配置目录下 `0600` 权限的密钥文件(不依赖 Secret Service / D-Bus，headless 也能用)。密钥不离开本机，所以直接把配置文件拷到别的机器是解不开的 —— 跨机迁移请用带口令的导出功能。

📖 **[查看完整功能巡览 →](./docs/FEATURES_zh.md)**

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

`.msi` 以及 `.zip` 里的 `.exe` 都带 Authenticode 签名，详见[代码签名策略](#-代码签名策略code-signing-policy)。

### Linux

Arch Linux（AUR）：

```bash
yay -S zedis-bin
```

其他发行版：每个 [release](https://github.com/vicanso/zedis/releases/latest) 都附带 `.deb`、`.rpm`、AppImage 和普通 tarball（x86_64 与 aarch64）。

### Cargo（跨平台源码编译）

> **说明：** Zedis 依赖的是 GPUI 的未发布（git）版本，而 crates.io 不允许带 git 依赖发布，
> 因此**最新版本无法发布到 crates.io**，那里的版本可能滞后。想用最新版，建议用上面的
> Homebrew / Scoop / AUR 安装，或[下载发布版](https://github.com/vicanso/zedis/releases)；
> 要源码编译请用下面的 `--git` 命令。

```bash
# 来自 crates.io —— 可能是较旧的版本（见上方说明）
cargo install --locked zedis-gui

# 最新版：直接从 GitHub 源码编译（会解析 git 依赖）
cargo install --git https://github.com/vicanso/zedis --locked zedis-gui
```

---

## 🌐 Web 版（自托管）

Zedis 也能在浏览器里运行：同一套代码编译成 WebAssembly，用 canvas 渲染。浏览器无法直接建立 TCP 连接，所以由一个很小的 HTTP 服务 —— `zedis-bridge` —— 同时提供页面，并代替浏览器与 Redis 通信。在 Redis 旁边部署一次，整个团队打开浏览器就能用，无需安装任何东西。Redis 的密码只保存在 bridge 上（加密存储），不会发送给浏览器。

> **早期预览。** 镜像已发布 linux/amd64 与 linux/arm64 两个架构（约 26 MB）：正式发布对应 `:latest` 与版本号标签，`:nightly` 是跟随 `main` 的滚动构建。

### 快速试用

```bash
docker run -d --name zedis-web -p 7379:7379 \
  -e ZEDIS_BRIDGE_USERS="admin@change-me" \
  -v zedis-data:/data \
  vicanso/zedis-web:latest --listen 0.0.0.0:7379 --insecure-cookie
```

打开 <http://localhost:7379>，用 `admin` / `change-me` 登录。

- **`ZEDIS_BRIDGE_USERS` 必填** —— 格式为 `用户名@密码,用户名2@密码2`，未设置则 bridge 拒绝启动。每一项按第一个 `@` 切分，因此用户名不能包含 `@` 或 `:`，密码不能包含逗号。脚本可以用 HTTP Basic（`curl -u admin:change-me …/v1/servers`）。
- **`/data`** 保存服务器列表（其中的密码由同目录的 `master.key` 加密）和已保存的登录状态。请保留这个卷，否则每次重启都是空的。
- **`--insecure-cookie` 仅用于纯 http 的试用。** 登录 cookie 默认带 `Secure`，而浏览器会静默丢弃通过纯 http 收到的 `Secure` cookie —— 只有 `localhost` 例外（部分浏览器连 `localhost` 也不例外）。不加这个参数时，现象是登录成功、紧接着的请求返回 `401`。
- **Redis 跑在 Docker 宿主机上**时，容器内的 `127.0.0.1` 指的是容器自己。请使用 `host.docker.internal`（Linux 上需加 `--add-host=host.docker.internal:host-gateway`）或 `--network host`。

### 正式部署

纯 http 下，账号密码以及从 Redis 读到的所有数据都是明文传输。除试用外，请去掉 `--insecure-cookie`，端口只发布到回环地址，并在前面放一个 HTTPS 反向代理：

```bash
docker run -d --name zedis-web -p 127.0.0.1:7379:7379 \
  -e ZEDIS_BRIDGE_USERS="alice@…,bob@…" \
  -v zedis-data:/data \
  vicanso/zedis-web:latest
```

```caddyfile
zedis.example.com {
    reverse_proxy 127.0.0.1:7379
}
```

账号密码可以被猜测：如果 bridge 能从内网之外访问，请在代理层加上限流。

### 与其它项目共用域名

如果域名不是 Zedis 独占的，可以用 `ZEDIS_BRIDGE_BASE_PATH`（或 `--base-path`）给 bridge 指定一个专属路径。页面、静态资源和 API 会全部移到这个前缀之下，前缀之外的路径一律不响应；登录 cookie 的作用范围也限定在这个前缀内 —— 同域名下的其它项目不会收到它。

```bash
docker run -d --name zedis-web -p 127.0.0.1:7379:7379 \
  -e ZEDIS_BRIDGE_USERS="alice@…,bob@…" \
  -e ZEDIS_BRIDGE_BASE_PATH=/zedis \
  -v zedis-data:/data \
  vicanso/zedis-web:latest
```

```caddyfile
tools.example.com {
    # 用 `handle` 而不是 `handle_path`：前缀要原样转发，不能剥掉。
    handle /zedis* {
        reverse_proxy 127.0.0.1:7379
    }
    # … 其它项目
}
```

访问 `https://tools.example.com/zedis/`。nginx 的等价写法是 `location /zedis { proxy_pass http://127.0.0.1:7379; }` —— `proxy_pass` 末尾不要加斜杠，否则前缀会被剥掉。健康检查地址也随之变为 `/zedis/v1/health`。

### 账号与共享

服务器条目属于添加它的账号，其他人看不到；勾选 **Shared** 后则对所有账号可见。没有角色之分：任何账号都可以编辑或删除共享条目。

### Web 版不包含的功能

所有"一问一答"式的功能都可用：key 树、各类型的值编辑器、终端、指标、慢日志、配置、客户端、内存分析、按值搜索。浏览器中不可用的有：流式面板（`MONITOR`、Pub/Sub、键空间事件）、拓扑与 Sentinel 管理、Lua 脚本库与 Protobuf 描述编辑器、多数据库键搜索、迁移（文件导入 / 导出）与跨服务器对比、连接诊断、回收站与指标的 1h / 24h / 7d 历史（两者都需要刷新后仍在的存储），以及被浏览器自己占用的快捷键（⌘N / ⌘T / ⌘W）。标签、备注、收藏和保存的脚本可以使用，但只保存在当前页面，刷新后会清空。页面处于后台标签页时会自动降低轮询频率。被摘掉的面板会明确提示不可用，而不是报错。桌面版仍是功能完整的客户端。

不使用 Docker 的话，`make web-dist` 可以把同样的内容构建成一个自包含的单文件二进制。

---

## 🔏 代码签名策略（Code signing policy）

Free code signing provided by [SignPath.io](https://signpath.io), certificate by [SignPath Foundation](https://signpath.org).

每个 release 附带的 Windows 二进制 —— `zedis-windows-*.msi`，以及 `zedis-windows-*.zip` 里的 `zedis.exe` —— 都使用 SignPath Foundation 的证书做了 Authenticode 签名。被签名的就是 GitHub Actions 从本仓库对应 tag 构建出的产物（[`publish.yml`](./.github/workflows/publish.yml)），每次发版都经人工审批后才签名。macOS 版本另行使用维护者的 Apple Developer ID 签名并公证。

**团队**

- 提交者与审阅者（committers / reviewers）：[@vicanso](https://github.com/vicanso)
- 审批者（approvers）：[@vicanso](https://github.com/vicanso)

团队之外的改动一律以 pull request 提交，由提交者审阅后合并。

**隐私声明**

本程序不会向其他联网系统传输任何信息，除非用户或安装、运行本程序的人明确要求；仅有以下两处例外，且都由你掌控：

- **更新检查。** 启动时（最多每两天一次）Zedis 会从 GitHub Releases 下载发布清单，判断是否有新版本。**若 GitHub 无法访问**，同一请求会改用 [Gitee 镜像](https://gitee.com/vicanso/zedis)——发布流程会把每个正式版本原样复制过去——因此在访问不了 GitHub 的网络里，检查和下载依然可用。无论走哪一个地址，请求都只带应用版本号（`User-Agent`），不含任何关于你、你的机器或数据的信息；下载会用发布清单里的 SHA-256 校验，所以镜像若提供了别的内容，会和传输损坏一样被拒绝。可在 *设置 → 自动检查更新* 中关闭；新版本只有在你点击更新时才会下载。
- **AI 助手。** 只有在 *设置 → AI 服务地址* 中填入了端点之后，你明确交给它的文本（命令描述、内存报告）才会发送到该端点，不会发往别处。

你的 Redis 服务器、SSH 隧道和可选代理只连接到你指定的地址。Zedis 没有遥测，也不会上传崩溃报告：崩溃报告只保存在本地，直到你自己导出诊断包。

---

## 🤝 参与贡献

我们希望将 Zedis 打造成终极 Redis 客户端，非常欢迎你的参与！无论是新增功能、翻译界面还是修复 Bug，一切贡献都受到欢迎。

欢迎提交 issue 或 PR 参与进来。提交 PR 即表示你同意我们的[贡献者许可协议（CLA）](./CLA.md)。

## 📄 许可证

Zedis 是根据 [Apache License 2.0](./LICENSE) 授权的开源软件。
