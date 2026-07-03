#!/bin/bash
set -e

# Generate flatpak/cargo-sources.json — the offline crate mirror Flathub
# builds require (their builders have no network access).
#
# Usage:
#   ./scripts/gen-flatpak-sources.sh           # from the working-tree Cargo.lock
#   ./scripts/gen-flatpak-sources.sh v0.4.7    # from that tag's Cargo.lock

GENERATOR_URL="https://raw.githubusercontent.com/flatpak/flatpak-builder-tools/master/cargo/flatpak-cargo-generator.py"
OUTPUT_FILE="flatpak/cargo-sources.json"
TAG="${1:-}"

cd "$(dirname "$0")/.."

WORKDIR=$(mktemp -d)
trap 'rm -rf "$WORKDIR"' EXIT

LOCKFILE=Cargo.lock
if [ -n "$TAG" ]; then
  # Use the lockfile exactly as it was at the release tag, so the generated
  # sources match what the manifest's git source will build.
  LOCKFILE="$WORKDIR/Cargo.lock"
  git show "$TAG:Cargo.lock" > "$LOCKFILE"
  echo "Using Cargo.lock from $TAG"
fi

echo "Downloading generator..."
curl -sfL "$GENERATOR_URL" -o "$WORKDIR/flatpak-cargo-generator.py"

echo "Generating cargo sources (this walks every crate in the lockfile)..."
python3 -m venv "$WORKDIR/venv"
source "$WORKDIR/venv/bin/activate"
pip install --quiet aiohttp toml tomlkit
python3 "$WORKDIR/flatpak-cargo-generator.py" "$LOCKFILE" -o "$OUTPUT_FILE"
deactivate

echo "Done! Generated $OUTPUT_FILE"
