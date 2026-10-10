//! 偏好页「存储」：数据目录位置与切换、存储占用与数据概览、清理资源缓存并压缩数据库。
//!
//! 切换存储位置时：暂停采集 → 等在途入库与清理结束 → 关闭连接池 → 复制数据 → 改 manifest →
//! 在新位置重开数据库 → 各存储跟着 `rebase` → 删除旧数据。任何一步失败都退回原位置重开数据库。
//! 打开目录交给宿主（[`Core::preference_directory`] 只给路径）。

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::Context;
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

use crate::clipboard;
use crate::db::overview::HistoryOverview;
use crate::disk::{available_space, dir_size, file_size};
use crate::error::{AppError, Result};
use crate::events::CoreEvent;
use crate::i18n::commands::{label, Key};
use crate::paths::{CorePaths, StorageLocation};
use crate::root::{Core, CoreInner};
use crate::settings::{Settings, SettingsDelta};

const STORAGE_CONTENT_DIRS: [&str; 4] = ["db", "resources", "config", "state"];
const CUSTOM_STORAGE_CONTAINER_DIR: &str = "KwikPasteData";
const CLIPBOARD_IMAGES_DIR: &str = "clipboard-images";
const APP_ICONS_DIR: &str = "app-icons";
const FILE_ICONS_DIR: &str = "file-icons";
/// 数据迁出默认位置时启动锚点里要留下的条目：manifest 与本机同步身份目录。
const BOOTSTRAP_KEEP: [&str; 2] = ["storage.json", "sync"];

/// 偏好页侧栏展示的本地存储占用概览。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StorageUsage {
    /// 数据实际占用，与存储上限清理的计量口径一致（不含 SQLite 可复用空间）。
    pub total_bytes: u64,
    pub database_bytes: u64,
    pub resources_bytes: u64,
    pub settings_bytes: u64,
}

/// 清理本地资源缓存后的结果，用于展示与刷新侧栏存储占用。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CleanCacheResult {
    pub removed_files: u64,
    pub removed_bytes: u64,
    /// 压缩数据库文件缩小的字节数。
    pub compacted_bytes: u64,
    pub storage_usage: StorageUsage,
}

/// 数据实际占用按来源拆分，各项之和等于 [`StorageUsage::total_bytes`]。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StorageBreakdown {
    /// 数据库主文件扣除 SQLite 可复用空闲页后的部分。
    pub database_bytes: u64,
    /// 图片原图与缩略图。
    pub image_bytes: u64,
    /// 来源应用图标与文件类型图标缓存。
    pub icon_bytes: u64,
    /// 设置、窗口状态等其余文件。
    pub other_bytes: u64,
}

/// 资源目录里已不被任何记录引用、可以安全清理的缓存文件。
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReclaimableCache {
    pub files: u64,
    pub bytes: u64,
}

/// 偏好页「数据概览」：存储占用拆分、可清理缓存和历史记录的多维统计。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StorageOverview {
    pub usage: StorageUsage,
    pub breakdown: StorageBreakdown,
    pub reclaimable: ReclaimableCache,
    pub history: HistoryOverview,
}

/// 更改或还原数据目录后的刷新结果。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangeStorageLocationResult {
    pub location: StorageLocation,
    pub storage_usage: StorageUsage,
}

/// 偏好页允许打开的固定本地目录。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PreferenceDirectory {
    Data,
    Logs,
}

impl Core {
    /// 当前真实数据目录位置。
    pub fn storage_location(&self) -> Result<StorageLocation> {
        self.0.paths.storage_location()
    }

    /// 偏好页「打开目录」的目标路径；目录不存在时先建出来，宿主拿到后交给文件管理器打开。
    pub fn preference_directory(&self, target: PreferenceDirectory) -> Result<PathBuf> {
        let path = match target {
            PreferenceDirectory::Data => self.0.paths.app_data_dir()?,
            PreferenceDirectory::Logs => self.0.paths.log_dir(),
        };
        fs::create_dir_all(&path)
            .with_context(|| format!("failed to create directory {path:?}"))?;
        Ok(path)
    }

