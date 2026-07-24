# Changelog

## [0.6.4](https://github.com/vicanso/zedis/compare/v0.6.3..v0.6.4) - 2026-07-24

### ⛰️  Features

- *(editor)* Enrich the no-key empty state with actions and tips - ([3330bcb](https://github.com/vicanso/zedis/commit/3330bcbadace3b09e80369ec3d1127eb4bb348be))
- *(security)* Encrypt config secrets with a per-machine key ([#117](https://github.com/orhun/git-cliff/issues/117)) - ([012fe59](https://github.com/vicanso/zedis/commit/012fe596249d208efbfa5b4543ab8b56a5b4939e))

### 🐛 Bug Fixes

- *(dialog)* Dismiss open dialog on Esc instead of swallowing it - ([bd0c683](https://github.com/vicanso/zedis/commit/bd0c683aae95c8ec03a25e98aa19672257ca9433))
- *(i18n)* Repair mojibake in translations and guard against it in build.rs ([#118](https://github.com/orhun/git-cliff/issues/118)) - ([1edc42f](https://github.com/vicanso/zedis/commit/1edc42f06519874a4fe42433a7e5c5fb56935bda))
- *(kv-table)* Account for the workspace tab strip in edit-form height - ([9739ff1](https://github.com/vicanso/zedis/commit/9739ff1c334855c55f75d4ab15f425bd0aef2f3d))
- *(tabs)* Persist Home tabs so they survive a restart - ([db7eee2](https://github.com/vicanso/zedis/commit/db7eee235d6ab083d902e4cd8802041f8089a682))

### ⚙️ Miscellaneous Tasks

- *(master-key)* Skip the OS keychain for RUST_ENV=dev runs - ([309939d](https://github.com/vicanso/zedis/commit/309939d2af3137df26531ac7a839a6dd47594ccf))

## [0.6.3](https://github.com/vicanso/zedis/compare/v0.6.2..v0.6.3) - 2026-07-21

### ⛰️  Features

- *(multi-search)* Add title-bar menu entry to open it - ([bfebd7c](https://github.com/vicanso/zedis/commit/bfebd7c3291b37abcf8ae3742644280b30d0b5a0))
- *(tabs)* Add "open servers in a new tab" preference - ([2891824](https://github.com/vicanso/zedis/commit/28918248c8186abeb54448c506cfbfc6d3803abe))
- *(tabs)* Duplicate-tab gesture (⌘⇧-click) and ⌘W to close a tab - ([a737d09](https://github.com/vicanso/zedis/commit/a737d0939c95d1723e91853fc03bff2d7f5d07ec))
- *(title-bar)* Inline × on the update chip to dismiss it for the session - ([1a5ad02](https://github.com/vicanso/zedis/commit/1a5ad02fc4318f30a70f1d2abddce27b269537d8))
- *(ui)* Surface keyboard shortcuts on the controls they belong to - ([0068bff](https://github.com/vicanso/zedis/commit/0068bff0151221b3ee07df5a14e81cd04cd6848b))
- Multi-database key search palette (⌘⇧F) - ([ec2d7c3](https://github.com/vicanso/zedis/commit/ec2d7c398129f15c209e2344ac242e0e32a230ca))

### 🐛 Bug Fixes

- *(key-tree)* Keep re-search clean while a prefix "Load more" is in flight - ([9f92019](https://github.com/vicanso/zedis/commit/9f92019e228763fda02573b373eaacd986fa5900))

### 🚜 Refactor

- *(settings)* Pin 14px default UI font size + compact status-bar chips - ([985822d](https://github.com/vicanso/zedis/commit/985822d4d3d4b74286ca1a5d7e2de3b26a91de5d))
- *(status-bar)* Native ghost buttons for right-side chips - ([f439ab4](https://github.com/vicanso/zedis/commit/f439ab458453e2633dfc0fa65a8d646ee9b71058))
- *(ui)* Polish multi-database search palette layout - ([1f4908b](https://github.com/vicanso/zedis/commit/1f4908bdab5ea0558d4f0600d12feac719f4edbe))
- Change key tree min width - ([3f30f80](https://github.com/vicanso/zedis/commit/3f30f804807f6ea530861ab358de5f90d873a33c))

### 📚 Documentation

- Update readme - ([61fcecc](https://github.com/vicanso/zedis/commit/61fceccf32a66af4c7d0524283a30a04d09177f5))

### ⚡ Performance

- *(multi-search)* Use a fixed SCAN COUNT of 1000, not the result cap - ([ff79217](https://github.com/vicanso/zedis/commit/ff79217c71da48b519ff6e30b4ef072d7b8398aa))

### 🎨 Styling

- *(tables)* Mute column-header text (drop primary accent) - ([f164707](https://github.com/vicanso/zedis/commit/f164707d2fb6984342cf3c1e5573507f85ceacaa))

### ⚙️ Miscellaneous Tasks

- *(smoke)* Add a hard macOS first-frame gate; linux stays a build gate - ([4643b41](https://github.com/vicanso/zedis/commit/4643b4115295bc3f7f9b842f17f4f5f745d972d4))
- Pin all workflows to Rust 1.97.1 - ([9e5eb41](https://github.com/vicanso/zedis/commit/9e5eb41e30c7900b243aed63b5a21bc065132918))

### I18n

- *(kv-table)* Localize Field/Value/Score/Entry Id/TTL headers - ([0f1d066](https://github.com/vicanso/zedis/commit/0f1d066b4303e37925a9cd6a288bad8dcfb2d885))

## [0.6.2](https://github.com/vicanso/zedis/compare/v0.5.4..v0.6.2) - 2026-07-18

### ⛰️  Features

- *(capability)* Wire read-class capabilities; harden CI publish flow - ([b118996](https://github.com/vicanso/zedis/commit/b118996516a41dda40c80feae4903c27966e4636))
- *(command-stats)* Filter idle/self noise, summary, export, fix first-sample flash - ([856b803](https://github.com/vicanso/zedis/commit/856b8039e8e18c3727e40b8fb5c28820c8ad49ad))
- *(config)* Add help text for 39 more Redis config params - ([7c2635d](https://github.com/vicanso/zedis/commit/7c2635de903903e7220e3561817ccb0134687d72))
- *(functions)* Sticky form + templates, FCALL, filter, DUMP/RESTORE/FLUSH/STATS - ([fcbb7a2](https://github.com/vicanso/zedis/commit/fcbb7a21469b0217b747d5606db28cedf38ac04d))
- *(keyspace)* Empty states, filters, pause/export, enable presets - ([4e4aa9a](https://github.com/vicanso/zedis/commit/4e4aa9a42ba683121fc32fdc7773ccf68642efd4))
- *(kv-table)* Resizable entry panel width, persisted globally - ([ce39eb1](https://github.com/vicanso/zedis/commit/ce39eb18da496769923d4409fc470e3d13e62b28))
- *(lua-scripts)* Sticky form, templates, cache warm/flush, filter, import/export - ([b122e8b](https://github.com/vicanso/zedis/commit/b122e8b15b1d45c13566b88ce3c51269e73d154c))
- *(persistence)* Richer status cards, CONFIG policy, cluster rows, completion toasts - ([c51f715](https://github.com/vicanso/zedis/commit/c51f7159cf1c50694f16b207e97e39526a33c3d4))
- *(tabs)* Per-tab ⌘1–8 shortcut hints; fix inactive title color - ([81aec22](https://github.com/vicanso/zedis/commit/81aec22ca55d6846e50ae78d4c755dce3f4b118c))
- *(tabs)* Middle-click close, context menu, drag reorder - ([5ad3f10](https://github.com/vicanso/zedis/commit/5ad3f107a27813cfdb05f671c7f1c76266b866f9))
- *(topology)* Nodes UX, ClusterWrite gate, reshard pickers, slot/load links - ([aac057f](https://github.com/vicanso/zedis/commit/aac057f104bf11512c487a4e349bdc28cc509431))
- *(update)* Live download progress, then quit to install - ([c06bd69](https://github.com/vicanso/zedis/commit/c06bd697e454316004f1dbc486d8b13ce55c132a))
- *(value)* Size-gate oversized String/JSON loads; enforce read-only in bytes editor - ([cee17ab](https://github.com/vicanso/zedis/commit/cee17abd5e30d71774357086b631289d30338645))
- Polish status/title bar and topology lag; drop clippy allows - ([01807ef](https://github.com/vicanso/zedis/commit/01807efb7b0fd795ad784cf05c6d22b46bf22985))
- Value-search UX polish and ⌘1–8 workspace tab switching - ([c4a0e86](https://github.com/vicanso/zedis/commit/c4a0e861facaaad6f338ca389fbe27689768196d))

### 🐛 Bug Fixes

- *(acl)* Restore the user editor — missing footer, inputs losing state - ([b0a594e](https://github.com/vicanso/zedis/commit/b0a594e20f0771d637c76e6739b8f10a5a3aaa14))
- *(config)* Stop edit cards ballooning in the parameter grid - ([338f8e8](https://github.com/vicanso/zedis/commit/338f8e88ba6a9fc454964b8ed8f58b9e99769d2b))
- *(editor)* Stop the key name collapsing to an ellipsis - ([addc659](https://github.com/vicanso/zedis/commit/addc659bec2f42cdb146fa6e3500ddd155e4d2b3))
- *(focus)* Keep ⌘F working after workspace tab switches - ([7447d97](https://github.com/vicanso/zedis/commit/7447d97244350dfab21f3ff7803498a322a1340f))
- *(tables)* Vertically center header text in all custom render_th - ([3071f26](https://github.com/vicanso/zedis/commit/3071f26fcfe39d38dba38640e4c03bd73707a3a9))
- *(ui)* Scrollbars clipped by max_h in trash dialog and import preview - ([409a495](https://github.com/vicanso/zedis/commit/409a495ca094a265f7199b16aadbe320afe07bf2))
- *(value)* Keep NUL-bearing bytes as Bytes so bitmaps are detected - ([5e2a54a](https://github.com/vicanso/zedis/commit/5e2a54aaaa93d149b85e30280a3d3e49da401634))

### 🚜 Refactor

- *(clients)* Filter by client type; fix truncated cells - ([93941ba](https://github.com/vicanso/zedis/commit/93941ba023a343ee0ea36f79823100fce2502ff4))
- *(config)* Move help docs to embedded per-locale JSON - ([76d194d](https://github.com/vicanso/zedis/commit/76d194d8670aed25e22b74474bc0cbab69293cb5))
- *(connection)* Extract the connection layer into zedis-connection - ([8d31935](https://github.com/vicanso/zedis/commit/8d319356ecf28d082d11eee87471364e9ff85b9f))
- *(connection)* Drop the gpui dependency from the connection layer - ([ced69a7](https://github.com/vicanso/zedis/commit/ced69a7ce3657c66f80afa17acd9c919decee19f))
- *(db)* Extract zedis-db and split the error type by domain - ([e2b131b](https://github.com/vicanso/zedis/commit/e2b131bcbb260c39b628c61bfbed8d2d69ee10c5))
- *(search)* Smarter query chips, examples, pagination, SORTBY/DIALECT - ([b46de6e](https://github.com/vicanso/zedis/commit/b46de6e12f54b4b369f5c126bb58f65fd62c7512))
- *(sidebar)* Label on the collapse toggle when expanded - ([33e623d](https://github.com/vicanso/zedis/commit/33e623dc4922c3660d3c025bfbf522025ca91db1))
- *(slowlog)* Keyword filter; commands as a checkable dropdown - ([a922ce1](https://github.com/vicanso/zedis/commit/a922ce14af217488981ec08a90979bf8170b7d99))
- *(status-bar)* Show connection health as a link/unlink icon ([#111](https://github.com/orhun/git-cliff/issues/111)) - ([23e0e73](https://github.com/vicanso/zedis/commit/23e0e735168c495844c51721d6e4a2fa96c9cbd3))
- *(ui)* Primary blue, full-height sidebar, filter shortcuts - ([5d47713](https://github.com/vicanso/zedis/commit/5d477135ffc20ab03f4e1d5d89cdf2e579999b85))
- *(views)* Memory-analysis dropdown + Analyze fix; MONITOR production warning - ([baef9a0](https://github.com/vicanso/zedis/commit/baef9a054f8454aa75637f7d003c9f3ce6e39976))
- *(views)* Split the four largest view files into submodules - ([656eccf](https://github.com/vicanso/zedis/commit/656eccf8b51c27f987ab715ba412c5b7053ec038))

### 📚 Documentation

- Crate READMEs and workspace-tab interactions in FEATURES - ([c3ce9ab](https://github.com/vicanso/zedis/commit/c3ce9ab297bbdf4ff3a1c2758a941830535f0444))

### ⚡ Performance

- *(i18n)* Parse embedded locales lazily, one per first lookup - ([7f314af](https://github.com/vicanso/zedis/commit/7f314af580e9d421228b88200c97f35a7e571b51))
- *(key-tree)* Arc-share key TTLs and cut build-time string churn - ([e85e0c1](https://github.com/vicanso/zedis/commit/e85e0c1160b2e3054c6a0ebf830a942dd91535ba))
- Cut idle cluster/UI churn and speed value-search / palette paths - ([2232cc8](https://github.com/vicanso/zedis/commit/2232cc804dacae2855883db0996780a5d60d2458))
- Cut redundant I/O and per-frame work across hot paths - ([28ade35](https://github.com/vicanso/zedis/commit/28ade3517ab475fd396e22650d8f4d8379439004))

### ⚙️ Miscellaneous Tasks

- *(i18n)* Drop 46 unused locale keys across all languages - ([5c8ea3c](https://github.com/vicanso/zedis/commit/5c8ea3c2676b775cc535068be8a9a6989192280d))
- *(publish)* Rewrite upload_asset.sh with the gh CLI - ([ff945bd](https://github.com/vicanso/zedis/commit/ff945bd4b43a5ce1f9645252def830d85b5682bb))
- *(publish)* Don't create a draft when the tag already has a release - ([b2e463c](https://github.com/vicanso/zedis/commit/b2e463c0d06cd6531592ea122f358f302f756b95))
- *(smoke)* Linux smoke was never green — drop the self-drive, go best-effort - ([ecf226f](https://github.com/vicanso/zedis/commit/ecf226f7f7891339ff2149890f0bf68811fa02e6))
- *(smoke)* Self-drive frames in smoke mode; add per-push Linux gate - ([00010db](https://github.com/vicanso/zedis/commit/00010db703dcc0d5cb5272c18d45e4fa7afbd432))
- *(test)* Run the workspace test suite in CI; lock tab-move arithmetic - ([6e6a909](https://github.com/vicanso/zedis/commit/6e6a90908b0add3863e0adf13b4d5f17f84e9362))
- Version 0.6.1 - ([c32b053](https://github.com/vicanso/zedis/commit/c32b05333a13f18bddf66341b253490ebc13b29a))
- Version 0.6.0 - ([a617f85](https://github.com/vicanso/zedis/commit/a617f85d4bb43349417956817e42d839e9067980))

### I18n

- Fill in the English fallbacks across all 7 non-en locales - ([105dccc](https://github.com/vicanso/zedis/commit/105dccc6b8da39cd4aaa54fa3550c5daa96e55fa))

## [0.5.4](https://github.com/vicanso/zedis/compare/v0.5.2..v0.5.4) - 2026-07-11

### ⛰️  Features

- *(capability)* Wire PublishMessage / FunctionWrite / EvalScript gates - ([9a5cbdc](https://github.com/vicanso/zedis/commit/9a5cbdcb7bc8eea111982a1381c190e04d5ddd3e))
- *(clients)* Batch kill filtered client connections - ([57873c5](https://github.com/vicanso/zedis/commit/57873c56604edb07660f1eae2eaa8230452aeda9))
- *(config)* Bilingual (en/zh) full help text in the config popover - ([e6537be](https://github.com/vicanso/zedis/commit/e6537be8d9bf3cf88d3edda92bfb17889a61a434))
- *(connection)* Add read-only capability matrix with table-driven tests - ([3658a2a](https://github.com/vicanso/zedis/commit/3658a2adf52060181de010f0f43bc3c0a3f9ef55))
- *(docs)* Add GitHub Pages landing site for zedis.net - ([ef7bc72](https://github.com/vicanso/zedis/commit/ef7bc720488614cfdeb188d6c8ff58aeb28f9090))
- *(help)* In-panel help popovers for specialized panels - ([1957b25](https://github.com/vicanso/zedis/commit/1957b254119c0845b8579e0d5602a17c1c5e3267))
- *(key-tree)* Copy / rename / favorite in the context menu - ([2deda1d](https://github.com/vicanso/zedis/commit/2deda1db4929e4ad3f4ddc36bb27ab3e23a031b7))
- *(key-tree)* Batch tag selection, folder tag aggregates, surface Tag filter - ([46c9369](https://github.com/vicanso/zedis/commit/46c936909ebae021c1c2d30072fef97de416738b))
- *(key-tree)* Combine type, tag, and TTL filters with local AND - ([db3e3cd](https://github.com/vicanso/zedis/commit/db3e3cd873221974ac6b160131409ef7dcc2eca2))
- *(keys)* Recently opened keys — key-tree menu and ⌘P Quick Open - ([73978ce](https://github.com/vicanso/zedis/commit/73978ce41e5869fb55a00d48ef86cd10f5a9f0b1))
- *(migration)* Conflict strategy preview and move Import Keys to Tools - ([93e7df6](https://github.com/vicanso/zedis/commit/93e7df691680950f8dc040c499f9ac914612bc2c))
- *(servers)* Per-server key-tree prefs and Keys form tab - ([d249ddb](https://github.com/vicanso/zedis/commit/d249ddbaa4f67877415851e01471ad4c91ba5a32))
- *(settings)* Configurable UI and monospace fonts - ([5194ee5](https://github.com/vicanso/zedis/commit/5194ee51267059407a314e15459ed2e09977412e))
- *(sidebar)* Theme-aware logo (light variant on dark themes) - ([e3b5fbe](https://github.com/vicanso/zedis/commit/e3b5fbe394ccb92bf6ed94d9785d801c81b6c3d1))
- *(sidebar)* Rework home row and collapse toggle - ([d803c95](https://github.com/vicanso/zedis/commit/d803c95a6ea8c88f09925a669a50f5779fb299f3))
- *(status-bar)* Connect/disconnect toggle on the health dot ([#111](https://github.com/orhun/git-cliff/issues/111)) - ([1e9a13c](https://github.com/vicanso/zedis/commit/1e9a13c547662231121706eeb84535d4c3fe1aa7))
- *(tabs)* Multi-connection workspace tabs - ([07cfab9](https://github.com/vicanso/zedis/commit/07cfab9ba08aeaa4cc9f1c60de2e8ad3ae137de8))
- *(theme)* Brand-blue primary for default light/dark themes - ([00f371f](https://github.com/vicanso/zedis/commit/00f371fa2f13375c11f5bae4a1fadcd77d2b2509))
- *(topology)* Slot map, node load heatmap, and reshard wizard - ([c5fa55f](https://github.com/vicanso/zedis/commit/c5fa55fa9802178e438298b48aaa70377d3aea32))
- *(views)* Jump to key from Monitor / Keyspace / Memory Analyzer - ([4f06d06](https://github.com/vicanso/zedis/commit/4f06d06d07e346329e5c9c2ccdd6e3b5a5313455))

### 🐛 Bug Fixes

- *(about)* Make the About window movable - ([d8bc7a3](https://github.com/vicanso/zedis/commit/d8bc7a336a6258dbf037a6051c04f47f425dbf12))
- *(content)* Clear startup loading skeleton when server load finishes - ([b40a999](https://github.com/vicanso/zedis/commit/b40a999c17f599e8aa36e3640e2b429008c4d58e))
- *(docs)* Make Download button label high-contrast on dark landing page - ([72ef324](https://github.com/vicanso/zedis/commit/72ef324109a17785026ee6bbcfb92da5b14ff16d))
- *(docs)* Host screenshots in docs/images and brand ZEDIS - ([5f75918](https://github.com/vicanso/zedis/commit/5f7591828bae27a7ae6ab7bb46d8ad8895d20f19))
- *(key-tree)* Keep folder refresh in read-only context menu ([#114](https://github.com/orhun/git-cliff/issues/114)) - ([76916cb](https://github.com/vicanso/zedis/commit/76916cb0bed0834b60f4c80b00ead8f42cfcf6ce))
- *(linux)* Set window title and app_id so Wayland shows name and icon ([#106](https://github.com/orhun/git-cliff/issues/106)) - ([8edb191](https://github.com/vicanso/zedis/commit/8edb191a7365ce2a2db34c117867b0967689ffab))
- *(startup)* Load proto/script/lua caches before the window opens ([#105](https://github.com/orhun/git-cliff/issues/105)) - ([011c0dd](https://github.com/vicanso/zedis/commit/011c0dd4a1726975d56ec068a424a7d702c23901))
- *(titlebar)* Baseline-align server name and host - ([8d5904c](https://github.com/vicanso/zedis/commit/8d5904c3984d36f9802b8f6802d2c22278860996))
- *(tray)* Sync quick-connect with main window via GlobalEvent - ([5a3f498](https://github.com/vicanso/zedis/commit/5a3f498775062009b7c13b21de2eaf8872662c23))
- *(update)* Show scrollbar for long release notes in the update dialog - ([15abde2](https://github.com/vicanso/zedis/commit/15abde2540a4e92c22c12c1a7a4f25f900ec74c3))
- *(updater)* Restore markdown release notes in the update prompt - ([e71fc67](https://github.com/vicanso/zedis/commit/e71fc672fee0faa927afe4297502296eddc00495))

### 🚜 Refactor

- *(content)* Keep key-tree across server tool routes - ([aa7aa55](https://github.com/vicanso/zedis/commit/aa7aa557e77c9a3ead682cb9f2c2825f6274e377))
- *(kv-table)* Gate modes through the capability matrix - ([5fbecbb](https://github.com/vicanso/zedis/commit/5fbecbbb07ead909f7b461190e667984735b5a85))
- *(kv-table)* Scale edit-form field heights with the font size ([#108](https://github.com/orhun/git-cliff/issues/108)) - ([f9c5e92](https://github.com/vicanso/zedis/commit/f9c5e92665bfa77fb109ff3e56972e58bc96e749))
- *(kv-table)* Allow entry preview on read-only connections ([#109](https://github.com/orhun/git-cliff/issues/109)) - ([3acd890](https://github.com/vicanso/zedis/commit/3acd8903ba04c3e03273e5e7ad1f4f8a897926e2))
- *(views)* Split oversized view files into submodules - ([390695d](https://github.com/vicanso/zedis/commit/390695d37c34e4ec0e97e91dcd39e06af04478df))

### ⚙️ Miscellaneous Tasks

- Update dependencies - ([d90a802](https://github.com/vicanso/zedis/commit/d90a8025451f314246d144c8542cb12c87e9a23d))

## [0.5.2](https://github.com/vicanso/zedis/compare/v0.5.1..v0.5.2) - 2026-07-05

### ⛰️  Features

- *(keys)* Local recycle bin for deleted keys - ([11b0a84](https://github.com/vicanso/zedis/commit/11b0a844e5c7148647e4490f6de1959fc91d8a1b))
- *(metrics)* Persist metrics history with 1h/24h/7d chart ranges - ([37b83f5](https://github.com/vicanso/zedis/commit/37b83f5ed9aace10ade3d42d565e7cd945ee82f7))
- *(servers)* Staged connection diagnostics - ([f4a278c](https://github.com/vicanso/zedis/commit/f4a278c86b8efa522f9db698b4e9ca8427c5beff))

### 🐛 Bug Fixes

- *(tray)* Quick-connect and new-connection broken by route refactor - ([ca9323d](https://github.com/vicanso/zedis/commit/ca9323d81a9c54dae94ee3b4af3b5573016cfc52))

### 🚜 Refactor

- *(icon)* Update zedis icons - ([28c5cee](https://github.com/vicanso/zedis/commit/28c5cee81c48f5516bdea51978586f23fe72cfb2))

### 🧪 Testing

- Add route-logic tests and cross-platform smoke gate - ([d10113b](https://github.com/vicanso/zedis/commit/d10113bd008fe6a238ab3cedbaba735612ed6dff))

## [0.5.1](https://github.com/vicanso/zedis/compare/v0.4.6..v0.5.1) - 2026-07-04

### ⛰️  Features

- *(key-tree)* Sticky ancestor breadcrumb and large-tree polish - ([ea27724](https://github.com/vicanso/zedis/commit/ea277240a0cf6d4856524c8abf5c45d6c10f2a4d))
- *(servers)* Passphrase-encrypted share tokens for config export/import - ([4d849aa](https://github.com/vicanso/zedis/commit/4d849aa6995f561ee3fb3ad76acfb6d8b26b17dd))
- *(ssh)* Smarter agent auth — pinned .pub key, success memory, fail fast on disconnect ([#107](https://github.com/orhun/git-cliff/issues/107)) - ([2597385](https://github.com/vicanso/zedis/commit/2597385e250d05f4852714a6b78235dcd3ce02a6))
- *(status-bar)* Full-width bottom bar with design-aligned styling - ([7dad4ac](https://github.com/vicanso/zedis/commit/7dad4acadbf46d86a74bb021a9713f674256b745))
- Bundle a monospace font across data views and restructure + persist routing - ([c57ca19](https://github.com/vicanso/zedis/commit/c57ca191fda4e034764ff45fc631a85b28c189b7))

### 🐛 Bug Fixes

- *(editor)* Honor the read-only toggle on already-open value editors - ([cbcd50d](https://github.com/vicanso/zedis/commit/cbcd50da83f56e8b51c07273b69257677471eebe))
- *(key-tree)* Attribute and order nested "Load more" rows - ([6467bed](https://github.com/vicanso/zedis/commit/6467bed2b665db146943820b18f3ba185c45db7e))
- *(views)* Guard config load on empty server, brighten RediSearch schema text - ([4965a96](https://github.com/vicanso/zedis/commit/4965a96e11c97babd4d1a89b47a3f887da517b2e))
- Fixed the issue where the main window would not appear after Windows startup. ([#98](https://github.com/orhun/git-cliff/issues/98)) - ([30ff3a7](https://github.com/vicanso/zedis/commit/30ff3a75a383b0c33d9e24953e3b97a7665e7380))

### 🚜 Refactor

- *(connection)* Hint at TLS on dropped links + handle servers without ROLE - ([29a619e](https://github.com/vicanso/zedis/commit/29a619e70a4bc234abd83c1d2446dd67172f64b6))
- *(editor)* Redesign value-editor toolbar and add undo/redo - ([940bf7d](https://github.com/vicanso/zedis/commit/940bf7d7196a33808ebc4d3f127b03a8cbe378dd))
- *(key-tree)* Restyle rows to match the design mockup - ([60049c3](https://github.com/vicanso/zedis/commit/60049c3e1d8199c66eb63db69beaadbcb0fb7927))
- *(servers)* Redesign the dashboard — toolbar, search, and footer actions - ([3fb20a7](https://github.com/vicanso/zedis/commit/3fb20a7534d9779559d1976eda58c137ee6bc602))
- *(sidebar)* Redesign server rail with database icons and a collapsible icon-only mode - ([79f5ca5](https://github.com/vicanso/zedis/commit/79f5ca50f9735648729bdafa9203e07c64fe9d4f))
- *(ui)* Apply monospace font to editor, status bar, key-tree, value editors and data panels - ([0fbd699](https://github.com/vicanso/zedis/commit/0fbd699c4c8a05aaa086a257012e72e671b09484))
- *(ui)* Bundle JetBrains Mono and apply it across data views - ([a584a4f](https://github.com/vicanso/zedis/commit/a584a4fcd2448a2ee0522b26965fd94700194362))
- *(updater)* Disable in-app update check for App Store builds - ([45efbaf](https://github.com/vicanso/zedis/commit/45efbaf2623cc65d29199fa93bf34b584b7244a6))
- *(updater)* Move update chip to the title bar with click-to-recheck - ([267aff5](https://github.com/vicanso/zedis/commit/267aff5fe4e8d81e86f358c67445c07f7a3c3294))
- Adjust spinner for views - ([b4ebf24](https://github.com/vicanso/zedis/commit/b4ebf240c4d819e0002f6752bea4439b668ac232))
- Adjust route server schema - ([045d8a1](https://github.com/vicanso/zedis/commit/045d8a1eba299c379fc73599467542a65436aa03))
- Key-tree hover/progress polish, grouped counts - ([d7ff119](https://github.com/vicanso/zedis/commit/d7ff119c6ab9a89488daab0c04fd8cdf4d411481))

### ⚙️ Miscellaneous Tasks

- Adjust schedule build - ([daff635](https://github.com/vicanso/zedis/commit/daff635f4cf5ae8cfa274ac555633a05be719e11))
- Adjust development build - ([e193829](https://github.com/vicanso/zedis/commit/e193829964151ed2ee39aacfc5292c10a8a3776f))
- Version 0.5.0 - ([faffff2](https://github.com/vicanso/zedis/commit/faffff29fdc093b22300b84fa28f1cb8d02a1fcc))
- Add script for flatpak and widget - ([d7b88c3](https://github.com/vicanso/zedis/commit/d7b88c38c361ed655f621b44fb8ee1b45544bb71))

## [0.5.0](https://github.com/vicanso/zedis/compare/v0.4.6..v0.5.0) - 2026-07-03

### ⛰️  Features

- *(key-tree)* Sticky ancestor breadcrumb and large-tree polish - ([ea27724](https://github.com/vicanso/zedis/commit/ea277240a0cf6d4856524c8abf5c45d6c10f2a4d))
- *(servers)* Passphrase-encrypted share tokens for config export/import - ([4d849aa](https://github.com/vicanso/zedis/commit/4d849aa6995f561ee3fb3ad76acfb6d8b26b17dd))
- *(status-bar)* Full-width bottom bar with design-aligned styling - ([7dad4ac](https://github.com/vicanso/zedis/commit/7dad4acadbf46d86a74bb021a9713f674256b745))
- Bundle a monospace font across data views and restructure + persist routing - ([c57ca19](https://github.com/vicanso/zedis/commit/c57ca191fda4e034764ff45fc631a85b28c189b7))

### 🐛 Bug Fixes

- *(editor)* Honor the read-only toggle on already-open value editors - ([cbcd50d](https://github.com/vicanso/zedis/commit/cbcd50da83f56e8b51c07273b69257677471eebe))
- *(key-tree)* Attribute and order nested "Load more" rows - ([6467bed](https://github.com/vicanso/zedis/commit/6467bed2b665db146943820b18f3ba185c45db7e))
- *(views)* Guard config load on empty server, brighten RediSearch schema text - ([4965a96](https://github.com/vicanso/zedis/commit/4965a96e11c97babd4d1a89b47a3f887da517b2e))

### 🚜 Refactor

- *(connection)* Hint at TLS on dropped links + handle servers without ROLE - ([29a619e](https://github.com/vicanso/zedis/commit/29a619e70a4bc234abd83c1d2446dd67172f64b6))
- *(editor)* Redesign value-editor toolbar and add undo/redo - ([940bf7d](https://github.com/vicanso/zedis/commit/940bf7d7196a33808ebc4d3f127b03a8cbe378dd))
- *(key-tree)* Restyle rows to match the design mockup - ([60049c3](https://github.com/vicanso/zedis/commit/60049c3e1d8199c66eb63db69beaadbcb0fb7927))
- *(servers)* Redesign the dashboard — toolbar, search, and footer actions - ([3fb20a7](https://github.com/vicanso/zedis/commit/3fb20a7534d9779559d1976eda58c137ee6bc602))
- *(sidebar)* Redesign server rail with database icons and a collapsible icon-only mode - ([79f5ca5](https://github.com/vicanso/zedis/commit/79f5ca50f9735648729bdafa9203e07c64fe9d4f))
- *(ui)* Apply monospace font to editor, status bar, key-tree, value editors and data panels - ([0fbd699](https://github.com/vicanso/zedis/commit/0fbd699c4c8a05aaa086a257012e72e671b09484))
- *(ui)* Bundle JetBrains Mono and apply it across data views - ([a584a4f](https://github.com/vicanso/zedis/commit/a584a4fcd2448a2ee0522b26965fd94700194362))
- *(updater)* Disable in-app update check for App Store builds - ([45efbaf](https://github.com/vicanso/zedis/commit/45efbaf2623cc65d29199fa93bf34b584b7244a6))
- *(updater)* Move update chip to the title bar with click-to-recheck - ([267aff5](https://github.com/vicanso/zedis/commit/267aff5fe4e8d81e86f358c67445c07f7a3c3294))
- Adjust spinner for views - ([b4ebf24](https://github.com/vicanso/zedis/commit/b4ebf240c4d819e0002f6752bea4439b668ac232))
- Adjust route server schema - ([045d8a1](https://github.com/vicanso/zedis/commit/045d8a1eba299c379fc73599467542a65436aa03))
- Key-tree hover/progress polish, grouped counts - ([d7ff119](https://github.com/vicanso/zedis/commit/d7ff119c6ab9a89488daab0c04fd8cdf4d411481))

### ⚙️ Miscellaneous Tasks

- Add script for flatpak and widget - ([d7b88c3](https://github.com/vicanso/zedis/commit/d7b88c38c361ed655f621b44fb8ee1b45544bb71))

## [0.4.6](https://github.com/vicanso/zedis/compare/v0.4.4..v0.4.6) - 2026-06-27

### ⛰️  Features

- *(config)* Loading indicator + type-aware editing ([#97](https://github.com/orhun/git-cliff/issues/97)) - ([f2730e9](https://github.com/vicanso/zedis/commit/f2730e9b35242f5c4bb1bcd7d7cb4d70ce39540e))
- *(startup)* Show a clear error and exit when the database is locked - ([16de150](https://github.com/vicanso/zedis/commit/16de150a2a8267ee6199a744bc60f3752b99017f))
- *(updater)* Add download progress and refine update logic - ([2e58908](https://github.com/vicanso/zedis/commit/2e58908a736663abf39d8a64350196efc59d5dba))
- *(window)* Remember window position per display (uuid-anchored) - ([74272bc](https://github.com/vicanso/zedis/commit/74272bc1aa7bc24324bbab414bd19bcb07712ac9))
- Add tracing appender log - ([27cbb09](https://github.com/vicanso/zedis/commit/27cbb09496d6cc99555d6f5c1db60e3ca390e37f))
- In-app update check with assisted install - ([45b6ec7](https://github.com/vicanso/zedis/commit/45b6ec77f87e36c3d8533a89a7046c0df8d152db))
- One-click reconnect + ⌘E rename-key shortcut - ([d8c1831](https://github.com/vicanso/zedis/commit/d8c1831842623f9ba71a2c265e929cadf4c01ea8))

### 🐛 Bug Fixes

- *(hash)* Allow adding a field without a TTL (and actually save it) - ([d8af45c](https://github.com/vicanso/zedis/commit/d8af45ccaf135b7f78a22e9ffc85dd585017ae77))

### 🚜 Refactor

- *(settings)* Nest theme & language into submenus - ([19930e6](https://github.com/vicanso/zedis/commit/19930e64aa9189b57cde780e4bb33b0290e10ae4))
- *(ui)* Zed-style settings menu + ⌘W close + startup flash fix - ([fb435d7](https://github.com/vicanso/zedis/commit/fb435d7760ceefdc4ff6f8462583ec23eb7cdd0d))
- *(window)* Cross-platform ⌘W close + extend startup flash fix to Windows - ([8b60b64](https://github.com/vicanso/zedis/commit/8b60b644aca284a14a9992491abd6104ac2cf9e8))
- Adjust file logging, cross-platform shortcuts & config-editor polish - ([f222e05](https://github.com/vicanso/zedis/commit/f222e0557e13b3132b3432daad06a8dc06274f50))
- Surface available updates in the status bar - ([f5c5b7a](https://github.com/vicanso/zedis/commit/f5c5b7a8f1f8cc0666c63356033b3beeec5f5c4b))
- Surface live-tail / MONITOR connection failures - ([9682a37](https://github.com/vicanso/zedis/commit/9682a373ded44b3ce00c703a70f26789875916e0))
- Status-bar reconnect with failure reason + ⌘E rename shortcut - ([7f6eac1](https://github.com/vicanso/zedis/commit/7f6eac11597508ad6f60825dadd1bb7267276058))

### 📚 Documentation

- Update readme - ([8b4ee24](https://github.com/vicanso/zedis/commit/8b4ee24b57672cd4aa9a4563aa262ded4d68ea9a))

### ⚙️ Miscellaneous Tasks

- Update tray lib - ([cc8f238](https://github.com/vicanso/zedis/commit/cc8f2381919761094d6c60b1cd5ee2a09ed60cdf))
- Version 0.4.5 - ([dea9e42](https://github.com/vicanso/zedis/commit/dea9e420f7eff68e7038da110efc4420f57bbae5))

### Build

- *(app-store)* Patch gpui_platform/gpui_macros, not just gpui - ([14eba6d](https://github.com/vicanso/zedis/commit/14eba6d3ec724e3fc006eb8206cc1bf3a3b12183))

## [0.4.5](https://github.com/vicanso/zedis/compare/v0.4.4..v0.4.5) - 2026-06-26

### ⛰️  Features

- *(config)* Loading indicator + type-aware editing ([#97](https://github.com/orhun/git-cliff/issues/97)) - ([f2730e9](https://github.com/vicanso/zedis/commit/f2730e9b35242f5c4bb1bcd7d7cb4d70ce39540e))
- *(startup)* Show a clear error and exit when the database is locked - ([16de150](https://github.com/vicanso/zedis/commit/16de150a2a8267ee6199a744bc60f3752b99017f))
- *(updater)* Add download progress and refine update logic - ([2e58908](https://github.com/vicanso/zedis/commit/2e58908a736663abf39d8a64350196efc59d5dba))
- *(window)* Remember window position per display (uuid-anchored) - ([74272bc](https://github.com/vicanso/zedis/commit/74272bc1aa7bc24324bbab414bd19bcb07712ac9))
- Add tracing appender log - ([27cbb09](https://github.com/vicanso/zedis/commit/27cbb09496d6cc99555d6f5c1db60e3ca390e37f))
- In-app update check with assisted install - ([45b6ec7](https://github.com/vicanso/zedis/commit/45b6ec77f87e36c3d8533a89a7046c0df8d152db))
- One-click reconnect + ⌘E rename-key shortcut - ([d8c1831](https://github.com/vicanso/zedis/commit/d8c1831842623f9ba71a2c265e929cadf4c01ea8))

### 🚜 Refactor

- *(settings)* Nest theme & language into submenus - ([19930e6](https://github.com/vicanso/zedis/commit/19930e64aa9189b57cde780e4bb33b0290e10ae4))
- *(ui)* Zed-style settings menu + ⌘W close + startup flash fix - ([fb435d7](https://github.com/vicanso/zedis/commit/fb435d7760ceefdc4ff6f8462583ec23eb7cdd0d))
- *(window)* Cross-platform ⌘W close + extend startup flash fix to Windows - ([8b60b64](https://github.com/vicanso/zedis/commit/8b60b644aca284a14a9992491abd6104ac2cf9e8))
- Adjust file logging, cross-platform shortcuts & config-editor polish - ([f222e05](https://github.com/vicanso/zedis/commit/f222e0557e13b3132b3432daad06a8dc06274f50))
- Surface available updates in the status bar - ([f5c5b7a](https://github.com/vicanso/zedis/commit/f5c5b7a8f1f8cc0666c63356033b3beeec5f5c4b))
- Surface live-tail / MONITOR connection failures - ([9682a37](https://github.com/vicanso/zedis/commit/9682a373ded44b3ce00c703a70f26789875916e0))
- Status-bar reconnect with failure reason + ⌘E rename shortcut - ([7f6eac1](https://github.com/vicanso/zedis/commit/7f6eac11597508ad6f60825dadd1bb7267276058))

## [0.4.4](https://github.com/vicanso/zedis/compare/v0.4.3..v0.4.4) - 2026-06-19

### ⛰️  Features

- Show module/version-gated tools disabled with a why-hint - ([55c9476](https://github.com/vicanso/zedis/commit/55c94767babd3668bb7a5458679cea6b08106661))
- Themes, font slider, per-server DB memory, type filter, palette polish - ([77524ba](https://github.com/vicanso/zedis/commit/77524bab2a2a20a998f2bb42c7e3f9d808e7e3d6))
- VS Code-style scoped command palette (keys, favorites, commands) - ([111f2f2](https://github.com/vicanso/zedis/commit/111f2f2ad2cab2b9500ecd6e8932ff86f0402a64))
- Search loaded keys from the command palette - ([64c36f8](https://github.com/vicanso/zedis/commit/64c36f8e341857c57c9347aadeb8be89bf663496))
- Welcome empty state for the home server list and live connection status dot in the status bar - ([07872cb](https://github.com/vicanso/zedis/commit/07872cb26abb857541968fca422450e73bcc2666))
- Import and export Redis connections - ([6fd5625](https://github.com/vicanso/zedis/commit/6fd5625d86bc64ebdbf453ef94e9e09d5468d529))
- Import Redis Insight connections, with localized import errors - ([ba1ca0f](https://github.com/vicanso/zedis/commit/ba1ca0f5e7c1c0234bbe801d9d9f941863b4bc37))
- Auto-expand single-child folder chains in the key tree - ([d9c2e7d](https://github.com/vicanso/zedis/commit/d9c2e7d2098861c3293d5943343e06bc185d377c))
- Key-tree inline delete + collapsible JSONPath bar - ([3da1f3a](https://github.com/vicanso/zedis/commit/3da1f3a239509714a9749b3c91f072bd18fe53d7))

### 🚜 Refactor

- Empty state for the editor when no key is selected - ([eae6c2f](https://github.com/vicanso/zedis/commit/eae6c2f543d1f01a3754fdd91b11066f29c92c85))

### 📚 Documentation

- Update readme - ([92f2c86](https://github.com/vicanso/zedis/commit/92f2c86b7ea4b75c5dcbcf3cf4d7afb8d95a99d5))

### ⚙️ Miscellaneous Tasks

- Version 0.4.4 - ([21bc3e1](https://github.com/vicanso/zedis/commit/21bc3e18260c1ecbfd81311cc01f398c2c520d00))
- Binary-size + UX tuning and a documentation overhaul - ([80716c8](https://github.com/vicanso/zedis/commit/80716c88a791cff23cf016cac540da0244649c4b))

## [0.4.3](https://github.com/vicanso/zedis/compare/v0.4.2..v0.4.3) - 2026-06-13

### ⛰️  Features

- CSV export for collection editors (Hash/List/Set/Zset/Stream) - ([40e3a29](https://github.com/vicanso/zedis/commit/40e3a29377357c12e8c7eb0a8fbc0bbf0023109f))
- Keyboard shortcut for deleting the selected key - ([99412a2](https://github.com/vicanso/zedis/commit/99412a271b8bf4e3f1121989afd34cf77da2e0de))
- Server env tags as a fixed preset (None/Dev/UAT/Prod) - ([a968b50](https://github.com/vicanso/zedis/commit/a968b5058bf532f76bc32c75c1f5c1a1e9f50cfc))
- Shortcuts overlay, Slow Log export, per-server timeouts; faster conn failure - ([c43a0c9](https://github.com/vicanso/zedis/commit/c43a0c9c0de83ba10bbd519c3abb8762025d4caa))
- Cross-server config diff (CONFIG GET *) - ([311e56c](https://github.com/vicanso/zedis/commit/311e56cd94e4a442c1b3710ece92d58f11833a3d))
- Reuse key_type_badge as component in editor select key titlebar ([#91](https://github.com/orhun/git-cliff/issues/91)) - ([280e172](https://github.com/vicanso/zedis/commit/280e172d419a65152de0f303eab506a8053c81ec))
- Value search, command stats, cross-server diff, batch TTL & CSV export - ([0026c5b](https://github.com/vicanso/zedis/commit/0026c5b993388379ea518c37eb0fd561d4c1fda3))
- Command Stats page + Tools menu grouping + persistence card fix - ([fef2517](https://github.com/vicanso/zedis/commit/fef25179807189980f0bc0c33877dcd65a5df2c5))
- HyperLogLog + Bitmap viewers, key rename / cross-server copy - ([891117a](https://github.com/vicanso/zedis/commit/891117ac1d97027fcaa209554f4e83254ad99112))
- GEO map viewer for geospatial sorted sets - ([80d1231](https://github.com/vicanso/zedis/commit/80d123182275c67e69b1aed07351b57719ac653b))
- AI optimization advice for memory analysis - ([242cf1a](https://github.com/vicanso/zedis/commit/242cf1ab2674ff438cb64117b5b7fb016a36e6a2))

### 🐛 Bug Fixes

- Surface swallowed load errors instead of empty panels - ([de659b9](https://github.com/vicanso/zedis/commit/de659b93ac1579649d45d2e6cadeeb2f6756d4c3))
- Memory analysis surfaces scan errors + samples large DBs by default - ([4dff1ad](https://github.com/vicanso/zedis/commit/4dff1ad639db7b1ec3da85a15e77732399285d5b))
- Key type badge sizes to content instead of wrapping - ([f456e28](https://github.com/vicanso/zedis/commit/f456e289acae17a2758466816fc7ccd3c4929539))
- Retry server load after failure when re-selecting from home - ([30476d0](https://github.com/vicanso/zedis/commit/30476d0ed2897d9a021f1b82c84b1de790b39b99))
- Fix lint - ([2db2b23](https://github.com/vicanso/zedis/commit/2db2b234364a5d0df9c810a6d5d8250aeae163e6))
- User-configurable db count + INFO keyspace fallback when CONFIG is blocked ([#88](https://github.com/orhun/git-cliff/issues/88)) - ([bafb01f](https://github.com/vicanso/zedis/commit/bafb01f5c7f82eb68bc21c8a16d509b9642516fe))

### 🚜 Refactor

- Move caret to end after selecting a terminal command suggestion ([#90](https://github.com/orhun/git-cliff/issues/90)) - ([ed84df6](https://github.com/vicanso/zedis/commit/ed84df6c4b40942a0cf40b38b16c5590e8a0abc8))
- Environment-oriented tag colors - ([e465c96](https://github.com/vicanso/zedis/commit/e465c967e3dfc82dc7927ec8abedd9914bb5957d))

### ⚡ Performance

- Cache metrics chart data at heartbeat instead of per frame - ([6b551b0](https://github.com/vicanso/zedis/commit/6b551b095e011298dbca8161a3ed62eda016c0be))

### ⚙️ Miscellaneous Tasks

- Update dependencies - ([553000c](https://github.com/vicanso/zedis/commit/553000c69ba3a0c49c037c756424fbfa1c148f1e))

### I18n

- Translate status bar tool tooltips (de/es/fr/ja/pt/ru) - ([9b6ed2b](https://github.com/vicanso/zedis/commit/9b6ed2b687045b5812561c26c5e329c2801fdbe8))

## [0.4.2](https://github.com/vicanso/zedis/compare/v0.4.1..v0.4.2) - 2026-06-07

### ⛰️  Features

- Auto-format Unix timestamp string values - ([6671361](https://github.com/vicanso/zedis/commit/667136158b73de803950051ef631aa63dfd95001))
- Multi-line batch mode in the terminal workbench - ([4212667](https://github.com/vicanso/zedis/commit/42126674e2ede0bfbfabb47d2b49fe121bc9fa5a))
- Redis 8 Vector Set viewer with interactive KNN - ([7e59f3a](https://github.com/vicanso/zedis/commit/7e59f3a8e8a8f31dd3fefe495133d7c9506fbd12))
- RedisBloom probabilistic-structure viewer - ([f6dc1ad](https://github.com/vicanso/zedis/commit/f6dc1ad99291a38a60588ce90e21ed6356ba6bbe))
- Esc returns to editor from tool pages - ([26c19e5](https://github.com/vicanso/zedis/commit/26c19e51755898b4740986349aebb20997a11011))
- RedisTimeSeries chart viewer for TSDB-TYPE keys - ([c87f8b9](https://github.com/vicanso/zedis/commit/c87f8b96663f607ff9cc59a7f2f76f787d810796))
- Accept redis:// connection URI in server import - ([86e84e4](https://github.com/vicanso/zedis/commit/86e84e4b151a1616c44b4fbf8b4e6388ae61789c))
- Adaptive key-tree scanning, merged memory analysis, settings reset - ([a53a911](https://github.com/vicanso/zedis/commit/a53a9115d5e869086449242971ae957a20fbf4f5))
- Per-master scan sizing and streaming prefix scan - ([a89cf9d](https://github.com/vicanso/zedis/commit/a89cf9d59e35f403f5f46a51877ed861e93a5bf8))
- SSH tunnel keyboard-interactive auth + TOFU host key verification - ([c40974e](https://github.com/vicanso/zedis/commit/c40974e73129e7635cf197e1bc0f67cb87eb06ed))

### 🐛 Bug Fixes

- Fix cargo fmt - ([f74b937](https://github.com/vicanso/zedis/commit/f74b9373386dd08ae92b91aeb7ac8a1f27cbbb96))
- Show persistent scrollbar in value diff view and bound its height - ([d5068ab](https://github.com/vicanso/zedis/commit/d5068ab928a7d88411415dca122cd665ba047e31))
- Fix typos - ([64800e2](https://github.com/vicanso/zedis/commit/64800e2c826447529a5115f6fbe1aaf9b70bcc22))

### 🚜 Refactor

- Surface server tag in card and sidebar - ([79cdc04](https://github.com/vicanso/zedis/commit/79cdc04c4d5038a08d43875b153f5320a6d73868))

## [0.4.1](https://github.com/vicanso/zedis/compare/v0.4.0..v0.4.1) - 2026-05-31

### ⛰️  Features

- Implement json diff viewer and key space notification support - ([9533aae](https://github.com/vicanso/zedis/commit/9533aae75f394c5a346e0066a8efdb31917fd52e))
- Support keyboard shortcut to refresh key tree - ([ee58f90](https://github.com/vicanso/zedis/commit/ee58f90893088d610132d3969559ad4e70c65a48))
- Support auto-completion for jsonpath input - ([1c4775f](https://github.com/vicanso/zedis/commit/1c4775feaa936747da02146d6611090482a26689))

### 🐛 Bug Fixes

- Resolve focus loss in cmd+k palette after multiple invocations - ([38feaa3](https://github.com/vicanso/zedis/commit/38feaa3000b60329faaa4aadf677b091c90140fd))

### 🚜 Refactor

- Adjust human readable duration - ([3fb8e18](https://github.com/vicanso/zedis/commit/3fb8e184450a5f02907f549caf22670971a5a64d))
- Optimize json data detection in jsonpath evaluation - ([28b1900](https://github.com/vicanso/zedis/commit/28b1900fc87f553275641ed0e1f384ad9a6dd491))
- Optimize the display layout of the command palette - ([d91731b](https://github.com/vicanso/zedis/commit/d91731bc4f2fba25810502ef18a22535078fd8b5))

## [0.4.0](https://github.com/vicanso/zedis/compare/v0.3.4..v0.4.0) - 2026-05-16

### ⛰️  Features

- Support stream consumer group management and live tailing - ([10a9ae1](https://github.com/vicanso/zedis/commit/10a9ae1bb38c45f476322ad4403071afcf605412))
- Implement command palette (cmd+k) with fuzzy search - ([58d3396](https://github.com/vicanso/zedis/commit/58d3396d5a6d39aaab6f872dbae945c881b873b6))
- Support batch insertion via csv/tsv paste for collections - ([7c0c98f](https://github.com/vicanso/zedis/commit/7c0c98feedeab403f0f9ac82e1fe6ad211c62aeb))
- Support grouping and sorting for server configurations - ([637b288](https://github.com/vicanso/zedis/commit/637b288486d3c7dd2e990489b1368b2a50b63923))
- Add lua script editor - ([02ecf0b](https://github.com/vicanso/zedis/commit/02ecf0b68f7fbedc9cf357a33497ba50204389cd))
- Add redis function editor - ([58faeb2](https://github.com/vicanso/zedis/commit/58faeb2742c66a1ff6ece942e849b7ac11a0adf7))
- Add redis search editor and query builder - ([69cd7a0](https://github.com/vicanso/zedis/commit/69cd7a06a8ab99adc3ea29ca8ec0d355c5b43f00))
- Record modification history in memory for data rollback - ([a7de736](https://github.com/vicanso/zedis/commit/a7de73621fa7ace7e27a25aeac0bb74733a2326c))
- Support editing binary data via Hex editor - ([04c5c87](https://github.com/vicanso/zedis/commit/04c5c87c462141198753177eeed1085327fce4c3))
- Add json path filtering support for json data - ([89b6398](https://github.com/vicanso/zedis/commit/89b6398004e755fb51041cbb5a4829bd42817d94))
- Display ttl for keys in the keytree view - ([f5c4c82](https://github.com/vicanso/zedis/commit/f5c4c8234f753fec89f97274252d0926ba66a08f))
- Add key heat score for memory analysis - ([c329457](https://github.com/vicanso/zedis/commit/c329457f25d141b54f416f8a06547150bb87f757))
- Add cluster topology visualization and replication lag monitoring - ([571737b](https://github.com/vicanso/zedis/commit/571737bc316a08aeed565d23624741d17effdeae))
- Add redis 6+ acl management and connection safety features - ([1d17fc4](https://github.com/vicanso/zedis/commit/1d17fc4ac3297ba902bd109e399b3ddf6c32b972))
- Support data import and export functionality - ([d600c37](https://github.com/vicanso/zedis/commit/d600c37c6c9ce6408535c595f8b40c4f5e6ace4f))

### 🐛 Bug Fixes

- Resolve issue where table fails to load more data ([#82](https://github.com/orhun/git-cliff/issues/82)) - ([015930b](https://github.com/vicanso/zedis/commit/015930b0c90ccbaa34c805e4e12fee812d55ae15))

### 🚜 Refactor

- Optimize the display of server groups - ([3d588b9](https://github.com/vicanso/zedis/commit/3d588b9cc8c94aea2a6e1caafb9b014d536488d8))

## [0.3.4](https://github.com/vicanso/zedis/compare/v0.3.3..v0.3.4) - 2026-04-26

### ⛰️  Features

- *(keytree)* Add reload support for directory ([#67](https://github.com/orhun/git-cliff/issues/67)) - ([3e8a7ba](https://github.com/vicanso/zedis/commit/3e8a7baa27c7c561d3a0e5b6334d7de6978a9bbb))

### 🐛 Bug Fixes

- Fix cargo fmt - ([9967a91](https://github.com/vicanso/zedis/commit/9967a911c8aa5ec21da184cf86937cd0aa0950f0))
- Fix rustls-webpki ([#70](https://github.com/orhun/git-cliff/issues/70)) - ([44f533b](https://github.com/vicanso/zedis/commit/44f533bdc9dcca1a73681f746d649c75f1a34eda))

### 🚜 Refactor

- Pipe notifications to application logs - ([626ce37](https://github.com/vicanso/zedis/commit/626ce3724b67e068b1b8da6dafeb56ed0f37ff70))
- Optimize master name retrieval logic for redis sentinel - ([b57fbec](https://github.com/vicanso/zedis/commit/b57fbecca6e4eb2869b6e10967a51a0aa114e0f3))
- Clear list selection when performing a new search - ([e7fd7da](https://github.com/vicanso/zedis/commit/e7fd7da8431e821ea22efca7d6e1b0a3ccd4c487))
- Support config editor - ([06fffb7](https://github.com/vicanso/zedis/commit/06fffb720759f227482298b99ed3ffff38ef0ade))

### ⚙️ Miscellaneous Tasks

- Update github workflow - ([5de6440](https://github.com/vicanso/zedis/commit/5de6440dad0722f719b7e91fb4b03b6f930d96db))

## [0.3.2](https://github.com/vicanso/zedis/compare/v0.3.1..v0.3.2) - 2026-04-12

### ⛰️  Features

- Support custom scripts for data parsing ([#66](https://github.com/orhun/git-cliff/issues/66)) - ([c231d7b](https://github.com/vicanso/zedis/commit/c231d7bc9935cf332a52275f2426f0f4a2f26359))
- Support field-level ttl for hash (redis 7.4+) - ([2f93fb4](https://github.com/vicanso/zedis/commit/2f93fb40b983458e5ec3bb8c21bff3038b69d9e9))
- Display loaded redis modules in the status bar - ([a4eb85a](https://github.com/vicanso/zedis/commit/a4eb85a70cbe1658dca5f683ff8904558161f32d))
- Dynamically render database list based on server info ([#66](https://github.com/orhun/git-cliff/issues/66)) - ([eeea878](https://github.com/vicanso/zedis/commit/eeea8784e59b3bf5ab571ae3e34a163516e55281))

### 🐛 Bug Fixes

- Limit to a single secondary window per unique page ([#64](https://github.com/orhun/git-cliff/issues/64)) - ([3086053](https://github.com/vicanso/zedis/commit/3086053e98f9961bce75b3ced2b693d26c12ed60))

### 🚜 Refactor

- Auto-select exact match key during scan results - ([64afead](https://github.com/vicanso/zedis/commit/64afead9bf112082b29c156885796f3ceef27748))

### 📚 Documentation

- Update readme - ([30398a7](https://github.com/vicanso/zedis/commit/30398a73f2666d21fe4d16730c4433d1c41e22ae))

## [0.3.2](https://github.com/vicanso/zedis/compare/v0.3.0..v0.3.2) - 2026-03-28

### ⛰️  Features

- Add action_button_factory support to kv table for custom actions - ([746f6b8](https://github.com/vicanso/zedis/commit/746f6b802f53807f153c2d5af8ed9d5f1f43c95e))
- Support sorting and status inspection for redis streams ([#61](https://github.com/orhun/git-cliff/issues/61)) - ([d487f89](https://github.com/vicanso/zedis/commit/d487f89fae1e4cc41359852912f65871794aa198))
- Add i18n support for more languages - ([ac9c618](https://github.com/vicanso/zedis/commit/ac9c618586516f95396d454d6b5c6afae01e69d9))
- Add support for creating new ReJSON-RL keys ([#59](https://github.com/orhun/git-cliff/issues/59)) - ([3fc70c6](https://github.com/vicanso/zedis/commit/3fc70c659c4c94a9be7e826f760816f6395235b8))
- Support partial field updates for redis json using JSON.MERGE - ([eb4ed3d](https://github.com/vicanso/zedis/commit/eb4ed3dd44312717f903cdd52aa4e18cf23a86cf))
- Support overwriting ReJSON-RL data - ([512c69f](https://github.com/vicanso/zedis/commit/512c69f6fb62c0887d063f8784e9665bdce0004b))

### 🐛 Bug Fixes

- Fix loading status of kvtable - ([56bb544](https://github.com/vicanso/zedis/commit/56bb544f79f239bd0437a14461e52eeb1dddff79))
- Fix libcrux-sha3 ([#62](https://github.com/orhun/git-cliff/issues/62)) - ([b6b266a](https://github.com/vicanso/zedis/commit/b6b266aebeb8feb3a048c673d6e65170d4a805fa))
- Fix build for macod x86 - ([f3a721c](https://github.com/vicanso/zedis/commit/f3a721c538dc2b835d2cba827abfd106549b2bf6))

### 🚜 Refactor

- *(setting)* Open settings in a separate window - ([d4588c4](https://github.com/vicanso/zedis/commit/d4588c4f432e4ad0b2bd13c59eb63d7ffc2078b9))
- Refine sorting interaction and default order for stream table - ([e8f8b57](https://github.com/vicanso/zedis/commit/e8f8b574bb93484d6fe83e7fd4008e9285b262c4))
- Adjust language and font setting - ([b746c67](https://github.com/vicanso/zedis/commit/b746c67a726aba4bc86527c752ff2fb105897478))
- Enhance stream data visualization in table and form - ([374cb18](https://github.com/vicanso/zedis/commit/374cb18440e6de3151d638d7f39a21b6b9cdcf03))
- Change redis command terminal to read-only editor mode - ([cd588e6](https://github.com/vicanso/zedis/commit/cd588e62fb2842dc5c8ff1ea077a8ff911cf7f09))

### 📚 Documentation

- Adjust PR template to include code merge guidelines - ([dcb23b1](https://github.com/vicanso/zedis/commit/dcb23b10eb5ffb77dcd56be1919c8a03ffafb95d))
- Update demo image - ([aa7c86e](https://github.com/vicanso/zedis/commit/aa7c86edf08fe9035a214a6c9d041619969c1b2a))

### 🎨 Styling

- Optimize Slow Logs toolbar layout and spacing - ([de09f79](https://github.com/vicanso/zedis/commit/de09f79891f1cabb2665a11a6bb2529a5c35a95a))

### ⚙️ Miscellaneous Tasks

- Update rust version - ([1ed238a](https://github.com/vicanso/zedis/commit/1ed238aaee9f053c88552244fedb02150387eb20))
- Update pull request template - ([fbf5968](https://github.com/vicanso/zedis/commit/fbf5968c40618ac0d9110fa4cdb8955876956963))
- Add rustfmt component - ([94395ab](https://github.com/vicanso/zedis/commit/94395abeae836fe9fc07ec509396781cf96e7c78))
- Update dependencies and rust toolchain to latest - ([51642c7](https://github.com/vicanso/zedis/commit/51642c7f309fd346771067390063caafdcc3b2df))


## [0.3.0](https://github.com/vicanso/zedis/compare/v0.2.7..v0.3.0) - 2026-03-19

### ⛰️  Features

- Add read-only support for ReJSON-RL data type ([#59](https://github.com/orhun/git-cliff/issues/59)) - ([e38d0c3](https://github.com/vicanso/zedis/commit/e38d0c3debe2d77c1ff17bef7bda1eec9458fb91))
- Implement Live Monitor for real-time command streaming - ([b2bd846](https://github.com/vicanso/zedis/commit/b2bd846a89078ed255701ecb61a1f6f1fc87a1bc))
- Add client management dashboard - ([713ed0d](https://github.com/vicanso/zedis/commit/713ed0d10be1b2c38395644f4d9861d6ee3d547b))
- Add system tray support for quick server status monitoring - ([8bf8d67](https://github.com/vicanso/zedis/commit/8bf8d67edb9bca53afbba3706bbe0b08e3b03247))

### 🐛 Bug Fixes

- Fix clippy error - ([55d7197](https://github.com/vicanso/zedis/commit/55d719708f511f08947871ff958089f674a45d87))
- Resolve single command execution in cli mode - ([f977ff7](https://github.com/vicanso/zedis/commit/f977ff7431ffe67631815baef4739673f7e00b01))
- Resolve incorrect rendering of proto editor - ([02d5f46](https://github.com/vicanso/zedis/commit/02d5f4654dfc3b5745ba3db24c933a4369156a25))
- Fix lz4_flex ([#58](https://github.com/orhun/git-cliff/issues/58)) - ([98ab121](https://github.com/vicanso/zedis/commit/98ab1215b64f8198387a98094143b0768a7f38c1))

### 🚜 Refactor

- Unify keytree refresh events and fix sync issue on key deletion - ([ae59e6d](https://github.com/vicanso/zedis/commit/ae59e6dbd567a26524c29367235a18726a422d81))
- Add i18n support for system tray - ([587971d](https://github.com/vicanso/zedis/commit/587971df8f29a4b9e5e6fc0e5a98394135772500))
- Enable system tray for non-Linux platforms only - ([9b71637](https://github.com/vicanso/zedis/commit/9b71637342242e2914750656af9eca017cf7ff4e))
- Only enable system tray for non-Linux platforms only - ([24bddd1](https://github.com/vicanso/zedis/commit/24bddd119ce129cad04a07d5c38be06d26ff3def))
- Optimize error handling and keytree rendering - ([13aa492](https://github.com/vicanso/zedis/commit/13aa492f8cbdf7a123c91d2693a1651e2c928ff4))

### 📚 Documentation

- Change video source to GitHub asset link - ([ad9decc](https://github.com/vicanso/zedis/commit/ad9deccd8dae64b4bee0a33fcb1f1baf72defb4a))
- Update demo video - ([e9e0592](https://github.com/vicanso/zedis/commit/e9e05922376423bafa30dd09acdab771407b181a))

### ⚡ Performance

- Optimize keytree refresh logic during scan operations - ([37eaac4](https://github.com/vicanso/zedis/commit/37eaac4f290025425d9b2c52fc6ab5183cecde68))

### 🎨 Styling

- Optimize status bar layout for better clarity - ([4a01818](https://github.com/vicanso/zedis/commit/4a0181873b92bd0817f38fbd4296028b393bfe32))

### ⚙️ Miscellaneous Tasks

- Update Windows build to generate both MSI and EXE installers ([#49](https://github.com/orhun/git-cliff/issues/49)) - ([07da2d4](https://github.com/vicanso/zedis/commit/07da2d4f67e1bea15d6bbbdc7aa579f1c4d288e2))

## [0.2.7](https://github.com/vicanso/zedis/compare/v0.2.6..v0.2.7) - 2026-03-15

### ⛰️  Features

- Add command and execution time filters for slow logs - ([79e43c2](https://github.com/vicanso/zedis/commit/79e43c2b3029b4174e63336650eb8f511c27c016))
- Add connection test feature - ([edb8412](https://github.com/vicanso/zedis/commit/edb8412ef35047e73ecf629d671669b83b4aa252))
- Add memory analysis view for redis keys - ([b55fc2a](https://github.com/vicanso/zedis/commit/b55fc2a0e673feadc72da6cb1cdf0cad4c4e7d72))
- Add slow log table view - ([d8b0b7a](https://github.com/vicanso/zedis/commit/d8b0b7ad9cbc143639bb5c01592dd9ec9578739e))
- Add automatic decoding for message queue data - ([0c585fc](https://github.com/vicanso/zedis/commit/0c585fcfd2ef3d8f6b9ef10fba1d175828b596f5))
- Add table view for pub/sub messages - ([35fec44](https://github.com/vicanso/zedis/commit/35fec4481d061742b9e3fff12d2653f1b501ff78))

### 🐛 Bug Fixes

- *(windows)* Resolve missing icon for application shortcuts - ([9e49211](https://github.com/vicanso/zedis/commit/9e492117adcb8782a3457fbd44f8f6d89fdcd26d))
- Fix git2 ([#32](https://github.com/orhun/git-cliff/issues/32)) - ([39d2af0](https://github.com/vicanso/zedis/commit/39d2af0d2c578696e90ad5f975a3b4048fc00c6e))

### 🚜 Refactor

- Prevent empty username and password strings in connection url - ([3e508a0](https://github.com/vicanso/zedis/commit/3e508a059d919ff90bff01d925e6cf92efaff4db))
- Add column sorting to slow logs table - ([1da35a2](https://github.com/vicanso/zedis/commit/1da35a2b3f86d2b845be91108e5c5b69e53723f6))
- Optimize SharedString conversion logic - ([1554519](https://github.com/vicanso/zedis/commit/15545196ae23e84cfb6dec6594260e0b99b53765))
- Adjust pubsub editor - ([c450eee](https://github.com/vicanso/zedis/commit/c450eeec2c6ecdffd36212af7f61ba4fce34f221))
- Optimize redis command documentation generation - ([6737343](https://github.com/vicanso/zedis/commit/6737343d9693e71bfac6e38e1b792840f9708a95))

### 📚 Documentation

- Update documentation - ([be1d5fd](https://github.com/vicanso/zedis/commit/be1d5fda6a292eb7a84daf432fd3144b49d9ac09))

### ⚡ Performance

- Add sleep intervals to profiling to reduce redis server load - ([e1aa72e](https://github.com/vicanso/zedis/commit/e1aa72e8562ef9d0e8d2896f9ae7dbe095e8018e))
- Optimize key scanning and memory sampling logic - ([2bd3c9e](https://github.com/vicanso/zedis/commit/2bd3c9e7bd1e1680e4e112cbe38470208081562f))
- Optimize scan logic for cluster mode - ([6e59a94](https://github.com/vicanso/zedis/commit/6e59a94a841a4d344ab02a31424e8c2c4c2e3eb7))
- Optimize memory analysis using pipelining for better performance - ([643f2d1](https://github.com/vicanso/zedis/commit/643f2d1e82f05d307255164ac4a355c3c68f27ef))

### ⚙️ Miscellaneous Tasks

- Add GitHub issue templates - ([42051da](https://github.com/vicanso/zedis/commit/42051da40cdfa34b2447912410180fffa29e1125))
- Fix cargo wix build for windows - ([be8b8fe](https://github.com/vicanso/zedis/commit/be8b8fe196ecc4c11f2e2796fead715910af548c))
- Fix cargo wix build for windows - ([bf4647a](https://github.com/vicanso/zedis/commit/bf4647a8df95445438ec39c0a143372e32cf7cbd))
- Fix cargo wix build for windows - ([51d3374](https://github.com/vicanso/zedis/commit/51d3374eca5aad09b677a9b20a6ed9346a65eb01))
- Fix cargo wix build for windows - ([30db027](https://github.com/vicanso/zedis/commit/30db0272ae6b046a6bbad8d8a0237b6342a292b4))
- Adjust windows build script - ([7671f08](https://github.com/vicanso/zedis/commit/7671f08ebd8ceb1baaa665cb119a4d72ed483d0e))
- Update wix script - ([a242218](https://github.com/vicanso/zedis/commit/a2422182a20184b7b6f6cb73a8a2b2dfe4f6792e))
- Update wix script - ([f055572](https://github.com/vicanso/zedis/commit/f055572f0548fb450bc60c1051b65749889f1e13))
- Update wix script - ([4ef3e59](https://github.com/vicanso/zedis/commit/4ef3e5939fa85e0c3f2a6f76f8095be5ec11d9a7))
- Update wix script - ([c168de9](https://github.com/vicanso/zedis/commit/c168de9b984851bcd4e2841145a1be945eed25d3))
- Update wix script - ([6c39e08](https://github.com/vicanso/zedis/commit/6c39e08420179f4585bdf5f46c07235bbc7da974))
- Add build script for wix - ([6263856](https://github.com/vicanso/zedis/commit/626385674317c46fb94be3359c74db3eaf12294b))
- Use rustls-pki-types instead of rustls-pemfile ([#4](https://github.com/orhun/git-cliff/issues/4)) - ([d31000e](https://github.com/vicanso/zedis/commit/d31000e1a4c4ab7ffcacc7603490fc362c91cf29))

## [0.2.6](https://github.com/vicanso/zedis/compare/v0.2.5..v0.2.6) - 2026-03-08

### ⛰️  Features

- *(ui)* Support adding dynamic fields and editor component in form - ([4420bc9](https://github.com/vicanso/zedis/commit/4420bc9404bd9a4a447dbb531b00ed936fab6a9f))
- *(ui)* Add foot action builder support to zedis form - ([a9a0815](https://github.com/vicanso/zedis/commit/a9a081573fa2e5a9541a15f4735251ecccf632b3))
- Add version detection for valkey - ([fd9f306](https://github.com/vicanso/zedis/commit/fd9f306eee617f57d0ff92ac960f58ed5784736c))
- Add tls support for ssh tunnel connections - ([91becef](https://github.com/vicanso/zedis/commit/91becef5c09c967a6780864beeac7f8fb17ec55b))
- Add support for favoriting keys - ([87560d9](https://github.com/vicanso/zedis/commit/87560d9be575a3dcb9b3db55fe9836ec4509a4ae))
- Add y-axis label support and optimize chart logic - ([b6f7ecd](https://github.com/vicanso/zedis/commit/b6f7ecd1eeda02cd51edc2e9778e409e4062ee00))
- Support pasting values into kv table - ([b493a91](https://github.com/vicanso/zedis/commit/b493a91851ccb640cfbc201d1c65a72105ee8bf3))
- Support copying multiple commands in cli - ([34d6585](https://github.com/vicanso/zedis/commit/34d65855f86b84ac4e8123bddc5c938c78008423))
- Support decimal values for ttl settings - ([adb3d6b](https://github.com/vicanso/zedis/commit/adb3d6b245015b11185c4b9e469d5af9c7d377c0))

### 🐛 Bug Fixes

- Resolve conflict between "test" directory and "test" key - ([0836ed2](https://github.com/vicanso/zedis/commit/0836ed2f074d13322be05704843f2a018227b969))
- Fix get value of form - ([fae10c2](https://github.com/vicanso/zedis/commit/fae10c271f8f96ea24ea2227ec14ca08fdc8f199))
- Keyboard navigation for command history in cli - ([58c5cf9](https://github.com/vicanso/zedis/commit/58c5cf959f0502ea2feb5414ec6fa38b74c6e32c))

### 🚜 Refactor

- *(kv-table)* Optimize edit and add logic - ([a39dc51](https://github.com/vicanso/zedis/commit/a39dc5114f19cb2af28b812a18fd0521e1d38874))
- *(ui)* Migrate form to zedis-ui crate and use custom components - ([85e3983](https://github.com/vicanso/zedis/commit/85e39839d6ddfadb623867370cb63567b6974961))
- Support get connection not from cache pool - ([8e8b56b](https://github.com/vicanso/zedis/commit/8e8b56bfe282ad10bc1313b6c9e9cc55fe3754ea))
- Detect utf8 encoding before decompression to avoid redundant processing - ([8992c54](https://github.com/vicanso/zedis/commit/8992c54c73b97dd4d100bc080c8cd8e050ba5aa4))
- Clarify form field visibility vs submission semantics - ([c5627ac](https://github.com/vicanso/zedis/commit/c5627ac298aa2836a831f2da03648d03187ef5a9))
- Optimize metrics page with i18n label support - ([a28eff4](https://github.com/vicanso/zedis/commit/a28eff41359950b32299d22e8393fe2bf13bf0b6))
- Automatically select and expand directory when adding a new key - ([964bf76](https://github.com/vicanso/zedis/commit/964bf76ffeaa5c9c8312cc7b8396383f51a704fa))
- Skip auto-refresh if data hasn't changed - ([857e10d](https://github.com/vicanso/zedis/commit/857e10da1272e4326ff0161f4480958a68695437))
- Remove appears_transparent from about window - ([301ca09](https://github.com/vicanso/zedis/commit/301ca095ab75610a742091096105a51ce2b7c28c))
- Optimize copy logic and text ellipsis for kv table - ([dac4044](https://github.com/vicanso/zedis/commit/dac4044c698c88a69f09c94f75f416a726d4a929))
- Adjust components - ([eee421b](https://github.com/vicanso/zedis/commit/eee421b568287206ee75f98b2798961ecc9d15c5))
- Optimize element height calculation in KV table - ([548cc70](https://github.com/vicanso/zedis/commit/548cc70d4acfaef36c6f61c908aeeb988cfd0de9))
- Add "About" link to the feature dropdown menu - ([f648e46](https://github.com/vicanso/zedis/commit/f648e46d6e597b10fd7f270c78d1197f805790e7))
- Add system information display to about page - ([067b01d](https://github.com/vicanso/zedis/commit/067b01d17985ff49705ae8bb567c798a3ec815ca))
- Add `FluentBuilder` for dialog and form - ([27fad16](https://github.com/vicanso/zedis/commit/27fad16598f899c9cefdf23a5e295dacd10862b9))
- Adjust dialog component - ([d2f1750](https://github.com/vicanso/zedis/commit/d2f1750a3066902403dbc759b909a5158aa1a3bc))
- Use development versions of gpui and gpui-component - ([6b2a187](https://github.com/vicanso/zedis/commit/6b2a1877ed950ea2d1d5c3a453c1684391412fa1))

### 📚 Documentation

- Update skills - ([89c9090](https://github.com/vicanso/zedis/commit/89c9090a1c6b6244d705c9cff65a0391c079a383))

### ⚙️ Miscellaneous Tasks

- Fix build for linux - ([3c38cad](https://github.com/vicanso/zedis/commit/3c38cad766d9f4e181c1d2cf72bd969e313e7e53))

## [0.2.5](https://github.com/vicanso/zedis/compare/v0.2.4..v0.2.5) - 2026-02-23

### ⛰️  Features

- *(cli)* Support command history navigation ([#37](https://github.com/orhun/git-cliff/issues/37)) - ([4d858d6](https://github.com/vicanso/zedis/commit/4d858d6fa647fa398ba40e6842bae0ce90c59d47))
- *(metrics)* Implement server metrics dashboard - ([07657f5](https://github.com/vicanso/zedis/commit/07657f5bc5bb7e33b9b33e344495de3d2a4578bb))
- *(slowlog)* Display slow log count for current period - ([5e99e81](https://github.com/vicanso/zedis/commit/5e99e81013de5ad93369cd9be287ff739b973837))
- Support redis stream data type ([#35](https://github.com/orhun/git-cliff/issues/35)) - ([a4c3f61](https://github.com/vicanso/zedis/commit/a4c3f61642bd21c58d44711f52e981088ac47ab0))

### 🐛 Bug Fixes

- Pipeline exception when deleting multiple keys in cluster mode - ([233621a](https://github.com/vicanso/zedis/commit/233621a648be86a566101a4a342c27582022cc35))

### 🚜 Refactor

- *(key)* Trim redis keys before addition - ([6980de3](https://github.com/vicanso/zedis/commit/6980de3beaac45bcbbd5418dc24b925831dae2a3))
- *(kvtable)* Add auto_created field to kv table - ([6d6cb88](https://github.com/vicanso/zedis/commit/6d6cb8847699972e17546e2687043bce26f99c63))
- *(metrics)* Add more metrics - ([8cc684e](https://github.com/vicanso/zedis/commit/8cc684ef932aa9cfe97c8383e370f15b33a9f468))
- *(metrics)* Optimize chart tick display logic - ([031a3e2](https://github.com/vicanso/zedis/commit/031a3e2e9ddc16a94065cfbdd0efc491441324e4))
- *(metrics)* Add more server metric charts - ([ceb18bb](https://github.com/vicanso/zedis/commit/ceb18bbe3169bb2db0706732c4315ef0f1bd6135))
- *(metrics)* Optimize Redis metrics collection and caching - ([7e0996f](https://github.com/vicanso/zedis/commit/7e0996fc9b250f6fa2de4fca54671630fefe38e4))
- *(redis)* Ignore `role` command error then use standalone mode ([#41](https://github.com/orhun/git-cliff/issues/41)) - ([9558c8b](https://github.com/vicanso/zedis/commit/9558c8bcb514e18b673f9608520305c3afe710b3))
- *(redis)* Estimate memory usage for Redis < 4.0 ([#40](https://github.com/orhun/git-cliff/issues/40)) - ([85442c4](https://github.com/vicanso/zedis/commit/85442c47dd23a6222d0d147005d493455ba5daa6))
- Optimize slow log display - ([b71d7e0](https://github.com/vicanso/zedis/commit/b71d7e0536db0969b5a276b2b08eb83823f33f96))
- Adjust memory usage for redis < 4.0 - ([8139731](https://github.com/vicanso/zedis/commit/8139731c56656995f8b780961b880b3a9248eec3))

## [0.2.4](https://github.com/vicanso/zedis/compare/v0.2.3..v0.2.4) - 2026-02-15

### ⛰️  Features

- *(keytree)* Support periodic auto-refresh ([#39](https://github.com/orhun/git-cliff/issues/39)) - ([d49b58a](https://github.com/vicanso/zedis/commit/d49b58a2345836a4e9c52e05c22aabc3574186e2))
- *(value)* Support configurable auto-refresh for key values ([#39](https://github.com/orhun/git-cliff/issues/39)) - ([54064a5](https://github.com/vicanso/zedis/commit/54064a5c4bab4a9d99a8c57c3f9208633d0f6377))

### 🐛 Bug Fixes

- *(kvtable)* Reset state when switching tables - ([bbd7e72](https://github.com/vicanso/zedis/commit/bbd7e725410f81f2b4f95c58cee7dbd6b18d358f))
- *(ui)* Fix blurry windows application icon - ([a20f70e](https://github.com/vicanso/zedis/commit/a20f70ef3ba36b4cb0a854c3c40221aa8b08a65e))
- Fix system language detection - ([d87dcda](https://github.com/vicanso/zedis/commit/d87dcda763eba3a78816b0a2b21f1ece330a57ba))
- Fix read-only permission detection - ([f8622e2](https://github.com/vicanso/zedis/commit/f8622e22020136e159c5ccbba2d0134cd1d1225d))

### 🚜 Refactor

- *(about)* Refine about page - ([39ed6f4](https://github.com/vicanso/zedis/commit/39ed6f4326a57247c0e28f7905894c56bf5c6e7d))
- *(config)* Make session view settings independent of server config - ([dcdc59f](https://github.com/vicanso/zedis/commit/dcdc59f884d430dcc1b79f3e1937258e7419cf32))
- *(keytree)* Refine collapse and multi-selection logic - ([3787d29](https://github.com/vicanso/zedis/commit/3787d29ce4a4635beb1eb73a4dcd16894fa77715))
- *(keytree)* Refine styling - ([7adb6e4](https://github.com/vicanso/zedis/commit/7adb6e411b891fdb329a055e29e297e27a8c9805))
- *(keytree)* Restore search button icon - ([700f020](https://github.com/vicanso/zedis/commit/700f0204fbce7cf6d24fe396d83da59e1c5b9e92))
- *(keytree)* Optimize collapse all logic ([#36](https://github.com/orhun/git-cliff/issues/36)) - ([2fccb87](https://github.com/vicanso/zedis/commit/2fccb871a2fd0ef0bc484644e99e89e91f35e22a))
- *(kvtable)* Reuse edit logic for adding kv elements - ([85b2cdd](https://github.com/vicanso/zedis/commit/85b2cdddedb90e93edc202c6da883d1fac62164f))
- *(kvtable)* Refine editor for kv table - ([1438335](https://github.com/vicanso/zedis/commit/1438335954acd1110ee42647f731620d79476e4e))
- *(statusbar)* Optimize status bar layout - ([248843c](https://github.com/vicanso/zedis/commit/248843c86d57e0a97ceacdb02081a91757d75234))
- *(statusbar)* Adjust rendering timing for status bar - ([85fa4f6](https://github.com/vicanso/zedis/commit/85fa4f6f9749b6e35e1cd5c006e3fa0cc0bf6b9a))
- *(ui)* Adjust width and placeholder for ttl input - ([c950d22](https://github.com/vicanso/zedis/commit/c950d22dccf9b575d0ccb80cfe466a27cb40a584))
- Optimize interaction logic for kv table updates - ([eeff799](https://github.com/vicanso/zedis/commit/eeff799e0da627a4557deda5aea62619bce4836d))
- Optimize system language detection logic - ([43d05a5](https://github.com/vicanso/zedis/commit/43d05a5166be6955a7dd20cdcb8ec93f96cddab7))

### 🎨 Styling

- *(ui)* Improve windows icon clarity - ([90be864](https://github.com/vicanso/zedis/commit/90be86461e1521c57b4bdaec0fcbd6bec58a1c22))

### ⚙️ Miscellaneous Tasks

- Adjust winres build - ([c1a8785](https://github.com/vicanso/zedis/commit/c1a87852544fb5b9a47d0b0bfbdcb7b91f9726b9))
- Add nightly release - ([c3beb8b](https://github.com/vicanso/zedis/commit/c3beb8b435d172e4e16f0881bb165db286f82c2e))
- Add nightly release - ([6e7e4aa](https://github.com/vicanso/zedis/commit/6e7e4aaf674b7589c6b842868c8fc9b1cb786597))
- Add nightly release - ([58989ee](https://github.com/vicanso/zedis/commit/58989eea3d026f8adf31ecca52a1fb746d4e0482))

## [0.2.3](https://github.com/vicanso/zedis/compare/v0.2.1..v0.2.3) - 2026-02-07

### ⛰️  Features

- *(keyscan)* Support configuring scan count - ([9118149](https://github.com/vicanso/zedis/commit/911814990676bdfe4b30154c52494a8f59fa75b4))
- *(proto)* Support selecting target message - ([52f34c1](https://github.com/vicanso/zedis/commit/52f34c1b7d4ce435c4e408cfdce1899fc593bd7b))
- *(ssh)* Support ssh-agent authentication ([#29](https://github.com/orhun/git-cliff/issues/29)) - ([88e9adb](https://github.com/vicanso/zedis/commit/88e9adb83920d9a48334b9f43cfdccf24c562cfb))
- *(ui)* Add advanced section to redis server config - ([527a049](https://github.com/vicanso/zedis/commit/527a04938a0812cd04128ac971ed527d5abb447c))

### 🐛 Bug Fixes

- *(bytes)* Fix integer overflow ([#30](https://github.com/orhun/git-cliff/issues/30)) - ([be9d683](https://github.com/vicanso/zedis/commit/be9d683329fb3ed532993d0d57f486397d8055a3))
- *(cluster)* Fix multi-key deletion - ([ccb5f50](https://github.com/vicanso/zedis/commit/ccb5f50e406c4e7437e1954b578259a2e3842200))
- *(dialog)* Localize confirmation dialog button text - ([a305da4](https://github.com/vicanso/zedis/commit/a305da475f61ac17d40993d1a0bbfb20fe6d487b))
- *(keytree)* Disable delete action in read-only mode - ([9a75fce](https://github.com/vicanso/zedis/commit/9a75fce6de0ced2ab44eb52c57923c9be19e587e))
- *(ssh)* Fix public key path parsing - ([c718c29](https://github.com/vicanso/zedis/commit/c718c299befee16e03fd92bfdeb59f655a7bc1a0))
- *(ssh)* Restrict ssh-agent support to unix platforms ([#29](https://github.com/orhun/git-cliff/issues/29)) - ([7d5eb00](https://github.com/vicanso/zedis/commit/7d5eb006a2f1429b825b8190cb5c1f2293cccc1a))
- Fix denial of service via stack exhaustion ([#34](https://github.com/orhun/git-cliff/issues/34)) - ([39bef06](https://github.com/vicanso/zedis/commit/39bef061ef473f0748801081a7a29f5f096cb157))

### 🚜 Refactor

- *(config)* Make redis server editor height adaptive - ([289fc26](https://github.com/vicanso/zedis/commit/289fc2683fa1ec77b6cf603399937195d2812ab3))
- *(font)* Prefer Menlo on macOS and Cascadia Code on Windows - ([8a40161](https://github.com/vicanso/zedis/commit/8a401612f60b57ce160785bab776a2d413f28f35))
- *(form)* Show validation error messages - ([13c94d4](https://github.com/vicanso/zedis/commit/13c94d491903961d636e7de11a966bc066f1f528))
- *(keytree)* Optimize show_collapse_keys logic - ([0224196](https://github.com/vicanso/zedis/commit/0224196e08f27014f8defcda7393647cfb50ccb0))
- *(keytree)* Add confirmation dialog for deletion - ([bc61880](https://github.com/vicanso/zedis/commit/bc618806a4313d57b3c87d1c406d205a77033acb))
- *(keytree)* Add cmd-f shortcut to focus search input - ([2fe289d](https://github.com/vicanso/zedis/commit/2fe289dd1646de920251c247b185070b99aecda4))
- *(keytree)* Highlight multi-select button when active - ([ec5af84](https://github.com/vicanso/zedis/commit/ec5af8400ddec9d46a7ffb9dbb3ce10b02841c3e))
- *(notification)* Handle notifications as global events - ([1ba78c6](https://github.com/vicanso/zedis/commit/1ba78c62c2a5e025379491cb7926c22f63e5bdcc))
- *(proto)* Enhance editor functionality - ([5eb4cbc](https://github.com/vicanso/zedis/commit/5eb4cbc05ab14b9132df67b1da153b5d1426c6de))
- *(redis)* Adjust timeout handling - ([a60a9d6](https://github.com/vicanso/zedis/commit/a60a9d61ffe416aa0187acf9f0e7ec41b105b21b))
- *(server)* Handle server events as global events - ([1550116](https://github.com/vicanso/zedis/commit/1550116ba813e53b385814f68cbe81e33bd77e3f))

### ⚙️ Miscellaneous Tasks

- *(cargo)* Rename package to zedis-gui - ([946fe5a](https://github.com/vicanso/zedis/commit/946fe5a2951c077fb529bc3c2713c6fb32426935))
- *(ci)* Add support for windows aarch64 - ([70d3205](https://github.com/vicanso/zedis/commit/70d3205535599d9190d71b825e7e1cd07cca8edc))
- *(ci)* Add support for linux aarch64 - ([432b2c3](https://github.com/vicanso/zedis/commit/432b2c391b61e8117e71c0233dc619b4608b15f1))
- *(ci)* Add support for linux aarch64 - ([29f07ee](https://github.com/vicanso/zedis/commit/29f07eeb124fc4f8bc16925236ecc6bbccf0810f))
- *(ci)* Add support for Windows aarch64 - ([36d9fcd](https://github.com/vicanso/zedis/commit/36d9fcd6b9dbe77919c3e0ee2c742871fc949020))
- *(ci)* Adjust rust cache key - ([1b0757c](https://github.com/vicanso/zedis/commit/1b0757cd790444f4e267875bc54eae33ff573cca))
- *(ci)* Downgrade build runner to ubuntu-22.04 ([#33](https://github.com/orhun/git-cliff/issues/33)) - ([d66483d](https://github.com/vicanso/zedis/commit/d66483d17485b1fc0f45a7add034e5e80dc11b30))
- *(ci)* Downgrade build runner to ubuntu-20.04 ([#33](https://github.com/orhun/git-cliff/issues/33)) - ([c812c92](https://github.com/vicanso/zedis/commit/c812c92dd5f5d80eaf26c34a74d8b3531954b670))
- *(ci)* Downgrade build runner to ubuntu-20.04 ([#33](https://github.com/orhun/git-cliff/issues/33)) - ([b41e22b](https://github.com/vicanso/zedis/commit/b41e22b03ab1456957617fcfa9d79edf9fa9beaa))
- Version 0.2.2 - ([899997c](https://github.com/vicanso/zedis/commit/899997c4ce46defddcf03ca2c9b28ece2223e610))
- Add debug log - ([db31b34](https://github.com/vicanso/zedis/commit/db31b343bdf008f4dbb04ec1a651c5564ff99007))

## [0.2.2](https://github.com/vicanso/zedis/compare/v0.2.1..v0.2.2) - 2026-02-07

### ⛰️  Features

- *(keyscan)* Support configuring scan count - ([9118149](https://github.com/vicanso/zedis/commit/911814990676bdfe4b30154c52494a8f59fa75b4))
- *(proto)* Support selecting target message - ([52f34c1](https://github.com/vicanso/zedis/commit/52f34c1b7d4ce435c4e408cfdce1899fc593bd7b))
- *(ssh)* Support ssh-agent authentication ([#29](https://github.com/orhun/git-cliff/issues/29)) - ([88e9adb](https://github.com/vicanso/zedis/commit/88e9adb83920d9a48334b9f43cfdccf24c562cfb))
- *(ui)* Add advanced section to redis server config - ([527a049](https://github.com/vicanso/zedis/commit/527a04938a0812cd04128ac971ed527d5abb447c))

### 🐛 Bug Fixes

- *(bytes)* Fix integer overflow ([#30](https://github.com/orhun/git-cliff/issues/30)) - ([be9d683](https://github.com/vicanso/zedis/commit/be9d683329fb3ed532993d0d57f486397d8055a3))
- *(cluster)* Fix multi-key deletion - ([ccb5f50](https://github.com/vicanso/zedis/commit/ccb5f50e406c4e7437e1954b578259a2e3842200))
- *(ssh)* Restrict ssh-agent support to unix platforms ([#29](https://github.com/orhun/git-cliff/issues/29)) - ([7d5eb00](https://github.com/vicanso/zedis/commit/7d5eb006a2f1429b825b8190cb5c1f2293cccc1a))

### 🚜 Refactor

- *(config)* Make redis server editor height adaptive - ([289fc26](https://github.com/vicanso/zedis/commit/289fc2683fa1ec77b6cf603399937195d2812ab3))
- *(font)* Prefer Menlo on macOS and Cascadia Code on Windows - ([8a40161](https://github.com/vicanso/zedis/commit/8a401612f60b57ce160785bab776a2d413f28f35))
- *(form)* Show validation error messages - ([13c94d4](https://github.com/vicanso/zedis/commit/13c94d491903961d636e7de11a966bc066f1f528))
- *(keytree)* Highlight multi-select button when active - ([ec5af84](https://github.com/vicanso/zedis/commit/ec5af8400ddec9d46a7ffb9dbb3ce10b02841c3e))
- *(notification)* Handle notifications as global events - ([1ba78c6](https://github.com/vicanso/zedis/commit/1ba78c62c2a5e025379491cb7926c22f63e5bdcc))
- *(proto)* Enhance editor functionality - ([5eb4cbc](https://github.com/vicanso/zedis/commit/5eb4cbc05ab14b9132df67b1da153b5d1426c6de))
- *(redis)* Adjust timeout handling - ([a60a9d6](https://github.com/vicanso/zedis/commit/a60a9d61ffe416aa0187acf9f0e7ec41b105b21b))
- *(server)* Handle server events as global events - ([1550116](https://github.com/vicanso/zedis/commit/1550116ba813e53b385814f68cbe81e33bd77e3f))

### ⚙️ Miscellaneous Tasks

- *(cargo)* Rename package to zedis-gui - ([946fe5a](https://github.com/vicanso/zedis/commit/946fe5a2951c077fb529bc3c2713c6fb32426935))
- *(ci)* Adjust rust cache key - ([1b0757c](https://github.com/vicanso/zedis/commit/1b0757cd790444f4e267875bc54eae33ff573cca))
- *(ci)* Downgrade build runner to ubuntu-22.04 ([#33](https://github.com/orhun/git-cliff/issues/33)) - ([d66483d](https://github.com/vicanso/zedis/commit/d66483d17485b1fc0f45a7add034e5e80dc11b30))
- *(ci)* Downgrade build runner to ubuntu-20.04 ([#33](https://github.com/orhun/git-cliff/issues/33)) - ([c812c92](https://github.com/vicanso/zedis/commit/c812c92dd5f5d80eaf26c34a74d8b3531954b670))
- *(ci)* Downgrade build runner to ubuntu-20.04 ([#33](https://github.com/orhun/git-cliff/issues/33)) - ([b41e22b](https://github.com/vicanso/zedis/commit/b41e22b03ab1456957617fcfa9d79edf9fa9beaa))
- Add debug log - ([db31b34](https://github.com/vicanso/zedis/commit/db31b343bdf008f4dbb04ec1a651c5564ff99007))

## [0.2.1](https://github.com/vicanso/zedis/compare/v0.1.9..v0.2.1) - 2026-01-31

### ⛰️  Features

- *(cli)* Support redis command completion - ([019f38a](https://github.com/vicanso/zedis/commit/019f38a791651f063843e2568e3797dd6fbc8726))
- *(key)* Support batch deletion of keys ([#25](https://github.com/orhun/git-cliff/issues/25)) - ([4c531f2](https://github.com/vicanso/zedis/commit/4c531f2019c3918077658cf23a9b0f3280fe87b3))
- *(search)* Support clearing search history - ([a0dce97](https://github.com/vicanso/zedis/commit/a0dce9790d02b6b088b795dafddaf2aa2d799b9d))
- Support proto parsing - ([c921d82](https://github.com/vicanso/zedis/commit/c921d829492afed3ab1eea5851c77bc90fad34c6))
- Support specifying run mode - ([293b9c9](https://github.com/vicanso/zedis/commit/293b9c9f80fd332c37ae551bdfeb5c4661241e24))

### 🐛 Bug Fixes

- *(cli)* Make command matching case-insensitive - ([3e63989](https://github.com/vicanso/zedis/commit/3e63989472d380024105721f5ec7ff6ba0c5ecff))
- *(config)* Correct default value for proto server - ([2e62259](https://github.com/vicanso/zedis/commit/2e6225968d5860bda94761bbb8fd4335ab2e7cba))
- *(keytree)* Fix read-only mode toggle - ([e51e984](https://github.com/vicanso/zedis/commit/e51e984b5db3474057615400363ad5328bd006b4))
- *(log)* Filter error messages by current server - ([0830f5d](https://github.com/vicanso/zedis/commit/0830f5dd4dcaf173f635bd8679843f75f93817cf))
- *(sentinel)* Fix master name of sentinel mode - ([fcca322](https://github.com/vicanso/zedis/commit/fcca322fc8d90999b38ce253de58d785b0ea12a7))

### 🚜 Refactor

- *(db)* Use separate redb path for development - ([4d29ca7](https://github.com/vicanso/zedis/commit/4d29ca783405dcb3b714df9dbfffb9ed7efabd3c))
- *(editor)* Use code editor for hash value editing - ([e286464](https://github.com/vicanso/zedis/commit/e286464e1efc673d0f4ce8b21c27a3a157c83815))
- *(editor)* Optimize hash editor interface - ([29e88a4](https://github.com/vicanso/zedis/commit/29e88a4bdb408f4d2ee2b5ccac0641d74e8c89f3))
- *(proto)* Improve auto-detection logic - ([791c4ae](https://github.com/vicanso/zedis/commit/791c4aed8e06644add4750647a810be70e64d488))
- *(ui)* Add tooltip for multi-select mode - ([c0e6dfd](https://github.com/vicanso/zedis/commit/c0e6dfd2b618be78011ab93603674ade183e4dea))
- *(ui)* Optimize layout of form action buttons - ([f8f42a3](https://github.com/vicanso/zedis/commit/f8f42a3e17028f8097537e8fe75884d68c9c6752))

### ⚙️ Miscellaneous Tasks

- Version 0.2.0 - ([3ff5390](https://github.com/vicanso/zedis/commit/3ff5390fd5d43a8590e187cbbc2f1a363ef0dac0))
- Notarize and staple app - ([b24ba3d](https://github.com/vicanso/zedis/commit/b24ba3d2437340ed310e4d7f9c7767aaed7f738d))
- Upgrade rust toolchain to 1.93.0 and update deps - ([f9aa1d2](https://github.com/vicanso/zedis/commit/f9aa1d2f75fc423f56d9ecd1153af87af13cb117))

## [0.2.0](https://github.com/vicanso/zedis/compare/v0.1.9..v0.2.0) - 2026-01-31

### ⛰️  Features

- *(cli)* Support redis command completion - ([019f38a](https://github.com/vicanso/zedis/commit/019f38a791651f063843e2568e3797dd6fbc8726))
- *(key)* Support batch deletion of keys ([#25](https://github.com/orhun/git-cliff/issues/25)) - ([4c531f2](https://github.com/vicanso/zedis/commit/4c531f2019c3918077658cf23a9b0f3280fe87b3))
- *(search)* Support clearing search history - ([a0dce97](https://github.com/vicanso/zedis/commit/a0dce9790d02b6b088b795dafddaf2aa2d799b9d))
- Support proto parsing - ([c921d82](https://github.com/vicanso/zedis/commit/c921d829492afed3ab1eea5851c77bc90fad34c6))
- Support specifying run mode - ([293b9c9](https://github.com/vicanso/zedis/commit/293b9c9f80fd332c37ae551bdfeb5c4661241e24))

### 🐛 Bug Fixes

- *(cli)* Make command matching case-insensitive - ([3e63989](https://github.com/vicanso/zedis/commit/3e63989472d380024105721f5ec7ff6ba0c5ecff))
- *(keytree)* Fix read-only mode toggle - ([e51e984](https://github.com/vicanso/zedis/commit/e51e984b5db3474057615400363ad5328bd006b4))
- *(log)* Filter error messages by current server - ([0830f5d](https://github.com/vicanso/zedis/commit/0830f5dd4dcaf173f635bd8679843f75f93817cf))
- *(sentinel)* Fix master name of sentinel mode - ([fcca322](https://github.com/vicanso/zedis/commit/fcca322fc8d90999b38ce253de58d785b0ea12a7))

### 🚜 Refactor

- *(db)* Use separate redb path for development - ([4d29ca7](https://github.com/vicanso/zedis/commit/4d29ca783405dcb3b714df9dbfffb9ed7efabd3c))
- *(editor)* Use code editor for hash value editing - ([e286464](https://github.com/vicanso/zedis/commit/e286464e1efc673d0f4ce8b21c27a3a157c83815))
- *(editor)* Optimize hash editor interface - ([29e88a4](https://github.com/vicanso/zedis/commit/29e88a4bdb408f4d2ee2b5ccac0641d74e8c89f3))
- *(proto)* Improve auto-detection logic - ([791c4ae](https://github.com/vicanso/zedis/commit/791c4aed8e06644add4750647a810be70e64d488))
- *(ui)* Add tooltip for multi-select mode - ([c0e6dfd](https://github.com/vicanso/zedis/commit/c0e6dfd2b618be78011ab93603674ade183e4dea))
- *(ui)* Optimize layout of form action buttons - ([f8f42a3](https://github.com/vicanso/zedis/commit/f8f42a3e17028f8097537e8fe75884d68c9c6752))

### ⚙️ Miscellaneous Tasks

- Notarize and staple app - ([b24ba3d](https://github.com/vicanso/zedis/commit/b24ba3d2437340ed310e4d7f9c7767aaed7f738d))
- Upgrade rust toolchain to 1.93.0 and update deps - ([f9aa1d2](https://github.com/vicanso/zedis/commit/f9aa1d2f75fc423f56d9ecd1153af87af13cb117))

## [0.1.9](https://github.com/vicanso/zedis/compare/v0.1.8..v0.1.9) - 2026-01-23

### ⛰️  Features

- *(config)* Support connection and response timeouts - ([3a7e2c7](https://github.com/vicanso/zedis/commit/3a7e2c7967969227826553444006632cc2223207))
- *(connection)* Periodically prune idle connections - ([52775e9](https://github.com/vicanso/zedis/commit/52775e9104cff9e2c9821c3b3f0d5ffdbd853f1b))
- *(connection)* Support temporarily toggling read-only mode - ([f61930b](https://github.com/vicanso/zedis/commit/f61930bb467635afa2749961345e613f00d8a3ed))
- *(editor)* Support read-only mode - ([95d1ba0](https://github.com/vicanso/zedis/commit/95d1ba0e7237b0ccec41fbe10362f994802454eb))
- *(keytree)* Support search history - ([8ee5460](https://github.com/vicanso/zedis/commit/8ee5460eea5cb38bbd11c3ea2a80ef0d67f88cc0))
- *(keytree)* Disable new button in read-only mode - ([6077a63](https://github.com/vicanso/zedis/commit/6077a6322d75f8faa569107731fb56cdf16ff6fd))
- *(redis)* Use MEMORY USAGE to query value memory ([#21](https://github.com/orhun/git-cliff/issues/21)) - ([81491ee](https://github.com/vicanso/zedis/commit/81491eea711881d4d09cb5698388daa5d9e36573))
- *(ssh)* Support tunnel for cluster ([#17](https://github.com/orhun/git-cliff/issues/17)) - ([6398c92](https://github.com/vicanso/zedis/commit/6398c92d069874e5c3ca4f972c7cb04b4fc33efa))
- *(ssh)* Support tunnel for Standalone and Sentinel ([#17](https://github.com/orhun/git-cliff/issues/17)) - ([c207a94](https://github.com/vicanso/zedis/commit/c207a94e7e2d956fea6e814850040637ff1649a1))
- *(ui)* Add skeleton loading for key tree ([#19](https://github.com/orhun/git-cliff/issues/19)) - ([97a0f83](https://github.com/vicanso/zedis/commit/97a0f83e5dd78c7e4ee9ebf7c6452a632ae3cd59))

### 🐛 Bug Fixes

- *(bytes)* Fix incorrect key memory calculation - ([bccd5fe](https://github.com/vicanso/zedis/commit/bccd5fee57c4c3afa066d66aba034a253b871c02))
- *(config)* Fix global config cache - ([c977931](https://github.com/vicanso/zedis/commit/c977931e15580c96e20bfee57954c2d253924cf8))
- *(ui)* Fix directory tree toggle icon - ([2c8fffb](https://github.com/vicanso/zedis/commit/2c8fffb71eb300cd92fb2eb36c5cfc4b49af940c))
- *(ui)* Shorten text display to fix layout ([#20](https://github.com/orhun/git-cliff/issues/20)) - ([c9765e5](https://github.com/vicanso/zedis/commit/c9765e580567a9b5fab296491e1ab2bc26ccfc3b))
- Fix clippy error - ([e1c4d68](https://github.com/vicanso/zedis/commit/e1c4d683ed3cf1ec37d58abfbca55e5e193a2f9e))

### 🚜 Refactor

- *(client)* Use config hash as cache key - ([c2c8d76](https://github.com/vicanso/zedis/commit/c2c8d76c6c1712c056386f0f18d2054f7141eb21))
- *(config)* Organize redis server config into tabs - ([4a7035b](https://github.com/vicanso/zedis/commit/4a7035b42ed1d10955e527be996adace6cfe827c))
- *(config)* Add global cache for redis server configs - ([4cd4d07](https://github.com/vicanso/zedis/commit/4cd4d074213a49577f299f51f840b6f44424a238))
- *(connection)* Optimize connection reuse - ([e60b5a9](https://github.com/vicanso/zedis/commit/e60b5a99eea0e8deeb29e28aaf6eb8a7ba7166d8))
- *(editor)* Optimize redis-cli shortcuts - ([b499b3f](https://github.com/vicanso/zedis/commit/b499b3f215b81d75dc49d4f813ade6e52e18d7f6))
- *(log)* Enhance startup logs with os, git hash, and version - ([9ed10fd](https://github.com/vicanso/zedis/commit/9ed10fd802a2d8a69a3f69aad31de65b96c2bf23))
- *(ssh)* Optimize connection health check - ([1863e8f](https://github.com/vicanso/zedis/commit/1863e8fd73672e20483a3eac0593ce53cc51525f))
- *(ssh)* Enforce ssh host key checking for tunnels - ([c6a8303](https://github.com/vicanso/zedis/commit/c6a8303361c88a207eec34eb1ee5146eccc7192e))
- *(ssh)* Support `~` in file paths - ([80aef95](https://github.com/vicanso/zedis/commit/80aef95f1f9d83eb85eca11ab88172122dd71997))
- *(table)* Adjust hash table column widths - ([e08a1bc](https://github.com/vicanso/zedis/commit/e08a1bc1d5c682fd2da0574320a95df023832357))

### 📚 Documentation

- *(readme)* Add arch linux installation instructions ([#24](https://github.com/orhun/git-cliff/issues/24)) - ([d021b4e](https://github.com/vicanso/zedis/commit/d021b4e576a9d633fa71a146702a778989da49cf))
- *(readme)* Add windows installation instructions ([#23](https://github.com/orhun/git-cliff/issues/23)) - ([c91715f](https://github.com/vicanso/zedis/commit/c91715f336d69bc5919544fed5b69d593892bf4c))
- Update feature list with ssh and tls support - ([387e924](https://github.com/vicanso/zedis/commit/387e92429771161c984461f40e0e400f73a7da65))

### ⚙️ Miscellaneous Tasks

- Add clippy - ([7e29082](https://github.com/vicanso/zedis/commit/7e29082d5b5345825916d97757184c548b1ab04a))
- Add typeos - ([dbff453](https://github.com/vicanso/zedis/commit/dbff45378a8e890b9d74490447de17b1637d9d81))

## [0.1.8](https://github.com/vicanso/zedis/compare/v0.1.7..v0.1.8) - 2026-01-15

### ⛰️  Features

- *(cli)* Support redis-cli style interactive mode ([#14](https://github.com/orhun/git-cliff/issues/14)) - ([2c36916](https://github.com/vicanso/zedis/commit/2c369163bad518a40c9e57c30525bb1226e16fd3))
- *(connection)* Support insecure tls mode (skip certificate verification) ([#12](https://github.com/orhun/git-cliff/issues/12)) - ([95c25cd](https://github.com/vicanso/zedis/commit/95c25cdcc46aadb5a3c1b22dbd0387ef02ca20d6))
- *(connection)* Implement full tls support (standard tls  & mTls) ([#12](https://github.com/orhun/git-cliff/issues/12)) - ([3cfa099](https://github.com/vicanso/zedis/commit/3cfa099bfb7e2f0e5c0610966523dbb8f19638c1))
- *(json)* Support json truncated format - ([be70525](https://github.com/vicanso/zedis/commit/be705255fee09126fd1e10babb30cdce2adfa83d))
- *(value)* Support lz4 and snappy formats - ([6c9b598](https://github.com/vicanso/zedis/commit/6c9b598a353527346ed3d25a7aee286448062883))

### 🐛 Bug Fixes

- *(filter)* Fix display logic for keyword filtering - ([a56e526](https://github.com/vicanso/zedis/commit/a56e5265edc1e3732b04556f763b7c8a39ad4225))
- *(keytree)* Reset state on database switch - ([24d72b7](https://github.com/vicanso/zedis/commit/24d72b798a7ecee57533ad57431b32f1e59103ab))
- *(ui)* Align dialog button order with os standards - ([cdd90fe](https://github.com/vicanso/zedis/commit/cdd90feb0c9b27d431af8e6306cf4af86ea45a61))

### 🚜 Refactor

- *(editor)* Make hotkeys global within the editor - ([f3a603d](https://github.com/vicanso/zedis/commit/f3a603d4e7c4de84ea90fcfb4463138e3e11ebd8))
- *(editor)* Improve ttl display format - ([c99b7a8](https://github.com/vicanso/zedis/commit/c99b7a8c7dccf083a379b9a28c9cee222e3b7691))
- *(editor)* Optimize type auto-detection for bytes - ([39a68ab](https://github.com/vicanso/zedis/commit/39a68abb942c0ad469d119acbb54fb23f80c5d59))
- *(editor)* Support configuring max length for json string values - ([f7863f4](https://github.com/vicanso/zedis/commit/f7863f4c9792c3cc7601c6160f787a1df3380c0b))

### ⚙️ Miscellaneous Tasks

- *(linux)* Update build script - ([0e2d887](https://github.com/vicanso/zedis/commit/0e2d887a950944e9027125d121525ebe6bfe0f8e))
- *(linux)* Make binary executable - ([b08b791](https://github.com/vicanso/zedis/commit/b08b79106ed7b66dbc7cc528cdb44685946fcb26))
- *(macos)* Build both aarch64 and x86_64 targets - ([bd0d77c](https://github.com/vicanso/zedis/commit/bd0d77c7820a325c1bc76e1ae92d1dba4a0c17dd))

## [0.1.6](https://github.com/vicanso/zedis/compare/v0.1.5..v0.1.6) - 2026-01-10

### ⛰️  Features

- *(connection)* Support tls connection ([#12](https://github.com/orhun/git-cliff/issues/12)) - ([818c64e](https://github.com/vicanso/zedis/commit/818c64ebc89ef0cdd8819b5f43db961d5ebf63fc))
- *(db)* Support database selection - ([896c33e](https://github.com/vicanso/zedis/commit/896c33ede689f6d8f9f77d5f534c256dab8b8f66))

### 🐛 Bug Fixes

- *(linux)* Fix crash when window opens ([#10](https://github.com/orhun/git-cliff/issues/10)) - ([117c23a](https://github.com/vicanso/zedis/commit/117c23aba544b8dc699a87223f958f36083e2dd2))

### 🚜 Refactor

- *(status-bar)* Reset status bar on database switch - ([cc9f11a](https://github.com/vicanso/zedis/commit/cc9f11ad6bbb46281fef42bde53e246c7fd43418))
- *(tree)* Improve select and confirm event handling - ([9c09587](https://github.com/vicanso/zedis/commit/9c095874af9b3cf17b1f338a5540cada2d886e58))

### 📚 Documentation

- Add Homebrew installation guide ([#8](https://github.com/orhun/git-cliff/issues/8)) - ([7af91a5](https://github.com/vicanso/zedis/commit/7af91a55d964a5f9361c4981a0a832707ff2dd13))
- Update readme - ([6c0b20a](https://github.com/vicanso/zedis/commit/6c0b20a6f11f17ee42a707d4bb337debd1e552a5))

### ⚙️ Miscellaneous Tasks

- *(flatpak)* Add initial configuration (untested) - ([521b117](https://github.com/vicanso/zedis/commit/521b117d930b936f7afdc0ef0a51c9072359672f))
- *(linux)* Install appimagetool and update build config - ([095f502](https://github.com/vicanso/zedis/commit/095f5026b6eb1f6fb4c3d5b40ceae165f3aa0fbc))
- *(linux)* Add app image build support - ([ae1aedf](https://github.com/vicanso/zedis/commit/ae1aedfdf2a911df3d0ef9d15a84ced5c155cffa))

## [0.1.6](https://github.com/vicanso/zedis/compare/v0.1.5..v0.1.6) - 2026-01-07

### ⛰️  Features

- *(auth)* Add username support for Redis 6.0+ - ([e8497a0](https://github.com/vicanso/zedis/commit/e8497a06666661bcda585d014dc1acfcccd8845d))
- *(config)* Store max key tree depth - ([1a6a6d7](https://github.com/vicanso/zedis/commit/1a6a6d70526dcc7ffaca90f9f5691c4528e9c2c4))
- *(connection)* Support redis:// connection strings - ([b80019c](https://github.com/vicanso/zedis/commit/b80019c9fc7b2433b7cd03512acc3dcfccbac438))
- *(editor)* Add shortcut to update ttl - ([77b119a](https://github.com/vicanso/zedis/commit/77b119afab7de443e9256402142d37dd623f87b8))
- *(keys)* Add shortcut to create new key - ([470e010](https://github.com/vicanso/zedis/commit/470e01089898267389de24a0ae0fceda4eacf909))
- *(tree)* Support keyboard navigation - ([a3db054](https://github.com/vicanso/zedis/commit/a3db054cfbe4852cdd18e155a4a1bb7128c26797))
- *(tree)* Support custom key separator - ([c4a3d78](https://github.com/vicanso/zedis/commit/c4a3d783d94f1c15683a089c5da2a93f28b8d7e9))
- *(tree)* Support setting max display depth - ([2d63d49](https://github.com/vicanso/zedis/commit/2d63d495dc8154f6cd772b068e7b0869e213cfe6))
- *(ui)* Support global font size setting - ([ef44c6f](https://github.com/vicanso/zedis/commit/ef44c6f27c8fe748961070888e9c37b206fd1937))
- *(ui)* Apply font size setting to key tree, editor, and table - ([a17f56e](https://github.com/vicanso/zedis/commit/a17f56e94f8edc68fa3bb14964ed7fee25ecd20d))
- Support keyboard shortcuts in editor - ([f8616c6](https://github.com/vicanso/zedis/commit/f8616c6c9a4fd61c7a6b46084a6e734cadca45e3))
- Support collapsing all expanded keys - ([63c35e3](https://github.com/vicanso/zedis/commit/63c35e332e5348f30d797df98e883fd53d70267a))

### 🐛 Bug Fixes

- *(tree)* Reset state on connection switch - ([62e4cf9](https://github.com/vicanso/zedis/commit/62e4cf9ab7a19db32ba6094748acc669fd85305d))

### 🚜 Refactor

- *(ui)* Optimize flex layout for resizable panel - ([2f1e560](https://github.com/vicanso/zedis/commit/2f1e560ee77f3e0ced4efcc819cf3eb492dff9ef))
- Limit key tree expansion to 5 levels - ([c689009](https://github.com/vicanso/zedis/commit/c6890095bb87c43dc4d2b3988b8f71ea0765732d))
- Adjust key fill function - ([e7ea850](https://github.com/vicanso/zedis/commit/e7ea85074536ac343561fd17ed8afddcd75a1b69))
- Adjust collapse all key function - ([e99b3cf](https://github.com/vicanso/zedis/commit/e99b3cfa9961ed4ce0b4ae8c27168c0d0b62c018))
- Adjust folder and file order - ([da448e7](https://github.com/vicanso/zedis/commit/da448e72e7d887b4b4bef5d081bbef2c1f104bd7))
- Improve the performance of key tree - ([787f1e3](https://github.com/vicanso/zedis/commit/787f1e39556524e2df5c58ee690b443b434cd697))

### 📚 Documentation

- *(readme)* Clarify that PRs are not currently accepted - ([ba6607b](https://github.com/vicanso/zedis/commit/ba6607bc32fc33a3ecc04c86a5fb53fade03a08b))
- Update readme - ([37ff13d](https://github.com/vicanso/zedis/commit/37ff13dc72970e9b7763fbce4c76e8efff72ab57))

### ⚙️ Miscellaneous Tasks

- *(release)* Adjust app store build - ([b0ab723](https://github.com/vicanso/zedis/commit/b0ab72332d35e1637e9f18a6f6e7fe4de4138970))
- *(windows)* Add application icon - ([8089db8](https://github.com/vicanso/zedis/commit/8089db8399cd3bf5c31b71a474f3b57a067f6cf0))
