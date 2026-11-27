# Changelog

All notable changes to KwikPaste are documented here.

## 2.0.11 - 2026-11-27

- Added remote sync: create a sync group under Sync → Remote Sync and let other devices join with an 8-digit pairing code, so new copies and preferences sync even when the devices aren't on the same LAN. Content is end-to-end encrypted and the server can't read it. Use the official server (text is kept for 24 hours so offline devices catch up; images are compressed and forwarded live, never stored) or run your own with Docker (choose image quality and size limits, and optionally sync the whole history, including favorites, pins, notes and deletions).
- The Translation extension can use a chosen service: Google, Microsoft, Volcano or Tencent TranSmart. Auto-translate also handles short foreign text and re-copied older items.
- Translations can be picked word by word in the words view.
- A single copied image file shows as an image in the preview, and Ctrl+wheel (Cmd+wheel on macOS) over a preview image zooms it in place.
- On Windows, images sent from a phone through the official WeType app are recorded as images instead of a file that soon disappears.
- WeType sync errors are localized, and pairing codes may contain spaces.
- In Preferences, enabled and disabled extensions are easier to tell apart, and the Extensions menu in the sidebar can be collapsed.

## 2.0.10 - 2026-11-23

- Added the Translation extension: once installed from Extensions in Preferences, text items can be translated from the context menu or with a shortcut, and the translation shows on the card and in the preview, ready to copy or paste on its own. It uses free translation by default, works with OpenAI-compatible AI APIs, and can translate new foreign-language copies automatically.
- Added Pin to Screen: pin image and text items to the screen as always-on-top floating windows.
- Ctrl+click (Cmd+click on macOS) or Shift+click a card to start multi-select.
- Added a Links category to the group bar.
- Appearance has a new Text Size option: Small, Default or Large.
- Added Join Line Breaks for These Apps: text copied from the chosen apps, such as PDF readers, has the line breaks inside paragraphs joined.
- View Image is now View Image / Edit Text: its key can be changed and text items open the editor; card clicks can be set to Double-click to View Image / Edit Text.
- Scrolling the mouse wheel over an image in the preview opens the full-size viewer.
- Custom rule regexes accept lookarounds, backreferences and \K.
- In Preferences, each installed extension has its own page under Extensions in the sidebar, and search also matches descriptions and option names.
- The tray menu has an Open Clipboard item.
- Menus, dropdowns and toasts have a clear outline and shadow, and many details in the list, Preferences and onboarding were polished.
- On Windows, KwikPaste releases most of its memory after sitting hidden for 30 seconds.
- On Windows, temporary image files copied from apps such as WeChat are recorded as images.
- Terminals whose paste key isn't Ctrl+V get Shift+Insert instead.
- Pressing Enter or quick paste right after typing a search pastes the new result instead of the row selected before the search.
- Sensitive masking also covers JSON headers, curl -u, credentials in URLs and password fields.
- More reliable auto-update: signatures are checked against the package file name, the updated app no longer inherits crash-recovery arguments, a changed mirror URL doesn't trigger a new download, and update errors offer Retry.
- LAN sync finishes large images on slow networks and retries failed catch-up automatically.
- Backup import and export stream their data and use much less memory on large backups.
- Fixed submenus running outside the window, the clipboard window position not being remembered after dragging it past the top or left screen edge, double-tap modifiers occasionally getting stuck, and keys going to the other pane in the two-column layout on Windows.

## 2.0.9 - 2026-10-30

- Added Group by Source App: items copied from the chosen apps go into a group automatically; items already in another group stay there.
- Appearance lets you choose which group bar buttons appear after All and drag them into order; the selected group button stays icon-only instead of expanding with its name.
- Ctrl+click (Cmd+click on macOS) a link or email in quick snippets to open it.
- Global shortcuts can be recorded with punctuation, Shift+digits and arrow keys.
- Sensitive masking covers only secrets and credential header values, so the rest of a curl command stays readable; passwords and keys right next to Chinese text are now detected too.
- Split Words breaks at underscores, so text like 123-456_654-321 is no longer one piece.
- The preview closes once the pointer leaves and stays open over the quick snippet menu; long quick snippets wrap.
- Shortening history retention asks for confirmation before deleting the items outside the new range.
- More reliable auto-update: installer signatures are checked again right before installing, macOS restores the original app if replacing it fails, and update checks keep running after a failed preparation.
- More reliable backup import and export: an overwrite import keeps the existing history until it completes, a merge keeps last-used times, settings refresh right after an import, and on macOS backups open straight from Finder.
- More stable LAN sync: fixed items missed after a failed transfer, stalled connections, and unpaired devices still connecting.
- Fixed the list jumping up when reusing the top visible item after scrolling.
- Fixed batch deletion possibly removing items that had just been pinned or favorited.
- Fixed V being typed into other apps when Win is released first while holding Win+V on Windows, and windows freezing or losing transparency after a graphics driver recovery.
- Fixed global shortcuts staying off after closing Preferences while recording one, the F key alone being accepted as a shortcut, and Enter in a dialog text field closing the dialog.
- Fixed the full-size viewer changing size when moved between displays with different scaling, and long context menus running outside the window.

