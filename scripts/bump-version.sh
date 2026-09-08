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

# The in-tree crates are path dependencies that also carry a `version` (what
# the published manifests keep) — every one of them moves with the workspace.
CURRENT="$CURRENT" NEXT="$NEXT" perl -pi -e \
  's|^(zedis-[a-z]+ = \{ path = "crates/zedis-[a-z]+", version = ")\Q$ENV{CURRENT}\E"|$1$ENV{NEXT}"|' Cargo.toml
[ "$(grep -c "^zedis-[a-z]* = { path = \"crates/zedis-[a-z]*\", version = \"$NEXT\" }" Cargo.toml)" = 4 ] \
  || { echo "failed to bump the in-tree dependency versions in Cargo.toml" >&2; exit 1; }

cargo update --workspace --offline --quiet

# Landing pages pin the version in several places (nav pill, CTAs, JSON-LD,
# footer). docs/README.md lists this as a release step; do it here so a
# bump cannot leave zedis.net on the previous tag.
for f in docs/index.html docs/zh/index.html; do
  CURRENT="$CURRENT" NEXT="$NEXT" perl -pi -e \
    's/\Q$ENV{CURRENT}\E/$ENV{NEXT}/g' "$f"
  grep -q "$NEXT" "$f" || { echo "failed to bump $f" >&2; exit 1; }
done

# SECURITY.md tracks the latest *line* (0.8.x), not the patch. Refresh it
# when the minor/major moves so "supported versions" cannot lag a release.
OLD_LINE="${CURRENT%.*}"
NEW_LINE="${NEXT%.*}"
if [ "$OLD_LINE" != "$NEW_LINE" ]; then
  OLD_LINE="$OLD_LINE" NEW_LINE="$NEW_LINE" perl -pi -e \
    's/\Q$ENV{OLD_LINE}\E/$ENV{NEW_LINE}/g' SECURITY.md
  grep -q "$NEW_LINE.x" SECURITY.md || { echo "failed to bump SECURITY.md" >&2; exit 1; }
fi

echo "version: $CURRENT -> $NEXT"
