# Zedis 全视图功能与 UI 优化清单（按使用频率排序）

本文档梳理 Zedis 客户端中**全部视图（View）、内嵌子编辑器（Sub-Editor）、全局快捷浮层（Palette/Overlay）、独立辅助窗口（Secondary Window）以及模态弹窗（Dialog）**的完整功能说明、路由与门控机制、以及面向 [ui-design-standards.md](ui-design-standards.md) 的具体 UI 优化切入点。

文档严格按**用户真实使用频率从高到低（P0 → P4，共 50 个视图单元）**排序，供后续分批推进 UI 一致性与体验打磨时作为基线参考。

---

## 一、路由与视图层级架构速览

Zedis 的界面由五层容器组成：

1. **常驻应用外壳（App Shell）**：多工作区标签栏（`Zedis` / `ContentTab`）、顶部标题栏（`ZedisTitleBar`）、左侧连接侧边栏（`ZedisSidebar`）、底部状态栏（`ZedisStatusBar`）及工作区路由容器（`ZedisContent`）。
2. **顶层路由（`Route`）**：
   - `Route::Home`：连接管理卡片页（`ZedisServers`）
   - `Route::Server { server_id, db, view: ServerView }`：单连接工作区（含默认 `ServerView::Editor` 及 20+ 个服务端工具面板）
   - `Route::Settings` / `Route::Protos` / `Route::Scripts`：应用设置、Protobuf 映射与自定义脚本解码器
3. **键值内嵌子编辑器（Value Sub-Editors）**：挂载在 `ZedisEditor` 内，按数据类型与编码分派（`ZedisBytesEditor`、`ZedisJsonTree`、`ZedisKvTable` 及其 Hash/List/Set/ZSet/Stream 面板，以及 Geo、TimeSeries、VectorSet、Bitmap、Probabilistic、HyperLogLog、ValueDiff 等专用视图）。
4. **全局快捷浮层（Global Overlays）**：命令面板（`⌘K`）、最近打开键面板（`⌘P`）、跨库键搜索（`⌘⇧F`）、快捷键速查表（`⌘/`）。
5. **独立窗口与模态弹窗（Secondary Windows & Dialogs）**：桌面端迁移/比对/设置/关于独立窗口（Web 端自动降级为模态框），以及键操作、安全确认、时间线、回收站等模态弹窗。

> **状态存活策略**：在切换 `ServerView` 工具面板时，`ZedisKeyTree` + `ZedisEditor` + `ZedisTerminal` 始终保活（不丢失展开树节点、未保存草稿与终端会话）；声明了 `survives_navigation()` 的重型任务面板（`MemoryAnalysis`、`ValueSearch`、`Benchmark`、`Workbench`）在返回编辑器再切回时也保留运行进度与结果。

---

## 二、全量视图按使用频率总览表（#1 – #50）

| 排名 | 频率梯队 | 视图名称 | Rust 核心组件 / 入口 | 源码位置 | 形态分类 | 建议 UI 优化优先级 |
| :---: | :---: | :--- | :--- | :--- | :--- | :---: |
| #1 | **P0** | 应用根容器与多工作区标签栏 | `Zedis`, `ContentTab` | `src/root.rs`, `src/root/tabs.rs` | 常驻外壳 | ✅ 已完成 |
| #2 | **P0** | 顶部标题栏与在线更新浮层 | `ZedisTitleBar`, `update_prompt` | `src/views/title_bar.rs`, `update_prompt.rs` | 常驻外壳 | ✅ 已完成 |
| #3 | **P0** | 左侧服务器侧边栏 | `ZedisSidebar` | `src/views/sidebar.rs` + `sidebar/menu.rs` | 常驻外壳 | ✅ 已完成 |
| #4 | **P0** | 底部状态栏与工具总菜单 | `ZedisStatusBar` | `src/views/status_bar.rs` + `status_bar/*` | 常驻外壳 | ✅ 已完成 |
| #5 | **P0** | 内容区工作台路由与不支持占位页 | `ZedisContent`, `ZedisUnsupportedPanel` | `src/views/content.rs`, `unsupported_panel.rs` | 路由容器 | ✅ 已完成 |
| #6 | **P0** | 首页服务器连接管理与诊断/导入导出 | `ZedisServers`, `ZedisConnectionDiagnostics` | `src/views/servers.rs` + `servers/*`, `connection_diagnostics.rs` | 顶层页面 + 弹窗 | ✅ 已完成 |
| #7 | **P0** | 键树导航、过滤与批量模式操作 | `ZedisKeyTree` | `src/views/key_tree.rs` + `key_tree/*` | 核心主面板 | ✅ 已完成 |
| #8 | **P0** | 键值编辑器外壳与版本/冲突管理 | `ZedisEditor` | `src/views/editor.rs` + `editor/*` | 核心主面板 | ✅ 已完成 |
| #9 | **P0** | String / Bytes / JSON 树与多格式编辑器 | `ZedisBytesEditor`, `ZedisJsonTree` | `src/views/bytes_editor.rs`, `json_tree.rs` | 内嵌子编辑器 | ✅ 已完成 |
| #10 | **P0** | 集合通用表格框架与 Hash / List / Set / ZSet 编辑器 | `ZedisKvTable`, `ZedisHashEditor` 等 | `src/views/kv_table.rs` + `hash/list/set/zset_editor.rs` | 内嵌子编辑器 | ✅ 已完成 |
| #11 | **P0** | Stream 消息流、消费者组与 PEL 编辑器 | `ZedisStreamEditor` | `src/views/stream_editor.rs` + `stream_editor/*` | 内嵌子编辑器 | ✅ 已完成 |
| #12 | **P1** | 内嵌交互式 CLI 终端 | `ZedisTerminal` | `src/views/terminal.rs` + `terminal/*` | 底部停靠面板 | ✅ 已完成 |
| #13 | **P1** | 全局命令面板 (`⌘K`) | `ZedisCommandPalette` | `src/views/command_palette.rs` | 全局浮层 | ✅ 已完成 |
| #14 | **P1** | 最近访问键切换面板 (`⌘P`) | `ZedisRecentKeysPalette` | `src/views/recent_keys_palette.rs` | 全局浮层 | ✅ 已完成 |
| #15 | **P1** | 跨实例/多库键搜索面板 (`⌘⇧F`) | `ZedisMultiSearch` | `src/views/multi_search.rs` | 全局浮层 | ✅ 已完成 |
| #16 | **P1** | 高危操作确认与生产写锁弹窗 | `confirm_dangerous_command`, `TypedConfirm` | `src/views/danger_confirm.rs` | 安全弹窗 | ✅ 已完成 |
| #17 | **P1** | TTL 相对时长与绝对过期时间弹窗 | `ZedisTtlDialog`, `ZedisExpireAtDialog` | `src/views/ttl_dialog.rs`, `expire_at_dialog.rs` | 键级弹窗 | ✅ 已完成 |
| #18 | **P1** | 键颜色标签与本地备注弹窗 | `open_key_tag_dialog` | `src/views/key_tag_dialog.rs` | 键级弹窗 | ✅ 已完成 |
| #19 | **P1** | 键值差异比对视图 (Diff & JSON Merge) | `ZedisValueDiff` | `src/views/value_diff.rs` | 内嵌替换面板 | ✅ 已完成 |
| #20 | **P1** | 本地回收站弹窗 (Trash) | `ZedisTrashDialog` | `src/views/trash_dialog.rs` | 模态弹窗 | ✅ 已完成 |
| #21 | **P1** | 跨服务器复制键 / 目标库选择弹窗 | `ZedisCopyKeyDialog` | `src/views/copy_key_dialog.rs` | 模态弹窗 | ✅ 已完成 |
| #22 | **P1** | 集合多键原生运算弹窗 (Inter/Union/Diff/Move) | `ZedisKeyOpDialog` | `src/views/key_op_dialog.rs` | 模态弹窗 | ✅ 已完成 |
| #23 | **P1** | 单键实时时间线与变更监控弹窗 | `ZedisKeyTimelineDialog` | `src/views/key_timeline_dialog.rs` | 模态弹窗 | ✅ 已完成 |
| #24 | **P1** | 会话内集合修改审计弹窗 | `ZedisChangeLogDialog` | `src/views/change_log_dialog.rs` | 模态弹窗 | ✅ 已完成 |
| #25 | **P1** | 全局快捷键速查浮层 (`⌘/`) | `ZedisShortcutsOverlay` | `src/views/shortcuts_overlay.rs` | 全局浮层 | ✅ 已完成 |
| #26 | **P2** | 实时监控指标大盘与阈值告警 | `ZedisMetrics`, `ZedisServerAlertsDialog` | `src/views/metrics.rs`, `metrics/alerts_dialog.rs`, `charts.rs` | `ServerView` 面板 | ✅ 已完成 |
| #27 | **P2** | 慢查询、Valkey 命令日志与延迟诊断 | `ZedisSlowlogEditor` | `src/views/slowlog_editor.rs` + `slowlog_editor/*` | `ServerView` 面板 | ✅ 已完成 |
| #28 | **P2** | 在线内存采样与离线 RDB 深度分析 | `ZedisMemoryAnalysis`, `ZedisDataDictionaryDialog` | `src/views/memory_analysis.rs` + `memory_analysis/*` | `ServerView` 面板 | ✅ 已完成 |
| #29 | **P2** | 多标签查询工作台与代码片段库 | `ZedisWorkbench` | `src/views/workbench.rs` + `workbench/*` | `ServerView` 面板 | ✅ 已完成 |
| #30 | **P2** | 客户端连接管理与暂停/查杀 | `ZedisClientsManager`, `client_dialogs` | `src/views/clients_manager.rs`, `client_dialogs.rs` | `ServerView` 面板 | ✅ 已完成 |
| #31 | **P2** | 实时命令流监控与流量回放 (Monitor) | `ZedisMonitor`, `MonitorReplayDialog` | `src/views/monitor.rs` + `monitor/*` | `ServerView` 面板 | ✅ 已完成 |
| #32 | **P2** | 键值内容全文检索 (Value Search) | `ZedisValueSearch` | `src/views/value_search.rs` + `value_search/*` | `ServerView` 面板 | ✅ 已完成 |
| #33 | **P2** | Pub/Sub 实时消息订阅与发布面板 | `ZedisPubsubEditor`, `ZedisPubsubChannelsDialog` | `src/views/pubsub_editor.rs`, `pubsub_channels_dialog.rs` | 内嵌替换面板 | ✅ 已完成 |
| #34 | **P2** | 服务端原始 `INFO` 浏览器与快照对比 | `ZedisServerInfo` | `src/views/server_info.rs` | `ServerView` 面板 | ✅ 已完成 |
| #35 | **P2** | 服务端命令吞吐负载分析 (Commandstats) | `ZedisServerLoad` | `src/views/server_load.rs` | `ServerView` 面板 | ✅ 已完成 |
| #36 | **P2** | Redis 8.6+ 热点键追踪面板 (Hotkeys) | `ZedisHotkeys` | `src/views/hotkeys.rs` | `ServerView` 面板 | ✅ 已完成 |
| #37 | **P2** | 一键故障排查报告与 Doctor 诊断报告 | `ZedisTroubleshootReport`, `ZedisServerReportDialog` | `src/views/troubleshoot_report.rs`, `server_report_dialog.rs` | 模态弹窗 | ✅ 已完成 |
| #38 | **P2** | 服务端命令能力探测矩阵弹窗 | `open_features_dialog` | `src/views/features_dialog.rs` | 模态弹窗 | ✅ 已完成 |
| #39 | **P3** | 运行时参数配置编辑器与跨库对比 | `ZedisConfigEditor` | `src/views/config_editor.rs`, `config_doc.rs` | `ServerView` 面板 | ✅ 已完成 |
| #40 | **P3** | ACL 权限用户管理、权限模拟与安全日志 | `ZedisAclManager` | `src/views/acl_manager.rs` | `ServerView` 面板 | ✅ 已完成 |
| #41 | **P3** | RDB / AOF 持久化状态与后台落盘控制 | `ZedisPersistence` | `src/views/persistence.rs` | `ServerView` 面板 | ✅ 已完成 |
| #42 | **P3** | 集群 / 哨兵 / 主从拓扑管理与在线重分片 | `ZedisTopology` | `src/views/topology.rs` + `topology/*`, `sentinel_dialogs.rs` | `ServerView` 面板 | ✅ 已完成 |
| #43 | **P3** | RediSearch 全文与向量索引管理器 (`FT.*`) | `ZedisSearchManager` | `src/views/search_manager.rs` + `search_manager/*` | `ServerView` 面板 | ✅ 已完成 |
| #44 | **P3** | Lua 脚本库与交互式单步调试器 | `ZedisLuaScriptLibrary`, `LuaDebugger` | `src/views/lua_script_library.rs` + `lua_script_library/*` | `ServerView` 面板 | ✅ 已完成 |
| #45 | **P3** | Redis 7+ Functions 函数库管理器 | `ZedisFunctionEditor` | `src/views/function_editor.rs` | `ServerView` 面板 | ✅ 已完成 |
| #46 | **P3** | 键空间事件实时订阅器 (Keyspace Notifications) | `ZedisKeyspaceNotifications` | `src/views/keyspace_notifications.rs` | `ServerView` 面板 | ✅ 已完成 |
| #47 | **P3** | 内置基准压测工具 (Benchmark) | `ZedisBenchmark` | `src/views/benchmark.rs` + `benchmark/*` | `ServerView` 面板 | ✅ 已完成 |
| #48 | **P3** | 数据导入 / 导出 / 跨实例迁移与增量跟随 | `ZedisMigrationWindow`, `ZedisKeyExport` | `src/views/migration_window.rs` + `key_export.rs`, `export.rs` | 独立窗口 / 弹窗 | ✅ 已完成 |
| #49 | **P3** | 跨数据库全量键比对工具 (Compare DBs) | `ZedisCompareWindow` | `src/views/compare_window.rs` | 独立窗口 / 弹窗 | ✅ 已完成 |
| #50 | **P4** | 专有数据结构视图与应用级设置/管理窗口组 | Geo / TimeSeries / VectorSet / Bitmap / Bloom / HLL / LCS / Settings / Protos / Scripts / Account / About | `src/views/geo_map.rs`, `setting_editor.rs`, `account.rs` 等 | 子视图 / 独立窗口 | ✅ 已完成 |

---

## 三、P0 梯队：常驻应用外壳与每日核心键值工作流

这一梯队构成了用户打开 Zedis 后 **90% 以上停留时间的界面**，任何间距、对齐、颜色层级或交互态瑕疵都会被高频感知，是 UI 优化的第一优先级。

---

### #1 应用根容器与多工作区标签栏 (`Zedis`, `ContentTab`) — ✅ 已完成

