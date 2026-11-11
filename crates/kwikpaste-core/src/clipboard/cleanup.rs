//! 历史自动清理：按保留规则、条数上限与存储上限删除普通记录。
//!
//! 触发时机：启动时、设置变更后（[`request`]）、有新记录入库后（[`notify_inserted`]）防抖执行，
//! 另有每分钟一次的后台检查兜底按时间过期的记录。每轮都从 `SettingsStore` 取最新配置。
//! 存储占用要遍历数据目录，只在有新记录、设置变更或长时间没统计过时才统计。
//! 收藏、置顶、分组内和有备注的记录一律保留（由 [`crate::db::retention`] 保证）。
//!
//! 设置读盘时历史清理相关的字段有回落（[`crate::settings::SettingsStore::cleanup_paused`]），
//! 后台清理只统计存储占用，不删记录、不释放空闲页，直到用户显式保存一次历史设置；
//! 用户在偏好页手动执行的清理不受影响。

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Context;
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use serde::Serialize;
use sqlx::SqlitePool;
use tokio::sync::Notify;
use tokio::task::JoinHandle;

use crate::db::items::{reusable_page_bytes, CleanupOutcome};
use crate::db::retention::{
    count_protected, delete_expired, delete_least_recent_until, delete_over_count,
    release_free_pages, rule_match_stats, AgePlan, RuleMatchStats, RulePlan,
};
use crate::disk::dir_size_excluding;
use crate::error::Result;
use crate::events::CoreEvent;
use crate::root::CoreInner;
use crate::settings::{History, Retention, RetentionUnit, StorageLimitAction};

/// 后台检查间隔：按时间过期的记录最迟这么久后被清理。
const SCHEDULER_TICK: Duration = Duration::from_secs(60);
/// 设置变更、新记录入库后等这么久再清理，合并连续触发。
const REQUEST_DEBOUNCE: Duration = Duration::from_millis(1500);
/// 有新记录时两次存储统计之间的最短间隔。
const STORAGE_CHECK_MIN_INTERVAL: Duration = Duration::from_secs(2 * 60);
/// 没有新记录也定期重新统计一次存储占用，覆盖缩略图生成等目录外部变化。
const STORAGE_CHECK_MAX_INTERVAL: Duration = Duration::from_secs(30 * 60);

/// 自动清理调度状态，由 `Core` 持有。
#[derive(Default)]
pub struct CleanupScheduler {
    wake: Notify,
    pending: Mutex<Pending>,
    status: Mutex<CleanupStatus>,
    /// 图片从落盘到入库持共享锁；清理、删除与换库持独占锁，且都先于 upsert_lock 获取。
    running: Arc<tokio::sync::RwLock<()>>,
}

#[derive(Default)]
struct Pending {
    full: bool,
    count_dirty: bool,
    storage_dirty: bool,
    last_storage_check: Option<Instant>,
}

/// 本轮要执行的清理范围；按时间过期每轮都会检查。
#[derive(Debug, Clone, Copy)]
struct PassScope {
    count: bool,
    storage: bool,
}

impl PassScope {
    const FULL: Self = Self {
        count: true,
        storage: true,
    };
}

/// 偏好页与剪贴板窗口展示的清理状态。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CleanupStatus {
    /// 本次启动后最近一次删除了记录的清理。
    pub last_run: Option<CleanupReport>,
    /// 最近一次存储占用统计。
    pub storage: Option<StorageCheck>,
    /// 历史设置读盘时有回落，后台自动清理已暂停，等用户在偏好页保存一次历史设置。
    pub auto_cleanup_paused: bool,
}

/// 一轮清理的结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CleanupReport {
    pub finished_at: DateTime<Utc>,
    pub removed: u64,
    /// 释放的空间：文本按内容大小、图片按原图与缩略图文件估算。
    pub freed_bytes: u64,
    /// 其中超过保留时长的条数。
    pub expired: u64,
    /// 其中超过最大保留条数的条数。
    pub over_count: u64,
    /// 其中为回到存储上限以内删除的条数。
    pub over_storage: u64,
}

/// 一次存储占用统计。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StorageCheck {
    pub used_bytes: u64,
    pub limit_bytes: u64,
    pub over_limit: bool,
    /// 设为自动清理但删光普通记录也回不到上限以内：占用主要来自受保护的记录或缓存。
    pub cleanup_blocked: bool,
}

