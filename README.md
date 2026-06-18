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

**Zedis** is built from the ground up for developers who demand native performance. Powered by **GPUI** (the same rendering engine behind the [Zed Editor](https://zed.dev)), Zedis delivers a native, buttery-smooth 60+ FPS experience with a minimal memory footprint — even when navigating massive databases.

## ✨ Highlights

- 🦀 **Native, not Electron** — every pixel on the GPU, virtual-scrolled `SCAN`; millions of keys at 60+ FPS with tiny RAM.
- 🧠 **Understands your data** — auto-decompresses and decodes JSON/JSONPath, Protobuf, MessagePack, timestamps, images and hex, with purpose-built viewers for every Redis type and module.
- 📊 **Real-time observability** — live metrics, a memory analyzer with offline + AI recommendations, Slow Log ↔ Latency, `MONITOR`, and value search.
- 🔐 **Privacy-first & safe** — metadata stays in a local file, secrets are encrypted, and destructive actions escalate their confirms on production.
- 🌐 **Connect anything** — TLS/SSL, SSH tunnels, Cluster/Sentinel, Redis Insight import, and 8 UI languages.
- ⌨️ **Built for power users** — ⌘K command palette, redis-cli with completion, batch mode, and cross-server copy/diff.

## 📸 Screenshots

<!--
  Approach A — host images on GitHub: drag-and-drop each screenshot into any
  issue/PR comment (or a release) to get a
  https://github.com/user-attachments/assets/... URL, then replace each
  REPLACE-* placeholder below. Clicking a thumbnail opens the full-resolution
  image; width="260" keeps the 3-wide grid to ~one screen.
  Suggested shots (most important first):
    1. key-browser     — namespace tree + JSON/syntax-highlighted value editor
    2. memory-analyzer — Top-N table + TTL histogram + recommendations
    3. live-metrics    — real-time GPU charts (CPU / memory / network)
    4. geo-map         — a sorted set plotted on the radar
    5. vector-set      — Vector Set + KNN (VSIM) results
    6. command-palette — the ⌘K palette
-->

<table>
  <tr>
    <td><a href="REPLACE-key-browser"><img src="REPLACE-key-browser" width="260" alt="Key browser & data viewer"></a></td>
    <td><a href="REPLACE-memory-analyzer"><img src="REPLACE-memory-analyzer" width="260" alt="Memory analyzer"></a></td>
    <td><a href="REPLACE-live-metrics"><img src="REPLACE-live-metrics" width="260" alt="Live metrics"></a></td>
  </tr>
  <tr>
    <td align="center"><sub>Key browser & data viewer</sub></td>
    <td align="center"><sub>Memory analyzer</sub></td>
    <td align="center"><sub>Live metrics</sub></td>
  </tr>
  <tr>
    <td><a href="REPLACE-geo-map"><img src="REPLACE-geo-map" width="260" alt="Geo map"></a></td>
    <td><a href="REPLACE-vector-set"><img src="REPLACE-vector-set" width="260" alt="Vector Set + KNN"></a></td>
    <td><a href="REPLACE-command-palette"><img src="REPLACE-command-palette" width="260" alt="Command palette"></a></td>
  </tr>
  <tr>
    <td align="center"><sub>Geo map</sub></td>
    <td align="center"><sub>Vector Set + KNN</sub></td>
    <td align="center"><sub>Command palette (⌘K)</sub></td>
  </tr>
</table>

## 🧩 Features at a Glance

| Area | What's inside |
| --- | --- |
| 🚀 **Native & Fast** | GPU rendering · virtual-scrolled `SCAN`, 60+ FPS on millions of keys · macOS / Windows / Linux · Light / Dark / System |
| 🧠 **Smart Data Viewer** | Auto-decompress (LZ4 / Snappy / GZIP / ZSTD) · JSON & RedisJSON + JSONPath · Protobuf · MessagePack · timestamps · images · hex · custom script viewer |
| 🗂️ **Type & Module Viewers** | Bitmap · HyperLogLog · Vector Set (KNN) · Geo map · Bloom / Cuckoo / Count-Min / Top-K · Time Series · Streams (live-tail) · Pub/Sub · RediSearch · Functions |
| 📊 **Observability** | Live metrics · memory analyzer + AI tips · Slow Log ↔ Latency · `MONITOR` · value search · cluster health · persistence & keyspace events |
| 🔑 **Keys & Data** | Namespace tree with TTL chips · tags / notes / favorites · rename · field-level TTL · version history · file import/export · bulk ops · cross-server copy & diff |
| 🔐 **Security & Privacy** | Env tags with PROD-escalated confirms · read-only lock · ACL editor · TLS/SSL & SSH · encrypted secrets · local-only, no telemetry |
| ⌨️ **Productivity** | ⌘K palette · ⌘/ shortcut reference · redis-cli with completion · multi-line batch mode · Lua script library |

📖 **[See the full feature tour →](./docs/FEATURES.md)**

---

## 📦 Installation

Ready to feel the speed? Install Zedis via your favorite package manager:

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

### Linux (Arch)

```bash
yay -S zedis-bin
```

### Cargo (Cross-Platform via Source)

```bash
# Latest published release (from crates.io)
cargo install --locked zedis-gui

# Or build the latest commit straight from GitHub
cargo install --git https://github.com/vicanso/zedis --locked zedis-gui
```

---

## 🤝 Contributing

We want to make Zedis the ultimate Redis client, and we'd love your help! Whether it's adding new features, translating the UI, or fixing bugs, all contributions are welcome.

Open an issue or a PR to get started. By submitting a PR, you agree to our lightweight [Contributor License Agreement (CLA)](./CLA.md).

## 📄 License

Zedis is open-source software licensed under the [Apache License, Version 2.0](./LICENSE).