- **源码位置**：`src/root.rs`、`src/root/tabs.rs`
- **入口与快捷键**：应用主窗口根视图；标签栏右侧 `+` 按钮新建标签页（支持侧边栏 `⌘+点击` / `⌘⇧+点击`）、`⌘W` 关闭当前标签/窗口、`⌘1`–`⌘8` 切换第 N 个标签、`⌘[` / `⌘]` 路由历史后退与前进。
- **使用频率理由**：100% 会话常驻外壳，承载多连接并发排障场景。
- **布局结构**：顶部 `ZedisTitleBar` → 可选多工作区标签栏（仅在打开 `≥2` 个标签页时渲染，单标签时完全隐藏零高度开销；`min_h(EDITOR_KEY_BAR_HEIGHT)` 与侧边栏 `ZEDIS` 顶栏严格 40px 共线，右侧固定 `+` 新建标签按钮）→ 中部水平排布的 `ZedisSidebar` + 当前激活 `ContentTab` 的 `ZedisContent` → 底部当前标签页专属的 `ZedisStatusBar` → 顶层挂载全局浮层（Command Palette、Recent Keys、Multi-Search、Shortcuts）与 Dialog/Notification 层。
- **完整功能与交互**：
  - **隔离式多标签工作区**：最多同时打开 8 个工作区标签页；每个 `ContentTab` 拥有独立的 `Entity<ZedisServerState>` 与 `Entity<ZedisContent>`，互不干扰。
  - **标签交互**：支持拖拽排序（带 `drag_over` 左边缘蓝线插槽指示与 `popover` 浮层预览）、鼠标中键点击关闭、`×` 关闭按钮、末尾固定 `+` 新建标签按钮（达上限自动禁用提示）、完整信息 Tooltip、右键上下文菜单（关闭、关闭其他、关闭右侧标签页）。
  - **标签内容**：呈现 Home 或服务器名（160px 上限省略号截断 + Tooltip 全称恢复）、UAT/Prod 风险色点、等宽 `db N` 徽标与 `⌘1–⌘8` 快捷键提示。
  - **启动与后台管家**：启动时自动恢复上次未关闭的工作区标签列表；异步清理过期的本地回收站（Trash）与历史快照；桌面端支持一键导出脱敏诊断压缩包（Diagnostics Bundle，聚合日志、构建信息与 sanitized 配置）。
- **已完成的 UI 优化**：
  - 修复标签栏与侧边栏 `ZEDIS` 行之间 8px 底边阶梯错位，采用全高停靠式（`EDITOR_KEY_BAR_HEIGHT` = `40px`）IDE 标签并与工作区无缝融合；
  - 激活标签采用 `theme().background`（消除底部分割线直连下方内容区）+ 顶部 `2px` `theme().primary` 指示条 + 主色前置图标 + 半粗体前景标题，与沉底槽 `workspace_tab_bar_background(cx)` 形成鲜明一眼对比；
  - 弱化未激活标签关闭 `×` 图标常态透明度（45%，悬停恢复 100%），消除一排刺眼白叉视觉干扰；
  - 统一标题 160px 省略截断 + Tooltip、等宽字体 `db N` 与快捷键、UAT/Prod 环境圆点，补充固定右侧带左侧竖分割线的 `+` 新建标签单元及完整 `#[gpui::test]` 集成覆盖。

---

### #2 顶部标题栏与在线更新浮层 (`ZedisTitleBar`, `update_prompt`) — ✅ 已完成

- **源码位置**：`src/views/title_bar.rs`、`src/views/update_prompt.rs`
- **入口与快捷键**：应用窗口最顶栏；设置齿轮菜单覆盖全局入口。
- **使用频率理由**：全局高频状态感知栏（当前连接的生产环境警示、主题/语言切换、全局面板入口）。
- **布局结构**：左区（macOS 红绿灯留白 / Web 端 `ZEDIS` 品牌回首页按钮）+ 中区（居中当前活跃服务器身份胶囊 `TITLE_SERVER_IDENTITY_ID`，含完整信息 Tooltip）+ 右区（新版本更新徽标、设置齿轮下拉菜单、Linux CSD/Windows 窗口控制按钮）。
- **完整功能与交互**：
  - **连接身份与生产警示**：连接服务器后居中显示 `名称 · host:port · db N`；当连接标记为 **Production（生产环境）** 时，不仅展示实心洋红 `[PROD]` 胶囊，还会为整条标题栏铺上低饱和度警示底色并对图标和元信息着对比色。
  - **在线自更新浮层（Desktop `update_prompt`）**：后台检测到新版本时在右侧亮起 `UPDATE_CHIP_ID` 胶囊（展开期间保持选中高亮）；点击弹出 `Popover`，顶行展示版本号、等宽 `current → version` 升级路径及发行说明外部链接，中部内嵌圆角卡片视口渲染 Markdown Release Notes（上限 20,000 字符），底部提供统一 `.small()` 尺寸的 `Skip this version` / `Update and restart` 或等宽实时下载进度条。
  - **全局齿轮菜单**：
    - 外观模式切换（`Light` / `Dark` / `System`）及 6 套内置配色主题；
    - 8 国界面语言即时切换（`en / zh / ru / ja / pt / es / de / fr`）；
    - 全局工具快捷入口：Command Palette (`⌘K`)、Multi-DB Search (`⌘⇧F`)、Keyboard Shortcuts (`⌘/`)；
    - 桌面端配置子菜单：Protobuf 配置、自定义脚本视图（Script Viewers）、其他设置（`⌘,`）；
    - 维护与诊断：检查更新、打开日志目录、导出诊断包（Diagnostics Bundle）、关于窗口；
    - 会话控制：Web 端登录态下展示 `Account…`（账户与管理后台）与 `Sign out`；桌面端展示 `Quit`。
- **已完成的 UI 优化**：
  - 修复 PROD 标题栏内 `[PROD]` 标签同色融合不可见的问题，改为 `resolve_tag_chip(Some("magenta"), dark)` 高对比实心洋红胶囊与 `theme.radius * 0.5` 圆角，并调整 PROD 图标/元数据前景色；
  - 为居中连接名（`220px`）与 `host · db N`（`200px`）补充 `min_w_0()` 省略号截断及全称 Tooltip 恢复，防止窄窗（`960px`）下挤压左右控制区；
  - 统一标题栏按钮簇间距（`gap_1().mr_1()`）、`UPDATE_CHIP_ID` 打开期 `.selected()` 态与 `list_hover` 悬停态，并为 `update_prompt` 补齐 Release 外链、基于字号基准的 Markdown 标题比例、内嵌卡片质感及等宽版本/下载数值排版。

---

### #3 左侧服务器侧边栏 (`ZedisSidebar`) — ✅ 已完成

- **源码位置**：`src/views/sidebar.rs`、`src/views/sidebar/menu.rs`
- **入口与快捷键**：主窗口左侧常驻导航栏（可通过顶部折叠箭头或窄轨底部展开按钮持久化切换 `sidebar_collapsed`）。
- **使用频率理由**：日常多环境（Dev / UAT / Prod）快速切换连接与新建标签页的主入口。
- **布局结构**：采用主题 `sidebar` 面板表面与 `sidebar_border` 右分割线，顶部固定 `EDITOR_KEY_BAR_HEIGHT`（`40px`）品牌头（`ZEDIS` 回首页按钮 + `.small()` 折叠按钮）→ 可滚动分组/服务器列表（支持空闲态提示与折叠分组计数）→ 折叠窄轨底部固定 `STATUS_BAR_HEIGHT`（`35px`）展开控制栏。
- **完整功能与交互**：
  - **双态自适应布局**：
    - **展开模式**：渲染对齐首行图标脊线的可焦点分组折叠头（带 `aria_expanded` 与等宽数量角标）、数据库图标、服务器名称、按 `text_xs` + `rems` 缩放的 UAT/Prod 环境短标签、以及实时连接健康圆点（已连接绿点 / 重连中黄点 / 断开红点）。
    - **折叠窄轨模式（Collapsed Rail）**：为每台服务器生成去重缩写徽标（2 位 ASCII 字母或首字 CJK 汉字）+ 紧贴 `28×28` 按钮的选中/悬停/焦点环边界 + 右下角以 `theme.sidebar` 描边隔离的状态圆点 + 悬停完整名称 Tooltip，底部展开按钮补全本地化 `"Expand"` Tooltip。
  - **多修饰键与键盘导航**：全部首页、分组与服务器项均由 `gpui_kit::base::Button` 驱动，支持 `Tab` 键焦点遍历、外扩 3px 同心聚焦环无裁切渲染、`Enter`/`Space` 激活，以及普通点击 / `⌘+Click` / `⌘⇧+Click` 多标签路由。
  - **右键上下文菜单（`ServerMenu`）**：打开、在新标签页打开、编辑连接、克隆连接、连接诊断（打开 `ZedisConnectionDiagnostics`）、移动到分组子菜单（含新建分组弹窗 `open_new_group_dialog`）、断开/重连、删除连接；右键菜单展开期间触发行维持打开高亮态（`list_hover`）并抑制自身 tooltip。
- **已完成的 UI 优化**：
  - 修复侧边栏背景/分割线与状态圆点描边脱离 `theme.sidebar` / `theme.sidebar_border` 令牌的问题，并把 `Home`（`ZEDIS`）与选中服务器统一改用 `theme.sidebar_accent` / `sidebar_accent_foreground` 表面填充与 `list_hover` 分层（消除选中行被 hover 冲淡与窄轨多余背景矩形）；
  - 将手工 `ListItem` 与分组 `div().on_click(...)` 升级为带 `FocusHandle`、`aria_selected` / `aria_expanded` 与外扩同心 `focus_ring_style` 的可键盘交互 `BaseButton`，缩小 `home_item` 高度至 `h_7()`（`28px`）确保上下聚焦环留白充足；
  - 对齐分组标题左缘（`mx_2 + px_2`）与图标脊线、将环境标签从硬编码 `px(10.)` 升级至 `text_xs` + `rems` 比例、补充空列表提示与窄轨 `sidebar.expand` 8 国语言 Tooltip，并将右键菜单与新建分组弹窗拆分至独立子模块 `src/views/sidebar/menu.rs`。

---

### #4 底部状态栏与工具总菜单 (`ZedisStatusBar`) ✅ 已完成

- **源码位置**：`src/views/status_bar.rs`、`status_bar/editor_controls.rs`、`status_bar/server_status.rs`、`status_bar/telemetry.rs`、`status_bar/tools_menu.rs`
- **入口与快捷键**：连接服务器后常驻主窗口最底栏；`⌘J` 切换终端按钮位于左侧。
- **使用频率理由**：承载 DB 切换、生产写锁解锁、**全部 20+ 个服务端工具面板的总入口（Tools 菜单）**、实时遥测跳转胶囊以及 String 编码查看器切换。
- **布局结构**：采用四档响应式宽度预算 `BarFit`（`Full` → `NoUnits` 省略单位 → `FoldedEditorControls` 折叠右侧编辑器按钮为下拉框 → `LinkIconOnly` 仅保留连接图标），分左、中、右三区。
- **完整功能与交互**：
  - **左区（会话控制与工具箱）**：
    - 环境标签、`Replica` 从库只读徽章、CLI 终端开关（`⌘J`）；
    - `DB: N` 选择下拉菜单（自动读取 `INFO keyspace` 在每个 DB 旁展示实时键数量）；
    - 客户端暂停解除胶囊：当检测到服务端处于 `CLIENT PAUSE` 时亮起，一键发送 `CLIENT UNPAUSE`；
    - **只读开关 / 生产 15 分钟写锁按钮（ADR 14）**：普通连接一键切换只读；生产环境默认写锁定，点击经安全确认后解锁 15 分钟并实时显示倒计时，支持随时重新上锁；
    - **Tools 分类总菜单（4 大二级子菜单 + 顶层项）**：
      - *Observability（可观测性）*：Metrics、Slowlog/Perf、Memory Analysis、Clients、Monitor、Server Load、Hotkeys、TimeSeries Explorer；
      - *Query & Scripting（查询与脚本）*：Workbench、Value Search、RediSearch、Lua Scripts、Functions、Pub/Sub；
      - *Keys & Data（键与数据）*：Import Keys、Export Keys、Compare DBs、Recycle Bin；
      - *Administration（运维管理）*：Persistence、Keyspace Notifications、Config、ACL、`FLUSHDB`、`FLUSHALL`；
      - *顶层直达*：Topology（集群/哨兵/主从拓扑）、`Server Capabilities… · N limited`（能力受限计数提示）、`Troubleshooting Report…`（一键排障报告）；
      - 对当前服务器版本不支持或命令被代理禁用的菜单项，自动灰显并附带原因说明。
    - 键扫描进度胶囊：`ChevronsDown` 继续扫描按钮 + `已加载 / 总键数` 计数器 + 集群/主从 `masters / replicas` 拓扑摘要。
  - **中区**：右对齐瞬态错误提示文案。
  - **右区（链路控制、遥测胶囊与格式器）**：
    - 连接状态开关（`Connected` / `Reconnecting` / `Offline` 点击断开或重连）+ **慢脚本熔断按钮**（当服务端返回 `BUSY` 时亮起红色 `Kill Script`，一键执行 `SCRIPT KILL` / `FUNCTION KILL`）；
    - **4 个实时遥测跳转胶囊**：延迟 ms（点击进 `Metrics`，存在活跃阈值告警时显示黄色警示图标）、内存占用（进 `MemoryAnalysis`）、连接/阻塞客户端数（进 `Clients`）、慢日志计数（进 `Slowlog`）；
    - **String 编辑器专属控制**：自动换行（`Soft Wrap`）开关、自动识别的 `DataFormat` 格式徽章、`Auto | Plain | Hex` 查看模式切换。
- **UI 优化关注点**：
  - 状态栏信息密度极高，需重点在最小窗口宽度与放大 UI 字号（如 `18px`）下验证 `BarFit` 四档折叠阈值是否出现文字截断或按钮挤压。
  - 确保整条状态栏所有控件统一使用同一尺寸档位（`.xsmall()` / `.small()`），消除高低参差。

---

### #5 内容区工作台路由与不支持占位页 (`ZedisContent`, `ZedisUnsupportedPanel`) ✅ 已完成

- **源码位置**：`src/views/content.rs`、`src/views/unsupported_panel.rs`
- **入口与快捷键**：主工作区中央画布；负责拦截并将 `⌘F` 转发给当前激活面板。
- **使用频率理由**：所有主界面视图的布局骨架与分栏拖拽容器。
- **完整功能与交互**：
  - **分栏持久化**：在 `ServerView::Editor` 下维护左侧 `ZedisKeyTree` 与右侧 `ZedisEditor`（或 `Pub/Sub`）的可拖拽水平分割线（宽度持久化至 `key_tree_width`），以及底部 `ZedisTerminal` 的垂直分割面板。
  - **能力拦截降级（`ZedisUnsupportedPanel`）**：当用户通过历史记录或命令面板进入当前服务器不支持的面板（如代理禁用了 `CONFIG` 或 `ACL`）时，自动展示清晰的受限原因说明，并提供 `Re-probe`（重新探测权限）与 `Back to Editor` 按钮。
- **UI 优化关注点**：
  - 检查水平/垂直 `Resizable` 分割线在双侧面板各自带边框时是否产生 2px 重叠粗线。
  - `ZedisUnsupportedPanel` 空状态排版应与全局空状态视觉规范（居中图标 + 标题 + 原因 + 单一操作按钮）对齐。

---

### #6 首页服务器连接管理与诊断/导入导出 (`ZedisServers` 套件) ✅ 已完成

