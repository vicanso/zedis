#!/usr/bin/env bash
# Starts the Redis topology behind `crates/zedis-connection/tests/live.rs`:
#
#   standalone  127.0.0.1:16379   plain server (ACL users are created by the tests)
#   tls         127.0.0.1:16380   TLS-only server with a self-signed CA (.run/tls/ca.crt)
#   sentinel    127.0.0.1:16479   one sentinel over master 16381 / replica 16382 ("mymaster")
#   cluster     127.0.0.1:17000-17005   3 masters + 3 replicas
#
# Two modes, same script:
#   local   — `redis-server` / `redis-cli` from PATH (override with SERVER_BIN / CLI_BIN),
#             e.g. Homebrew Redis on a dev machine.
#   docker  — `REDIS_IMAGE=redis:7.2 scripts/it/up.sh`: every process is a container on the
#             host network (Linux / GitHub runners). Valkey images need SERVER_BIN=valkey-server
#             CLI_BIN=valkey-cli; `IT_STACK=1` runs redis-stack-server's own entrypoint instead
#             (modules need it) and limits the topology to `standalone`.
#
# `IT_SCENARIOS="standalone tls"` narrows what is started. The resulting ZEDIS_IT_* variables
# are written to scripts/it/.env (and appended to $GITHUB_ENV when set); `make it` sources it.
# `scripts/it/down.sh` stops everything.
set -euo pipefail

HERE=$(cd "$(dirname "$0")" && pwd)
IT_DIR=${IT_DIR:-$HERE/.run}
IMAGE=${REDIS_IMAGE:-}
STACK=${IT_STACK:-0}
SERVER_BIN=${SERVER_BIN:-redis-server}
CLI_BIN=${CLI_BIN:-redis-cli}
SCENARIOS=${IT_SCENARIOS:-"standalone tls sentinel cluster"}
if [ "$STACK" = "1" ]; then SCENARIOS="standalone"; fi

PORT_STANDALONE=16379
PORT_TLS=16380
PORT_MASTER=16381
PORT_REPLICA=16382
PORT_SENTINEL=16479
PORT_CLUSTER_BASE=17000
MASTER_NAME=mymaster

"$HERE/down.sh" >/dev/null 2>&1 || true
mkdir -p "$IT_DIR"
# Paths as the *server* sees them: the container mounts $IT_DIR at /it.
if [ -n "$IMAGE" ]; then FS=/it; else FS=$IT_DIR; fi

