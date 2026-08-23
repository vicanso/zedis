# Security Policy / 安全策略

## 🇬🇧 English

### Supported versions
Security fixes are applied to the latest release line only.

| Version | Supported |
| --- | --- |
| 0.8.x (latest) | ✅ |
| < 0.8 | ❌ |

### Reporting a vulnerability
**Please do not report security vulnerabilities through public GitHub issues.**

Instead, report privately via GitHub's [**Report a vulnerability**](https://github.com/vicanso/zedis/security/advisories/new) (the repo's **Security → Advisories** tab). Please include:

- the affected version and platform,
- a description of the issue and its impact,
- steps to reproduce (and a proof of concept if possible).

We aim to acknowledge your report within a few days and will keep you updated as we work on a fix, coordinating disclosure timing with you.

### Scope
Zedis is a **local desktop client** — it stores connection secrets encrypted at rest, keeps metadata (tags, notes, favorites, history) in a local file, and makes no outbound network calls unless you explicitly configure the optional AI analysis. Reports are especially welcome around:

- connection-secret handling and the at-rest encryption,
- TLS/SSL and SSH-tunnel handling,
- the custom script viewer (which runs local shell commands),
- the optional AI analysis (what leaves your machine, and where).

### Threat model
A Redis server you connect to is treated as **untrusted input**. Key names and
values are chosen by whoever can write to that server, so Zedis never lets them
become executable syntax: the custom script viewer passes `{KEY}` and `{VALUE}`
to the shell through environment variables that expand after the command line is
parsed, and every script run is bounded by a timeout and an output cap. A report
showing that data from a server can influence what runs on the client machine is
exactly the kind we want.

---

## 🇨🇳 中文

### 受支持的版本
安全修复仅应用于最新的发布线。

| 版本 | 是否支持 |
| --- | --- |
| 0.8.x(最新) | ✅ |
| < 0.8 | ❌ |

### 报告漏洞
**请勿通过公开的 GitHub issue 报告安全漏洞。**

请通过 GitHub 的 [**Report a vulnerability**](https://github.com/vicanso/zedis/security/advisories/new)(仓库 **Security → Advisories** 标签)私下报告,并尽量包含:

- 受影响的版本与平台;
- 问题描述及其影响;
- 复现步骤(如可能,附概念验证)。

我们会争取在数日内确认你的报告,并在修复过程中持续同步进展,与你协调披露时间。

### 范围
Zedis 是**本地桌面客户端** —— 连接密钥加密存储,元数据(标签、备注、收藏、历史)只存本地文件,且除非你显式配置可选的 AI 分析,否则不发起任何外发网络请求。以下方面的报告尤其欢迎:

- 连接密钥的处理与静态加密;
- TLS/SSL 与 SSH 隧道的处理;
- 自定义脚本查看器(会执行本地 Shell 命令);
- 可选的 AI 分析(哪些数据离开本机、发往何处)。

### 威胁模型
所连接的 Redis 服务端被视为**不可信输入**。键名与值由任何对该服务端有写权限的人决定,
因此 Zedis 不会让它们变成可执行的语法:自定义脚本查看器通过环境变量把 `{KEY}` 与
`{VALUE}` 传给 Shell,变量在命令行解析完成之后才展开;每次脚本执行都有超时与输出上限。
如果你能证明来自服务端的数据可以影响客户端上执行的内容,正是我们最希望收到的报告。
