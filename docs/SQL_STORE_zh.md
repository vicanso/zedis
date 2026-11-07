中文 | [English](./SQL_STORE.md) · [← 返回 README](../README_zh.md)

# Zedis Web 版：PostgreSQL、MySQL 与 Redis 存储

`zedis-bridge` —— [Web 版](./WEB_zh.md)的服务端 —— 默认把所有数据存在它的数据目录里（镜像中是 `/data`）：只有一个进程，不用额外部署任何东西，备份就是复制一个目录。下面两种情况，改用 **PostgreSQL 14+**、**MySQL 8.0.19+** 或 **Redis / Valkey** 存储：

- **要运行多个 bridge**：放在负载均衡之后，或者希望某个 bridge 挂掉时不会让所有人都要重新登录；
- **要能查询的审计**：使用数据库时审计默认开启，保留一年（Redis 中为 90 天滑动 TTL），可以在账号页查看。

桌面应用不受影响：它的数据保存在本机。不支持 MariaDB、SQLite，也不支持其他只是兼容 MySQL 或 PostgreSQL 协议的服务。

> **用哪个镜像。** 数据库存储从 v0.14.0 之后的第一个正式版本开始提供。在它发布之前，请把下面命令里的 `:latest` 换成 `vicanso/zedis-web:nightly`（由 `main` 构建）：v0.14.0 不认识 `ZEDIS_BRIDGE_STORE`，会忽略它，并在启动时以 `no accounts are configured` 退出。

