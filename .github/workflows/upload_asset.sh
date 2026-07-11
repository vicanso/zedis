#!/bin/bash
set -euo pipefail

# Usage: upload_asset.sh <FILE> [TOKEN]
#
# Uploads FILE to the release of the current tag via the gh CLI. The release
# is normally pre-created by prepare_vars (publish.yml); the create here is a
# fallback for manual/partial runs. `--clobber` replaces an already-uploaded
# asset of the same name, so re-running a job doesn't fail with 422.
if [ $# -lt 1 ]; then
    echo "Usage: upload_asset.sh <FILE> [TOKEN]"
    exit 1
fi

repo="vicanso/zedis"
file_path=$1

# gh reads GH_TOKEN / GITHUB_TOKEN from the environment; the second argument
# is kept for backward compatibility with existing callers.
if [ -n "${2:-}" ]; then
    export GH_TOKEN=$2
fi

tag="$(git describe --tags --abbrev=0)"
if [ -z "$tag" ]; then
    printf "\e[31mError: Unable to find git tag\e[0m\n"
    exit 1
fi
echo "Uploading $file_path to $repo@$tag"

# Fallback only: `gh release list` includes drafts, unlike a lookup via
# /releases/tags/:tag, so a pre-created draft is correctly detected.
if ! gh release list -R "$repo" --json tagName -q '.[].tagName' | grep -qx "$tag"; then
    echo "No release for $tag; creating draft..."
    gh release create "$tag" -R "$repo" --draft --title "$tag" --notes "" || true
fi

gh release upload "$tag" "$file_path" -R "$repo" --clobber

printf "\e[32mSuccess\e[0m\n"
