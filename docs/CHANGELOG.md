# Changelog

All notable changes to KwikPaste are documented here.

## 1.2.0 - 2026-09-23

- Turned on in-app updates. Update packages are signed with the KwikPaste key and verified before installing, and downloads come from a mainland China CDN with GitHub as the fallback.
- Automatic update checks are now on by default, including beta and nightly builds. When several channels are enabled, KwikPaste installs whichever one offers the newest version.
- Unified window materials and appearance controls, using native materials where the system supports them and falling back where it doesn't.
- Turned the hover preview into a native-material panel window.
- Load image thumbnails on demand, with same-size placeholders while they load.
- Stopped the hover preview from redoing window setup and data loading on every hover frame.
- Avoided redundant database reads and full-text index rewrites.
- Fixed hover preview panel positioning and opacity.
- Named the app and installers KwikPaste on both Windows and macOS.
- Removed the first-run legacy data import step.

## 1.1.0 - 2026-09-16

- Established the Chinese product name **快贴** and the English technical name **KwikPaste**.
- Changed the application identifier to `com.fastthree.kwikpaste`.
- Replaced application, installer, favicon, and tray artwork with the KwikPaste K mark.
- Added `.kwikpastebak` backup packages and isolated `KwikPasteData` storage.
- Disabled the inherited updater until FastThree signing and release infrastructure are ready.
- Removed the inherited sponsor QR entry and redirected the project link to `https://paste.fastthree.com`.

Earlier upstream release history is available in the [EcoPaste](https://github.com/EcoPasteHub/EcoPaste) repository. Required attribution is retained in README.md and LICENSE.
