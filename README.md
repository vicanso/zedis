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
  <a href="#-installation">Install</a> ·
  <a href="#-features-at-a-glance">Features</a> ·
  <a href="#-web-version-self-hosted">Web version</a> ·
  <a href="./docs/FEATURES.md">Full feature tour</a> ·
  <a href="https://zedis.net">Website</a>
</p>

<p align="center">
  <video src="https://github.com/user-attachments/assets/d7007ecf-bbfd-4e68-bbaf-437091f711e7" autoplay loop muted playsinline width="100%"></video>
</p>

---

## 🤔 Why Zedis?

Tired of Electron-based Redis clients that eat gigabytes of RAM to show one JSON string, freeze when a key has 100,000 elements, and turn compressed or binary values into garbled bytes? We were too.

**Zedis** is a native app, drawn on the GPU by **GPUI** — the rendering engine behind the [Zed editor](https://zed.dev). It stays at 60+ FPS on a small memory footprint, even in a database with millions of keys.

## ✨ Highlights

- 🦀 **Native, not Electron** — every pixel on the GPU and a virtual-scrolled `SCAN`: millions of keys, 60+ FPS, little RAM.
- 🧠 **Understands your data** — decompresses and decodes by itself: JSON, Protobuf, MessagePack, JWT, images and more, with a viewer for every Redis type and module.
- 📊 **Observability built in** — live metrics, a memory analyzer, hot keys, slow log, `MONITOR` and cluster health in one window.
- 🔐 **Careful with production** — Prod connections start write-locked, a destructive command there asks for the server's name, secrets are encrypted per machine, and there is no telemetry.
- 🌐 **Connects to anything** — TLS, SSH tunnels, Cluster and Sentinel; Redis and Valkey are both first-class; proxies and managed clouds grey out what they lack, with the reason, instead of failing.
- ⌨️ **Built for power users** — ⌘K command palette, a redis-cli with completion, an AI command assistant, copy and diff across servers.
- 🕸️ **In the browser too** — the same app as WebAssembly, self-hosted from one ~26 MB Docker image, with a read-only MCP door for AI assistants.

> ### 🔄 Already using Redis Insight?
> **Paste its database export and every connection lands at once** — no re-entering hosts, ports, and passwords one by one. Exports from ARDM and Tiny RDM import the same way. Point Zedis at your real setup in about a minute, then judge the speed for yourself.

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

The `.msi` and the `.exe` in the `.zip` are Authenticode-signed — see the [Code signing policy](#-code-signing-policy).

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

## 🧩 Features at a Glance

| Area | What's inside |
| --- | --- |
| 🚀 **Native & fast** | GPU rendering · virtual-scrolled `SCAN` · macOS / Windows / Linux · light, dark and 6 bundled themes · 8 UI languages |
| 🧠 **Smart data viewer** | LZ4 / Snappy / GZIP / ZSTD · JSON + JSONPath · Protobuf · MessagePack · Java / PHP / pickle · BSON · JWT · images · hex · your own script viewer |
| 🗂️ **Types & modules** | Hash / List / Set / ZSet / Stream editors · Bitmap · HyperLogLog · Geo map · Vector Set (KNN) · JSON · Search · Time Series · Bloom family · Pub/Sub · Functions |
| 📊 **Observability** | Live metrics with 7-day history · memory analyzer (live scan or offline RDB) with AI tips · hot keys · Slow Log ↔ Latency · `MONITOR` · value search · cluster reshard and rebalance · typed CONFIG editor |
| 🔑 **Keys & data** | Namespace tree · tags, notes, favorites · field-level TTL · version history and diff · 24h recycle bin · import / export · copy and compare across servers |
| 🔐 **Security & privacy** | Environment tags · write lock on Prod · escalated confirms · ACL editor · TLS & SSH · secrets encrypted with a per-machine key · local-only, no telemetry |
| 🧭 **Any server** | Redis and Valkey · Cluster / Sentinel · proxies, managed clouds and Redis-compatible servers are probed after connect, and what they lack is greyed out with the reason |
| ⌨️ **Productivity** | Workspace tabs · ⌘K palette · ⌘P recent keys · redis-cli with history and completion · AI command assistant · batch mode · Lua script library · custom keybindings |

📖 **[Everything, in detail: the full feature tour →](./docs/FEATURES.md)**

## 🌐 Web version (self-hosted)

The same app, compiled to WebAssembly: host it once next to your Redis servers and the whole team opens them from a browser tab, with nothing to install. A small server, `zedis-bridge`, serves the page and talks to Redis for the browser — Redis passwords stay on the bridge, encrypted at rest, and never reach the page.

<p align="center">
  <img src="docs/images/architecture.svg" width="100%" alt="Code-sharing diagram, not a deployment: the Zedis UI and the Redis layer are code compiled into more than one program, and neither runs as a service of its own. The desktop app is one native process holding both, and its UI calls the Redis layer in-process. In the web version the same UI runs as WebAssembly in a browser tab and calls zedis-bridge over an HTTP API; the bridge is a separate server process that turns those calls into Redis commands with the same Redis layer. The desktop app and the bridge each talk RESP to standalone, Sentinel and Cluster deployments of Redis and Valkey.">
</p>

```bash
docker run -d --name zedis-web -p 7379:7379 \
  -e ZEDIS_BRIDGE_USERS="admin@change-me" \
  -v zedis-data:/data \
  vicanso/zedis-web:latest --listen 0.0.0.0:7379 --insecure-cookie
```

Open <http://localhost:7379> and sign in as `admin` / `change-me`. That command is a plain-http trial: past it, drop `--insecure-cookie` and put HTTPS in front.

- **Accounts and roles** — sign-in is required, an entry is private until it is shared, and an account can be read-only or limited to some servers.
- **Your own SSO** — the bridge can take the identity from the header an authenticating reverse proxy writes.
- **Production stays safe** — Prod entries are write-locked and open for 15 minutes at a time, and an audit log records logins, entry changes and every confirmed command.
- **AI assistants (MCP)** — Claude Code, Cursor or any MCP client can read your Redis as a read-only account, every call audited.

> **Early preview.** Published for linux/amd64 and linux/arm64 (~26 MB). Some desktop panels are not in the browser; the guide lists them.

📖 **[Self-hosting guide: HTTPS, accounts, SSO, audit log, MCP →](./docs/WEB.md)**

## 🔀 Redis and Valkey

Valkey is not a compatibility mode here. Every feature that depends on a server version has a floor of its own for each of the two, so a server is never sent a command it did not ship — and what only Valkey has (`COMMANDLOG`, atomic slot migration, multiple databases in cluster mode) has its panel or action. A copy between a Redis and a Valkey lands in both directions, even though their `DUMP` payloads do not. The live integration suite runs Redis 6.2 – 8 and Valkey 8.0 – 9.1 on every change.

📖 **[The full compatibility matrix →](./docs/REDIS_VALKEY.md)**

## 📚 Documentation

- **[Full feature tour](./docs/FEATURES.md)** — every panel, viewer and shortcut.
- **[Web version: self-hosting guide](./docs/WEB.md)** — deployment, accounts, SSO, the audit log and MCP.
- **[Redis and Valkey](./docs/REDIS_VALKEY.md)** — what differs between the two, and how a copy between them works.
- **[Security policy](./SECURITY.md)** · **[Changelog](./CHANGELOG.md)**

---

## 🔏 Code signing policy

Free code signing provided by [SignPath.io](https://signpath.io), certificate by [SignPath Foundation](https://signpath.org).

The Windows binaries attached to each release — `zedis-windows-*.msi` and the `zedis.exe` inside `zedis-windows-*.zip` — are Authenticode-signed with the SignPath Foundation certificate. What gets signed is exactly what GitHub Actions built from the tagged commit of this repository ([`publish.yml`](./.github/workflows/publish.yml)), and every release is approved by hand before it is signed. macOS builds are signed and notarized separately with the maintainer's Apple Developer ID.

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

We want to make Zedis the ultimate Redis client, and we'd love your help! Whether it's adding new features, translating the UI, or fixing bugs, all contributions are welcome.

Open an issue or a PR to get started. By submitting a PR, you agree to our lightweight [Contributor License Agreement (CLA)](./CLA.md).

## 📄 License

Zedis is open-source software licensed under the [Apache License, Version 2.0](./LICENSE).
