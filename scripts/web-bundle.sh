#!/usr/bin/env bash
# The browser bundle (ADR 9): the kit's icons copied beside the page, then the
# wasm built by wasm-pack into zedis-web/www/wasm — the directory the bridge
# serves with `make web-serve`.
#
#   scripts/web-bundle.sh            the iteration build: `--profile web`, the
#                                    name section kept for readable panics, no
#                                    wasm-opt
#   scripts/web-bundle.sh --release  the shipped form, `make release`'s
#                                    counterpart: `--profile web-release` (fat
#                                    LTO, one codegen unit, stripped), then
#                                    `wasm-opt -Oz`
#
# Runs from zedis-web/ so rustup reads the `rust-toolchain.toml` there and
# selects the nightly the browser backend needs.
set -euo pipefail
cd "$(dirname "$0")/.."

mode=dev
case "${1:-}" in
  "") ;;
  --release) mode=release ;;
  *)
    echo "usage: $0 [--release]" >&2
    exit 2
    ;;
esac

# Checked before the build, not after it: a release bundle without the
# optimizer is a different artifact, and finding that out after a fat-LTO
# build is the expensive way.
if [ "$mode" = release ]; then
  if ! command -v wasm-opt >/dev/null 2>&1; then
    echo "wasm-opt is required for --release: brew install binaryen, or cargo install wasm-opt" >&2
    exit 1
  fi
  if ! command -v brotli >/dev/null 2>&1; then
    echo "brotli is required for --release: brew install brotli (apt install brotli)" >&2
    exit 1
  fi
fi

# The kit's icons are fetched by the page (`/assets/icons/<name>.svg`), not
# embedded, so they are copied from wherever cargo has the crate.
kit_manifest=$(cargo metadata --format-version 1 2>/dev/null \
  | python3 -c 'import json,sys; m=json.load(sys.stdin); print(next(p["manifest_path"] for p in m["packages"] if p["name"]=="gpui-kit-assets"))')
kit_dir=$(dirname "$kit_manifest")
mkdir -p zedis-web/www/assets/icons
cp "$kit_dir"/assets/icons/*.svg zedis-web/www/assets/icons/
echo "icons: $(ls zedis-web/www/assets/icons | wc -l | tr -d ' ') from $kit_dir"
# The tab icon. Declared by the page, because a page that names none makes
# the browser ask for `/favicon.ico` — the site root, which under a base path
# (`--base-path`) is another application's.
cp assets/icon.png zedis-web/www/assets/icon.png
# Locale files stay out of the wasm (no zstd inflater there) and are fetched
# by the page / a language switch. Same origin as the kit icons.
mkdir -p zedis-web/www/locales
cp locales/*.toml zedis-web/www/locales/
echo "locales: $(ls zedis-web/www/locales | wc -l | tr -d ' ')"
# Mono fonts, command metadata and CONFIG help: fetched after the first
# frame so they do not sit uncompressed in the wasm.
cp assets/fonts/JetBrainsMono-*.ttf zedis-web/www/fonts/
mkdir -p zedis-web/www/assets/config_docs
cp assets/commands.json zedis-web/www/assets/commands.json
cp assets/config_docs/*.json zedis-web/www/assets/config_docs/

profile=web
if [ "$mode" = release ]; then
  profile=web-release
fi
(
  cd zedis-web
  # `--no-opt` in both modes: the release form runs wasm-opt itself below,
  # with its flags in view, rather than through wasm-pack's defaults.
  wasm-pack build . --target web --profile "$profile" --no-opt --out-dir www/wasm --out-name zedis_web
)

wasm=zedis-web/www/wasm/zedis_web_bg.wasm
size() { wc -c < "$1" | tr -d ' '; }
mib() { awk -v b="$1" 'BEGIN { printf "%.1f MiB", b / 1048576 }'; }

if [ "$mode" = release ]; then
  before=$(size "$wasm")
  # The features wasm-opt may assume, spelled out: cargo's `strip = true`
  # removes every custom section, the module's own `target_features` list
  # included, so wasm-opt's default detection sees an MVP module and refuses
  # the atomics it then meets. The first six are rustc's baseline for
  # wasm32-unknown-unknown (Rust 1.82+). `threads` is there because
  # gpui-pre-web's default `multithreaded` feature compiles atomic
  # instructions into the module even though this app runs
  # `single_threaded_web()`; the flag lets them validate, and the memory
  # stays unshared. Measured on this module (ADR 9), -Oz takes ~11% off the
  # raw size and puts ~7% onto the compressed one; the bridge serves raw
  # bytes, and raw is what the browser parses and holds.
  features=(
    --enable-bulk-memory
    --enable-mutable-globals
    --enable-sign-ext
    --enable-nontrapping-float-to-int
    --enable-multivalue
    --enable-reference-types
    --enable-threads
  )
  # --strip-debug drops whatever name/DWARF section survived cargo's strip;
  # --strip-producers the toolchain-version section, which no engine reads.
  wasm-opt -Oz --strip-debug --strip-producers "${features[@]}" -o "$wasm.opt" "$wasm"
  mv "$wasm.opt" "$wasm"
  echo "wasm-opt -Oz: $(mib "$before") -> $(mib "$(size "$wasm")")"
fi

# The bridge never sends the module raw: it stores and serves `.gz` (every
# browser accepts gzip) and, from a release bundle, `.br` — a quarter of the
# bytes. Anything left over from an earlier build goes first, because the
# bridge prefers `.br`, and a stale one would win over a fresh `.gz`.
js=zedis-web/www/wasm/zedis_web.js
rm -f "$wasm.gz" "$wasm.br" "$js.gz" "$js.br"
if [ "$mode" = release ]; then
  for file in "$wasm" "$js"; do
    gzip -9 -k "$file"
    brotli -q 11 -k "$file"
  done
  echo "wasm ($mode): $(mib "$(size "$wasm")") raw -> $(mib "$(size "$wasm.gz")") gzip, $(mib "$(size "$wasm.br")") brotli"
else
  # Fast, not small: this is rebuilt on every iteration and read over loopback.
  gzip -1 -k "$wasm"
  echo "wasm ($mode): $(mib "$(size "$wasm")") raw -> $(mib "$(size "$wasm.gz")") gzip -1"
fi

# Content hashes inlined into `index.html` so the page does not need a
# second request for `asset-rev.json`. The marker, the hashing rules and the
# git clean filter that keeps the built map out of commits all live in one
# script — two copies of that regex is how the builder and the filter would
# come to disagree about what they are both editing.
python3 scripts/asset-rev.py write