    /// 当前数据目录的存储占用，拆出数据库、资源与设置文件。
    pub async fn storage_usage(&self) -> Result<StorageUsage> {
        let core = self.clone();
        self.hop(async move { storage_usage(&core.0).await }).await
    }

    /// 偏好页数据概览：占用拆分、可清理缓存，以及按类别 / 来源 / 分组 / 日期的记录统计。
    pub async fn storage_overview(&self) -> Result<StorageOverview> {
        let core = self.clone();
        self.hop(async move {
            let inner = &core.0;
            let pool = inner.db.pool().await;
            let usage = storage_usage(inner).await?;
            let breakdown = storage_breakdown(inner, &pool, usage.total_bytes).await?;
            let reclaimable = sweep_resource_cache(inner, &pool, CacheSweep::Measure).await?;
            let mut history = crate::db::overview::load_history_overview(
                &pool,
                chrono::Utc::now(),
                &chrono::Local,
            )
            .await?;

            for source_app in &mut history.source_apps {
                source_app.icon_path = source_app.icon_file.as_deref().and_then(|file_name| {
                    inner
                        .app_icons
                        .icon_path(file_name)
                        .to_str()
                        .map(str::to_owned)
                });
            }

            Ok(StorageOverview {
                usage,
                breakdown,
                reclaimable: ReclaimableCache {
                    files: reclaimable.files,
                    bytes: reclaimable.bytes,
                },
                history,
            })
        })
        .await
    }

    /// 把数据目录迁到用户选择的父目录下（实际是其中的 `KwikPasteData/<env>`），并热切换各存储。
    /// 成功后发 [`CoreEvent::SettingsUpdated`]（整份替换）与 [`CoreEvent::ClipboardReloaded`]。
    pub async fn change_storage_location(
        &self,
        target_parent_dir: PathBuf,
    ) -> Result<ChangeStorageLocationResult> {
        let core = self.clone();
        self.hop(async move {
            ensure_storage_relocatable(&core.0)?;
            let target = core.0.paths.custom_data_dir(&target_parent_dir);
            switch_storage_location(&core.0, target).await
        })
        .await
    }

    /// 把数据目录迁回默认位置，并热切换各存储。
    pub async fn reset_storage_location(&self) -> Result<ChangeStorageLocationResult> {
        let core = self.clone();
        self.hop(async move {
            ensure_storage_relocatable(&core.0)?;
            let target = core.0.paths.default_data_dir();
            switch_storage_location(&core.0, target).await
        })
        .await
    }

    /// 删除路径已全部不在磁盘上的文件记录（收藏与置顶保留），再删除资源目录中不再被记录或
    /// 资源索引引用的文件，最后整库 VACUUM 压缩数据库。
    ///
    /// 历史设置回落、自动清理暂停时也照常执行；与自动清理互斥，正在清理时等它结束再开始。
    pub async fn clean_resource_cache(&self) -> Result<CleanCacheResult> {
        let core = self.clone();
        self.hop(async move {
            let inner = &core.0;
            let removed;
            let compacted_bytes;
            {
                let _exclusive = inner.cleanup.exclusive().await;
                let pool = inner.db.pool().await;
                remove_missing_file_items(inner, &pool).await?;
                removed = sweep_resource_cache(inner, &pool, CacheSweep::Delete).await?;

                let before = database_bytes(&inner.paths)?;
                crate::db::retention::compact(&pool).await?;
                compacted_bytes = before.saturating_sub(database_bytes(&inner.paths)?);
            }

            Ok(CleanCacheResult {
                removed_files: removed.files,
                removed_bytes: removed.bytes,
                compacted_bytes,
                storage_usage: storage_usage(inner).await?,
            })
        })
        .await
    }
}

/// 统计当前数据目录的存储占用。
pub(crate) async fn storage_usage(core: &CoreInner) -> Result<StorageUsage> {
    let pool = core.db.pool().await;
    let total_bytes = clipboard::cleanup::storage_bytes_in_use(core, &pool).await?;

    Ok(StorageUsage {
        total_bytes,
        database_bytes: database_bytes(&core.paths)?,
        resources_bytes: dir_size(&core.paths.resources_dir()?)?,
        settings_bytes: file_size(&core.paths.config_dir()?.join("settings.json"))?,
    })
}

