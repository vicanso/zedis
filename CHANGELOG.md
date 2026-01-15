# Changelog

## [0.1.8](https://github.com/vicanso/pingap/compare/v0.1.7..v0.1.8) - 2026-01-15

### ⛰️  Features

- *(cli)* Support redis-cli style interactive mode ([#14](https://github.com/orhun/git-cliff/issues/14)) - ([2c36916](https://github.com/vicanso/pingap/commit/2c369163bad518a40c9e57c30525bb1226e16fd3))
- *(connection)* Support insecure tls mode (skip certificate verification) ([#12](https://github.com/orhun/git-cliff/issues/12)) - ([95c25cd](https://github.com/vicanso/pingap/commit/95c25cdcc46aadb5a3c1b22dbd0387ef02ca20d6))
- *(connection)* Implement full tls support (standard tls  & mTls) ([#12](https://github.com/orhun/git-cliff/issues/12)) - ([3cfa099](https://github.com/vicanso/pingap/commit/3cfa099bfb7e2f0e5c0610966523dbb8f19638c1))
- *(json)* Support json truncated format - ([be70525](https://github.com/vicanso/pingap/commit/be705255fee09126fd1e10babb30cdce2adfa83d))
- *(value)* Support lz4 and snappy formats - ([6c9b598](https://github.com/vicanso/pingap/commit/6c9b598a353527346ed3d25a7aee286448062883))

### 🐛 Bug Fixes

- *(filter)* Fix display logic for keyword filtering - ([a56e526](https://github.com/vicanso/pingap/commit/a56e5265edc1e3732b04556f763b7c8a39ad4225))
- *(keytree)* Reset state on database switch - ([24d72b7](https://github.com/vicanso/pingap/commit/24d72b798a7ecee57533ad57431b32f1e59103ab))
- *(ui)* Align dialog button order with os standards - ([cdd90fe](https://github.com/vicanso/pingap/commit/cdd90feb0c9b27d431af8e6306cf4af86ea45a61))

### 🚜 Refactor

- *(editor)* Make hotkeys global within the editor - ([f3a603d](https://github.com/vicanso/pingap/commit/f3a603d4e7c4de84ea90fcfb4463138e3e11ebd8))
- *(editor)* Improve ttl display format - ([c99b7a8](https://github.com/vicanso/pingap/commit/c99b7a8c7dccf083a379b9a28c9cee222e3b7691))
- *(editor)* Optimize type auto-detection for bytes - ([39a68ab](https://github.com/vicanso/pingap/commit/39a68abb942c0ad469d119acbb54fb23f80c5d59))
- *(editor)* Support configuring max length for json string values - ([f7863f4](https://github.com/vicanso/pingap/commit/f7863f4c9792c3cc7601c6160f787a1df3380c0b))

### ⚙️ Miscellaneous Tasks

- *(linux)* Update build script - ([0e2d887](https://github.com/vicanso/pingap/commit/0e2d887a950944e9027125d121525ebe6bfe0f8e))
- *(linux)* Make binary executable - ([b08b791](https://github.com/vicanso/pingap/commit/b08b79106ed7b66dbc7cc528cdb44685946fcb26))
- *(macos)* Build both aarch64 and x86_64 targets - ([bd0d77c](https://github.com/vicanso/pingap/commit/bd0d77c7820a325c1bc76e1ae92d1dba4a0c17dd))

## [0.1.6](https://github.com/vicanso/pingap/compare/v0.1.5..v0.1.6) - 2026-01-10

### ⛰️  Features

- *(connection)* Support tls connection ([#12](https://github.com/orhun/git-cliff/issues/12)) - ([818c64e](https://github.com/vicanso/pingap/commit/818c64ebc89ef0cdd8819b5f43db961d5ebf63fc))
- *(db)* Support database selection - ([896c33e](https://github.com/vicanso/pingap/commit/896c33ede689f6d8f9f77d5f534c256dab8b8f66))

### 🐛 Bug Fixes

- *(linux)* Fix crash when window opens ([#10](https://github.com/orhun/git-cliff/issues/10)) - ([117c23a](https://github.com/vicanso/pingap/commit/117c23aba544b8dc699a87223f958f36083e2dd2))

### 🚜 Refactor

- *(status-bar)* Reset status bar on database switch - ([cc9f11a](https://github.com/vicanso/pingap/commit/cc9f11ad6bbb46281fef42bde53e246c7fd43418))
- *(tree)* Improve select and confirm event handling - ([9c09587](https://github.com/vicanso/pingap/commit/9c095874af9b3cf17b1f338a5540cada2d886e58))

### 📚 Documentation

- Add Homebrew installation guide ([#8](https://github.com/orhun/git-cliff/issues/8)) - ([7af91a5](https://github.com/vicanso/pingap/commit/7af91a55d964a5f9361c4981a0a832707ff2dd13))
- Update readme - ([6c0b20a](https://github.com/vicanso/pingap/commit/6c0b20a6f11f17ee42a707d4bb337debd1e552a5))

### ⚙️ Miscellaneous Tasks

- *(flatpak)* Add initial configuration (untested) - ([521b117](https://github.com/vicanso/pingap/commit/521b117d930b936f7afdc0ef0a51c9072359672f))
- *(linux)* Install appimagetool and update build config - ([095f502](https://github.com/vicanso/pingap/commit/095f5026b6eb1f6fb4c3d5b40ceae165f3aa0fbc))
- *(linux)* Add app image build support - ([ae1aedf](https://github.com/vicanso/pingap/commit/ae1aedfdf2a911df3d0ef9d15a84ced5c155cffa))

## [0.1.6](https://github.com/vicanso/pingap/compare/v0.1.5..v0.1.6) - 2026-01-07

### ⛰️  Features

- *(auth)* Add username support for Redis 6.0+ - ([e8497a0](https://github.com/vicanso/pingap/commit/e8497a06666661bcda585d014dc1acfcccd8845d))
- *(config)* Store max key tree depth - ([1a6a6d7](https://github.com/vicanso/pingap/commit/1a6a6d70526dcc7ffaca90f9f5691c4528e9c2c4))
- *(connection)* Support redis:// connection strings - ([b80019c](https://github.com/vicanso/pingap/commit/b80019c9fc7b2433b7cd03512acc3dcfccbac438))
- *(editor)* Add shortcut to update ttl - ([77b119a](https://github.com/vicanso/pingap/commit/77b119afab7de443e9256402142d37dd623f87b8))
- *(keys)* Add shortcut to create new key - ([470e010](https://github.com/vicanso/pingap/commit/470e01089898267389de24a0ae0fceda4eacf909))
- *(tree)* Support keyboard navigation - ([a3db054](https://github.com/vicanso/pingap/commit/a3db054cfbe4852cdd18e155a4a1bb7128c26797))
- *(tree)* Support custom key separator - ([c4a3d78](https://github.com/vicanso/pingap/commit/c4a3d783d94f1c15683a089c5da2a93f28b8d7e9))
- *(tree)* Support setting max display depth - ([2d63d49](https://github.com/vicanso/pingap/commit/2d63d495dc8154f6cd772b068e7b0869e213cfe6))
- *(ui)* Support global font size setting - ([ef44c6f](https://github.com/vicanso/pingap/commit/ef44c6f27c8fe748961070888e9c37b206fd1937))
- *(ui)* Apply font size setting to key tree, editor, and table - ([a17f56e](https://github.com/vicanso/pingap/commit/a17f56e94f8edc68fa3bb14964ed7fee25ecd20d))
- Support keyboard shortcuts in editor - ([f8616c6](https://github.com/vicanso/pingap/commit/f8616c6c9a4fd61c7a6b46084a6e734cadca45e3))
- Support collapsing all expanded keys - ([63c35e3](https://github.com/vicanso/pingap/commit/63c35e332e5348f30d797df98e883fd53d70267a))

### 🐛 Bug Fixes

- *(tree)* Reset state on connection switch - ([62e4cf9](https://github.com/vicanso/pingap/commit/62e4cf9ab7a19db32ba6094748acc669fd85305d))

### 🚜 Refactor

- *(ui)* Optimize flex layout for resizable panel - ([2f1e560](https://github.com/vicanso/pingap/commit/2f1e560ee77f3e0ced4efcc819cf3eb492dff9ef))
- Limit key tree expansion to 5 levels - ([c689009](https://github.com/vicanso/pingap/commit/c6890095bb87c43dc4d2b3988b8f71ea0765732d))
- Adjust key fill function - ([e7ea850](https://github.com/vicanso/pingap/commit/e7ea85074536ac343561fd17ed8afddcd75a1b69))
- Adjust collapse all key function - ([e99b3cf](https://github.com/vicanso/pingap/commit/e99b3cfa9961ed4ce0b4ae8c27168c0d0b62c018))
- Adjust folder and file order - ([da448e7](https://github.com/vicanso/pingap/commit/da448e72e7d887b4b4bef5d081bbef2c1f104bd7))
- Improve the performance of key tree - ([787f1e3](https://github.com/vicanso/pingap/commit/787f1e39556524e2df5c58ee690b443b434cd697))

### 📚 Documentation

- *(readme)* Clarify that PRs are not currently accepted - ([ba6607b](https://github.com/vicanso/pingap/commit/ba6607bc32fc33a3ecc04c86a5fb53fade03a08b))
- Update readme - ([37ff13d](https://github.com/vicanso/pingap/commit/37ff13dc72970e9b7763fbce4c76e8efff72ab57))

### ⚙️ Miscellaneous Tasks

- *(release)* Adjust app store build - ([b0ab723](https://github.com/vicanso/pingap/commit/b0ab72332d35e1637e9f18a6f6e7fe4de4138970))
- *(windows)* Add application icon - ([8089db8](https://github.com/vicanso/pingap/commit/8089db8399cd3bf5c31b71a474f3b57a067f6cf0))

