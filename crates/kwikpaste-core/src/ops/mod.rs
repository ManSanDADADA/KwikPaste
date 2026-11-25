//! 记录、分组、来源应用与存储位置的操作：从 1.4.0 命令层下沉的业务规则（写回前的纯文本判定、复用计数、
//! 备注归一化、分组校验、删除连带的图片文件、链接规范化、数据目录迁移与缓存清理等），
//! 以 `Core` 方法的形式提供给宿主。
//!
//! 窗口相关的收尾（复制后隐藏窗口、粘贴前隐藏并注入按键、打开链接、在文件管理器中显示、
//! 另存为对话框）归宿主：这里只返回宿主需要的信息。

mod apps;
mod groups;
mod items;
mod storage;
#[cfg(test)]
mod storage_tests;
#[cfg(test)]
mod tests;

pub use crate::db::items::{ReorderAnchor, ReorderSection};
pub use apps::{ClipboardAppView, HistoryApp};
pub use groups::{ClipboardGroupInput, ClipboardGroupLayoutInput, DEFAULT_CLIPBOARD_GROUP_ICON};
pub(crate) use items::delete_one;
pub use items::{
    CapturedItem, CopyOutcome, ImageSave, MergedSelection, QuickPasteTicket, UpdateNoteResult,
};
pub use storage::{
    ChangeStorageLocationResult, CleanCacheResult, PreferenceDirectory, ReclaimableCache,
    StorageBreakdown, StorageOverview, StorageUsage,
};
