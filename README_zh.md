中文 | [English](./README.md)

<h1 align="center">Zedis</h1>

<p align="center">
  <strong>一个使用 Rust 🦀 和 GPUI ⚡️ 构建的高性能、GPU 加速的 Redis 客户端</strong>
</p>

<p align="center">
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-Apache--2.0-blue.svg" alt="License"></a>
  <a href="https://x.com/tree0507"><img src="https://img.shields.io/twitter/follow/tree0507?style=social" alt="Twitter Follow"></a>
  <img src="https://img.shields.io/github/downloads/vicanso/zedis/total" alt="Downloads">
  <a href="https://www.blazingly.fast"><img src="https://www.blazingly.fast/api/badge.svg?repo=vicanso%2Fzedis" alt="blazingly fast"></a>
</p>

<p align="center">
  <video src="https://github.com/user-attachments/assets/217cc0a7-cc7e-40d0-ac7e-1ec61c36a02b" autoplay loop muted playsinline width="100%"></video>
</p>

---

## 🤔 为什么选择 Zedis？

厌倦了那些仅仅为了显示一个 JSON 字符串就吃掉几 GB 内存的 Electron Redis 客户端，或者在你不小心点击了一个包含 10 万个元素的键时直接卡死？我们也有同感。

**Zedis** 专为追求原生性能的开发者而生，从零开始打造。由 **GPUI**（[Zed Editor](https://zed.dev) 背后同款渲染引擎）驱动，即便在浏览超大数据库时，Zedis 也能以极低的内存占用，带来流畅丝滑的 60+ FPS 原生体验。

## ✨ 亮点

- 🦀 **原生，而非 Electron** —— 每个像素都在 GPU 上绘制、虚拟滚动 `SCAN`，百万级键也保持 60+ FPS、极低内存。
- 🧠 **看得懂你的数据** —— 自动解压并解码 JSON/JSONPath、Protobuf、MessagePack、时间戳、图片与 Hex，并为每种 Redis 类型和模块提供专用查看器。
- 📊 **实时可观测** —— 实时指标、带离线 + AI 建议的内存分析器、慢日志 ↔ Latency、`MONITOR`、按值搜索。
- 🔐 **隐私优先且安全** —— 元数据只存本地文件、密钥加密存储、破坏性操作对生产环境升级确认措辞。
- 🌐 **连接一切** —— TLS/SSL、SSH 隧道、Cluster/Sentinel、Redis Insight 导入，以及 8 种界面语言。
- ⌨️ **为重度用户而生** —— ⌘K 命令面板、带补全的 redis-cli、Batch 模式、跨服务器复制/对比。

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
    <td><a href="https://github.com/user-attachments/assets/20384b9e-9af7-4921-bacc-dde2c58a74fa"><img src="https://github.com/user-attachments/assets/20384b9e-9af7-4921-bacc-dde2c58a74fa" width="260" alt="键浏览与数据查看"></a></td>
    <td><a href="https://github.com/user-attachments/assets/4442055e-4fe2-4195-a28a-9367466cb4e0"><img src="https://github.com/user-attachments/assets/4442055e-4fe2-4195-a28a-9367466cb4e0" width="260" alt="内存分析器"></a></td>
    <td><a href="https://github.com/user-attachments/assets/40daafd8-1190-41ee-9dd1-1bbcd6f0db21"><img src="https://github.com/user-attachments/assets/40daafd8-1190-41ee-9dd1-1bbcd6f0db21" width="260" alt="实时指标"></a></td>
  </tr>
  <tr>
    <td align="center"><sub>键浏览与数据查看</sub></td>
    <td align="center"><sub>内存分析器</sub></td>
    <td align="center"><sub>实时指标</sub></td>
  </tr>
  <tr>
    <td><a href="https://github.com/user-attachments/assets/6a716d9c-cb58-40b1-9847-b0193c390507"><img src="https://github.com/user-attachments/assets/6a716d9c-cb58-40b1-9847-b0193c390507" width="260" alt="地理地图"></a></td>
    <td><a href="https://github.com/user-attachments/assets/00c6c782-af7e-4847-aa14-5f7756f66f13"><img src="https://github.com/user-attachments/assets/00c6c782-af7e-4847-aa14-5f7756f66f13" width="260" alt="向量集 + KNN"></a></td>
    <td><a href="https://github.com/user-attachments/assets/b208f236-f6aa-4686-bdfe-2e2474a6757b"><img src="https://github.com/user-attachments/assets/b208f236-f6aa-4686-bdfe-2e2474a6757b" width="260" alt="命令面板"></a></td>
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
| 🚀 **原生 & 快** | GPU 渲染 · 虚拟滚动 `SCAN`，百万键 60+ FPS · macOS / Windows / Linux · 浅色 / 深色 / 跟随系统 |
| 🧠 **智能数据查看器** | 自动解压(LZ4 / Snappy / GZIP / ZSTD)· JSON & RedisJSON + JSONPath · Protobuf · MessagePack · 时间戳 · 图片 · Hex · 自定义脚本 |
| 🗂️ **类型 & 模块查看器** | 位图 · HyperLogLog · 向量集(KNN)· 地理地图 · Bloom / Cuckoo / Count-Min / Top-K · 时间序列 · Streams(实时跟踪)· Pub/Sub · RediSearch · Functions |
| 📊 **可观测性** | 实时指标 · 内存分析 + AI 建议 · 慢日志 ↔ Latency · `MONITOR` · 按值搜索 · 集群健康 · 持久化 & 键事件 |
| 🔑 **Keys & 数据** | 带 TTL chip 的命名空间树 · 标签 / 备注 / 收藏 · 重命名 · 字段级 TTL · 版本历史 · 文件导入导出 · 批量操作 · 跨服务器复制 & 对比 |
| 🔐 **安全 & 隐私** | 环境标签 + PROD 升级确认 · 只读锁 · ACL 编辑 · TLS/SSL & SSH · 密钥加密 · 纯本地、无遥测 |
| ⌨️ **效率** | ⌘K 面板 · ⌘/ 快捷键速查 · 带补全的 redis-cli · 多行 Batch 模式 · Lua 脚本库 |

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

### Linux (Arch)

```bash
yay -S zedis-bin
```

### Cargo（跨平台源码编译）

```bash
# 最新发布版（来自 crates.io）
cargo install --locked zedis-gui

# 或直接从 GitHub 编译最新提交
cargo install --git https://github.com/vicanso/zedis --locked zedis-gui
```

---

## 🤝 参与贡献

我们希望将 Zedis 打造成终极 Redis 客户端，非常欢迎你的参与！无论是新增功能、翻译界面还是修复 Bug，一切贡献都受到欢迎。

欢迎提交 issue 或 PR 参与进来。提交 PR 即表示你同意我们的[贡献者许可协议（CLA）](./CLA.md)。

## 📄 许可证

Zedis 是根据 [Apache License 2.0](./LICENSE) 授权的开源软件。
