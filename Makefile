# The single gate. `cargo fmt --check` comes first because CI runs it first,
# and because leaving it out made "make lint passes" a claim that did not
# cover formatting: a test file landed unformatted, the gate stayed green,
# and the lint job went red on a diff the author never saw.
lint:
	cargo fmt --check
	typos
	cargo clippy --all-targets --all -- --deny=warnings

# The GPUI Fast build (the `gpui-fast` feature — every platform's `-fast` files):
# clippy for what sits behind the feature, which `make lint` never compiles,
# then the app crate's tests on that engine. Run it after touching anything
# gated on the feature, or the engine wiring in `lib.rs`.
lint-fast:
	cargo clippy --all-targets --features gpui-fast -- --deny=warnings
	ZEDIS_CONFIG_DIR="$$(mktemp -d)" cargo test --features gpui-fast

# The browser half of the build (ADR 9). `make lint` cannot see it: clippy
# there is native-only, so a native-only API leaking into shared code compiles
# clean locally and only fails when someone builds the page. Run from
# zedis-web/, whose rust-toolchain.toml selects the nightly GPUI's web backend
# needs; `rustup target add wasm32-unknown-unknown` once.
check-web:
	cd zedis-web && RUSTFLAGS="-D warnings" cargo check --target wasm32-unknown-unknown \
		-p zedis-core -p zedis-ui -p zedis-connection -p zedis-db -p zedis-gui -p zedis-web

# The browser bundle (ADR 9): the kit's icons copied beside the page and the
# wasm built by wasm-pack (`cargo install wasm-pack` once). `web-bundle` is
# the iteration build (`--profile web`: name section kept, no wasm-opt);
# `web-release` is the shipped form, `release`'s counterpart — `--profile
# web-release` (fat LTO, one codegen unit, stripped) then `wasm-opt -Oz`,
# which needs binaryen (`brew install binaryen` / `cargo install wasm-opt`).
# Both write the same `zedis-web/www/`, which the bridge compiles into itself
# in a release build and reads from disk in a debug one — so `web-serve`, a
# debug run, serves whichever bundle was built last with no rebuild of the
# bridge. It serves the page and the API from one origin; open
# http://127.0.0.1:7379/. The first time, the page asks to create the
# administrator, with a setup token this target makes `dev` unless
# `ZEDIS_BRIDGE_SETUP_TOKEN` is set — the token counts only while there is no
# administrator. Invite a second account from the account page to see that
# each one's private entries are its own.
#
# `RUST_ENV=dev`, the same as `bacon.toml` sets for `make dev`: the bridge
# then keeps everything under `<config_dir>/dev` — the server list it serves
# (and rewrites on save), its key file and its accounts — and never touches
# the installed app's list. A bridge without it *is* the production
# deployment.
web-bundle:
	scripts/web-bundle.sh

web-release:
	scripts/web-bundle.sh --release

# The deployment package: `zedis-bridge` in release form with the release web
# bundle compiled into it, as `zedis-web-<version>-<host>.tar.gz` under
# `web-dist/` in cargo's target directory (the script prints the path), with
# a `.sha256` beside it. It rebuilds the bundle in release form first, so a
# dev bundle sitting in `www/wasm/` is never what ships — and `build.rs`
# refuses a release bridge whose `www/wasm` is missing. The binary is the
# host's; build on the platform being deployed to.
web-dist:
	scripts/web-dist.sh

web-serve:
	RUST_ENV=dev ZEDIS_BRIDGE_SETUP_TOKEN="$${ZEDIS_BRIDGE_SETUP_TOKEN:-dev}" cargo run -p zedis-bridge -- --insecure-cookie

# Dependency gate (advisories / licenses / bans / sources); the config is
# deny.toml. `cargo install cargo-deny --locked` once.
deny:
	cargo deny check advisories bans licenses sources

fmt:
	cargo fmt

# The run gets a scratch config directory of its own. A test that touches
# configuration already points itself at one (`override_config_dir`, or
# `test_support::init_ui` in the app crate); this is for the one that forgets:
# it then reads an empty server list instead of the real `redis-servers.toml`,
# and never asks the OS keychain for the key that decrypts it — on macOS a
# password prompt per run, a test binary being unsigned and new on each build.
test:
	ZEDIS_CONFIG_DIR="$$(mktemp -d)" cargo test --workspace

