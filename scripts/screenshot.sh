#!/bin/bash
# Launch Zedis on a given route, capture its window, and exit — for
# before/after visual comparison while iterating on UI changes (macOS only).
#
# Usage:
#   scripts/screenshot.sh <route> [output.png] [wait_seconds] [--release]
#
#   <route>        A `Route::from_name` token: home, settings, protos, scripts,
#                  editor, metrics, slowlog, memoryanalysis, clients, monitor,
#                  config, acl, search, functions, luascripts, persistence,
#                  keyspacenotifications, topology, serverload, valuesearch
#   [output.png]   Defaults to screenshots/<route>.png
#   [wait_seconds] Time to wait for the first frame + data (default 5)
#   --release      Build/run the release binary instead of debug
#
# Requirements:
#   - Screen Recording permission for your terminal (System Settings →
#     Privacy & Security → Screen Recording), or the capture comes out empty.
#   - Server-scoped routes render against the last remembered connection
#     (zedis.toml `selected_server`) — connect once manually beforehand.
#   - No other Zedis instance running (single-instance DB lock).
set -euo pipefail

ROUTE="${1:?usage: scripts/screenshot.sh <route> [output.png] [wait_seconds] [--release]}"
OUT="${2:-screenshots/${ROUTE}.png}"
WAIT="${3:-5}"
PROFILE="debug"
for arg in "$@"; do
  [ "$arg" = "--release" ] && PROFILE="release"
done

cd "$(dirname "$0")/.."

if pgrep -xq zedis; then
  echo "error: a Zedis instance is already running (single-instance DB lock); quit it first" >&2
  exit 1
fi

echo "building ($PROFILE)…"
if [ "$PROFILE" = "release" ]; then
  cargo build --release --quiet
else
  cargo build --quiet
fi

# Resolve the target dir from cargo itself so CARGO_TARGET_DIR / .cargo config
# redirections (e.g. ~/cargo-target) work — never assume ./target.
TARGET_DIR="$(cargo metadata --format-version=1 --no-deps 2>/dev/null \
  | sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p')"
BIN="${TARGET_DIR:-target}/$PROFILE/zedis"
if [ ! -x "$BIN" ]; then
  echo "error: built binary not found at $BIN" >&2
  exit 1
fi

mkdir -p "$(dirname "$OUT")"

"$BIN" --route "$ROUTE" &
APP_PID=$!
cleanup() { kill "$APP_PID" 2>/dev/null || true; wait "$APP_PID" 2>/dev/null || true; }
trap cleanup EXIT

echo "waiting ${WAIT}s for the ${ROUTE} view to render…"
sleep "$WAIT"

# Resolve the window ID of the largest normal-layer window owned by the zedis
# process, via CGWindowList (no external tools; swift ships with Xcode CLT).
WINDOW_SWIFT="$(mktemp -t zedis-window).swift"
cat > "$WINDOW_SWIFT" <<'EOF'
import CoreGraphics
import Foundation
guard let list = CGWindowListCopyWindowInfo([.optionOnScreenOnly, .excludeDesktopElements], kCGNullWindowID)
    as? [[String: Any]] else { exit(1) }
var best: (id: Int, area: Double)? = nil
for w in list {
    guard let owner = w["kCGWindowOwnerName"] as? String, owner.lowercased() == "zedis",
        let layer = w["kCGWindowLayer"] as? Int, layer == 0,
        let id = w["kCGWindowNumber"] as? Int,
        let bounds = w["kCGWindowBounds"] as? [String: Double] else { continue }
    let area = (bounds["Width"] ?? 0) * (bounds["Height"] ?? 0)
    if best == nil || area > best!.area { best = (id, area) }
}
guard let found = best else { exit(2) }
print(found.id)
EOF
WINDOW_ID="$(swift "$WINDOW_SWIFT" 2>/dev/null || true)"
rm -f "$WINDOW_SWIFT"

if [ -z "$WINDOW_ID" ]; then
  echo "error: no zedis window found — did the app fail to start, or is the wait too short?" >&2
  exit 1
fi

# -o: omit the window shadow (stable pixels for diffs); -x: no capture sound.
screencapture -o -x -l "$WINDOW_ID" "$OUT"

if [ ! -s "$OUT" ]; then
  echo "error: capture produced no image — grant your terminal Screen Recording permission" >&2
  exit 1
fi
echo "saved: $OUT"