- [用 Docker 快速开始](#用-docker-快速开始)
- [连接 URL](#连接-url)
- [数据库与用户](#数据库与用户)
- [哪些数据存在哪里](#哪些数据存在哪里)
- [多个 bridge](#多个-bridge)
- [迁移已有的 bridge](#迁移已有的-bridge)
- [审计](#审计)
- [备份](#备份)
- [升级](#升级)
- [启动失败时](#启动失败时)
- [表结构](#表结构)

## 用 Docker 快速开始

两个例子都把数据库和 bridge 放在同一个 Docker 网络里，bridge 用容器名访问数据库。把所有 `change-me` 换成你自己的密码。bridge 第一次启动时会自己建表，不需要手动执行任何 SQL。

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

从 PostgreSQL 18 起，官方镜像把数据放在 `/var/lib/postgresql` 下，所以卷挂在这里；`postgres:14`–`17` 请改挂 `/var/lib/postgresql/data`。

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

然后打开 <http://localhost:7379>，用日志里的初始化令牌创建管理员，和[自托管指南](./WEB_zh.md#快速试用)里一样。试用之外的场景请在前面加 HTTPS（见[部署](./WEB_zh.md#正式部署)）。

bridge 启动时如果连不上数据库，会退出并在日志里写明原因；`--restart unless-stopped` 会一直重新启动它，直到数据库就绪。

### Docker Compose

同样的 PostgreSQL 部署，bridge 等数据库健康检查通过后才启动：

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

换成 MySQL 时，`db` 服务改为 `image: mysql:8.4`，环境变量用 `MYSQL_ROOT_PASSWORD`、`MYSQL_DATABASE`、`MYSQL_USER`、`MYSQL_PASSWORD`，卷挂到 `/var/lib/mysql`，健康检查用 `["CMD", "mysqladmin", "ping", "-h", "127.0.0.1", "--silent"]`，URL 为 `mysql://zedis:change-me@db:3306/zedis`。

## 连接 URL

`ZEDIS_BRIDGE_STORE`（或 `--store`）指定数据库：

| | PostgreSQL | MySQL |
|---|---|---|
| 格式 | `postgres://user:password@host:5432/database` | `mysql://user:password@host:3306/database` |
| TLS | `?sslmode=…` | `?ssl-mode=…` |
| 自有 CA | `&sslrootcert=/path/ca.pem` | `&ssl-ca=/path/ca.pem` |

- **优先用环境变量。** URL 里带着密码，而命令行参数能被主机上任何能列出进程的人看到。bridge 的日志只会显示 URL 里的主机名。
- **URL 保留字符要转义。** 密码里有 `@`、`:`、`/`、`?`、`#` 或 `%` 时要写成百分号编码：`p@ss` 写作 `p%40ss`。
- **TLS。** PostgreSQL 的 `sslmode` 可选 `disable`、`prefer`（默认：服务端支持就加密，但不做任何校验）、`require`（加密，不校验）、`verify-ca`（用受信任的根证书校验证书）、`verify-full`（再校验主机名）。MySQL 的 `ssl-mode` 可选 `DISABLED`、`PREFERRED`（默认）、`REQUIRED`、`VERIFY_CA`、`VERIFY_IDENTITY`。经过不受你控制的网络时，请用 `verify-full` / `VERIFY_IDENTITY`；在同一个 Docker 网络里用默认值即可。
- **受信任的根证书。** bridge 信任的是内置的公共 Web 证书颁发机构，而不是宿主机的证书库。如果数据库证书由自有 CA 签发 —— 自签名，或者云厂商自己的 CA（例如 Amazon RDS）—— 需要把 CA 文件挂进容器，并用 `sslrootcert` / `ssl-ca` 指定，例如 `-v /etc/zedis/rds-ca.pem:/data/rds-ca.pem:ro` 加上 `?sslmode=verify-full&sslrootcert=/data/rds-ca.pem`。
- **MySQL 8 用 `caching_sha2_password` 认证。** 不开 TLS 时，bridge 会用 RSA 加密发送密码，但之后的所有数据都是明文 —— 网络不归你管的地方，请至少用 `REQUIRED`。
- 每个 bridge 最多保持 10 个数据库连接。

## 数据库与用户

给 bridge 一个专用的数据库，以及一个能在其中建表的用户。bridge 第一次启动时建表，新版本需要的表也在该版本第一次启动时补上 —— 每次启动都会检查，sqlx 的 `_sqlx_migrations` 表记录已经执行过哪些步骤 —— 所以只能读写数据行的用户是不够的。上面 Docker 镜像里的 `POSTGRES_USER` / `MYSQL_USER` 已经满足这个要求。在自己的数据库服务上：

PostgreSQL —— 数据库的所有者设为 bridge 的用户（从 PostgreSQL 15 起，除非另行授权，其他用户不能在数据库的 `public` schema 里建表，所有者可以）：

```sql
CREATE ROLE zedis LOGIN PASSWORD 'change-me';
CREATE DATABASE zedis OWNER zedis;
```

MySQL —— 授予它对专用数据库的全部权限（至少需要 `SELECT`、`INSERT`、`UPDATE`、`DELETE`、`CREATE`、`ALTER` 和 `INDEX`）：

```sql
CREATE DATABASE zedis;
CREATE USER 'zedis'@'%' IDENTIFIED BY 'change-me';
GRANT ALL PRIVILEGES ON zedis.* TO 'zedis'@'%';
```

表的字符集和排序规则由建表语句自己指定（`utf8mb4` 加 `utf8mb4_bin`，所以 `Alice` 和 `alice` 是两个账号），与数据库的默认值无关。不需要修改任何服务端配置：回收站的一条记录最多约 1 MB，远小于 MySQL 默认的 `max_allowed_packet`。

## 哪些数据存在哪里

存在数据库里的：账号、邀请、账号申请和 MCP 令牌（只存哈希），登录状态，服务器列表，回收站，指标历史，页面数据（收藏、最近打开的 key、搜索历史、偏好设置、key 的标签与备注、保存的 Lua 脚本），初始化令牌（只存哈希），登录失败记录，以及审计。

不在数据库里的：**`master.key`**，在 bridge 的数据目录里（`/data/master.key`）。服务器条目的密码和回收站里的值在写入前都用它加密，所以只拿到数据库是解不开它们的。请备份它，并且和数据库的备份分开存放：缺了任何一个，另一个都没用，也不应该放在一起传输。

## 多个 bridge

连同一个数据库的所有 bridge，在浏览器看来就是同一个 bridge：在一个上登录，所有 bridge 都算已登录；登录失败 10 次是所有 bridge 合计 10 次；在一个 bridge 上做的变更 —— 停用账号、删掉一条服务器规则、退出登录 —— 两秒内传到其他 bridge。需要安排好三件事，详见[自托管指南](./WEB_zh.md#多个-bridge-共用一个数据库)：

- **所有 bridge 用同一个 `master.key`。** 第一个连上空数据库的 bridge 会用它的密钥写入一个校验值；密钥不同的 bridge 会拒绝启动，而不是写入其他 bridge 读不出来的密码。把第一个 bridge 的密钥复制出来，挂进其他每个 bridge，并保证镜像里的用户（uid 65532）能读：

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

- **每个浏览器固定落在一个 bridge 上**，按登录 cookie `zedis_bridge` 做会话保持（负载均衡的 sticky session）：终端会话和各个实时面板是开在某一个 bridge 里的 Redis 连接。
- **健康检查指向 `/v1/ready`**：这个 bridge 的数据库两秒内没有响应时返回 `503`，负载均衡就会把流量转给其他 bridge。`/v1/health` 只表示进程还活着，用作容器的存活探针。
- **各台主机时钟同步**（NTP）。

## 迁移已有的 bridge

先停掉它，再把它的数据目录一次性复制进空数据库。复制时用的是这个目录里的 `master.key`，之后连这个数据库的所有 bridge 都要用它：

```bash
docker stop zedis-web
docker run --rm --network zedis -v zedis-data:/data \
  -e ZEDIS_BRIDGE_STORE='postgres://zedis:change-me@zedis-pg:5432/zedis' \
  vicanso/zedis-web:latest import-local --audit-file /data/audit.log
```

它会原样复制账号、邀请和待处理的申请，登录状态（不用重新登录），服务器列表，保存的脚本，页面数据，回收站和指标历史，以及等待中的初始化令牌；加上 `--audit-file` 还会导入审计文件里的记录。数据库里已经有账号、服务器条目或脚本时，它会拒绝执行。完成后，按[快速开始](#用-docker-快速开始)重新创建 bridge 容器，沿用同一个 `/data` 卷 —— 先 `docker rm zedis-web`，再带上 `ZEDIS_BRIDGE_STORE` 执行 `docker run`。迁移后想回退，只能靠数据库自己的备份。

## 审计

使用数据库时审计默认开启，存在 `audit` 表里，管理员可以在账号页（**账号…** → *审计*）或通过 `GET /v1/admin/audit` 查看。超过一年的记录会被删除；用 `--audit-retention-days`（`ZEDIS_BRIDGE_AUDIT_RETENTION_DAYS`）修改保留天数，设为 `0` 表示永久保留。`--audit-log off` 关闭审计，`--audit-log <文件>` 改为写文件 —— 每个 bridge 各写各的。每行记录的内容见[自托管指南](./WEB_zh.md#审计日志)。

## 备份

数据库用它自己的工具备份，`master.key` 单独备份：

```bash
# PostgreSQL
docker exec zedis-pg pg_dump -U zedis -d zedis -Fc > zedis-$(date +%F).dump
# MySQL (--no-tablespaces: a user without the PROCESS privilege needs it)
docker exec zedis-mysql mysqldump -u zedis -pchange-me --single-transaction --no-tablespaces zedis > zedis-$(date +%F).sql
```

恢复时导入一个空数据库，再用同一个 `master.key` 启动各个 bridge。

## 升级

新版本可能会给表结构增加内容。新版本第一个启动的 bridge 会在数据库自己的锁（`pg_advisory_lock` / `GET_LOCK`）之下完成这一步，所以同时启动的多个 bridge 只会执行一次。之后，旧版本的 bridge 在这个数据库上会拒绝启动 —— 它不认识新的步骤 —— 所以请同时升级所有 bridge，并且先做备份：想回退就只能从备份恢复。每一步都只会新增，不会改动旧版本写入的任何东西。如果某一步执行到一半被打断 —— bridge 被停掉，或数据库连接断了；MySQL 无法撤回已经执行的那一半 —— 下次启动时会重新执行这一步，并在日志里记一条警告：每一步里的每条语句都可以安全地执行两次。

## 启动失败时

| 日志里写的是 | 原因 |
|---|---|
| `this bridge's master.key is not the one the store's other bridges use` | 它用的是另一个密钥，或者读不到挂进来的密钥文件（必须能被 uid 65532 读取）。挂入其他 bridge 用的那个密钥。 |
| `the store's schema could not be brought up to date: … permission denied …` | 它的用户不能在这个数据库里建表。见[数据库与用户](#数据库与用户)。 |
| `… was previously applied but is missing in the resolved migrations` | bridge 比表结构旧：更新的版本已经在这个数据库上运行过。请升级它。 |
| `… was previously applied but has been modified` | bridge 是用改动过的表结构文件构建的 —— 不是正式发布版本。已发布的步骤永远不会改动；请从发布的 tag 构建。 |
| 启动时报错，并提到 URL 中的主机名 | 数据库还连不上，或者 URL 写错了。日志里只显示主机名。 |
| `import-local`：`the database already holds accounts, server entries or scripts` | `import-local` 只能复制进空数据库。 |

## 表结构

这些语句不需要你执行：bridge 第一次启动时会自己执行。列在这里是为了方便审阅、让 DBA 预先准备数据库，以及说明备份里有什么。它们就是 `zedis-bridge/migrations/` 下的文件原文；每条语句都可以重复执行（`IF NOT EXISTS`），所以先手动执行一遍也没有坏处 —— bridge 第一次启动时仍会把它们记入 `_sqlx_migrations`，所以它的用户仍然需要建表权限。

| 表 | 内容 |
|---|---|
| `store_versions` | 每类在各 bridge 内存中缓存的数据各有一个计数器，每 2 秒轮询一次以保持同步 |
| `store_meta` | master key 的校验值 |
| `accounts` | 每个账号一行：角色、服务器规则、密码哈希、MCP 令牌哈希 |
| `invites` | 未使用的邀请（只存邀请码的哈希） |
| `signups` | 等待管理员处理的账号申请 |
| `servers` | 服务器列表；密码用 `master.key` 加密 |
| `logins` | 浏览器登录（只存 cookie 的哈希） |
| `throttle` | 按地址记录的登录失败，所有 bridge 共享 |
| `setup_token` | 最多一行：等待中的初始化令牌的哈希 |
| `trash_bins`、`trash`、`trash_payloads` | 回收站：每个服务器一行、每条记录、以及单独存放的加密后的值 |
| `metrics` | 指标历史：每个服务器每分钟一个采样 |
| `page_rows` | 每个账号的收藏、最近打开的 key、搜索历史和偏好设置 |
| `key_notes` | 按服务器存放的 key 标签与备注 |
| `scripts` | 保存的 Lua 脚本 |
| `audit` | 审计 |

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
