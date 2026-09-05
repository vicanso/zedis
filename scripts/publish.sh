#!/bin/bash
set -euo pipefail

# Publish the workspace to crates.io: the four library crates under crates/
# (zedis-core, zedis-connection, zedis-db, zedis-ui) and the app itself
# (zedis-gui, the `zedis` binary). Called from `make publish` and
# `make publish-check`.
#
#   scripts/publish.sh            upload every crate at the workspace version
#   scripts/publish.sh --dry-run  package + verify (build) each crate, upload nothing
#
# `cargo publish --workspace` (cargo 1.90+) works out the dependency order —
# zedis-core first, the app last — and waits for each upload to reach the
# index before the crate that depends on it is verified, so one command
# covers the whole set. Everything that is already on crates.io at this
# version is passed as `--exclude`, so a run that stopped half-way (crates.io
# rate limit, network) is simply repeated. zedis-cmd-builder carries
# `publish = false` and is never picked up.
#
# A real upload only runs from a clean tree checked out at the release tag
# (`make version` creates it) with a crates.io token — `cargo login`, or
# CARGO_REGISTRY_TOKEN in the environment. The dry run has no such gate and
# tolerates a dirty tree, so it can be run on a branch before the release.

cd "$(dirname "$0")/.."

DRY_RUN=0
for arg in "$@"; do
  case "$arg" in
    --dry-run) DRY_RUN=1 ;;
    *) echo "usage: scripts/publish.sh [--dry-run]" >&2; exit 2 ;;
  esac
done

VERSION=$(sed -n 's/^version = "\([^"]*\)"/\1/p' Cargo.toml | head -1)
[ -n "$VERSION" ] || { echo "failed to read the workspace version from Cargo.toml" >&2; exit 1; }

# The crates.io side of the workspace, in dependency order (informational —
# cargo orders the upload itself).
CRATES=(zedis-core zedis-connection zedis-db zedis-ui zedis-gui)

# `cargo publish --workspace` and its index wait landed in cargo 1.90.
NEED_CARGO=1.90.0
HAVE_CARGO=$(cargo --version | awk '{print $2}')
if [ "$(printf '%s\n' "$NEED_CARGO" "$HAVE_CARGO" | sort -V | head -1)" != "$NEED_CARGO" ]; then
  echo "cargo >= $NEED_CARGO is needed for a workspace publish (have $HAVE_CARGO)" >&2
  exit 1
fi

if [ "$DRY_RUN" = 0 ]; then
  [ -z "$(git status --porcelain)" ] || { echo "the working tree is not clean — commit or stash first" >&2; exit 1; }
  TAG="v$VERSION"
  git rev-parse -q --verify "refs/tags/$TAG^{commit}" >/dev/null \
    || { echo "tag $TAG does not exist — cut the release with \`make version\` first" >&2; exit 1; }
  [ "$(git rev-parse HEAD)" = "$(git rev-parse "$TAG^{commit}")" ] \
    || { echo "HEAD is not at $TAG — check out the release tag before publishing" >&2; exit 1; }
  if [ -z "${CARGO_REGISTRY_TOKEN:-}" ] && ! grep -qs '^token' "${CARGO_HOME:-$HOME/.cargo}/credentials.toml"; then
    echo "no crates.io token — run \`cargo login\` or export CARGO_REGISTRY_TOKEN" >&2
    exit 1
  fi
fi

# Whether crates.io already has <crate>@<version>, read from the sparse
# index (no cargo involvement, no auth). A crate that does not exist yet is
# a 404 and counts as "not published"; so does a network failure, in which
# case cargo reports the duplicate itself.
index_path() {
  local name=$1 len=${#1}
  case "$len" in
    1) echo "1/$name" ;;
    2) echo "2/$name" ;;
    3) echo "3/${name:0:1}/$name" ;;
    *) echo "${name:0:2}/${name:2:2}/$name" ;;
  esac
}
published() {
  curl -fsS --max-time 15 "https://index.crates.io/$(index_path "$1")" 2>/dev/null \
    | grep -q "\"vers\":\"$2\""
}

EXCLUDE=()
for crate in "${CRATES[@]}"; do
  if published "$crate" "$VERSION"; then
    echo "$crate $VERSION is already on crates.io — skipping"
    EXCLUDE+=(--exclude "$crate")
  fi
done
if [ "${#EXCLUDE[@]}" = $((2 * ${#CRATES[@]})) ]; then
  echo "every crate is already published at $VERSION"
  exit 0
fi

ARGS=(publish --workspace --locked)
if [ "$DRY_RUN" = 1 ]; then
  ARGS+=(--dry-run --allow-dirty)
fi
ARGS+=(${EXCLUDE[@]+"${EXCLUDE[@]}"})

echo "publishing $VERSION: ${CRATES[*]}"
cargo "${ARGS[@]}"

if [ "$DRY_RUN" = 0 ]; then
  echo
  echo "published $VERSION:"
  for crate in "${CRATES[@]}"; do
    echo "  https://crates.io/crates/$crate"
  done
fi
