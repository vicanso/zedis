#!/usr/bin/env bash
# Starts the Redis topology behind `crates/zedis-connection/tests/live.rs`:
#
#   standalone  127.0.0.1:16379   plain server (ACL users are created by the tests)
#   tls         127.0.0.1:16380   TLS-only server with a self-signed CA (.run/tls/ca.crt)
#   mtls        127.0.0.1:16388   TLS-only server that *requires* a client certificate
#                                 (--tls-auth-clients yes) off the same CA, plus a second
#                                 copy of the client key encrypted with a passphrase
#   sentinel    127.0.0.1:16479   one sentinel over two masters: 16381 / replica 16382
#                                 ("mymaster") and 16383 / replica 16384 ("mymaster2").
#                                 Password-protected, and the sentinel's own password is a
#                                 *different* one — the shape `sentinel_password` exists for.
#   busy        127.0.0.1:16385   plain server the runaway-script test parks in BUSY (its own,
#                                 so the other tests never see the BUSY replies)
#   replication 127.0.0.1:16386   a primary with a replica on 16387, for the REPLICAOF / FAILOVER
#                                 test (its own pair: that test promotes and fails over, which
#                                 would upset the sentinel's)
#                                 (`IT_PORT_BASE=26379` shifts these twelve ports together)
#   ssh         127.0.0.1:16389   an unprivileged sshd, public-key only, that the tunnel tests
#                                 reach the standalone server through (skipped when the host has
#                                 no sshd — `make it` then skips the tunnel tests too)
#   cluster     127.0.0.1:17000-17005   3 masters + 3 replicas, password-protected (cluster bus on
#                                       27000-27005; `IT_CLUSTER_BASE=7100` moves the block when
#                                       those are taken)
#
# Sentinel and cluster are the two topologies whose auth plumbing is not just "send AUTH"
# (`masterauth` for replication, `sentinel auth-pass` for monitoring, a redirect that has to
# re-authenticate), so they carry passwords; standalone / tls / busy / replication stay open so
# the no-credentials path keeps its coverage too.
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
SCENARIOS=${IT_SCENARIOS:-"standalone tls mtls sentinel cluster busy replication ssh"}
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
PORT_BUSY=$((PORT_BASE + 6))
PORT_REPL_PRIMARY=$((PORT_BASE + 7))
PORT_REPL_REPLICA=$((PORT_BASE + 8))
PORT_MTLS=$((PORT_BASE + 9))
PORT_SSH=$((PORT_BASE + 10))
PORT_SENTINEL=$((PORT_BASE + 100))
PORT_CLUSTER_BASE=${IT_CLUSTER_BASE:-17000}
MASTER_NAME=mymaster
MASTER_NAME2=mymaster2
# The data nodes' password (sentinel's masters and every cluster node), and
# the sentinel's own — deliberately different, because a sentinel with its
# own credentials is the case `sentinel_username` / `sentinel_password`
# exist for and the only way to prove they are used where they should be.
DATA_PASSWORD=zedis-it-data-pw
SENTINEL_PASSWORD=zedis-it-sentinel-pw
# `--no-auth-warning` keeps redis-cli from writing the password to stderr on
# every single call (6.0+, and the oldest server in the matrix is 6.2).
DATA_AUTH=(-a "$DATA_PASSWORD" --no-auth-warning)
SENTINEL_AUTH=(-a "$SENTINEL_PASSWORD" --no-auth-warning)

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
# Always a host process, never a container: sshd is not part of the Redis
# image, and in docker mode the servers are on the host network anyway, so
# a tunnel opened here reaches them all the same.
start_local() { # <name> <command…>
  local name=$1; shift
  ( cd "$IT_DIR" && exec "$@" ) > "$IT_DIR/$name.log" 2>&1 &
  echo $! > "$IT_DIR/$name.pid"
  echo $! >> "$IT_DIR/pids"
}
# Whether the server started under <name> is still running. Keyed on how it
# was started, not on the mode: `start_local` writes a pid file even in
# docker mode, and asking docker about a host process answers "not running"
# for something that is perfectly alive.
alive() { # <name>
  if [ -f "$IT_DIR/$1.pid" ]; then
    local pid
    pid=$(cat "$IT_DIR/$1.pid" 2>/dev/null || true)
    [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null
    return
  fi
  [ -n "$IMAGE" ] && [ "$(docker inspect -f '{{.State.Running}}' "zedis-it-$1" 2>/dev/null)" = "true" ]
}
cli() {
  if [ -n "$IMAGE" ]; then
    docker run --rm --network host -v "$IT_DIR:/it" "$IMAGE" "$CLI_BIN" "$@"
  else
    "$CLI_BIN" "$@"
  fi
}
# Prints the server's own log on a failure. Same rule as `alive`: a host
# process has a log file, whatever mode the rest of the topology runs in.
show_log() { # <name>
  local name=$1
  echo "---- $name log ----" >&2
  if [ -f "$IT_DIR/$name.log" ]; then tail -20 "$IT_DIR/$name.log" >&2 2>/dev/null || true
  elif [ -n "$IMAGE" ]; then docker logs "zedis-it-$name" 2>&1 | tail -20 >&2 || true; fi
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

# One CA for both TLS scenarios, generated once: `tls` verifies the server,
# `mtls` additionally verifies the client, and a client certificate is only
# a test of anything if the server trusts the issuer it was signed by.
CLIENT_KEY_PASSPHRASE=zedis-it-client-key
gen_tls_material() {
  if [ -f "$IT_DIR/tls/server.crt" ]; then
    return 0
  fi
  mkdir -p "$IT_DIR/tls"
  (
    cd "$IT_DIR/tls"
    openssl req -x509 -newkey rsa:2048 -nodes -keyout ca.key -out ca.crt -days 2 -subj "/CN=zedis-it-ca" 2>/dev/null
    openssl req -newkey rsa:2048 -nodes -keyout server.key -out server.csr -subj "/CN=127.0.0.1" 2>/dev/null
    printf "subjectAltName=IP:127.0.0.1,DNS:localhost\n" > san.cnf
    openssl x509 -req -in server.csr -CA ca.crt -CAkey ca.key -CAcreateserial -out server.crt -days 2 -extfile san.cnf 2>/dev/null
    # The client half: same CA, plus a PKCS#8 copy of the key encrypted with
    # a passphrase — the `client_key_passphrase` path, which a plaintext key
    # never exercises.
    #
    # `-extfile` is not decoration: `x509 -req` emits a **version 1**
    # certificate when it adds no extensions (OpenSSL 3.0 — 3.6 happens to
    # add SKID/AKID on its own and yield v3), and rustls refuses a v1 peer
    # certificate outright with `UnsupportedCertVersion`. The server cert
    # gets its v3 from `san.cnf`; give the client one of its own so both are
    # v3 on every OpenSSL.
    printf "basicConstraints=CA:FALSE\nkeyUsage=digitalSignature,keyEncipherment\nextendedKeyUsage=clientAuth\n" > client.cnf
    openssl req -newkey rsa:2048 -nodes -keyout client.key -out client.csr -subj "/CN=zedis-it-client" 2>/dev/null
    openssl x509 -req -in client.csr -CA ca.crt -CAkey ca.key -CAcreateserial -out client.crt -days 2 -extfile client.cnf 2>/dev/null
    openssl pkcs8 -topk8 -in client.key -out client.enc.key -passout "pass:$CLIENT_KEY_PASSPHRASE" 2>/dev/null
    chmod 644 server.key ca.key client.key client.enc.key
  )
  # Catch a v1 certificate here rather than as a rustls error inside a test
  # ten minutes later — this differs by OpenSSL version, so it has to be
  # asserted on the host that generated them.
  for cert in ca server client; do
    if ! openssl x509 -in "$IT_DIR/tls/$cert.crt" -noout -text 2>/dev/null | grep -q "Version: 3"; then
      echo "!! tls/$cert.crt is not X.509 v3 — rustls will refuse it (openssl $(openssl version))" >&2
      exit 1
    fi
  done
  env_put ZEDIS_IT_TLS_CA "$IT_DIR/tls/ca.crt"
}

# ── tls ──────────────────────────────────────────────────────────────────
if has tls; then
  echo "tls :$PORT_TLS"
  gen_tls_material
  wait_port_free "$PORT_TLS" tls
  start tls --port 0 --tls-port "$PORT_TLS" --tls-cert-file "$FS/tls/server.crt" --tls-key-file "$FS/tls/server.key" \
    --tls-ca-cert-file "$FS/tls/ca.crt" --tls-auth-clients no --save "" --appendonly no --dir "$FS"
  wait_pong tls --tls --cacert "$FS/tls/ca.crt" -p "$PORT_TLS"
  env_put ZEDIS_IT_TLS "127.0.0.1:$PORT_TLS"
fi

# ── mtls ─────────────────────────────────────────────────────────────────
# Its own port rather than flipping the `tls` one: `--tls-auth-clients yes`
# rejects every certificate-less client, which is exactly what the `tls`
# tests are.
if has mtls; then
  echo "mtls :$PORT_MTLS"
  gen_tls_material
  wait_port_free "$PORT_MTLS" mtls
  start mtls --port 0 --tls-port "$PORT_MTLS" --tls-cert-file "$FS/tls/server.crt" --tls-key-file "$FS/tls/server.key" \
    --tls-ca-cert-file "$FS/tls/ca.crt" --tls-auth-clients yes --save "" --appendonly no --dir "$FS"
  wait_pong mtls --tls --cacert "$FS/tls/ca.crt" --cert "$FS/tls/client.crt" --key "$FS/tls/client.key" -p "$PORT_MTLS"
  env_put ZEDIS_IT_MTLS "127.0.0.1:$PORT_MTLS"
  env_put ZEDIS_IT_TLS_CLIENT_CERT "$IT_DIR/tls/client.crt"
  env_put ZEDIS_IT_TLS_CLIENT_KEY "$IT_DIR/tls/client.key"
  env_put ZEDIS_IT_TLS_CLIENT_KEY_ENC "$IT_DIR/tls/client.enc.key"
  env_put ZEDIS_IT_TLS_CLIENT_KEY_PASSPHRASE "$CLIENT_KEY_PASSPHRASE"
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
  # `masterauth` as well as `requirepass`: a replica has to authenticate to
  # its master, and after a failover every one of these four takes both
  # roles in turn.
  AUTH=(--requirepass "$DATA_PASSWORD" --masterauth "$DATA_PASSWORD")
  start master --port "$PORT_MASTER" --save "" --appendonly no --dir "$FS" "${AUTH[@]}"
  start replica --port "$PORT_REPLICA" --save "" --appendonly no --dir "$FS" "${AUTH[@]}" --replicaof 127.0.0.1 "$PORT_MASTER"
  start master2 --port "$PORT_MASTER2" --save "" --appendonly no --dir "$FS" "${AUTH[@]}"
  start replica2 --port "$PORT_REPLICA2" --save "" --appendonly no --dir "$FS" "${AUTH[@]}" --replicaof 127.0.0.1 "$PORT_MASTER2"
  wait_pong master -p "$PORT_MASTER" "${DATA_AUTH[@]}"
  wait_pong replica -p "$PORT_REPLICA" "${DATA_AUTH[@]}"
  wait_pong master2 -p "$PORT_MASTER2" "${DATA_AUTH[@]}"
  wait_pong replica2 -p "$PORT_REPLICA2" "${DATA_AUTH[@]}"
  cat > "$IT_DIR/sentinel.conf" <<CONF
port $PORT_SENTINEL
dir $FS
requirepass $SENTINEL_PASSWORD
sentinel monitor $MASTER_NAME 127.0.0.1 $PORT_MASTER 1
sentinel auth-pass $MASTER_NAME $DATA_PASSWORD
sentinel down-after-milliseconds $MASTER_NAME 5000
sentinel failover-timeout $MASTER_NAME 10000
sentinel monitor $MASTER_NAME2 127.0.0.1 $PORT_MASTER2 1
sentinel auth-pass $MASTER_NAME2 $DATA_PASSWORD
sentinel down-after-milliseconds $MASTER_NAME2 5000
sentinel failover-timeout $MASTER_NAME2 10000
sentinel resolve-hostnames no
CONF
  chmod 666 "$IT_DIR/sentinel.conf"
  start sentinel "$FS/sentinel.conf" --sentinel
  wait_pong sentinel -p "$PORT_SENTINEL" "${SENTINEL_AUTH[@]}"
  # A failover needs a replica the sentinel has already seen (INFO poll,
  # ~10s): wait until `num-slaves` is non-zero so tests can fail over at once.
  for _ in $(seq 1 60); do
    n=$(cli -p "$PORT_SENTINEL" "${SENTINEL_AUTH[@]}" sentinel master "$MASTER_NAME" 2>/dev/null | grep -A1 '^num-slaves$' | tail -1)
    if [ "${n:-0}" != "0" ] && [ -n "$n" ]; then break; fi
    sleep 0.5
  done
  echo "  sentinel sees $n replica(s)"
  env_put ZEDIS_IT_SENTINEL "127.0.0.1:$PORT_SENTINEL"
  env_put ZEDIS_IT_MASTER_NAME "$MASTER_NAME"
  env_put ZEDIS_IT_MASTER_NAME2 "$MASTER_NAME2"
  env_put ZEDIS_IT_SENTINEL_PASSWORD "$SENTINEL_PASSWORD"
fi

# ── busy ─────────────────────────────────────────────────────────────────
if has busy; then
  echo "busy :$PORT_BUSY"
  wait_port_free "$PORT_BUSY" busy
  start busy --port "$PORT_BUSY" --save "" --appendonly no --dir "$FS"
  wait_pong busy -p "$PORT_BUSY"
  env_put ZEDIS_IT_BUSY "127.0.0.1:$PORT_BUSY"
fi

# ── replication ──────────────────────────────────────────────────────────
if has replication; then
  echo "replication :$PORT_REPL_PRIMARY (replica :$PORT_REPL_REPLICA)"
  wait_port_free "$PORT_REPL_PRIMARY" "replication primary"
  wait_port_free "$PORT_REPL_REPLICA" "replication replica"
  start repl-primary --port "$PORT_REPL_PRIMARY" --save "" --appendonly no --dir "$FS"
  start repl-replica --port "$PORT_REPL_REPLICA" --save "" --appendonly no --dir "$FS" --replicaof 127.0.0.1 "$PORT_REPL_PRIMARY"
  wait_pong repl-primary -p "$PORT_REPL_PRIMARY"
  wait_pong repl-replica -p "$PORT_REPL_REPLICA"
  env_put ZEDIS_IT_REPL_PRIMARY "127.0.0.1:$PORT_REPL_PRIMARY"
  env_put ZEDIS_IT_REPL_REPLICA "127.0.0.1:$PORT_REPL_REPLICA"
fi

# ── ssh ──────────────────────────────────────────────────────────────────
# An sshd of our own, run as the current user on a high port. Unprivileged
# sshd cannot change uid, so the only login it can grant is back to the user
# who started it — which is exactly the account whose `authorized_keys` we
# just wrote, and no wider a door than that.
#
# Public-key only: password auth would need this user's real system
# password, which no test can know. That still covers both key paths the app
# has, since the second key here is passphrase-encrypted.
if has ssh; then
  SSHD_BIN=${SSHD_BIN:-$(command -v sshd || echo /usr/sbin/sshd)}
  SSH_KEY_PASSPHRASE=zedis-it-ssh-key
  if [ ! -x "$SSHD_BIN" ]; then
    echo "ssh: no sshd on this host — skipping the scenario (the tunnel tests skip with it)"
  else
    echo "ssh :$PORT_SSH"
    rm -rf "$IT_DIR/ssh"
    mkdir -p "$IT_DIR/ssh"
    ssh-keygen -q -t ed25519 -N "" -C zedis-it-host -f "$IT_DIR/ssh/host_ed25519"
    ssh-keygen -q -t ed25519 -N "" -C zedis-it-client -f "$IT_DIR/ssh/id_ed25519"
    # A second copy of the same key, encrypted: the `ssh_key_passphrase`
    # path, which an unencrypted key never reaches.
    cp "$IT_DIR/ssh/id_ed25519" "$IT_DIR/ssh/id_ed25519_enc"
    ssh-keygen -q -p -P "" -N "$SSH_KEY_PASSPHRASE" -f "$IT_DIR/ssh/id_ed25519_enc"
    cp "$IT_DIR/ssh/id_ed25519.pub" "$IT_DIR/ssh/authorized_keys"
    chmod 700 "$IT_DIR/ssh"
    chmod 600 "$IT_DIR/ssh/host_ed25519" "$IT_DIR/ssh/id_ed25519" "$IT_DIR/ssh/id_ed25519_enc" \
      "$IT_DIR/ssh/authorized_keys"
    cat > "$IT_DIR/ssh/sshd_config" <<CONF
Port $PORT_SSH
ListenAddress 127.0.0.1
HostKey $IT_DIR/ssh/host_ed25519
AuthorizedKeysFile $IT_DIR/ssh/authorized_keys
PidFile $IT_DIR/ssh/sshd.pid
# The run directory lives under the repo, whose permissions sshd would
# otherwise refuse to trust.
StrictModes no
UsePAM no
PasswordAuthentication no
KbdInteractiveAuthentication no
PubkeyAuthentication yes
AllowTcpForwarding yes
PermitTunnel no
X11Forwarding no
PrintMotd no
LogLevel VERBOSE
CONF
    wait_port_free "$PORT_SSH" ssh
    start_local sshd "$SSHD_BIN" -D -e -f "$IT_DIR/ssh/sshd_config"
    ready=0
    for _ in $(seq 1 40); do
      if port_busy "$PORT_SSH"; then ready=1; break; fi
      if ! alive sshd; then break; fi
      sleep 0.25
    done
    if [ "$ready" = 1 ]; then
      echo "  sshd ready"
      env_put ZEDIS_IT_SSH "127.0.0.1:$PORT_SSH"
      env_put ZEDIS_IT_SSH_USER "$(id -un)"
      env_put ZEDIS_IT_SSH_KEY "$IT_DIR/ssh/id_ed25519"
      env_put ZEDIS_IT_SSH_KEY_ENC "$IT_DIR/ssh/id_ed25519_enc"
      env_put ZEDIS_IT_SSH_KEY_PASSPHRASE "$SSH_KEY_PASSPHRASE"
    else
      echo "!! sshd did not come up — skipping the scenario" >&2
      show_log sshd
    fi
  fi
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
      --cluster-node-timeout 5000 --cluster-announce-ip 127.0.0.1 --save "" --appendonly no --dir "$FS" \
      --requirepass "$DATA_PASSWORD" --masterauth "$DATA_PASSWORD"
    nodes="$nodes 127.0.0.1:$port"
  done
  for i in 0 1 2 3 4 5; do wait_pong "cluster-$((PORT_CLUSTER_BASE + i))" -p $((PORT_CLUSTER_BASE + i)) "${DATA_AUTH[@]}"; done
  # shellcheck disable=SC2086
  cli "${DATA_AUTH[@]}" --cluster create $nodes --cluster-replicas 1 --cluster-yes >/dev/null
  for _ in $(seq 1 60); do
    if cli -p "$PORT_CLUSTER_BASE" "${DATA_AUTH[@]}" cluster info 2>/dev/null | grep -q 'cluster_state:ok'; then break; fi
    sleep 0.5
  done
  cli -p "$PORT_CLUSTER_BASE" "${DATA_AUTH[@]}" cluster info | grep -q 'cluster_state:ok' || { echo "!! cluster never reached state ok" >&2; exit 1; }
  echo "  cluster ready"
  env_put ZEDIS_IT_CLUSTER "127.0.0.1:$PORT_CLUSTER_BASE"
fi

# The password the two protected topologies share; the tests attach it to
# every sentinel / cluster entry they register.
if has sentinel || has cluster; then env_put ZEDIS_IT_PASSWORD "$DATA_PASSWORD"; fi
[ "$STACK" = "1" ] && env_put ZEDIS_IT_STACK 1
cp "$IT_DIR/env" "$HERE/.env"
if [ -n "${GITHUB_ENV:-}" ]; then cat "$IT_DIR/env" >> "$GITHUB_ENV"; fi
echo "wrote $HERE/.env:"
sed 's/^/  /' "$HERE/.env"
