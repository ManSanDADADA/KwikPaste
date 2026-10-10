//! 采集到的记录去重入库并通知宿主。剪贴板监听、手动读取和局域网同步收到的记录都走这里，
//! 保证几条入口的入库语义与事件一致。

use crate::db::apps::upsert_app;
use crate::db::items::{upsert_item, UpsertResult};
use crate::db::models::{ClipboardApp, ClipboardItem};
use crate::error::Result;
use crate::events::CoreEvent;
use crate::root::CoreInner;

/// 先写来源应用再去重入库，然后通知清理与宿主、推给在线的同步设备，最后按设置播放复制提示音。
/// 本机复制（监听、手动读取）走这里；局域网同步收到的记录走 [`store_and_emit`]，不放提示音。
///
/// 应用 upsert 失败不阻断条目入库——清掉 `source_app_id` 后继续，避免单次系统调用抽风丢内容。
pub(crate) async fn persist_and_notify(
    core: &CoreInner,
    item: &ClipboardItem,
    source_app: Option<&ClipboardApp>,
) -> Result<UpsertResult> {
    let pool = core.db.pool().await;
    let mut item_to_write = item.clone();
    if let Some(src) = source_app {
        if let Err(err) = upsert_app(&pool, src).await {
            log::warn!("clipboard source app upsert failed ({}): {err}", src.id);
            item_to_write.source_app_id = None;
        }
    }

    let result = store_and_emit(core, &item_to_write).await?;
    // 本机复制的记录才有同步序号：对端补齐时按它取「上次之后本机复制过的」。
    let seq = match crate::db::sync::assign_sync_seq(&core.db.pool().await, &result.id).await {
        Ok(seq) => seq,
        Err(err) => {
            log::warn!("assign sync sequence failed: {err}");
            None
        }
    };
    crate::sync::on_local_capture(core, &item_to_write, seq);
    let feedback = core.settings.snapshot().clipboard.feedback;
    if feedback.copy_sound {
        core.platform()
            .play_copy_sound(feedback.copy_sound_volume.min(100));
    }
    Ok(result)
}

/// 去重入库 + 通知清理 + 发 [`CoreEvent::ClipboardUpserted`]。
pub(crate) async fn store_and_emit(core: &CoreInner, item: &ClipboardItem) -> Result<UpsertResult> {
    // 去重是「先查再插」：一次复制触发的多个剪贴板事件、同步收到的记录和本机复制撞上时，
    // 并发的两次入库可能都查不到而插出两行同样的内容，所以这一段串行执行。
    // 连接池在拿到锁之后再取：切换存储位置、覆盖导入换库期间持有这把锁，等它们换完拿到的是新库。
    let result = {
        let _serial = core.upsert_lock.lock().await;
        let pool = core.db.pool().await;
        upsert_item(&pool, item).await?
    };
    if !result.deduplicated {
        super::cleanup::notify_inserted(core);
        if item.kind == crate::db::models::ClipboardKind::Image
            && core.extensions.resolve("ocr").is_some()
        {
            core.ocr.nudge();
        }
    }

    core.events.emit(CoreEvent::ClipboardUpserted {
        id: result.id.clone(),
        kind: item.kind,
        deduplicated: result.deduplicated,
    });
    Ok(result)
}
