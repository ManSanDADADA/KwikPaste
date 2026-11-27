<div align="center">
  <img src="../public/logo.png" alt="KwikPaste" width="96" height="96" />

# KwikPaste · 快贴

**A fast, local-first clipboard manager for macOS and Windows.**

**Website: [paste.fastthree.com](https://paste.fastthree.com/en/)**

[简体中文](../README.md) | English

  <br />

  <img alt="100% Rust" src="https://img.shields.io/badge/Rust-100%25-b7410e?style=flat-square&logo=rust&logoColor=white" />
  <img alt="GPUI" src="https://img.shields.io/badge/UI-GPUI-6e56cf?style=flat-square" />
  <img alt="No WebView" src="https://img.shields.io/badge/WebView-none-2ea44f?style=flat-square" />
  <img alt="macOS" src="https://img.shields.io/badge/macOS-supported-000000?style=flat-square&logo=apple&logoColor=white" />
  <img alt="Windows" src="https://img.shields.io/badge/Windows-supported-0078d4?style=flat-square&logo=windows&logoColor=white" />
  <img alt="License" src="https://img.shields.io/badge/license-Apache--2.0-blue?style=flat-square" />
</div>

## About

KwikPaste keeps everything you copy — plain text, rich text, images and files — one shortcut away, and never sends any of it anywhere. History, the search index, cached resources and settings all stay on your machine.

KwikPaste 2.0 is rewritten from scratch as a pure Rust native app: the interface is drawn on the GPU with [GPUI](https://www.gpui.rs), with no embedded WebView. Clipboard capture, storage, search, system integration and rendering all run in one native process.

- **Tiny memory footprint**: under 15 MB in the background on Windows with the window hidden (measured), versus about 370 MB for 1.x — roughly a twentieth.
- **Opens instantly**: the window appears the moment you press the shortcut, with no web engine to start.
- **Native on both platforms**: Direct3D 11 rendering with Mica / frosted glass on Windows, Metal rendering with a native panel on macOS, and it never steals focus from the app you are in.

## Download

Get the latest installer from the [website](https://paste.fastthree.com/en/) or [Releases](https://github.com/ManSanDADADA/KwikPaste/releases):

- **Windows** — the `-setup.exe` installer, for x64 or ARM64.
- **Windows portable** — the `_portable.zip`, for x64 or ARM64. Unzip it and run `KwikPaste\KwikPaste.exe`. History, settings and logs stay in the `data` folder next to it, so the whole folder can move to another PC or a USB drive. Keep `portable.txt` beside the exe. The portable and installed versions can't run at the same time.
- **macOS** — the `.dmg` for your chip: `aarch64` for Apple silicon, `x64` for Intel.

On macOS you can also install it with [Homebrew](https://brew.sh), which picks the build for your chip:

```bash
brew install --cask mansandadada/tap/kwikpaste
```

See [homebrew-tap](https://github.com/ManSanDADADA/homebrew-tap) for upgrading and uninstalling.

> [!NOTE]
> The installers are not code-signed by Microsoft or Apple yet, so the first launch may show a security prompt. On Windows choose **More info → Run anyway**. On macOS open **System Settings → Privacy & Security** and click **Open Anyway**.

After that, KwikPaste keeps itself up to date. Every update package is signature-checked before it is installed, and downloads are served from a mainland China CDN with GitHub as the fallback.

**Upgrading from 1.x**: 1.x does not update to 2.0 on its own. Download 2.0 and install it over 1.x; your records, groups, settings and images carry over. After the upgrade 1.x can no longer open the same data, so if you may want to go back, choose Export Backup in 1.x first.

## Usage

| Shortcut | Action |
| --- | --- |
| <kbd>Alt</kbd> + <kbd>C</kbd> (macOS: <kbd>⌥</kbd> + <kbd>C</kbd>) | Open clipboard history |
| <kbd>Alt</kbd> + <kbd>X</kbd> (macOS: <kbd>⌥</kbd> + <kbd>X</kbd>) | Open preferences |

Both shortcuts can be changed in preferences. On Windows, KwikPaste can also take over <kbd>Win</kbd> + <kbd>V</kbd> from the built-in clipboard panel.

## Features

- Capture clipboard history for plain text, HTML, RTF, images, files, and folders.
- Search clipboard content and notes with SQLite FTS5.
- Filter history by source application and content type.
- Protect sensitive content by skipping high-confidence secrets such as private keys, service tokens, AWS keys, and JWTs.
- Preview text, images, and files in a dedicated preview window.
- Paste, copy, copy as plain text, reveal files, open links, add notes, pin, favorite, delete, and drag items out to other apps.
- Organize history with favorites, pinned items, notes, custom groups, and configurable item actions.
- Tune capture order, size limits, retention, display density, list sorting, and window behavior.
- Sync with LAN Sync: pair devices on the same network with a pairing code, and copied text and images reach each other in real time without passing through any server.
- Paste as plain text with a global shortcut, and pause shortcuts automatically while a full-screen app (such as a game) or a chosen app is in front.
- Export and import `.kwikpastebak` backups, including encrypted backup containers, or back up only favorites or chosen groups; records can also be exported to Excel or Markdown.
- Stay up to date with signed in-app updates.
- Keep clipboard data, resources, and settings local to your machine.

## Contributing

Development setup, architecture notes, quality checks, and contribution
expectations live in the [contribution guide](./CONTRIBUTING.md).

## License

KwikPaste is licensed under the [Apache License 2.0](../LICENSE).

> KwikPaste is derived from [EcoPaste](https://github.com/EcoPasteHub/EcoPaste) under the Apache-2.0 license.
