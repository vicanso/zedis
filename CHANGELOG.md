# Changelog

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