/// 按一份候选设置预演清理的结果，偏好页保存前据此确认，并展示每条规则的匹配情况。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CleanupPreview {
    /// 按这份设置立即清理会删除的条数。
    pub removed: u64,
    pub freed_bytes: u64,
    pub expired: u64,
    pub over_count: u64,
    pub over_storage: u64,
    pub storage_blocked: bool,
    /// 受保护、不会被自动清理的记录条数。
    pub protected: u64,
    /// 已启用的规则逐条统计；停用的规则不参与匹配，不在列表里。
    pub rules: Vec<RulePreview>,
    /// 没有命中任何规则、按默认保留时长处理的记录。
    pub fallback: RulePreview,
}

/// 一条规则当前匹配的普通记录数，以及其中已超过保留时长的条数。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RulePreview {
    pub id: String,
    pub matched: u64,
    pub expired: u64,
}

impl RulePreview {
    fn new(id: &str, stats: RuleMatchStats) -> Self {
        Self {
            id: id.to_owned(),
            matched: stats.matched,
            expired: stats.expired,
        }
    }
}

/// 在 core runtime 上启动后台清理任务：启动后立即完整清理一次。
///
/// 任务只持有 `Core` 的弱引用，宿主丢掉所有 `Core` 后最迟一个检查周期就退出。
pub(crate) fn spawn(core: &Arc<CoreInner>) -> JoinHandle<()> {
    core.cleanup.pending().full = true;
    let weak = Arc::downgrade(core);

    core.rt.spawn(async move {
        loop {
            let Some(core) = weak.upgrade() else {
                return;
            };
            run_due(&core).await;

            let woke = tokio::time::timeout(SCHEDULER_TICK, core.cleanup.wake.notified())
                .await
                .is_ok();
            if woke {
                tokio::time::sleep(REQUEST_DEBOUNCE).await;
            }
        }
    })
}

/// 清理设置变化后请求尽快完整清理一次。
pub(crate) fn request(core: &CoreInner) {
    core.cleanup.pending().full = true;
    core.cleanup.wake.notify_one();
}

/// 有新记录入库：尽快检查条数上限，存储占用按节流间隔统计。
pub(crate) fn notify_inserted(core: &CoreInner) {
    {
        let mut pending = core.cleanup.pending();
        pending.count_dirty = true;
        pending.storage_dirty = true;
    }
    core.cleanup.wake.notify_one();
}

/// 当前清理状态快照；是否暂停按当前设置状态实时给出。
pub(crate) fn status(core: &CoreInner) -> CleanupStatus {
    let mut status = core.cleanup.status().clone();
    status.auto_cleanup_paused = core.settings.cleanup_paused();
    status
}

/// 立即按当前设置完整清理一次，返回本轮结果（没有删除记录时 `removed = 0`）。
/// 这是用户的显式操作，自动清理暂停时也照常执行。
pub(crate) async fn run_now(core: &CoreInner) -> Result<CleanupReport> {
    {
        let mut pending = core.cleanup.pending();
        pending.storage_dirty = false;
        pending.last_storage_check = Some(Instant::now());
    }

    let _ocr = core.ocr.suspend().await;
    let _running = core.cleanup.exclusive().await;
    let paused = core.settings.cleanup_paused();
    let (report, storage) = execute(core, PassScope::FULL, false).await?;
    core.cleanup.record(core, &report, storage, paused);
    Ok(report)
}

/// 按候选设置在事务里预演一轮完整清理后回滚，不删除任何记录或文件。
pub(crate) async fn preview(core: &CoreInner, history: &History) -> Result<CleanupPreview> {
    let _running = core.cleanup.exclusive().await;
    let pool = core.db.pool().await;
    let rules = enabled_rules(history).collect::<Vec<_>>();
    let plan = age_plan(history, Utc::now());
    let image_bytes = |file_name: &str| stored_image_bytes(core, file_name);

    let mut tx = pool
        .begin()
        .await
        .context("failed to begin history cleanup")?;
    let (per_rule, fallback) = rule_match_stats(&mut tx, &plan).await?;
    let protected = count_protected(&mut tx).await?;
    let expired = delete_expired(&mut tx, &plan).await?;
    let over_count = delete_over_count(&mut tx, history.max_count).await?;

    let mut preview = CleanupPreview {
        expired: expired.removed,
        over_count: over_count.removed,
        protected,
        rules: rules
            .iter()
            .zip(per_rule)
            .map(|(rule, stats)| RulePreview::new(&rule.id, stats))
            .collect(),
        fallback: RulePreview::new("", fallback),
        ..CleanupPreview::default()
    };
    let mut outcome = expired;
    outcome.merge(over_count);
    let mut freed = estimated_bytes(&outcome, image_bytes);

    if history.storage_limit_action == StorageLimitAction::Cleanup {
        // 事务外的连接看不到预演删除，统计的是当前占用，扣掉前两步预计释放的部分再比较。
        let used = storage_bytes_in_use(core, &pool)
            .await?
            .saturating_sub(freed);
        let limit = history.storage_limit_bytes();
        if used > limit {
            let over = delete_least_recent_until(&mut tx, used - limit, image_bytes).await?;
            preview.storage_blocked = over.removed == 0;
            preview.over_storage = over.removed;
            freed += estimated_bytes(&over, image_bytes);
            outcome.merge(over);
        }
    }
    tx.rollback()
        .await
        .context("failed to roll back cleanup preview")?;

    preview.removed = outcome.removed;
    preview.freed_bytes = freed;
    Ok(preview)
}

