中文 | [English](./README.md)

<h1 align="center">Zedis</h1>

<p align="center">
  <strong>能打开你那个百万级 key 的库而不转圈的 Redis 客户端 —— 原生、GPU 加速,由 Rust 🦀 和 GPUI ⚡️ 驱动</strong>
</p>

<p align="center">
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-Apache--2.0-blue.svg" alt="License"></a>
  <a href="https://x.com/tree_xie"><img src="https://img.shields.io/twitter/follow/tree_xie?style=social" alt="Twitter Follow"></a>
  <img src="https://img.shields.io/github/downloads/vicanso/zedis/total" alt="Downloads">
  <a href="https://www.blazingly.fast"><img src="https://www.blazingly.fast/api/badge.svg?repo=vicanso%2Fzedis" alt="blazingly fast"></a>
</p>

<p align="center">
  <video src="https://github.com/user-attachments/assets/d7007ecf-bbfd-4e68-bbaf-437091f711e7" autoplay loop muted playsinline width="100%"></video>
</p>

---

## 🤔 为什么选择 Zedis？

厌倦了那些仅仅为了显示一个 JSON 字符串就吃掉几 GB 内存、点开一个 10 万元素的键就直接卡死、集群模式操作处处别扭、把压缩和二进制 value 显示成一堆乱码的 Electron Redis 客户端？我们也有同感。

