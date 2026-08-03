#!/bin/bash
set -e

# Keep secondary release metadata in step with the workspace version when
# cutting a release. Called from `make version` (which derives the tag from
# Cargo.toml's [workspace.package] version) — safe to re-run, each update
# is skipped once it is already in place.
#
#   - flatpak/io.github.vicanso.zedis.metainfo.xml — prepend the
#     <release> entry Flathub / software centers show for this version.
#
# Deliberately NOT handled here: the flatpak manifest's tag/commit pin and
# cargo-sources.json. The commit hash only exists once the tag is created,
# so scripts/submit-flathub.sh owns those — run it after tagging.

TAG=${1:?usage: sync-release-meta.sh vX.Y.Z}
VERSION=${TAG#v}
DATE=$(date +%Y-%m-%d)
METAINFO=flatpak/io.github.vicanso.zedis.metainfo.xml

cd "$(dirname "$0")/.."

if grep -q "release version=\"$VERSION\"" "$METAINFO"; then
  echo "metainfo: <release $VERSION> already present, skipping"
else
  perl -pi -e "s|^(\s*)<releases>|\$1<releases>\n\$1  <release version=\"$VERSION\" date=\"$DATE\">\n\$1    <url>https://github.com/vicanso/zedis/releases/tag/$TAG</url>\n\$1  </release>|" "$METAINFO"
  grep -q "release version=\"$VERSION\"" "$METAINFO" || {
    echo "failed to add <release $VERSION> to $METAINFO" >&2
    exit 1
  }
  echo "metainfo: added <release $VERSION $DATE>"
fi