/// 数据实际占用：数据目录总大小（不含 WAL / SHM 旁路文件）减去 SQLite 可复用的空闲页。
/// 偏好页展示与存储上限清理共用这一口径——删行后数据库文件不一定立即缩小，
/// 按目录原始大小判断会让下一轮把已释放的空间再算一遍而继续误删，侧栏也会一直显示超限。
///
/// 旁路文件在遍历时直接跳过，而不是先加后减：Windows 上目录枚举拿到的是打开中文件的旧大小，
/// 单独查到的是最新大小，先加旧值再减新值会把占用算少（WAL 刚写满时能少掉好几 MB）。
pub(crate) async fn storage_bytes_in_use(core: &CoreInner, pool: &SqlitePool) -> Result<u64> {
    let db_path = crate::db::db_path(&core.paths)?;
    let sidecars = ["-wal", "-shm"].map(|suffix| sidecar(&db_path, suffix));
    let app_data_dir = core.paths.app_data_dir()?;
    // 遍历整个数据目录是同步 I/O，放到阻塞线程池，不占 core runtime 的工作线程。
    let total = tokio::task::spawn_blocking(move || dir_size_excluding(&app_data_dir, &sidecars))
        .await
        .map_err(|err| anyhow::anyhow!("storage size task failed: {err}"))??;

    Ok(total.saturating_sub(reusable_page_bytes(pool).await?))
}

fn sidecar(db_path: &Path, suffix: &str) -> PathBuf {
    PathBuf::from(format!("{}{suffix}", db_path.display()))
}

/// 按范围删除记录后删掉对应图片文件并通知宿主刷新列表；没删到记录时什么都不做。
pub(crate) async fn apply_outcome(core: &CoreInner, outcome: &CleanupOutcome, reason: &str) {
    if outcome.removed == 0 {
        return;
    }

    remove_files(core, outcome).await;
    log::info!("{reason} cleanup removed {} item(s)", outcome.removed);
    emit_cleanup(core, outcome.removed);
}

impl CleanupScheduler {
    /// 跨图片落盘与异步入库持有；拿到此锁之后才能获取 upsert_lock。
    pub(crate) async fn image_publication(&self) -> tokio::sync::OwnedRwLockReadGuard<()> {
        self.running.clone().read_owned().await
    }

    /// 独占清理锁：持有期间后台清理、手动清理与预演都会等待。VACUUM、切换存储位置、
    /// 覆盖导入备份时持有，避免和清理同时改数据库与资源文件。
    pub(crate) async fn exclusive(&self) -> tokio::sync::RwLockWriteGuard<'_, ()> {
        self.running.write().await
    }

    fn pending(&self) -> std::sync::MutexGuard<'_, Pending> {
        self.pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn status(&self) -> std::sync::MutexGuard<'_, CleanupStatus> {
        self.status
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// 取出本轮到期的清理范围，并把存储统计时间记到现在。
    fn take_due(&self) -> PassScope {
        let mut pending = self.pending();
        let full = std::mem::take(&mut pending.full);
        let count = full || std::mem::take(&mut pending.count_dirty);
        let storage = full
            || pending.last_storage_check.is_none_or(|checked_at| {
                let elapsed = checked_at.elapsed();
                elapsed >= STORAGE_CHECK_MAX_INTERVAL
                    || (pending.storage_dirty && elapsed >= STORAGE_CHECK_MIN_INTERVAL)
            });

        if storage {
            pending.storage_dirty = false;
            pending.last_storage_check = Some(Instant::now());
        }

        PassScope { count, storage }
    }

    /// 记下本轮结果，状态有变化时通知宿主。
    fn record(
        &self,
        core: &CoreInner,
        report: &CleanupReport,
        storage: Option<StorageCheck>,
        paused: bool,
    ) {
        let next = {
            let mut status = self.status();
            let before = status.clone();
            if report.removed > 0 {
                status.last_run = Some(report.clone());
            }
            if let Some(check) = storage {
                status.storage = Some(check);
            }
            status.auto_cleanup_paused = paused;
            if *status == before {
                return;
            }
            status.clone()
        };

        core.events.emit(CoreEvent::CleanupStatus(next));
    }
}

