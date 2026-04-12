[中文](./README_zh.md) | English

<h1 align="center">Zedis</h1>

<p align="center">
  <strong>A High-Performance, GPU-Accelerated Redis GUI Client Built with Rust 🦀 and GPUI ⚡️</strong>
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

## 🤔 Why Zedis?

Tired of Electron-based Redis clients that eat gigabytes of RAM just to display a JSON string, or freeze entirely when you accidentally click a key with 100,000 elements? We were too. 

**Zedis** is built from the ground up for developers who demand native performance. Powered by **GPUI** (the same revolutionary rendering engine behind the [Zed Editor](https://zed.dev)), Zedis delivers a native, buttery-smooth 60+ FPS experience with a minimal memory footprint—even when navigating massive databases.

## ✨ Killer Features

### 🚀 Blazingly Fast & Native
- **GPU Rendering**: Every pixel is drawn on the GPU. Experience zero-lag scrolling and instant tab switching.
- **Virtual Lists**: Fearlessly browse instances with millions of keys. Virtual scrolling combined with `SCAN` iteration ensures your UI never blocks.
- **Cross-Platform**: A truly native feel across **macOS**, **Windows**, and **Linux**, complete with Light, Dark, and System themes.

### 🧠 Smart Data Viewer
Stop manually decoding your data. Zedis automatically detects (`ViewerMode::Auto`) and formats your payloads on the fly:
- **Auto-Decompression**: Transparently unpacks `LZ4`, `SNAPPY`, `GZIP`, and `ZSTD` data.
- **Rich Content Decoding**:
  - **JSON & RedisJSON**: Full read/write support with pretty-printing and syntax highlighting. Smartly computes RFC 7396 Merge Patch diffs to send minimal `JSON.MERGE` commands instead of heavy document overwrites.
  - **Protobuf & MessagePack**: Zero-config binary deserialization into readable JSON-like formats.
  - **Media & Hex**: Native preview for images (`PNG`, `JPG`, `WEBP`, `SVG`, `GIF`) and an adaptive 8/16-byte Hex dump for raw binary.
- **Custom Script Viewer**: Pipe any Redis value through an external shell command for fully custom decoding. Configure a shell command template with placeholders (`{KEY}`, `{VALUE}`, `{HEX}`, `{RAW_FILE}`) and Zedis runs it via `sh -c` (Unix/macOS) or `cmd /c` (Windows), displaying stdout as the formatted value. Perfect for base64, custom binary protocols, or any tool in your `$PATH`. Key patterns are matched by exact, prefix, suffix, or regex rules per server.
- **Hash Field-Level TTL** (Redis 7.4+): Set individual expiry times on specific hash fields using `HEXPIRE` / `HPERSIST` — no need to restructure your data model just to expire a subset of fields.

### 📊 Real-Time Observability
Transform how you monitor your Redis instances with a built-in, GPU-accelerated dashboard.
- **Live Metrics**: Beautifully rendered, real-time charts for CPU, Memory, and Network I/O.
- **Memory Analyzer**: Visually hunt down **BigKeys** and optimize storage efficiency to prevent OOMs.
- **Deep Diagnostics**: Track Slowlogs, monitor live `MONITOR` streams with powerful keyword filtering, and manage active clients (`CLIENT LIST/KILL`) via an intuitive GUI.

### 🛡️ Enterprise-Grade Security & Productivity
- **Read-Only Mode**: Lock down connections to prevent accidental writes in production environments.
- **Advanced Tunnels**: Full support for TLS/SSL (custom CA, client certs) and SSH Tunneling (Password, Private Key, SSH Agent).
- **Integrated CLI**: A built-in terminal for `redis-cli` allows you to leverage your command-line muscle memory without leaving the app.
- **Namespace Tree View**: Automatically groups keys separated by colons (`:`) into an easily manageable nested directory tree structure.

---

## 📦 Installation

Ready to feel the speed? Install Zedis via your favorite package manager:

### macOS
The recommended way to install Zedis is via Homebrew:

```bash
brew install --cask zedis
```

### Windows

Bash

```
scoop bucket add extras
scoop install zedis
```

### Linux (Arch)

Bash

```
yay -S zedis-bin
```

### Cargo (Cross-Platform via Source)

Bash

```
cargo install --locked zedis-gui
```

---

## 🤝 Contributing

We want to make Zedis the ultimate Redis client, and we'd love your help! Whether it's adding new features, translating the UI, or fixing bugs, all contributions are welcome.

Please read our [Contributing Guidelines](https://www.google.com/search?q=CONTRIBUTING.md) to get started. By submitting a PR, you agree to our lightweight [Contributor License Agreement (CLA)](https://www.google.com/search?q=CLA.md).

## 📄 License

Zedis is open-source software licensed under the [Apache License, Version 2.0](./LICENSE).
