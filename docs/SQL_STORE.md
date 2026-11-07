[中文](./SQL_STORE_zh.md) | English · [← Back to README](../README.md)

# Zedis Web: PostgreSQL, MySQL and Redis storage

By default `zedis-bridge` — the server of the [web version](./WEB.md) — keeps everything in its data directory (`/data` in the image): one process, nothing else to run, and a backup is one directory to copy. Put its data in **PostgreSQL 14+**, **MySQL 8.0.19+** or **Redis / Valkey** instead when you want

- **more than one bridge**, behind a load balancer, or so that a bridge can go down without signing everyone out;
- **an audit you can search**: with a database it is on by default, kept for a year (90 days sliding TTL in Redis), and read on the account page.

The desktop app is not affected: it keeps its data on your machine. MariaDB, SQLite and other servers that only speak the MySQL or PostgreSQL protocol are not supported.

> **Which image.** The database store comes with the first release after v0.14.0. Until that release is out, use `vicanso/zedis-web:nightly` (built from `main`) wherever the commands below say `:latest`: v0.14.0 does not know `ZEDIS_BRIDGE_STORE`, ignores it, and stops at start with `no accounts are configured`.

- [Quick start with Docker](#quick-start-with-docker)
- [The connection URL](#the-connection-url)
- [The database and its user](#the-database-and-its-user)
- [What is kept where](#what-is-kept-where)
- [Several bridges](#several-bridges)
- [Moving an existing bridge](#moving-an-existing-bridge)
- [The audit](#the-audit)
- [Backups](#backups)
- [Upgrading](#upgrading)
- [When it does not start](#when-it-does-not-start)
- [The schema](#the-schema)

## Quick start with Docker

Both examples put the database and the bridge on one Docker network, where the bridge reaches the database by its container name. Replace every `change-me`. The bridge creates its tables on its first start; there is nothing to run by hand.

### PostgreSQL

```bash
docker network create zedis

docker run -d --name zedis-pg --network zedis --restart unless-stopped \
  -e POSTGRES_USER=zedis \
  -e POSTGRES_PASSWORD=change-me \
  -e POSTGRES_DB=zedis \
  -v zedis-pg:/var/lib/postgresql \
  postgres:18

# Wait until it takes connections over TCP (not only its first-start setup).
until docker exec zedis-pg pg_isready -h 127.0.0.1 -U zedis -d zedis; do sleep 1; done

docker run -d --name zedis-web --network zedis --restart unless-stopped \
  -p 127.0.0.1:7379:7379 \
  -v zedis-data:/data \
  -e ZEDIS_BRIDGE_STORE='postgres://zedis:change-me@zedis-pg:5432/zedis' \
  vicanso/zedis-web:latest

docker logs zedis-web 2>&1 | grep -E "stores are in a database|setup token"
```

From PostgreSQL 18 the image keeps its data under `/var/lib/postgresql`, which is what the volume mounts; for `postgres:14`–`17` mount `/var/lib/postgresql/data` instead.

### MySQL

```bash
docker network create zedis

docker run -d --name zedis-mysql --network zedis --restart unless-stopped \
  -e MYSQL_ROOT_PASSWORD=change-me-too \
  -e MYSQL_DATABASE=zedis \
  -e MYSQL_USER=zedis \
  -e MYSQL_PASSWORD=change-me \
  -v zedis-mysql:/var/lib/mysql \
  mysql:8.4

# Wait until it takes connections (not only its first-start setup).
until docker exec zedis-mysql mysqladmin ping -h 127.0.0.1 --silent; do sleep 1; done

docker run -d --name zedis-web --network zedis --restart unless-stopped \
  -p 127.0.0.1:7379:7379 \
  -v zedis-data:/data \
  -e ZEDIS_BRIDGE_STORE='mysql://zedis:change-me@zedis-mysql:3306/zedis' \
  vicanso/zedis-web:latest

docker logs zedis-web 2>&1 | grep -E "stores are in a database|setup token"
```

### Redis / Valkey

```bash
docker network create zedis

docker run -d --name zedis-redis --network zedis --restart unless-stopped \
  -v zedis-redis:/data \
  redis:8 redis-server --appendonly yes --requirepass change-me

docker run -d --name zedis-web --network zedis --restart unless-stopped \
  -p 127.0.0.1:7379:7379 \
  -v zedis-data:/data \
  -e ZEDIS_BRIDGE_STORE='redis://:change-me@zedis-redis:6379/0?prefix=zedis' \
  vicanso/zedis-web:latest

docker logs zedis-web 2>&1 | grep -E "stores are in Redis|setup token"
```

Then open <http://localhost:7379> and create the administrator with the setup token from the log, as in the [self-hosting guide](./WEB.md#try-it). For anything past a trial put HTTPS in front ([Deploy it](./WEB.md#deploy-it)).

A bridge that cannot reach its database when it starts stops with the reason in its log; `--restart unless-stopped` starts it again until the database is there.

### Docker Compose

The same with PostgreSQL, the bridge waiting for the database's health check:

```yaml
services:
  db:
    image: postgres:18
    restart: unless-stopped
    environment:
      POSTGRES_USER: zedis
      POSTGRES_PASSWORD: change-me
      POSTGRES_DB: zedis
    volumes:
      - zedis-pg:/var/lib/postgresql
    healthcheck:
      test: ["CMD", "pg_isready", "-h", "127.0.0.1", "-U", "zedis", "-d", "zedis"]
      interval: 2s
      retries: 30

  zedis-web:
    image: vicanso/zedis-web:latest
    restart: unless-stopped
    depends_on:
      db:
        condition: service_healthy
    ports:
      - "127.0.0.1:7379:7379"
    volumes:
      - zedis-data:/data
    environment:
      ZEDIS_BRIDGE_STORE: postgres://zedis:change-me@db:5432/zedis

volumes:
  zedis-pg:
  zedis-data:
```

For MySQL, the `db` service is `image: mysql:8.4` with `MYSQL_ROOT_PASSWORD`, `MYSQL_DATABASE`, `MYSQL_USER` and `MYSQL_PASSWORD`, the volume on `/var/lib/mysql`, the health check `["CMD", "mysqladmin", "ping", "-h", "127.0.0.1", "--silent"]`, and the URL `mysql://zedis:change-me@db:3306/zedis`.

## The connection URL

`ZEDIS_BRIDGE_STORE` (or `--store`) names the database:

| | PostgreSQL | MySQL |
|---|---|---|
| Form | `postgres://user:password@host:5432/database` | `mysql://user:password@host:3306/database` |
| TLS | `?sslmode=…` | `?ssl-mode=…` |
| A CA of your own | `&sslrootcert=/path/ca.pem` | `&ssl-ca=/path/ca.pem` |

- **Prefer the environment variable.** The URL carries the password, and a command-line flag is visible to anyone who can list the host's processes. The bridge only ever logs the URL's host.
- **Encode what a URL reserves.** A password with `@`, `:`, `/`, `?`, `#` or `%` is written percent-encoded: `p@ss` is `p%40ss`.
- **TLS.** PostgreSQL's `sslmode` is `disable`, `prefer` (the default: encrypted when the server offers it, nothing verified), `require` (encrypted, nothing verified), `verify-ca` (the certificate checked against trusted roots) or `verify-full` (the host name too). MySQL's `ssl-mode` is `DISABLED`, `PREFERRED` (the default), `REQUIRED`, `VERIFY_CA` or `VERIFY_IDENTITY`. Across a network you do not control, use `verify-full` / `VERIFY_IDENTITY`; inside one Docker network the defaults are fine.
- **Trusted roots.** The bridge trusts the public web certificate authorities (built in), not the host's certificate store. A database whose certificate comes from a CA of its own — self-signed, or a cloud provider's such as Amazon RDS's — needs that CA's file mounted into the container and named with `sslrootcert` / `ssl-ca`, for example `-v /etc/zedis/rds-ca.pem:/data/rds-ca.pem:ro` and `?sslmode=verify-full&sslrootcert=/data/rds-ca.pem`.
- **MySQL 8 signs in with `caching_sha2_password`.** Without TLS the bridge sends the password RSA-encrypted, but everything after it travels in the clear — ask for `REQUIRED` or better wherever the network is not yours.
- Each bridge keeps up to 10 connections to the database.

## The database and its user

Give the bridge a database of its own and a user that may create tables in it. The bridge creates its tables on its first start and adds what a new release needs on the first start of that release — every start checks, and sqlx's `_sqlx_migrations` table records which steps have run — so a user that may only read and write rows is not enough. The Docker images' `POSTGRES_USER` / `MYSQL_USER` above already are such users. On a server of your own:

PostgreSQL — the database owned by the bridge's user (from PostgreSQL 15 other users may not create tables in a database's `public` schema unless granted; its owner may):

```sql
CREATE ROLE zedis LOGIN PASSWORD 'change-me';
CREATE DATABASE zedis OWNER zedis;
```

MySQL — every privilege on its own database (at least `SELECT`, `INSERT`, `UPDATE`, `DELETE`, `CREATE`, `ALTER` and `INDEX`):

```sql
CREATE DATABASE zedis;
CREATE USER 'zedis'@'%' IDENTIFIED BY 'change-me';
GRANT ALL PRIVILEGES ON zedis.* TO 'zedis'@'%';
```

The tables choose their own character set and collation (`utf8mb4` with `utf8mb4_bin`, so `Alice` and `alice` are two accounts), whatever the database's default is. No server setting needs changing: a recycle-bin entry is at most about 1 MB, well under MySQL's default `max_allowed_packet`.

## What is kept where

In the database: the accounts, invitations, requests for an account and MCP tokens (only their hashes), the logins, the server list, the recycle bin, the Metrics history, the page's data (favorites, recent keys, search history, preferences, key tags and notes, saved Lua scripts), the setup token (its hash), the failed sign-ins and the audit.

Not in the database: **`master.key`**, in the bridge's data directory (`/data/master.key`). It encrypts the server entries' passwords and the recycle bin's values before they are written, so the database alone opens neither. Back it up, and keep it apart from the database's backups: one is no use without the other, and they should not travel together.

## Several bridges

Every bridge on one database is the same bridge to a browser: signed in on one is signed in on all, ten failed sign-ins are ten across all of them, and a change made on one — an account disabled, a server rule removed, a sign-out — reaches the others within two seconds. Three things to arrange, described in the [self-hosting guide](./WEB.md#several-bridges-on-one-database):

- **One `master.key` for every bridge.** The first bridge on an empty database seals a check value with its key; a bridge with another key refuses to start rather than write passwords the others cannot read. Copy the first one's key and mount it into every other, readable by the image's user (uid 65532):

  ```bash
  docker cp zedis-web:/data/master.key ./master.key
  sudo chown 65532:65532 master.key && chmod 600 master.key

  docker run -d --name zedis-web-2 --network zedis --restart unless-stopped \
    -p 127.0.0.1:7380:7379 \
    -v zedis-data-2:/data \
    -v "$PWD/master.key:/data/master.key:ro" \
    -e ZEDIS_BRIDGE_STORE='postgres://zedis:change-me@zedis-pg:5432/zedis' \
    vicanso/zedis-web:latest
  ```

- **Keep each browser on one bridge** by its `zedis_bridge` login cookie (sticky sessions on the load balancer): the terminal's session and the streaming panels are open Redis connections inside one bridge.
- **Health checks on `/v1/ready`**: `503` while that bridge's database does not answer within two seconds, so the load balancer sends the traffic to the others. `/v1/health` only says the process is up, for a container's liveness probe.
- **Synchronized clocks** (NTP) on every host.

## Moving an existing bridge

Stop it, then copy its data directory into the empty database once, with that directory's `master.key`, which every bridge on the database then uses:

```bash
docker stop zedis-web
docker run --rm --network zedis -v zedis-data:/data \
  -e ZEDIS_BRIDGE_STORE='postgres://zedis:change-me@zedis-pg:5432/zedis' \
  vicanso/zedis-web:latest import-local --audit-file /data/audit.log
```

It copies the accounts, invitations and waiting requests, the logins (nobody signs in again), the server list, the saved scripts, the page's data, the recycle bin and the Metrics history as they were, and a waiting setup token; `--audit-file` adds the lines of an audit file. It refuses a database that already holds accounts, server entries or scripts. Then replace the bridge with one started as in the [quick start](#quick-start-with-docker), on the same `/data` volume — `docker rm zedis-web`, then `docker run` with `ZEDIS_BRIDGE_STORE` set. There is no way back but the database's own backups.

## The audit

With a database the audit is on unless told otherwise, kept in the `audit` table and read by administrators on the account page (**Account…** → *Audit*) or through `GET /v1/admin/audit`. Lines older than a year are removed; `--audit-retention-days` (`ZEDIS_BRIDGE_AUDIT_RETENTION_DAYS`) changes that, and `0` keeps them all. `--audit-log off` turns the audit off, and `--audit-log <file>` writes a file instead — each bridge its own. What a line records is in the [self-hosting guide](./WEB.md#audit-log).

## Backups

The database with its own tools, and `master.key` apart from it:

```bash
# PostgreSQL
docker exec zedis-pg pg_dump -U zedis -d zedis -Fc > zedis-$(date +%F).dump
# MySQL (--no-tablespaces: a user without the PROCESS privilege needs it)
docker exec zedis-mysql mysqldump -u zedis -pchange-me --single-transaction --no-tablespaces zedis > zedis-$(date +%F).sql
```

A restore goes into an empty database; then start the bridges with the same `master.key`.

## Upgrading

A release may add to the schema. The first bridge of the new release to start adds it, under the database's own lock (`pg_advisory_lock` / `GET_LOCK`), so bridges starting together do it once. A bridge of an older release then refuses to start on that database — it does not know the new step — so upgrade every bridge together, and take a backup first: going back means restoring it. A step only ever adds; nothing a release before it wrote is changed. A step cut short — the bridge stopped, or the database went away, in the middle of one; MySQL cannot take back the half that ran — is run again at the next start, with a warning in the log: every statement of a step is safe to run twice.

## When it does not start

| The log says | Because |
|---|---|
| `this bridge's master.key is not the one the store's other bridges use` | It has another key, or cannot read the mounted one (it must be readable by uid 65532). Mount the key every other bridge uses. |
| `the store's schema could not be brought up to date: … permission denied …` | Its user may not create tables in that database. See [The database and its user](#the-database-and-its-user). |
| `… was previously applied but is missing in the resolved migrations` | A bridge older than the schema: a newer release has already run on this database. Upgrade it. |
| `… was previously applied but has been modified` | A bridge built from changed schema files — not a release. Released steps never change; build from a release tag. |
| An error naming the URL's host, at start | The database is not reachable yet, or the URL is wrong. The log shows the host only. |
| `import-local`: `the database already holds accounts, server entries or scripts` | `import-local` copies into an empty database only. |

## The schema

You do not run these: the bridge does, on its first start. They are here to review, for a DBA who prepares the database, and to show what a backup holds. They are the files under `zedis-bridge/migrations/`, as they are; every statement is safe to run again (`IF NOT EXISTS`), so running them by hand first does no harm — the bridge still records them in `_sqlx_migrations` on its first start, which is why its user still needs to be able to create tables.

| Table | Holds |
|---|---|
| `store_versions` | A counter per kind of data each bridge keeps in memory, polled every 2 s to stay in step |
| `store_meta` | The master key's check value |
| `accounts` | One row per account: its role, server rules, password hash, MCP token hashes |
| `invites` | Open invitations (the hash of their code) |
| `signups` | Requests for an account waiting for an administrator |
| `servers` | The server list; passwords sealed with `master.key` |
| `logins` | Browser logins (the hash of their cookie) |
| `throttle` | Failed sign-ins per address, shared by every bridge |
| `setup_token` | At most one row: the waiting setup token's hash |
| `trash_bins`, `trash`, `trash_payloads` | The recycle bin: a row per server, each entry, its sealed value apart |
| `metrics` | The Metrics history: one sample a minute per server |
| `page_rows` | Each account's favorites, recent keys, search history and preferences |
| `key_notes` | Key tags and notes, per server |
| `scripts` | Saved Lua scripts |
| `audit` | The audit |

### PostgreSQL

`zedis-bridge/migrations/postgres/0001_stores.sql`:

```sql
-- The bridge's stores in PostgreSQL (ADR 18). Every statement runs again
-- safely: a migration that stopped halfway is finished by running it again.
--
-- Records are documents (`doc`, the record's JSON) beside the columns a
-- statement needs; times are unix milliseconds unless named `_secs`.

-- The count of changes to each set a process keeps in memory.
CREATE TABLE IF NOT EXISTS store_versions (
    name TEXT PRIMARY KEY,
    version BIGINT NOT NULL
);
INSERT INTO store_versions (name, version)
VALUES ('servers', 0), ('accounts', 0), ('logins_revoked', 0)
ON CONFLICT (name) DO NOTHING;

-- What the store says about itself: the master key's check value.
CREATE TABLE IF NOT EXISTS store_meta (
    name TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS accounts (
    name TEXT PRIMARY KEY,
    doc TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS invites (
    id TEXT PRIMARY KEY,
    expires_at_secs BIGINT NOT NULL,
    doc TEXT NOT NULL
);

-- Secrets in `doc` are sealed with the master key.
CREATE TABLE IF NOT EXISTS servers (
    id TEXT PRIMARY KEY,
    owner TEXT NULL,
    version BIGINT NOT NULL,
    doc TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS logins (
    login_key TEXT PRIMARY KEY,
    account TEXT NOT NULL,
    last_used_secs BIGINT NOT NULL,
    doc TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS throttle (
    id BIGSERIAL PRIMARY KEY,
    process_id TEXT NOT NULL,
    key_sha256 TEXT NOT NULL,
    attempt_key TEXT NOT NULL,
    at_ms BIGINT NOT NULL
);
CREATE INDEX IF NOT EXISTS throttle_key ON throttle (key_sha256);
CREATE INDEX IF NOT EXISTS throttle_at ON throttle (at_ms);

-- One row at most: the waiting token's hash.
CREATE TABLE IF NOT EXISTS setup_token (
    id INTEGER PRIMARY KEY,
    token_sha256 TEXT NOT NULL,
    created_at_secs BIGINT NOT NULL
);

-- A row per server whose bin is in use: what a put locks, so two
-- processes making room in one bin do it one after the other.
CREATE TABLE IF NOT EXISTS trash_bins (
    server_id TEXT PRIMARY KEY
);
CREATE TABLE IF NOT EXISTS trash (
    server_id TEXT NOT NULL,
    id TEXT NOT NULL,
    deleted_at BIGINT NOT NULL,
    size_bytes BIGINT NOT NULL,
    meta TEXT NOT NULL,
    PRIMARY KEY (server_id, id)
);
CREATE INDEX IF NOT EXISTS trash_deleted_at ON trash (deleted_at);
-- The payloads apart, so a listing never reads one (ADR 17's blobs).
CREATE TABLE IF NOT EXISTS trash_payloads (
    server_id TEXT NOT NULL,
    id TEXT NOT NULL,
    payload BYTEA NOT NULL,
    PRIMARY KEY (server_id, id)
);

-- One sample a minute per server: the key says so.
CREATE TABLE IF NOT EXISTS metrics (
    server_id TEXT NOT NULL,
    minute BIGINT NOT NULL,
    ts_ms BIGINT NOT NULL,
    sample TEXT NOT NULL,
    PRIMARY KEY (server_id, minute)
);
CREATE INDEX IF NOT EXISTS metrics_ts ON metrics (ts_ms);

CREATE TABLE IF NOT EXISTS page_rows (
    account TEXT NOT NULL,
    row_table TEXT NOT NULL,
    row_key TEXT NOT NULL,
    row_value TEXT NOT NULL,
    PRIMARY KEY (account, row_table, row_key)
);

-- Keyed by the Redis key's SHA-256: a key name can be longer than an
-- index entry may be.
CREATE TABLE IF NOT EXISTS key_notes (
    server_id TEXT NOT NULL,
    key_sha256 TEXT NOT NULL,
    redis_key TEXT NOT NULL,
    doc TEXT NOT NULL,
    PRIMARY KEY (server_id, key_sha256)
);

CREATE TABLE IF NOT EXISTS scripts (
    id TEXT PRIMARY KEY,
    owner TEXT NULL,
    doc TEXT NOT NULL
);
```

`zedis-bridge/migrations/postgres/0002_audit.sql`:

```sql
-- The audit in the database (ADR 18): every bridge's lines in one table,
-- looked up by time, account, event and server. `line` is the JSON object
-- the file would have held.
CREATE TABLE IF NOT EXISTS audit (
    id BIGSERIAL PRIMARY KEY,
    at_ms BIGINT NOT NULL,
    account TEXT NOT NULL,
    event TEXT NOT NULL,
    server_id TEXT NULL,
    line TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS audit_at ON audit (at_ms);
CREATE INDEX IF NOT EXISTS audit_account ON audit (account, id);
CREATE INDEX IF NOT EXISTS audit_event ON audit (event, id);
CREATE INDEX IF NOT EXISTS audit_server ON audit (server_id, id);
```

`zedis-bridge/migrations/postgres/0003_signups.sql`:

```sql
-- Requests for an account, waiting for an administrator (ADR 17, amended
-- 2026-10-18): records beside the accounts and the invitations, changed
-- under the same `accounts` counter.
CREATE TABLE IF NOT EXISTS signups (
    id TEXT PRIMARY KEY,
    expires_at_secs BIGINT NOT NULL,
    doc TEXT NOT NULL
);
```

### MySQL

`zedis-bridge/migrations/mysql/0001_stores.sql`:

```sql
-- The bridge's stores in MySQL 8.0.19 and later (ADR 18). Every statement
-- runs again safely: MySQL does not run DDL in a transaction, and a
-- migration that stopped halfway is finished by running it again.
--
-- Every table is utf8mb4 with the binary collation: MySQL's default
-- (utf8mb4_0900_ai_ci) ignores case and accents, which would make `Alice`
-- and `alice` one account. Records are documents (`doc`, the record's
-- JSON) beside the columns a statement needs; times are unix milliseconds
-- unless named `_secs`. Column sizes follow what the bridge accepts: an
-- account name is at most 64 characters, an id 128, a row key 512.

CREATE TABLE IF NOT EXISTS store_versions (
    name VARCHAR(64) NOT NULL PRIMARY KEY,
    version BIGINT NOT NULL
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_bin;
INSERT IGNORE INTO store_versions (name, version)
VALUES ('servers', 0), ('accounts', 0), ('logins_revoked', 0);

CREATE TABLE IF NOT EXISTS store_meta (
    name VARCHAR(64) NOT NULL PRIMARY KEY,
    value MEDIUMTEXT NOT NULL
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_bin;

CREATE TABLE IF NOT EXISTS accounts (
    name VARCHAR(64) NOT NULL PRIMARY KEY,
    doc MEDIUMTEXT NOT NULL
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_bin;

CREATE TABLE IF NOT EXISTS invites (
    id VARCHAR(64) NOT NULL PRIMARY KEY,
    expires_at_secs BIGINT NOT NULL,
    doc MEDIUMTEXT NOT NULL
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_bin;

CREATE TABLE IF NOT EXISTS servers (
    id VARCHAR(191) NOT NULL PRIMARY KEY,
    owner VARCHAR(64) NULL,
    version BIGINT NOT NULL,
    doc MEDIUMTEXT NOT NULL
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_bin;

CREATE TABLE IF NOT EXISTS logins (
    login_key VARCHAR(128) NOT NULL PRIMARY KEY,
    account VARCHAR(64) NOT NULL,
    last_used_secs BIGINT NOT NULL,
    doc MEDIUMTEXT NOT NULL
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_bin;

CREATE TABLE IF NOT EXISTS throttle (
    id BIGINT NOT NULL AUTO_INCREMENT PRIMARY KEY,
    process_id VARCHAR(64) NOT NULL,
    key_sha256 VARCHAR(64) NOT NULL,
    attempt_key MEDIUMTEXT NOT NULL,
    at_ms BIGINT NOT NULL,
    INDEX throttle_key (key_sha256),
    INDEX throttle_at (at_ms)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_bin;

CREATE TABLE IF NOT EXISTS setup_token (
    id INT NOT NULL PRIMARY KEY,
    token_sha256 VARCHAR(64) NOT NULL,
    created_at_secs BIGINT NOT NULL
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_bin;

CREATE TABLE IF NOT EXISTS trash_bins (
    server_id VARCHAR(191) NOT NULL PRIMARY KEY
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_bin;
CREATE TABLE IF NOT EXISTS trash (
    server_id VARCHAR(191) NOT NULL,
    id VARCHAR(64) NOT NULL,
    deleted_at BIGINT NOT NULL,
    size_bytes BIGINT NOT NULL,
    meta MEDIUMTEXT NOT NULL,
    PRIMARY KEY (server_id, id),
    INDEX trash_deleted_at (deleted_at)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_bin;
-- A payload is up to 1 MB and its seal; BLOB stops at 64 KB.
CREATE TABLE IF NOT EXISTS trash_payloads (
    server_id VARCHAR(191) NOT NULL,
    id VARCHAR(64) NOT NULL,
    payload LONGBLOB NOT NULL,
    PRIMARY KEY (server_id, id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_bin;

CREATE TABLE IF NOT EXISTS metrics (
    server_id VARCHAR(191) NOT NULL,
    minute BIGINT NOT NULL,
    ts_ms BIGINT NOT NULL,
    sample MEDIUMTEXT NOT NULL,
    PRIMARY KEY (server_id, minute),
    INDEX metrics_ts (ts_ms)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_bin;

CREATE TABLE IF NOT EXISTS page_rows (
    account VARCHAR(64) NOT NULL,
    row_table VARCHAR(32) NOT NULL,
    row_key VARCHAR(512) NOT NULL,
    row_value MEDIUMTEXT NOT NULL,
    PRIMARY KEY (account, row_table, row_key)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_bin;

CREATE TABLE IF NOT EXISTS key_notes (
    server_id VARCHAR(191) NOT NULL,
    key_sha256 VARCHAR(64) NOT NULL,
    redis_key MEDIUMTEXT NOT NULL,
    doc MEDIUMTEXT NOT NULL,
    PRIMARY KEY (server_id, key_sha256)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_bin;

CREATE TABLE IF NOT EXISTS scripts (
    id VARCHAR(191) NOT NULL PRIMARY KEY,
    owner VARCHAR(64) NULL,
    doc MEDIUMTEXT NOT NULL
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_bin;
```

`zedis-bridge/migrations/mysql/0002_audit.sql`:

```sql
-- The audit in the database (ADR 18): every bridge's lines in one table,
-- looked up by time, account, event and server. `line` is the JSON object
-- the file would have held. An account is at most 64 characters; a name
-- someone tried, which the audit also files, is cut to the same.
CREATE TABLE IF NOT EXISTS audit (
    id BIGINT NOT NULL AUTO_INCREMENT PRIMARY KEY,
    at_ms BIGINT NOT NULL,
    account VARCHAR(255) NOT NULL,
    event VARCHAR(64) NOT NULL,
    server_id VARCHAR(191) NULL,
    line MEDIUMTEXT NOT NULL,
    INDEX audit_at (at_ms),
    INDEX audit_account (account, id),
    INDEX audit_event (event, id),
    INDEX audit_server (server_id, id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_bin;
```

`zedis-bridge/migrations/mysql/0003_signups.sql`:

```sql
-- Requests for an account, waiting for an administrator (ADR 17, amended
-- 2026-10-18): records beside the accounts and the invitations, changed
-- under the same `accounts` counter.
CREATE TABLE IF NOT EXISTS signups (
    id VARCHAR(64) NOT NULL PRIMARY KEY,
    expires_at_secs BIGINT NOT NULL,
    doc MEDIUMTEXT NOT NULL
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_bin;
```
