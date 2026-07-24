#!/bin/bash
set -euo pipefail

# Usage: upload_asset.sh <FILE> [TOKEN]
#
# Uploads FILE to the release of the current tag via the gh CLI. The release
# is normally pre-created by prepare_vars (publish.yml); the create here is a
# fallback for manual/partial runs. `--clobber` replaces an already-uploaded
# asset of the same name, so re-running a job doesn't fail with 422.
#
# Existence checks go through the Releases API (not `gh release list | grep`):
# list+grep has been observed to miss a published release on macOS runners,
# after which `gh release create --draft` leaves an empty *untagged* draft
# next to the real release (GitHub allows multiple drafts with the same
# tag_name when the tag is already bound to a published release).
if [ $# -lt 1 ]; then
    echo "Usage: upload_asset.sh <FILE> [TOKEN]"
    exit 1
fi

repo="vicanso/zedis"
file_path=$1

# gh reads GH_TOKEN / GITHUB_TOKEN from the environment; the second argument
# is kept for backward compatibility with existing callers.
if [ -n "${2:-}" ]; then
    export GH_TOKEN="$2"
fi

tag="$(git describe --tags --abbrev=0)"
if [ -z "$tag" ]; then
    printf "\e[31mError: Unable to find git tag\e[0m\n"
    exit 1
fi
echo "Uploading $file_path to $repo@$tag"

# True when any release (published or draft) claims this tag_name.
# `gh release view` only resolves *published* tags, so a pre-created draft
# would look missing and every platform job would race-create another.
release_ids="$(
    gh api --paginate "repos/${repo}/releases" \
        -q ".[] | select(.tag_name == \"${tag}\") | .id" 2>/dev/null || true
)"
if [ -z "${release_ids}" ]; then
    echo "No release for $tag; creating draft..."
    # Ignore failure if a peer job won the race or a published release
    # appeared between the check and create — upload below still targets
    # the tag, and a second draft would be worse than a failed create.
    gh release create "$tag" -R "$repo" --draft --title "$tag" --notes "" || true
else
    echo "Found release for $tag (id(s): $(echo "$release_ids" | tr '\n' ' '))"
fi

gh release upload "$tag" "$file_path" -R "$repo" --clobber

printf "\e[32mSuccess\e[0m\n"
