# Changelog

All notable changes to KwikPaste are documented here.

## 1.3.2 - 2026-09-24

- Added Quick Paste: hold the modifier keys and press a number to paste without opening the clipboard window. 1–9 paste items 1–9 and 0 pastes item 10, in the same order as the All list. It is off by default; turn it on in Preference › Shortcuts, where the modifier keys (Ctrl+Shift by default) can be changed.
- Text records now list the codes, model numbers, numbers and links found in them below each item. Click one to paste just that part. This is on by default and can be turned off with Extract Quick Info.
- Added Split Words: split a text record into words, pick the ones you need, and paste only those.
- The hover preview stays open while the pointer moves onto it, and its text can be shown as words to pick from and paste in place.
- Hover preview is now on by default for new installs. Existing settings are kept.
- The clipboard window now opens on All by default for new installs. Existing settings are kept.
- Opening Preference now hides the clipboard window.
- Added a Windows portable build: unzip it and run. Data stays in the `data` folder next to the app, and in-app updates replace the portable app in place.
- Shrank the Windows installer from 7.6 MB to 4.2 MB.

## 1.3.0 - 2026-09-24

- Added a Storage Limit setting, 1 GB by default. The storage meter in the sidebar fills at this size. Going over it only shows a reminder by default; switch to Auto clean to delete the oldest regular records until usage is back under the limit. Favorites and pinned records are always kept.
- Moved Clear Records from the clipboard window's more-actions menu into the Storage Locations settings.
- Trimmed the tray menu to Preference and Exit, and removed the version number from the tray tooltip.
- Update checks now use the system proxy, query every enabled channel at the same time, and time out instead of hanging when a server can't be reached.

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