/// 便携版的数据根固定在 exe 旁，偏好页不提供迁移入口，这里兜住绕过界面的调用。
///
/// 自定义目录启动时不可用、本次临时用默认目录时也不能迁移：此时改 manifest 会丢掉原位置，
/// 再迁回原位置会用临时数据覆盖那里的真实历史。
fn ensure_storage_relocatable(core: &CoreInner) -> Result<()> {
    if core.paths.is_portable() {
        return Err(anyhow::anyhow!(label(core.language(), Key::PortableStorageFixed)).into());
    }
    if core
        .paths
        .storage_location()?
        .unavailable_custom_path
        .is_some()
    {
        return Err(anyhow::anyhow!(label(core.language(), Key::StorageCustomUnavailable)).into());
    }

    Ok(())
}

async fn switch_storage_location(
    core: &CoreInner,
    target: PathBuf,
) -> Result<ChangeStorageLocationResult> {
    let paths = &core.paths;
    let current = paths.app_data_dir()?;
    if current == target {
        return location_result(core).await;
    }

    reject_nested_storage_move(&current, &target)?;
    let target_preexisting = target.exists();
    if target != paths.default_data_dir() {
        paths.validate_storage_target(&target)?;
        if paths.storage_target_has_data(&target)? {
            return Err(anyhow::anyhow!(label(core.language(), Key::StorageTargetHasData)).into());
        }
    }
    let bytes_to_copy = storage_bytes_to_copy(&current)?;
    let available = available_space(&target).map_err(|error| {
        anyhow::anyhow!(
            "{}: {error}",
            label(core.language(), Key::StorageSpaceUnavailable)
        )
    })?;
    if available < bytes_to_copy {
        return Err(anyhow::anyhow!(label(core.language(), Key::StorageInsufficientSpace)).into());
    }

    {
        // 先停新的采集，再等在途的入库与清理结束，复制期间没有人写库或删图片文件。
        let _pause = core.watcher_pause.pause_scoped();
        let _ocr = core.ocr.suspend().await;
        let _upsert = core.upsert_lock.lock().await;
        let _exclusive = core.cleanup.exclusive().await;
        let switch_error = Mutex::new(None::<AppError>);

        core.db
            .close_and_replace(|| async {
                let switched = copy_storage_data(paths, &current, &target)
                    .and_then(|()| paths.set_app_data_dir(target.clone()));
                let opened = match switched {
                    Ok(()) => crate::db::init(paths, core.db_max_connections).await,
                    Err(err) => Err(err),
                };

                match opened {
                    Ok(pool) => Ok(pool),
                    Err(err) => {
                        *lock(&switch_error) = Some(err);
                        // manifest 没能指回原位置时不能删目标：下次启动还要从那里读。
                        paths.set_app_data_dir(current.clone())?;
                        if let Err(cleanup) = cleanup_partial_target(&target, target_preexisting) {
                            log::warn!(
                                "failed to clean partial storage target {target:?}: {cleanup:#}"
                            );
                        }
                        crate::db::init(paths, core.db_max_connections).await
                    }
                }
            })
            .await?;

        if let Some(err) = lock(&switch_error).take() {
            return Err(err);
        }

        let settings = rebase_storage_states(core).await?;
        crate::sync::settings_changed(core);
        if let Err(error) = remove_old_storage_data(paths, &current) {
            log::warn!("old storage cleanup failed; keeping completed switch: {error:#}");
        }
        core.events.emit(CoreEvent::SettingsUpdated {
            settings: Arc::new(settings),
            delta: SettingsDelta::replaced(),
        });
        core.events.emit(CoreEvent::ClipboardReloaded);
    }

    location_result(core).await
}