**Zedis** 专为追求原生性能的开发者而生，从零开始打造。由 **GPUI**（[Zed Editor](https://zed.dev) 背后同款渲染引擎）驱动，即便在浏览超大数据库时，Zedis 也能以极低的内存占用，带来流畅丝滑的 60+ FPS 原生体验。

## ✨ 亮点

- 🦀 **原生，而非 Electron** —— 每个像素都在 GPU 上绘制、虚拟滚动 `SCAN`，百万级键也保持 60+ FPS、极低内存。
- 🧠 **看得懂你的数据** —— 自动解压并解码 JSON/JSONPath、Protobuf、MessagePack、时间戳、图片与 Hex，并为每种 Redis 类型和模块提供专用查看器。
- 📊 **实时可观测** —— 实时指标、内存分析器（离线 + AI 建议、服务端 key 大小直方图）、热点 Key 跟踪（`HOTKEYS`）、集群每 slot 统计、慢日志 ↔ Latency、`MONITOR`、按值搜索。
- 🔐 **隐私优先且安全** —— 元数据只存本地文件、密钥用每机唯一密钥加密存储、破坏性操作对生产环境升级确认措辞。
- 🌐 **连接一切** —— TLS/SSL、SSH 隧道（含带口令的加密密钥）、Cluster/Sentinel、从 Redis Insight / ARDM / Tiny RDM 导入，以及 8 种界面语言。
- ⌨️ **为重度用户而生** —— ⌘K 命令面板、带补全的 redis-cli、表格 / JSON 回复视图 + AI 命令助手、Batch 模式、跨服务器复制/对比。

> ### 🔄 已经在用 Redis Insight?
> **粘贴它导出的数据库配置,所有连接一次迁入** —— 不用一个个重填地址、端口和密码。花大约一分钟,就能拿你真实的连接试试 Zedis,快不快自己判断。

## 📸 截图

<!--
  方案 A —— 图片托管在 GitHub:把每张截图拖进任意 issue/PR 评论(或 release),
  得到 https://github.com/user-attachments/assets/... 链接,然后替换下面的
  REPLACE-* 占位符。点击缩略图打开原图;width="260" 让三列网格约占一屏。
  建议截图(按重要性):
    1. key-browser     —— 命名空间树 + JSON / 语法高亮的值编辑器
    2. memory-analyzer —— Top-N 表 + TTL 直方图 + 体检建议
    3. live-metrics    —— 实时 GPU 图表(CPU / 内存 / 网络)
    4. geo-map         —— sorted set 画在雷达上
    5. vector-set      —— 向量集 + KNN(VSIM)结果
    6. command-palette —— ⌘K 命令面板
-->

<table>
  <tr>
    <td><a href="https://github.com/user-attachments/assets/c06e4d80-7607-4d6c-807e-2a62a2ee556f"><img src="https://github.com/user-attachments/assets/c06e4d80-7607-4d6c-807e-2a62a2ee556f" width="260" alt="键浏览与数据查看"></a></td>
    <td><a href="https://github.com/user-attachments/assets/88091f50-ec77-41d5-acda-047a835079f8"><img src="https://github.com/user-attachments/assets/88091f50-ec77-41d5-acda-047a835079f8" width="260" alt="内存分析器"></a></td>
    <td><a href="https://github.com/user-attachments/assets/d5801a8c-da94-461b-83b6-6c9b70e2007d"><img src="https://github.com/user-attachments/assets/d5801a8c-da94-461b-83b6-6c9b70e2007d" width="260" alt="实时指标"></a></td>
  </tr>
  <tr>
    <td align="center"><sub>键浏览与数据查看</sub></td>
    <td align="center"><sub>内存分析器</sub></td>
    <td align="center"><sub>实时指标</sub></td>
  </tr>
  <tr>
    <td><a href="https://github.com/user-attachments/assets/2525cec9-5dd6-4049-9ea9-60fcb4cc249f"><img src="https://github.com/user-attachments/assets/2525cec9-5dd6-4049-9ea9-60fcb4cc249f" width="260" alt="地理地图"></a></td>
    <td><a href="https://github.com/user-attachments/assets/b4733051-2965-40ff-9d49-6bb909551513"><img src="https://github.com/user-attachments/assets/b4733051-2965-40ff-9d49-6bb909551513" width="260" alt="向量集 + KNN"></a></td>
    <td><a href="https://github.com/user-attachments/assets/4335c12b-cbca-467e-abd9-7b50ffd568c5"><img src="https://github.com/user-attachments/assets/4335c12b-cbca-467e-abd9-7b50ffd568c5" width="260" alt="命令面板"></a></td>
  </tr>
  <tr>
    <td align="center"><sub>地理地图</sub></td>
    <td align="center"><sub>向量集 + KNN</sub></td>
    <td align="center"><sub>命令面板(⌘K)</sub></td>
  </tr>
</table>

## 🧩 功能一览

| 领域 | 包含内容 |
| --- | --- |
| 🚀 **原生 & 快** | GPU 渲染 · 虚拟滚动 `SCAN`，百万键 60+ FPS · macOS / Windows / Linux · 浅色 / 深色 / 跟随系统 + 6 套内置主题 · 界面与等宽字体可自选 |
| 🧠 **智能数据查看器** | 自动解压(LZ4 / Snappy / GZIP / ZSTD)· JSON & RedisJSON + JSONPath · Protobuf · MessagePack · 时间戳 · 图片 · Hex · 自定义脚本 |
| 🗂️ **类型 & 模块查看器** | 位图 · HyperLogLog · 向量集(KNN)· 地理地图 · Bloom / Cuckoo / Count-Min / Top-K · 时间序列 · Streams(实时跟踪)· Pub/Sub(含分片)· RediSearch · Functions |
| 📊 **可观测性** | 实时指标 + 7 天历史 · 内存分析（在线扫描或离线 RDB 文件）+ AI 建议 · 慢日志 ↔ Latency · `MONITOR` · 按值搜索 · 集群健康 & 重分片 · 持久化 & 键事件 · 带类型的 CONFIG 编辑器 · 原始 INFO 浏览器 |
| 🔑 **Keys & 数据** | 带 TTL chip 的命名空间树 · 分页加载的 Hash / List / Set / ZSet 编辑器（`HSCAN`/`SSCAN`/`ZSCAN`）· 标签 / 备注 / 收藏 · 重命名 · 字段级 TTL · 版本历史 · 本地回收站(24h)· 文件导入导出 · 批量操作(Tools 导出、前缀过滤、二进制 / JSON / CSV)· 跨服务器复制 & 对比 |
| 🔐 **安全 & 隐私** | 环境标签 + PROD 升级确认 · 只读锁 · ACL 编辑 · TLS/SSL & SSH · 分阶段连接诊断 · 断线自愈并跟随 Sentinel/Cluster 故障转移 · 每机密钥加密 · 纯本地、无遥测 |
| 🧭 **受限服务端** | 连接后自动探测能力：代理（Twemproxy / Codis / Envoy）、云托管（ElastiCache / Azure / Tair）和 Redis 兼容服务端（Valkey / Dragonfly / KeyDB / Kvrocks）上，依赖缺失命令的面板与按钮会灰显并*说明原因*（`CONFIG GET` 不支持、`SLOWLOG` 无权限）而不是报错 · 没有 `SCAN` 时键编辑器仍可按键名打开 · 完整命令矩阵在 工具 → 服务端能力 中查看 |
| ⌨️ **效率** | 多连接工作区标签页 · ⌘K 面板 · ⌘P 最近打开的键 · ⌘⇧F 多数据库键搜索 · ⌘/ 快捷键速查 · redis-cli 带补全、按服务器的历史与 `Ctrl+R` 反向搜索 · AI 命令助手（终端内 `?`）· 多行 Batch 模式 · Lua 脚本库 · 可关闭的更新检查（下载带校验和验证）· 可选系统托盘（macOS / Windows）· 应用自身请求可走 HTTP / SOCKS5 代理 · 滚动文件日志 · 导出诊断包（日志、崩溃报告、脱敏配置、连接状态打成一个 zip） |

> 🔐 **连接密钥存放位置：** 密码与 SSH 私钥用每台机器**唯一的随机密钥**加密 —— macOS 存 **钥匙串(Keychain)**、Windows 存 **凭据管理器**、**Linux** 存配置目录下 `0600` 权限的密钥文件(不依赖 Secret Service / D-Bus，headless 也能用)。密钥不离开本机，所以直接把配置文件拷到别的机器是解不开的 —— 跨机迁移请用带口令的导出功能。

📖 **[查看完整功能巡览 →](./docs/FEATURES_zh.md)**

---

## 📦 安装

准备好感受极速体验了吗？通过你喜欢的包管理器安装 Zedis：

### macOS
推荐通过 Homebrew 安装：

```bash
brew install --cask zedis
```

### Windows

```bash
scoop bucket add extras
scoop install zedis
```

### Linux

Arch Linux（AUR）：

```bash
yay -S zedis-bin
```

其他发行版：每个 [release](https://github.com/vicanso/zedis/releases/latest) 都附带 `.deb`、`.rpm`、AppImage 和普通 tarball（x86_64 与 aarch64）。

### Cargo（跨平台源码编译）

> **说明：** Zedis 依赖的是 GPUI 的未发布（git）版本，而 crates.io 不允许带 git 依赖发布，
> 因此**最新版本无法发布到 crates.io**，那里的版本可能滞后。想用最新版，建议用上面的
> Homebrew / Scoop / AUR 安装，或[下载发布版](https://github.com/vicanso/zedis/releases)；
> 要源码编译请用下面的 `--git` 命令。

```bash
# 来自 crates.io —— 可能是较旧的版本（见上方说明）
cargo install --locked zedis-gui

# 最新版：直接从 GitHub 源码编译（会解析 git 依赖）
cargo install --git https://github.com/vicanso/zedis --locked zedis-gui
```

---

## 🤝 参与贡献

我们希望将 Zedis 打造成终极 Redis 客户端，非常欢迎你的参与！无论是新增功能、翻译界面还是修复 Bug，一切贡献都受到欢迎。

欢迎提交 issue 或 PR 参与进来。提交 PR 即表示你同意我们的[贡献者许可协议（CLA）](./CLA.md)。

## 📄 许可证

Zedis 是根据 [Apache License 2.0](./LICENSE) 授权的开源软件。