/// 后台跑一轮到期的清理；失败只记日志，下一轮再试。
pub(crate) async fn run_due(core: &CoreInner) {
    let scope = core.cleanup.take_due();
    let _ocr = core.ocr.suspend().await;
    let _running = core.cleanup.exclusive().await;
    let paused = core.settings.cleanup_paused();

    match execute(core, scope, paused).await {
        Ok((report, storage)) => core.cleanup.record(core, &report, storage, paused),
        Err(err) => log::warn!("history cleanup failed: {err}"),
    }
}

/// 执行一轮清理：先按时间与条数，再按存储上限。每一步单独提交，删掉的图片文件随后移除。
/// `paused` 时只统计存储占用，什么都不删。
async fn execute(
    core: &CoreInner,
    scope: PassScope,
    paused: bool,
) -> Result<(CleanupReport, Option<StorageCheck>)> {
    let history = core.settings.snapshot().clipboard.history;
    let pool = core.db.pool().await;

    if paused {
        log::debug!("history cleanup paused until history settings are saved");
        let storage = if scope.storage {
            let (check, _, _) = enforce_storage_limit(core, &pool, &history, false).await?;
            Some(check)
        } else {
            None
        };
        let report = CleanupReport {
            finished_at: Utc::now(),
            removed: 0,
            freed_bytes: 0,
            expired: 0,
            over_count: 0,
            over_storage: 0,
        };
        return Ok((report, storage));
    }

    let plan = age_plan(&history, Utc::now());

    let mut tx = pool
        .begin()
        .await
        .context("failed to begin history cleanup")?;
    let expired = delete_expired(&mut tx, &plan).await?;
    let over_count = if scope.count {
        delete_over_count(&mut tx, history.max_count).await?
    } else {
        CleanupOutcome::default()
    };
    tx.commit()
        .await
        .context("failed to commit history cleanup")?;

    let mut report = CleanupReport {
        finished_at: Utc::now(),
        removed: 0,
        freed_bytes: 0,
        expired: expired.removed,
        over_count: over_count.removed,
        over_storage: 0,
    };
    let mut outcome = expired;
    outcome.merge(over_count);
    report.freed_bytes += remove_files(core, &outcome).await;
    shrink_database(&pool, &outcome).await;

    let storage = if scope.storage {
        let (check, over, freed) = match enforce_storage_limit(core, &pool, &history, true).await {
            Ok(result) => result,
            Err(err) => {
                // 前两步已提交，界面仍要刷新，否则会残留已删除的卡片。
                if outcome.removed > 0 {
                    emit_cleanup(core, outcome.removed);
                }
                return Err(err);
            }
        };
        report.over_storage = over.removed;
        report.freed_bytes += freed;
        outcome.merge(over);
        Some(check)
    } else {
        None
    };

    report.removed = outcome.removed;
    report.finished_at = Utc::now();
    if report.removed > 0 {
        log::info!(
            "history cleanup removed {} item(s): {} expired, {} over count, {} over storage",
            report.removed,
            report.expired,
            report.over_count,
            report.over_storage
        );
        emit_cleanup(core, report.removed);
    }

    Ok((report, storage))
}