async fn location_result(core: &CoreInner) -> Result<ChangeStorageLocationResult> {
    Ok(ChangeStorageLocationResult {
        location: core.paths.storage_location()?,
        storage_usage: storage_usage(core).await?,
    })
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn reject_nested_storage_move(current: &Path, target: &Path) -> Result<()> {
    if target.starts_with(current) || current.starts_with(target) {
        return Err(anyhow::anyhow!("新旧数据目录不能互相包含").into());
    }

    Ok(())
}

fn storage_bytes_to_copy(current: &Path) -> Result<u64> {
    let mut total: u64 = 0;
    for name in STORAGE_CONTENT_DIRS {
        total = total.saturating_add(dir_size(&current.join(name))?);
    }
    Ok(total)
}

/// 回滚复制失败时只移除本次迁移写入的目标内容；预先存在的空目录与 identity 保留。
fn cleanup_partial_target(target: &Path, preexisting: bool) -> Result<()> {
    if !preexisting {
        return remove_path(target);
    }
    for name in STORAGE_CONTENT_DIRS {
        let path = target.join(name);
        if path.exists() {
            remove_path(&path)?;
        }
    }
    Ok(())
}

fn copy_storage_data(paths: &CorePaths, src: &Path, dst: &Path) -> Result<()> {
    fs::create_dir_all(dst).with_context(|| format!("failed to create storage dir {dst:?}"))?;
    for name in STORAGE_CONTENT_DIRS {
        replace_path(&src.join(name), &dst.join(name))?;
    }
    let identity = dst.join(".kwikpaste-storage.json");
    if !identity.exists() {
        paths.write_storage_identity(dst)?;
    }
    Ok(())
}

/// 数据库已在新位置打开后，设置、窗口状态、图片与图标存储改到新目录，来源应用缓存从新库重建。
async fn rebase_storage_states(core: &CoreInner) -> Result<Settings> {
    let settings = core.settings.rebase(&core.paths)?;
    core.window_state.rebase(&core.paths)?;
    core.images.rebase(&core.paths)?;
    core.app_icons.rebase(&core.paths)?;
    core.file_icons.rebase(&core.paths)?;
    core.apps.load_from_db(core).await?;

    Ok(settings)
}

fn remove_old_storage_data(paths: &CorePaths, old: &Path) -> Result<()> {
    if old == paths.default_data_dir() {
        return remove_bootstrap_storage_payload(old);
    }

    remove_custom_storage_root(old)
}

fn remove_custom_storage_root(old: &Path) -> Result<()> {
    if old.exists() {
        fs::remove_dir_all(old)
            .with_context(|| format!("failed to remove old data dir {old:?}"))?;
    }

    let Some(container) = old.parent() else {
        return Ok(());
    };
    if container.file_name().and_then(|name| name.to_str()) != Some(CUSTOM_STORAGE_CONTAINER_DIR) {
        return Ok(());
    }

    match fs::remove_dir(container) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::DirectoryNotEmpty => Ok(()),
        Err(err) => {
            Err(anyhow::anyhow!("failed to remove old data container {container:?}: {err}").into())
        }
    }
}

/// 默认数据根同时是启动锚点：只删数据，保留 `storage.json` 和不随数据搬走的 `sync/`
/// （本机同步身份与已配对设备，见 [`CorePaths::sync_dir`]）。
fn remove_bootstrap_storage_payload(dir: &Path) -> Result<()> {
    if !dir.exists() {
        return Ok(());
    }

    for entry in
        fs::read_dir(dir).with_context(|| format!("failed to read old data dir {dir:?}"))?
    {
        let entry = entry.with_context(|| format!("failed to read entry under {dir:?}"))?;
        if BOOTSTRAP_KEEP.iter().any(|name| entry.file_name() == *name) {
            continue;
        }

        remove_path(&entry.path())?;
    }

    Ok(())
}

fn replace_path(src: &Path, dst: &Path) -> Result<()> {
    if dst.exists() {
        remove_path(dst)?;
    }

    if !src.exists() {
        return Ok(());
    }

    if src.is_dir() {
        return copy_dir_all(src, dst);
    }

    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create parent dir {parent:?}"))?;
    }
    fs::copy(src, dst).with_context(|| format!("failed to copy {src:?} to {dst:?}"))?;
    Ok(())
}

fn remove_path(path: &Path) -> Result<()> {
    if path.is_dir() {
        fs::remove_dir_all(path).with_context(|| format!("failed to remove {path:?}"))?;
        return Ok(());
    }

    fs::remove_file(path).with_context(|| format!("failed to remove {path:?}"))?;
    Ok(())
}