## 2.0.8 - 2026-10-24

- Text items can be edited in a separate, resizable window; double-click the text in the preview to edit it directly.
- Several selected images and files can be pasted, copied or dragged to another app together.
- The preview and the full-size viewer can read a QR code in an image and copy its content, which is recorded as a new item.
- Quick snippets on cards have a context menu to copy or paste that part and to open links and emails; middle-click opens a link or email directly.
- Added Hide Favorites from All: favorites only show under Favorites, and search still finds them.
- Image items can show their saved original with Show in Explorer (Show in Finder on macOS).
- Report a Problem takes an optional reply email, and the diagnostics now include the OS edition and hardware.
- macOS builds are signed with a fixed certificate, so updates no longer require granting Accessibility again (updating to 2.0.8 still asks once more).
- Dates such as 05/28 and 2026-05-28 stay whole in quick snippets and Split Words.
- When there is no room for the preview beside the clipboard window (for example on a portrait screen), it goes above or below and covers as little of the window as possible.
- The quick snippets area in the preview has a maximum height and scrolls when there are many; Split Words applies only to the current preview and no longer changes the preview setting.
- Pasting as plain text drops trailing spaces and line breaks.
- When the system blocks the paste keystroke (often game anti-cheat), KwikPaste says so and offers a one-click restart to bring auto-paste back.
- Fixed dragging the panel by a blank area not working right after a click on Windows.
- Fixed a reused item not moving to the top right away once the list had been scrolled.
- Fixed a hotkey with Alt ending a rename in File Explorer when it opened the clipboard window on Windows.
- Fixed the copy sound not playing with Equalizer APO on Windows.
- Fixed the preview jumping up or down when switching between image and text or the Split Words view.
- Fixed long submenus running outside the window and not scrolling with the mouse wheel.
- Fixed dialog buttons being cut off when the window is short.

## 2.0.7 - 2026-10-18

- Fixed screenshots from tools such as Snowshot sometimes not being recorded.
- Fixed Run as administrator being lost after an automatic update, which left KwikPaste running without administrator rights.
- Fixed the system file picker sometimes hanging once images had been shown in the list.

## 2.0.6 - 2026-10-18

- Added a full-size image viewer, opened from the preview, the View Full Size menu item or Shift+Space. Recognized text sits over the image where it appears: drag to select, double-click a line, press Ctrl/Cmd+A and Ctrl/Cmd+C to copy, and press T to highlight text regions. Copied text is recorded as a new item.
- Added PP-OCRv6 text recognition engines on Windows in three tiers (Light, Standard, High accuracy), downloaded and switched in the image text recognition card on the Extensions page. They read small text, code and URLs far more accurately; after switching, Recognize all again updates older images.
- Added Text matching rules: match copied text with a regular expression to show it as a quick snippet, mark it sensitive or skip recording it, with templates for mainland ID numbers, tracking numbers, IPv4 and MAC addresses.
- Added Report a Problem in Preferences, which sends a description to the developer along with logs and diagnostics. Clipboard contents are never uploaded.
- Edited items keep their original content, restored with Restore Original in the context menu; image items can be pasted as a file with Paste as File; several selected text items can be pasted or copied as one merged text.
- HTML items are shown as a structured document in the preview, clicking a file row in the preview reveals it in the file manager, and the gap between the preview and the item can be set.
- The copy sound has a choice of tones: Classic, Chirp, Bubble, Chime and Tick.
- Updates now download in the background and install automatically once the computer has been idle for 10 minutes with no KwikPaste window open.
- All shortcuts are on one page, and the keys inside the clipboard window can be rebound; Ctrl/Cmd+E opens the content editor.
- Dropdowns in Preferences are replaced by picture tiles and segmented buttons.
- The Acrylic material is replaced by Frosted, which makes the whole window translucent; on Windows, windows with a title bar draw their own so the title bar and content are one frosted surface.
- On Windows the source app is taken from the clipboard's actual writer, so it is more accurate; thumbnails and previews are decoded at display size, which is faster and uses less memory.
- Multi-line notes are shown on one line on the card, separated by " / ".
- Fixed the start of the copy sound being cut off and stuttering on Windows.
- Fixed the mouse wheel not scrolling in dialogs, and the split-words preview not closing when the pointer leaves it.
- Fixed dropdowns, toasts and dialogs turning transparent and cards getting a dark ring under the Frosted material.

