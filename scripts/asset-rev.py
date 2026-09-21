#!/usr/bin/env python3
"""The `ASSET_REV` map in `zedis-web/www/index.html`, and the three things
anyone does to it.

The page needs content hashes to cache-bust its own assets, and they are
inlined rather than fetched so the page does not pay a round trip for an
`asset-rev.json` before it can ask for anything else. That makes a *built*
`index.html` differ from the authored one — by a map that includes the wasm
module's hash, so it changes on every build of every branch.

Committing that map would be noise with no reader: every release path
(`web-dist.sh`, the Dockerfile) regenerates it, so the stored value is never
the one served. Hence the git clean filter: the worktree keeps the built map,
git stores the empty placeholder. `.gitattributes` names the driver and
`make install-git-filters` configures it — a filter cannot travel in the
repository, which is why `check` exists as well.

Three modes, one owner of the marker:

    write   compute the hashes and inline them (used by scripts/web-bundle.sh)
    clean   stdin -> stdout with the map emptied (the git clean filter)
    check   fail if what git has staged carries a non-empty map
"""

import hashlib
import json
import re
import subprocess
import sys
from pathlib import Path

HTML = Path("zedis-web/www/index.html")
WWW = HTML.parent

# The one spelling of the marker. `web-bundle.sh` used to carry its own copy;
# a second one is how the filter and the builder would come to disagree about
# what they are both editing.
MARKER = re.compile(r"const ASSET_REV = /\*ASSET_REV\*/.*?/\*/ASSET_REV\*/", re.S)
EMPTY = "const ASSET_REV = /*ASSET_REV*/{}/*/ASSET_REV*/"

# Skipped: the compressed twins (the page asks for the original and the
# bridge negotiates), TypeScript declarations that no browser fetches, and
# the kit's icons, which it fetches by name and which stay on ETag.
SKIP_SUFFIX = {".gz", ".br", ".d.ts"}
SKIP_NAME = {".gitignore", "package.json", "asset-rev.json", "index.html"}
SKIP_PREFIX = ("assets/icons/",)


def replace(html: str, payload: str) -> str:
    """`html` with the map set to `payload`, or unchanged if the marker is
    absent — a filter that mangled a file it did not recognise would be worse
    than one that passed it through."""
    updated, n = MARKER.subn(f"const ASSET_REV = /*ASSET_REV*/{payload}/*/ASSET_REV*/", html, count=1)
    return updated if n == 1 else html


def revisions() -> dict[str, str]:
    revs = {}
    for path in sorted(WWW.rglob("*")):
        if not path.is_file() or path.suffix in SKIP_SUFFIX or path.name in SKIP_NAME:
            continue
        rel = path.relative_to(WWW).as_posix()
        if rel.startswith(SKIP_PREFIX):
            continue
        revs[rel] = hashlib.md5(path.read_bytes()).hexdigest()[:12]
    return revs


def write() -> int:
    html = HTML.read_text()
    if not MARKER.search(html):
        sys.exit(f"{HTML} is missing the const ASSET_REV placeholder")
    revs = revisions()
    payload = json.dumps(revs, separators=(",", ":"), sort_keys=True)
    HTML.write_text(replace(html, payload))
    (WWW / "asset-rev.json").unlink(missing_ok=True)
    print(f"asset-rev: {len(revs)} files inlined into index.html")
    return 0


def clean() -> int:
    """The git clean filter: worktree -> index, byte for byte but for the map.

    Reads and writes binary so a file with no trailing newline, or with CRLF,
    comes back as it went in.
    """
    raw = sys.stdin.buffer.read()
    try:
        text = raw.decode()
    except UnicodeDecodeError:
        sys.stdout.buffer.write(raw)
        return 0
    sys.stdout.buffer.write(replace(text, "{}").encode())
    return 0


def check() -> int:
    """Fail if the copy git would commit carries a built map.

    The filter is local configuration and cannot travel in the repository, so
    a fresh clone that never ran `make install-git-filters` silently goes back
    to storing the built map. This is what makes that loud instead.

    `:path` is the staged blob, which falls back to HEAD's when nothing is
    staged — in both cases exactly what a commit would record.
    """
    try:
        staged = subprocess.run(
            ["git", "show", f":{HTML.as_posix()}"],
            capture_output=True,
            text=True,
            check=True,
        ).stdout
    except (OSError, subprocess.CalledProcessError):
        # No git, no checkout, or the file is not tracked: nothing to say.
        # A packaged build has no repository and must not fail here.
        return 0
    match = MARKER.search(staged)
    if match is None or match.group(0) == EMPTY:
        return 0
    print(
        f"{HTML} is staged with a built ASSET_REV map, which no release ever reads.\n"
        "  Install the filter and re-stage it:\n"
        "      make install-git-filters\n"
        f"      git add {HTML.as_posix()}\n"
        "  Or drop the change:\n"
        f"      git restore --staged --worktree {HTML.as_posix()}",
        file=sys.stderr,
    )
    return 1


def main() -> int:
    modes = {"write": write, "clean": clean, "check": check}
    if len(sys.argv) != 2 or sys.argv[1] not in modes:
        sys.exit(f"usage: {sys.argv[0]} {{{'|'.join(modes)}}}")
    return modes[sys.argv[1]]()


if __name__ == "__main__":
    sys.exit(main())
