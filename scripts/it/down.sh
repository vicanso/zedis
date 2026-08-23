#!/usr/bin/env bash
# Stops everything `up.sh` started (local processes or docker containers)
# and waits until they are really gone, so an immediate `up.sh` can rebind
# the same ports.
set -uo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
IT_DIR=${IT_DIR:-$HERE/.run}
if [ -f "$IT_DIR/containers" ]; then
  xargs -r docker rm -f < "$IT_DIR/containers" >/dev/null 2>&1 || true
fi
if [ -f "$IT_DIR/pids" ]; then
  pids=$(tr '\n' ' ' < "$IT_DIR/pids")
  # shellcheck disable=SC2086
  kill $pids 2>/dev/null || true
  for _ in $(seq 1 40); do
    alive=0
    for pid in $pids; do kill -0 "$pid" 2>/dev/null && alive=1; done
    [ "$alive" = 0 ] && break
    sleep 0.25
  done
  # shellcheck disable=SC2086
  kill -9 $pids 2>/dev/null || true
fi
rm -rf "$IT_DIR" "$HERE/.env"
