use crate::i18n::keys::StartupKey as Key;

/// 返回美式英文启动自检弹窗文案。
pub fn label(key: Key) -> &'static str {
    match key {
        Key::DialogTitle => "KwikPaste",
        Key::PortableDirNotWritable => {
            "KwikPaste Portable can't save data in the folder it runs from.\n\nMove the whole folder somewhere writable (for example the desktop, drive D: or a USB drive), then open it again."
        }
        Key::WebviewMissing => {
            "This computer is missing Microsoft Edge WebView2, which KwikPaste needs to run.\n\nClick OK to download the installer, then open KwikPaste again once it is installed."
        }
    }
}
