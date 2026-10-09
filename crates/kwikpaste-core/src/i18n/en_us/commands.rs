use crate::i18n::keys::CommandKey as Key;

/// 返回美式英文 操作失败的根因文案。
pub fn label(key: Key) -> &'static str {
    match key {
        Key::ExportNoGroups => "Select at least one group or Ungrouped",
        Key::ExportPreviewChanged => "Export data changed. Preview again before confirming",
        Key::ExportEmpty => "There are no records to export in this scope",
        Key::ExportInvalidTarget => {
            "Choose a valid export folder or a file path with the matching extension"
        }
        Key::ExportExcelLimit => {
            "Content exceeds Excel cell or row limits. Use Markdown or a smaller scope"
        }
        Key::BackupPartialOverwrite => {
            "This backup contains only some records. Import it with Merge instead"
        }

        Key::PasteBusy => {
            "The previous paste or copy is still running. Try again shortly; the clipboard was not changed by this request"
        }
        Key::PasteHandoffFailed => {
            "Content was copied to the clipboard. The target window or keyboard handoff is not ready. Automatic paste did not complete; please paste manually"
        }
        Key::PastePermissionMissing => "Accessibility permission is missing. Content was copied to the clipboard; please paste manually and allow KwikPaste in System Settings > Privacy & Security > Accessibility",

        Key::DragSourceFilesMissing => "The dragged source files no longer exist",
        Key::DragImageMissing => "The image file no longer exists",
        Key::DragTextEmpty => "Text content is empty",
        Key::ExternalUrlUnsupported => "Only links starting with http or https can be opened",
        Key::FragmentUnavailable => "The selected text is no longer in this record",
        Key::SplitTextOnly => "Only text records can be split into words",
        Key::SplitSensitiveRedacted => "Sensitive content is redacted and can't be split",
        Key::PortableStorageFixed => {
            "KwikPaste Portable always keeps its data in the data folder next to the app"
        }
        Key::StorageInsufficientSpace => {
            "There is not enough free space on the target disk to move your data"
        }
        Key::StorageSpaceUnavailable => {
            "Cannot read the target disk’s free space. Check folder permissions and connectivity"
        }
        Key::StorageTargetHasData => "The target directory already contains KwikPaste data",
        Key::StorageCustomUnavailable => {
            "The custom data directory is unavailable. Reconnect it and restart KwikPaste before changing directories"
        }
        Key::SyncNotRunning => "Turn on LAN sync first",
        Key::SyncInvalidCode => "The pairing code is 6 digits",
        Key::SyncWrongCode => "The pairing code is incorrect",
        Key::SyncPairingUnavailable => {
            "The pairing code has expired. Refresh it on the other device"
        }
        Key::SyncUnreachable => {
            "Can't reach this device. Make sure both are on the same network and LAN sync is on"
        }
        Key::SyncInvalidAddress => "Enter an address like 192.168.1.8:41573 or [fe80::1%12]:41573",
        Key::SyncIncompatible => {
            "The other device runs an incompatible KwikPaste version. Update both to the latest"
        }
        Key::SyncPeerOutdated => "The other device runs an older KwikPaste. Update it first",
        Key::SyncSelfOutdated => "This device runs an older KwikPaste. Update it first",
        Key::SyncNotPaired => "This device isn't paired yet. Pair it with a code first",
        Key::SyncDeviceNotFound => "This device is no longer in the nearby list",
        Key::SyncSelfPairing => "You can't pair this device with itself",
    }
}