## 2.0.5 - 2026-10-15

- Added Double-tap to open: quickly press Ctrl, Alt or Shift twice (and Command on macOS) to show or hide the clipboard window. It can be limited to the left or right key, and is turned on in Preferences under shortcuts.
- A shortcut conflict now names the feature it clashes with right under the setting, and a shortcut already taken by another app is reported too.
- The search box can filter records by time, source app and type.
- Preferences are regrouped into 10 tabs, rarely used options are folded under More options, and search finds settings across tabs.
- The text preview lists the links, emails, phone numbers, times and codes found in the text; click one to copy only that part. They are colored by type (can be turned off in Preferences), links in the preview open directly, and the gap between the preview and the clipboard window is tighter.
- Fixed Win+Shift+V and other combinations being taken over as Win+V on Windows; only a plain Win+V is taken over now.
- Fixed being unable to reset to the default data folder when a custom one is unavailable (for example an unplugged drive), and the install folder can no longer be chosen as the data folder.
- Fixed the first launch removing a startup entry left by 1.x.
- Fixed KwikPaste's hidden window being flagged by some games' anti-cheat on Windows.
- Fixed rough rounded borders on translucent windows.
- Fixed on macOS: the target app not receiving keys when pasting from a pinned clipboard window, the app being restarted again and again after waking from sleep, and Preferences and the welcome window not coming to the front.
- Fixed Split words selecting a record other than the one clicked, the note caret not being at the end, and a click not entering edit mode again.

## 2.0.4 - 2026-10-14

- Added Window layout: switch to two columns to keep favorites and custom groups beside the clipboard history, so you don't have to switch back and forth.
- The clipboard window can be made smaller, down to 280×400 per column.
- The search box is wider and fills the rest of the header; the Preferences button moved to the footer, which also shows the position of the selected item.
- Link, email, color and path labels are now colored tags that are easier to tell apart, and rich-text (HTML, RTF) records that contain a link can open it directly. New installs include Open link in the quick actions by default.
- Import backup accepts EcoPaste backups and tells you when a backup file is not recognized.
- Updates are now checked every six hours, and a failed check is retried after an hour. A new version opens the update window once; after you close it, a NEW badge stays at the top of the clipboard window and the same version does not pop up again. The Check for updates automatically, frequency and Skip this version options were removed.
- The Preferences window can be made smaller, its sidebar can be collapsed and stays collapsed after a restart, and Esc closes the window. The storage card at the bottom of the sidebar was removed.
- A single Esc in the search box now closes the clipboard window instead of needing two.
- Fixed Excel reporting a clipboard problem when dragging cells on Windows.
- Fixed the clipboard window covering Preferences or the update window when they were opened from it.

## 2.0.3 - 2026-10-11

- Fixed KwikPaste crashing when Check for updates finds a new version.
- Fixed the update window being too short when an update was already known, which cut off the release notes and put the buttons on top of the text.

## 2.0.2 - 2026-10-11

- Added Preferences › Extensions, where you can install, enable, update and remove the official extensions. Extensions are downloaded separately, signature-checked and updated in the background; if you do not install one it uses no disk space or memory.
- Added the Image text recognition extension: text in copied images is recognized automatically with the system's own recognizer, entirely on your device. You can then find an image by the text in it, use Copy text from image in the context menu, or switch the preview to Text to copy only part of it. Preferences › Image text recognition shows the progress.
- Added the WeType sync extension: copied text and images are exchanged with WeType (微信输入法) on your phone, paired with a 6-digit code. While the WeType PC client is running it handles the exchange and KwikPaste only records what the phone sends. This is unofficial and may stop working after a WeType update.
- Text records can be edited: choose Edit content in the context menu. Formatted content becomes plain text when saved.
- Pinned records now scroll with the list instead of staying fixed above it.
- The copy sound moved to Preferences › Paste & copy, with a volume slider and a preview, and can be found by searching for sound.
- Run as administrator now stays on instead of elevating only once, and shows whether KwikPaste is currently running as administrator.
- Single image files are now listed under the Images group, and Clean cache also removes file records whose files are gone from disk. Favorites and pinned records are kept.
- Fixed the clipboard window not hiding on Windows when the foreground window changes, for example with Alt+Tab.
- Fixed the category filter being ignored while viewing favorites.
- Fixed copying a PNG image sometimes re-encoding it when the clipboard was busy, which made the same image show up twice in the history.
- Fixed the Windows key being recorded as Ctrl when recording a shortcut on Windows.
- Fixed jagged diagonal strokes in small Chinese text on Windows.
- The Hide after copy description now says it applies to copying only; the window always hides after pasting.

