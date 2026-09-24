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
    Delete,
}

#[derive(Debug, Clone, Copy)]
pub enum CommandKey {
    DragSourceFilesMissing,
    DragImageMissing,
    DragTextEmpty,
    ExternalUrlUnsupported,
    FragmentUnavailable,
    SplitTextOnly,
    SplitSensitiveRedacted,
    PortableStorageFixed,
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
