#!/bin/bash
set -e

# 定义下载地址
GENERATOR_URL="https://raw.githubusercontent.com/flatpak/flatpak-builder-tools/master/cargo/flatpak-cargo-generator.py"
OUTPUT_FILE="build-aux/flatpak/cargo-sources.json"

echo "Downloading generator..."
# 下载脚本到临时文件
curl -sfL "$GENERATOR_URL" -o /tmp/flatpak-cargo-generator.py

echo "Generating cargo sources..."

# 1. 创建一个名为 .venv 的虚拟环境
python3 -m venv .venv

# 2. 激活环境 (激活后你的命令行前面会出现 (.venv) 字样)
source .venv/bin/activate

# 3. 在这个隔离环境里安装依赖 (不会报错了)
pip install aiohttp toml tomlkit

# 运行生成器，指向 Cargo.lock，输出到指定位置
python3 /tmp/flatpak-cargo-generator.py Cargo.lock -o "$OUTPUT_FILE"

deactivate
rm -rf .venv

echo "Done! Generated $OUTPUT_FILE"