#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")"

if ! command -v cargo >/dev/null 2>&1; then
    echo "错误：未找到 cargo。请先安装 Rust 1.85+：https://rustup.rs" >&2
    exit 1
fi

echo "==> 检查运行时依赖"
if ! command -v xsel >/dev/null 2>&1; then
    echo "提示：未找到 xsel，个别应用取词可能失败（可选安装）"
fi
if ! command -v gst-launch-1.0 >/dev/null 2>&1 \
    && ! command -v pw-play >/dev/null 2>&1 \
    && ! command -v paplay >/dev/null 2>&1 \
    && ! command -v mpv >/dev/null 2>&1; then
    echo "提示：未找到音频播放器，发音功能不可用（可选安装 gstreamer1.0-tools）"
fi

echo "==> cargo build --release"
cargo build --release

echo
echo "构建完成："
echo "  target/release/yanxi-cli"
echo "  target/release/yanxi-gui"
echo "运行 ./install.sh 安装到 ~/.local/bin"