fn copy_dir_all(src: &Path, dst: &Path) -> Result<()> {
    fs::create_dir_all(dst).with_context(|| format!("failed to create dir {dst:?}"))?;
    for entry in fs::read_dir(src).with_context(|| format!("failed to read dir {src:?}"))? {
        let entry = entry.with_context(|| format!("failed to read entry under {src:?}"))?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        let metadata = entry
            .metadata()
            .with_context(|| format!("failed to read metadata at {src_path:?}"))?;

        if metadata.is_dir() {
            copy_dir_all(&src_path, &dst_path)?;
            continue;
        }

        fs::copy(&src_path, &dst_path)
            .with_context(|| format!("failed to copy {src_path:?} to {dst_path:?}"))?;
    }
    Ok(())
}

/// 删除路径已全部不在磁盘上的文件记录并通知列表刷新；还有一个路径在就保留。
async fn remove_missing_file_items(core: &CoreInner, pool: &SqlitePool) -> Result<()> {
    let candidates = crate::db::items::unprotected_file_items(pool).await?;
    let missing = tokio::task::spawn_blocking(move || {
        candidates
            .into_iter()
            .filter(|(_, content)| {
                content
                    .split('\n')
                    .filter(|path| !path.is_empty())
                    .all(|path| matches!(Path::new(path).try_exists(), Ok(false)))
            })
            .map(|(id, _)| id)
            .collect::<Vec<_>>()
    })
    .await
    .context("failed to check file paths")?;

    let outcome = crate::db::items::delete_items(pool, &missing).await?;
    clipboard::cleanup::apply_outcome(core, &outcome, "missing files");
    Ok(())
}

/// 查询仍被 image 历史记录引用的图片文件名。
async fn referenced_image_files(pool: &SqlitePool) -> Result<HashSet<String>> {
    let rows =
        sqlx::query_scalar::<_, String>("SELECT content FROM clipboard_items WHERE kind = 'image'")
            .fetch_all(pool)
            .await
            .context("failed to query referenced image files")?;

    Ok(rows.into_iter().collect())
}

/// 查询仍被来源应用引用的应用图标文件名。
async fn referenced_app_icon_files(pool: &SqlitePool) -> Result<HashSet<String>> {
    let rows = sqlx::query_scalar::<_, String>(
        "SELECT DISTINCT icon_file FROM clipboard_apps WHERE icon_file IS NOT NULL",
    )
    .fetch_all(pool)
    .await
    .context("failed to query referenced app icon files")?;

    Ok(rows.into_iter().collect())
}

/// 查询仍被文件类型图标索引引用的文件名。
async fn referenced_file_icon_files(pool: &SqlitePool) -> Result<HashSet<String>> {
    let rows = sqlx::query_scalar::<_, String>("SELECT DISTINCT icon_file FROM file_type_icons")
        .fetch_all(pool)
        .await
        .context("failed to query referenced file icon files")?;

    Ok(rows.into_iter().collect())
}

/// 统计 SQLite 主文件与 WAL / SHM sidecar，反映真实数据库占用。
fn database_bytes(paths: &CorePaths) -> Result<u64> {
    let db_path = crate::db::db_path(paths)?;
    let mut total = file_size(&db_path)?;

    for suffix in ["-wal", "-shm"] {
        total += file_size(Path::new(&format!("{}{}", db_path.display(), suffix)))?;
    }

    Ok(total)
}

/// 数据实际占用按数据库、图片、图标拆分，剩余部分归入其他。
async fn storage_breakdown(
    core: &CoreInner,
    pool: &SqlitePool,
    total_bytes: u64,
) -> Result<StorageBreakdown> {
    let resources_dir = core.paths.resources_dir()?;
    let database_bytes = file_size(&crate::db::db_path(&core.paths)?)?
        .saturating_sub(crate::db::items::reusable_page_bytes(pool).await?);
    let image_bytes = dir_size(&resources_dir.join(CLIPBOARD_IMAGES_DIR))?;
    let icon_bytes = dir_size(&resources_dir.join(APP_ICONS_DIR))?
        + dir_size(&resources_dir.join(FILE_ICONS_DIR))?;
    let other_bytes = total_bytes
        .saturating_sub(database_bytes)
        .saturating_sub(image_bytes)
        .saturating_sub(icon_bytes);

    Ok(StorageBreakdown {
        database_bytes,
        image_bytes,
        icon_bytes,
        other_bytes,
    })
}

