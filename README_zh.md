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
  - **JSON & RedisJSON**：完整读写支持，内置美化输出与语法高亮。智能计算 RFC 7396 Merge Patch 差异，发送最小化的 `JSON.MERGE` 命令，而非全量覆盖写入。
  - **Protobuf & MessagePack**：零配置二进制反序列化，输出为可读的类 JSON 格式。
  - **媒体 & 十六进制**：原生预览图片（`PNG`、`JPG`、`WEBP`、`SVG`、`GIF`），以及自适应 8/16 字节十六进制转储，用于查看原始二进制数据。
- **自定义脚本查看器**：通过外部 Shell 命令对任意 Redis 值进行自定义解码。配置包含占位符（`{KEY}`、`{VALUE}`、`{HEX}`、`{RAW_FILE}`）的命令模板，Zedis 将在 Unix/macOS 上通过 `sh -c`、在 Windows 上通过 `cmd /c` 执行该命令，并将标准输出作为格式化结果显示。适用于 base64、自定义二进制协议或 `$PATH` 中的任意工具，支持按服务器配置精确匹配、前缀、后缀或正则表达式的键名匹配规则。
- **Hash 字段级 TTL**（Redis 7.4+）：通过 `HEXPIRE` / `HPERSIST` 为 Hash 中的特定字段设置独立过期时间，无需为了让部分字段过期而重新设计数据模型。
- **Redis Streams**：完整支持 Redis Streams——浏览流条目、查看消费者组与待处理消息（Pending Entries），并通过 `XINFO` 查看流元数据，全程无需离开 GUI。
- **Pub/Sub**：内置订阅/发布界面，支持订阅频道模式、实时接收消息，并可直接在 GUI 中发布消息，无需切换到 `redis-cli`。

### 📊 实时可观测性
内置 GPU 加速仪表盘，彻底改变你监控 Redis 实例的方式。
- **实时指标**：精美渲染的 CPU、内存和网络 I/O 实时图表。
- **内存分析器**：可视化排查 **BigKey**，优化存储效率，预防 OOM。
- **深度诊断**：追踪慢日志，通过关键字过滤实时监控 `MONITOR` 流，并通过直观的 GUI 管理活跃客户端（`CLIENT LIST/KILL`）。

### 🛡️ 企业级安全与效率
- **只读模式**：锁定连接，防止在生产环境中误操作写入数据。
- **高级隧道**：完整支持 TLS/SSL（自定义 CA、客户端证书）和 SSH 隧道（密码、私钥、SSH Agent）。
- **集成 CLI**：内置 `redis-cli` 终端，无需离开应用即可使用命令行。
- **命名空间树视图**：自动将以冒号（`:`）分隔的键整理为嵌套目录树，右键菜单支持刷新指定文件夹或一键删除该前缀下的所有键。
- **多选批量删除**：切换多选模式，一次性标记并删除多个键，无需编写任何命令。
- **键收藏与搜索历史**：收藏常用键以便随时快速访问，并可从持久化历史面板中回溯近期搜索记录。
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
