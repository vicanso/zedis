# syntax=docker/dockerfile:1
#
# The web build of Zedis as one image (ADR 9): `zedis-bridge` with the browser
# bundle compiled into it. Not the desktop app — that one needs a window.
#
#   docker run -p 7379:7379 -v zedis-data:/data \
#     -e ZEDIS_BRIDGE_USERS="alice@secret,bob@hunter2" vicanso/zedis-web
#
# Self-contained on purpose: `docker build .` on a fresh clone produces the
# image, with no bundle built beforehand. The price is that each architecture
# compiles the (architecture-independent) wasm for itself.

FROM rust:1-bookworm AS builder

# Pinned, like every tool the release workflow installs (`--locked` + version):
# an unpinned install resolves fresh and lets a bad upstream release break ours.
ARG WASM_PACK_VERSION=0.15.0
# From GitHub, not apt: Debian's binaryen is years older than the wasm a
# current rustc emits, and `wasm-opt` refuses what it cannot parse.
ARG BINARYEN_VERSION=version_132

# clang/libclang: gpui's build script runs bindgen, on every target.
# pkg-config/libssl: vergen-git2 → libgit2, a build dependency of the app crate.
# python3/brotli: scripts/web-bundle.sh.
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        clang libclang-dev pkg-config libssl-dev python3 brotli curl ca-certificates \
    && rm -rf /var/lib/apt/lists/*

RUN arch="$(uname -m)" \
    && curl -fsSL "https://github.com/WebAssembly/binaryen/releases/download/${BINARYEN_VERSION}/binaryen-${BINARYEN_VERSION}-${arch}-linux.tar.gz" \
        | tar -xz -C /opt \
    && ln -s "/opt/binaryen-${BINARYEN_VERSION}/bin/wasm-opt" /usr/local/bin/wasm-opt \
    && wasm-opt --version

# A download that stalls is retried rather than fatal, and HTTP/2
# multiplexing is off: behind a flaky NAT it is what turns one slow stream
# into "transferred 0 bytes in 30s" for all of them. Harmless on a good line.
ENV CARGO_NET_RETRY=10 \
    CARGO_HTTP_TIMEOUT=120 \
    CARGO_HTTP_MULTIPLEXING=false

# Optional, for a network where crates.io and static.rust-lang.org crawl:
#   --build-arg CARGO_MIRROR=sparse+https://rsproxy.cn/index/
#   --build-arg RUSTUP_DIST_SERVER=https://mirrors.ustc.edu.cn/rust-static
# Unset — which is what the release workflow builds with — nothing changes.
# A mirror replaces where crates are fetched from, not what `Cargo.lock` pins,
# so `--locked` still means the same versions and checksums.
ARG CARGO_MIRROR=""
RUN if [ -n "$CARGO_MIRROR" ]; then \
        printf '[source.crates-io]\nreplace-with = "mirror"\n\n[source.mirror]\nregistry = "%s"\n' "$CARGO_MIRROR" \
            > "$CARGO_HOME/config.toml"; \
    fi

RUN cargo install --locked "wasm-pack@${WASM_PACK_VERSION}"

WORKDIR /src

# The two toolchains, before the sources, so that a source change does not
# download them again: the pinned stable for the bridge, and the nightly the
# browser backend needs, which `zedis-web/` selects for itself.
COPY rust-toolchain.toml ./
COPY zedis-web/rust-toolchain.toml zedis-web/
# Declared here and not with `CARGO_MIRROR` above: an ARG is part of the cache
# key of every step after it, and this one has no business invalidating the
# wasm-pack install. Exported only when given: an empty value is not "the
# default".
ARG RUSTUP_DIST_SERVER=""
RUN if [ -n "$RUSTUP_DIST_SERVER" ]; then export RUSTUP_DIST_SERVER; else unset RUSTUP_DIST_SERVER; fi \
    && rustup toolchain install "$(sed -n 's/^channel = "\(.*\)"/\1/p' rust-toolchain.toml)" \
        --profile minimal --no-self-update \
    && rustup toolchain install "$(sed -n 's/^channel = "\(.*\)"/\1/p' zedis-web/rust-toolchain.toml)" \
        --profile minimal --no-self-update --target wasm32-unknown-unknown

COPY . .

# The bundle first, in its release form, because the bridge embeds whatever
# `zedis-web/www/` holds when it is compiled — and its `build.rs` refuses a
# release build that would embed nothing. `cargo metadata` names the target
# directory rather than assuming `target/`.
RUN scripts/web-bundle.sh --release \
    && cargo build --release --locked -p zedis-bridge \
    && cp "$(cargo metadata --format-version 1 --no-deps \
        | python3 -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])')/release/zedis-bridge" /zedis-bridge \
    && mkdir /data

# glibc and libgcc and nothing else: no shell, no package manager, not root.
# TLS to a Redis server verifies against the roots compiled into the binary
# (webpki-roots), so the image needs no certificate store of its own.
FROM gcr.io/distroless/cc-debian12:nonroot

COPY --from=builder /zedis-bridge /usr/local/bin/zedis-bridge
# The server list, its key file and the saved logins. Owned by the image's
# user, because a fresh named volume takes its ownership from this directory.
COPY --from=builder --chown=nonroot:nonroot /data /data

ENV ZEDIS_CONFIG_DIR=/data
VOLUME ["/data"]
EXPOSE 7379

# Every interface, because inside a container loopback reaches nobody; what is
# published, and to whom, is `docker run -p`'s decision. `ZEDIS_BRIDGE_USERS`
# is required — the bridge does not start without accounts. Behind plain http
# add `--insecure-cookie`, or the login cookie (Secure by default) is dropped.
# `ZEDIS_BRIDGE_BASE_PATH=/zedis` mounts everything under that path, for a host
# name shared with other applications — an environment variable because
# arguments to `docker run` replace this whole CMD.
ENTRYPOINT ["/usr/local/bin/zedis-bridge"]
CMD ["--listen", "0.0.0.0:7379"]