/// 扫描资源目录里不再被引用的图片与图标：`Measure` 只统计，`Delete` 同时删除。
async fn sweep_resource_cache(
    core: &CoreInner,
    pool: &SqlitePool,
    sweep: CacheSweep,
) -> Result<CleanCacheStats> {
    let resources_dir = core.paths.resources_dir()?;
    let images_dir = resources_dir.join(CLIPBOARD_IMAGES_DIR);
    let image_files = referenced_image_files(pool).await?;
    let app_icon_files = referenced_app_icon_files(pool).await?;
    let file_icon_files = referenced_file_icon_files(pool).await?;

    let mut stats = CleanCacheStats::default();
    clean_sharded_files(&images_dir.join("origin"), &image_files, sweep, &mut stats)?;
    clean_sharded_files(
        &images_dir.join("thumbnails"),
        &image_files,
        sweep,
        &mut stats,
    )?;
    // 单图文件记录的缩略图是随时能重建的缓存，整个算作可清理。
    clean_sharded_files(
        &images_dir.join(crate::clipboard::FILE_THUMBNAILS_DIR),
        &HashSet::new(),
        sweep,
        &mut stats,
    )?;
    clean_flat_files(
        &resources_dir.join(APP_ICONS_DIR),
        &app_icon_files,
        sweep,
        &mut stats,
    )?;
    clean_flat_files(
        &resources_dir.join(FILE_ICONS_DIR),
        &file_icon_files,
        sweep,
        &mut stats,
    )?;

    Ok(stats)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CacheSweep {
    Measure,
    Delete,
}

#[derive(Default)]
struct CleanCacheStats {
    files: u64,
    bytes: u64,
}

/// 清理平铺目录下没有被引用的文件。
fn clean_flat_files(
    root: &Path,
    referenced_files: &HashSet<String>,
    sweep: CacheSweep,
    removed: &mut CleanCacheStats,
) -> Result<()> {
    if !root.exists() {
        return Ok(());
    }

    for entry in
        fs::read_dir(root).with_context(|| format!("failed to read directory at {root:?}"))?
    {
        let entry = entry.with_context(|| format!("failed to read entry under {root:?}"))?;
        let path = entry.path();
        let metadata = entry
            .metadata()
            .with_context(|| format!("failed to read metadata at {path:?}"))?;

        if metadata.is_dir() {
            clean_flat_files(&path, referenced_files, sweep, removed)?;
            remove_dir_if_empty(&path, sweep);
            continue;
        }

        remove_unreferenced_file(&path, metadata.len(), referenced_files, sweep, removed)?;
    }

    remove_dir_if_empty(root, sweep);

    Ok(())
}

/// 清理带分片子目录的缓存文件，保留仍被数据库引用的文件名。
fn clean_sharded_files(
    root: &Path,
    referenced_files: &HashSet<String>,
    sweep: CacheSweep,
    removed: &mut CleanCacheStats,
) -> Result<()> {
    clean_flat_files(root, referenced_files, sweep, removed)
}

/// 文件名不在引用集合中时计入统计，`Delete` 模式下同时删除该文件。
fn remove_unreferenced_file(
    path: &Path,
    file_bytes: u64,
    referenced_files: &HashSet<String>,
    sweep: CacheSweep,
    removed: &mut CleanCacheStats,
) -> Result<()> {
    let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
        return Ok(());
    };
    if referenced_files.contains(file_name) {
        return Ok(());
    }

    if sweep == CacheSweep::Delete {
        fs::remove_file(path).with_context(|| format!("failed to remove cache file {path:?}"))?;
    }
    removed.files += 1;
    removed.bytes += file_bytes;

    Ok(())
}

