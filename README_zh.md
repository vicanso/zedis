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
  <a href="#-安装">安装</a> ·
  <a href="#-功能一览">功能</a> ·
  <a href="#-web-版自托管">Web 版</a> ·
  <a href="./docs/FEATURES_zh.md">完整功能巡览</a> ·
  <a href="https://zedis.net/zh/">官网</a>
</p>

<p align="center">
  <video src="https://github.com/user-attachments/assets/d7007ecf-bbfd-4e68-bbaf-437091f711e7" autoplay loop muted playsinline width="100%"></video>
</p>

---

## 🤔 为什么选择 Zedis？

厌倦了那些为了显示一个 JSON 字符串就吃掉几 GB 内存、点开一个 10 万元素的键就卡死、把压缩和二进制 value 显示成乱码的 Electron Redis 客户端？我们也是。

**Zedis** 是原生应用，由 **GPUI**（[Zed 编辑器](https://zed.dev)背后的渲染引擎）在 GPU 上绘制。即使面对百万级 key 的数据库，也能保持 60+ FPS 和很低的内存占用。

## ✨ 亮点

- 🦀 **原生，而非 Electron** —— 每个像素都在 GPU 上绘制，`SCAN` 结果虚拟滚动：百万级 key、60+ FPS、内存占用很低。
- 🧠 **看得懂你的数据** —— 自动解压、自动解码：JSON、Protobuf、MessagePack、JWT、图片等等，每种 Redis 类型和模块都有专用查看器。
- 📊 **自带可观测性** —— 实时指标、内存分析、热点 key、慢日志、`MONITOR`、集群健康，都在一个窗口里。
- 🔐 **对生产环境足够谨慎** —— Prod 连接默认锁写，在生产上执行破坏性命令要输入服务器名确认，密钥按机器加密，没有任何遥测。
- 🌐 **什么都能连** —— TLS、SSH 隧道、Cluster、Sentinel；Redis 与 Valkey 同为一等公民；遇到代理或云托管，缺失的功能会灰显并说明原因，而不是报错。
- ⌨️ **为重度用户而生** —— ⌘K 命令面板、带补全的 redis-cli、AI 命令助手、跨服务器复制与对比。
- 🕸️ **浏览器里也能用** —— 同一套代码编译成 WebAssembly，一个约 26 MB 的 Docker 镜像即可自托管，并为 AI 助手留了一扇只读的 MCP 门。

> ### 🔄 已经在用 Redis Insight？
> **粘贴它导出的数据库配置，所有连接一次迁入** —— 不用一个个重填地址、端口和密码。ARDM 和 Tiny RDM 的导出同样可以直接导入。花大约一分钟，就能拿你真实的连接试试 Zedis，快不快自己判断。

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

## 📦 安装

### macOS

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

<details>
<summary><strong>用 Cargo 从源码编译</strong></summary>

Zedis 依赖的是 GPUI 的未发布（git）版本，而 crates.io 不允许带 git 依赖发布，所以 crates.io 上的版本可能滞后。想用最新版，请用上面的包管理器、[下载发布版](https://github.com/vicanso/zedis/releases)，或使用下面的 `--git` 命令。

```bash
# 来自 crates.io —— 可能是较旧的版本
cargo install --locked zedis-gui

# 最新版：直接从 GitHub 源码编译（会解析 git 依赖）
cargo install --git https://github.com/vicanso/zedis --locked zedis-gui
```

</details>

## 🧩 功能一览

| 领域 | 包含内容 |
| --- | --- |
| 🚀 **原生 & 快** | GPU 渲染 · 虚拟滚动 `SCAN` · macOS / Windows / Linux · 浅色、深色与 6 套内置主题 · 8 种界面语言 |
| 🧠 **智能数据查看器** | LZ4 / Snappy / GZIP / ZSTD · JSON + JSONPath · Protobuf · MessagePack · Java / PHP / pickle · BSON · JWT · 图片 · Hex · 自定义脚本查看器 |
| 🗂️ **类型 & 模块** | Hash / List / Set / ZSet / Stream 编辑器 · 位图 · HyperLogLog · 地理地图 · 向量集（KNN）· JSON · 搜索 · 时间序列 · Bloom 系列 · Pub/Sub · Functions |
| 📊 **可观测性** | 实时指标与 7 天历史 · 内存分析（在线扫描或离线 RDB）与 AI 建议 · 热点 key · 慢日志 ↔ Latency · `MONITOR` · 按值搜索 · 集群重分片与再平衡 · 带类型的 CONFIG 编辑器 |
| 🔑 **Keys & 数据** | 命名空间树 · 标签、备注、收藏 · 字段级 TTL · 版本历史与 diff · 24 小时回收站 · 导入 / 导出 · 跨服务器复制与对比 |
| 🔐 **安全 & 隐私** | 环境标签 · Prod 默认锁写 · 升级确认 · ACL 编辑 · TLS 与 SSH · 密钥用每机唯一密钥加密 · 纯本地、无遥测 |
| 🧭 **各种服务端** | Redis 与 Valkey · Cluster / Sentinel · 代理、云托管与 Redis 兼容服务端在连接后自动探测，缺失的功能灰显并说明原因 |
| ⌨️ **效率** | 工作区标签页 · ⌘K 面板 · ⌘P 最近打开的键 · 带历史与补全的 redis-cli · AI 命令助手 · Batch 模式 · Lua 脚本库 · 自定义快捷键 |

📖 **[全部细节：完整功能巡览 →](./docs/FEATURES_zh.md)**

## 🌐 Web 版（自托管）

同一套代码编译成 WebAssembly：在 Redis 旁边部署一次，整个团队打开浏览器标签页就能用，无需安装任何东西。一个很小的服务 `zedis-bridge` 负责提供页面，并代替浏览器与 Redis 通信 —— Redis 的密码只保存在 bridge 上（加密存储），不会发送给页面。

<p align="center">
  <img src="docs/images/architecture.svg" width="100%" alt="代码复用示意图（不是部署图）：Zedis 的界面和 Redis 连接层是编译进多个程序的同一份代码，都不是独立运行的服务。桌面应用是一个原生进程，两者都在其中，界面在进程内直接调用连接层。Web 版里，同一套界面以 WebAssembly 形式运行在浏览器标签页中，通过 HTTP API 访问 zedis-bridge；bridge 是另一个服务进程，用同一套连接层把这些请求转换成 Redis 命令。桌面应用和 bridge 各自以 RESP 连接单机、哨兵与集群部署的 Redis 和 Valkey。">
</p>

```bash
docker run -d --name zedis-web -p 7379:7379 \
  -e ZEDIS_BRIDGE_USERS="admin@change-me" \
  -v zedis-data:/data \
  vicanso/zedis-web:latest --listen 0.0.0.0:7379 --insecure-cookie
```

打开 <http://localhost:7379>，用 `admin` / `change-me` 登录。这条命令只适合纯 http 试用：正式使用请去掉 `--insecure-cookie`，并在前面加上 HTTPS。

- **账号与角色** —— 必须登录；条目默认私有，勾选共享后才对他人可见；账号可以设为只读，或只能看到部分服务器。
- **接入你自己的 SSO** —— bridge 可以采用认证反向代理写入请求头的身份。
- **生产环境有保护** —— Prod 条目默认锁写，每次解锁 15 分钟；审计日志记录登录、条目变更和每条经确认的命令。
- **AI 助手（MCP）** —— Claude Code、Cursor 或任何 MCP 客户端都能以只读账号读取你的 Redis，每次调用都留审计。

> **早期预览。** 镜像已发布 linux/amd64 与 linux/arm64（约 26 MB）。部分桌面面板在浏览器里不可用，指南里有完整清单。

📖 **[自托管指南：HTTPS、账号、SSO、审计日志、MCP →](./docs/WEB_zh.md)**

## 🔀 Redis 与 Valkey

Valkey 在这里不是“兼容模式”。每个依赖服务器版本的功能都为两者分别设了门槛，所以服务器不会收到它没有实现的命令；Valkey 独有的功能（`COMMANDLOG`、原子槽迁移、集群模式下的多数据库）也都有对应的面板或操作。Redis 与 Valkey 之间的复制两个方向都能落地，即使它们的 `DUMP` 载荷互不相认。集成测试在每次改动时都会跑 Redis 6.2 – 8 和 Valkey 8.0 – 9.1。

📖 **[完整兼容矩阵 →](./docs/REDIS_VALKEY_zh.md)**

## 📚 文档

- **[完整功能巡览](./docs/FEATURES_zh.md)** —— 每个面板、查看器和快捷键。
- **[Web 版：自托管指南](./docs/WEB_zh.md)** —— 部署、账号、SSO、审计日志与 MCP。
- **[Redis 与 Valkey](./docs/REDIS_VALKEY_zh.md)** —— 两者的差异，以及它们之间的复制如何工作。
- **[安全策略](./SECURITY.md)** · **[更新日志](./CHANGELOG.md)**

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
