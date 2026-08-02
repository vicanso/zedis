[中文](./README_zh.md) | English

<h1 align="center">Zedis</h1>

<p align="center">
  <strong>The Redis GUI that opens your million-key database without the spinner — native, GPU-accelerated with Rust 🦀 and GPUI ⚡️</strong>
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

## 🤔 Why Zedis?

Tired of Electron-based Redis clients that eat gigabytes of RAM just to display a JSON string, freeze the instant you open a key with 100,000 elements, turn cluster mode into a chore, or render your compressed and binary values as garbled bytes? We were too.

**Zedis** is built from the ground up for developers who demand native performance. Powered by **GPUI** (the same rendering engine behind the [Zed Editor](https://zed.dev)), Zedis delivers a native, buttery-smooth 60+ FPS experience with a minimal memory footprint — even when navigating massive databases.

## ✨ Highlights

- 🦀 **Native, not Electron** — every pixel on the GPU, virtual-scrolled `SCAN`; millions of keys at 60+ FPS with tiny RAM.
- 🧠 **Understands your data** — auto-decompresses and decodes JSON/JSONPath, Protobuf, MessagePack, timestamps, images and hex, with purpose-built viewers for every Redis type and module.
- 📊 **Real-time observability** — live metrics, a memory analyzer with offline + AI recommendations, Slow Log ↔ Latency, `MONITOR`, and value search.
- 🔐 **Privacy-first & safe** — metadata stays in a local file, secrets are encrypted with a per-machine key, and destructive actions escalate their confirms on production.
- 🌐 **Connect anything** — TLS/SSL, SSH tunnels (incl. passphrase-protected keys), Cluster/Sentinel, import from Redis Insight / ARDM / Tiny RDM, and 8 UI languages.
- ⌨️ **Built for power users** — ⌘K command palette, redis-cli with completion + AI command assistant, batch mode, and cross-server copy/diff.

> ### 🔄 Already using Redis Insight?
> **Paste its database export and every connection lands at once** — no re-entering hosts, ports, and passwords one by one. Point Zedis at your real setup in about a minute, then judge the speed for yourself.

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
    <td><a href="https://github.com/user-attachments/assets/c06e4d80-7607-4d6c-807e-2a62a2ee556f"><img src="https://github.com/user-attachments/assets/c06e4d80-7607-4d6c-807e-2a62a2ee556f" width="260" alt="Key browser & data viewer"></a></td>
    <td><a href="https://github.com/user-attachments/assets/88091f50-ec77-41d5-acda-047a835079f8"><img src="https://github.com/user-attachments/assets/88091f50-ec77-41d5-acda-047a835079f8" width="260" alt="Memory analyzer"></a></td>
    <td><a href="https://github.com/user-attachments/assets/d5801a8c-da94-461b-83b6-6c9b70e2007d"><img src="https://github.com/user-attachments/assets/d5801a8c-da94-461b-83b6-6c9b70e2007d" width="260" alt="Live metrics"></a></td>
  </tr>
  <tr>
    <td align="center"><sub>Key browser & data viewer</sub></td>
    <td align="center"><sub>Memory analyzer</sub></td>
    <td align="center"><sub>Live metrics</sub></td>
  </tr>
  <tr>
    <td><a href="https://github.com/user-attachments/assets/2525cec9-5dd6-4049-9ea9-60fcb4cc249f"><img src="https://github.com/user-attachments/assets/2525cec9-5dd6-4049-9ea9-60fcb4cc249f" width="260" alt="Geo map"></a></td>
    <td><a href="https://github.com/user-attachments/assets/b4733051-2965-40ff-9d49-6bb909551513"><img src="https://github.com/user-attachments/assets/b4733051-2965-40ff-9d49-6bb909551513" width="260" alt="Vector Set + KNN"></a></td>
    <td><a href="https://github.com/user-attachments/assets/4335c12b-cbca-467e-abd9-7b50ffd568c5"><img src="https://github.com/user-attachments/assets/4335c12b-cbca-467e-abd9-7b50ffd568c5" width="260" alt="Command palette"></a></td>
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
| 🗂️ **Type & Module Viewers** | Bitmap · HyperLogLog · Vector Set (KNN) · Geo map · Bloom / Cuckoo / Count-Min / Top-K · Time Series · Streams (live-tail) · Pub/Sub (incl. sharded) · RediSearch · Functions |
| 📊 **Observability** | Live metrics + 7-day history · memory analyzer (live scan or offline RDB file) + AI tips · Slow Log ↔ Latency · `MONITOR` · value search · cluster health & slot reshard · persistence & keyspace events · typed CONFIG editor · raw INFO browser |
| 🔑 **Keys & Data** | Namespace tree with TTL chips · tags / notes / favorites · rename · field-level TTL · version history · local recycle bin (24h) · file import/export · bulk ops (incl. JSON/CSV export) · cross-server copy & diff |
| 🔐 **Security & Privacy** | Env tags with PROD-escalated confirms · read-only lock · ACL editor · TLS/SSL & SSH · staged connection diagnostics · per-machine encrypted secrets · local-only, no telemetry |
| ⌨️ **Productivity** | Multi-connection workspace tabs · ⌘K palette · ⌘⇧F multi-database key search · ⌘/ shortcut reference · redis-cli with completion · AI command assistant (`?` in terminal) · multi-line batch mode · Lua script library · opt-out update check with download progress · rotating file logs |

> 🔐 **Where connection secrets live:** passwords and SSH keys are encrypted with a random **per-machine** key — kept in the **macOS Keychain** or **Windows Credential Manager**, and in a `0600`-permission key file under the config dir on **Linux** (no Secret Service / D-Bus dependency, so it works headless too). The key never leaves the machine, so a copied config won't decrypt elsewhere — use the passphrase-protected export to move connections between machines.

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

> **Note:** Zedis depends on an unreleased (git) version of GPUI, which crates.io
> doesn't allow — so the **latest** version can't be published to crates.io, and
> the crates.io build may lag behind. For the newest version, prefer Homebrew /
> Scoop / the AUR above or a [release download](https://github.com/vicanso/zedis/releases);
> to build from source, use the `--git` command below.

```bash
# From crates.io — may be an older version (see note above)
cargo install --locked zedis-gui

# Latest: build straight from GitHub (resolves the git dependencies)
cargo install --git https://github.com/vicanso/zedis --locked zedis-gui
```

---

## 🤝 Contributing

We want to make Zedis the ultimate Redis client, and we'd love your help! Whether it's adding new features, translating the UI, or fixing bugs, all contributions are welcome.

Open an issue or a PR to get started. By submitting a PR, you agree to our lightweight [Contributor License Agreement (CLA)](./CLA.md).

## 📄 License

Zedis is open-source software licensed under the [Apache License, Version 2.0](./LICENSE).
