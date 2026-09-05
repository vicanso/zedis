lint:
	typos
	cargo clippy --all-targets --all -- --deny=warnings

# Dependency gate (advisories / licenses / bans / sources); the config is
# deny.toml. `cargo install cargo-deny --locked` once.
deny:
	cargo deny check advisories bans licenses sources

fmt:
	cargo fmt

test:
	cargo test --workspace

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

# Live integration tests against real servers (crates/zedis-connection/tests/live.rs).
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
