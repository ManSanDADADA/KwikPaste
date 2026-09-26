//! 历史清理后台任务：按 `clipboard.history.retention` + `maxCount` 定期裁剪，
//! 存储上限设为自动清理时再按占用裁剪。
//!
//! 启动即跑一次；之后按用户设置的清理周期触发，每次都从 `SettingsStore` 取最新配置——
//! 用户在偏好里调时长 / 上限后不必重启即可生效。置顶与收藏项一律保留（由 [`cleanup_history`] 保证）。

use std::path::Path;
use std::time::{Duration, Instant};

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use serde_json::json;
use sqlx::SqlitePool;
use tauri::{AppHandle, Emitter, Manager};

use super::storage::ImageStore;
use super::watcher::CLIPBOARD_UPDATED_EVENT;
use crate::core::disk::{dir_size, file_size};
use crate::db::items::{
    cleanup_history, cleanup_oldest_until, reusable_page_bytes, CleanupOutcome,
};
use crate::settings::{Retention, RetentionUnit, SettingsStore, StorageLimitAction};

/// 调度器检查设置与到期状态的频率；真正清理只在用户设置周期到期后执行。
const SCHEDULER_TICK_INTERVAL: Duration = Duration::from_secs(60);

/// 启动历史清理后台任务：启动立即清理一次，之后按设置周期到点清理。
pub fn spawn(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        run_once(&app).await;
        enforce_storage_limit(&app).await;
        let mut last_cleanup_at = Instant::now();
        let mut ticker = tokio::time::interval(SCHEDULER_TICK_INTERVAL);
        ticker.tick().await;

        loop {
            ticker.tick().await;
            // 存储上限不跟随清理周期（默认只在启动时清理），否则新图片写入后要到下次启动才回到上限以内。
            enforce_storage_limit(&app).await;

            let Some(interval) = cleanup_interval(&app) else {
                continue;
            };

            if last_cleanup_at.elapsed() < interval {
                continue;
            }

            run_once(&app).await;
            last_cleanup_at = Instant::now();
        }
    });
}

async fn run_once(app: &AppHandle) {
    let history = match app.try_state::<SettingsStore>() {
        Some(store) => store.snapshot().clipboard.history,
        None => return,
    };

    let cutoff = retention_cutoff(&history.retention, Utc::now());
    let max = (history.max_count > 0).then_some(history.max_count);

    if cutoff.is_none() && max.is_none() {
        return;
    }

    let pool = app.state::<crate::db::DatabaseState>().pool().await;
    match cleanup_history(&pool, cutoff, max).await {
        Ok(outcome) => apply_outcome(app, &outcome, "history"),
        Err(err) => log::warn!("history cleanup failed: {err}"),
    }
}

/// 存储上限设为自动清理且占用超出时，从最旧的普通记录删起，直到回到上限以内。
async fn enforce_storage_limit(app: &AppHandle) {
    let history = match app.try_state::<SettingsStore>() {
        Some(store) => store.snapshot().clipboard.history,
        None => return,
    };
    if history.storage_limit_action != StorageLimitAction::Cleanup {
        return;
    }

    let pool = app.state::<crate::db::DatabaseState>().pool().await;
    let used = match storage_bytes_in_use(app, &pool).await {
        Ok(used) => used,
        Err(err) => {
            log::warn!("measure storage for limit cleanup failed: {err}");
            return;
        }
    };
    let limit = history.storage_limit_bytes();
    if used <= limit {
        return;
    }

    let image_bytes = |file_name: &str| {
        app.try_state::<ImageStore>()
            .map_or(0, |store| store.stored_bytes(file_name))
    };
    match cleanup_oldest_until(&pool, used - limit, image_bytes).await {
        Ok(outcome) if outcome.removed == 0 => {
            // 每分钟都会走到这里，只记 debug，避免超限期间刷满日志文件。
            log::debug!("storage stays over the limit: history cleanup cannot free enough space");
        }
        Ok(outcome) => apply_outcome(app, &outcome, "storage limit"),
        Err(err) => log::warn!("storage limit cleanup failed: {err}"),
    }
}