/// 统计存储占用；设为自动清理、允许删除且超出上限时，从最久没用的普通记录删起，直到回到上限以内。
/// 返回统计结果、删除结果与释放空间估算。
async fn enforce_storage_limit(
    core: &CoreInner,
    pool: &SqlitePool,
    history: &History,
    allow_delete: bool,
) -> Result<(StorageCheck, CleanupOutcome, u64)> {
    let limit = history.storage_limit_bytes();
    let mut used = storage_bytes_in_use(core, pool).await?;
    let mut over = CleanupOutcome::default();
    let mut freed = 0;
    let mut cleanup_blocked = false;

    if allow_delete && used > limit && history.storage_limit_action == StorageLimitAction::Cleanup {
        let image_bytes = |file_name: &str| stored_image_bytes(core, file_name);
        let mut tx = pool
            .begin()
            .await
            .context("failed to begin history cleanup")?;
        over = delete_least_recent_until(&mut tx, used - limit, image_bytes).await?;
        tx.commit()
            .await
            .context("failed to commit history cleanup")?;

        if over.removed == 0 {
            // 每次统计都会走到这里，只记 debug，避免超限期间刷满日志文件。
            log::debug!("storage stays over the limit: history cleanup cannot free enough space");
            cleanup_blocked = true;
        } else {
            // 图片文件要在重新统计前删掉，统计结果才准。
            freed = remove_files(core, &over).await;
            shrink_database(pool, &over).await;
            used = storage_bytes_in_use(core, pool).await?;
        }
    }

    let check = StorageCheck {
        used_bytes: used,
        limit_bytes: limit,
        over_limit: used > limit,
        cleanup_blocked,
    };
    Ok((check, over, freed))
}

/// 删行后把 SQLite 空闲页还给文件系统；失败不影响清理结果。
async fn shrink_database(pool: &SqlitePool, outcome: &CleanupOutcome) {
    if outcome.removed == 0 {
        return;
    }
    if let Err(err) = release_free_pages(pool).await {
        log::warn!("release sqlite free pages after cleanup failed: {err}");
    }
}

/// 设置 → 按时间清理计划：只取启用的规则，截止时间按当前时刻换算。
fn age_plan(history: &History, now: DateTime<Utc>) -> AgePlan {
    AgePlan {
        rules: enabled_rules(history)
            .map(|rule| RulePlan {
                categories: rule.categories.clone(),
                min_size_bytes: (rule.min_size_kb > 0).then(|| u64::from(rule.min_size_kb) * 1024),
                source_app_ids: rule.source_app_ids.clone(),
                sensitive_only: rule.sensitive_only,
                unused_only: rule.unused_only,
                cutoff: retention_cutoff(&rule.keep, now),
            })
            .collect(),
        fallback_cutoff: retention_cutoff(&history.retention, now),
    }
}

fn enabled_rules(history: &History) -> impl Iterator<Item = &crate::settings::RetentionRule> {
    history.rules.iter().filter(|rule| rule.enabled)
}

/// 被删记录预计释放的空间：非图片按内容大小，图片按落盘文件。
fn estimated_bytes(outcome: &CleanupOutcome, image_bytes: impl Fn(&str) -> u64) -> u64 {
    outcome.content_bytes
        + outcome
            .image_files
            .iter()
            .map(|file_name| image_bytes(file_name))
            .sum::<u64>()
}

fn stored_image_bytes(core: &CoreInner, file_name: &str) -> u64 {
    core.images.stored_bytes(file_name)
}

/// 删除被清理图片记录的落盘文件（原图 + 缩略图），返回本批记录释放的空间估算。
/// 单个文件删除失败只记日志、不阻断——清理本身已成功，残留文件最坏只是占用磁盘。
/// 逐个文件的 metadata 与删除是同步 I/O，放到阻塞线程池执行。
async fn remove_files(core: &CoreInner, outcome: &CleanupOutcome) -> u64 {
    let images = core.images.clone();
    let files = outcome.image_files.clone();
    let content_bytes = outcome.content_bytes;

    let removed = tokio::task::spawn_blocking(move || {
        let mut freed = content_bytes;
        for file_name in &files {
            freed += images.stored_bytes(file_name);
            if let Err(err) = images.remove(file_name) {
                log::warn!("remove cleaned image {file_name} failed: {err}");
            }
        }
        freed
    })
    .await;
    removed.unwrap_or_else(|err| {
        log::warn!("remove cleaned images task failed: {err}");
        content_bytes
    })
}

/// 通知宿主列表按清理结果刷新。
fn emit_cleanup(core: &CoreInner, removed: u64) {
    core.events.emit(CoreEvent::ClipboardCleaned { removed });
}

