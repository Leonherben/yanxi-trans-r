# 言蹊翻译 (Yanxi-Trans-R)

> 版本 **0.5.9**，简化为单一桌面程序 `yanxi`（移除独立 CLI 与 `-gui` 命名后缀），新增「下自成蹊」应用 Logo（覆盖桌面/开始菜单、窗口图标及 Windows 可执行文件图标），并修复 Windows 环境下中文字体字形探测误判的问题。

<div align="center">

<img src="assets/icon.svg" alt="言蹊翻译 Logo" width="96" height="96" />

[![CI](https://github.com/Leonherben/yanxi-trans-r/actions/workflows/ci.yml/badge.svg)](https://github.com/Leonherben/yanxi-trans-r/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Platform](https://img.shields.io/badge/Platform-Linux%20%7C%20Windows-brightgreen.svg)]()
[![Rust Version](https://img.shields.io/badge/Rust-1.85%2B-orange.svg)]()

**言蹊翻译 (Yanxi-Trans-R)** 是一款使用纯 Rust 打造的超轻量、极速跨平台划词翻译悬浮窗工具。  
由原有 Python/PySide6 版本（[yanxi-trans](https://github.com/Leonherben/yanxi-trans.git)）重构演进而来，追求极致的启动响应、超低内存开销以及开箱即用的免配置体验。

</div>

---

## 🌟 核心特性

- ⚡ **极致轻量 & 零依赖**：单文件原生纯 Rust 可执行程序 `yanxi`，无需 Python 环境及 Qt/PySide6 依赖库。
- 🪟 **原生桌面极简悬浮弹窗**：基于 `eframe`/`egui` 打造的极简浅色悬浮窗，默认「按需划词」模式（平时划选静默不打扰，`Alt+Q` 唤醒后划选连续翻译，点空白处自动收起），同时支持「划选翻译」与「手动模式」。
- 📌 **图钉置顶与顺滑拖拽**：未 Pin 时为普通窗口层级，点击外部或失焦自动收起；开启 Pin 后真正置顶（`AlwaysOnTop`）并固定位置与尺寸，重启自动恢复。
- 🎯 **零轮询全局取词**：Linux 基于 X11 XFixes 事件驱动；Windows 支持 UI Automation 优先取词与保护性剪贴板回退。
- 🌐 **微软官方免 Key 极速通道 + 兼容大模型**：开箱即用走微软网页通道（支持音标与自动语种识别），并在设置页内支持 DeepSeek、OpenAI、智谱 GLM、本地 Ollama 等任意 OpenAI 兼容接口。
- 📄 **学术双栏 PDF 跨行断词自动修复**：自动合并双栏论文复制产生的连字符跨行断词（如 `convo-\n lutional` → `convolutional`）。
- ⚡ **毫秒级 SQLite 本地缓存**：命中翻译 ~0ms 响应，表结构与原版完全兼容。

---

## 🚀 快速上手

### 1. 环境依赖（Linux X11）

运行时可选依赖：

- `xsel`：部分应用不按 ICCCM 返回选区时的回退取词（可选）
- GStreamer（`gst-launch-1.0`）、PipeWire（`pw-play`）、PulseAudio（`paplay`）、`mpv` 任一即可用于发音
- 中文字体：Noto Sans CJK、文泉驿、HarmonyOS Sans 等任意系统中文字体

构建依赖（Debian/Ubuntu）：

```bash
sudo apt install build-essential pkg-config libx11-dev libxkbcommon-dev \
  libgl1-mesa-dev xsel gstreamer1.0-tools fonts-noto-cjk
```

Fedora：

```bash
sudo dnf install gcc make pkg-config libX11-devel libxkbcommon-devel \
  mesa-libGL-devel xsel gstreamer1-plugins-base google-noto-sans-cjk-fonts
```

### 2. 构建与安装

```bash
git clone https://github.com/Leonherben/yanxi-trans-r.git
cd yanxi-trans-r
bash build.sh      # 检查依赖并 cargo build --release
bash install.sh    # 安装 ~/.local/bin/yanxi、应用图标与桌面快捷方式
```

安装后可直接从开始菜单点击「言蹊翻译」或运行 `yanxi` 启动，按 `Alt + Q` 呼出/收起悬浮窗。首次使用默认走微软免 Key 通道；如需切换大模型，点击右上角 `⚙` 进入设置页填写 API Key。

---

## ⚙️ 配置文件与存储位置

言蹊遵循现代桌面标准规范存储配置与缓存数据：

- **配置文件**: `~/.config/yanxi/config.json`
- **缓存数据库**: `~/.local/share/yanxi/cache.db` (或 `~/.config/yanxi/cache.db`)

配置示例：
```json
{
  "active_provider": "microsoft",
  "target_lang": "zh-CN",
  "providers": {
    "microsoft": {
      "name": "microsoft",
      "provider_type": "microsoft",
      "base_url": "https://cn.bing.com",
      "api_key": "",
      "model": "",
      "timeout_seconds": 10.0,
      "system_prompt": ""
    },
    "deepseek": {
      "name": "deepseek",
      "provider_type": "openai_compatible",
      "base_url": "https://api.deepseek.com/v1",
      "api_key": "sk-...",
      "model": "deepseek-chat",
      "timeout_seconds": 15.0,
      "system_prompt": "You are a professional translator. Translate naturally and concisely."
    }
  }
}
```

---

## 📄 开源许可证

本项目基于 [MIT License](LICENSE) 开源发布。