# Criterion benches for the pure hot paths (crates/zedis-core/benches):
# fuzzy scan, RDB parse, JSONPath. No CI baseline — run before and after
# touching those paths and compare the reports; `make lint` keeps the
# bench targets compiling via clippy --all-targets.
bench:
	cargo bench -p zedis-core

# Locale hygiene on demand (tests/locale_keys.rs, also part of `make test`):
# key parity across the 8 locales — reliable even when build.rs's
# rerun-if-changed misses an in-place edit — plus the orphan-key scan
# (keys translated everywhere but referenced nowhere in the source).
check-locales:
	cargo test --test locale_keys

# What the GUI crate knows about Redis (tests/view_layering.rs): nothing, the
# baseline being empty — a failure here means the new code belongs in
# zedis-connection as a typed operation, not that the baseline wants rewriting.
check-layering:
	cargo test --test view_layering

# Live integration tests against real servers (crates/zedis-connection/tests/live/).
# `make it-up` starts the topology (local redis-server, or REDIS_IMAGE=redis:7.2 for docker),
# `make it` runs the ignored tests with its ZEDIS_IT_* env, `make it-down` stops it.
it-up:
	scripts/it/up.sh

it:
	set -a && . scripts/it/.env && set +a && cargo test -p zedis-connection --test live -- --ignored --test-threads=4

it-down:
	scripts/it/down.sh

# crates.io release of the whole workspace — crates/* (zedis-core,
# zedis-connection, zedis-db, zedis-ui) and the app (zedis-gui) — in
# dependency order via `cargo publish --workspace` (cargo 1.90+).
# `make publish-check` packages and build-verifies every crate without
# uploading (fine on a dirty branch); `make publish` needs a clean tree at
# the release tag and a crates.io token (`cargo login` / CARGO_REGISTRY_TOKEN),
# and skips crates already published at this version, so it can be re-run.
publish-check:
	scripts/publish.sh --dry-run

publish:
	scripts/publish.sh

# The rolling nightly, by hand: a workflow_dispatch of publish.yml on main
# (the scheduled one skips itself when main has not moved). It builds
# origin/main, not the working tree — scripts/nightly.sh says what is local
# only, refuses a second run while one is going, and asks before triggering.
# `make nightly` is everything (~30 min, macOS signing included);
# `make nightly-docker` rebuilds only vicanso/zedis-web:nightly and leaves
# the release alone. Extra flags: `make nightly ARGS="--watch --yes"`.
nightly:
	scripts/nightly.sh all $(ARGS)

nightly-docker:
	scripts/nightly.sh docker $(ARGS)

build-cmd:
	cargo run --package zedis-cmd-builder

dev:
	bacon run

debug:
	RUST_LOG=DEBUG make dev

release:
	cargo build --release --features mimalloc

bundle:
	cargo bundle --release  --features mimalloc

udeps:
	cargo +nightly udeps

msrv:
	cargo msrv list

bloat:
	cargo bloat --release --crates --bin zedis

# Release version — read from Cargo.toml's [workspace.package], the single
# source of truth every build derives from (crates, MSI, AppImage, deb/rpm).
VERSION := $(shell sed -n 's/^version = "\([^"]*\)"/\1/p' Cargo.toml | head -1)

# Prepend the changelog for the upcoming tag and sync secondary release
# metadata (flatpak metainfo <release> entry). Assumes Cargo.toml already
# holds the release version — use version-{patch,minor,major} to bump and
# sync in one step. The flatpak manifest's tag/commit pin +
# cargo-sources.json are post-tag work — run scripts/submit-flathub.sh
# after tagging.
version:
	git cliff --unreleased --tag v$(VERSION) --prepend CHANGELOG.md
	./scripts/sync-release-meta.sh v$(VERSION)

# Bump Cargo.toml (+ Cargo.lock) then run `version` in a fresh make
# invocation — VERSION is expanded at parse time, so the recursive $(MAKE)
# is what picks up the just-bumped number.
version-patch:
	./scripts/bump-version.sh patch
	$(MAKE) version

version-minor:
	./scripts/bump-version.sh minor
	$(MAKE) version

version-major:
	./scripts/bump-version.sh major
	$(MAKE) version
