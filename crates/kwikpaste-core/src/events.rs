//! core 通知宿主的事件。
//!
//! 对应 1.x 里 `app.emit` 发给前端的领域事件；宿主收到后自行决定刷新哪些界面、执行哪些系统级副作用。

use std::sync::Arc;

use crate::clipboard::CleanupStatus;
use crate::db::models::ClipboardKind;
use crate::settings::{Settings, SettingsDelta};

/// 接收 [`CoreEvent`] 的出口。任意线程都可能调用，实现不得阻塞（转发到 channel 即可）。
pub trait EventSink: Send + Sync + 'static {
    fn emit(&self, event: CoreEvent);
}

impl<F> EventSink for F
where
    F: Fn(CoreEvent) + Send + Sync + 'static,
{
    fn emit(&self, event: CoreEvent) {
        self(event);
    }
}

/// 丢弃所有事件，供不关心通知的宿主与测试使用。
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopSink;

impl EventSink for NoopSink {
    fn emit(&self, _event: CoreEvent) {}
}

#[non_exhaustive]
#[derive(Debug, Clone)]
pub enum CoreEvent {
    ExtensionsChanged,
    /// 图片文字识别状态变化，运行中进度最多每秒两次。
    OcrChanged,
    /// 一条记录入库或命中已有内容（1.x `clipboard://updated` 的 `{ id, kind, deduplicated }`）。
    ClipboardUpserted {
        id: String,
        kind: ClipboardKind,
        deduplicated: bool,
    },
    /// 自动或手动清理删掉了记录（1.x `clipboard://updated` 的 `{ cleanup }`），列表需要整体刷新。
    ClipboardCleaned {
        removed: u64,
    },
    /// 设置已落盘（1.x `settings://updated`）。`delta` 说明这次改了哪些部分，宿主据此重注册快捷键、
    /// 重建托盘、切换材质、同步自启等。
    SettingsUpdated {
        settings: Arc<Settings>,
        delta: SettingsDelta,
    },
    /// 清理状态变化（1.x `cleanup://status`）。
    CleanupStatus(CleanupStatus),
    /// 自定义分组增删改或排序变了（1.x `clipboard-groups://updated`），分组栏需要重新拉取。
    GroupsUpdated,
    /// 历史数据整体换了一份：切换存储位置、导入备份（1.x `clipboard://updated` 的 `{ imported: true }`）。
    /// 列表、分组栏、来源应用都要重新拉取。
    ClipboardReloaded,
    /// 局域网同步状态变了（启停、设备上下线、配对码、附近设备，1.x `sync://lan-state`），
    /// 偏好页需要时用 [`crate::Core::lan_sync_state`] 重新取。
    LanSyncChanged,
    /// 别的设备用本机配对码配对成功（1.x `sync://lan-paired`）。
    LanDevicePaired {
        name: String,
    },
}
