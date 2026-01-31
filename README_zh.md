中文 | [English](./README.md)

# Zedis

一个使用 **Rust** 🦀 和 **GPUI** ⚡️ 构建的高性能、GPU 加速的 Redis 客户端

[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)
![GitHub Downloads (all assets, all releases)](https://img.shields.io/github/downloads/vicanso/zedis/total)

![Zedis](./assets/zedis.png)

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

## ✨ 功能特性

### 🚀 极致速度
- **GPU 渲染**：所有 UI 元素均由 GPU 渲染，带来丝般顺滑的流畅体验。
- **虚拟列表**：利用虚拟滚动和 `SCAN` 迭代技术，轻松高效地处理 10 万级以上的 Key 列表。

### 🧠 智能数据检视
Zedis 自动检测内容类型 (`ViewerMode::Auto`) 并以最直观的格式呈现：
- **自动解压**：透明检测并解压 **LZ4**、**SNAPPY**、**GZIP** 和 **ZSTD** 数据（例如：压缩的 JSON 会被自动解压并格式化显示）。
- **JSON**：支持自动 **格式化美化 (Pretty-print)** 和完整的 **语法高亮**。
- **Protobuf**：支持反序列化 Protobuf 数据并自动 **格式化美化 (Pretty-print)** 和完整的 **语法高亮**。
- **MessagePack**：将二进制 MsgPack 数据反序列化为易读的类 JSON 格式。
- **图片预览**：原生支持存储图片的预览 (`PNG`, `JPG`, `WEBP`, `SVG`, `GIF`)。
- **Hex 视图**：自适应 8/16 字节的十六进制转储 (Hex Dump)，便于分析原始二进制数据。
- **文本支持**：UTF-8 校验与大文本流畅支持。

### 🛡️ 安全与防护
- **只读模式**：将连接标记为 **只读 (Read-only)**，防止意外写入或删除操作。让你能安心地检查生产环境数据，无后顾之忧。
- **SSH 隧道**：支持通过跳板机安全访问私有 Redis 实例。支持 密码、私钥认证。
- **TLS/SSL**：完整支持 SSL/TLS 加密连接，包括自定义 CA、客户端证书和私钥配置。

### ⚡ 开发效率
- **命令补全**：智能 **IntelliSense-style** 命令补全，提供实时语法建议和参数提示，基于你的 Redis 服务器版本。
- **搜索历史**：自动在本地记录搜索关键词。历史记录是 **连接隔离 (Connection-scoped)** 的，确保生产环境的查询记录不会污染本地开发工作流。
- **批量操作**：支持选择多个键进行批量删除或者指定前缀删除，简化批量数据管理。

### 🎨 现代体验
- **跨平台**：基于 GPUI 构建，在 **macOS**、**Windows** 和 **Linux** 上提供一致的高性能原生体验。
- **智能拓扑识别**：自动识别 **单机 (Standalone)**、**集群 (Cluster)** 或 **哨兵 (Sentinel)** 模式。只需连接任意节点，Zedis 自动处理拓扑映射，无需复杂配置。
- **多主题**：内置 **亮色**、**暗色** 以及 **跟随系统** 主题。
- **国际化**：完整支持 **英文** 和 **简体中文**。
- **响应式布局**：适应任意窗口尺寸的分栏设计。

🚧 开发阶段声明

Zedis 目前处于早期核心开发阶段 (Pre-Alpha)。为了保持架构的灵活性和开发节奏，我们暂时不接受 Pull Requests。

核心功能稳定后，我们将开放贡献。欢迎先 Star 或 Watch 本仓库以获取最新动态。

## 📄 许可证

本项目采用 [Apache License, Version 2.0](./LICENSE) 授权。