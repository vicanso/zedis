[中文](./README_zh.md) | English

<h1 align="center">Zedis</h1>

<p align="center">
  <strong>The Redis GUI that opens a million-key database without the spinner — native, GPU-accelerated, Rust 🦀 × GPUI ⚡️</strong>
</p>

<p align="center">
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-Apache--2.0-blue.svg" alt="License"></a>
  <a href="https://x.com/tree_xie"><img src="https://img.shields.io/twitter/follow/tree_xie?style=social" alt="Twitter Follow"></a>
  <img src="https://img.shields.io/github/downloads/vicanso/zedis/total" alt="Downloads">
  <a href="https://www.blazingly.fast"><img src="https://www.blazingly.fast/api/badge.svg?repo=vicanso%2Fzedis" alt="blazingly fast"></a>
</p>

<p align="center">
  <a href="#-features">Features</a> ·
  <a href="#-installation">Install</a> ·
  <a href="#-web-version-self-hosted">Web version</a> ·
  <a href="./docs/FEATURES.md">Full feature tour</a> ·
  <a href="https://zedis.net">Website</a>
</p>

<p align="center">
  <video src="https://github.com/user-attachments/assets/d7007ecf-bbfd-4e68-bbaf-437091f711e7" autoplay loop muted playsinline width="100%"></video>
</p>

---

## ✨ Features