- **源码位置**：`src/views/servers.rs`（1,745 行）、`servers/form.rs`、`servers/transfer.rs`、`export_servers_dialog.rs`、`connection_diagnostics.rs`
- **入口与快捷键**：`Route::Home`（应用启动默认首页、点击左上角 `ZEDIS` 标志）。
- **使用频率理由**：每次新增环境、修改密码/SSH 隧道、排查连不上服务器或团队分享连接配置时的必经入口。
- **布局结构**：顶部工具栏（连接总数/筛选数徽章、实时搜索框、排序选择器、新建/导入/导出按钮）+ 可折叠分组卡片网格（响应式 1 / 2 / 3 列 `ZedisCard`）；无连接时展示居中大图引导空状态。
- **完整功能与交互**：
  - **快速检索与连接**：搜索框支持按名称、地址、分组实时过滤；当过滤结果仅剩 **1 台服务器**时，按 `Enter` 直接发起连接；支持按手动顺序（`Manual`，悬停卡片显示 `↑` / `↓` 微调按钮）、名称（`Name`）、最近使用（`LastUsed`）排序。
  - **卡片信息与操作**：显示连接地址、DB、环境色标（Dev / Test / UAT / Staging / Prod）、相对最后访问/编辑时间、一键 `Edit` 与 `⋯` 更多菜单（克隆、单连接导出、二次确认删除）。
  - **6 标签页新建/编辑连接弹窗（`servers/form.rs`）**：
    1. *General*：名称、分组、单机/集群/哨兵地址、用户名/密码、默认 DB；Web 模式下支持管理员勾选 `Shared` 共享给团队；
    2. *TLS*：启用 TLS、跳过证书校验、SNI 覆盖、mTLS 客户端证书/私钥与 CA 证书选择；
    3. *SSH*：SSH 跳板机隧道地址、端口、用户名、密码或私钥文件及口令；
    4. *Advanced*：连接类型覆盖、Sentinel 主节点名称自动拉取（`Fetch Masters`）、连接/响应超时覆盖；
    5. *Safety（安全护栏）*：环境标签（Dev/Test/UAT/Staging/Prod）、写权限策略（允许写 / 15 分钟写锁 / 严格只读）、每次写操作二次确认开关、指标阈值告警开关；
    6. *Keys*：单连接命名空间分隔符覆盖、每次 `SCAN` 步长覆盖；
    - 弹窗底部固定提供 `Test Connection`（快速测通）与 `Diagnose`（分阶段链路诊断）。
  - **连接链路分步诊断器（`ZedisConnectionDiagnostics`）**：按瀑布流依次探测 `DNS 解析` → `TCP 握手` → `SSH 认证` → `SSH 隧道转发` → `TLS 握手与证书` → `AUTH 鉴权` → `PING 往返`，逐项展示耗时（ms）与针对失败环节的本地化排障建议（`DiagHint`）。
  - **多格式导入与加密分享导出（`transfer.rs`, `export_servers_dialog.rs`）**：
    - 支持导入 Zedis JSON、多行 `redis://` / `rediss://` URI、Redis Insight、Another Redis Desktop Manager (`.ano`)、TinyRDM (`.yaml`) 以及 `ZEDIS1.…` 口令加密分享串（支持文件选择或直接拖拽进入弹窗）；
    - 导出弹窗支持多选服务器、开关“包含密码/私钥敏感信息”、设置口令生成 `ZEDIS1.…` 加密短令牌或保存 JSON 文件。
- **UI 优化关注点**：
  - **单文件超限**：`servers.rs` 已达 1,745 行（超过 1,500 行红线），做 UI 优化时应顺带将卡片网格渲染或分组管理拆入 `servers/` 子模块。
  - **悬停专属操作隐患**：手动排序下的 `↑` / `↓` 按钮目前仅在悬停时浮现，需确保键盘焦点同样可达且不引起卡片内部布局抖动（无位移原则）。
  - 统一顶部工具栏搜索框、排序下拉框与 `New / Import / Export` 按钮高度与间距（最多保留 `New` 一个 `.primary()` 按钮）。

---

### #7 键树导航、过滤与批量模式操作 (`ZedisKeyTree`) ✅ 已完成

- **源码位置**：`src/views/key_tree.rs`、`key_tree/render.rs`、`bar_menus.rs`、`row_menu.rs`、`add_key.rs`、`delegate.rs`、`pattern_ops.rs`、`folder_delete.rs`、`key_actions.rs`
- **入口与快捷键**：`ServerView::Editor` 左侧核心面板；`⌘R` 刷新键树。
- **使用频率理由**：Zedis 使用时长排名第一的交互控件，承载十万级键空间的命名空间折叠浏览、多维过滤与批量管理。
- **布局结构**：顶部搜索与过滤操作栏 → 活跃过滤条件胶囊栏（仅激活过滤时显示）/ 多选计数条 / 正则语法错误提示条 → 吸顶父级命名空间面包屑（`ktree-sticky-path`）→ 虚拟化滚动树列表（`List`）+ 右上角轻量刷新 Spinner。
- **完整功能与交互**：
  - **多维组合过滤器（`FilterMenu`）**：
    - 搜索历史、最近打开的键、收藏夹（Favorites）快捷回填；
    - 查询匹配模式：`All (*pattern*)` 模糊包含、`Prefix (pattern*)` 前缀、`Exact (=key)` 精确直查（精确模式直接走 `EXISTS/TYPE` 绕过全库 `SCAN`）；
    - 按数据类型过滤（涵盖基础类型及服务端已加载的 JSON / TimeSeries / Bloom / VectorSet 模块类型）；
    - 按本地颜色标签（Tag Color）过滤；
    - 按剩余 TTL 区间过滤（永不过期、`<1m`、`<1h`、`<24h`、`>24h` 等）；
    - 同级节点排序（按名称、TTL、数据类型升/降序）。
  - **树列表高级特性**：
    - 按分隔符（默认 `:`）自动构建目录树，支持单子节点路径自动折叠压缩（如 `a:b:c` 合并展示）；
    - 固定槽位对齐：缩进引导线、3px 左侧颜色标签条、`KeyTypeBadge` 类型徽章、键名、备注/标签 Tooltip、右侧等宽 `TTL` 倒计时（悬停行时平滑替换为快捷删除图标）；
    - `Shift+Click` 范围多选与 `⌘+Click` 离散多选（自动浮现行首复选框与批量删除/批量打标栏）；
    - 目录末尾合成 `Load more` 游标续扫行；当代理禁用 `SCAN` 时优雅退化为顶部手动输入精确键名直查模式；
    - **过期键保留策略**：点开某键发现已过期（`TTL -2`）时保留树节点并置灰标记，不突兀吞行。
  - **右键菜单与三大核心子弹窗**：
    - *键右键菜单*：收藏/取消收藏、设置颜色标签与备注、复制键名/值/`zedis://` 深链、克隆/跨服复制、修改 TTL、导出、删除；
    - *目录右键菜单*：以该前缀新建键、全量加载该目录、打开批量模式操作、在 Keyspace Notifications 中监听该前缀、导出该目录、删除整个目录（弹出 `FolderDeletePreview` 异步预估匹配键数与抽样内存占用）；
    - *新建键弹窗（`add_key.rs`）*：支持 **10 种数据类型**（String、List、Set、ZSet、Hash [含 Redis 7.4+ 字段级 TTL]、Stream、JSON、TimeSeries、Bloom、Cuckoo），集合类型支持多行 TSV / NDJSON 一次性批量粘贴建键；
    - *模式批量操作弹窗（`PatternOpsForm`）*：输入 `SCAN MATCH` 通配符先执行 Dry-Run 统计命中键数，再一键执行批量删除（`DEL`/`UNLINK`）、批量设过期（`EXPIRE` 支持 `NX/XX/GT/LT`）或批量持久化（`PERSIST`）；
    - *顶栏 `⋯` 更多菜单（`TreeMoreMenu`）*：刷新、全部折叠、开启多选模式、批量模式操作、自动定时刷新（`0–120s`）、切换 `Pub/Sub` 模式、切换扁平列表视图（`Flat View`）、开启客户端正则二次过滤（`Regex Filter`）。
- **UI 优化关注点**：
  - **【高优】修复暗色模式 Hover 喧宾夺主问题**：`src/views/key_tree/delegate.rs` 中暗色主题下树节点悬停底色使用了高纯度 `blue.alpha(0.3)`，视觉权重反而强于选中态 `foreground.alpha(0.1)`，直接违背设计规范“Hover 必须弱于 Selected（统一采用 `list_hover` 与 `list_active`）”。
  - 检查悬停时右侧 `TTL` 切换为删除图标时是否保持固定列宽，杜绝键名发生哪怕 1–2px 的水平跳动。
  - 核查顶部过滤输入栏 + `+` 按钮 + `⋯` 按钮 + 活跃过滤胶囊在窄侧边栏宽度（如 `220px`）下的折行与留白。

---

### #8 键值编辑器外壳与版本/冲突管理 (`ZedisEditor`) ✅ 已完成

- **源码位置**：`src/views/editor.rs`、`editor/toolbar.rs`、`editor/more_menu.rs`、`editor/render.rs`、`editor/dialogs.rs`
- **入口与快捷键**：`ServerView::Editor` 右侧主画布；`⌘S` 保存修改。
- **使用频率理由**：所有选中键的元信息头部、TTL 控制、保存冲突拦截与高级键级动作中枢。
- **布局结构**：顶部单行紧凑元信息与动作栏（Key Header Bar）+ 下方自适应填满的数据类型子编辑器视口。
- **完整功能与交互**：
  - **完备的状态页设计**：
    - *未选键空态*：展示高频快捷键速查卡与 `+ New Key` 引导按钮；
    - *防闪烁延迟骨架屏*：加载超过 300ms 才渲染柔和骨架条，避免极速切键时频闪；
    - *超大 String 安全门控*：当值大小超过 `MAX_INLINE_VALUE_SIZE` 时不直接拉爆内存，而是展示体积告警卡，提供基于 `GETRANGE` 的分页预览器（上一页/下一页 + 按需加载完整值）或可随时取消的强制全量加载；
    - *未知模块类型兜底*：遇到未内置解析器的第三方 Redis Module 键时，自动通过 `DUMP` 提供只读十六进制序列化视图。
  - **顶部元信息与核心控制**：
    - 一键复制键名、星标收藏开关、`KeyTypeBadge`、智能中间省略键名（保留首尾关键标识）、体积胶囊、只读锁标志、`OBJECT ENCODING` 编码与 `IDLETIME` / `FREQ` 冷热度徽章；
    - **CAS 乐观锁冲突检测保存（`Save ⌘S`）**：在 Redis 8.4+ 上自动利用 `SET … IFEQ` 检测编辑期间是否被其他客户端并发修改，若冲突则弹出 `open_save_conflict_dialog`（选择强制覆盖、放弃并重新加载、或打开 `ZedisValueDiff` 逐行比对差异）；
    - **内联 TTL 编辑器与下拉菜单**：支持直接点击修改相对秒数，或下拉选择设置时长、设置绝对过期时间（`Expire At`）、移除过期（`Persist`）；
    - **手动刷新与定时轮询**：一键重载 + `Off / 1s / 2s / 5s / 10s / 30s / 60s` 自动刷新；
    - **单键时间线按钮**：一键调起 `ZedisKeyTimelineDialog` 实时追踪该键变化。
  - **`⋯` 更多操作菜单（`MoreActions`）**：
    - 以 Bitmap 位图模式打开（String 键）、导出值到文件 / 从文件导入覆盖值、查找与替换、JSON 格式化/压缩；
    - 会话修改审计（`Session Change Log`）、本地历史版本快照（`Version History`，自动在本地 `redb` 保留最近 10 次修改前快照，支持一键恢复或 Diff 比对）；
    - 集合多键运算子菜单（`KeyOpAction`）、安全重命名（默认 `RENAMENX`，目标存在时二次确认覆盖，并自动迁移本地标签/收藏/历史记录）、复制到其他服务器（自动执行 `copy_preflight` 跨引擎兼容检查）、复制 `zedis://` 链接、与其他服务器同名键 Diff 比对、String `LCS` 最长公共子串比对、删除键。
- **UI 优化关注点**：
  - 顶部栏承载了键名、4–5 个元数据徽章与 5–6 个操作按钮，需重点审视徽章中性化策略（除只读警告外全部采用低对比中性胶囊），把唯一 `.primary()` 预算留给有未保存改动时的 `Save` 按钮。
  - 核查窄窗口下键名中间截断（Middle Truncate）的最小可读宽度保底。

---

### #9 String / Bytes / JSON 树与多格式编辑器 (`ZedisBytesEditor`, `ZedisJsonTree`) ✅ 已完成

- **源码位置**：`src/views/bytes_editor.rs`（1,457 行）、`src/views/json_tree.rs`、`src/views/jsonpath_completion.rs`
- **入口与快捷键**：打开任意 `String` 或 `ReJSON-RL` 键自动挂载；`⌘F` 页内查找。
- **使用频率理由**：Redis 中占比最高的缓存对象（JSON、Protobuf、压缩字节流、序列化数据、图片验证码）均在此查看与编辑。
- **布局结构**：可选顶部解码链路/只读提示条 → 主编辑视口（`Text` 代码编辑器 / `Tree` 结构化树 / `Image` 图片预览 / `Hex` 十六进制视图）→ 可折叠底部 `JSONPath` 实时查询拆分栏。
- **完整功能与交互**：
  - **6 阶自动嗅探与多层解包管线**：自动探测并链式解包 `Gzip / Zstd / Snappy / LZ4 / Zlib / Brotli` 压缩层 + `JSON / MsgPack / Protobuf（关联 Route::Protos）/ PHP Serialize / Java & Python Pickle / 自定义外部脚本解码器（Route::Scripts）/ PNG·JPEG·GIF·WebP·SVG 图像 / 原始二进制 Hex`。
  - **四大视图形态无缝切换**：
    1. *代码文本编辑器（`Editor`）*：超过 `512 kB` 自动关闭软换行防卡顿，超大文本自动限制语法高亮字节上限；当处于解码视图或连接只读时，采用无暗淡遮罩的只读模式（拦截 `InputEvent::Change` 回滚，保证文字清晰可读且支持自由划选复制）；
    2. *交互式 JSON 树（`ZedisJsonTree`）*：支持 `Text | Tree` 一键切换，虚拟化展开大 JSON；右键节点不仅支持复制 JSONPath / 节点值 / 填入查询栏，还支持对 `ReJSON-RL` 或普通 JSON String **原地局部修改**（`JSON.SET` 编辑值、新增子字段/数组元素、`JSON.ARRAPPEND`、`JSON.NUMINCRBY` 数值自增、`JSON.TOGGLE` 布尔翻转、`JSON.STRAPPEND`、`JSON.CLEAR`、`JSON.DEL` 删除节点）；
    3. *像素级图像查看器（`Image`）*：针对 `≤128px` 的小图标或图形验证码自动启用整数倍最近邻锐利放大（Nearest-Neighbor），显示图像尺寸与字节大小；
    4. *虚拟化十六进制编辑器（`Hex`）*：支持只读 `Dump`（偏移量 + 十六进制 + ASCII 对照）与可写回原始字节流的 `Edit` 模式切换。
  - **带自动补全的 `JSONPath` 查询栏**：输入 `$..` 表达式时由 `JsonPathCompletionProvider` 根据当前 JSON 结构实时补全字段名，右侧/下方分栏即时展示提取结果。
- **UI 优化关注点**：
  - `bytes_editor.rs` 已接近 1,500 行上限（1,457 行），后续优化工具条或 Hex/Image 子视图时建议拆分为 `bytes_editor/` 子模块。
  - 检查 `Text | Tree` 切换开关、解码提示条与底部状态栏右侧 `Auto | Plain | Hex` 控件之间的职责边界，避免用户困惑两处模式切换的关系。

---

### #10 集合通用表格框架与 Hash / List / Set / ZSet 编辑器 (`ZedisKvTable` 等) ✅ 已完成