start() { # <name> <server args…>
  local name=$1; shift
  if [ -n "$IMAGE" ]; then
    docker run -d --rm --network host --name "zedis-it-$name" -v "$IT_DIR:/it" "$IMAGE" "$SERVER_BIN" "$@" >/dev/null
    echo "zedis-it-$name" >> "$IT_DIR/containers"
  else
    ( cd "$IT_DIR" && exec "$SERVER_BIN" "$@" ) > "$IT_DIR/$name.log" 2>&1 &
    echo $! >> "$IT_DIR/pids"
  fi
}
cli() {
  if [ -n "$IMAGE" ]; then
    docker run --rm --network host -v "$IT_DIR:/it" "$IMAGE" "$CLI_BIN" "$@"
  else
    "$CLI_BIN" "$@"
  fi
}
wait_pong() { # <label> <cli args…>
  local label=$1; shift
  for _ in $(seq 1 60); do
    if [ "$(cli "$@" ping 2>/dev/null || true)" = "PONG" ]; then echo "  $label ready"; return 0; fi
    sleep 0.5
  done
  echo "!! $label did not come up" >&2
  [ -n "$IMAGE" ] || cat "$IT_DIR"/*.log >&2 || true
  exit 1
}
has() { case " $SCENARIOS " in *" $1 "*) return 0;; *) return 1;; esac; }

: > "$IT_DIR/env"
env_put() { echo "$1=$2" >> "$IT_DIR/env"; }

# ── standalone ───────────────────────────────────────────────────────────
if has standalone; then
  echo "standalone :$PORT_STANDALONE"
  if [ "$STACK" = "1" ]; then
    docker run -d --rm --network host --name zedis-it-standalone \
      -e REDIS_ARGS="--port $PORT_STANDALONE --save '' --appendonly no" "$IMAGE" >/dev/null
    echo zedis-it-standalone >> "$IT_DIR/containers"
  else
    start standalone --port "$PORT_STANDALONE" --save "" --appendonly no --dir "$FS"
  fi
  wait_pong standalone -p "$PORT_STANDALONE"
  env_put ZEDIS_IT_STANDALONE "127.0.0.1:$PORT_STANDALONE"
fi

# ── tls ──────────────────────────────────────────────────────────────────
if has tls; then
  echo "tls :$PORT_TLS"
  mkdir -p "$IT_DIR/tls"
  (
    cd "$IT_DIR/tls"
    openssl req -x509 -newkey rsa:2048 -nodes -keyout ca.key -out ca.crt -days 2 -subj "/CN=zedis-it-ca" 2>/dev/null
    openssl req -newkey rsa:2048 -nodes -keyout server.key -out server.csr -subj "/CN=127.0.0.1" 2>/dev/null
    printf "subjectAltName=IP:127.0.0.1,DNS:localhost\n" > san.cnf
    openssl x509 -req -in server.csr -CA ca.crt -CAkey ca.key -CAcreateserial -out server.crt -days 2 -extfile san.cnf 2>/dev/null
    chmod 644 server.key ca.key
  )
  start tls --port 0 --tls-port "$PORT_TLS" --tls-cert-file "$FS/tls/server.crt" --tls-key-file "$FS/tls/server.key" \
    --tls-ca-cert-file "$FS/tls/ca.crt" --tls-auth-clients no --save "" --appendonly no --dir "$FS"
  wait_pong tls --tls --cacert "$FS/tls/ca.crt" -p "$PORT_TLS"
  env_put ZEDIS_IT_TLS "127.0.0.1:$PORT_TLS"
  env_put ZEDIS_IT_TLS_CA "$IT_DIR/tls/ca.crt"
fi

# ── sentinel ─────────────────────────────────────────────────────────────
if has sentinel; then
  echo "sentinel :$PORT_SENTINEL (master :$PORT_MASTER, replica :$PORT_REPLICA)"
  start master --port "$PORT_MASTER" --save "" --appendonly no --dir "$FS"
  start replica --port "$PORT_REPLICA" --save "" --appendonly no --dir "$FS" --replicaof 127.0.0.1 "$PORT_MASTER"
  wait_pong master -p "$PORT_MASTER"
  wait_pong replica -p "$PORT_REPLICA"
  cat > "$IT_DIR/sentinel.conf" <<CONF
port $PORT_SENTINEL
dir $FS
sentinel monitor $MASTER_NAME 127.0.0.1 $PORT_MASTER 1
sentinel down-after-milliseconds $MASTER_NAME 5000
sentinel failover-timeout $MASTER_NAME 10000
sentinel resolve-hostnames no
CONF
  chmod 666 "$IT_DIR/sentinel.conf"
  start sentinel "$FS/sentinel.conf" --sentinel
  wait_pong sentinel -p "$PORT_SENTINEL"
  env_put ZEDIS_IT_SENTINEL "127.0.0.1:$PORT_SENTINEL"
  env_put ZEDIS_IT_MASTER_NAME "$MASTER_NAME"
  env_put ZEDIS_IT_SENTINEL_MASTER_PORT "$PORT_MASTER"
fi

# ── cluster ──────────────────────────────────────────────────────────────
if has cluster; then
  echo "cluster :$PORT_CLUSTER_BASE-$((PORT_CLUSTER_BASE + 5))"
  nodes=""
  for i in 0 1 2 3 4 5; do
    port=$((PORT_CLUSTER_BASE + i))
    start "cluster-$port" --port "$port" --cluster-enabled yes --cluster-config-file "$FS/nodes-$port.conf" \
      --cluster-node-timeout 5000 --cluster-announce-ip 127.0.0.1 --save "" --appendonly no --dir "$FS"
    nodes="$nodes 127.0.0.1:$port"
  done
  for i in 0 1 2 3 4 5; do wait_pong "cluster node $((PORT_CLUSTER_BASE + i))" -p $((PORT_CLUSTER_BASE + i)); done
  # shellcheck disable=SC2086
  cli --cluster create $nodes --cluster-replicas 1 --cluster-yes >/dev/null
  for _ in $(seq 1 60); do
    if cli -p "$PORT_CLUSTER_BASE" cluster info 2>/dev/null | grep -q 'cluster_state:ok'; then break; fi
    sleep 0.5
  done
  cli -p "$PORT_CLUSTER_BASE" cluster info | grep -q 'cluster_state:ok' || { echo "!! cluster never reached state ok" >&2; exit 1; }
  echo "  cluster ready"
  env_put ZEDIS_IT_CLUSTER "127.0.0.1:$PORT_CLUSTER_BASE"
fi

[ "$STACK" = "1" ] && env_put ZEDIS_IT_STACK 1
cp "$IT_DIR/env" "$HERE/.env"
if [ -n "${GITHUB_ENV:-}" ]; then cat "$IT_DIR/env" >> "$GITHUB_ENV"; fi
echo "wrote $HERE/.env:"
sed 's/^/  /' "$HERE/.env"