## 2.0.1 - 2026-10-09

- Added Change data directory in Preference › Data. You can move your records, images and settings to another folder, and restore the default location later.
- The update window now shows a short summary of what changed in the new version.
- Lower memory use: closed windows now release their graphics memory, the clipboard window drops its thumbnails and hidden preview text when it hides, and app and file icons are decoded at display size and loaded with less overhead on Windows.
- Fixed KwikPaste swallowing keys that are not bound to any panel shortcut while the clipboard window is open. Plain Delete now deletes the selected record.
- Fixed the tray menu on Windows not following the system dark mode.
- Fixed the clipboard window's Mica frame going flat after it was activated for editing on Windows.
- Fixed unreadable text on some macOS windows, the permission buttons not opening the matching privacy pane, and a double-click on the top edge of the clipboard window zooming it.

## 2.0.0 - 2026-10-08

- KwikPaste 2.0 is here. The whole app is rewritten with a native Rust interface and no longer uses a WebView, so it uses far less memory and opens faster. It also comes with a refreshed look.
- Install it over 1.x and your records, groups, settings and images carry over. 1.x does not update to 2.0 on its own; download 2.0 from the website or GitHub. After the upgrade 1.x can no longer open the same data, so if you may want to go back, choose Export Backup in 1.x first.
- Added LAN Sync. Turn it on in Preference › Sync and pair devices on the same network with a 6-digit pairing code; copied text and images then reach each other in real time. You can choose whether to sync text and images, cap the image size, and add a device by its address.
- Added a Paste as plain text global shortcut. It pastes the current clipboard content without formatting, without opening the clipboard window.
- Added Pause shortcuts in full-screen apps and Pause shortcuts in these apps. While a full-screen app, such as a game, or a listed app is in front, KwikPaste's shortcuts and mouse button trigger go back to that app.
- The shortcut recorder now tells you when a shortcut is already used by another app or conflicts with another KwikPaste shortcut.
- Pinned records and favorites can be reordered by dragging them in the clipboard window.
- Export Backup and Export to files are merged into one Export dialog. A backup can now hold only favorites or chosen groups; such a backup can only be merged on import. When importing a backup you can choose whether to import its settings too.
- Updates now come through a single channel, so the Receive Beta Updates and Receive Nightly Updates options are gone. The Taskbar Icon option on Windows, which had no effect, is also gone.

## 2.0.0-beta.1 - 2026-10-07

- The first beta of KwikPaste 2.0. The whole app is rewritten with a native Rust interface and no longer uses a WebView, so it uses less memory and opens faster.
- You can install it over 1.x; your records, settings and images carry over.
- This is a beta and may still have problems. Please report anything you run into.

## 1.4.0 - 2026-10-02

- Added Export to files in Preference › Data › Backup & Migration. It exports records to Excel or Markdown for reading, sorting and sharing. You can export only favorites or chosen groups, as one file or one file per group; sensitive records are left out by default. Exported files are not encrypted and cannot restore your data, so keep using Export Backup for full backups.
- Added in-app announcements. When KwikPaste has important news, such as a new release, it shows a system dialog during the update check, at most once a day. Choose Don't remind me to hide that announcement for good. With Automatically Check for Updates turned off, KwikPaste no longer fetches announcements at launch.
- New installs now start with Space Preview off and Text Preview Style set to Words. Existing settings stay as they are.
- Fixed a single copy occasionally creating two identical records.
- Fixed the clipboard window on Windows sometimes appearing behind other always-on-top windows.

## 1.3.8 - 2026-09-29

