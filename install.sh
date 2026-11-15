#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")"

PREFIX="${PREFIX:-$HOME/.local}"
BIN_DIR="$PREFIX/bin"
APPS_DIR="$HOME/.local/share/applications"

if [[ ! -x target/release/yanxi-cli || ! -x target/release/yanxi-gui ]]; then
    echo "错误：未找到构建产物，请先运行 ./build.sh" >&2
    exit 1
fi

mkdir -p "$BIN_DIR" "$APPS_DIR"
install -m 755 target/release/yanxi-cli "$BIN_DIR/yanxi-cli"
install -m 755 target/release/yanxi-gui "$BIN_DIR/yanxi-gui"

sed "s|@BIN@|$BIN_DIR/yanxi-gui|g" yanxi-trans.desktop > "$APPS_DIR/yanxi-trans.desktop"
chmod 644 "$APPS_DIR/yanxi-trans.desktop"

echo "已安装："
echo "  $BIN_DIR/yanxi-cli"
echo "  $BIN_DIR/yanxi-gui"
echo "  $APPS_DIR/yanxi-trans.desktop"
echo
echo "请确保 $BIN_DIR 在 PATH 中。运行 yanxi-gui，按 Alt+Q 呼出/收起浮窗。"