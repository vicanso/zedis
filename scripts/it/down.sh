#!/usr/bin/env bash
# Stops everything `up.sh` started (local processes or docker containers)
# and waits until they are really gone, so an immediate `up.sh` can rebind
# the same ports.
set -uo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
IT_DIR=${IT_DIR:-$HERE/.run}
CONTAINERS="$IT_DIR/containers"
if [ -f "$CONTAINERS" ]; then
  xargs -r docker rm -f < "$CONTAINERS" >/dev/null 2>&1 || true
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
# A containerised server writes into the mounted run directory as the
# image's own user (uid 999 in the redis images), and a file it created is
# not the runner's to delete: `rm -rf` then fails with "Permission denied"
# and the teardown — not any test — fails the job. It takes one command to
# produce such a file (`BGREWRITEAOF` leaves an `appendonlydir/`, which is a
# *directory* from Redis 7 on, so `rm -r` needs write permission *inside*
# it), so this is about the general case, not that one command.
#
# Root inside a throwaway container of the same image can remove them; the
# image is already pulled, so this costs nothing. Best-effort: the plain
# `rm -rf` below still has to succeed, and does once the directory is empty.
IMAGE=${REDIS_IMAGE:-}
[ -z "$IMAGE" ] && [ -f "$IT_DIR/image" ] && IMAGE=$(cat "$IT_DIR/image")
if [ -n "$IMAGE" ] && [ -d "$IT_DIR" ]; then
  docker run --rm --user 0:0 -v "$IT_DIR:/it" --entrypoint sh "$IMAGE" \
    -c 'rm -rf /it/* /it/.[!.]* 2>/dev/null; exit 0' >/dev/null 2>&1 || true
fi
rm -rf "$IT_DIR" "$HERE/.env"