/// `Retention` → 绝对截止时间。`Forever` 或 `value == 0` 表示不按时间清理。
/// 月份近似按 30 天处理（与前端展示口径一致，不引日历库）。
/// 设置文件或导入的设置里可能有离谱的数值：时长或截止时间超出 chrono 的范围时同样不按时间清理。
fn retention_cutoff(retention: &Retention, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    if retention.value == 0 {
        return None;
    }
    let value = i64::from(retention.value);
    let duration = match retention.unit {
        RetentionUnit::Forever => return None,
        RetentionUnit::Minutes => ChronoDuration::try_minutes(value),
        RetentionUnit::Hours => ChronoDuration::try_hours(value),
        RetentionUnit::Days => ChronoDuration::try_days(value),
        RetentionUnit::Weeks => ChronoDuration::try_weeks(value),
        RetentionUnit::Months => ChronoDuration::try_days(value * 30),
    }?;
    now.checked_sub_signed(duration)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::overview::ContentCategory;
    use crate::settings::RetentionRule;

    fn now() -> DateTime<Utc> {
        DateTime::from_timestamp(1_700_000_000, 0).unwrap()
    }

    fn retention(value: u32, unit: RetentionUnit) -> Retention {
        Retention { value, unit }
    }

    #[test]
    fn retention_cutoff_returns_none_when_disabled() {
        assert!(retention_cutoff(&retention(0, RetentionUnit::Days), now()).is_none());
        assert!(retention_cutoff(&retention(7, RetentionUnit::Forever), now()).is_none());
    }

    #[test]
    fn retention_cutoff_out_of_range_never_expires() {
        for unit in [
            RetentionUnit::Hours,
            RetentionUnit::Days,
            RetentionUnit::Weeks,
            RetentionUnit::Months,
        ] {
            assert_eq!(retention_cutoff(&retention(u32::MAX, unit), now()), None);
        }
        assert!(retention_cutoff(&retention(u32::MAX, RetentionUnit::Minutes), now()).is_some());
    }

    #[test]
    fn retention_cutoff_subtracts_by_unit() {
        let n = now();
        let cases = [
            (
                retention(30, RetentionUnit::Minutes),
                ChronoDuration::minutes(30),
            ),
            (retention(2, RetentionUnit::Hours), ChronoDuration::hours(2)),
            (retention(3, RetentionUnit::Days), ChronoDuration::days(3)),
            (retention(1, RetentionUnit::Weeks), ChronoDuration::weeks(1)),
            (
                retention(1, RetentionUnit::Months),
                ChronoDuration::days(30),
            ),
        ];
        for (retention, duration) in cases {
            assert_eq!(retention_cutoff(&retention, n), Some(n - duration));
        }
    }

    #[test]
    fn age_plan_keeps_enabled_rules_in_order() {
        let history = History {
            retention: retention(30, RetentionUnit::Days),
            rules: vec![
                RetentionRule {
                    id: "images".to_owned(),
                    categories: vec![ContentCategory::Image],
                    min_size_kb: 2048,
                    keep: retention(3, RetentionUnit::Days),
                    ..RetentionRule::default()
                },
                RetentionRule {
                    id: "off".to_owned(),
                    enabled: false,
                    keep: retention(1, RetentionUnit::Hours),
                    ..RetentionRule::default()
                },
                RetentionRule {
                    id: "wechat".to_owned(),
                    source_app_ids: vec!["wechat".to_owned()],
                    sensitive_only: true,
                    unused_only: true,
                    keep: retention(0, RetentionUnit::Forever),
                    ..RetentionRule::default()
                },
            ],
            ..History::default()
        };

        let plan = age_plan(&history, now());

        assert_eq!(
            plan,
            AgePlan {
                rules: vec![
                    RulePlan {
                        categories: vec![ContentCategory::Image],
                        min_size_bytes: Some(2048 * 1024),
                        cutoff: Some(now() - ChronoDuration::days(3)),
                        ..RulePlan::default()
                    },
                    RulePlan {
                        source_app_ids: vec!["wechat".to_owned()],
                        sensitive_only: true,
                        unused_only: true,
                        cutoff: None,
                        ..RulePlan::default()
                    },
                ],
                fallback_cutoff: Some(now() - ChronoDuration::days(30)),
            }
        );
    }

    #[test]
    fn inserts_throttle_storage_checks_but_settings_changes_force_them() {
        let scheduler = CleanupScheduler::default();
        let first = scheduler.take_due();
        assert!(first.storage, "never measured yet");

        scheduler.pending().count_dirty = true;
        scheduler.pending().storage_dirty = true;
        let after_insert = scheduler.take_due();
        assert!(after_insert.count);
        assert!(!after_insert.storage, "measured just now");
        assert!(scheduler.pending().storage_dirty, "kept for the next check");

        scheduler.pending().full = true;
        let forced = scheduler.take_due();
        assert!(forced.count && forced.storage);
        assert!(!scheduler.pending().storage_dirty);

        let idle = scheduler.take_due();
        assert!(!idle.count && !idle.storage);
    }
}
