//! 剪贴板管线：监听、读取、归类、内容识别、图片与图标落盘、写回、回环抑制、来源应用登记与历史自动清理。
//!
//! 直接调用 Win32 / AppKit 的部分（前台应用识别、应用扫描、提示音）经 [`crate::platform`] 由平台层提供。

mod app_store;
pub(crate) mod apps_registry;
mod backend;
pub(crate) mod cleanup;
mod detect;
mod file_icon_store;
mod fragment;
mod guard;
pub mod icon;
mod ingest;
mod payload;
pub(crate) mod persist;
mod read;
mod secrets;
mod storage;
pub(crate) mod watcher;
mod write;

pub use app_store::AppIconStore;
pub use apps_registry::{materialize_source, AppsRegistry};
pub use backend::{
    ClipboardBackend, ClipboardFormat, ClipboardProvider, ClipboardWrite, DecodedImage,
    MemoryClipboard, MemoryState, SystemClipboard, SystemClipboardProvider,
};
pub use cleanup::{CleanupPreview, CleanupReport, CleanupStatus, RulePreview, StorageCheck};
pub use detect::{detect_text_sub_kind, sanitize_css_color};
pub use file_icon_store::FileIconStore;
pub(crate) use fragment::select_words;
pub use fragment::{
    fragment_source, quick_snippets, resolve_fragment, split_words, word_spans, ClipboardFragment,
    WordSpan, WordSplit, WordToken, MAX_SPLIT_CHARS,
};
pub use guard::WritebackGuard;
#[cfg(target_os = "windows")]
pub use icon::set_helper_exe;
pub use icon::{get_icon_cache_key, icon_png, DIR_CACHE_KEY};
#[cfg(test)]
pub use ingest::build_item;
pub(crate) use ingest::rewrite_text_content;
pub use ingest::{build_item_with_settings, SUMMARY_MAX_CHARS};
pub use payload::{ClipboardPayload, ImagePayload, TextPayload};
pub use read::{png_dimensions, ClipboardReader};
pub use secrets::contains_secret;
pub(crate) use storage::FILE_THUMBNAILS_DIR;
pub use storage::{
    image_file_dimensions, validate_image_file_name, ImageStore, StoredImage, THUMBNAIL_MAX,
};
pub use watcher::WatcherPause;
pub use write::{write_text_fragment, write_to_clipboard};
