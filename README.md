# 言蹊翻译 (Yanxi-Trans-R)

<div align="center">

[![CI](https://github.com/Leonherben/yanxi-trans-r/actions/workflows/ci.yml/badge.svg)](https://github.com/Leonherben/yanxi-trans-r/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Platform](https://img.shields.io/badge/Platform-Linux%20%7C%20Windows-brightgreen.svg)]()
[![Rust Version](https://img.shields.io/badge/Rust-1.75%2B-orange.svg)]()

**言蹊翻译 (Yanxi-Trans-R)** 是一款使用纯 Rust 打造的超轻量、极速跨平台划词翻译引擎与终端生产力工具。  
由原有 Python/PySide6 版本（[yanxi-trans](https://github.com/Leonherben/yanxi-trans.git)）重构演进而来，追求极致的启动响应、超低内存开销以及开箱即用的免配置体验。

</div>

---

## 🌟 核心特性

- ⚡ **极致轻量 & 零依赖**：单文件原生纯 Rust 可执行程序，无需 Python 环境及沉重的 Qt/PySide6 依赖库，相较于原版内存下降 **80%+**。
- 🪟 **原生桌面极简悬浮弹窗 (`yanxi-gui`)**：基于 `eframe`/`egui` 打造的高性能毛玻璃深色悬浮窗，无焦点窃取打扰，智能跟随光标定位并具备屏幕边缘防溢出。
- 🎯 **X11 XFixes 全局无感划词监听**：彻底摒弃传统 24 小时低级鼠标钩子轮询，日常 **完全零 CPU 占用**，在任意应用中选词高亮即时触发。
- 🌐 **微软官方免 Key 极速通道**：内置高拟真 Bing/Edge 网页端自适应动态会话签名机制，无需申请任何 API Key 即可无限免费体验极速翻译（支持注音/拼音与语种自动识别）。
- 🤖 **兼容大模型生态**：原生支持 OpenAI、DeepSeek、Moonshot 等任何兼容 `/chat/completions` 协议的大模型，支持自定义 System Prompt。
- 📄 **学术双栏 PDF 跨行断词自动修复**：智能识别并修复论文双栏排版中因分行产生的带连字符截断词（例如 `convo-\n lutional` 自动还原为 `convolutional`）。
- ⚡ **毫秒级 SQLite 本地持久化缓存**：零开销本地哈希缓存，命中翻译直接秒开（~0ms 响应），且 **100% 兼容** Python 原版缓存数据库规范，无缝迁移。
- 🔄 **管道与终端交互 REPL**：支持标准输入管道（例如 `echo "text" | yanxi-cli` 或配合 `xclip`）、单次翻译与连续交互式命令行模式。

---

## 🚀 快速上手

### 1. 源码编译与安装

确保系统已安装 Rust 工具链（1.75+）：

```bash
git clone https://github.com/Leonherben/yanxi-trans-r.git
cd yanxi-trans-r
cargo build --release

# 安装 CLI 与 GUI 工具至用户本地 bin
install -Dm755 target/release/yanxi-cli ~/.local/bin/yanxi-cli
install -Dm755 target/release/yanxi-gui ~/.local/bin/yanxi-gui
```

### 2. 基本使用

#### 直接翻译
```bash
# 翻译英文词组
yanxi-cli "convolutional neural network"

# 自动处理学术 PDF 双栏复制产生的跨行断词
yanxi-cli "convo-
  lutional neural network"
```

#### 管道输入 (配合剪贴板 / 脚本)
```bash
# 管道输入
echo "transfer learning" | yanxi-cli

# Linux X11 配合 xclip 快速翻译选中内容
xclip -o | yanxi-cli
```

#### 连通性测试
```bash
yanxi-cli --test
```

#### 查看与管理提供商
```bash
# 查看所有已配置的服务提供商
yanxi-cli --list-providers

# 设置 DeepSeek API Key
yanxi-cli --set-key deepseek sk-xxxxxxxxxxxxxxxxxxxx

# 临时切换使用 DeepSeek 翻译
yanxi-cli -p deepseek "attention is all you need"

# 设置默认提供商
yanxi-cli --set-provider deepseek
```

#### 启动桌面悬浮弹窗 (GUI 模式)
```bash
# 启动桌面常驻无感划词悬浮窗 (划选文字自动弹出并翻译)
yanxi-gui

# 或通过 CLI 启动 GUI 模式
yanxi-cli -g
```

#### 桌面终端划词监听模式 (CLI Watch)
```bash
# 在终端中实时监控划词并输出
yanxi-cli -w
```

#### 终端交互 REPL 模式
```bash
yanxi-cli -i
```

#### 清空或绕过本地缓存
```bash
# 强制跳过缓存直接调用接口
yanxi-cli --no-cache "transformer architecture"

# 清空本地缓存数据库
yanxi-cli --clear-cache
```

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

## 🗺️ 路线图 (Roadmap)

- [x] **Phase 1: 核心引擎与 CLI**
  - [x] 微软 Edge 免 Key 翻译与 Azure 官方专线双模支持
  - [x] OpenAI 兼容通用大模型 API 支持
  - [x] SQLite 本地缓存引擎与 Python 版双向兼容
  - [x] 学术论文 PDF 跨行断词自动修复
  - [x] 现代化 CLI 终端工具 (`yanxi-cli`)
- [x] **Phase 2: 桌面全局划词监听与选区捕获**
  - [x] 基于 X11 XFixes 协议的异步选区变动监听 (零轮询、零 CPU 占用)
  - [x] 鼠标指针屏幕绝对坐标 `(x, y)` 毫秒级精准捕获
  - [x] 左键连续拖拽物理消抖与重复选词抑制机制
  - [x] Windows 跨平台选区抽象层
  - [x] CLI 划词监听守护运行模式 (`yanxi-cli -w`)
- [x] **Phase 3: 超轻量悬浮翻译弹窗 (当前版本)**
  - [x] 基于 `eframe`/`egui` 打造的高性能无焦点窃取悬浮窗 (`yanxi-gui`)
  - [x] 光标跟随动态定位与屏幕边缘防溢出翻转
  - [x] 多平台中文字体自适应探测与渲染（优先加载系统 HarmonyOS Sans / Noto Sans CJK / 微软雅黑 / 苹方，完美杜绝豆腐方块乱码）
  - [x] 暗黑毛玻璃卡片拟态、拼音注音、一键复制与图钉固定 (📌)
  - [x] 内存常驻大幅降低至 ~20MB，运行响应如丝顺滑

---

## 📄 开源许可证

本项目基于 [MIT License](LICENSE) 开源发布。
