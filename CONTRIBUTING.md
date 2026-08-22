# Contributing to Zedis / 为 Zedis 贡献

## 🇬🇧 English

Thanks for your interest in improving Zedis! 🦀 This guide covers how to get set up and what we look for in a contribution.

### Ways to contribute
- 🐛 **Report bugs** — use the Bug Report issue template.
- ✨ **Propose features** — open a Feature Request issue **first** so we can align on scope before you write code.
- 🌍 **Improve translations** — the 8 bundled locales (`en zh de es fr ja pt ru`).
- 📝 **Improve docs** — README, `docs/FEATURES*.md`, and inline docs.

### Before you start
- **New features:** open an issue to discuss first — it saves your time and ours, and ensures the work fits the roadmap.
- **Typos / formatting:** please don't open PRs *solely* for minor doc/comment typos; we batch those or fix them alongside larger changes.

### Development setup
Zedis is built with Rust (edition 2024; **Rust 1.95.0** is the toolchain we build with) and [GPUI](https://www.gpui.rs/).

```bash
git clone https://github.com/vicanso/zedis
cd zedis
make dev      # run in development (bacon run)
make debug    # run with RUST_LOG=DEBUG
make release  # optimized release build (--features mimalloc)
```

### Before opening a PR
Run these locally and make sure they pass — they are the same gates CI enforces:

```bash
make fmt      # cargo fmt
make lint     # typos + clippy --all-targets --all -- --deny=warnings
cargo test
```

A few project-specific rules:
- **No `.unwrap()`** — Clippy runs with `unwrap_used = "deny"` **including tests**. Use `.expect("…")` or proper matching.
- **i18n parity** — UI strings live in `locales/*.toml`. All 8 locales must have the **exact same key set**, or `build.rs` fails the build. Adding or removing a UI string means editing all 8 files; translate natively where the surrounding section already is.
- **README parity** — keep `README.md` / `README_zh.md` (and the `docs/FEATURES.md` / `docs/FEATURES_zh.md` pair) in sync when features change.
- **Components** — prefer `gpui-component`'s built-in components first; fall back to the shared widgets in `crates/zedis-ui` only when none fit.
- **Connection-layer changes** — anything that depends on a real server (ACL, versions, modules, cluster / sentinel / TLS) has live tests in `crates/zedis-connection/tests/live.rs`: `make it-up && make it && make it-down` (needs `redis-server` on PATH, or `REDIS_IMAGE=redis:7.2 make it-up` with docker). CI runs them against Redis 6.2 / 7.2 / 8.0, Valkey and redis-stack.

### Submitting a Pull Request
1. Fork the repo and create a branch off `main`.
2. Make your change; ensure `make fmt`, `make lint`, and `cargo test` all pass.
3. Open a PR using the template and complete the checklist.
4. By submitting a PR, you agree to the [Contributor License Agreement](CLA.md) — confirming your contribution is original and licensed under the project's open-source terms.

---

## 🇨🇳 中文

感谢你有兴趣改进 Zedis！🦀 本指南介绍如何搭建开发环境以及我们对贡献的期望。

### 贡献方式
- 🐛 **报告 Bug** —— 使用 Bug Report issue 模板。
- ✨ **提议新特性** —— **请先**提交 Feature Request issue,以便在你写代码前对齐范围。
- 🌍 **完善翻译** —— 内置 8 种语言(`en zh de es fr ja pt ru`)。
- 📝 **完善文档** —— README、`docs/FEATURES*.md` 及代码内文档。

### 开始之前
- **新特性:** 请先提 issue 讨论 —— 既省你我的时间,也确保改动契合项目规划。
- **拼写 / 格式:** 请**不要**仅为修复文档/注释中个别拼写或格式问题而单独提 PR;这类问题我们会集中处理或在较大改动中顺带修复。

### 开发环境
Zedis 使用 Rust(edition 2024;我们以 **Rust 1.95.0** 构建)与 [GPUI](https://www.gpui.rs/) 开发。

```bash
git clone https://github.com/vicanso/zedis
cd zedis
make dev      # 开发模式运行(bacon run)
make debug    # 带 RUST_LOG=DEBUG 运行
make release  # 优化的发布构建(--features mimalloc)
```

### 提 PR 之前
请在本地运行以下命令并确保通过 —— 与 CI 的门禁一致:

```bash
make fmt      # cargo fmt
make lint     # typos + clippy --all-targets --all -- --deny=warnings
cargo test
```

几条项目专属规则:
- **禁用 `.unwrap()`** —— Clippy 以 `unwrap_used = "deny"` 运行,**包括测试**。请用 `.expect("…")` 或正确的匹配处理。
- **i18n 一致性** —— UI 文案位于 `locales/*.toml`。8 种语言必须拥有**完全相同的 key 集合**,否则 `build.rs` 会编译失败。新增/删除一条 UI 文案需同时改 8 个文件;所在区段已翻译的请原生翻译。
- **README 一致性** —— 功能变动时,保持 `README.md` / `README_zh.md`(以及 `docs/FEATURES.md` / `docs/FEATURES_zh.md`)同步。
- **组件选用** —— 优先使用 `gpui-component` 的内置组件;仅当没有合适组件时,才用 `crates/zedis-ui` 里的共享控件。
- **连接层改动** —— 凡依赖真实服务端的行为(ACL、版本、模块、cluster / sentinel / TLS)都有实机测试 `crates/zedis-connection/tests/live.rs`:`make it-up && make it && make it-down`(需要 PATH 里有 `redis-server`,或用 docker:`REDIS_IMAGE=redis:7.2 make it-up`)。CI 会对 Redis 6.2 / 7.2 / 8.0、Valkey 和 redis-stack 各跑一遍。

### 提交 Pull Request
1. Fork 仓库,从 `main` 切出分支。
2. 完成改动;确保 `make fmt`、`make lint`、`cargo test` 全部通过。
3. 使用模板提 PR 并完成自查表。
4. 提交 PR 即表示你同意 [贡献者许可协议(CLA)](CLA.md) —— 确认你的贡献为原创,并授权在项目开源协议下使用。