/// 尽力删除空目录；非空、缺失或无权限时由文件清理主流程处理即可。
fn remove_dir_if_empty(path: &Path, sweep: CacheSweep) {
    if sweep == CacheSweep::Delete {
        let _ = fs::remove_dir(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            let path = std::env::temp_dir()
                .join(format!("kwikpaste-clean-cache-{}", uuid::Uuid::new_v4()));
            fs::create_dir_all(&path).unwrap();

            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).ok();
        }
    }

    #[test]
    fn clean_flat_files_removes_only_unreferenced_files() {
        let temp = TempDir::new();
        let root = temp.path().join("app-icons");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("keep.png"), b"keep").unwrap();
        fs::write(root.join("drop.png"), b"drop-this").unwrap();

        let referenced = HashSet::from(["keep.png".to_string()]);
        let mut removed = CleanCacheStats::default();

        clean_flat_files(&root, &referenced, CacheSweep::Delete, &mut removed).unwrap();

        assert!(root.join("keep.png").exists());
        assert!(!root.join("drop.png").exists());
        assert_eq!(removed.files, 1);
        assert_eq!(removed.bytes, 9);
    }

    #[test]
    fn clean_sharded_files_removes_empty_shard_directories() {
        let temp = TempDir::new();
        let root = temp.path().join("clipboard-images").join("origin");
        let shard = root.join("ab");
        fs::create_dir_all(&shard).unwrap();
        fs::write(shard.join("abandoned.png"), b"x").unwrap();

        let mut removed = CleanCacheStats::default();

        clean_sharded_files(&root, &HashSet::new(), CacheSweep::Delete, &mut removed).unwrap();

        assert_eq!(removed.files, 1);
        assert!(!shard.exists());
        assert!(!root.exists());
    }

    #[test]
    fn measure_sweep_counts_without_deleting() {
        let temp = TempDir::new();
        let root = temp.path().join("clipboard-images").join("thumbnails");
        let shard = root.join("cd");
        fs::create_dir_all(&shard).unwrap();
        fs::write(shard.join("orphan.png"), b"orphan").unwrap();
        fs::write(shard.join("kept.png"), b"kept").unwrap();

        let referenced = HashSet::from(["kept.png".to_string()]);
        let mut found = CleanCacheStats::default();

        clean_sharded_files(&root, &referenced, CacheSweep::Measure, &mut found).unwrap();

        assert_eq!((found.files, found.bytes), (1, 6));
        assert!(shard.join("orphan.png").exists());
    }

    #[test]
    fn bootstrap_storage_cleanup_keeps_only_manifest_and_sync_identity() {
        let temp = TempDir::new();
        fs::write(temp.path().join("storage.json"), "{}").unwrap();
        fs::write(temp.path().join(".kwikpaste-storage.json"), "{}").unwrap();
        fs::create_dir_all(temp.path().join("db")).unwrap();
        fs::write(temp.path().join("db").join("clipboard.db"), b"db").unwrap();
        fs::create_dir_all(temp.path().join("resources")).unwrap();
        fs::write(temp.path().join("resources").join("image.png"), b"image").unwrap();
        fs::create_dir_all(temp.path().join("sync")).unwrap();
        fs::write(temp.path().join("sync").join("identity.json"), b"{}").unwrap();

        remove_bootstrap_storage_payload(temp.path()).unwrap();

        assert!(temp.path().join("storage.json").exists());
        assert!(temp.path().join("sync").join("identity.json").exists());
        assert!(!temp.path().join(".kwikpaste-storage.json").exists());
        assert!(!temp.path().join("db").exists());
        assert!(!temp.path().join("resources").exists());
    }

    #[test]
    fn remove_custom_storage_root_deletes_empty_container() {
        let temp = TempDir::new();
        let root = temp.path().join("KwikPasteData").join("dev");
        fs::create_dir_all(root.join("config")).unwrap();
        fs::write(root.join("config").join("settings.json"), "{}").unwrap();

        remove_custom_storage_root(&root).unwrap();

        assert!(!root.exists());
        assert!(!temp.path().join("KwikPasteData").exists());
    }

    #[test]
    fn remove_custom_storage_root_keeps_container_with_sibling_environment() {
        let temp = TempDir::new();
        let container = temp.path().join("KwikPasteData");
        let root = container.join("dev");
        fs::create_dir_all(root.join("config")).unwrap();
        fs::write(root.join("config").join("settings.json"), "{}").unwrap();
        fs::create_dir_all(container.join("prod")).unwrap();

        remove_custom_storage_root(&root).unwrap();

        assert!(!root.exists());
        assert!(container.exists());
        assert!(container.join("prod").exists());
    }
}