- **源码位置**：`src/views/kv_table.rs`（+ `kv_table/toolbar.rs`, `entry_panel.rs`）、`hash_editor.rs`（+ `hash_sample_dialog.rs`）、`list_editor.rs`（+ `list_editor/pop_move_dialog.rs`）、`set_editor.rs`（+ `set_ismember_dialog.rs`）、`zset_editor.rs`（+ `zset_editor/zmpop_dialog.rs`, `score_filter_dialog.rs`）
- **入口与快捷键**：选中 `Hash`、`List`、`Set`、`ZSet` 类型键自动挂载。
- **使用频率理由**：四大经典集合类型共享同一套主从表格与右侧详情编辑抽屉，优化 `ZedisKvTable` 即可一次性提升全部集合编辑体验。
- **布局结构**：顶部集合专属工具栏（搜索框、专属过滤/采样按钮、新增行、批量删除、导出 CSV）→ 左主虚拟化数据表 + 右侧可拖拽宽度详情查看/编辑抽屉（`entry_panel.rs`）→ 底部 `已加载 / 总元素数` 与加载更多状态栏。
- **完整功能与交互**：
  - **`ZedisKvTable` 共享能力**：
    - 支持多选行批量删除、`+ Add` 新增元素（支持直接粘贴多行 TSV 或 NDJSON 一次性写入多条字段/成员）、导出当前已加载表格为 CSV；
    - 区分服务端游标过滤（`HSCAN` / `SSCAN` / `ZSCAN`）与客户端内存过滤（`List` / `Stream` 输入关键字时明确提示仅过滤已加载页）；
    - 右侧详情抽屉上方自动对长字段值提供 `Decoded/Text | Tree | Hex` 结构化预览，下方提供表单输入框保存修改。
  - **各类型专属增强能力**：
    - **Hash (`ZedisHashEditor`)**：支持 **Redis 7.4+ 字段级过期时间（Field TTL）** 列显示、单独设置字段过期（`HEXPIRE` / `HPERSIST`）、“仅看即将过期字段”过滤开关，以及 `HRANDFIELD` 随机抽样弹窗（`ZedisHashSampleDialog`）；
    - **List (`ZedisListEditor`)**：`LRANGE` 分页加载，支持选择插入头部（`LPUSH`）或尾部（`RPUSH`），原地按索引修改（`LSET`），以及弹出/跨列表原子移动弹窗（`ZedisListPopMoveDialog`：`LMOVE` / `LMPOP`）；
    - **Set (`ZedisSetEditor`)**：`SSCAN` 增量加载、`SADD`/`SREM`，内置批量成员存在性校验弹窗（`ZedisSetIsMemberDialog`，基于 `SMISMEMBER` 一次校验多行候选值并标记命中/未命中）；
    - **ZSet (`ZedisZsetEditor`)**：升序/降序切换（`ZRANGE` / `ZREVRANGE`）、按分数区间过滤弹窗（`ZedisZsetScoreFilterDialog`：`ZRANGEBYSCORE` 支持 `-inf`/`+inf` 与开闭区间）、批量最值弹出弹窗（`ZedisZmpopDialog`：`ZMPOP`），以及 **自动识别 WGS84 Geohash 坐标分数** 并在工具栏浮现一键切换至 `View on Map`（`ZedisGeoMap` 地图视图）。
- **UI 优化关注点**：
  - **【高优】替换右侧抽屉中的裸 `Label` 预览**：`src/views/kv_table/entry_panel.rs` 目前使用裸 `Label` 渲染解码后的文本预览且带有硬编码 `.rounded(px(6.))`，导致长 JSON/文本无法鼠标拖选局部复制；应替换为只读 `Editor` + `theme.radius`。
  - 统一四个集合工具栏中辅助操作按钮（如 `Sample`、`Check Members`、`Score Filter`、`Pop/Move`）的图标+文案密度与 `.outline().small()` 样式。

---

### #11 Stream 消息流、消费者组与 PEL 编辑器 (`ZedisStreamEditor`) ✅ 已完成

- **源码位置**：`src/views/stream_editor.rs`、`stream_editor/info_pending.rs`、`stream_editor/tail.rs`、`stream_editor/dialogs.rs`
- **入口与快捷键**：选中 `Stream` 类型键自动挂载。
- **使用频率理由**：消息队列排障核心面板，集成了消息浏览、实时阻塞跟尾（Tail）以及完整的 Consumer Group / Pending Entries List (PEL) 运维闭环。
- **布局结构**：双子视图切换——**Messages（消息数据表视图，复用 `ZedisKvTable`）** 与 **Stream Info & Groups（消费者组与未确认消息深度管理视图）**。
- **完整功能与交互**：
  - **消息流浏览与实时 Tail**：
    - 支持正序/倒序（`XREVRANGE` 最新消息置顶）浏览、`XADD` 添加多字段消息（自动 `*` ID 或指定 ID）、`XDEL` 删除消息；
    - **双模式实时跟尾（Live Tail）**：点击 Tail 可选择普通流跟尾（独立连接运行 `XREAD BLOCK $`）或模拟消费者跟尾（运行 `XREADGROUP BLOCK >` 实时拉取分配给指定 Group/Consumer 的新消息）。
  - **Stream 概览与裁剪维护**：
    - 展示首尾 Entry ID、消息总数、基数树节点统计，以及 **Redis 8.6+ IDMP 幂等生产者统计卡片**；
    - 支持 `XSETID` 调整最后生成 ID，以及高级 `XTRIM` 弹窗（支持 `MAXLEN` / `MINID`、精确 `=` 或近似 `~`、`LIMIT`，以及 **Redis 8.2+ `KEEPREF` / `DELREF` / `ACKED`** 引用策略）。
  - **消费者组与 PEL 全生命周期运维（`info_pending.rs`）**：
    - 创建组（`XGROUP CREATE`，支持 `$`、`0`、自定义 ID、`MKSTREAM`、`ENTRIESREAD`）；
    - 合并的 **Groups + Consumers 树形状态表**：直观查看每个 Group 的 `lag`、`pending` 数、`last-delivered-id` 及旗下每个 Consumer 的空闲时长，支持按组一键批量 `XACK`、创建消费者（`CREATECONSUMER`）、删除消费者（`DELCONSUMER`）、重置游标（`SETID`）、自动认领超时消息（`XAUTOCLAIM`）及销毁组（`DESTROY`）；
    - **交互式 `XPENDING` 检查器**：按组胶囊切换查看每条未确认消息的投递次数与空闲时长，支持过滤、一键跳回消息详情（Inspect）、批量确认当前列表（`Ack shown`），以及单条执行 `XACK`、**Redis 8.2+ `XACKDEL`**（确认并删除）、**Redis 8.8+ `XNACK`** 或跨消费者转交 `XCLAIM`。
- **UI 优化关注点**：
  - `Stream Info & Groups` 子页面包含多张卡片、树形表格与 PEL 列表，信息层级丰富，需重点审视卡片间距（`gap_4` / `gap_6`）与“禁止卡片套卡片（No card in a card）”准则，确保单页滚动条贴合最右侧边缘。

---

## 四、P1 梯队：高频快捷浮层、CLI 终端与日常键级弹窗

这一梯队是熟练用户与高频排障场景下随时通过快捷键或右键菜单唤起的“随手工具”。

---

### #12 内嵌交互式 CLI 终端 (`ZedisTerminal`) ✅ 已完成

- **源码位置**：`src/views/terminal.rs`、`terminal/completion.rs`、`terminal/execution.rs`、`terminal/transcript.rs`
- **入口与快捷键**：底部状态栏终端按钮或全局快捷键 `⌘J`。
- **使用频率理由**：GUI 之外最直接的补充交互层，用于验证原生命令、事务块与批量命令脚本。
- **布局结构**：顶部紧凑控制栏（会话状态、`Text | Table | JSON` 渲染格式切换、`REPL | Batch` 模式切换、复制/保存/清空记录、关闭按钮）→ 中部虚拟化历史输出回滚区（`transcript.rs`）→ 底部命令输入区 + 浮动编号自动补全弹窗。
- **完整功能与交互**：
  - **连接完全隔离（`TerminalSession`）**：独占一条专属连接，用户在终端里执行 `SELECT`、`AUTH`、`CLIENT SETNAME` 或开启 `MULTI` 事务绝不污染 GUI 键树连接池；当终端 DB 与主界面不一致时自动在提示符显示 `[db]` 提醒。
  - **实时可追溯多格式输出（`ReplyFormat`）**：切换 `Text`（经典 `redis-cli` 缩进）、`Table`（将数组/Hash/事务块结果渲染为对齐 ASCII 表格）或 `JSON` 时，**已执行的历史记录也会即时重新格式化渲染**。
  - **双输入模式**：单行自动增高 `REPL` 模式（粘贴带行首 `redis-cli` 前缀的命令时自动剥离前缀）与多行脚本 `Batch` 模式（`⌘Enter` 逐行批量执行）。
  - **智能补全、历史反向搜索与 AI 辅助**：
    - 输入命令时浮现带 `1–9` 数字键直选的语法提示弹窗（含参数签名、简介与跳往官方 Redis 文档外链）；
    - 支持标准终端 `Ctrl+R` `(reverse-i-search)` 历史命令增量检索；
    - **桌面端 `? <自然语言>` AI 命令生成**：输入如 `? 找出 user: 下所有剩余过期小于 60 秒的键` 回车后，调用配置的 OpenAI 兼容模型将建议命令回填至输入框（**绝不自动执行**，由人工审阅后回车运行）；
    - **危险与劫持命令拦截**：受生产写锁与高危命令弹窗保护；对会永久阻塞或劫持协议的命令（`SUBSCRIBE`、`MONITOR`、`BLPOP`、`PSYNC`）友好拦截并引导一键跳转至对应的专用 GUI 面板。
- **UI 优化关注点**：
  - 检查自动补全浮窗在终端高度较矮时是否能够自动向上弹出而不被窗口下沿裁切。
  - 统一终端顶部栏 `Text | Table | JSON` 与 `REPL | Batch` 两组分段开关的选中态视觉表现。

---

### #13 全局命令面板 (`ZedisCommandPalette`) ✅ 已完成

- **源码位置**：`src/views/command_palette.rs`
- **入口与快捷键**：`⌘K`（macOS）/ `Ctrl+K`（Windows/Linux），或右上角齿轮菜单。
- **使用频率理由**：纯键盘流用户的核心中枢，一秒跳转任意服务器、已加载键或全部 22 个功能面板。
- **布局结构**：顶部居中模态浮层：带前缀语法提示的搜索输入框 + 虚拟化分组结果列表 + 底部快捷键操作提示栏。
- **完整功能与交互**：
  - **三态前缀作用域（Sigils）**：
    - 无前缀（`General`）：默认列出所有服务器连接 + 输入关键字时并发对当前已加载键树做模糊打分检索（上限 50 条）+ 匹配所有功能动作；
    - `>` 前缀（`Commands`）：检索全部 22 个路由与 `ServerView` 工具面板（对当前版本/权限不支持的命令直接内联展示受限原因）、切换 Pub/Sub、打开设置/快捷键/回收站/导入导出/比对/能力矩阵/日志/诊断包；
    - `*` 前缀（`Favorites`）：直接列出并模糊过滤当前服务器收藏夹中的星标键。
  - 按 `(scope, query, key_tree_id)` 缓存排序结果保证零卡顿输入；按 `Esc` 关闭时通过 `hand_focus_on` 精确归还打开前的焦点。
- **UI 优化关注点**：
  - 检查结果行右侧快捷键徽章、分类副标题与左侧图标的列对齐；确保焦点上下循环滚动时首尾留白舒适。

---

### #14 最近访问键切换面板 (`ZedisRecentKeysPalette`) ✅ 已完成

- **源码位置**：`src/views/recent_keys_palette.rs`
- **入口与快捷键**：`⌘P` / `Ctrl+P`。
- **使用频率理由**：在固定几个热点键之间反复横跳排障时比键树翻找快一个数量级。
- **完整功能与交互**：按当前 `(server_id, db)` 作用域展示最近打开过的键列表（MRU 顺序），支持模糊输入过滤、显示键类型徽章，回车直接切回 `ServerView::Editor` 并定位打开目标键。
- **UI 优化关注点**：与 `ZedisCommandPalette` 共用完全一致的浮层宽度、圆角（`radius_lg`）、阴影层级与空状态文案风格。

---

### #15 跨实例/多库键搜索面板 (`ZedisMultiSearch`) ✅ 已完成

- **源码位置**：`src/views/multi_search.rs`
- **入口与快捷键**：`⌘⇧F` / `Ctrl+Shift+F`，或齿轮菜单 `Multi-DB Search`。
- **使用频率理由**：微服务多 Redis 实例架构下，“不知道某个键落在哪台实例或哪个 DB”时的救命工具。
- **布局结构**：大尺寸模态浮层：顶部 Scope 作用域切换条 + 搜索框与 `SCAN` 上限设置 → 左侧范围勾选栏（按打开的标签页、按服务器分组、或自由多选服务器）+ 右侧分阶段实时命中结果表与导出栏。
- **完整功能与交互**：
  - **两阶段零浪费检索策略**：
    - *第 1 阶段（极速精确探测）*：并发向所有目标实例发送极低开销的 `TYPE <query>`，毫秒级返回所有精确同名键；
    - *第 2 阶段（受控 `SCAN` 扫描）*：若精确探测 0 命中则自动启动带上限的并发 `SCAN *query*`；若已有精确命中则提供 `Also scan` 按钮按需扩大扫描，保护生产集群；
  - 关闭浮层再重开时完整保留上次查询词与结果；支持一键导出命中清单为 CSV；点击任意结果行智能跳转（复用已有同服标签页或新建标签打开对应键）。
- **UI 优化关注点**：
  - **【高优】修复多 `.primary()` 与手拼分段按钮**：`src/views/multi_search.rs` 顶部 Scope 切换（Open Tabs / Group / Servers）使用了 `.primary()` / `.ghost()` 手动拼接，与右侧搜索触发主按钮形成同屏双 `.primary()` 冲突；应改为 `ButtonGroup` 或 `.outline().selected(true)`。

---

### #16 – #25 日常键级与全局辅助弹窗组 ✅ 已完成

