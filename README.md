[中文](README.md) | [English](README.en.md)

# ashell

![Preview](preview.png)

`ashell` 是一款现代化的、基于 GPUI Component 构建的 Rust 桌面终端客户端。

该项目旨在通过结合本地和远程环境，并内置丰富的核心功能，提供一个高性能且美观的 Shell 工作区。

## 🚀 v0.4 版本重要升级

v0.4 在 v0.3 打下的基础上，重点带来了更完整的工作区操作能力和更顺手的日常体验：
- ✨ **键位管理功能**：支持在设置中可视化查看和修改常用快捷键，并提供冲突提示。
- ✨ **设置页优化**：设置页的布局和交互更加紧凑顺手，整体信息层级更清晰。
- ✨ **Tab 内多 Pane 与类 tmux 体验**：一个 tab 内现在可以管理多个 pane，支持分屏、聚焦和切换，提供更接近 tmux 的工作流。
- ✨ **传输历史增强**：传输历史面板展示更完整的任务信息，便于回溯上传和下载过程。
- ✨ **SSH passphrase 支持**：现在可以为私钥填写 passphrase，并在连接时自动使用保存的口令。
- ✨ **终端显示优化**：终端渲染进一步增强，支持 Block Elements 等自定义图形字符的更完整显示。

## 下载

您可以从 [GitHub Releases 页面](https://github.com/rust-kotlin/ashell/releases/latest) 下载最新安装包：

- macOS：x64 和 arm64 的 `.dmg`，同时提供便携 `.zip`。
- Windows：x64、x86 和 arm64 的 `Setup.exe`，同时提供便携 `.zip`。
- Linux：x64 便携 `.tar.gz`。

## Mac 安装指南

### 方法 1: Homebrew 安装 (推荐)

如果您使用 [Homebrew](https://brew.sh/)，可以通过以下命令快速安装：

```bash
brew install rust-kotlin/taps/ashell --cask
```

更新应用：

```bash
brew update
brew upgrade ashell --cask
```

> **注意**: 由于应用采用本地签名，Homebrew 在安装或更新过程中会执行一个后置脚本来自动处理隔离属性（quarantine flag），这需要您输入管理员密码以授权。

### 方法 2: 手动下载

1. 从 [Releases 页面](https://github.com/rust-kotlin/ashell/releases/latest) 下载与处理器匹配的 `.dmg`。
2. 打开 DMG，将 `ashell.app` 拖入 **Applications** 目录。
3. 由于应用采用本地签名，初次启动时如果系统提示“App 已损坏，无法打开”，请打开终端（Terminal）并执行以下命令：

```bash
sudo xattr -cr /Applications/ashell.app
```

## Windows 安装指南

从 [Releases 页面](https://github.com/rust-kotlin/ashell/releases/latest) 下载对应安装程序：

| 系统架构 | 安装包名称后缀 |
| --- | --- |
| Intel/AMD 64 位 | `windows-x64-setup.exe` |
| Intel/AMD 32 位 | `windows-x86-setup.exe` |
| Windows on ARM | `windows-arm64-setup.exe` |

运行安装程序后，可选择是否创建桌面快捷方式；应用也会出现在开始菜单和系统卸载列表中。
当前安装包尚未使用商业代码签名证书，Windows SmartScreen 可能显示安全提醒。

## 功能特性

当前版本提供了一个功能完备的 GPUI 原生工作区：

- **本地与远程会话**：支持打开本地终端标签页或通过 SSH 连接到远程服务器。
- **高级 SSH 认证**：支持基于密码和基于密钥（文件路径或内联内容）的 SSH 连接方式。
- **会话管理**：可以轻松地保存、重新打开、编辑和删除您的 SSH 会话。
- **SFTP 集成**：内置 SFTP 文件管理器，可以浏览、上传、下载以及管理远程文件。
- **强大的终端模拟器**：使用 `alacritty_terminal` 解析终端输出，支持丰富的 ANSI 颜色代码、极速渲染和完整的键盘输入转发。
- **系统遥测**：在左侧边栏实时可视化显示 CPU、内存、Swap、网络和磁盘的使用指标。
- **主题系统**：支持直接从顶部工具栏切换多种 GPUI Component 颜色主题。
- **内置字体**：开箱即用，内置 Maple Mono NF CN 字体，提供卓越的中日韩（CJK）字符与 Nerd Font 图标支持。
- **v0.3 核心增强**：全局字体与字号可调，支持 SFTP 并发传输、布局记忆、断线感知、多语言热切换，以及终端右键复制粘贴等增强能力。

## 运行

在本地运行该应用：

```bash
cargo run --release
```

## 终端通知与任务提醒

ashell 支持 [OSC 9](https://iterm2.com/documentation-escape-codes.html)、
[OSC 99](https://sw.kovidgoyal.net/kitty/desktop-notifications/)、
[OSC 777](https://wezterm.org/escape-sequences.html) 和终端响铃（BEL），通知能力不绑定
Codex 或其它特定工具。只要 CLI、脚本或远程程序发送受支持的终端控制序列，本地终端、
SSH 和串口标签页都会将其转换为 macOS 或 Windows 系统通知，无需修改工具配置或伪装
其它终端。

OSC 9 携带通知正文；OSC 777 和 OSC 99 可以同时携带标题与正文，OSC 99 还支持
`always`、`unfocused` 和 `invisible` 显示时机。BEL 本身不含正文，因此仅显示通用提醒，
不会读取或推断当前终端画面中的内容。

未读通知会在 macOS Dock 或 Windows 任务栏图标上显示红色徽标，切回对应标签后自动
清除。标签标题左侧的 Loading 动画优先识别 OSC 133/633 shell integration 的命令开始
和结束标记；未提供协议标记时根据近期终端输出活动回退显示。

可以分别使用下面的命令测试四种提醒：

```bash
sleep 3; printf '\033]9;Task completed\007'
sleep 3; printf '\033]777;notify;Deploy;Production is ready\033\\'
sleep 3; printf '\033]99;i=ashell-test:d=0;Build completed\033\\'; printf '\033]99;i=ashell-test:p=body;Artifacts are ready\033\\'
sleep 3; printf '\007'
```

执行后在 3 秒内切换到其他应用。macOS 第一次收到通知时可能会询问权限；如果仍未显示，
请在“系统设置 > 通知 > ashell”中确认已允许通知。

## 打包 macOS 应用

```bash
./scripts/package-macos-app.sh
open target/release/ashell.app
```

该打包脚本会创建一个标准的 `.app` 应用程序包。它没有附加 entitlements 文件，并且在签名后会验证是否不存在 `com.apple.security.app-sandbox`（这意味着它在非沙盒模式下运行）。

## 打包 Linux (Debian/Ubuntu)

### 前置条件

```bash
sudo apt install pkg-config libfontconfig1-dev
cargo install cargo-deb
```

### 构建 .deb 包

```bash
cargo build --release
cargo deb
```

生成的 `.deb` 文件位于：

```
target/debian/ashell_0.4.9-1_amd64.deb
```

### 安装

```bash
sudo dpkg -i target/debian/ashell_0.4.9-1_amd64.deb
```

安装后可通过应用菜单或命令行 `ashell` 启动。`.deb` 包包含以下内容：
- `/usr/bin/ashell` — 主程序
- `/usr/share/applications/ashell.desktop` — 桌面入口
- `/usr/share/icons/hicolor/256x256/apps/ashell.png` — 应用图标

## 协议

本项目采用 [GPL-3.0-or-later 协议](LICENSE) 开源。
