<div align="center">
  <img src="./public/logo.png" alt="KwikPaste" width="96" height="96" />

# KwikPaste · 快贴

**A fast, local-first clipboard manager for macOS and Windows.**

English | [简体中文](./docs/README.zh-CN.md)

  <br />

  <img alt="Tauri v2" src="https://img.shields.io/badge/Tauri-v2-24c8db?style=flat-square" />
  <img alt="Rust first" src="https://img.shields.io/badge/Rust-first-b7410e?style=flat-square" />
  <img alt="React 19" src="https://img.shields.io/badge/React-19-61dafb?style=flat-square" />
  <img alt="macOS" src="https://img.shields.io/badge/macOS-supported-000000?style=flat-square&logo=apple&logoColor=white" />
  <img alt="Windows" src="https://img.shields.io/badge/Windows-supported-0078d4?style=flat-square&logo=windows&logoColor=white" />
  <img alt="License" src="https://img.shields.io/badge/license-Apache--2.0-blue?style=flat-square" />
</div>

## About

KwikPaste keeps everything you copy — plain text, rich text, images and files — one shortcut away, and never sends any of it anywhere. History, the search index, cached resources and settings all stay on your machine.

It is built on a Rust-first Tauri architecture: clipboard capture, storage, search and system integration run in Rust, while the React frontend focuses on rendering and interaction. The result is a small, fast app that feels at home on both macOS and Windows.

## Download

Get the latest installer from [Releases](https://github.com/ManSanDADADA/KwikPaste/releases):

- **Windows** — the `-setup.exe` installer, for x64 or ARM64.
- **macOS** — the `.dmg` for your chip: `aarch64` for Apple silicon, `x64` for Intel.

> [!NOTE]
> The installers are not code-signed by Microsoft or Apple yet, so the first launch may show a security prompt. On Windows choose **More info → Run anyway**. On macOS open **System Settings → Privacy & Security** and click **Open Anyway**.

After that, KwikPaste keeps itself up to date. Every update package is signature-checked before it is installed, and downloads are served from a mainland China CDN with GitHub as the fallback.

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
- Export and import `.kwikpastebak` backups, including encrypted backup containers.
- Stay up to date with signed in-app updates.
- Keep clipboard data, resources, and settings local to your machine.

## Contributing

Development setup, architecture notes, quality checks, and contribution
expectations live in the [contribution guide](./docs/CONTRIBUTING.md).

## License

KwikPaste is licensed under the [Apache License 2.0](./LICENSE).

> KwikPaste is derived from [EcoPaste](https://github.com/EcoPasteHub/EcoPaste) under the Apache-2.0 license.