| 排名 | 视图与源码 | 触发方式 | 完整功能与交互亮点 | UI 优化关注点 |
| :---: | :--- | :--- | :--- | :--- |
| **#16** | **高危操作与生产写锁确认**<br>`danger_confirm.rs` (`confirm_dangerous_command`, `confirm_delete_key`, `TypedConfirm`) | 执行 `FLUSHDB`、`FLUSHALL`、`CONFIG SET`、`SCRIPT DEBUG SYNC`、解锁生产 15m 写锁、删键等 | 统一危险分级网关：普通环境使用红色 `.danger()` 确认框（删键时自动感知是否已开启回收站软删除并调整提示语）；**生产环境高危命令自动升级为 `TypedConfirm`**，要求手动完整输入服务器名称方可解锁确认按钮。 | 核查所有确认弹窗标题是否严格遵循简洁动宾问句（如 `Delete "user:1"?`），去除冗余礼貌套话。 |
| **#17** | **TTL 与绝对过期时间弹窗**<br>`ttl_dialog.rs` (`ZedisTtlDialog`)<br>`expire_at_dialog.rs` (`ZedisExpireAtDialog`) | 编辑器顶栏 TTL 下拉菜单、键树右键菜单 | `ZedisTtlDialog` 支持人性化时长表达式（如 `1h 30m`、纯秒数、`-1` 永不过期）及快捷预设胶囊；`ZedisExpireAtDialog` 支持输入日期时间或 Unix 时间戳执行 `EXPIREAT`，并实时换算展示距今剩余相对时长。 | 预设时长胶囊确认使用 `.outline().selected(...)` 而非 `.primary()`。 |
| **#18** | **键颜色标签与备注弹窗**<br>`key_tag_dialog.rs` (`open_key_tag_dialog`, `open_batch_key_tag_dialog`) | 键树单选/多选右键菜单 | 提供 8 色高辨识度调色板圆点 + 本地备注输入框（持久化于本地 `redb`，重命名键时自动跟随迁移；支持对多选键一键批量打色标）。 | 检查 8 色圆点在键盘 Tab 导航下的选中指示环（不能仅靠颜色区分选中态）。 |
| **#19** | **键值差异比对视图**<br>`value_diff.rs` (`ZedisValueDiff`) | 本地历史快照对比、CAS 保存冲突对比、跨服务器同名键对比 | 临时接管编辑器主视口：后台线程异步计算全文补丁，通过虚拟化 `Diff` 组件支持 `Split | Unified` 双排/单排切换（持久化偏好）、词级（Word-level）内联高亮、JSON 语法着色、折叠未变更上下文、`F7`/`⇧F7` 跳上下处差异；对 `ReJSON-RL` 键还在底部额外展示虚拟化的 **RFC 7396 `JSON.MERGE` 补丁预览**。 | 检查顶栏 `Exit Diff` 返回动作与 `Split | Unified` 切换按钮的对齐和快捷键提示。 |
| **#20** | **本地回收站弹窗**<br>`trash_dialog.rs` (`ZedisTrashDialog`) | Tools → Keys & Data → Recycle Bin / 命令面板 | 列出当前服务器所有 DB 被软删除的键快照（保存删前 `DUMP` 序列化载荷与剩余 `PTTL`，Web 桥接模式下还记录操作人账号 `deleted_by`）；支持关键字实时过滤、单条恢复（`RESTORE`，防 `BUSYKEY` 覆盖冲突）/ 彻底移除、一键全部恢复（`Restore All`），恢复后自动同步刷新键树。 | 空回收站状态补充说明“可在设置中开关删键自动进入回收站”的引导提示。 |
| **#21** | **跨服务器复制键 / 选库弹窗**<br>`copy_key_dialog.rs` (`ZedisCopyKeyDialog`) | 键右键/更多菜单 `Copy to Server`、`Diff with Server`、配置页 `Compare Config` | 跨实例通用目标选择器：选择目标连接、目标 DB、目标键名及“覆盖已有同名键（`REPLACE`）”开关；复制键时自动结合 `copy_preflight` 在 Redis 与 Valkey 跨版本序列化不兼容时自动回退为命令级重建复制。 | **【中优】** 修复弹窗内服务器选择项误用 `.primary()` 高亮当前项的问题（`copy_key_dialog.rs#L153`），改为 `.outline().selected(true)`。 |
| **#22** | **集合多键原生运算弹窗**<br>`key_op_dialog.rs` (`ZedisKeyOpDialog`) | Set / List / ZSet 编辑器 `⋯` 更多菜单 | 覆盖多键集合运算与移动：`SINTER` / `SUNION` / `SDIFF`（含 `*STORE` 落地新键与 Redis 7.0+ `SINTERCARD` 仅算基数）、`ZINTER` / `ZUNION` / `ZDIFF`（支持权重 `WEIGHTS` 与 `SUM|MIN|MAX` 聚合方式）、`LPOP`/`RPOP`/`SPOP`/`ZPOPMIN`/`ZPOPMAX`、`SMOVE`、`LMOVE`、`COPY`，支持先预览结果再写入目标键。 | 动态表单项较多，检查切换不同 `KeyOpAction` 时的弹窗高度稳定性与输入焦点自动落位。 |
| **#23** | **单键实时时间线监控弹窗**<br>`key_timeline_dialog.rs` (`ZedisKeyTimelineDialog`) | 编辑器顶栏时钟图标、Keyspace Notifications 点击行 | 双引擎单键显微镜：同时结合 `__keyspace@<db>__:<key>` 事件推送 + 定时轻量轮询（智能过滤单纯的 TTL 自然倒计时跳秒，仅在值/类型/TTL 被重置或键被删建时生成新快照）；本地 `redb` 环形持久化最多 50 个版本；左栏快照时间线列表 + 右栏紧凑 Diff 变更对比 / Raw 原始值切换，支持暂停、立即拍快照、清空、复制或导出 Markdown 排障记录。 | **【高优】** `key_timeline_dialog.rs` 右侧快照与 Diff 详情区目前使用裸 `Label` 渲染且存在多处硬编码 `.rounded(px(4.))` / `.rounded(px(6.))`；应改为只读 `Editor`（支持复制选中行）并统一使用 `theme.radius`。 |
| **#24** | **会话内集合修改审计弹窗**<br>`change_log_dialog.rs` (`ZedisChangeLogDialog`) | 集合编辑器 `⋯` → `Session Change Log` | 双表审计当前打开会话内用户对该集合键做过的每一次增删改：上半区通过 `kv_diff` 汇总计算 **Net changes（净变更结果）**（按位置寻址的 `List` 自动省略净变更表），下半区按时间顺序完整列出 **Every change（逐笔操作流水）**。 | 检查两张表格在无变更空态与多条记录滚动时的高度分配。 |
| **#25** | **全局快捷键速查浮层**<br>`shortcuts_overlay.rs` (`ZedisShortcutsOverlay`) | `⌘/` / `Ctrl+/`，或齿轮菜单 | 按功能域分组展示全套键盘快捷键，自动按当前操作系统渲染原生键帽符号（macOS `⌘ ⇧ ⌥` vs Windows/Linux `Ctrl Shift Alt`）。 | 纯只读参考面板，保持紧凑双列排版与 `Esc` 一键关闭归还焦点即可。 |

---

## 五、P2 梯队：可观测性、性能诊断与内容检索面板

当线上出现抖动、CPU 飙高、内存暴涨或需要检索未知键内容时，用户从底部状态栏遥测胶囊或 `Tools → Observability / Query` 进入这一梯队面板。

---

### #26 实时监控指标大盘与阈值告警 (`ZedisMetrics`, `ZedisServerAlertsDialog`) ✅ 已完成

- **源码位置**：`src/views/metrics.rs`、`src/views/metrics/alerts_dialog.rs`、`src/views/charts.rs`
- **入口与快捷键**：`ServerView::Metrics`（点击状态栏右侧延迟胶囊或 Tools → Observability → Metrics）。
- **使用频率理由**：连接服务器后最常点开的健康总览大盘，且状态栏延迟胶囊直达。
- **布局结构**：顶部导航与时间跨度控制栏（返回编辑器、`Live / 1h / 24h / 7d` 切换、Memory Report 按钮、Alerts 告警规则按钮、导出 CSV）→ 8 张核心 KPI 指标卡片网格 → 8 张响应式 1/2 列 GPU 折线/面积图表（`charts.rs`）。
- **完整功能与交互**：
  - **实时 + 本地持久化时序回溯**：`Live` 模式展示 2 秒级高频内存采样；同时后台按每分钟 1 次将关键指标落盘至本地 `redb`，支持跨重启查看 **`1h` / `24h` / `7d`** 历史趋势（渲染时自动降采样保帧率，导出 CSV 时输出全精度原始采样点）。
  - **8 大维度图表**：CPU 使用率（sys/user）、内存占用（MB）、PING 响应延迟（ms）、连接与阻塞客户端数、每秒吞吐 `Ops/s`、网络出入带宽 `kbps`、内存碎片率、缓存命中率 `%` 与驱逐键计数 `evicted_keys`。
  - **自定义指标阈值告警弹窗（`ZedisServerAlertsDialog`）**：支持按服务器独立启用并配置多条阈值规则（比较符、阈值、持续触发秒数防毛刺、冷却静默秒数、一键恢复默认规则），并在弹窗底部记录最近 20 次告警触发历史；触发时自动在状态栏亮起黄色预警图标并推送通知。
- **UI 优化关注点**：
  - **【高优】移除根节点全局等宽字体**：`metrics.rs#L934` 在最外层容器设置了 `.font_family(get_mono_font_family())`，导致顶栏标题、卡片说明文字、按钮文案全部变成等宽字体；应改回 UI 字体，仅在 KPI 数值、坐标轴刻度与 Tooltip 数据上使用等宽字体。
  - 顶栏建议统一迁移至 `zedis_ui::panel_header()`，消除各工具页返回按钮与标题间距的细微偏差。

---

### #27 慢查询、Valkey 命令日志与延迟诊断 (`ZedisSlowlogEditor`) ✅ 已完成

- **源码位置**：`src/views/slowlog_editor.rs`、`slowlog_editor/commandlog.rs`、`latency.rs`、`table.rs`、`top_commands.rs`、`server_report_dialog.rs`
- **入口与快捷键**：`ServerView::Slowlog`（点击状态栏慢日志计数胶囊或 Tools → Observability → Slowlog）。
- **使用频率理由**：定位慢命令、大请求/大响应包以及主线程延迟尖峰的标准排障页面。
- **布局结构**：采用 `panel_header` 顶栏 + 三大子标签页切换：**`Slow Log`（慢日志/命令日志明细）**、**`Top Commands`（慢命令聚合排行）**、**`Latency`（服务端延迟事件监控）**。
- **完整功能与交互**：
  - **`Slow Log` 标签页**：
    - 顶部直观展示当前服务端 `slowlog-log-slower-than` 与 `slowlog-max-len` 配置阈值及跳往 Config 修改的快捷入口；
    - **Valkey 8.1+ `COMMANDLOG` 扩展**：在 Valkey 8.1+ 上自动多出切换开关，除传统慢执行日志外还可直接查看 **`LARGE-REQUEST`（超大请求包）** 与 **`LARGE-REPLY`（超大返回包）**；
    - 支持按命令多选下拉过滤、`≥ ms`（或 `≥ kB`）阈值过滤、参数关键字过滤；自动将慢日志时间戳与 `LATENCY` 尖峰关联标注，点击胶囊直跳 Latency 页；悬停行支持一键打开首参数对应的键，或对 `EVALSHA` 调用 **Valkey `SCRIPT SHOW`** 弹窗直接反查服务端缓存的 Lua 源码；支持导出 CSV/JSON 与 `SLOWLOG RESET`。
  - **`Top Commands` 标签页**：按命令名聚合出现次数、累计耗时、最大耗时与占比条，点击任意命令自动切回 `Slow Log` 并带上该命令过滤条件。
  - **`Latency` 标签页**：每 5 秒轮询 `LATENCY LATEST`；若检测到服务端未开启 `latency-monitor-threshold`（值为 0），提供一键开启（`CONFIG SET latency-monitor-threshold 100`）引导按钮；点击任意事件展开 `LATENCY HISTORY` GPU 趋势图，并可反跳回该时间窗口附近的慢查询列表；支持查看 `LATENCY DOCTOR` 诊断报告与 `LATENCY RESET`。
- **UI 优化关注点**：
  - 同样需移除 `slowlog_editor.rs#L473` 根容器上的全局 `.font_family(get_mono_font_family())`，让标签名、配置提示横幅与空状态回归清晰易读的 UI 字体。

---

### #28 在线内存采样与离线 RDB 深度分析 (`ZedisMemoryAnalysis`, `ZedisDataDictionaryDialog`) ✅ 已完成

- **源码位置**：`src/views/memory_analysis.rs`、`memory_analysis/render.rs`、`sampler.rs`、`rules.rs`、`remediation.rs`、`schema_dialog.rs`、`export.rs`、`report.rs`、`stats.rs`、`tables.rs`
- **入口与快捷键**：`ServerView::MemoryAnalysis`（点击状态栏内存胶囊或 Tools → Observability → Memory Analysis；声明 `survives_navigation`）。
- **使用频率理由**：排查大 Key（BigKey）、内存膨胀、过期雪崩（TTL Cliff）与前缀空间分布的旗舰分析器。
- **布局结构**：`panel_header` 顶栏（在线开始/停止采样、桌面端打开本地 `.rdb` 离线分析、数据字典弹窗、导出报告）+ 实时采样 `Progress` 进度条 → 纵向分节滚动报告（智能诊断建议 → 碎片率趋势 → 大小直方图 → 类型/编码占比 → TTL 分布 → 3 层可下钻前缀排行 + Top 单键排行）。
- **完整功能与交互**：
  - **在线采样 + 离线 RDB 双引擎**：
    - *在线模式*：按总键数自动缩放采样率（目标约 50,000 样本键），通过流水线并发采集 `SCAN` + `TYPE` + `MEMORY USAGE` + `TTL` + `OBJECT ENCODING`，并根据淘汰策略自动探测 `OBJECT FREQ`（LFU 热度）或 `OBJECT IDLETIME`（LRU 空闲时长）；
    - *离线 RDB 模式*：直接解析本地 `.rdb` 快照文件生成同构全量内存报告，对生产实例 **零负载冲击**。
  - **规则引擎一键治理（`rules.rs` + `remediation.rs`）**：自动识别超大单键、永不过期键堆积、`noeviction` 风险、高内存碎片（同时满足比例与 `≥200MB` 实际浪费门槛才标红告警）、海量碎小 String、单一前缀垄断等隐患，每条建议附带 **1-Click Remediation**（直达打开该大键、跳往 Config 调参、一键执行 `MEMORY PURGE` 回收脏页、或带前缀打开批量模式清理），桌面端还支持一键调用 **AI 深度优化解读**。
  - **多维可视化与下钻**：
    - **Redis 8+ `INFO keysizes`** 服务端精确 2 的幂次体积直方图（按类型分色）；
    - 按数据类型与 `(Type, Encoding)` 编码组合的内存占比条；
    - 6 档 TTL 过期分布柱状图及集中过期雪崩（TTL Cliff）预警提示；
    - **3 层可点击下钻的 Top 前缀目录表**（带面包屑导航）与支持按 **`Size（体积）| Hot（最热）| Cold（最冷僵尸键）`** 三种维度切换排序的 Top 单键表；
  - **自动推断数据字典弹窗（`ZedisDataDictionaryDialog`, `schema_dialog.rs`）**：将散乱键名自动聚类归纳为模式（如 `user:{int}:profile`、`order:{uuid}:items`），抽样探测 Hash 字段与 JSON 结构，一键导出团队可读的 Markdown / JSON 数据字典文档；同时自动将采样结论注入一键故障排查报告（ADR 19）。
- **UI 优化关注点**：
  - 移除 `memory_analysis/render.rs#L1206` 根节点的全局等宽字体设置，使大段诊断建议文案与规则标题采用自然 UI 字体渲染。
  - 修复 `schema_dialog.rs#L154` 中的硬编码 `.rounded(px(4.))`，统一对齐 `theme.radius`。

---

### #29 多标签查询工作台与代码片段库 (`ZedisWorkbench`) ✅ 已完成

- **源码位置**：`src/views/workbench.rs`、`workbench/tab.rs`、`workbench/results.rs`、`workbench/snippets.rs`
- **入口与快捷键**：`ServerView::Workbench`（Tools → Query & Scripting → Workbench；声明 `survives_navigation`）。
- **使用频率理由**：相比底部单会话 CLI 终端，Workbench 专为需要保存复用排障脚本、带动态参数模板与逐条对照多语句输出的研发场景设计。
- **布局结构**：顶部面板头 + 工作台内部多脚本标签栏 → 左侧可折叠 **Saved Snippets（本地持久化代码片段库）** 边栏 + 右侧上下分割区（上方带语法高亮的多行脚本编辑器 + 自动提取的 `{{param}}` 参数填空栏 + 运行工具条；下方逐语句结构化结果卡片流）。
- **完整功能与交互**：
  - **参数化脚本模板**：在编辑器中编写任意包含 `{{user_id}}`、`{{prefix}}` 占位符的多行 Redis 命令脚本，编辑器下方自动生成对应的参数输入框，修改参数值即可反复安全执行，无需改动脚本正文。
  - **代码片段管理（`snippets.rs`）**：将常用排障命令组保存至本地 `redb`（含标题、描述、脚本正文及默认参数值），一键插入或新开标签。
  - **隔离执行与多视图结果卡片（`results.rs`）**：支持 `Run Selection`（仅运行选中行）与 `Run All` / `Stop`，同样接入生产写锁与高危命令确认；下方按每条语句生成独立结果卡片，展示原始命令、成功/错误徽标、单条耗时（ms）、`Text | Table | JSON` 独立视图切换及单卡/全部复制。
