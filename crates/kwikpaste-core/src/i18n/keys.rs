#[derive(Debug, Clone, Copy)]
pub enum ClipboardMenuKey {
    Paste,
    PasteAsPlainText,
    PasteAsPath,
    Copy,
    SaveImage,
    SplitWords,
    OpenLink,
    SendEmail,
    RevealInFinder,
    RevealInExplorer,
    Favorite,
    Unfavorite,
    PinItem,
    UnpinItem,
    MoveToGroup,
    AddNote,
    EditNote,
    Select,
    Delete,
}

#[derive(Debug, Clone, Copy)]
pub enum CommandKey {
    ExportNoGroups,
    ExportPreviewChanged,
    ExportEmpty,
    ExportInvalidTarget,
    ExportExcelLimit,
    BackupPartialOverwrite,

    PasteBusy,
    PasteHandoffFailed,
    PastePermissionMissing,

    DragSourceFilesMissing,
    DragImageMissing,
    DragTextEmpty,
    ExternalUrlUnsupported,
    FragmentUnavailable,
    SplitTextOnly,
    SplitSensitiveRedacted,
    PortableStorageFixed,
    StorageInsufficientSpace,
    StorageSpaceUnavailable,
    StorageTargetHasData,
    StorageCustomUnavailable,
    SyncNotRunning,
    SyncInvalidCode,
    SyncWrongCode,
    SyncPairingUnavailable,
    SyncUnreachable,
    SyncInvalidAddress,
    SyncIncompatible,
    SyncPeerOutdated,
    SyncSelfOutdated,
    SyncNotPaired,
    SyncDeviceNotFound,
    SyncSelfPairing,
}

#[cfg(target_os = "windows")]
#[derive(Debug, Clone, Copy)]
pub enum StartupKey {
    DialogTitle,
    PortableDirNotWritable,
    WebviewMissing,
}

#[derive(Debug, Clone, Copy)]
pub enum TrayKey {
    Preference,
    Exit,
}
