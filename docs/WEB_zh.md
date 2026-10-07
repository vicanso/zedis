中文 | [English](./WEB.md) · [← 返回 README](../README_zh.md)

# Zedis Web 版：自托管指南

Zedis 也能在浏览器里运行：同一套代码编译成 WebAssembly，用 canvas 渲染。浏览器无法直接建立 TCP 连接，所以由一个很小的 HTTP 服务 —— `zedis-bridge` —— 同时提供页面，并代替浏览器与 Redis 通信。在 Redis 旁边部署一次，整个团队打开浏览器就能用，无需安装任何东西。Redis 的密码只保存在 bridge 上（加密存储），不会发送给浏览器。

<p align="center">
  <img src="images/architecture.svg" width="100%" alt="代码复用示意图（不是部署图）：Zedis 的界面和 Redis 连接层是编译进多个程序的同一份代码，都不是独立运行的服务。桌面应用是一个原生进程，两者都在其中，界面在进程内直接调用连接层。Web 版里，同一套界面以 WebAssembly 形式运行在浏览器标签页中，通过 HTTP API 访问 zedis-bridge；bridge 是另一个服务进程，用同一套连接层把这些请求转换成 Redis 命令。桌面应用和 bridge 各自以 RESP 连接单机、哨兵与集群部署的 Redis 和 Valkey。">
</p>

> **早期预览。** 镜像已发布 linux/amd64 与 linux/arm64 两个架构（约 26 MB）：正式发布对应 `:latest` 与版本号标签，`:nightly` 是跟随 `main` 的滚动构建。

## 快速试用

```bash
docker run -d --name zedis-web -p 7379:7379 \
  -e ZEDIS_BRIDGE_USERS="admin@change-me" \
  -v zedis-data:/data \
  vicanso/zedis-web:latest --listen 0.0.0.0:7379 --insecure-cookie
```

打开 <http://localhost:7379>，用 `admin` / `change-me` 登录。

