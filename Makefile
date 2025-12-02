lint:
	cargo clippy --all-targets --all -- --deny=warnings

fmt:
	cargo fmt

dev:
	bacon run

release:
	cargo build --release

bundle:
	cargo bundle --release 

udeps:
	cargo +nightly udeps

msrv:
	cargo msrv list

bloat:
	cargo bloat --release --crates --bin zedis