#!/usr/bin/env bash
# Starts the Redis topology behind `crates/zedis-connection/tests/live.rs`:
#
#   standalone  127.0.0.1:16379   plain server (ACL users are created by the tests)
#   tls         127.0.0.1:16380   TLS-only server with a self-signed CA (.run/tls/ca.crt)
#   sentinel    127.0.0.1:16479   one sentinel over two masters: 16381 / replica 16382
#                                 ("mymaster") and 16383 / replica 16384 ("mymaster2")
#                                 (`IT_PORT_BASE=26379` shifts these seven ports together)
#   cluster     127.0.0.1:17000-17005   3 masters + 3 replicas (cluster bus on 27000-27005;
#                                       `IT_CLUSTER_BASE=7100` moves the block when those are taken)
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

# `IT_PORT_BASE` shifts the whole non-cluster block (defaults: 16379 /
# 16380 / 16381 / 16382 / 16479); `IT_CLUSTER_BASE` the six cluster nodes
# (and their bus ports at +10000).
PORT_BASE=${IT_PORT_BASE:-16379}
PORT_STANDALONE=$PORT_BASE
PORT_TLS=$((PORT_BASE + 1))
PORT_MASTER=$((PORT_BASE + 2))
PORT_REPLICA=$((PORT_BASE + 3))
PORT_MASTER2=$((PORT_BASE + 4))
PORT_REPLICA2=$((PORT_BASE + 5))
PORT_SENTINEL=$((PORT_BASE + 100))
PORT_CLUSTER_BASE=${IT_CLUSTER_BASE:-17000}
MASTER_NAME=mymaster
MASTER_NAME2=mymaster2

"$HERE/down.sh" >/dev/null 2>&1 || true
mkdir -p "$IT_DIR"
# World-writable on purpose: the official images run the server as their own
# `redis` / `valkey` user (the entrypoint re-execs under gosu), while this
# bind-mounted directory keeps the *host* owner. Cluster nodes are the only
# servers here that must CREATE a file in it (`nodes-<port>.conf`), so
# without this they die at startup with "Can't open … in order to acquire a
# lock: Permission denied" while every other scenario comes up fine.
chmod 777 "$IT_DIR"
# Paths as the *server* sees them: the container mounts $IT_DIR at /it.
if [ -n "$IMAGE" ]; then FS=/it; else FS=$IT_DIR; fi

start() { # <name> <server args…>
  local name=$1; shift
  if [ -n "$IMAGE" ]; then
    # No `--rm`: a container that dies at startup must stay around for
    # `docker logs` (down.sh removes them all).
    docker run -d --network host --name "zedis-it-$name" -v "$IT_DIR:/it" "$IMAGE" "$SERVER_BIN" "$@" >/dev/null
    echo "zedis-it-$name" >> "$IT_DIR/containers"
  else
    ( cd "$IT_DIR" && exec "$SERVER_BIN" "$@" ) > "$IT_DIR/$name.log" 2>&1 &
    echo $! > "$IT_DIR/$name.pid"
    echo $! >> "$IT_DIR/pids"
  fi
}
# Whether the server started under <name> is still running.
alive() { # <name>
  if [ -n "$IMAGE" ]; then
    [ "$(docker inspect -f '{{.State.Running}}' "zedis-it-$1" 2>/dev/null)" = "true" ]
  else
    local pid
    pid=$(cat "$IT_DIR/$1.pid" 2>/dev/null || true)
    [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null
  fi
}
cli() {
  if [ -n "$IMAGE" ]; then
    docker run --rm --network host -v "$IT_DIR:/it" "$IMAGE" "$CLI_BIN" "$@"
  else
    "$CLI_BIN" "$@"
  fi
}
# Prints the server's own log (local file or `docker logs`) on a failure.
show_log() { # <name>
  local name=$1
  echo "---- $name log ----" >&2
  if [ -n "$IMAGE" ]; then docker logs "zedis-it-$name" 2>&1 | tail -20 >&2 || true
  else tail -20 "$IT_DIR/$name.log" >&2 2>/dev/null || true; fi
}
wait_pong() { # <name> <cli args…>   (name = the log to show on failure)
  local name=$1; shift
  for _ in $(seq 1 60); do
    if [ "$(cli "$@" ping 2>/dev/null || true)" = "PONG" ]; then echo "  $name ready"; return 0; fi
    # Don't sit out the timeout when the server already died (bad config,
    # port taken, unwritable dir): say so now and print why.
    if ! alive "$name"; then
      echo "!! $name exited at startup" >&2
      show_log "$name"
      exit 1
    fi
    sleep 0.5
  done
  echo "!! $name did not come up" >&2
  show_log "$name"
  exit 1
}
has() { case " $SCENARIOS " in *" $1 "*) return 0;; *) return 1;; esac; }
# True when something listens on 127.0.0.1:<port> (any protocol).
port_busy() { (exec 3<>"/dev/tcp/127.0.0.1/$1") 2>/dev/null; }
# A port still listening belongs to a previous run that hasn't finished
# exiting (or to something else entirely): starting on top of it would
# bind-fail while the old process still answers. Plain TCP probe — a PING
# would hang on a non-Redis listener.
wait_port_free() { # <port> <what>
  local port=$1 what=$2
  for _ in $(seq 1 40); do
    port_busy "$port" || return 0
    sleep 0.25
  done
  echo "!! port $port ($what) is already in use — stop that process or set IT_PORT_BASE / IT_CLUSTER_BASE" >&2
  exit 1
}

