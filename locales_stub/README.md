<!--
Intentionally empty locale directory.

`rust_i18n::i18n!` is pointed here (instead of `../locales`) so it embeds NO
translations at compile time — that codegen would otherwise be ~600KiB of
`_RUST_I18N_BACKEND` map-insertion instructions. The real `locales/*.toml` are
embedded (compressed) via rust-embed and parsed at runtime; see
`src/i18n_loader.rs`.

The `i18n!` macro globs `**/*.{yml,yaml,json,toml}` here and finds nothing, so
this `.md` keeper file is ignored. Do not add translation files to this folder —
edit `../locales/*.toml` instead.
-->
