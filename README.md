[中文](./README_zh.md) | English

# Zedis

A High-Performance, GPU-Accelerated Redis Client Built with **Rust** 🦀 and **GPUI** ⚡️

[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)
[![Twitter Follow](https://img.shields.io/twitter/follow/tree0507?style=social)](https://x.com/tree0507)
![GitHub Downloads (all assets, all releases)](https://img.shields.io/github/downloads/vicanso/zedis/total)
[![blazingly fast](https://www.blazingly.fast/api/badge.svg?repo=vicanso%2Fzedis)](https://www.blazingly.fast)



![Zedis](https://raw.githubusercontent.com/vicanso/zedis/main/assets/demo.gif)

---

## 📖 Introduction

**Zedis** is a next-generation Redis GUI client designed for developers who demand speed. 

Unlike Electron-based clients that can feel sluggish with large datasets, Zedis is built on **GPUI** (the same rendering engine powering the [Zed Editor](https://zed.dev)). This ensures a native, 60 FPS experience with minimal memory footprint, even when browsing millions of keys.

## 📦 Installation

### Cargo

```bash
cargo install --locked zedis-gui
```

### macOS
The recommended way to install Zedis is via Homebrew:

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

## ✨ Features

### 🚀 Blazing Fast
- **GPU Rendering**: All UI elements are rendered on the GPU for buttery smooth performance.
- **Virtual List**: Efficiently handle lists with 100k+ keys using virtual scrolling and `SCAN` iteration.

### 🧠 Smart Data Viewer
**Comprehensive Type Support**: Native editors for **String**, **List**, **Set**, **Sorted Set (ZSet)**, **Hash**, **Stream**, and real-time **Pub/Sub** channels.

Zedis automatically detects content types (`ViewerMode::Auto`) and renders them in the most useful format:
- **Automatic Decompression**: Transparently detects and decompresses **LZ4**, **SNAPPY**, **GZIP**, and **ZSTD** data (e.g., compressed JSON is automatically unpacked and pretty-printed).
- **Rich Content Support**:
  - **JSON**: Automatic **pretty-printing** with full **syntax highlighting**.
  - **Protobuf**: Zero-config deserialization with **syntax highlighting**.
  - **MessagePack**: Deserializes binary MsgPack data into a readable JSON-like format.
  - **Images**: Native preview for stored images (`PNG`, `JPG`, `WEBP`, `SVG`, `GIF`).
- **Hex View**: Adaptive 8/16-byte hex dump for analyzing raw binary data.
- **Text**: UTF-8 validation with large text support.

### 🛡️ Safety & Security
- **Read-only Mode**: Mark connections as **Read-only** to prevent accidental writes or deletions. Perfect for inspecting production environments with total peace of mind.
- **SSH Tunneling**: Securely access private Redis instances via bastion hosts. Supports authentication via Password, Private Key, and SSH Agent.
- **TLS/SSL**: Full support for encrypted connections, including custom CA, Client Certificates, and Private Keys.

### ⚡ Productivity
- **Pub/Sub Messaging**: Fully integrated Publisher and Subscriber interface. Listen to channels or pattern subscriptions in real-time, broadcast messages, and instantly decode complex payloads using the smart data viewer.
- **Namespace Grouping**: Automatically renders keys separated by colons (`:`) into a nested **Tree View** (e.g., `user:1001:profile`). Easily manage millions of keys by performing batch operations, such as deleting an entire key directory.
- **Integrated CLI**: Experience the power of `redis-cli` directly within Zedis. Execute raw commands, view text outputs, and leverage your existing command-line muscle memory without leaving the app.
- **Auto Refresh**: Monitor live data with configurable refresh intervals for both **Key Lists** and **Key Values**. Perfect for watching active queues or volatile cache data without manual reloading.
- **Command Autocomplete**: Intelligent **IntelliSense-style** code completion for Redis commands. It provides real-time syntax suggestions and parameter hints based on your Redis server version.
- **Search History**: Automatically records your search queries locally. History is **connection-scoped**, ensuring production queries never pollute your local development workflow.
- **Batch Operations**: Support selecting multiple keys for batch deletion or deleting keys with a specific prefix to simplify bulk data management.

### 🎨 Modern Experience
- **Cross-Platform**: Powered by GPUI, Zedis delivers a consistent, native experience across **macOS**, **Windows**, and **Linux**.
- **Smart Topology Detection**: Automatically identifies **Standalone**, **Cluster**, or **Sentinel** modes. Connect to any node, and Zedis handles the topology mapping automatically.
- **Themes**: Pre-loaded with **Light**, **Dark**, and **System** themes.
- **I18n**: Full support for **English** and **Chinese (Simplified)**.
- **Responsive**: Split-pane layout that adapts to any window size.

### 📊 Real-Time Observability & Diagnostics
Transform how you monitor Redis with a built-in, GPU-accelerated performance dashboard and deep diagnostic tools.
- **Live Server Metrics**: Keep a pulse on your instance with beautifully rendered, real-time charts for **CPU**, **Memory**, and **Network I/O** (kbps).
- **Memory Analyzer**: Deep dive into your Redis memory footprint. Visualize data distribution, instantly identify large keys (**BigKeys**), and optimize your storage efficiency to prevent OOM scenarios.
- **Slowlog Inspector**: Pinpoint performance bottlenecks with a dedicated Slowlogs panel. Effortlessly track slow queries, view exact execution times, and analyze command arguments to optimize your application's responsiveness.
- **Live Monitor**: Gain instant, real-time visibility into the exact commands your applications are sending to Redis. Stream the output of the `MONITOR` command in a buttery-smooth UI, and effortlessly tame the data firehose with powerful keyword and command-type filtering to debug complex interactions on the fly.
- **Client Management**: Visually explore and manage active connections via an intuitive interface for `CLIENT LIST`. Sort clients by IP, connected duration, or idle time, and effortlessly terminate (`CLIENT KILL`) rogue or zombie connections with a single click to prevent server blocking.
- **Deep Diagnostics**: Instantly spot system limits by tracking **Command Throughput (OPS)**, **Latency**, and **Client Connections**.
- **Cache Health**: Monitor critical business metrics like **Key Hit Rate** and **Evicted Keys** to prevent cache avalanches before they happen.
🚧 Development Status

Zedis is currently in early active development. To maintain development velocity and architectural flexibility, we are not accepting Pull Requests at this time.

We will open up for contributions once the core architecture stabilizes. Please Star or Watch the repository to stay updated!


## 📄 License

This project is Licensed under [Apache License, Version 2.0](./LICENSE).