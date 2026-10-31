中文 | [English](./README.md)

<h1 align="center">Zedis</h1>

<p align="center">
  <strong>能打开你那个百万 key 库、还不转圈的 Redis GUI —— 原生 GPU 加速，由 Rust 🦀 与 GPUI ⚡️ 驱动</strong>
</p>

<p align="center">
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-Apache--2.0-blue.svg" alt="License"></a>
  <a href="https://x.com/tree_xie"><img src="https://img.shields.io/twitter/follow/tree_xie?style=social" alt="Twitter Follow"></a>
  <img src="https://img.shields.io/github/downloads/vicanso/zedis/total" alt="Downloads">
  <a href="https://www.blazingly.fast"><img src="https://www.blazingly.fast/api/badge.svg?repo=vicanso%2Fzedis" alt="blazingly fast"></a>
</p>

<p align="center">
  <a href="#-功能">功能</a> ·
  <a href="#-安装">安装</a> ·
  <a href="#-web-版自托管">Web 版</a> ·
  <a href="./docs/FEATURES_zh.md">完整功能介绍</a> ·
  <a href="https://zedis.net/zh/">官网</a>
</p>

<p align="center">
  <video src="https://github.com/user-attachments/assets/d7007ecf-bbfd-4e68-bbaf-437091f711e7" autoplay loop muted playsinline width="100%"></video>
</p>

---

## ✨ 功能

