#!/bin/bash
set -e

# Create or update the winget (microsoft/winget-pkgs) manifest for Zedis via
# komac — cross-platform, so the PR can be opened straight from macOS/Linux.
#
# Usage:
#   ./scripts/submit-winget.sh 0.4.7 new      # first-time submission (interactive)
#   ./scripts/submit-winget.sh 0.4.8          # version update (non-interactive)
#
# Requirements:
#   brew install komac
#   a GitHub token with public_repo scope: `komac token update` or GITHUB_TOKEN
#
# `new` prompts for the one-time package metadata (publisher, license, ...)
# and asks before submitting; `update` reuses the published manifest and opens
# the PR directly. After the first release lands, consider wiring
# https://github.com/vedantmgoyal9/winget-releaser into publish.yml instead.

VERSION=${1:?usage: submit-winget.sh <version> [new|update]}
VERSION=${VERSION#v}
MODE=${2:-update}
ID=vicanso.Zedis
BASE="https://github.com/vicanso/zedis/releases/download/v$VERSION"
URLS=("$BASE/zedis-windows-x86_64.msi" "$BASE/zedis-windows-aarch64.msi")

command -v komac >/dev/null || { echo "komac not found — brew install komac" >&2; exit 1; }

for u in "${URLS[@]}"; do
  curl -sfIL -o /dev/null "$u" || { echo "release asset missing: $u" >&2; exit 1; }
done

case $MODE in
  new)
    komac new "$ID" --version "$VERSION" --urls "${URLS[@]}"
    ;;
  update)
    komac update "$ID" --version "$VERSION" --urls "${URLS[@]}" --submit
    ;;
  *)
    echo "unknown mode: $MODE (expected new|update)" >&2
    exit 1
    ;;
esac