- Added multi-select. Click Select multiple at the bottom of the clipboard window, or choose Select Multiple from a record's context menu, to pick several records and delete them at once. Click to select, hold Shift to select a range, and press Ctrl+A (⌘A on macOS) to select all. Favorite and pinned records still follow the delete protection settings.
- Added list display settings in Preference › Appearance. List Style switches between Cards and Seamless, where records sit edge to edge with dividers. List Density offers Comfortable, Standard, Compact and Custom; Custom lets you set Record Spacing, Vertical Padding and whether to Show Header Row.
- Added Tray Icon Click Opens on Windows: choose whether clicking the tray icon opens the clipboard window or Preferences. It sits under System Tray Icon in Preference › General and still opens the clipboard window by default.
- Simplified Preference: setting rows no longer carry icons, descriptions appear only where they help, and Overview has its own page in the sidebar.
- On macOS you can now install KwikPaste with Homebrew: `brew install --cask mansandadada/tap/kwikpaste`.
- Fixed text dragged out on Windows not dropping into WPS and other apps.
- Fixed the Preference window not shrinking back to 960×600 after raising the Windows text size.

## 1.3.7 - 2026-09-28

- Reworked Auto Cleanup in Preference › Data. Records now expire by when you last used them instead of when they were first copied, so something copied long ago that you still paste every day is kept. Copying or pasting from history counts as use even with Update Record on Reuse turned off.
- Added custom cleanup rules. Give text, images, files, sensitive content, records from certain apps, records over a given size, or records copied only once their own retention, from minutes to forever. Rules match from top to bottom, and records that match no rule follow the Default Retention. Presets cover common cases such as images over 5 MB and sensitive content.
- Saving a cleanup setting that would delete records right away now shows how many would go and why, and asks first. A new Cleanup Status row shows the last cleanup and has a Clean Up Now button.
- Automatic cleanup now also keeps records in a custom group and records with a note, as it already did for favorites and pinned records.
- Cleanup settings take effect right away instead of at the next launch, and the Cleanup Interval setting is gone. Maximum Items applies as soon as new records come in, and the storage limit no longer rescans the data folder every minute.
- The clipboard window's footer now warns when storage is over the limit, either in Remind only mode or when auto cleanup can't free enough space. Click the warning to open the storage settings.
- The database file now shrinks after records are cleaned up. For existing data, run Clean Cache once in Preference › Data to compact the database and turn this on.
- Added Open with Mouse Button on Windows: click the middle button or a side button to open or hide the clipboard window. It is off by default; turn it on in Preference › Shortcuts.
- Reorganized Preference into nine categories. Each category shows all its settings on one scrolling page, with links to its sections at the top.
- Fixed keys pressed in the note, group and confirmation dialogs, drop-down menus or the search box also reaching the list, such as Enter pasting the selected item or Esc hiding the window.
- Fixed Ctrl+V and other Ctrl edits in the Windows search box going to the app you were in before, and arrow keys leaving the search box.
- Fixed the note and group name inputs not taking focus when their dialogs open on Windows.

## 1.3.6 - 2026-09-26

- Added Data Overview in Preference › Data. It shows what takes up storage, the total record count, daily additions, content types and source apps, and can clear all records of one content type or from one app at once while keeping favorites and pinned items. Click the storage usage at the bottom left to open it.
- Fixed the clipboard window opening on the primary display instead of the one under the cursor on Windows setups with several displays and display scaling turned on.
- Fixed the clipboard, Preference, update, context menu, preview and onboarding windows cutting off their content after raising the Windows Text size setting. These windows now grow with it, and onboarding steps that do not fit can be scrolled.
- Fixed pinned items showing as a solid block with the Mica or Acrylic window material.
- Fixed a User Account Control prompt appearing at every login when Run as Administrator and Launch at Login were both on. Starting with administrator privileges on a laptop running on battery no longer fails.
- Fixed shortcuts, the tray icon and Launch at Login still following the old settings after importing a backup.
- Fixed drop-down options in Preference being cut off, such as the Quick Paste modifier keys showing only "Ctr...".

## 1.3.5 - 2026-09-25

- Fixed pasting an image copied from a browser or another app adding a duplicate record every time. Pasting large images is also faster, taking about 40% of the time it used to for a 2–3 megapixel image.
- Fixed a newly copied image staying a grey placeholder, or showing the previous image, when the clipboard window was already open.
- Hovering, selecting and scrolling in the clipboard list now redraw only the cards that changed instead of every visible card.
- Paging through history, the category tabs, custom groups and history cleanup now use database indexes, so they stay fast as history grows. The first launch after updating builds the indexes once, which takes about a second with 20,000 records.
- Copying no longer extracts the source app's icon every time; each app's icon is read once.
- App Info now has a Website link to the official site, and the GitHub repository moved to its own Source Code link.

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
