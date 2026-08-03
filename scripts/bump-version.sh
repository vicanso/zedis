#!/bin/bash
set -e

# Bump the workspace version in Cargo.toml — the single source every build
# reads (crates, MSI, AppImage, deb/rpm, `make version`). Called from the
# `make version-{patch,minor,major}` targets.
#
# Also refreshes the workspace members' entries in Cargo.lock (offline) so
# the lockfile committed with the bump matches the new version — the flatpak
# offline build regenerates its sources from the tag's lockfile and a stale
# one would fail `cargo --offline build`.

KIND=${1:?usage: bump-version.sh <patch|minor|major>}
cd "$(dirname "$0")/.."

CURRENT=$(sed -n 's/^version = "\([^"]*\)"/\1/p' Cargo.toml | head -1)
[ -n "$CURRENT" ] || { echo "failed to read version from Cargo.toml" >&2; exit 1; }
IFS=. read -r MAJ MIN PAT <<<"$CURRENT"

case "$KIND" in
  patch) PAT=$((PAT + 1)) ;;
  minor) MIN=$((MIN + 1)); PAT=0 ;;
  major) MAJ=$((MAJ + 1)); MIN=0; PAT=0 ;;
  *) echo "unknown bump kind: $KIND (want patch|minor|major)" >&2; exit 1 ;;
esac
NEXT="$MAJ.$MIN.$PAT"

# Replace only the first `version = "…"` line — that is [workspace.package]
# (the root [package] uses `version.workspace = true`, which doesn't match).
CURRENT="$CURRENT" NEXT="$NEXT" perl -pi -e \
  's|^version = "\Q$ENV{CURRENT}\E"|version = "$ENV{NEXT}"| && ++$done unless $done' Cargo.toml
grep -q "^version = \"$NEXT\"" Cargo.toml || { echo "failed to bump Cargo.toml" >&2; exit 1; }

cargo update --workspace --offline --quiet

echo "version: $CURRENT -> $NEXT"
