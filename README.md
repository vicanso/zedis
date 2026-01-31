[中文](./README_zh.md) | English

# Zedis

A High-Performance, GPU-Accelerated Redis Client Built with **Rust** 🦀 and **GPUI** ⚡️

[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)
[![Twitter Follow](https://img.shields.io/twitter/follow/tree0507?style=social)](https://x.com/tree0507)
![GitHub Downloads (all assets, all releases)](https://img.shields.io/github/downloads/vicanso/zedis/total)


![Zedis](./assets/zedis.png)

---

## 📖 Introduction

**Zedis** is a next-generation Redis GUI client designed for developers who demand speed. 

Unlike Electron-based clients that can feel sluggish with large datasets, Zedis is built on **GPUI** (the same rendering engine powering the [Zed Editor](https://zed.dev)). This ensures a native, 60 FPS experience with minimal memory footprint, even when browsing millions of keys.

## 📦 Installation

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
Zedis automatically detects content types (`ViewerMode::Auto`) and renders them in the most useful format:
- **Automatic Decompression**: Transparently detects and decompresses **LZ4**, **SNAPPY**, **GZIP**, and **ZSTD** data (e.g., compressed JSON is automatically unpacked and pretty-printed).
- **JSON**: Automatic **pretty-printing** with full **syntax highlighting**.
- **Protobuf**: Deserializes Protobuf data and automatically **pretty-printing** with full **syntax highlighting**.
- **MessagePack**: Deserializes binary MsgPack data into a readable JSON-like format.
- **Images**: Native preview for stored images (`PNG`, `JPG`, `WEBP`, `SVG`, `GIF`).
- **Hex View**: Adaptive 8/16-byte hex dump for analyzing raw binary data.
- **Text**: UTF-8 validation with large text support.

### 🛡️ Safety & Security
- **Read-only Mode**: Mark connections as **Read-only** to prevent accidental writes or deletions. Perfect for inspecting production environments with total peace of mind.
- **SSH Tunneling**: Securely access private Redis instances via bastion hosts. Supports authentication via Password, Private Key.
- **TLS/SSL**: Full support for encrypted connections, including custom CA, Client Certificates, and Private Keys.

### ⚡ Productivity
- **Command Autocomplete**: Intelligent **IntelliSense-style** code completion for Redis commands. It provides real-time syntax suggestions and parameter hints based on your Redis server version.
- **Search History**: Automatically records your search queries locally. History is **connection-scoped**, ensuring production queries never pollute your local development workflow.
- **Batch Operations**: Support selecting multiple keys for batch deletion or deleting keys with a specific prefix to simplify bulk data management.


### 🎨 Modern Experience
- **Cross-Platform**: Powered by GPUI, Zedis delivers a consistent, native experience across **macOS**, **Windows**, and **Linux**.
- **Smart Topology Detection**: Automatically identifies **Standalone**, **Cluster**, or **Sentinel** modes. Connect to any node, and Zedis handles the topology mapping automatically.
- **Themes**: Pre-loaded with **Light**, **Dark**, and **System** themes.
- **I18n**: Full support for **English** and **Chinese (Simplified)**.
- **Responsive**: Split-pane layout that adapts to any window size.

🚧 Development Status

Zedis is currently in early active development. To maintain development velocity and architectural flexibility, we are not accepting Pull Requests at this time.

We will open up for contributions once the core architecture stabilizes. Please Star or Watch the repository to stay updated!


## 📄 License

This project is Licensed under [Apache License, Version 2.0](./LICENSE).