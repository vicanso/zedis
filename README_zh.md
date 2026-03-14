中文 | [English](./README.md)

# Zedis

一个使用 **Rust** 🦀 和 **GPUI** ⚡️ 构建的高性能、GPU 加速的 Redis 客户端

[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)
![GitHub Downloads (all assets, all releases)](https://img.shields.io/github/downloads/vicanso/zedis/total)
[![blazingly fast](https://www.blazingly.fast/api/badge.svg?repo=vicanso%2Fzedis)](https://www.blazingly.fast)


![Zedis](./assets/demo.gif)

---

## 📖 简介

**Zedis** 是为追求速度的开发者设计的下一代 Redis GUI 客户端。

与处理大数据集时容易感到卡顿的基于 Electron 的客户端不同，Zedis 基于 **GPUI**（驱动 [Zed Editor](https://zed.dev) 的同一渲染引擎）构建。这确保了原生的、60 FPS 的流畅体验，即使在浏览数百万个键时，内存占用也极低。

## 📦 安装方式

### macOS
推荐使用 Homebrew 安装：

```bash
brew install --cask zedis
```

### Windows

```bash
scoop bucket add extras
scoop install zedis
```

### Arch linux

```bash
yay -S zedis-bin
```

## ✨ 核心特性

### 🚀 极致疾速
- **GPU 渲染**：所有 UI 元素均基于 GPU 渲染，带来如丝般顺滑的操作体验。
- **虚拟列表**：借助虚拟滚动技术与 `SCAN` 迭代，毫不费力地高效渲染 10 万+ 级别的数据列表。

### 🧠 智能数据查看器
**全面数据类型支持**：原生支持编辑 **String**, **List**, **Set**, **Sorted Set (ZSet)**, **Hash**, **Stream** 以及实时的 **Pub/Sub**（发布/订阅）频道。

Zedis 会自动检测内容类型 (`ViewerMode::Auto`)，并以最直观、实用的格式进行渲染：
- **无感自动解压**：自动检测并解压 **LZ4**, **SNAPPY**, **GZIP**, 和 **ZSTD** 压缩数据（例如：自动解压并格式化被压缩的 JSON 数据）。
- **富文本内容支持**：
  - **JSON**：自动**格式化（Pretty-print）**并提供完整的**语法高亮**。
  - **Protobuf**：零配置反序列化，并带有**语法高亮**。
  - **MessagePack**：将二进制 MsgPack 数据反序列化为易读的类 JSON 格式。
  - **图片**：原生预览存储的图片文件 (`PNG`, `JPG`, `WEBP`, `SVG`, `GIF`)。
- **十六进制视图**：自适应 8/16 字节的 Hex 视图，用于深度分析原始二进制数据。
- **文本**：支持严格的 UTF-8 验证与超大文本的高效显示。

### 🛡️ 安全防护
- **只读模式**：将连接标记为**只读**，防止任何意外的写入或删除操作。让您在排查生产环境时毫无后顾之忧。
- **SSH 隧道**：通过堡垒机安全访问内网 Redis 实例。全面支持密码、私钥以及 SSH Agent 身份认证。
- **TLS/SSL 加密**：全面支持加密连接，支持自定义 CA 证书、客户端证书和私钥配置。

### ⚡ 高效生产力
- **Pub/Sub 消息平台**：完全集成的发布与订阅界面。实时监听频道或模式匹配订阅、广播消息，并使用智能数据查看器瞬间解码复杂的 Payload（负载内容）。
- **命名空间分组**：自动将以冒号 (`:`) 分隔的 Key 渲染为嵌套的**树状视图**（例如 `user:1001:profile`）。轻松管理数百万个 Key，支持一键删除整个目录下的批量操作。
- **内置 CLI**：在 Zedis 内直接体验 `redis-cli` 的强大能力。执行原生命令、查看文本输出，无缝衔接您的命令行肌肉记忆，无需离开应用。
- **自动刷新**：为**键列表 (Key Lists)** 和**键值 (Key Values)** 配置自定义刷新频率，实时监控活数据。非常适合盯盘活跃队列或高频更新的缓存数据，告别繁琐的手动刷新。
- **命令自动补全**：智能的 **IntelliSense 风格** Redis 代码补全。根据您的 Redis 服务器版本，实时提供精准的语法建议和参数提示。
- **搜索历史**：在本地自动记录您的搜索记录。历史记录基于**连接隔离**，确保生产环境的查询记录绝不会污染您的本地开发工作流。
- **批量操作**：支持跨选多个 Key 进行批量删除，或根据特定前缀一次性清理数据，极大地简化海量数据管理。

### 🎨 现代化体验
- **跨平台原生体验**：由 GPUI 强力驱动，Zedis 在 **macOS**, **Windows**, 和 **Linux** 上均能提供丝滑、一致的原生级体验。
- **智能拓扑检测**：自动识别 **单机 (Standalone)**, **集群 (Cluster)**, 或 **哨兵 (Sentinel)** 架构。只需连接任意节点，Zedis 即可自动完成拓扑映射。
- **主题切换**：内置 **明亮 (Light)**, **暗黑 (Dark)** 主题，支持跟随 **系统 (System)** 自动切换。
- **国际化 (I18n)**：全面支持 **英语** 与 **简体中文**。
- **响应式布局**：自适应分割面板设计，完美适配任何尺寸的显示器窗口。

### 📊 实时可观测性与诊断
借助内置的、GPU 加速的性能看板与深度诊断工具，彻底重塑您监控 Redis 的方式。
- **实时服务器指标**：通过精美流畅的实时图表，持续掌握实例的 **CPU**, **内存**, 和 **网络 I/O** (kbps) 脉搏。
- **内存分析器 (Memory Analyzer)**：深入剖析 Redis 内存占用。直观可视化数据分布，瞬间定位大键 (**BigKeys**)，优化存储效率，把 OOM（内存溢出）危机扼杀在摇篮里。
- **慢查询排查 (Slowlog Inspector)**：通过专属的慢日志面板精准锁定性能瓶颈。轻松追踪慢查询，查看精确的执行耗时，并深度剖析命令参数，助力应用程序响应速度的极致优化。
- **深度诊断**：通过追踪 **命令吞吐量 (OPS)**, **延迟 (Latency)**, 和 **客户端连接数**，瞬间探明系统性能极限。
- **缓存健康度**：密切监控关键业务指标，如 **键命中率 (Key Hit Rate)** 和 **驱逐键 (Evicted Keys)**，防患于未然，彻底告别缓存雪崩。

🚧 开发阶段声明

Zedis 目前处于早期核心开发阶段 (Pre-Alpha)。为了保持架构的灵活性和开发节奏，我们暂时不接受 Pull Requests。

核心功能稳定后，我们将开放贡献。欢迎先 Star 或 Watch 本仓库以获取最新动态。

## 📄 许可证

本项目采用 [Apache License, Version 2.0](./LICENSE) 授权。