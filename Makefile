lint:
	typos
	cargo clippy --all-targets --all -- --deny=warnings

fmt:
	cargo fmt

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

version:
	git cliff --unreleased --tag v0.2.0 --prepend CHANGELOG.md