**Zedis** 是原生的 Redis / Valkey GUI。[GPUI](https://zed.dev)（Zed 编辑器的渲染引擎）在 GPU 上画每一帧，`SCAN` 结果虚拟滚动，百万 key 也能保持 60+ FPS 和很低的内存占用。

- 🧠 **看得懂你的数据** —— 自动解压、自动解码：JSON、Protobuf、MessagePack、JWT、图片等等，每种 Redis 类型和模块都有专用查看器。
- 📊 **可观测性就在同一个窗口** —— 实时指标、内存分析、热点 key、慢日志、`MONITOR`、集群健康。
- 🔐 **生产环境锁得住** —— Prod 连接默认锁写，破坏性命令要输入服务器名，密码和私钥按机器加密，没有任何遥测。
- 🌐 **连你真正在跑的东西** —— TLS、SSH、Cluster、Sentinel；Redis 与 Valkey 同为一等公民；Dragonfly、Garnet、代理和云托管缺什么会灰显并说明原因。
- ⌨️ **给天天盯着 Redis 的人** —— ⌘K、带补全的 redis-cli、AI 命令助手、跨服务器复制与对比，同一套应用也能跑在浏览器里。

已经在用 Redis Insight、ARDM 或 Tiny RDM？**粘贴导出文件，所有连接一次迁入。**

📖 **[每个面板、查看器和快捷键 →](./docs/FEATURES_zh.md)**

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

### Linux

Arch Linux（AUR）：

```bash
yay -S zedis-bin
```

其他发行版：每个 [release](https://github.com/vicanso/zedis/releases/latest) 都附带 `.deb`、`.rpm`、AppImage 和普通 tarball（x86_64 与 aarch64）。

### GPUI Fast 版（试用）

每个 [release](https://github.com/vicanso/zedis/releases/latest) 还附带同一个应用的 GPUI Fast 版——改用 gpui-kit 的可选渲染引擎 GPUI Fast 绘制，x86_64 与 aarch64 都有：

| 平台 | 文件 |
| --- | --- |
| macOS | `Zedis-<arch>-fast.dmg` |
| Windows | `zedis-windows-<arch>-fast.msi`，或免安装的 `zedis-windows-<arch>-fast.zip` |
| Linux | `zedis-<arch>-fast.AppImage.tar.gz`，或普通 tarball `zedis-linux-<arch>-fast.tar.gz` |

提供它是为了先在真实使用中验证这个引擎，之后再决定怎么用；Homebrew、Scoop、winget、AUR 以及 `.deb` / `.rpm` 安装的都是普通版。按我们在 macOS 上的实测，它在大部分场景下的 CPU 占用都低于普通版：滚动 Config、Metrics 这类页面时约低 20%，数据表格和编辑器低几个百分点，key 树持平；内存占用约多 20%。

在 macOS 和 Windows 上它直接覆盖安装普通版；在 Linux 上它是单独的一个文件。两种情况都保留设置，之后的自动更新也会继续取 fast 版。*关于 → Copy System Info* 里的 `Renderer: GPUI Fast` 表示当前运行的是这个版本，反馈问题时请附上这段信息。想换回普通版，从同一页面下载普通版的文件安装即可。

<details>
<summary><strong>用 Cargo 从源码编译</strong></summary>

从 crates.io 用 `cargo install` 安装到的是已发布的版本。想用还没发布的最新代码，用下面的 `--git` 命令从 GitHub 编译。

```bash
# 已发布的版本，来自 crates.io
cargo install --locked zedis-gui

# 最新代码：直接从 GitHub 源码编译
cargo install --git https://github.com/vicanso/zedis --locked zedis-gui
```

</details>

## 📸 截图

<table>
  <tr>
    <td><a href="docs/images/key-browser.png"><img src="docs/images/key-browser.png" width="260" alt="键浏览与数据查看"></a></td>
    <td><a href="docs/images/memory-analyzer.png"><img src="docs/images/memory-analyzer.png" width="260" alt="内存分析器"></a></td>
    <td><a href="docs/images/live-metrics.png"><img src="docs/images/live-metrics.png" width="260" alt="实时指标"></a></td>
  </tr>
  <tr>
    <td align="center"><sub>键浏览与数据查看</sub></td>
    <td align="center"><sub>内存分析器</sub></td>
    <td align="center"><sub>实时指标</sub></td>
  </tr>
  <tr>
    <td><a href="docs/images/geo-map.png"><img src="docs/images/geo-map.png" width="260" alt="地理地图"></a></td>
    <td><a href="docs/images/vector-set.png"><img src="docs/images/vector-set.png" width="260" alt="向量集 + KNN"></a></td>
    <td><a href="docs/images/command-palette.png"><img src="docs/images/command-palette.png" width="260" alt="命令面板"></a></td>
  </tr>
  <tr>
    <td align="center"><sub>地理地图</sub></td>
    <td align="center"><sub>向量集 + KNN</sub></td>
    <td align="center"><sub>命令面板(⌘K)</sub></td>
  </tr>
</table>

## 🌐 Web 版（自托管）

同一套 GUI，编译成 WebAssembly。部署在 Redis 旁边，打开浏览器标签页就能用。`zedis-bridge` 提供页面并代替浏览器访问 Redis —— 密码只保存在 bridge 上（加密存储），不会到达页面。

<p align="center">
  <img src="docs/images/architecture.svg" width="100%" alt="Zedis 的界面和 Redis 连接层是同一份代码、两套程序。桌面应用是一个原生进程，两者都在其中。Web 版把同一套界面以 WebAssembly 跑在标签页里，通过 HTTP 访问 zedis-bridge；bridge 用同一套连接层把请求变成 Redis 命令。桌面应用和 bridge 都以 RESP 连接单机、哨兵与集群的 Redis 和 Valkey。">
</p>

```bash
docker run -d --name zedis-web -p 7379:7379 \
  -v zedis-data:/data \
  vicanso/zedis-web:latest --listen 0.0.0.0:7379 --insecure-cookie
docker logs zedis-web 2>&1 | grep "setup token"
```

打开 <http://localhost:7379>，用日志里的初始化令牌创建管理员。这条命令只适合纯 http 试用：正式使用请去掉 `--insecure-cookie`，并在前面加上 HTTPS。

- **账号、角色、SSO** —— 第一个账号是管理员，其他人凭邀请注册，或提交申请由管理员审核；条目默认私有；账号可以只读或只能看到部分服务器；身份可以来自反向代理写入的请求头。
- **生产环境默认锁写** —— Prod 每次解锁 15 分钟；审计日志记录登录、条目变更和每条经确认的命令。
- **MCP** —— Claude Code、Cursor 或任何 MCP 客户端以只读账号读 Redis，每次调用都留审计。
- **多个 bridge 共用一个数据库** —— 用 PostgreSQL 或 MySQL 保存账号、登录状态与审计，bridge 可以放在负载均衡之后（[指南](./docs/SQL_STORE_zh.md)）。

> **早期预览。** linux/amd64 与 linux/arm64（约 26 MB）。部分桌面面板在浏览器里不可用，指南里有清单。

📖 **[自托管：HTTPS、账号、SSO、审计日志、MCP →](./docs/WEB_zh.md)**

## 🔀 Redis 与 Valkey

Valkey 在这里是一等公民，不是兼容模式。依赖版本的功能能不能用，按两者各自的版本分别判断，服务器不会收到它没有实现的命令。Valkey 独有的功能（`COMMANDLOG`、原子槽迁移、集群多数据库）都有对应面板。Redis 与 Valkey 之间的复制两个方向都能成功，即使两边的 `DUMP` 载荷互不兼容。每次改动都会跑 Redis 6.2–8 和 Valkey 8.0–9.1 的集成测试。Dragonfly 和 Garnet 也能连接：会按各自的版本号识别并显示，它们没有的命令经探测后不再提供（比如 Garnet 没有 Stream）。CI 里各有一条冒烟测试保证连得上、基本操作可用，但不按完整矩阵要求。

📖 **[兼容矩阵 →](./docs/REDIS_VALKEY_zh.md)**

## 📚 文档

- **[完整功能介绍](./docs/FEATURES_zh.md)** —— 每个面板、查看器和快捷键。
- **[Web 版：自托管指南](./docs/WEB_zh.md)** —— 部署、账号、SSO、审计日志与 MCP。
- **[Web 版：PostgreSQL 与 MySQL 存储](./docs/SQL_STORE_zh.md)** —— Docker 启动命令、数据库用户、TLS、备份、升级与建表语句。
- **[Redis 与 Valkey](./docs/REDIS_VALKEY_zh.md)** —— 两者的差异，以及它们之间的复制如何工作。
- **[安全策略](./SECURITY.md)** · **[更新日志](./CHANGELOG.md)**

---

## 🔏 代码签名策略（Code signing policy）

macOS 版本使用维护者的 Apple Developer ID 签名并公证。

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

欢迎提交 issue 或 PR —— 新功能、翻译、修 bug 都可以。提交 PR 即表示你同意[贡献者许可协议（CLA）](./CLA.md)。

## 📄 许可证

Zedis 是根据 [Apache License 2.0](./LICENSE) 授权的开源软件。
