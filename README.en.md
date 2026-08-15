[中文](README.md) | [English](README.en.md)

# ashell

![Preview](preview.png)

`ashell` is a modern, GPUI Component-based desktop terminal client written in Rust.

This project focuses on providing a high-performance and visually appealing shell workspace by combining local and remote environments with a rich set of built-in features. 

## 🚀 v0.4 Major Upgrades

v0.4 builds on the v0.3 foundation and focuses on more capable workspace operations and a smoother daily workflow:
- ✨ **Keybinding Management**: View and edit common shortcuts from the settings UI with conflict hints.
- ✨ **Settings Page Polish**: A more compact and clearer settings experience with improved layout and interaction flow.
- ✨ **Multi-Pane Tabs with a tmux-like Workflow**: A single tab can now host multiple panes, with split, focus, and switching actions for a tmux-inspired experience.
- ✨ **Transfer History Improvements**: The transfer history panel now presents richer task details, making upload and download activity easier to review.
- ✨ **SSH Passphrase Support**: Private keys can now store a passphrase, and SSH connections will use it automatically.
- ✨ **Terminal Rendering Enhancements**: Terminal rendering now handles Block Elements and similar custom glyphs more completely.

## Download

You can download the latest installers from the [GitHub Releases page](https://github.com/rust-kotlin/ashell/releases/latest):

- macOS: `.dmg` installers for x64 and arm64, plus portable `.zip` archives.
- Windows: `Setup.exe` installers for x64, x86, and arm64, plus portable `.zip` archives.
- Linux: a portable x64 `.tar.gz` archive.

## Mac Installation Guide

### Method 1: Homebrew (Recommended)

If you use [Homebrew](https://brew.sh/), you can install it quickly with:

```bash
brew install rust-kotlin/taps/ashell --cask
```

To update the app:

```bash
brew update
brew upgrade ashell --cask
```

> **Note**: Since the app uses ad-hoc signing, the Homebrew installation or update includes a postflight script to automatically handle the quarantine flag. This will require you to enter your administrator password for authorization.

### Method 2: Manual Download

1. Download the `.dmg` matching your processor from the [Releases page](https://github.com/rust-kotlin/ashell/releases/latest).
2. Open the DMG and drag `ashell.app` into **Applications**.
3. Since the app uses ad-hoc signing, macOS may warn that the app is "damaged" upon first launch. If this happens, open Terminal and run:

```bash
sudo xattr -cr /Applications/ashell.app
```

## Windows Installation Guide

Download the appropriate installer from the [Releases page](https://github.com/rust-kotlin/ashell/releases/latest):

| System architecture | Installer suffix |
| --- | --- |
| Intel/AMD 64-bit | `windows-x64-setup.exe` |
| Intel/AMD 32-bit | `windows-x86-setup.exe` |
| Windows on ARM | `windows-arm64-setup.exe` |

The installer can optionally create a desktop shortcut and registers ashell in the Start menu and the Windows uninstall list.
The installer is not yet signed with a commercial code-signing certificate, so Windows SmartScreen may display a security warning.

## Features

The current version provides a fully-featured GPUI-native workspace:

- **Local & Remote Sessions:** Open local terminal tabs or connect to remote servers via SSH.
- **Advanced SSH Authentication:** Supports both password-based and key-based (file path or inline) SSH connections.
- **Session Management:** Easily save, reopen, edit, and remove your SSH sessions.
- **SFTP Integration:** Built-in SFTP file manager to browse, upload, download, and manage remote files.
- **Robust Terminal Emulator:** Parses terminal output with `alacritty_terminal`, supporting rich ANSI color spans, fast rendering, and complete keyboard input forwarding.
- **System Telemetry:** Real-time visualization of CPU, memory, swap, network, and disk metrics in the left cockpit sidebar.
- **Theming System:** Switch between multiple GPUI Component themes directly from the top toolbar.
- **Embedded Fonts:** Uses embedded Maple Mono NF CN fonts out-of-the-box for excellent CJK character and Nerd Font icon support.
- **v0.3 Core Enhancements:** Global font and font-size controls, concurrent SFTP transfers, persistent layout state, disconnect awareness, hot-swappable i18n, and smart terminal right-click copy/paste.

## Run

To run the application locally:

```bash
cargo run --release
```

## Package macOS App

```bash
./scripts/package-macos-app.sh
open target/release/ashell.app
```

The packaging script creates a standard `.app` bundle. It does not attach an entitlements file, and after signing, it verifies that `com.apple.security.app-sandbox` is not present (meaning it runs non-sandboxed).

## Package Linux (Debian/Ubuntu)

### Prerequisites

```bash
sudo apt install pkg-config libfontconfig1-dev
cargo install cargo-deb
```

### Build .deb

```bash
cargo build --release
cargo deb
```

The `.deb` file will be generated at:

```
target/debian/ashell_0.4.8-1_amd64.deb
```

### Install

```bash
sudo dpkg -i target/debian/ashell_0.4.8-1_amd64.deb
```

After installation, you can launch from the application menu or by running `ashell` in the terminal. The `.deb` package includes:
- `/usr/bin/ashell` — Main binary
- `/usr/share/applications/ashell.desktop` — Desktop entry
- `/usr/share/icons/hicolor/256x256/apps/ashell.png` — Application icon

## License

This project is licensed under the [GPL-3.0-or-later License](LICENSE).
