#!/usr/bin/env bash
# Stops everything `up.sh` started (local processes or docker containers).
set -uo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
IT_DIR=${IT_DIR:-$HERE/.run}
if [ -f "$IT_DIR/containers" ]; then
  xargs -r docker rm -f < "$IT_DIR/containers" >/dev/null 2>&1 || true
fi
if [ -f "$IT_DIR/pids" ]; then
  while read -r pid; do kill "$pid" 2>/dev/null || true; done < "$IT_DIR/pids"
fi
rm -rf "$IT_DIR" "$HERE/.env"
