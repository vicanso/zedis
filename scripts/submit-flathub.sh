#!/bin/bash
set -e

# Prepare — and optionally submit — the Flathub PR for a release tag.
#
# Usage:
#   ./scripts/submit-flathub.sh v0.4.7            # pin manifest + generate sources
#   ./scripts/submit-flathub.sh v0.4.7 --submit   # ... and open the flathub/flathub PR
#
# The tag must already be pushed and must contain the flatpak/ directory.
# --submit needs an authenticated `gh` and an SSH key for github.com.
#
# First-time submission goes to flathub/flathub (new-pr branch). Once the app
# is accepted, releases are updated in the dedicated flathub/io.github.vicanso.zedis
# repo instead — push the same two files there and skip this script's PR step.

TAG=${1:?usage: submit-flathub.sh <tag> [--submit]}
SUBMIT=${2:-}
APP_ID=io.github.vicanso.zedis

cd "$(dirname "$0")/.."
REPO_ROOT=$(pwd)
MANIFEST=flatpak/$APP_ID.yml

git rev-parse -q --verify "refs/tags/$TAG" >/dev/null || {
  echo "tag $TAG not found locally — git fetch --tags first" >&2
  exit 1
}
COMMIT=$(git rev-parse "$TAG^{}")

# 1. Pin the manifest's git source to the tag (fills the TODO placeholder).
perl -pi -e "s|^(\s*tag:).*|\$1 $TAG|; s|^(\s*commit:).*|\$1 $COMMIT|" "$MANIFEST"
grep -q "commit: $COMMIT" "$MANIFEST" || { echo "failed to pin $MANIFEST" >&2; exit 1; }
echo "Pinned $MANIFEST to $TAG ($COMMIT)"

# 2. Offline crate mirror from that tag's lockfile.
./scripts/gen-flatpak-sources.sh "$TAG"

# 3. Assemble the submission files.
OUT=target/flathub-submission
rm -rf "$OUT" && mkdir -p "$OUT"
cp "$MANIFEST" flatpak/cargo-sources.json "$OUT/"
echo "Submission files ready in $OUT/"

if [ "$SUBMIT" != "--submit" ]; then
  cat <<EOF

Dry run only. Review the files, commit the manifest/cargo-sources changes,
then rerun with --submit to fork flathub/flathub and open the PR.
EOF
  exit 0
fi

# 4. Fork flathub/flathub and open the PR against its new-pr branch.
command -v gh >/dev/null || { echo "gh CLI required for --submit" >&2; exit 1; }
LOGIN=$(gh api user -q .login)
BRANCH="add-$APP_ID"

gh repo fork flathub/flathub --clone=false >/dev/null 2>&1 || true

WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
git clone --quiet --depth=1 --branch new-pr https://github.com/flathub/flathub "$WORK/flathub"
cd "$WORK/flathub"
git checkout -q -b "$BRANCH"
cp "$REPO_ROOT/$OUT"/* .
git add "$APP_ID.yml" cargo-sources.json
git commit -q -m "Add $APP_ID"
git push -f "git@github.com:$LOGIN/flathub.git" "$BRANCH"
gh pr create --repo flathub/flathub --base new-pr --head "$LOGIN:$BRANCH" \
  --title "Add $APP_ID" \
  --body "Zedis — a native, GPU-accelerated Redis GUI client built in Rust with GPUI.

Upstream: https://github.com/vicanso/zedis (Apache-2.0)
Pinned to $TAG ($COMMIT). I am the upstream author."