- **UI 优化关注点**：
  - **【高优】提升结果卡片文本可选中性**：`workbench/results.rs#L222` 目前使用裸 `Label` 渲染命令返回文本，无法用鼠标框选局部字段复制；建议升级为紧凑只读 `Editor` 或支持可选择文本块，并将顶栏迁移至统一 `panel_header`。

---

### #30 客户端连接管理与暂停/查杀 (`ZedisClientsManager`, `client_dialogs`) ✅ 已完成

- **源码位置**：`src/views/clients_manager.rs`（1,458 行）、`src/views/client_dialogs.rs`
- **入口与快捷键**：`ServerView::Clients`（点击状态栏连接数胶囊或 Tools → Observability → Clients）。
- **使用频率理由**：排查连接池泄漏、哪个微服务实例占满连接、阻塞命令挂起以及紧急流量止血（`CLIENT PAUSE` / 批量 `CLIENT KILL`）的核心运维视图。
- **布局结构**：顶栏 + 多条件筛选与批量操作栏 → 12 列虚拟化全宽客户端表。
- **完整功能与交互**：
  - 集群模式下自动并发聚合所有主节点的 `CLIENT LIST`；展示 `id`、`addr`、`name`、`user`、`age`、`idle`、`db`、`flags` 状态图标、最后执行 `cmd`（空命令优雅显示为静音 `—`）、客户端 SDK `lib-name/ver`、`qbuf` 输入缓冲与 `tot-mem` 内存占用。
  - **精细过滤**：支持关键字模糊搜 IP/名称/用户、按客户端角色标志过滤（普通 / 主从复制 / Monitor / PubSub / 阻塞中）、按 `idle >= N 秒` 抓僵尸连接、按 `age >= N 秒` 抓长连接。
  - **三级管控动作**：
    1. 单行右侧一键断开连接（`CLIENT KILL ID`，对主从复制链路 `S`/`M` 自动隐藏断开按钮防误断集群）；
    2. `Batch Kill` 一键查杀当前筛选出的全部非复制客户端；
    3. **高级条件查杀弹窗（`ZedisClientKillFilterDialog`）**（按 ID 列表、远端地址、本地端口、ACL 用户名、客户端类型、最大存活秒数、`SKIPME` 组合执行 `CLIENT KILL`）与 **客户端暂停弹窗（`ZedisClientPauseDialog`）**（执行 `CLIENT PAUSE <ms> [WRITE|ALL]`，开启后底部状态栏同步亮起一键恢复按钮）。
- **UI 优化关注点**：
  - `clients_manager.rs` 已达 1,458 行，后续改动应将筛选栏或表格列定义抽入 `clients_manager/` 子模块。
  - 移除根容器全局等宽字体（`clients_manager.rs#L1178`），并将手写 `h(px(40.))` 顶栏统一替换为 `panel_header`。

---

### #31 – #38 其余可观测性、内容搜索与诊断视图 ✅ 已完成

| 排名 | 视图与源码 | 入口与平台门控 | 完整功能与交互说明 | UI 优化关注点 |
| :---: | :--- | :--- | :--- | :--- |
| **#31** | **实时命令流监控与流量回放**<br>`monitor.rs` + `summary.rs`, `replay.rs` (`ZedisMonitor`, `MonitorReplayDialog`) | `ServerView::Monitor`（**仅桌面端**） | 在集群所有主节点开启专属 `MONITOR` 流（10,000 行环形缓冲，生产环境弹出性能损耗确认；当命令速率 `>5,000/s` 时自动熔断断开底层连接防拖垮服务端与 UI）；**默认自动过滤隐藏 Zedis 自身连接池发出的探针命令**（通过 `CLIENT LIST` 识别自身端口）；支持多词过滤、点击首参数直跳键编辑器；内置 **有界采样分析模式**（限定采 `5s/10s/30s` 或 `1k/5k/10k` 条后自动停止，切换 `Summary | Lines` 查看高频命令、热点前缀与最吵客户端排行）；支持 **只读流量回放器（`MonitorReplayDialog`）**，将捕获到的真实读请求按 `1x / 2x / Max` 倍速回放到测试环境并生成延迟对比报告。 | 已采用 `panel_header`，重点检查采样模式切换条与 `Summary` 三栏统计表在窄窗下的横向留白。 |
| **#32** | **键值内容全文检索**<br>`value_search.rs`（1,497 行）+ `preview.rs` (`ZedisValueSearch`) | `ServerView::ValueSearch`（声明 `survives_navigation`） | 带严格安全护栏的**值内子串搜索**（覆盖 `String / Hash / List / Set / ZSet`）：强制填写键名前缀（如 `session:`）拒绝全库裸扫，可调扫描键数上限、耗时预算与命中数上限，自动跳过 `>1MB` 或 `>10,000` 元素的超大集合；预算耗尽后支持点击 **Continue** 从上次游标无缝续扫；左栏虚拟化列表展示命中键与精确命中位置（字段名 / 索引 `[idx]` / 成员），右栏内嵌只读 `Editor` / `ZedisJsonTree` 实时预览与一键在主编辑器打开，支持导出 CSV。 | `value_search.rs` 达 1,497 行（触及拆分红线）；移除根容器全局等宽字体（`#L1178`）并迁移至统一 `panel_header`。 |
| **#33** | **Pub/Sub 实时消息订阅与发布**<br>`pubsub_editor.rs`, `pubsub_channels_dialog.rs` | 键树菜单 `Pub/Sub Mode` 或 Tools → Query → Pub/Sub | 接管右侧编辑区：顶栏输入空格分隔的通配频道执行 `PSUBSCRIBE`，或开启 **Redis 7+ `Sharded` 分片频道模式** 执行 `SSUBSCRIBE`；实时跳动显示 `"已订阅 5m · 最后消息 12s 前"` 心跳状态；点击 `Channels…` 弹出活跃频道探测器（`ZedisPubsubChannelsDialog`，调用 `PUBSUB CHANNELS`/`NUMSUB` 或 `SHARDCHANNELS` 一键勾选订阅）；中部 1,000 条自动解码消息表；底部 `PUBLISH` / `SPUBLISH` 消息发送栏，支持导出 CSV/JSON。 | 检查底部发布输入栏与底部全局 `ZedisStatusBar` 相邻时的双边框与内边距节奏。 |
| **#34** | **服务端原始 `INFO` 浏览器与快照对比**<br>`server_info.rs` (`ZedisServerInfo`) | `ServerView::ServerInfo` | 拉取集群各主节点 `INFO everything`，按 `节点 · 分节 / 字段名 / 值` 三列展示并支持实时关键字过滤；核心亮点是内置 **会话级快照对比（Take Snapshot & Compare）**：调优前后各拍一张快照，一键切换至差异模式展示新增/变更/移除的指标及 `旧值 → 新值` 变化，支持导出 CSV。 | 已使用 `panel_header`，整体克制干净，仅需核查快照对比模式下 `old → new` 颜色在亮色主题下的对比度。 |
| **#35** | **服务端命令吞吐负载分析**<br>`server_load.rs` (`ZedisServerLoad`) | `ServerView::ServerLoad` | 定时轮询 `INFO commandstats` 计算真实区间增量速率（每秒调用 `calls/s`、平均单次微秒耗时 `avg µs`、累计调用次数、占总 CPU 耗时百分比条）；默认智能隐藏区间 0 调用的静默命令及 Zedis 自身心跳命令；提供 `Top 25 / Top 50 / All` 档位切换、列头点击排序、复制 CSV 及 `CONFIG RESETSTAT` 重置统计。 | 顶栏迁移至 `panel_header`，检查百分比进度条列与右侧等宽数字列的固定槽位对齐。 |
| **#36** | **Redis 8.6+ 热点键追踪面板**<br>`hotkeys.rs` (`ZedisHotkeys`) | `ServerView::Hotkeys`（需 Redis 8.6+ `HOTKEYS` 命令支持） | 封装 Redis 8.6 原生服务端热点追踪：选择追踪维度（`CPU` 耗时、`NET` 网络字节流量）与 `Top 10 / 20 / 50`，一键在单机或集群所有主节点开启 `HOTKEYS START`、实时轮询合并展示双栏排行榜（名次、占比条、累计值、百分比、行尾「打开键 / 复制键名」可键盘导航动作按钮）、`STOP` 停止采集、`RESET` 清空，并自动将非空采样结果写入排障报告（ADR 19）。 | ✅ 已完成：移除根容器全局等宽字体，保留标准 `table_hover` 行悬停高亮，并在行尾补齐可键盘聚焦激活的「打开键」与「复制」按钮及 `#[gpui::test]`。 |
| **#37** | **一键故障排查报告与 Doctor 弹窗**<br>`troubleshoot_report.rs`, `server_report_dialog.rs` | Tools → `Troubleshooting Report…`；Metrics/Latency 页 Doctor 按钮 | `ZedisTroubleshootReport`（ADR 19）一键并发采集全套隐私安全排障快照：实例拓扑概览、多节点 `INFO` 横向对比表、`MEMORY DOCTOR`、`LATENCY LATEST`、**脱敏慢日志**（仅保留命令名与首参数键名，绝不导出业务 Value）、关键 `CONFIG` 参数，并自动拼入当前会话跑过的内存分析与热点键结论，支持 Markdown 预览及导出 Markdown / 独立单文件 HTML；`ZedisServerReportDialog` 展示原生 `MEMORY DOCTOR` + 可排序 `MEMORY STATS` 表或 `LATENCY DOCTOR`。 | 检查独立 HTML 与弹窗内 Markdown 表格在超宽节点列时的横向滚动体验。 |
| **#38** | **服务端命令能力探测矩阵弹窗**<br>`features_dialog.rs` (`open_features_dialog`) | 状态栏 Tools → `Server Capabilities…` 或命令面板 | 展示连接建立时只读探针检测到的服务端引擎风味（Redis / Valkey / Dragonfly / Garnet 等）及全部受控 `ServerCommand` 的可用状态，**自动将受限/缺失（`Denied` / `Missing`）命令置顶显示**，提供 `Re-probe` 按钮在运维修改 ACL 权限后即时刷新能力状态。 | 核查状态徽章除颜色外均有明确文字说明（`Available` / `Denied by ACL` / `Unsupported`）。 |

---

## 六、P3 梯队：服务端运维配置、脚本研发与跨库数据工具

这一梯队面向 DBA、架构师及研发深度运维场景，功能复杂度与表单密度最高。

---

### #39 运行时参数配置编辑器与跨库对比 (`ZedisConfigEditor`) ✅ 已完成

- **源码位置**：`src/views/config_editor.rs`（1,616 行）、`src/views/config_doc.rs`
- **入口与快捷键**：`ServerView::Config`（Tools → Administration → Config）。
- **使用频率理由**：调优内存淘汰策略、持久化阈值、慢日志参数及比对测试/生产环境配置差异的标准界面。
- **布局结构**：顶部搜索、分组筛选、跨服比对与 `CONFIG REWRITE` 工具栏 → 13 个语义折叠分组 + 紧凑 1/2/3 列自适应参数行网格。
- **完整功能与交互**：
  - 拉取 `CONFIG GET *`，自动处理主从参数历史别名（`replica-read-only` ↔ `slave-read-only`），将 200–400 项底层配置归类为 **13 个语义分组**（内存淘汰、AOF、RDB、网络、安全、复制、日志、可观测性、集群、TLS、碎片整理、数据结构阈值、脚本与其他）。
  - **内嵌中英双语官方配置文档（`config_doc.rs`）**：参数名旁附带 `?` 按钮，点击弹出 `Popover` 展示对应 `redis.conf` 官方说明与调优注意事项。
  - **类型推断内联编辑器**：根据参数语义自动渲染枚举下拉框（如 `maxmemory-policy`，含 Redis 8.6 新增策略门控）、布尔开关、数值框或文本框（支持 `Esc` 取消编辑）；保存时经高危确认执行 `CONFIG SET`；若实例以无配置文件模式启动则自动提示修改仅内存生效，否则提供 `CONFIG REWRITE` 落盘按钮。
  - **跨服务器配置差异对比（Compare with Server）**：选择另一台实例后切换为差异表，高亮列出两台实例不一致的所有运行参数。
- **UI 优化关注点**：
  - **单文件超限**：`config_editor.rs` 已达 1,616 行（超 1,500 行红线），做 UI 优化时应将跨服配置对比视图与类型推断输入控件拆入 `config_editor/` 子模块，并统一顶栏为 `panel_header`。

---

### #40 ACL 权限用户管理、权限模拟与安全日志 (`ZedisAclManager`) ✅ 已完成

- **源码位置**：`src/views/acl_manager.rs`
- **入口与快捷键**：`ServerView::Acl`（Tools → Administration → ACL，需 Redis 6+ ACL 支持）。
- **使用频率理由**：精细化管理微服务账号最小权限、排查线上 `NOPERM` 拒绝报错以及上线前验证 ACL 规则。
- **布局结构**：双标签页——**`Users`（ACL 用户卡片列表与规则管理）** 与 **`Security Log`（`ACL LOG` 鉴权拒绝审计表）**。
- **完整功能与交互**：
  - **`Users` 标签页**：并发查询 `ACL USERS`、`ACL GETUSER`、`ACL WHOAMI`（高亮当前登录账号）及 `aclfile` 配置；每张用户卡片展示启停状态、命令权限集、键前缀规则、Pub/Sub 频道规则、**Redis 7+ 多权限选择器（Selectors）** 与密码摘要；提供 `ACL SAVE` / `ACL LOAD`（当配置了外部 `aclfile` 时）。
  - **可视化新建/编辑用户弹窗（`ZedisAclEditor`）**：内置常用角色预设模板（只读、业务读写、运维管理）、可点击命令类别开关胶囊、`ACL GENPASS` 服务端高强度随机密码生成器、以及保留已有 Selectors 不丢失的安全 `ACL SETUSER` 更新；禁止误删内置 `default` 账号。
  - **Redis 7.0+ 权限沙箱模拟器（`ZedisAclDryRun`）**：点击任意用户卡片上的 `Dry Run` 弹出模拟窗，输入任意待测命令（如 `SET order:100 ok`），调用服务端 `ACL DRYRUN` 零副作用验证该账号是否放行或因哪条规则被拒。
  - **`Security Log` 标签页**：可排序/过滤展示 `ACL LOG` 捕获的认证失败与越权命令现场（客户端地址、违规用户名、被拒命令/键、发生时间与次数），支持 `ACL LOG RESET`。
- **UI 优化关注点**：
  - 移除 `acl_manager.rs#L850` 根容器全局等宽字体，使权限分类标签与说明回归 UI 字体；顶栏统一采用 `panel_header`。

---

### #41 RDB / AOF 持久化状态与后台落盘控制 (`ZedisPersistence`) ✅ 已完成

- **源码位置**：`src/views/persistence.rs`
- **入口与快捷键**：`ServerView::Persistence`（Tools → Administration → Persistence）。
- **完整功能与交互**：
  - 顶部 4 张核心状态卡（上次成功落盘时间、距上次落盘未保存变更数、AOF 启停与重写状态、AOF 当前体积与基准膨胀率），自动对 **快照陈旧（`>6h`）**、服务端正在加载持久化文件或上次 `BGSAVE` 失败亮起醒目横幅告警。
  - 中部汇总当前 RDB `save` 触发策略与 AOF `appendfsync` 配置摘要（附一键跳往 Config 修改）。
  - 底部提供 **`BGSAVE`**（在 **Valkey 8.1+** 上当后台子进程正在执行时额外提供 **`BGSAVE CANCEL`** 中止按钮）与 **`BGREWRITEAOF`** 操作卡片，触发后实时追踪后台 Fork 子进程耗时并在落盘完成时弹出通知；集群模式下额外列出所有主节点各自的落盘健康表。