/// 数据实际占用：数据目录总大小减去 SQLite 可复用的空闲页和 WAL 旁路文件。
/// 偏好页展示与存储上限清理共用这一口径——删行后数据库文件不会立即缩小，
/// 按目录原始大小判断会让下一轮把已释放的空间再算一遍而继续误删，侧栏也会一直显示超限。
pub async fn storage_bytes_in_use(app: &AppHandle, pool: &SqlitePool) -> crate::core::Result<u64> {
    let total = dir_size(&crate::core::paths::app_data_dir(app)?)?;
    let db_path = crate::db::db_path(app)?;

    let mut reusable = reusable_page_bytes(pool).await?;
    for suffix in ["-wal", "-shm"] {
        reusable += file_size(Path::new(&format!("{}{}", db_path.display(), suffix)))?;
    }

    Ok(total.saturating_sub(reusable))
}

/// 清理完成后删除对应图片文件并通知前端刷新列表；没删到记录时什么都不做。
pub fn apply_outcome(app: &AppHandle, outcome: &CleanupOutcome, reason: &str) {
    if outcome.removed == 0 {
        return;
    }

    remove_images(app, &outcome.image_files);
    log::info!("{reason} cleanup removed {} item(s)", outcome.removed);
    if let Err(err) = app.emit(
        CLIPBOARD_UPDATED_EVENT,
        json!({ "cleanup": outcome.removed }),
    ) {
        log::warn!("emit cleanup event failed: {err}");
    }
}

/// 读取当前清理周期。`0` 表示关闭周期性清理。
fn cleanup_interval(app: &AppHandle) -> Option<Duration> {
    let store = app.try_state::<SettingsStore>()?;
    let hours = store.snapshot().clipboard.history.cleanup_interval_hours;

    if hours == 0 {
        return None;
    }

    Some(Duration::from_secs(u64::from(hours) * 60 * 60))
}

/// 删除被清理图片记录的落盘文件（原图 + 缩略图）。`ImageStore` 未注册或单个文件删除失败
/// 都只记日志、不阻断——清理本身已成功，残留文件最坏只是占用磁盘，不影响功能。
fn remove_images(app: &AppHandle, file_names: &[String]) {
    if file_names.is_empty() {
        return;
    }
    let Some(store) = app.try_state::<ImageStore>() else {
        log::warn!(
            "image store unavailable; skip removing {} image file(s)",
            file_names.len()
        );
        return;
    };
    for file_name in file_names {
        if let Err(err) = store.remove(file_name) {
            log::warn!("remove cleaned image {file_name} failed: {err}");
        }
    }
}

/// `Retention` → 绝对截止时间。`Forever` 或 `value == 0` 表示禁用。
/// 月份近似按 30 天处理（与前端展示口径一致，不引日历库）。
fn retention_cutoff(r: &Retention, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    if r.value == 0 {
        return None;
    }
    let dur = match r.unit {
        RetentionUnit::Forever => return None,
        RetentionUnit::Hours => ChronoDuration::hours(r.value as i64),
        RetentionUnit::Days => ChronoDuration::days(r.value as i64),
        RetentionUnit::Weeks => ChronoDuration::weeks(r.value as i64),
        RetentionUnit::Months => ChronoDuration::days((r.value as i64) * 30),
    };
    Some(now - dur)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> DateTime<Utc> {
        DateTime::from_timestamp(1_700_000_000, 0).unwrap()
    }

    #[test]
    fn retention_cutoff_returns_none_when_disabled() {
        assert!(retention_cutoff(
            &Retention {
                value: 0,
                unit: RetentionUnit::Days
            },
            now()
        )
        .is_none());
        assert!(retention_cutoff(
            &Retention {
                value: 7,
                unit: RetentionUnit::Forever
            },
            now()
        )
        .is_none());
    }

    #[test]
    fn retention_cutoff_subtracts_by_unit() {
        let n = now();
        assert_eq!(
            retention_cutoff(
                &Retention {
                    value: 2,
                    unit: RetentionUnit::Hours
                },
                n
            ),
            Some(n - ChronoDuration::hours(2))
        );
        assert_eq!(
            retention_cutoff(
                &Retention {
                    value: 3,
                    unit: RetentionUnit::Days
                },
                n
            ),
            Some(n - ChronoDuration::days(3))
        );
        assert_eq!(
            retention_cutoff(
                &Retention {
                    value: 1,
                    unit: RetentionUnit::Weeks
                },
                n
            ),
            Some(n - ChronoDuration::weeks(1))
        );
        assert_eq!(
            retention_cutoff(
                &Retention {
                    value: 1,
                    unit: RetentionUnit::Months
                },
                n
            ),
            Some(n - ChronoDuration::days(30))
        );
    }
}
