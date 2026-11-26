#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")"

PREFIX="${PREFIX:-$HOME/.local}"
BIN_DIR="$PREFIX/bin"
APPS_DIR="$HOME/.local/share/applications"
ICON_PNG_DIR="$HOME/.local/share/icons/hicolor/256x256/apps"
ICON_SVG_DIR="$HOME/.local/share/icons/hicolor/scalable/apps"

if [[ ! -x target/release/yanxi ]]; then
    echo "错误：未找到构建产物，请先运行 ./build.sh" >&2
    exit 1
fi

mkdir -p "$BIN_DIR" "$APPS_DIR" "$ICON_PNG_DIR" "$ICON_SVG_DIR"
install -m 755 target/release/yanxi "$BIN_DIR/yanxi"
install -m 644 assets/icon.png "$ICON_PNG_DIR/yanxi.png"
install -m 644 assets/icon.svg "$ICON_SVG_DIR/yanxi.svg"

sed "s|@BIN@|$BIN_DIR/yanxi|g" yanxi-trans.desktop > "$APPS_DIR/yanxi-trans.desktop"
chmod 644 "$APPS_DIR/yanxi-trans.desktop"

if command -v gtk-update-icon-cache >/dev/null 2>&1; then
    gtk-update-icon-cache -f -t "$HOME/.local/share/icons/hicolor" >/dev/null 2>&1 || true
fi
if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database "$APPS_DIR" >/dev/null 2>&1 || true
fi

echo "已安装："
echo "  $BIN_DIR/yanxi"
echo "  $ICON_PNG_DIR/yanxi.png"
echo "  $ICON_SVG_DIR/yanxi.svg"
echo "  $APPS_DIR/yanxi-trans.desktop"
echo
echo "请确保 $BIN_DIR 在 PATH 中。运行 yanxi 或从开始菜单打开「言蹊翻译」，按 Alt+Q 呼出/收起浮窗。"