- **UI 优化关注点**：顶栏对齐 `panel_header`；检查两张操作触发卡片在同一屏幕内是否同时使用了 `.primary()` 按钮（应降级为 `.outline()`，避免双主按钮竞争焦点）。

---

### #42 集群 / 哨兵 / 主从拓扑管理与在线重分片 (`ZedisTopology`) ✅ 已完成

- **源码位置**：`src/views/topology.rs`、`topology/cluster_nodes.rs`、`cluster_slots.rs`、`cluster_load.rs`、`cluster_reshard.rs`、`replication.rs`、`sentinel.rs`、`sentinel_dialogs.rs`
- **入口与快捷键**：`ServerView::Topology`（**仅桌面端**，Tools → Topology）。
- **使用频率理由**：根据当前连接的服务器运行模式（Cluster / Sentinel / Standalone 主从）智能呈现对应的完整拓扑治理工作台（ADR 6）。
- **完整功能与交互**：
  - **Redis Cluster 模式（4 子标签页）**：
    1. *Nodes（节点树与扩缩容）*：主从层级树展示节点角色、槽位范围、**Valkey 可用区（AZ）徽章**、从库复制偏移延迟；支持节点级手动故障转移（`CLUSTER FAILOVER` / `FORCE`）、剔除下线节点（`CLUSTER FORGET`）、点击填充式新节点加入（`CLUSTER MEET`）与调整从属关系（`CLUSTER REPLICATE`）；
    2. *Slots（16,384 哈希槽矩阵）*：可视化展示全量槽位分布、未分配槽一键 `ADDSLOTS`、卡死迁移槽一键 `SETSLOT … STABLE` 修复，以及 **Redis 8.2+ `CLUSTER SLOT-STATS`** 热点槽位资源统计；
    3. *Load（分片负载均衡热力图）*：实时轮询对比各主节点的内存、QPS、连接数与键数量偏斜度；
    4. *Reshard（可视化在线迁移与自动均衡）*：支持规划指定槽位区间迁移或一键生成全集群均衡方案；在 **Valkey 9+** 上优先采用服务端原生原子迁移 `CLUSTER MIGRATESLOTS`，普通集群则执行带实时 `Progress` 进度条的客户端 `SETSLOT` + `MIGRATE` 流水线。
  - **Sentinel 哨兵模式**：展示受监控主节点、从节点与其余哨兵健康状态、Quorum 仲裁票数与超时配置，支持多 Master 切换、`SENTINEL CKQUORUM` 仲裁自检、主动 `FAILOVER`、`RESET`、`REMOVE`、`FLUSHCONFIG`、新增监控主库弹窗（`ZedisSentinelMonitorDialog`）及在线修改哨兵参数弹窗（`ZedisSentinelSetDialog`）。
  - **Standalone 主从模式**：展示当前主从同步链路偏移与延迟，支持对指定从库发起无损切换（Redis 6.2+ `FAILOVER`）或动态挂载/提升主库（`REPLICAOF host port` / `REPLICAOF NO ONE`）。
- **UI 优化关注点**：
  - **【高优】消除双主按钮**：`src/views/topology/cluster_nodes.rs#L70,L117` 在底部并排渲染了 `Meet` 与 `Replicate` 两个 `.primary()` 按钮，应改为 `.outline()` 并在各自提交动作时做区分。

---

### #43 RediSearch 全文与向量索引管理器 (`ZedisSearchManager`) ✅ 已完成

- **源码位置**：`src/views/search_manager.rs`（1,552 行）、`search_manager/render.rs`、`search_manager/cursor.rs`
- **入口与快捷键**：`ServerView::Search`（Tools → Query & Scripting → RediSearch）。
- **使用频率理由**：覆盖 Redis Stack / Redis 8 RediSearch (`FT.*`) 全文检索、聚合分析与向量 KNN 查询的一站式面板。
- **布局结构**：顶栏（索引下拉选择器 `FT._LIST`、刷新、新建索引、新增字段、删除索引）→ 可折叠 **Schema Inspector（`FT.INFO` 字段结构与索引体积统计面板）** → 查询输入栏（带字段名快捷点击填入胶囊、`Search | Aggregate` 模式切换、动态 `$param` 参数绑定行、LIMIT/RETURN/SORTBY/HIGHLIGHT/GROUPBY/REDUCE/DIALECT 选项栏）→ 可选内联执行计划面板（`FT.EXPLAIN` / `FT.PROFILE`）→ 结构化结果表。
- **完整功能与交互**：
  - **Schema 透视与 Tag 字典探测**：展示文档数、索引内存占用及全部字段类型/属性；点击任意 `TAG` 字段旁按钮可直接调用 `FT.TAGVALS` 展开该字段所有枚举标签值，点击标签自动转义并拼入查询框。
  - **`FT.SEARCH` 与 `FT.AGGREGATE`（含游标分页）**：支持全文查询、字段高亮（`<mark>`）、多聚合函数（`COUNT` / `SUM` / `AVG` 等）以及 `WITHCURSOR` 服务端游标分页（切换索引或离开页面时自动调用 `FT.CURSOR DEL` 释放服务端游标）。
  - **自动 `$param` 参数化绑定（含向量二进制编码）**：查询框中出现 `$vec` 或 `$min_price` 时自动在下方生成参数输入行（支持粘贴 JSON 浮点数组自动编码为 IEEE-754 小端二进制向量 Blob 供 KNN 查询使用）。
  - **执行计划剖析与零命中拼写纠错**：一键查看 `FT.EXPLAIN` 查询树或 `FT.PROFILE` 算子耗时树；当搜索 0 命中时自动调用 `FT.SPELLCHECK` 给出拼写纠错建议胶囊，点击一键替换原查询词重试。
  - **内联索引生命周期管理**：支持可视化表单新建索引（`FT.CREATE` ON HASH/JSON，配置前缀与 TEXT/NUMERIC/TAG/GEO 字段属性）、在线追加新字段（`FT.ALTER … SCHEMA ADD`）及安全删除索引（`FT.DROPINDEX` 默认不带 `DD`，保护底层源数据键不被连带抹除）。
- **UI 优化关注点**：
  - **单文件超限与输入密集度最高**：`search_manager.rs` 达 1,552 行，且面板内同时包含多达 12 个条件输入框（LIMIT、RETURN、HIGHLIGHT、GROUPBY、REDUCE、SORTBY、DIALECT 等）；做 UI 优化时建议将索引创建/修改表单拆为独立子模块，并将高级查询子句默认收拢进干净的可折叠高级选项区（Progressive Disclosure），大幅降低首屏视觉压迫感。

---

### #44 – #49 脚本开发、事件订阅、压测与跨库迁移/比对工具 ✅ 已完成

| 排名 | 视图与源码 | 入口与平台门控 | 完整功能与交互说明 | UI 优化关注点 |
| :---: | :--- | :--- | :--- | :--- |
| **#44** | **Lua 脚本库与交互式单步调试器**<br>`lua_script_library.rs` + `render.rs`, `debugger.rs` | `ServerView::LuaScripts`（调试器仅桌面端） | 管理本地 `redb`（或 Web 团队共享）Lua 脚本库：预计算 SHA1、探测服务端 `SCRIPT EXISTS` 缓存状态并支持一键预热（`SCRIPT LOAD`）、清空缓存（`SCRIPT FLUSH`）、终止慢脚本（`SCRIPT KILL`）、累计调用与 `EVALSHA` 命中率统计、4 套常用脚手架模板、带 `KEYS[n]` 数量校验与默认参数保存的 `EVALSHA` / `EVALSHA_RO`（7.0+）执行器、剪贴板 JSON 导入导出；桌面端内置 **交互式 Lua 单步调试器（`LuaDebugger`）**：支持 `SCRIPT DEBUG YES`（非阻塞 Fork 会话）与高危确认的 `SYNC` 模式、点击行号设断点、单步/步过/继续/中止、集群哈希槽一致性校验与目标主节点选择、实时局部变量表（`locals`）及 `Eval` / `redis.call` 调试控制台。 | **【中优】** 修复 `lua_script_library/render.rs#L346` 代码预览区使用 `.disabled(true)` 导致代码文字半透明发灰的问题，应改为清晰只读 `Editor`。 |
| **#45** | **Redis 7+ Functions 函数库管理器**<br>`function_editor.rs`（1,446 行） | `ServerView::Functions`（需 Redis 7.0+） | 基于 `FUNCTION LIST WITHCODE` 与 `FUNCTION STATS`（运行中函数横幅 + 一键 `FUNCTION KILL`）管理服务端持久化函数库：可展开查看库源码、带 `#!lua name=` 头部语法校验与 4 套模板的新建/覆盖加载编辑器（`FUNCTION LOAD [REPLACE]`）、单函数内联 `FCALL` / `FCALL_RO` 试运行面板、`FUNCTION DELETE`、`FUNCTION FLUSH` 以及基于 Base64 剪贴板的 `FUNCTION DUMP` / `FUNCTION RESTORE` 跨实例迁移。 | `function_editor.rs` 已达 1,446 行，后续优化时可将新建/编辑表单与试运行器拆入 `function_editor/` 子模块。 |
| **#46** | **键空间事件实时订阅器**<br>`keyspace_notifications.rs`（1,534 行） | `ServerView::KeyspaceNotifications`（**仅桌面端**；亦可从键树目录右键 `Watch This Prefix` 带前缀直达） | 订阅 `__keyspace@*__:*` 与 `__keyevent@*__:*` 实时捕获服务端发生的改键、删键、自然过期与内存驱逐事件；自动检查服务端 `notify-keyspace-events` 配置，未开启时提供一键配置预设按钮（如开启全事件或仅监听过期/驱逐事件）；支持按事件类型胶囊（`set / del / expired / evicted` 等）、DB 胶囊、消息源（`Both | Keyevent | Keyspace`）与键名关键字实时过滤，显示每秒事件速率 `events/s`，支持暂停、导出 CSV，点击任意事件直接打开该键或调起单键时间线 `ZedisKeyTimelineDialog`。 | `keyspace_notifications.rs` 达 1,534 行（超 1,500 行红线），建议拆分订阅后台流与表格渲染，并将顶栏对齐至 `panel_header`。 |
| **#47** | **内置基准压测工具**<br>`benchmark.rs` + `benchmark/render.rs` | `ServerView::Benchmark`（声明 `survives_navigation`） | 内置可视化 `redis-benchmark`：支持 `SET / GET / INCR / LPUSH / HSET / ZADD` 压测预设、流水线深度（`P=1..64`）、目标 DB、并发数、载荷字节数、随机键空间大小与持续时长；所有测试写入隔离在 `zedis:bench:<id>:` 命名空间下，结束或中途取消/销毁窗口时自动清理测试键（除非勾选保留）；运行中实时刷新吞吐与 P99 横幅；完成后展示 8 张核心指标卡（支持固定某次运行为 **Baseline 基准线**，跨实例跑压测时自动计算百分比增减 `Δ%`）、延迟分布直方图 + 吞吐时序折线图、跨连接共享的进程级历史记录表及 CSV 导出。 | **【高优】修正返回按钮位置反常问题**：`benchmark/render.rs#L93` 将 `Back` 返回按钮放在了顶栏**最右侧**，与全应用其余 20 个面板全部在最左侧返回的设计相悖；同时移除 `#L392` 硬编码 `.rounded(px(4.))`。 |
| **#48** | **数据导入 / 导出 / 跨实例迁移与增量跟随**<br>`migration_window.rs`（1,832 行）+ `sections.rs`, `key_export.rs`, `export.rs` | Tools → Keys & Data → Import / Export，键树右键导出，比对页一键同步 | **桌面端独立窗口 `ZedisMigrationWindow`（760×680）**：三合一引擎——导出至本地文件（`.zedis-dump` 二进制保真包、`.json`、`.csv`、`.redis` 命令脚本）、从文件导入、或 **跨服务器直连在线迁移**；支持仅已加载键或流式多模式 `SCAN` + 类型过滤、`Skip | Overwrite | Abort` 三种冲突策略、执行前 **Dry-Run 冲突预检**、最大速率限流阀（`keys/s` 保护线上网卡与主线程）、基于本地 `redb` 的 **断点续传任务检查点**、复制完成后自动抽样校验（`verify_copied_keys`）与一键跳往全量比对，以及全量复制结束后基于键空间通知的 **实时增量跟随模式（Follow Mode）**。Web 端自动适配为基于内存分块的 `ZedisKeyExport` 模态框（上限 64 MB 下载或桥接端直传）。 | **全库最大单文件（1,832 行）**：强烈建议在优化迁移向导分步表单视觉层级时，按 Export / Import / Copy / Follow 拆分为清晰子模块。 |
| **#49** | **跨数据库全量键比对工具**<br>`compare_window.rs` (`ZedisCompareWindow`) | Tools → Keys & Data → Compare DBs（桌面独立窗口 / Web 弹窗） | 对任意两个 `(server, db)` 在指定前缀与扫描上限（默认 `10,000` 键）下执行三阶段实时比对（扫描源库 → 扫描目标库 → 逐键摘要比对）；结果分为 **`Only in Source`（仅源库有）**、**`Only in Target`（仅目标库有）**、**`Differing`（值不一致或类型不匹配）** 三个可排序/搜索标签页；支持复制差异键名清单，或点击 **`Copy to Target (N)`** 一键带着差异键列表拉起迁移窗口执行覆盖同步。 | 检查源库与目标库选择器左右对称排版，以及三结果页签的数字角标样式与全局 `Tab` 组件对齐。 |

---

## 七、P4 梯队：专有数据结构视图与应用级设置/管理窗口组（#50 详拆） ✅ 已完成

虽然单个模块触发频率相对较低，但在特定业务场景（LBS 地理位置、IoT 时序、RAG 向量检索、位图签到、布隆去重、Protobuf 缓存）或初次配置应用时不可或缺。以下逐一列出全部 12 个专有子视图与系统级窗口：

### 50.1 专有数据结构与模块内嵌视图（7 个）

1. **地理位置雷达地图视图 (`ZedisGeoMap`)** — `src/views/geo_map.rs`（1,510 行）
   - **入口**：检测到包含有效经纬度 Geohash 分数的 `ZSet` 键时，点击工具栏 `View on Map`。
   - **功能**：自绘 Web Mercator 投影暗色网格雷达画布（最多渲染 2,000 个坐标点），支持滚轮缩放、拖拽平移、一键自适应视野（Fit）；支持 **`Shift+Click` 两点球面测距尺（Haversine 距离实时标尺）**；内置 `GEOSEARCH` 圆形半径 / 矩形框空间检索栏、`GEOADD` 新增坐标点与 `GEODIST` 测距弹窗；右侧虚拟化成员列表与画布双向悬停联动高亮，并单独归类越界无效坐标成员。
   - **UI 关注点**：单文件 1,510 行触及红线；检查亮色主题下暗色雷达画布与外围浅色边框/列表的过渡协调性。