- **必须配置账号** —— 要么 `ZEDIS_BRIDGE_USERS="用户名@密码,用户名2@密码2"`，要么 `--users-file`（见[账号与共享](#账号与共享)）。两者都不给，bridge 拒绝启动；两者都给，同样拒绝启动。行内写法按第一个 `@` 切分，因此用户名不能包含 `@` 或 `:`，密码不能包含逗号。脚本可以用 HTTP Basic（`curl -u admin:change-me …/v1/servers`）。
- **`/data`** 保存服务器列表（其中的密码由同目录的 `master.key` 加密）和已保存的登录状态。请保留这个卷，否则每次重启都是空的。
- **`--insecure-cookie` 仅用于纯 http 的试用。** 登录 cookie 默认带 `Secure`，而浏览器会静默丢弃通过纯 http 收到的 `Secure` cookie —— 只有 `localhost` 例外（部分浏览器连 `localhost` 也不例外）。不加这个参数时，现象是登录成功、紧接着的请求返回 `401`。
- **Redis 跑在 Docker 宿主机上**时，容器内的 `127.0.0.1` 指的是容器自己。请使用 `host.docker.internal`（Linux 上需加 `--add-host=host.docker.internal:host-gateway`）或 `--network host`。

## 正式部署

纯 http 下，账号密码以及从 Redis 读到的所有数据都是明文传输。除试用外，请去掉 `--insecure-cookie`，端口只发布到回环地址，并在前面放一个 HTTPS 反向代理：

```bash
docker run -d --name zedis-web -p 127.0.0.1:7379:7379 \
  -e ZEDIS_BRIDGE_USERS="alice@…,bob@…" \
  -v zedis-data:/data \
  vicanso/zedis-web:latest
```

```caddyfile
zedis.example.com {
    reverse_proxy 127.0.0.1:7379
}
```

账号密码可以被猜测：如果 bridge 能从内网之外访问，请在代理层加上限流。

## 与其它项目共用域名

如果域名不是 Zedis 独占的，可以用 `ZEDIS_BRIDGE_BASE_PATH`（或 `--base-path`）给 bridge 指定一个专属路径。页面、静态资源和 API 会全部移到这个前缀之下，前缀之外的路径一律不响应；登录 cookie 的作用范围也限定在这个前缀内 —— 同域名下的其它项目不会收到它。

```bash
docker run -d --name zedis-web -p 127.0.0.1:7379:7379 \
  -e ZEDIS_BRIDGE_USERS="alice@…,bob@…" \
  -e ZEDIS_BRIDGE_BASE_PATH=/zedis \
  -v zedis-data:/data \
  vicanso/zedis-web:latest
```

```caddyfile
tools.example.com {
    # 用 `handle` 而不是 `handle_path`：前缀要原样转发，不能剥掉。
    handle /zedis* {
        reverse_proxy 127.0.0.1:7379
    }
    # … 其它项目
}
```

访问 `https://tools.example.com/zedis/`。nginx 的等价写法是 `location /zedis { proxy_pass http://127.0.0.1:7379; }` —— `proxy_pass` 末尾不要加斜杠，否则前缀会被剥掉。健康检查地址也随之变为 `/zedis/v1/health`。

## 账号与共享

服务器条目属于添加它的账号，其他人看不到；勾选 **Shared** 后则对所有账号可见。能看到共享条目的账号都可以编辑或删除它。

账号来自两处之一，不能同时使用：

```bash
# 行内：名字后加 ":ro" 即为只读账号
-e ZEDIS_BRIDGE_USERS="alice@secret,bob:ro@hunter2"
```

```toml
# 或者用文件：--users-file /data/users.toml（ZEDIS_BRIDGE_USERS_FILE）
[[users]]
name = "alice"
password = "secret"

[[users]]
name = "bob"
password = "hunter2"
read_only = true

[[users]]
name = "carol"
password = "s3cret"
servers = ["prod-*:ro", "staging", "id:0199…"]   # 能看到哪些共享条目，在哪些上只读
```

用文件可以让密码不出现在每次 `docker inspect` 都会打印的环境变量里，也是唯一一种改动账号时不必重写整份列表的写法。行内写法把角色放在**名字**一侧，是因为密码里允许出现 `:`，而名字里不允许。

`servers` 限定账号能看到哪些**共享**条目——按名字匹配（`*`、`?` 是通配符）或按 `id:` 精确指定——规则后加 `:ro` 表示在这些条目上只读，其它地方仍是完整角色。多条规则对同一条目意见不一时，**可写优先**：宽的规则做限制，例外单独点名，`["prod-*:ro", "prod-eu"]` 就是所有生产只读、`prod-eu` 可写。不写这一项，看到全部共享条目；写成空列表，一个都看不到。账号自己添加的条目永远可见可写。只有文件写法支持这一项，行内写法放不下。条目名是主人自己起的，能编辑条目的人可以把它改进或改出某个模式——在意这一点就用 `id:`。

**只读账号**可以查看它能看到的一切，但什么都改不了：不能写 Redis，也不能新增、编辑或删除服务器条目。拒绝由 bridge 做出，而不是由页面做出 —— 任何不是读的请求都会收到 `403`，所以直接向 `/v1/exec` 发请求的脚本和页面被一视同仁地拒绝，确认参数也换不来放行。判定用的是**读命令的白名单**，而不是"要避开的写命令"清单：`EVAL` 能执行任意脚本，`BITFIELD` 名字像读实际会写，`GETDEL` 与 `GETEX` 是写成 get 样子的写，而所有模块命令都是核心命令表从未见过的 —— 因此不认识的一律拒绝；漏掉某个读命令的表现是某个面板提示不可用。

修改账号的密码**或角色**都会让它已有的登录失效，所以把某个账号降为只读，对已经登录的浏览器同样立即生效。

这是纵深防御，不能替代 Redis 自己的 ACL：带 `-@write` 的 Redis ACL 用户由服务端在每条连接上强制执行，无论谁来连；而这里的限制由 bridge 强制执行，它是浏览器唯一能碰到的东西。两者一起用 —— ACL 是保证，账号角色是让按钮在请求发出之前就变灰的那一层。

## 接入你自己的 SSO

公司里已经有单点登录的话，通行做法是在应用前面放一个认证反向代理——oauth2-proxy、Authelia、Pomerium、Cloudflare Access、Tailscale 都是这一类：代理负责登录，把登录者是谁写进一个请求头。bridge 可以相信这个头：

```bash
-e ZEDIS_BRIDGE_TRUSTED_HEADER=Remote-User          # --trusted-header
-e ZEDIS_BRIDGE_TRUSTED_PROXY=10.0.0.0/8,172.17.0.5  # --trusted-proxy：代理的地址
```

两个要么都配，要么都不配：任何人都能在请求里自己写一个 `Remote-User: alice`，所以只有**来自代理**的连接上这个头才算数——判断依据是 socket 对端地址，永远不看 `X-Forwarded-For`。头里的名字必须是 users 文件里的账号，该账号可以不写密码；不是账号的名字会被 `403` 拒绝（并记入审计日志），而不是当成新用户放进来。角色仍然在 users 文件里（`read_only = true`），因为代理只回答"你是谁"，不回答"你能做什么"。

```toml
[[users]]
name = "alice@example.com"   # 和代理写进头里的值完全一致
read_only = true
```

代理必须做到两件事：转发的每个请求都剥掉或覆盖这个头；并且是访问 bridge 的唯一入口——如果 bridge 还能被直连，地址检查就形同虚设。各家代理只有头名不同：Authelia 是 `Remote-User`，oauth2-proxy 是 `X-Auth-Request-User`（需开 `--set-xauthrequest`），Cloudflare Access 是 `Cf-Access-Authenticated-User-Email`，Tailscale 是 `Tailscale-User-Login`。不配这两项时，这个头根本不会被读取。

## 生产环境的写入锁

打了 **Prod** 标签的条目，桌面和浏览器里写入都默认锁定（任何条目都可以在“安全”标签页的“写入”里选：跟随标签 / 允许 / 锁定 / 只读）。状态栏的锁要求输入服务器名，然后打开一个 **15 分钟**的窗口——按钮上显示剩余时间，到点自动重新锁上。浏览器里 bridge 按账号维护同一个窗口（`POST` / `DELETE /v1/servers/{id}/unlock`，审计记为 `unlocked` / `locked`），窗口之外的写入和破坏性命令一样收到 `428`，脚本和页面一视同仁。从浏览器发往这类条目的 Lua 脚本或函数调用（`EVAL` / `EVALSHA` / `FCALL`，`_RO` 形式除外）每次都要确认，窗口内也一样，Prod 上要输入名字——脚本里写了什么，命令分类器看不见。升级后已有的 Prod 条目会以锁定状态开始；不想要就把它的“写入”改为“允许”。

## 审计日志

`--audit-log /data/audit.log`（`ZEDIS_BRIDGE_AUDIT_LOG`）会为每个事件追加一行 JSON：每次登录与登录失败、只读账号被拒绝的每次请求、服务器条目的新增、编辑（改了哪些设置、改前改后的值；改了哪些密钥，只记名字不记值；私有条目被改为共享）和删除、每条管理服务器的命令 —— `CONFIG SET`、`ACL SETUSER`、`REPLICAOF`、`MODULE LOAD`、`CLIENT KILL`、`FLUSHDB` 之类 —— 以及每条需要人确认才放行的命令，所以开了"每次写都要确认"的条目，在终端里输入的每一次写都会留痕。`--audit-writes`（`ZEDIS_BRIDGE_AUDIT_WRITES=1`）再加上普通的数据写命令；读命令永远不记。参数里的密码会被抹掉，过长的值会截断，同一批里的同名命令合并成一行并记数量，文件以仅属主可读的权限创建。

```json
{"ts":"2026-09-25T08:12:03.417Z","account":"alice","peer":"10.0.0.7:51234","event":"command","server":{"id":"0199…","name":"prod"},"db":0,"command":"CONFIG","args":["SET","maxmemory","2gb"],"outcome":"confirmed","kind":"config_set","confirm":"type_name"}
```

这只是 bridge 这一扇门的日志：`redis-cli` 和应用程序直连 Redis 的操作不在里面；它回答不了"谁改了这个 key"，能回答的是"有没有人手动改过"。bridge 只追加、从不重新打开文件，轮转请用 `copytruncate`。

## 让 AI 助手走同一扇门（MCP）

`POST /v1/mcp` 是一个 [Model Context Protocol](https://modelcontextprotocol.io) 服务端，Claude Code、Cursor 或任何 MCP 客户端都可以经由 bridge 读取你的 Redis —— 只能读。助手像脚本一样用 HTTP Basic 登录，账号**必须是只读的**（`ai:ro@secret`，或 `read_only = true`），完整权限的账号无论问什么都会被拒绝。工具是按模型而不是按终端的习惯设计的：`list_servers`、`scan_keys`（分页，集群的每个 master 都会扫到）、`inspect_key`（类型、TTL、内存、编码、长度和一小段预览）、`server_info` 与 `slowlog`（按 master 解析好），以及兜底的 `read_command`，跑任意其它只读命令。工具发出的每条命令都过页面同一份只读白名单，再加一层拒绝会改动共享连接状态的命令（`SELECT`、`AUTH`、`CLIENT SETNAME`、`SUBSCRIBE` 等）；写入、`_RO` 形式以外的脚本、管理命令以及会返回凭据的读（读密码的 `CONFIG GET`、`ACL LIST`）都会被拒绝，并附上模型读得懂的原因。大值会截断到能放进上下文的大小，每个账号每分钟最多 120 次调用，而且**每次调用都是审计日志里的一行** —— 读也记，因为调用者是替人做事的程序。

```sh
claude mcp add --transport http zedis https://bridge.example.com/v1/mcp \
  --header "Authorization: Basic $(printf 'ai:secret' | base64)"
```

它走的是页面同一扇门，而不是另开一条通道：用户文件里的 `servers` 规则决定助手看得见哪些条目，审计日志记录它读了什么，数据不经过任何第三方。

## Web 版不包含的功能

所有"一问一答"式的功能都可用：key 树、各类型的值编辑器、终端、指标、慢日志、配置、客户端、内存分析、按值搜索。浏览器中不可用的有：流式面板（`MONITOR`、Pub/Sub、键空间事件）、拓扑与 Sentinel 管理、Lua 脚本库与 Protobuf 描述编辑器、多数据库键搜索、迁移（文件导入 / 导出）与跨服务器对比、连接诊断、回收站与指标的 1h / 24h / 7d 历史（两者都需要刷新后仍在的存储），以及被浏览器自己占用的快捷键（⌘N / ⌘T / ⌘W）。值编辑器里的代码在浏览器中是纯文本：语法高亮被 `gpui-component` 放在一个 wasm 构建无法开启的 `tree-sitter` feature 后面，因此 JSON 之类不着色、也无法折叠——格式化、JSONPath 与编辑不受影响。标签、备注、收藏和保存的脚本可以使用，但只保存在当前页面，刷新后会清空。页面处于后台标签页时会自动降低轮询频率。被摘掉的面板会明确提示不可用，而不是报错。桌面版仍是功能完整的客户端。

不使用 Docker 的话，`make web-dist` 可以把同样的内容构建成一个自包含的单文件二进制。