**Zedis** is a native Redis and Valkey GUI. [GPUI](https://zed.dev) — the engine behind the Zed editor — draws every frame on the GPU, and `SCAN` is virtual-scrolled, so a million keys stay at 60+ FPS on a small memory footprint.

- 🧠 **Reads your values** — decompresses and decodes on its own: JSON, Protobuf, MessagePack, JWT, images and more, with a viewer for every Redis type and module.
- 📊 **Observability in the same window** — live metrics, a memory analyzer, hot keys, slow log, `MONITOR` and cluster health.
- 🔐 **Safe on production** — Prod connections start write-locked, a destructive command there asks for the server's name, secrets are encrypted per machine, and there is no telemetry.
- 🌐 **Connects to what you actually run** — TLS, SSH, Cluster, Sentinel; Redis and Valkey as first-class; Dragonfly, proxies and managed clouds grey out what they lack, with the reason.
- ⌨️ **For people who live in Redis** — ⌘K, a redis-cli with completion, an AI command assistant, copy and diff across servers, and the same app in the browser.

Already on Redis Insight, ARDM or Tiny RDM? **Paste the export and every connection lands at once.**

📖 **[Every panel, viewer and shortcut →](./docs/FEATURES.md)**

## 📦 Installation

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

Arch Linux (AUR):

```bash
yay -S zedis-bin
```

Other distributions: `.deb`, `.rpm`, an AppImage and a plain tarball (x86_64 and aarch64) are attached to every [release](https://github.com/vicanso/zedis/releases/latest).

<details>
<summary><strong>Build from source with Cargo</strong></summary>

Zedis depends on an unreleased (git) version of GPUI, which crates.io does not allow — so the crates.io build may lag behind. For the newest version use a package manager above, a [release download](https://github.com/vicanso/zedis/releases), or the `--git` command.

```bash
# From crates.io — may be an older version
cargo install --locked zedis-gui

# Latest: build straight from GitHub (resolves the git dependencies)
cargo install --git https://github.com/vicanso/zedis --locked zedis-gui
```

</details>

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

## 🌐 Web version (self-hosted)

The same GUI, compiled to WebAssembly. Host it next to Redis; the team opens a tab. `zedis-bridge` serves the page and talks to Redis — passwords stay on the bridge, encrypted at rest, and never reach the browser.

<p align="center">
  <img src="docs/images/architecture.svg" width="100%" alt="The Zedis UI and the Redis layer are one codebase in two programs. The desktop app is a native process holding both. The web build runs that UI as WebAssembly in a tab and calls zedis-bridge over HTTP; the bridge turns those calls into Redis commands with the same layer. Both talk RESP to standalone, Sentinel and Cluster Redis and Valkey.">
</p>

```bash
docker run -d --name zedis-web -p 7379:7379 \
  -e ZEDIS_BRIDGE_USERS="admin@change-me" \
  -v zedis-data:/data \
  vicanso/zedis-web:latest --listen 0.0.0.0:7379 --insecure-cookie
```

Open <http://localhost:7379> as `admin` / `change-me`. That command is a plain-http trial: drop `--insecure-cookie` and put HTTPS in front.

- **Accounts, roles, SSO** — sign-in is required; an entry is private until it is shared; an account can be read-only or limited to some servers; identity can come from a reverse-proxy header.
- **Production stays locked** — Prod entries open for 15 minutes at a time; the audit log records logins, entry changes and every confirmed command.
- **MCP** — Claude Code, Cursor or any MCP client reads Redis as a read-only account, every call audited.

> **Early preview.** linux/amd64 and linux/arm64 (~26 MB). A few desktop panels are missing in the browser; the guide lists them.

📖 **[Self-hosting: HTTPS, accounts, SSO, audit log, MCP →](./docs/WEB.md)**

## 🔀 Redis and Valkey

Valkey is first-class, not a compatibility mode. Each version-gated feature has its own Redis floor and Valkey floor, so a server is never sent a command it did not ship. What only Valkey has — `COMMANDLOG`, atomic slot migration, multiple databases in cluster — has a panel. Copy works both ways even though `DUMP` payloads do not. CI runs Redis 6.2–8 and Valkey 8.0–9.1 on every change. Dragonfly connects too: it is named in its own version, and the commands it does not have are probed and left out. A smoke lane in CI keeps it connecting; it is not held to the full matrix.

📖 **[Compatibility matrix →](./docs/REDIS_VALKEY.md)**

## 📚 Documentation

- **[Full feature tour](./docs/FEATURES.md)** — every panel, viewer and shortcut.
- **[Web version: self-hosting guide](./docs/WEB.md)** — deployment, accounts, SSO, the audit log and MCP.
- **[Redis and Valkey](./docs/REDIS_VALKEY.md)** — what differs between the two, and how a copy between them works.
- **[Security policy](./SECURITY.md)** · **[Changelog](./CHANGELOG.md)**

---

## 🔏 Code signing policy

macOS builds are signed and notarized with the maintainer's Apple Developer ID.

**Team**

- Committers and reviewers: [@vicanso](https://github.com/vicanso)
- Approvers: [@vicanso](https://github.com/vicanso)

Changes from outside the team arrive as pull requests and are reviewed by a committer before they are merged.

**Privacy policy**

This program will not transfer any information to other networked systems unless specifically requested by the user or the person installing or operating it, with two exceptions, both under your control:

- **Update check.** On startup, at most once every two days, Zedis downloads the release manifest from GitHub Releases to learn whether a newer version exists. **If GitHub cannot be reached**, the same request is retried against the [Gitee mirror](https://gitee.com/vicanso/zedis), which the release workflow copies every tagged release to — so the check, and any download you start, still work on networks where GitHub does not. Either way the request carries nothing but the app version in its `User-Agent` — nothing about you, your machine or your data — and a download is verified against the SHA-256 in the release manifest, so a mirror serving anything else fails the same check a corrupted transfer would. Turn it off with *Settings → Check for Updates Automatically*; a new version is only downloaded when you click Update.
- **AI assistant.** Only after you enter an endpoint under *Settings → AI Base URL*, the text you explicitly hand it — a command description, a memory report — is sent to that endpoint and nowhere else.

Your Redis servers, SSH tunnels and the optional proxy connect only where you point them. Zedis has no telemetry and sends no crash reports: they stay on disk until you export a diagnostics bundle yourself.

---

## 🤝 Contributing

Issues and PRs are welcome — features, translations, bug fixes. Opening a PR means you agree to the [Contributor License Agreement (CLA)](./CLA.md).

## 📄 License

Zedis is open-source software licensed under the [Apache License, Version 2.0](./LICENSE).