2. **单键时序编辑器 (`ZedisTimeSeriesEditor`) & 多序列对齐探索器 (`ZedisTimeSeriesExplorer`)** — `src/views/timeseries_editor.rs`、`src/views/timeseries_explorer.rs`
   - **入口**：打开 `TSDB-TYPE` 键进入单键编辑器；Tools → Observability → TimeSeries Explorer 进入跨键对齐探索器。
   - **功能**：单键视图展示 `TS.INFO` 元数据、支持时间窗口（`15m`–`All`）与聚合桶（`AVG / SUM / MIN / MAX / COUNT`）的 `TS.RANGE` GPU 折线图、降采样规则（Compaction Rules）新建与删除、`TS.ADD` 写入样本点及 `TS.ALTER` 修改保留期/重复策略/Labels；多序列探索器通过 `TS.MRANGE FILTER` 同时拉取最多 8 条序列叠绘在同一时间轴对比，并在下方生成 `min / max / avg / last` 汇总表。
   - **UI 关注点**：**【高优修复】** `timeseries_explorer.rs` 顶栏缺少底部分割线，将返回按钮、标题、过滤器、聚合器、时间段与 Run 挤在同一行，且时间段按钮误用 `.primary()` 导致同栏出现两个 `.primary()` 主按钮，同时根容器误设了全局等宽字体（`#L498`）。

3. **Redis 8 向量集合编辑器 (`ZedisVectorSetEditor`)** — `src/views/vector_set_editor.rs`
   - **入口**：打开 Redis 8.0+ 原生 `vectorset` 类型键自动挂载。
   - **功能**：展示 `VINFO` / `VCARD` / `VDIM` 维度与量化类型元数据、`VRANDMEMBER` 随机抽样浏览；核心提供交互式 **`VSIM` KNN 向量近邻搜索工作台**（支持按已有元素名 `ELE` 或直接输入浮点向量数组 `VALUES` 检索，可调 `K`、`EF` 搜索宽度、`FILTER` 过滤表达式、`FILTER-EF` 以及 Redis 8.2+ `WITHATTRIBS` JSON 属性），搜索结果支持一键 **Hop-by-Hop 跳点探索相邻节点**；支持 `VADD` 写入向量、复制 `VEMB` 原始向量、`VSETATTR` 编辑关联 JSON 属性与 `VREM` 删除。
   - **UI 关注点**：布局现代清晰，重点核查长 JSON 属性列与相似度分数进度条在窄分栏下的截断表现。

4. **可视化位图矩阵编辑器 (`ZedisBitmapEditor`)** — `src/views/bitmap_editor.rs`
   - **入口**：在任意 `String` 键点击 `⋯` → `View as Bitmap`。
   - **功能**：每页以 64×64 像素格（4,096 bits = 512 Bytes，MSB-first）GPU 矩阵直观绘制每一个二进制位，悬停查看精确全局 bit 偏移量与所属字节偏移；默认只读防手滑，开启显式 **`Edit bits` 开关**后点击任意格子即刻发送 `SETBIT` 翻转 0/1；顶部展示全键 `BITCOUNT` 置位统计与首个 0/1 `BITPOS`、支持按偏移量快速跳页、底部内置整数位域操作器 `BITFIELD`（支持有符号/无符号 `i8/u16/...` 的 `GET / SET / INCRBY`）以及多键位运算弹窗 `BITOP`（`AND / OR / XOR / NOT`）。
   - **UI 关注点**：移除根容器全局等宽字体（`#L672`）；修复 `BitOpForm`（`#L730`）运算类型单选按钮误用 `.primary()` 的问题。

5. **概率型数据结构统一查看器 (`ZedisProbabilisticEditor`)** — `src/views/probabilistic_editor.rs`
   - **入口**：打开 RedisBloom 模块五类键自动识别挂载。
   - **功能**：一套界面自适应五种结构——**Bloom Filter**（`BF.INFO` 容量/层级/误判率 + `BF.EXISTS` 存在性探测 + `BF.ADD`）、**Cuckoo Filter**（`CF.INFO` 桶统计 + `CF.EXISTS` / `CF.COUNT` + `CF.ADD` / `CF.DEL`）、**Count-Min Sketch**（`CMS.INFO` 宽高 + `CMS.QUERY` 频次估算 + `CMS.INCRBY`）、**Top-K**（`TOPK.INFO` + `TOPK.LIST WITHCOUNT` 实时热榜表 + `TOPK.QUERY` / `TOPK.ADD`）、**t-Digest**（`TDIGEST.INFO` + 自动计算 `p50 / p90 / p95 / p99 / p99.9` 分位数卡 + 自定义分位/CDF 查询与 `TDIGEST.ADD`）。
   - **UI 关注点**：移除根容器全局等宽字体（`#L352`）。

6. **HyperLogLog 基数查看器 (`ZedisHllEditor`)** — `src/views/hll_editor.rs`
   - **入口**：打开带有 `HYLL` 魔数头的 `String` 键时自动识别切换为专用 HLL 视图（亦可切回原始字节查看）。
   - **功能**：展示 `PFCOUNT` 估算去重基数、内部编码（`sparse` 稀疏 / `dense` 稠密）与字节体积卡片；支持多元素换行/空格分隔批量 `PFADD`（实时反馈基数寄存器是否发生变化）以及跨键合并弹窗 `PFMERGE`。
   - **UI 关注点**：移除根容器全局等宽字体（`#L333`）。

7. **String 最长公共子串比对弹窗 (`open_lcs_dialog`)** — `src/views/lcs_dialog.rs`
   - **入口**：String 编辑器 `⋯` → `LCS Compare`（需 Redis 7.0+ / Valkey 7.2+）。
   - **功能**：输入同 DB 另一个 String 键名，调用服务端原生 `LCS` 计算最长公共子串文本、仅算长度（`LEN`）或带偏移矩阵的匹配片段明细（`IDX` + `MINMATCHLEN`）。
   - **UI 关注点**：将结果输出区的裸 `Label`（`#L191`）替换为可划选复制的只读 `Editor`。

---

### 50.2 应用级配置、解码扩展与 Web 账户管理窗口（5 个）

1. **应用偏好设置 (`ZedisSettingEditor`)** — `src/views/setting_editor.rs`、`setting_editor/fields.rs`、`secondary_window.rs`
   - **入口**：`⌘,` / `Ctrl+,` 或齿轮菜单（桌面端为单例独立窗口 `SecondaryWindow`，Web 端为模态框）。
   - **功能**：左侧分组导航 + 右侧即时生效表单：
     - *Appearance*：全局 UI 字号缩放（rem 驱动全界面等比缩放）、UI 字体族、等宽代码字体族、界面语言；
     - *Date & Time*：本地时区 / UTC 切换、日期时间格式化模板；
     - *Key Behavior*：键树最大展开层级、单次 `SCAN` 数量、小目录自动展开阈值、键树是否展示 TTL、JSON 默认打开模式（`Text | Tree`）、删键进入本地回收站开关、超大值截断阈值、Value Search 默认扫描上限；
     - *Workspace Tabs*：点击左侧边栏连接是否默认在新标签打开；
     - *Desktop 专属分组*：`Redis`（默认连接/响应超时）、`AI`（OpenAI 兼容接口 Base URL、脱敏 API Key、模型名）、`System`（HTTP 代理、系统托盘图标开关、自动检查更新与预览版通道、打开配置目录、macOS 一键注册 `redis://` / `zedis://` 系统默认协议处理器、打开自定义 `keybindings.toml`）、`Local Data`（一键全量导出/导入本地 `redb` 收藏、标签、脚本与历史备份 JSON）。
   - **UI 关注点**：表单对齐规整，需在大字号下核查左侧分组导航栏宽度是否充足不截断多国语言（尤其是德语/俄语）。

2. **Protobuf Schema 解码映射管理 (`ZedisProtoEditor`)** — `src/views/proto_editor.rs`
   - **入口**：`Route::Protos`（齿轮菜单 → Settings → Protobuf，仅桌面端）。
   - **功能**：按服务器与键匹配规则（全部 / 前缀 / 后缀 / 正则）绑定本地 `.proto` 搜索目录、描述文件与目标 `Message` 类型；命中规则的二进制 String 值在 `ZedisBytesEditor` 中自动反序列化为可读 JSON 树。
   - **UI 关注点**：✅ 已修复：按 `content_area_width` 自适应计算列宽、为每行编辑/删除按钮使用 `(id, row_ix)` 唯一 ID、并将表单校验错误文案纳入 8 国语言包（`common.server_required` / `common.name_required` / `common.match_pattern_required`）。

3. **自定义外部脚本解码器 (`ZedisScriptEditor`)** — `src/views/script_editor.rs`
   - **入口**：`Route::Scripts`（齿轮菜单 → Settings → Script Viewers，仅桌面端）。
   - **功能**：为私有二进制协议配置外部命令行双向管道：按服务器与键匹配规则配置 **Decode 命令**（原始字节经 `stdin` 喂入，读取 `stdout` 展示）与可选 **Encode 命令**（开启后允许在编辑器直接修改解码文本并经脚本重新编码写回 Redis！）；内置 9 套开箱即用模板（`xxd`、`zstd`、`msgpack`、`base64`、`jwt` 等）并支持 JSON 导入导出。
   - **UI 关注点**：✅ 已修复：与 `proto_editor.rs` 同步按 `content_area_width` 自适应计算列宽、使用 `(id, row_ix)` 唯一按钮 ID、并将表单校验错误文案纳入 8 国语言包（`script_editor.shell_command_required` 等）。

4. **Web 桥接多租户账户与管理员后台 (`ZedisAccount`, ADR 17)** — `src/views/account.rs` + `account/{admin,audit,handover,invites,online,password,signups,tokens}.rs`
   - **入口**：Web 部署版本登录态下，右上角齿轮菜单 → `Account…`。
   - **功能**：按角色动态渲染左侧导航栏的模态管理中心：
     - *所有登录用户*：修改个人登录密码（`password.rs`）、签发/吊销只读 **MCP 令牌**（供外部 AI Agent 通过 `/mcp` 安全访问桥接器，`tokens.rs`）；
     - *Admin 管理员专属*：实时查看并强制踢出在线会话（`online.rs`）、账号全生命周期管理（创建用户、锁定/解锁、删除、离职资产交接 `handover.rs`、细粒度连接与命令 RBAC 授权规则 `admin.rs`）、生成注册邀请码（`invites.rs`）、审批自助注册申请（`signups.rs`）以及查询服务端安全操作审计日志（`audit.rs`）。
   - **UI 关注点**：子页面众多，需在无头浏览器下统一走查亮/暗主题、表格空状态及危险操作（踢人/删号/吊销 Token）的确认流一致性。

5. **关于窗口 (`open_about_window`)** — `src/views/about.rs`
   - **入口**：齿轮菜单 → `About`（仅桌面端 `600×500` 固定尺寸独立窗口）。
   - **功能**：展示应用图标、版本号、Git Commit SHA、当前 GPU 渲染后端信息、开源仓库与问题反馈外链（`Link`），以及一键复制完整系统环境信息（`Copy System Info`）。

---

## 八、全局 UI 优化共性问题汇总（建议横向专项治理）

结合代码走查与 [ui-design-standards.md](ui-design-standards.md) 的十条规范，除了按上述 P0 → P4 逐页精修外，以下 **7 类跨视图系统性 UI 债务** 非常适合按专题横向一次性扫清：

1. **消除同屏多个 `.primary()` 主按钮与误用 `.primary()` 表达选中态（违反规范 5）** ✅ 已完成
   - 每个决策平面最多只保留 **1 个** `.primary()` 按钮（对应回车提交动作），其余分段切换与单选胶囊一律改用 `.outline().selected(true)`、`ghost().selected(true)` 或 `ButtonGroup`：
   - 已整改完成：`ZedisMultiSearch` 顶部 Scope 切换（`multi_search.rs`）、`ZedisTimeSeriesExplorer` 时间范围胶囊与 Run 按钮（`timeseries_explorer.rs`）、`ZedisTopology` 集群节点页 `Meet` 与 `Replicate`（`cluster_nodes.rs`）、`ZedisCopyKeyDialog` 服务器列表项（`copy_key_dialog.rs`）、`BitOpForm` 位运算选择器（`bitmap_editor.rs`）及 `ZedisPersistence`（`persistence.rs`）。

2. **清理面板根容器上的全局等宽字体设置（违反规范 3）** ✅ 已完成
   - UI 标题、说明正文、空状态引导与按钮文案均恢复使用系统/UI 字体，仅键名、值内容、Redis 命令、指标数字与快捷键保留使用 `get_mono_font_family()`。
   - 已全部清理并保留数据区局部等宽：`metrics.rs`、`slowlog_editor.rs`、`memory_analysis/render.rs`、`clients_manager.rs`、`value_search.rs`、`hotkeys.rs`、`acl_manager.rs`、`timeseries_explorer.rs`、`bitmap_editor.rs`、`hll_editor.rs`、`probabilistic_editor.rs`、`server_info.rs`、`pubsub_editor.rs`、`keyspace_notifications.rs`、`server_load.rs`、`topology.rs`、`search_manager.rs`、`geo_map/render.rs`、`timeseries_editor.rs`、`vector_set_editor.rs`。

3. **统一全部 `ServerView` 面板的顶栏组件 `zedis_ui::panel_header()`（违反规范 1 & 4）** ✅ 已完成
   - 已将全部 `ServerView` 面板与子视图（含 `Clients`、`SearchManager`、`ValueSearch`、`Config`、`ACL`、`Persistence`、`KeyspaceNotifications`、`Hotkeys`、`ServerLoad`、`Workbench`、`TimeSeriesExplorer`、`Benchmark`、`Metrics`、`Topology`、`LuaScriptLibrary`、`LuaDebugger`、`Functions`）统一迁移至 `zedis_ui::panel_header()`。

4. **修正 Hover / Selected 表面对比度倒挂与硬编码圆角（违反规范 3 & 5）** ✅ 已完成
   - 修复 `ZedisKeyTree`（`key_tree/delegate.rs#L190`）暗色模式下悬停蓝色高亮强于选中态的问题，统一收敛到主题语义色 `list_hover` < `list_active`；
   - 清理 `key_timeline_dialog.rs`、`benchmark/render.rs`、`memory_analysis/schema_dialog.rs`、`kv_table/entry_panel.rs`、`migration_window/sections.rs`、`compare_window.rs`、`key_export.rs` 中全部硬编码 `.rounded(px(...))`，统一对齐 `theme.radius` / `theme.radius_lg`。

5. **将多行值预览与命令结果中的裸 `Label` 升级为只读 `Editor`（违反规范 7）** ✅ 已完成
   - 已全部升级为只读 `Editor`：`ZedisKvTable`、`ZedisKeyTimelineDialog`、`ZedisLuaScriptLibrary`、`ZedisWorkbench` 语句结果卡片（`results.rs`）及 `LcsDialog` 结果区（`lcs_dialog.rs`）。

6. **升级最后两张遗留配置页表格并清除硬编码英文（违反规范 1, 5, 9）** ✅ 已完成
   - 已更新 `proto_editor.rs` 与 `script_editor.rs`：按 `content_area_width` 自适应列宽、为每行操作按钮赋予带行索引 `(id, row_ix)` 唯一 `ElementId`，并将残留英文校验错误提示纳入全部 8 国语言包。

7. **顺带拆分触及 1,500 行红线的超大视图模块（遵守 `AGENTS.md` 模块化规范）** ✅ 已完成
   - 已拆分 `migration_window.rs`（→ `migration_window/{ops,render,sections}.rs`）、`config_editor.rs`（→ `config_editor/render.rs`）、`search_manager.rs`（→ `search_manager/{index_forms,render}.rs`）、`keyspace_notifications.rs`（→ `keyspace_notifications/table.rs`）、`geo_map.rs`（→ `geo_map/render.rs`），均已降至 1,500 生产代码行以下。