: > "$IT_DIR/env"
env_put() { echo "$1=$2" >> "$IT_DIR/env"; }

# ── standalone ───────────────────────────────────────────────────────────
if has standalone; then
  echo "standalone :$PORT_STANDALONE"
  wait_port_free "$PORT_STANDALONE" standalone
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
  wait_port_free "$PORT_TLS" tls
  start tls --port 0 --tls-port "$PORT_TLS" --tls-cert-file "$FS/tls/server.crt" --tls-key-file "$FS/tls/server.key" \
    --tls-ca-cert-file "$FS/tls/ca.crt" --tls-auth-clients no --save "" --appendonly no --dir "$FS"
  wait_pong tls --tls --cacert "$FS/tls/ca.crt" -p "$PORT_TLS"
  env_put ZEDIS_IT_TLS "127.0.0.1:$PORT_TLS"
  env_put ZEDIS_IT_TLS_CA "$IT_DIR/tls/ca.crt"
fi

# ── sentinel ─────────────────────────────────────────────────────────────
if has sentinel; then
  # Two monitored masters: the second is what the multi-master paths
  # (first-by-name selection, the Topology switcher) are tested against.
  echo "sentinel :$PORT_SENTINEL (masters :$PORT_MASTER / :$PORT_MASTER2, replicas :$PORT_REPLICA / :$PORT_REPLICA2)"
  wait_port_free "$PORT_MASTER" "sentinel master"
  wait_port_free "$PORT_REPLICA" "sentinel replica"
  wait_port_free "$PORT_MASTER2" "sentinel master 2"
  wait_port_free "$PORT_REPLICA2" "sentinel replica 2"
  wait_port_free "$PORT_SENTINEL" sentinel
  start master --port "$PORT_MASTER" --save "" --appendonly no --dir "$FS"
  start replica --port "$PORT_REPLICA" --save "" --appendonly no --dir "$FS" --replicaof 127.0.0.1 "$PORT_MASTER"
  start master2 --port "$PORT_MASTER2" --save "" --appendonly no --dir "$FS"
  start replica2 --port "$PORT_REPLICA2" --save "" --appendonly no --dir "$FS" --replicaof 127.0.0.1 "$PORT_MASTER2"
  wait_pong master -p "$PORT_MASTER"
  wait_pong replica -p "$PORT_REPLICA"
  wait_pong master2 -p "$PORT_MASTER2"
  wait_pong replica2 -p "$PORT_REPLICA2"
  cat > "$IT_DIR/sentinel.conf" <<CONF
port $PORT_SENTINEL
dir $FS
sentinel monitor $MASTER_NAME 127.0.0.1 $PORT_MASTER 1
sentinel down-after-milliseconds $MASTER_NAME 5000
sentinel failover-timeout $MASTER_NAME 10000
sentinel monitor $MASTER_NAME2 127.0.0.1 $PORT_MASTER2 1
sentinel down-after-milliseconds $MASTER_NAME2 5000
sentinel failover-timeout $MASTER_NAME2 10000
sentinel resolve-hostnames no
CONF
  chmod 666 "$IT_DIR/sentinel.conf"
  start sentinel "$FS/sentinel.conf" --sentinel
  wait_pong sentinel -p "$PORT_SENTINEL"
  # A failover needs a replica the sentinel has already seen (INFO poll,
  # ~10s): wait until `num-slaves` is non-zero so tests can fail over at once.
  for _ in $(seq 1 60); do
    n=$(cli -p "$PORT_SENTINEL" sentinel master "$MASTER_NAME" 2>/dev/null | grep -A1 '^num-slaves$' | tail -1)
    if [ "${n:-0}" != "0" ] && [ -n "$n" ]; then break; fi
    sleep 0.5
  done
  echo "  sentinel sees $n replica(s)"
  env_put ZEDIS_IT_SENTINEL "127.0.0.1:$PORT_SENTINEL"
  env_put ZEDIS_IT_MASTER_NAME "$MASTER_NAME"
  env_put ZEDIS_IT_MASTER_NAME2 "$MASTER_NAME2"
fi

# ── cluster ──────────────────────────────────────────────────────────────
if has cluster; then
  echo "cluster :$PORT_CLUSTER_BASE-$((PORT_CLUSTER_BASE + 5))"
  nodes=""
  # Each node also binds its cluster bus on port + 10000; a foreign listener
  # there makes redis-server exit at startup ("Could not bind").
  for i in 0 1 2 3 4 5; do
    wait_port_free $((PORT_CLUSTER_BASE + i)) "cluster node"
    wait_port_free $((PORT_CLUSTER_BASE + i + 10000)) "cluster bus of node $((PORT_CLUSTER_BASE + i))"
  done
  for i in 0 1 2 3 4 5; do
    port=$((PORT_CLUSTER_BASE + i))
    start "cluster-$port" --port "$port" --cluster-enabled yes --cluster-config-file "$FS/nodes-$port.conf" \
      --cluster-node-timeout 5000 --cluster-announce-ip 127.0.0.1 --save "" --appendonly no --dir "$FS"
    nodes="$nodes 127.0.0.1:$port"
  done
  for i in 0 1 2 3 4 5; do wait_pong "cluster-$((PORT_CLUSTER_BASE + i))" -p $((PORT_CLUSTER_BASE + i)); done
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
