#!/bin/bash
set -e

# 由于gpui使用了macos的未公开函数，因此需要单独的脚本来构建App Store版本

# ================= 配置 =================
FILE="Cargo.toml"

# 二进制文件的路径
# 使用 $HOME 替代 ~ 以确保在脚本中稳定展开，并处理路径空格
BINARY_PATH="$HOME/cargo-target/release/bundle/osx/Zedis.app/Contents/MacOS/Zedis"

# Patch 配置
PATCH_BLOCK='
[patch."https://github.com/zed-industries/zed"]
gpui = { path = "../zed/crates/gpui" }
[patch.crates-io]
gpui = { path = "../zed/crates/gpui" }
'
# =======================================

echo "🚀 准备修改 Cargo.toml..."

# 1. 备份机制
cleanup() {
    if [ -f "${FILE}.bak" ]; then
        mv "${FILE}.bak" "$FILE"
        echo "✅ Cargo.toml 已恢复原状"
    fi
}
trap cleanup EXIT

cp "$FILE" "${FILE}.bak"

# 2. 追加 Patch
echo "👉 步骤 1: 追加 Patch 配置..."
printf "%s\n" "$PATCH_BLOCK" >> "$FILE"

# 3. 执行构建
echo "🏗️ 开始构建..."
make bundle

# =======================================
# 4. 新增：私有 API 检查步骤
# =======================================
echo "🔍 步骤 2: 检查二进制文件符号表..."

# 先判断文件是否存在，防止误报
if [ ! -f "$BINARY_PATH" ]; then
    echo "❌ 错误：找不到构建好的二进制文件："
    echo "   $BINARY_PATH"
    exit 1
fi

# 使用 nm -g 查看外部符号，通过 grep 查找私有 API
# grep -q 表示静默模式，如果找到匹配项则返回 0 (True)，否则返回 1 (False)
if nm -g "$BINARY_PATH" | grep -q "_CGSSetWindowBackgroundBlurRadius"; then
    echo "❌ 失败：检测到非法私有 API 符号！"
    echo "   Symbol: _CGSSetWindowBackgroundBlurRadius"
    echo "   原因：包含此符号会导致 App Store 审核被拒或系统兼容性问题。"
    echo "   位置：$BINARY_PATH"
    
    # 打印出具体的那一行给用户看
    echo "   详细信息："
    nm -g "$BINARY_PATH" | grep "_CGSSetWindowBackgroundBlurRadius"
    
    exit 1 # 主动报错退出
else
    echo "✅ 检查通过：二进制文件未包含私有 WindowBlur API。"
fi

# 脚本结束，自动触发 cleanup
echo "🎉 所有任务完成！"