#!/bin/bash
set -euo pipefail

# The web deployment package (ADR 9): `zedis-bridge` built in release form
# with the release web bundle compiled into it, in one tarball. Called from
# `make web-dist`.
#
#   <cargo target dir>/web-dist/zedis-web-<version>-<host triple>.tar.gz
#   (+ .sha256; the script prints the path)
#
# A deployment is the one binary: the page is inside it and served from the
# same origin as the API — the login cookie is `SameSite=Strict`, so the page
# is not a thing to drop on another static host anyway.
#
# The bundle is rebuilt here in its release form before the bridge is built,
# because the bridge embeds whatever `zedis-web/www/` holds at compile time:
# after a `make web-bundle` that would be the 47 MB iteration build with its
# name section. The binary is the host's own: a tarball built on a Mac
# deploys to a Mac. Build on the target platform.

cd "$(dirname "$0")/.."

VERSION=$(sed -n 's/^version = "\([^"]*\)"/\1/p' Cargo.toml | head -1)
TARGET=$(rustc -vV | sed -n 's/^host: //p')
NAME="zedis-web-${VERSION}-${TARGET}"
# Asked of cargo, never spelled `target/`: `build.target-dir` in a user's
# `~/.cargo/config.toml` moves it, and a script that assumed the default
# would find no binary — or, worse, stage into a directory cargo never
# writes and package a stale one.
TARGET_DIR=$(cargo metadata --format-version 1 --no-deps 2>/dev/null \
  | python3 -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])')
OUT_DIR="$TARGET_DIR/web-dist"
STAGE="$OUT_DIR/$NAME"

echo "==> web bundle (release)"
scripts/web-bundle.sh --release

echo "==> zedis-bridge (release)"
cargo build --release -p zedis-bridge
BIN="$TARGET_DIR/release/zedis-bridge"
if [ -f "$BIN.exe" ]; then
  BIN="$BIN.exe"
fi

echo "==> $NAME"
rm -rf "$STAGE"
mkdir -p "$STAGE"
cp "$BIN" "$STAGE/"
cp LICENSE "$STAGE/"

cat > "$STAGE/README.txt" <<EOF
Zedis web build $VERSION ($TARGET)

  zedis-bridge   the whole deployment: it dials Redis, holds the server list
                 and its credentials, and serves the page (compiled into the
                 binary) and the API from one origin. --static <dir> serves a
                 directory as the page instead, for a rebuilt bundle.

Run:

  ZEDIS_BRIDGE_USERS="alice@secret,bob@hunter2" ./zedis-bridge

then open http://127.0.0.1:7379/ and sign in. The accounts are required:
the bridge does not start without them. Entries are comma-separated and
split at the first @, so a name cannot contain @ (or :), and a password
cannot contain a comma. Scripts send HTTP Basic (curl -u alice:secret).

A server entry belongs to the account that added it and nobody else sees
it, unless its "Shared" box is ticked, which makes it everyone's. Entries
with no owner — a list written before there were owners — are shared.
There are no roles: any account may edit or delete a shared entry.
Passwords are guessable: put a rate limit in front if the bridge is
reachable beyond your own network.

--listen 0.0.0.0:7379 binds beyond loopback; do that only behind TLS. The
login cookie is Secure by default, so over plain http the login visibly
does not stick — --insecure-cookie lifts that for a local plain-http run
and nothing else.

The server list is redis-servers.toml in the config directory. Its secrets
are encrypted with the master.key file beside it, never the OS keychain.
RUST_ENV=dev keeps everything under <config directory>/dev instead.
RUST_LOG=debug for more than the default info logging.
EOF

TARBALL="$OUT_DIR/$NAME.tar.gz"
tar -czf "$TARBALL" -C "$OUT_DIR" "$NAME"
if command -v sha256sum >/dev/null 2>&1; then
  (cd "$OUT_DIR" && sha256sum "$NAME.tar.gz" > "$NAME.tar.gz.sha256")
else
  (cd "$OUT_DIR" && shasum -a 256 "$NAME.tar.gz" > "$NAME.tar.gz.sha256")
fi

mib() { awk -v b="$1" 'BEGIN { printf "%.1f MiB", b / 1048576 }'; }
echo "bridge:  $(mib "$(wc -c < "$BIN" | tr -d ' ')")  (page included; the module alone is $(mib "$(wc -c < zedis-web/www/wasm/zedis_web_bg.wasm | tr -d ' ')"))"
echo "tarball: $(mib "$(wc -c < "$TARBALL" | tr -d ' ')")  $TARBALL"
cat "$TARBALL.sha256"
