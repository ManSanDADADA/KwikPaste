//! `.kwikpastebak` 历史备份的导出与导入。
//!
//! 格式与 1.x 相同，两边的备份可以互相读：
//! - 明文模式是标准 ZIP：`manifest.json`、`db/clipboard.db`、`resources/**`、`config/settings.json`；
//! - 加密模式是 KwikPaste 自有容器：`KWIKPASTEBAK` + u32 LE 头长度 + JSON 头 + 密文。
//!   密钥由 Argon2id（64 MiB、t=3、p=1、16 字节盐）从密码派生，XChaCha20-Poly1305 加密整个 ZIP。
//!
//! 导出默认是整库；按 [`BackupScope`] 只要收藏或部分分组时是「部分备份」：裁剪过的库、
//! 这些记录引用的资源文件和空设置（`{}`），manifest 里记下范围，导入时只能合并。
//!
//! 导入有两种：合并（按 `kind + content_hash` 去重插入、补齐缺少的资源文件、设置按 patch 合并）
//! 与覆盖（热替换数据库、资源目录和设置）。覆盖前先在暂存副本上跑完迁移，失败时当前数据原样保留。
//! 两种都可以不导入设置。
//!
//! 系统「打开文件」、拖入文件、偏好窗口的接收事件都归宿主；这里提供 [`inspect_backup_file`]、
//! [`is_backup_path`] 与 [`backup_path_from_args`] 给宿主识别备份文件。

use std::fmt;
use std::fs::{self, File};
use std::io::{BufReader, BufWriter, Cursor, Read, Seek, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use anyhow::{anyhow, Context};
use argon2::{Algorithm, Argon2, Params, Version};
use chacha20poly1305::aead::{AeadInPlace, KeyInit};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use chrono::{DateTime, Local, NaiveDateTime, TimeZone, Utc};
use flate2::read::GzDecoder;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
use sqlx::{ConnectOptions, Sqlite, SqlitePool};
use tempfile::{NamedTempFile, TempDir};
use walkdir::WalkDir;
use zeroize::Zeroizing;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

use crate::clipboard;
use crate::disk::dir_size;
use crate::env::AppInfo;
use crate::error::{AppError, Result};
use crate::events::CoreEvent;
use crate::i18n::commands::{label, Key};
use crate::paths::CorePaths;
use crate::root::{Core, CoreInner};
use crate::settings::SettingsDelta;

pub const BACKUP_EXTENSION: &str = "kwikpastebak";

const MAGIC: &[u8; 12] = b"KWIKPASTEBAK";
const ECOPASTE_MAGIC: &[u8; 12] = b"ECOPASTEBAK1";
const ZIP_MAGIC: &[u8; 2] = b"PK";
const GZIP_MAGIC: &[u8; 2] = b"\x1f\x8b";
const HEADER_LEN_BYTES: usize = 4;
const FORMAT_VERSION: u16 = 1;
const ARGON2_MEMORY_KIB: u32 = 64 * 1024;
const ARGON2_TIME_COST: u32 = 3;
const ARGON2_PARALLELISM: u32 = 1;
/// 导入时接受的 KDF 参数上限。参数写在备份文件头里、在核对密码之前就要用：不设上限的话，
/// 一个手工改过的文件头能让 argon2 申请上 TB 内存（分配失败直接终止进程）或者永远算不完。
const ARGON2_MEMORY_KIB_MAX: u32 = 512 * 1024;
const ARGON2_TIME_COST_MAX: u32 = 16;
const ARGON2_PARALLELISM_MAX: u32 = 16;
const SALT_LEN: usize = 16;
const NONCE_LEN: usize = 24;
const KEY_LEN: usize = 32;
/// Poly1305 标签长度：就地加密时追加在密文后面。
const TAG_LEN: usize = 16;
const SETTINGS_FILENAME: &str = "settings.json";
const DB_FILENAME: &str = "clipboard.db";
const DB_ARCHIVE_DIR: &str = "db";
const RESOURCES_ARCHIVE_DIR: &str = "resources";
const CONFIG_ARCHIVE_DIR: &str = "config";
const MANIFEST_FILENAME: &str = "manifest.json";
/// 覆盖导入时备份库先复制到这里跑迁移，成功后才换上。
const STAGED_DB_SUFFIX: &str = ".importing";
/// 覆盖导入换库时当前库临时改成这个名字，新库打不开就换回来。
const PREVIOUS_DB_SUFFIX: &str = ".previous";
const SQLITE_SIDECARS: [&str; 2] = ["-wal", "-shm"];

/// 导出选项。加密模式必须带至少 8 个字符的密码，明文模式不能带密码。
#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportHistoryBackupOptions {
    pub mode: BackupExportMode,
    pub password: Option<String>,
    #[serde(default)]
    pub scope: BackupScope,
}

/// 备份里放哪些记录。默认是全部；只要收藏或指定分组时导出部分备份。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupScope {
    pub favorites_only: bool,
    /// `None` 为全部分组；`Some` 为指定分组，未分组的记录由 `include_ungrouped` 决定。
    pub group_ids: Option<Vec<String>>,
    pub include_ungrouped: bool,
}

impl BackupScope {
    pub fn is_all(&self) -> bool {
        !self.favorites_only && self.group_ids.is_none()
    }
}

/// 要导入的备份文件；加密备份必须带密码，明文备份不能带密码。
#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportHistoryBackupInput {
    pub path: PathBuf,
    pub password: Option<String>,
    /// 关掉时只导入记录，当前设置不动。
    #[serde(default = "import_settings_default")]
    pub import_settings: bool,
}

fn import_settings_default() -> bool {
    true
}

impl fmt::Debug for ExportHistoryBackupOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ExportHistoryBackupOptions")
            .field("mode", &self.mode)
            .field("password", &self.password.as_ref().map(|_| "<redacted>"))
            .field("scope", &self.scope)
            .finish()
    }
}

impl fmt::Debug for ImportHistoryBackupInput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ImportHistoryBackupInput")
            .field("path", &self.path)
            .field("password", &self.password.as_ref().map(|_| "<redacted>"))
            .field("import_settings", &self.import_settings)
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BackupImportStrategy {
    Merge,
    Overwrite,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BackupExportMode {
    Encrypted,
    Plain,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportHistoryBackupResult {
    pub path: String,
    pub total_bytes: u64,
    pub item_count: i64,
    pub text_count: i64,
    pub image_count: i64,
    pub files_count: i64,
    pub resource_bytes: u64,
    pub exported_at: DateTime<Utc>,
    pub mode: BackupExportMode,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportHistoryBackupResult {
    pub strategy: BackupImportStrategy,
    pub imported_items: u64,
    pub skipped_items: u64,
    pub imported_resources: u64,
    pub imported_settings: bool,
    pub requires_restart: bool,
}

/// 备份文件的容器模式：决定导入前要不要问密码。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BackupContainerMode {
    Encrypted,
    Plain,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ContainerHeader {
    format_version: u16,
    mode: BackupContainerMode,
    kdf: Option<KdfHeader>,
    cipher: Option<CipherHeader>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct KdfHeader {
    algorithm: String,
    memory_kib: u32,
    time_cost: u32,
    parallelism: u32,
    salt: Vec<u8>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CipherHeader {
    algorithm: String,
    nonce: Vec<u8>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct BackupManifest {
    format_version: u16,
    app_name: String,
    app_version: String,
    exported_at: DateTime<Utc>,
    platform: String,
    encryption: ManifestEncryption,
    item_count: i64,
    text_count: i64,
    image_count: i64,
    files_count: i64,
    resource_bytes: u64,
    /// 只在部分备份里出现；导入时据此拒绝覆盖。
    #[serde(skip_serializing_if = "Option::is_none")]
    scope: Option<BackupScope>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
enum ManifestEncryption {
    None,
    Password,
}

#[derive(Debug, Clone, Copy)]
struct BackupCounts {
    item_count: i64,
    text_count: i64,
    image_count: i64,
    files_count: i64,
}

#[derive(Debug, Clone)]
struct BackupSourcePaths {
    db_path: PathBuf,
    resources_dir: PathBuf,
    settings_path: PathBuf,
}

impl Core {
    /// 把当前环境的历史数据库、资源文件和设置导出成 `.kwikpastebak`。
    /// `target` 没有后缀时补上 `.kwikpastebak`，后缀不对时报错。压缩与加密在阻塞线程池里做。
    pub async fn export_history_backup(
        &self,
        target: PathBuf,
        options: ExportHistoryBackupOptions,
    ) -> Result<ExportHistoryBackupResult> {
        let core = self.clone();
        self.hop(async move { export_history_backup(&core.0, target, options).await })
            .await
    }

    /// 从 `.kwikpastebak` 导入历史和设置：合并写入当前库，或覆盖热替换当前数据。
    ///
    /// 成功后发 [`CoreEvent::ClipboardReloaded`] 与 [`CoreEvent::SettingsUpdated`]
    /// （合并时 `delta` 是备份里的设置涉及的部分，覆盖时是整份替换），宿主据此刷新列表、
    /// 重注册快捷键、同步托盘与自启。覆盖期间采集暂停。
    pub async fn import_history_backup(
        &self,
        input: ImportHistoryBackupInput,
        strategy: BackupImportStrategy,
    ) -> Result<ImportHistoryBackupResult> {
        let core = self.clone();
        self.hop(async move { import_history_backup(&core.0, input, strategy).await })
            .await
    }
}

async fn export_history_backup(
    core: &CoreInner,
    target: PathBuf,
    options: ExportHistoryBackupOptions,
) -> Result<ExportHistoryBackupResult> {
    let target = normalize_backup_path(target)?;
    let password = validate_password_options(&options)?;
    let exported_at = Utc::now();
    // 删图片的清理与删除都要独占锁；导出持共享锁即可挡住它们，采集照常进行。
    let _publication = core.cleanup.image_publication().await;
    let pool = core.db.pool().await;
    // 数据库快照（部分备份还有裁剪后的源文件）在临时目录里，打包完才能删。
    let mut _staged = None;
    let (source_paths, counts) = if options.scope.is_all() {
        let counts = load_counts(&pool).await?;
        let staged = tempfile::tempdir().context("failed to create backup snapshot directory")?;
        let mut paths = backup_source_paths(&core.paths)?;
        paths.db_path = staged.path().join(DB_FILENAME);
        snapshot_database(&pool, &paths.db_path).await?;
        _staged = Some(staged);
        (paths, counts)
    } else {
        let (staged, counts) = stage_scoped_source(core, &pool, &options.scope).await?;
        let paths = BackupSourcePaths {
            db_path: staged.path().join(DB_FILENAME),
            resources_dir: staged.path().join(RESOURCES_ARCHIVE_DIR),
            settings_path: staged.path().join(SETTINGS_FILENAME),
        };
        _staged = Some(staged);
        (paths, counts)
    };
    let resource_bytes = dir_size(&source_paths.resources_dir)?;
    let mut manifest = build_manifest(
        &core.info,
        exported_at,
        options.mode,
        counts,
        resource_bytes,
    );
    manifest.scope = (!options.scope.is_all()).then(|| options.scope.clone());

    let mode = options.mode;
    let written_to = target.clone();
    let total_bytes = blocking(move || {
        let payload_file =
            NamedTempFile::new().context("failed to create temporary backup payload")?;
        write_payload_zip(&source_paths, payload_file.path(), &manifest, &written_to)?;
        write_container(&written_to, payload_file.path(), mode, password)
    })
    .await?;

    Ok(ExportHistoryBackupResult {
        path: target.to_string_lossy().into_owned(),
        total_bytes,
        item_count: counts.item_count,
        text_count: counts.text_count,
        image_count: counts.image_count,
        files_count: counts.files_count,
        resource_bytes,
        exported_at,
        mode,
    })
}

async fn import_history_backup(
    core: &CoreInner,
    input: ImportHistoryBackupInput,
    strategy: BackupImportStrategy,
) -> Result<ImportHistoryBackupResult> {
    validate_import_input(&input)?;

    let ImportHistoryBackupInput {
        path,
        password,
        import_settings,
    } = input;
    let password = password.map(Zeroizing::new);
    let (temp, legacy) = blocking(move || {
        match open_backup_payload(&path, password.as_ref().map(|value| value.as_str()))? {
            BackupPayload::File(file) => extract_payload(BufReader::new(file)),
            BackupPayload::Decrypted(bytes) => extract_payload(Cursor::new(bytes)),
        }
    })
    .await?;

    if legacy {
        return import_ecopaste_history(core, temp.path()).await;
    }

    match strategy {
        BackupImportStrategy::Merge => merge_import(core, temp.path(), import_settings).await,
        BackupImportStrategy::Overwrite => {
            if is_partial_backup(temp.path())? {
                return app_error(label(core.language(), Key::BackupPartialOverwrite));
            }
            overwrite_import(core, temp.path(), import_settings).await
        }
    }
}

/// manifest 里带 `scope` 的是部分备份。1.x 与更早的 2.0 写出的备份没有这个字段，都是整库。
fn is_partial_backup(root: &Path) -> Result<bool> {
    let manifest = read_json_file(&root.join(MANIFEST_FILENAME))?;
    Ok(manifest.get("scope").is_some_and(|scope| !scope.is_null()))
}

/// 按文件头识别 KwikPaste 或 EcoPaste 备份的容器模式；不解密、不导入。
pub fn inspect_backup_file(path: &Path) -> Result<BackupContainerMode> {
    let mut file = File::open(path).with_context(|| format!("failed to open backup {path:?}"))?;
    inspect_backup_reader(&mut file)
}

/// 从进程参数中查找 `.kwikpastebak` 路径，供文件关联启动和第二实例唤起使用。
pub fn backup_path_from_args(args: &[String]) -> Option<PathBuf> {
    args.iter()
        .map(PathBuf::from)
        .find(|path| is_backup_path(path))
}

/// 判断路径是否看起来是 KwikPaste 备份包。
pub fn is_backup_path(path: &Path) -> bool {
    path.extension()
        .and_then(|value| value.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case(BACKUP_EXTENSION))
}

/// 在阻塞线程池里做文件与加解密这类长时间的同步工作，不占 core runtime 的工作线程。
async fn blocking<T, F>(work: F) -> Result<T>
where
    F: FnOnce() -> Result<T> + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|err| AppError::Other(anyhow!("backup task failed: {err}")))?
}

fn validate_password_options(
    options: &ExportHistoryBackupOptions,
) -> Result<Option<Zeroizing<String>>> {
    match options.mode {
        BackupExportMode::Encrypted => {
            let Some(password) = options.password.as_ref() else {
                return app_error("请输入备份密码");
            };
            if password.chars().count() < 8 {
                return app_error("备份密码至少需要 8 个字符");
            }

            Ok(Some(Zeroizing::new(password.clone())))
        }
        BackupExportMode::Plain => {
            if options
                .password
                .as_deref()
                .is_some_and(|value| !value.is_empty())
            {
                return app_error("明文备份不应包含密码");
            }

            Ok(None)
        }
    }
}

fn validate_import_input(input: &ImportHistoryBackupInput) -> Result<()> {
    match inspect_backup_file(&input.path)? {
        BackupContainerMode::Encrypted => {
            if input.password.as_deref().is_none_or(str::is_empty) {
                return app_error("请输入备份密码");
            }
        }
        BackupContainerMode::Plain => {
            if input
                .password
                .as_deref()
                .is_some_and(|value| !value.is_empty())
            {
                return app_error("明文备份不应包含密码");
            }
        }
    }

    Ok(())
}

fn normalize_backup_path(path: PathBuf) -> Result<PathBuf> {
    if path.as_os_str().is_empty() {
        return app_error("请选择备份保存位置");
    }

    match path.extension().and_then(|value| value.to_str()) {
        Some(ext) if ext.eq_ignore_ascii_case(BACKUP_EXTENSION) => Ok(path),
        Some(_) => app_error(format!("备份文件后缀必须是 .{BACKUP_EXTENSION}")),
        None => Ok(path.with_extension(BACKUP_EXTENSION)),
    }
}

/// 用 `VACUUM INTO` 把当前库写成一份一致的快照。导出期间采集照常写库，
/// 直接打包正在使用的主库文件会撞上自动 checkpoint 改写它。
async fn snapshot_database(pool: &SqlitePool, target: &Path) -> Result<()> {
    sqlx::query("VACUUM INTO ?")
        .bind(target.to_string_lossy().into_owned())
        .execute(pool)
        .await
        .context("failed to snapshot the database for backup")?;

    Ok(())
}

async fn checkpoint_database(pool: &SqlitePool) -> Result<()> {
    sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)")
        .execute(pool)
        .await
        .context("failed to checkpoint backup database")?;

    Ok(())
}

async fn load_counts(pool: &SqlitePool) -> Result<BackupCounts> {
    let item_count = count_items(pool, None).await?;
    let text_count = count_items(pool, Some("text")).await?;
    let image_count = count_items(pool, Some("image")).await?;
    let files_count = count_items(pool, Some("files")).await?;

    Ok(BackupCounts {
        item_count,
        text_count,
        image_count,
        files_count,
    })
}

async fn count_items(pool: &SqlitePool, kind: Option<&str>) -> Result<i64> {
    let count = if let Some(kind) = kind {
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM clipboard_items WHERE kind = ?")
            .bind(kind)
            .fetch_one(pool)
            .await
    } else {
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM clipboard_items")
            .fetch_one(pool)
            .await
    }
    .context("failed to count backup items")?;

    Ok(count)
}

fn build_manifest(
    info: &AppInfo,
    exported_at: DateTime<Utc>,
    mode: BackupExportMode,
    counts: BackupCounts,
    resource_bytes: u64,
) -> BackupManifest {
    BackupManifest {
        format_version: FORMAT_VERSION,
        app_name: info.name.to_owned(),
        app_version: info.version.to_string(),
        exported_at,
        platform: current_platform().to_owned(),
        encryption: match mode {
            BackupExportMode::Encrypted => ManifestEncryption::Password,
            BackupExportMode::Plain => ManifestEncryption::None,
        },
        item_count: counts.item_count,
        text_count: counts.text_count,
        image_count: counts.image_count,
        files_count: counts.files_count,
        resource_bytes,
        scope: None,
    }
}

fn current_platform() -> &'static str {
    if cfg!(target_os = "macos") {
        "macos"
    } else {
        "windows"
    }
}

/// 汇总备份允许写入包内的源路径，避免把日志、缓存、临时文件等环境杂项带进迁移包。
fn backup_source_paths(paths: &CorePaths) -> Result<BackupSourcePaths> {
    Ok(BackupSourcePaths {
        db_path: crate::db::db_path(paths)?,
        resources_dir: paths.resources_dir()?,
        settings_path: paths.config_dir()?.join(SETTINGS_FILENAME),
    })
}

/// 在临时目录里摆出部分备份的源文件：裁剪到 `scope` 的库、这些记录引用的图片与来源应用图标，
/// 以及空设置。布局与 [`BackupSourcePaths`] 对应：`clipboard.db`、`resources/`、`settings.json`。
async fn stage_scoped_source(
    core: &CoreInner,
    pool: &SqlitePool,
    scope: &BackupScope,
) -> Result<(TempDir, BackupCounts)> {
    let lang = core.language();
    if scope.group_ids.as_ref().is_some_and(Vec::is_empty) && !scope.include_ungrouped {
        return app_error(label(lang, Key::ExportNoGroups));
    }

    let staged = tempfile::tempdir().context("failed to create partial backup directory")?;
    let db_path = staged.path().join(DB_FILENAME);
    snapshot_database(pool, &db_path).await?;

    let copy = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            SqliteConnectOptions::new()
                .filename(&db_path)
                .journal_mode(SqliteJournalMode::Wal)
                .foreign_keys(true)
                .disable_statement_logging(),
        )
        .await
        .context("failed to open the partial backup database")?;
    let pruned = async {
        prune_to_scope(&copy, scope).await?;
        strip_ocr_backup(&copy).await?;
        let counts = load_counts(&copy).await?;
        let images = sqlx::query_scalar::<_, String>(
            "SELECT DISTINCT content FROM clipboard_items WHERE kind = 'image'",
        )
        .fetch_all(&copy)
        .await
        .context("failed to read partial backup images")?;
        let app_icons = sqlx::query_scalar::<_, String>(
            "SELECT icon_file FROM clipboard_apps WHERE icon_file IS NOT NULL",
        )
        .fetch_all(&copy)
        .await
        .context("failed to read partial backup app icons")?;
        checkpoint_database(&copy).await?;
        Ok::<_, AppError>((counts, images, app_icons))
    }
    .await;
    copy.close().await;
    let (counts, images, app_icons) = pruned?;
    if counts.item_count == 0 {
        return app_error(label(lang, Key::ExportEmpty));
    }

    let resources_dir = core.paths.resources_dir()?;
    let files: Vec<PathBuf> = images
        .iter()
        .flat_map(|name| {
            [
                core.images.origin_path(name),
                core.images.thumbnail_path(name),
            ]
        })
        .chain(
            app_icons
                .iter()
                .filter(|name| is_plain_file_name(name))
                .map(|name| core.app_icons.icon_path(name)),
        )
        .collect();
    let staged_resources = staged.path().join(RESOURCES_ARCHIVE_DIR);
    let staged_settings = staged.path().join(SETTINGS_FILENAME);
    blocking(move || {
        fs::write(&staged_settings, b"{}")
            .with_context(|| format!("failed to write {staged_settings:?}"))?;
        fs::create_dir_all(&staged_resources)
            .with_context(|| format!("failed to create {staged_resources:?}"))?;
        for file in files {
            let Ok(relative) = file.strip_prefix(&resources_dir) else {
                continue;
            };
            if file.is_file() {
                link_or_copy(&file, &staged_resources.join(relative))?;
            }
        }
        Ok(())
    })
    .await?;

    Ok((staged, counts))
}

/// 部分备份不带图片识别文字：删掉派生的 OCR 正文、重建它的索引再 VACUUM，删掉的文字不留在索引段或空闲页里。
async fn strip_ocr_backup(pool: &SqlitePool) -> Result<()> {
    for statement in [
        "DELETE FROM image_texts",
        "INSERT INTO image_texts_fts(image_texts_fts) VALUES ('rebuild')",
        "VACUUM",
    ] {
        sqlx::query(statement)
            .execute(pool)
            .await
            .context("failed to strip derived OCR data from backup")?;
    }
    Ok(())
}

/// 删掉范围外的记录，以及不再被引用的分组、来源应用和全部文件类型图标（缓存，键里可能有完整路径）。
/// 全文索引整个重建、库再 VACUUM 一遍：删掉的内容不能留在索引段或空闲页里被带出去。
async fn prune_to_scope(pool: &SqlitePool, scope: &BackupScope) -> Result<()> {
    let group_ids = serde_json::to_string(scope.group_ids.as_deref().unwrap_or_default())
        .context("failed to serialize backup groups")?;
    sqlx::query(
        "DELETE FROM clipboard_items WHERE NOT ( \
           (? = 0 OR is_favorite = 1) \
           AND (? = 0 OR CASE WHEN group_id IS NULL THEN ? = 1 \
                ELSE group_id IN (SELECT value FROM json_each(?)) END))",
    )
    .bind(scope.favorites_only)
    .bind(scope.group_ids.is_some())
    .bind(scope.include_ungrouped)
    .bind(group_ids)
    .execute(pool)
    .await
    .context("failed to prune partial backup items")?;

    for statement in [
        "DELETE FROM clipboard_groups WHERE id NOT IN \
         (SELECT group_id FROM clipboard_items WHERE group_id IS NOT NULL)",
        "DELETE FROM clipboard_apps WHERE id NOT IN \
         (SELECT source_app_id FROM clipboard_items WHERE source_app_id IS NOT NULL)",
        "DELETE FROM file_type_icons",
        "INSERT INTO clipboard_items_fts(clipboard_items_fts) VALUES ('rebuild')",
        "VACUUM",
    ] {
        sqlx::query(statement)
            .execute(pool)
            .await
            .with_context(|| format!("failed to prune partial backup: {statement}"))?;
    }

    Ok(())
}

/// 单层文件名：来源应用图标的名字来自库里，拼路径前挡掉分隔符和 `..`。
fn is_plain_file_name(name: &str) -> bool {
    let mut parts = Path::new(name).components();
    matches!(parts.next(), Some(std::path::Component::Normal(_))) && parts.next().is_none()
}

/// 资源文件按内容命名、写下后不再改：能硬链接就不复制，跨盘时退回复制。
fn link_or_copy(src: &Path, dst: &Path) -> Result<()> {
    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent).with_context(|| format!("failed to create {parent:?}"))?;
    }
    if fs::hard_link(src, dst).is_ok() {
        return Ok(());
    }
    fs::copy(src, dst).with_context(|| format!("failed to copy {src:?} to {dst:?}"))?;

    Ok(())
}

fn write_payload_zip(
    source_paths: &BackupSourcePaths,
    path: &Path,
    manifest: &BackupManifest,
    target_path: &Path,
) -> Result<()> {
    let file = File::create(path).with_context(|| format!("failed to create payload {path:?}"))?;
    let mut zip = ZipWriter::new(BufWriter::new(file));
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);

    zip.start_file(MANIFEST_FILENAME, options)
        .context("failed to write backup manifest entry")?;
    let manifest_bytes =
        serde_json::to_vec_pretty(manifest).context("failed to serialize backup manifest")?;
    zip.write_all(&manifest_bytes)
        .context("failed to write backup manifest")?;

    add_optional_file(
        &mut zip,
        &source_paths.db_path,
        &archive_path(DB_ARCHIVE_DIR, Path::new(DB_FILENAME))?,
        options,
        target_path,
    )?;
    add_dir_contents(
        &mut zip,
        &source_paths.resources_dir,
        RESOURCES_ARCHIVE_DIR,
        options,
        target_path,
    )?;
    add_optional_file(
        &mut zip,
        &source_paths.settings_path,
        &archive_path(CONFIG_ARCHIVE_DIR, Path::new(SETTINGS_FILENAME))?,
        options,
        target_path,
    )?;

    zip.finish()
        .context("failed to finish backup payload archive")?;

    Ok(())
}

fn add_file<W: Write + Seek>(
    zip: &mut ZipWriter<W>,
    path: &Path,
    archive_name: &str,
    options: SimpleFileOptions,
) -> Result<()> {
    zip.start_file(archive_name, options)
        .with_context(|| format!("failed to start archive file {archive_name}"))?;
    let mut file = File::open(path).with_context(|| format!("failed to open {path:?}"))?;
    std::io::copy(&mut file, zip).with_context(|| format!("failed to archive {path:?}"))?;

    Ok(())
}

fn add_optional_file<W: Write + Seek>(
    zip: &mut ZipWriter<W>,
    path: &Path,
    archive_name: &str,
    options: SimpleFileOptions,
    target_path: &Path,
) -> Result<()> {
    if !path.exists() || same_path(path, target_path) {
        return Ok(());
    }

    add_file(zip, path, archive_name, options)
}

fn add_dir_contents<W: Write + Seek>(
    zip: &mut ZipWriter<W>,
    root: &Path,
    archive_root: &str,
    options: SimpleFileOptions,
    target_path: &Path,
) -> Result<()> {
    if !root.exists() {
        return Ok(());
    }

    for entry in WalkDir::new(root)
        .into_iter()
        .filter_map(std::result::Result::ok)
    {
        let path = entry.path();
        let metadata = entry
            .metadata()
            .with_context(|| format!("failed to read metadata at {path:?}"))?;
        if !metadata.is_file() {
            continue;
        }
        let Some(file_name) = path.file_name().and_then(|value| value.to_str()) else {
            continue;
        };
        if should_skip_backup_path(path, file_name, target_path) {
            continue;
        }

        let relative = path
            .strip_prefix(root)
            .with_context(|| format!("failed to strip root {root:?} from {path:?}"))?;
        if is_rebuildable_cache(relative) {
            continue;
        }
        let archive_name = archive_path(archive_root, relative)?;
        add_file(zip, path, &archive_name, options)?;
    }

    Ok(())
}

fn archive_path(prefix: &str, relative: &Path) -> Result<String> {
    let mut parts = Vec::new();
    if !prefix.is_empty() {
        parts.push(prefix.to_owned());
    }
    for part in relative.components() {
        let std::path::Component::Normal(value) = part else {
            return app_error("backup path contains unsupported component");
        };
        let value = value
            .to_str()
            .ok_or_else(|| anyhow!("backup path is not valid utf-8"))?;
        parts.push(value.to_owned());
    }

    Ok(parts.join("/"))
}

fn should_skip_backup_path(path: &Path, file_name: &str, target_path: &Path) -> bool {
    if same_path(path, target_path) {
        return true;
    }

    file_name.ends_with(".tmp")
        || file_name.ends_with(".temp")
        || file_name.starts_with(".tmp")
        || file_name == ".DS_Store"
}

/// 随时能重建的缓存不进备份：单图文件记录的缩略图（2.0 新增，1.x 的备份里本来就没有）。
fn is_rebuildable_cache(relative: &Path) -> bool {
    let mut parts = relative.components().map(|part| part.as_os_str());
    parts.next() == Some(std::ffi::OsStr::new("clipboard-images"))
        && parts.next() == Some(std::ffi::OsStr::new(crate::clipboard::FILE_THUMBNAILS_DIR))
}

fn same_path(left: &Path, right: &Path) -> bool {
    let left = fs::canonicalize(left).unwrap_or_else(|_| left.to_path_buf());
    let right = fs::canonicalize(right).unwrap_or_else(|_| right.to_path_buf());

    left == right
}

fn write_container(
    target: &Path,
    payload_path: &Path,
    mode: BackupExportMode,
    password: Option<Zeroizing<String>>,
) -> Result<u64> {
    let parent = target
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent).with_context(|| format!("failed to create {parent:?}"))?;

    let mut temp = NamedTempFile::new_in(parent)
        .with_context(|| format!("failed to create temporary file under {parent:?}"))?;
    match mode {
        BackupExportMode::Plain => {
            copy_file_into_writer(payload_path, &mut temp)?;
        }
        BackupExportMode::Encrypted => {
            let password = password.ok_or_else(|| anyhow!("missing backup password"))?;
            let mut file = File::open(payload_path)
                .with_context(|| format!("failed to open payload {payload_path:?}"))?;
            let len = file
                .metadata()
                .context("failed to read backup payload metadata")?
                .len();
            // 一次留够标签的位置：就地加密不再整块重新分配，内存里只有一份载荷。
            let mut payload = Vec::new();
            payload
                .try_reserve_exact(
                    usize::try_from(len)
                        .context("backup payload is too large")?
                        .saturating_add(TAG_LEN),
                )
                .context("backup payload is too large to encrypt")?;
            file.read_to_end(&mut payload)
                .context("failed to read backup payload")?;

            let encrypted = encrypt_payload(payload, &password)?;
            write_header(&mut temp, &encrypted.header)?;
            temp.write_all(&encrypted.ciphertext)
                .context("failed to write encrypted backup payload")?;
        }
    }

    temp.flush().context("failed to flush backup file")?;
    temp.as_file()
        .sync_all()
        .context("failed to sync backup file")?;
    temp.persist(target)
        .map_err(|err| anyhow!(err))
        .with_context(|| format!("failed to persist backup to {target:?}"))?;

    let total_bytes = fs::metadata(target)
        .with_context(|| format!("failed to read backup metadata {target:?}"))?
        .len();

    Ok(total_bytes)
}

struct EncryptedPayload {
    header: ContainerHeader,
    ciphertext: Vec<u8>,
}

/// 就地加密：密文（带标签）复用 `payload` 的内存。
fn encrypt_payload(mut payload: Vec<u8>, password: &str) -> Result<EncryptedPayload> {
    let mut salt = [0u8; SALT_LEN];
    let mut nonce = [0u8; NONCE_LEN];
    rand::rngs::OsRng.fill_bytes(&mut salt);
    rand::rngs::OsRng.fill_bytes(&mut nonce);

    let mut key = Zeroizing::new([0u8; KEY_LEN]);
    let params = Params::new(
        ARGON2_MEMORY_KIB,
        ARGON2_TIME_COST,
        ARGON2_PARALLELISM,
        Some(KEY_LEN),
    )
    .context("failed to build argon2 params")?;
    let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    argon2
        .hash_password_into(password.as_bytes(), &salt, key.as_mut())
        .context("failed to derive backup key")?;

    let cipher = XChaCha20Poly1305::new_from_slice(key.as_ref())
        .context("failed to initialize backup cipher")?;
    let nonce = XNonce::from(nonce);
    cipher
        .encrypt_in_place(&nonce, b"", &mut payload)
        .map_err(|_| anyhow!("failed to encrypt backup payload"))?;

    Ok(EncryptedPayload {
        header: ContainerHeader {
            format_version: FORMAT_VERSION,
            mode: BackupContainerMode::Encrypted,
            kdf: Some(KdfHeader {
                algorithm: "argon2id".to_owned(),
                memory_kib: ARGON2_MEMORY_KIB,
                time_cost: ARGON2_TIME_COST,
                parallelism: ARGON2_PARALLELISM,
                salt: salt.to_vec(),
            }),
            cipher: Some(CipherHeader {
                algorithm: "xchacha20poly1305".to_owned(),
                nonce: nonce.to_vec(),
            }),
        },
        ciphertext: payload,
    })
}

fn write_header<W: Write>(writer: &mut W, header: &ContainerHeader) -> Result<()> {
    let header_bytes = serde_json::to_vec(header).context("failed to serialize backup header")?;
    let header_len: u32 = header_bytes
        .len()
        .try_into()
        .context("backup header is too large")?;

    writer
        .write_all(MAGIC)
        .context("failed to write backup magic")?;
    writer
        .write_all(&header_len.to_le_bytes())
        .context("failed to write backup header length")?;
    writer
        .write_all(&header_bytes)
        .context("failed to write backup header")?;

    Ok(())
}

/// 备份载荷：明文备份直接从文件流式解包；加密备份读进一块内存就地解密，不另留一份密文。
enum BackupPayload {
    File(File),
    Decrypted(Vec<u8>),
}

fn open_backup_payload(path: &Path, password: Option<&str>) -> Result<BackupPayload> {
    let mut file = File::open(path).with_context(|| format!("failed to open backup {path:?}"))?;
    let mut magic = Vec::with_capacity(MAGIC.len());
    Read::by_ref(&mut file)
        .take(MAGIC.len() as u64)
        .read_to_end(&mut magic)
        .with_context(|| format!("failed to read backup {path:?}"))?;

    if magic.starts_with(ZIP_MAGIC) || magic.starts_with(GZIP_MAGIC) {
        file.rewind()
            .with_context(|| format!("failed to read backup {path:?}"))?;
        return Ok(BackupPayload::File(file));
    }
    if !magic.starts_with(MAGIC) && !magic.starts_with(ECOPASTE_MAGIC) {
        return app_error("不是有效的 KwikPaste 备份文件");
    }

    let header = read_container_header_after_magic(&mut file)?;
    let Some(kdf) = header.kdf else {
        return app_error("加密备份文件头无效");
    };
    let Some(cipher) = header.cipher else {
        return app_error("加密备份文件头无效");
    };
    let Some(password) = password else {
        return app_error("请输入备份密码");
    };

    let mut ciphertext = Vec::new();
    file.read_to_end(&mut ciphertext)
        .with_context(|| format!("failed to read backup {path:?}"))?;
    decrypt_payload(ciphertext, password, &kdf, &cipher).map(BackupPayload::Decrypted)
}

/// 就地解密：明文复用 `ciphertext` 的内存。
fn decrypt_payload(
    mut ciphertext: Vec<u8>,
    password: &str,
    kdf: &KdfHeader,
    cipher_header: &CipherHeader,
) -> Result<Vec<u8>> {
    if kdf.algorithm != "argon2id" || cipher_header.algorithm != "xchacha20poly1305" {
        return app_error("暂不支持该备份加密格式");
    }
    if kdf.salt.len() != SALT_LEN || cipher_header.nonce.len() != NONCE_LEN {
        return app_error("加密备份文件头无效");
    }
    if kdf.memory_kib > ARGON2_MEMORY_KIB_MAX
        || kdf.time_cost > ARGON2_TIME_COST_MAX
        || kdf.parallelism > ARGON2_PARALLELISM_MAX
    {
        return app_error("加密备份文件头无效");
    }

    let mut key = Zeroizing::new([0u8; KEY_LEN]);
    let params = Params::new(
        kdf.memory_kib,
        kdf.time_cost,
        kdf.parallelism,
        Some(KEY_LEN),
    )
    .context("failed to build argon2 params")?;
    let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    argon2
        .hash_password_into(password.as_bytes(), &kdf.salt, key.as_mut())
        .context("failed to derive backup key")?;

    let cipher =
        XChaCha20Poly1305::new_from_slice(key.as_ref()).context("failed to initialize cipher")?;
    let mut nonce = [0u8; NONCE_LEN];
    nonce.copy_from_slice(&cipher_header.nonce);
    let nonce = XNonce::from(nonce);
    cipher
        .decrypt_in_place(&nonce, b"", &mut ciphertext)
        .map_err(|_| AppError::Other(anyhow!("备份密码不正确或文件已损坏")))?;

    Ok(ciphertext)
}

/// 解包备份载荷：zip 解到临时目录并检查必需文件；EcoPaste 0.x 的 tar.gz 返回 `true`。
fn extract_payload<R: Read + Seek>(mut reader: R) -> Result<(TempDir, bool)> {
    let mut magic = Vec::with_capacity(GZIP_MAGIC.len());
    Read::by_ref(&mut reader)
        .take(GZIP_MAGIC.len() as u64)
        .read_to_end(&mut magic)
        .context("failed to read backup payload")?;
    reader.rewind().context("failed to read backup payload")?;
    if magic.starts_with(GZIP_MAGIC) {
        return Ok((extract_payload_tar_gz(reader)?, true));
    }

    let temp = extract_payload_zip(reader)?;
    validate_extracted_payload(temp.path())?;
    Ok((temp, false))
}

/// 把备份里的 zip 解到临时目录。只解普通文件和目录：zip 的 `extract` 遇到符号链接条目时按条目里
/// 声称的大小预分配缓冲，手工构造的文件能让它 panic 或申请几十 GB；路径一律经 `enclosed_name`，
/// 不会写到临时目录外面。
fn extract_payload_zip<R: Read + Seek>(payload: R) -> Result<TempDir> {
    let temp = tempfile::tempdir().context("failed to create temporary import directory")?;
    let mut archive = ZipArchive::new(payload).context("failed to read backup zip payload")?;
    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .context("failed to read backup zip entry")?;
        let Some(relative) = entry.enclosed_name() else {
            return app_error("备份文件里有无效的路径");
        };
        let target = temp.path().join(relative);
        if entry.is_dir() {
            std::fs::create_dir_all(&target)
                .with_context(|| format!("failed to create {target:?}"))?;
            continue;
        }
        if !entry.is_file() {
            return app_error("备份文件里有不支持的条目");
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {parent:?}"))?;
        }
        let mut file = std::fs::File::create(&target)
            .with_context(|| format!("failed to create {target:?}"))?;
        std::io::copy(&mut entry, &mut file).context("failed to extract backup payload")?;
    }

    Ok(temp)
}

/// EcoPaste 0.x 的 tar.gz 只接受临时目录内的普通相对路径、文件和目录。
fn extract_payload_tar_gz<R: Read>(payload: R) -> Result<TempDir> {
    let temp = tempfile::tempdir().context("failed to create temporary import directory")?;
    let mut archive = tar::Archive::new(GzDecoder::new(payload));
    for entry in archive
        .entries()
        .context("failed to read EcoPaste archive")?
    {
        let mut entry = entry.context("failed to read EcoPaste archive entry")?;
        let relative = entry.path().context("failed to read EcoPaste entry path")?;
        if relative.as_os_str().is_empty()
            || relative
                .components()
                .any(|part| !matches!(part, std::path::Component::Normal(_)))
        {
            return app_error("备份文件里有无效的路径");
        }
        let target = temp.path().join(relative);
        let kind = entry.header().entry_type();
        if kind.is_dir() {
            fs::create_dir_all(&target).with_context(|| format!("failed to create {target:?}"))?;
            continue;
        }
        if !kind.is_file() {
            return app_error("备份文件里有不支持的条目");
        }
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).with_context(|| format!("failed to create {parent:?}"))?;
        }
        let mut file =
            File::create(&target).with_context(|| format!("failed to create {target:?}"))?;
        std::io::copy(&mut entry, &mut file).context("failed to extract EcoPaste payload")?;
    }

    Ok(temp)
}

#[derive(sqlx::FromRow)]
struct EcoPasteRow {
    #[sqlx(rename = "type")]
    kind: String,
    value: String,
    search: Option<String>,
    favorite: Option<i64>,
    #[sqlx(rename = "createTime")]
    create_time: Option<String>,
    note: Option<String>,
    width: Option<i32>,
    height: Option<i32>,
}

impl EcoPasteRow {
    /// 旧图片路径只取文件名；设置、分组与使用次数不属于历史内容载荷。
    fn payload(&self, root: &Path) -> Result<Option<clipboard::ClipboardPayload>> {
        use clipboard::{ClipboardPayload, ImagePayload, TextPayload};

        let payload = match self.kind.as_str() {
            "text" => ClipboardPayload::Text(TextPayload {
                text: self.value.clone(),
                html: None,
                rtf: None,
            }),
            "html" | "rtf" => ClipboardPayload::Text(TextPayload {
                text: self.search.clone().unwrap_or_default(),
                html: (self.kind == "html").then(|| self.value.clone()),
                rtf: (self.kind == "rtf").then(|| self.value.clone()),
            }),
            "image" => {
                let name = self.value.rsplit(['/', '\\']).next().unwrap_or_default();
                let mut parts = Path::new(name).components();
                if !matches!(parts.next(), Some(std::path::Component::Normal(_)))
                    || parts.next().is_some()
                {
                    return Ok(None);
                }
                let path = root.join("images").join(name);
                if !path.is_file() {
                    return Ok(None);
                }
                ClipboardPayload::Image(ImagePayload {
                    bytes: fs::read(&path)
                        .with_context(|| format!("failed to read EcoPaste image {path:?}"))?,
                    width: self.width.unwrap_or(1).max(1) as u32,
                    height: self.height.unwrap_or(1).max(1) as u32,
                })
            }
            "files" => {
                let Ok(paths) = serde_json::from_str::<Vec<String>>(&self.value) else {
                    return Ok(None);
                };
                if paths.is_empty() {
                    return Ok(None);
                }
                ClipboardPayload::Files(paths)
            }
            _ => return Ok(None),
        };

        Ok(Some(payload))
    }
}

/// EcoPaste 0.x 始终合并旧历史：保留当前设置与重复记录，不产生本机采集的同步序号或提示音。
async fn import_ecopaste_history(
    core: &CoreInner,
    root: &Path,
) -> Result<ImportHistoryBackupResult> {
    let mut db_path = None;
    for entry in fs::read_dir(root).context("failed to list EcoPaste backup")? {
        let entry = entry.context("failed to read EcoPaste backup directory entry")?;
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "db") && path.is_file() {
            db_path = Some(path);
            break;
        }
    }
    let Some(db_path) = db_path else {
        return app_error("备份文件里没有 EcoPaste 的历史数据库");
    };
    let backup = open_backup_db(&db_path).await?;
    let rows = async {
        let exists: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'history'",
        )
        .fetch_one(&backup)
        .await
        .context("failed to inspect EcoPaste history database")?;
        if exists == 0 {
            return app_error("备份文件里没有 EcoPaste 的历史数据库");
        }
        Ok(sqlx::query_as::<_, EcoPasteRow>(
            "SELECT type, value, search, favorite, createTime, note, width, height \
             FROM history ORDER BY createTime ASC, id ASC",
        )
        .fetch_all(&backup)
        .await
        .context("failed to read EcoPaste history")?)
    }
    .await;
    backup.close().await;
    let rows = rows?;

    let mut imported_items = 0;
    let mut skipped_items = 0;
    {
        let _publication = core.cleanup.image_publication().await;
        let _upsert = core.upsert_lock.lock().await;
        let pool = core.db.pool().await;
        for row in rows {
            let Some(payload) = row.payload(root)? else {
                skipped_items += 1;
                continue;
            };
            let settings = core.settings.snapshot();
            let Some(mut item) = clipboard::build_item_with_settings(
                &core.images,
                &payload,
                &settings.clipboard.capture,
                &settings.clipboard.sensitive,
                settings.clipboard.content.copy_plain,
                settings.clipboard.compiled_rules(),
            )?
            else {
                skipped_items += 1;
                continue;
            };
            item.note = row
                .note
                .map(|note| note.trim().to_owned())
                .filter(|note| !note.is_empty());
            item.is_favorite = row.favorite == Some(1);
            let timestamp = row
                .create_time
                .as_deref()
                .and_then(|value| NaiveDateTime::parse_from_str(value, "%Y-%m-%d %H:%M:%S").ok())
                .and_then(|value| Local.from_local_datetime(&value).single())
                .map(|value| value.with_timezone(&Utc))
                .unwrap_or_else(Utc::now);
            item.created_at = timestamp;
            item.updated_at = timestamp;

            let exists: Option<i64> =
                sqlx::query_scalar("SELECT 1 FROM clipboard_items WHERE content_hash = ? LIMIT 1")
                    .bind(&item.content_hash)
                    .fetch_optional(&pool)
                    .await
                    .context("failed to check duplicate EcoPaste item")?;
            if exists.is_some() {
                skipped_items += 1;
                continue;
            }
            crate::db::items::insert_item(&pool, &item).await?;
            imported_items += 1;
        }
    }
    refresh_apps_registry(core).await;
    core.events.emit(CoreEvent::ClipboardReloaded);
    core.ocr.nudge();

    Ok(ImportHistoryBackupResult {
        strategy: BackupImportStrategy::Merge,
        imported_items,
        skipped_items,
        imported_resources: 0,
        imported_settings: false,
        requires_restart: false,
    })
}

fn validate_extracted_payload(root: &Path) -> Result<()> {
    if !root.join(MANIFEST_FILENAME).exists() {
        return app_error("备份文件缺少 manifest.json");
    }
    if !root.join(DB_ARCHIVE_DIR).join(DB_FILENAME).exists() {
        return app_error("备份文件缺少历史数据库");
    }
    if !root
        .join(CONFIG_ARCHIVE_DIR)
        .join(SETTINGS_FILENAME)
        .exists()
    {
        return app_error("备份文件缺少设置文件");
    }

    Ok(())
}

fn inspect_backup_reader<R: Read>(reader: &mut R) -> Result<BackupContainerMode> {
    let mut magic = [0u8; MAGIC.len()];
    let read = reader
        .read(&mut magic)
        .context("failed to read backup magic")?;
    if read >= MAGIC.len() && (&magic == MAGIC || &magic == ECOPASTE_MAGIC) {
        let header = read_container_header_after_magic(reader)?;
        return Ok(header.mode);
    }
    if read >= ZIP_MAGIC.len()
        && (&magic[..ZIP_MAGIC.len()] == ZIP_MAGIC || &magic[..GZIP_MAGIC.len()] == GZIP_MAGIC)
    {
        return Ok(BackupContainerMode::Plain);
    }

    app_error("不是有效的 KwikPaste 备份文件")
}

fn read_container_header_after_magic<R: Read>(reader: &mut R) -> Result<ContainerHeader> {
    let mut len = [0u8; HEADER_LEN_BYTES];
    reader
        .read_exact(&mut len)
        .context("failed to read backup header length")?;
    let header_len = u32::from_le_bytes(len) as usize;
    if header_len == 0 || header_len > 64 * 1024 {
        return app_error("备份文件头无效");
    }

    let mut header = vec![0u8; header_len];
    reader
        .read_exact(&mut header)
        .context("failed to read backup header")?;
    let header: ContainerHeader =
        serde_json::from_slice(&header).context("failed to parse backup header")?;
    if header.format_version != FORMAT_VERSION {
        return app_error("暂不支持该备份格式版本");
    }

    Ok(header)
}

async fn merge_import(
    core: &CoreInner,
    root: &Path,
    import_settings: bool,
) -> Result<ImportHistoryBackupResult> {
    let _publication = core.cleanup.image_publication().await;
    let pool = core.db.pool().await;
    let db_path = root.join(DB_ARCHIVE_DIR).join(DB_FILENAME);
    let backup_pool = open_backup_db(&db_path).await?;
    let merged = merge_history(&pool, &backup_pool).await;
    // 先关掉备份库再处理结果：Windows 上打开着的文件删不掉，临时目录会留下来。
    backup_pool.close().await;
    let outcome = merged?;

    let resources = root.join(RESOURCES_ARCHIVE_DIR);
    let resources_dir = core.paths.resources_dir()?;
    let imported_resources = blocking(move || copy_dir_missing(&resources, &resources_dir)).await?;
    refresh_apps_registry(core).await;
    core.events.emit(CoreEvent::ClipboardReloaded);
    core.ocr.nudge();

    let mut imported_settings = false;
    if import_settings {
        let settings_path = root.join(CONFIG_ARCHIVE_DIR).join(SETTINGS_FILENAME);
        let patch = read_json_file(&settings_path)?;
        // 部分备份的设置是 `{}`：没有可合并的，也就不用通知宿主重新应用。
        if patch.as_object().is_none_or(|object| !object.is_empty()) {
            let delta = SettingsDelta::from_patch(&patch);
            let next = core.settings.update(patch)?;
            apply_imported_settings(core, next, delta);
            imported_settings = true;
        }
    }

    Ok(ImportHistoryBackupResult {
        strategy: BackupImportStrategy::Merge,
        imported_items: outcome.imported_items,
        skipped_items: outcome.skipped_items,
        imported_resources,
        imported_settings,
        requires_restart: false,
    })
}

async fn overwrite_import(
    core: &CoreInner,
    root: &Path,
    import_settings: bool,
) -> Result<ImportHistoryBackupResult> {
    // 设置先读进来校验：读不懂就什么都不动，避免换了库却换不了设置。
    let settings = if import_settings {
        Some(crate::settings::read_replacement(
            &root.join(CONFIG_ARCHIVE_DIR).join(SETTINGS_FILENAME),
        )?)
    } else {
        None
    };
    // 先在目标卷复制资源，空间不足时还没有动当前库与资源目录。
    let resources_src = root.join(RESOURCES_ARCHIVE_DIR);
    let staged_resources = if resources_src.exists() {
        let parent = core.paths.app_data_dir()?;
        Some(
            blocking(move || {
                let staged =
                    tempfile::tempdir_in(parent).context("failed to stage backup resources")?;
                copy_dir_all(&resources_src, staged.path())?;
                Ok(staged)
            })
            .await?,
        )
    } else {
        None
    };

    {
        // 先停新的采集，再等在途的入库与清理结束，换库期间没有人写库或删图片文件。
        let _pause = core.watcher_pause.pause_scoped();
        let _ocr = core.ocr.suspend().await;
        let _exclusive = core.cleanup.exclusive().await;
        let _upsert = core.upsert_lock.lock().await;

        let live = crate::db::db_path(&core.paths)?;
        let staged =
            stage_backup_database(&root.join(DB_ARCHIVE_DIR).join(DB_FILENAME), &live).await?;
        let counter = crate::db::sync::sync_counter(&core.db.pool().await).await?;
        let swap_error = Mutex::new(None::<AppError>);
        core.db
            .close_and_replace(|| {
                swap_in_staged_database(
                    core,
                    &live,
                    &staged,
                    staged_resources.as_ref().map(TempDir::path),
                    settings.as_ref(),
                    counter,
                    &swap_error,
                )
            })
            .await?;
        if let Some(err) = lock(&swap_error).take() {
            return Err(err);
        }
        refresh_apps_registry(core).await;
        core.events.emit(CoreEvent::ClipboardReloaded);
        core.ocr.nudge();
    }

    let imported_settings = settings.is_some();
    if let Some(settings) = settings {
        apply_imported_settings(core, settings, SettingsDelta::replaced());
    }

    Ok(ImportHistoryBackupResult {
        strategy: BackupImportStrategy::Overwrite,
        imported_items: 0,
        skipped_items: 0,
        imported_resources: 0,
        imported_settings,
        requires_restart: false,
    })
}

/// 把备份库复制到当前库旁边的暂存位置，统一迁移校验和后跑一遍迁移（旧版本的备份在这里升级）。
/// 任何一步失败都删掉暂存文件并报错，当前库原样不动。
async fn stage_backup_database(source: &Path, live: &Path) -> Result<PathBuf> {
    let staged = sibling(live, STAGED_DB_SUFFIX);
    remove_database_files(&staged)?;
    fs::copy(source, &staged)
        .with_context(|| format!("failed to stage backup database at {staged:?}"))?;

    match migrate_staged_database(&staged).await {
        Ok(()) => Ok(staged),
        Err(err) => {
            if let Err(cleanup) = remove_database_files(&staged) {
                log::warn!("remove staged backup database failed: {cleanup}");
            }
            Err(err)
        }
    }
}

async fn migrate_staged_database(staged: &Path) -> Result<()> {
    let options = SqliteConnectOptions::new()
        .filename(staged)
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Normal)
        .foreign_keys(true)
        .disable_statement_logging();
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .with_context(|| format!("failed to open staged backup database at {staged:?}"))?;

    let migrated = async {
        let adopted = crate::db::init::adopt_published_checksums(&pool).await?;
        if adopted > 0 {
            log::info!(
                "backup database: adopted {adopted} migration checksum(s) of the other platform"
            );
        }
        crate::db::MIGRATOR
            .run(&pool)
            .await
            .context("备份数据库与当前版本不兼容")?;
        checkpoint_database(&pool).await
    }
    .await;
    pool.close().await;

    migrated
}

/// 当前库连同 sidecar 与资源目录改名留底，换上暂存数据并保存设置后才删除旧数据。
/// 任何一步失败都把原来的库与资源换回去重新打开，
/// 错误记进 `failure`：连接池总要换上一个能用的，不能留下已关闭的那个。
async fn swap_in_staged_database(
    core: &CoreInner,
    live: &Path,
    staged: &Path,
    resources: Option<&Path>,
    settings: Option<&crate::settings::Settings>,
    counter: i64,
    failure: &Mutex<Option<AppError>>,
) -> Result<SqlitePool> {
    let previous = sibling(live, PREVIOUS_DB_SUFFIX);
    let live_resources = core.paths.resources_dir()?;
    let previous_resources = sibling(&live_resources, PREVIOUS_DB_SUFFIX);
    let mut resources_changed = false;
    let swapped = async {
        remove_database_files(&previous)?;
        move_database_files(live, &previous)?;
        move_database_files(staged, live)?;
        let pool = crate::db::init(&core.paths, core.db_max_connections).await?;
        let finish = async {
            // 导入记录不作为本机采集同步，计数器不回退；失败时也恢复原库。
            crate::db::sync::reset_after_import(&pool, counter).await?;
            if let Some(resources) = resources {
                if previous_resources.exists() {
                    fs::remove_dir_all(&previous_resources)
                        .context("failed to remove previous backup resources")?;
                }
                if live_resources.exists() {
                    fs::rename(&live_resources, &previous_resources)
                        .context("failed to retain current resources")?;
                }
                resources_changed = true;
                fs::rename(resources, &live_resources)
                    .context("failed to install staged resources")?;
            }
            if let Some(settings) = settings {
                core.settings.replace(settings.clone())?;
            }
            Ok::<_, AppError>(())
        }
        .await;
        if let Err(err) = finish {
            pool.close().await;
            return Err(err);
        }
        Ok(pool)
    }
    .await;

    match swapped {
        Ok(pool) => {
            if resources_changed && previous_resources.exists() {
                if let Err(err) = fs::remove_dir_all(&previous_resources) {
                    log::warn!("remove previous resources after backup overwrite failed: {err}");
                }
            }
            if let Err(err) = remove_database_files(&previous) {
                log::warn!("remove previous database after backup overwrite failed: {err}");
            }
            Ok(pool)
        }
        Err(err) => {
            log::error!("swap in the imported database failed, restoring the previous one: {err}");
            *lock(failure) = Some(err);
            if resources_changed {
                if live_resources.exists() {
                    fs::remove_dir_all(&live_resources)
                        .context("failed to remove imported resources during rollback")?;
                }
                if previous_resources.exists() {
                    fs::rename(&previous_resources, &live_resources)
                        .context("failed to restore previous resources")?;
                }
            }
            if previous.exists() {
                remove_database_files(live)?;
                move_database_files(&previous, live)?;
            }
            if let Err(err) = remove_database_files(staged) {
                log::warn!("remove staged backup database failed: {err}");
            }
            crate::db::init(&core.paths, core.db_max_connections).await
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn sibling(db: &Path, suffix: &str) -> PathBuf {
    PathBuf::from(format!("{}{suffix}", db.display()))
}

/// 主库与它的 `-wal` / `-shm` 一起改名，保持配对。
fn move_database_files(from: &Path, to: &Path) -> Result<()> {
    for suffix in std::iter::once("").chain(SQLITE_SIDECARS) {
        let source = sibling(from, suffix);
        if !source.exists() {
            continue;
        }
        let target = sibling(to, suffix);
        fs::rename(&source, &target)
            .with_context(|| format!("failed to move {source:?} to {target:?}"))?;
    }

    Ok(())
}

fn remove_database_files(db: &Path) -> Result<()> {
    for suffix in std::iter::once("").chain(SQLITE_SIDECARS) {
        let path = sibling(db, suffix);
        if path.exists() {
            fs::remove_file(&path).with_context(|| format!("failed to remove {path:?}"))?;
        }
    }

    Ok(())
}

struct MergeOutcome {
    imported_items: u64,
    skipped_items: u64,
}

async fn open_backup_db(path: &Path) -> Result<SqlitePool> {
    // 只读连接保留备份原有的日志模式；强制切换到 WAL 会尝试写入旧数据库。
    let options = SqliteConnectOptions::new()
        .filename(path)
        .read_only(true)
        .foreign_keys(true);

    Ok(SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .with_context(|| format!("failed to open backup database at {path:?}"))?)
}

async fn merge_history(current: &SqlitePool, backup: &SqlitePool) -> Result<MergeOutcome> {
    let mut tx = current.begin().await.context("failed to begin import")?;
    merge_groups(&mut tx, backup).await?;
    merge_apps(&mut tx, backup).await?;
    merge_file_type_icons(&mut tx, backup).await?;
    let outcome = merge_items(&mut tx, backup).await?;
    tx.commit().await.context("failed to commit import")?;

    Ok(outcome)
}

async fn merge_groups(tx: &mut sqlx::Transaction<'_, Sqlite>, backup: &SqlitePool) -> Result<()> {
    let rows = sqlx::query_as::<
        _,
        (
            String,
            String,
            String,
            bool,
            i64,
            DateTime<Utc>,
            DateTime<Utc>,
        ),
    >(
        "SELECT id, name, icon, is_hidden, sort_order, created_at, updated_at FROM clipboard_groups",
    )
    .fetch_all(backup)
    .await
    .context("failed to read backup groups")?;

    for row in rows {
        sqlx::query(
            "INSERT OR IGNORE INTO clipboard_groups \
             (id, name, icon, is_hidden, sort_order, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(row.0)
        .bind(row.1)
        .bind(row.2)
        .bind(row.3)
        .bind(row.4)
        .bind(row.5)
        .bind(row.6)
        .execute(&mut **tx)
        .await
        .context("failed to import group")?;
    }

    Ok(())
}

async fn merge_apps(tx: &mut sqlx::Transaction<'_, Sqlite>, backup: &SqlitePool) -> Result<()> {
    let rows = sqlx::query_as::<
        _,
        (
            String,
            String,
            Option<String>,
            String,
            DateTime<Utc>,
            DateTime<Utc>,
        ),
    >(
        "SELECT id, name, icon_file, platform, created_at, updated_at FROM clipboard_apps"
    )
    .fetch_all(backup)
    .await
    .context("failed to read backup apps")?;

    for row in rows {
        sqlx::query(
            "INSERT OR IGNORE INTO clipboard_apps \
             (id, name, icon_file, platform, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(row.0)
        .bind(row.1)
        .bind(row.2)
        .bind(row.3)
        .bind(row.4)
        .bind(row.5)
        .execute(&mut **tx)
        .await
        .context("failed to import app")?;
    }

    Ok(())
}

async fn merge_file_type_icons(
    tx: &mut sqlx::Transaction<'_, Sqlite>,
    backup: &SqlitePool,
) -> Result<()> {
    let rows = sqlx::query_as::<_, (String, String, String, DateTime<Utc>, DateTime<Utc>)>(
        "SELECT cache_key, platform, icon_file, created_at, updated_at FROM file_type_icons",
    )
    .fetch_all(backup)
    .await
    .context("failed to read backup file type icons")?;

    for row in rows {
        sqlx::query(
            "INSERT OR IGNORE INTO file_type_icons \
             (cache_key, platform, icon_file, created_at, updated_at) VALUES (?, ?, ?, ?, ?)",
        )
        .bind(row.0)
        .bind(row.1)
        .bind(row.2)
        .bind(row.3)
        .bind(row.4)
        .execute(&mut **tx)
        .await
        .context("failed to import file type icon")?;
    }

    Ok(())
}

async fn merge_items(
    tx: &mut sqlx::Transaction<'_, Sqlite>,
    backup: &SqlitePool,
) -> Result<MergeOutcome> {
    let rows =
        sqlx::query_as::<_, BackupItemRow>("SELECT * FROM clipboard_items ORDER BY created_at ASC")
            .fetch_all(backup)
            .await
            .context("failed to read backup items")?;

    let mut imported_items = 0;
    let mut skipped_items = 0;
    for row in rows {
        let exists: Option<i64> = sqlx::query_scalar(
            "SELECT 1 FROM clipboard_items WHERE kind = ? AND content_hash = ? LIMIT 1",
        )
        .bind(row.kind.as_str())
        .bind(row.content_hash.as_str())
        .fetch_optional(&mut **tx)
        .await
        .context("failed to check duplicate item")?;
        if exists.is_some() {
            skipped_items += 1;
            continue;
        }

        // 同一记录编辑后 id 不变；内容不同的备份版本作为新记录保留。
        let id_exists: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM clipboard_items WHERE id = ?)")
                .bind(&row.id)
                .fetch_one(&mut **tx)
                .await
                .context("failed to check imported item id")?;
        let id = if id_exists {
            uuid::Uuid::new_v4().to_string()
        } else {
            row.id
        };

        // 旧备份缺列或没有有效的使用时间时，才回落到列表更新时间。
        let last_used_at = row
            .last_used_at
            .as_deref()
            .and_then(|value| value.parse::<DateTime<Utc>>().ok())
            .unwrap_or(row.updated_at);
        let inserted = sqlx::query(
            "INSERT OR IGNORE INTO clipboard_items \
             (id, kind, sub_kind, group_id, source_app_id, content, content_hash, search_text, \
              summary, file_types, size, width, height, use_count, is_favorite, is_pinned, favorite_order, pin_order, is_sensitive, platform, note, \
              created_at, updated_at, last_used_at, original_content) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?,
                     CASE WHEN ? THEN 1 + COALESCE((SELECT MAX(favorite_order) FROM clipboard_items), 0) END,
                     CASE WHEN ? THEN 1 + COALESCE((SELECT MAX(pin_order) FROM clipboard_items), 0) END,
                     ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(id)
        .bind(row.kind)
        .bind(row.sub_kind)
        .bind(row.group_id)
        .bind(row.source_app_id)
        .bind(row.content)
        .bind(row.content_hash)
        .bind(row.search_text)
        .bind(row.summary)
        .bind(row.file_types)
        .bind(row.size)
        .bind(row.width)
        .bind(row.height)
        .bind(row.use_count)
        .bind(row.is_favorite)
        .bind(row.is_pinned)
        .bind(row.is_favorite)
        .bind(row.is_pinned)
        .bind(row.is_sensitive)
        .bind(row.platform)
        .bind(row.note)
        .bind(row.created_at)
        .bind(row.updated_at)
        .bind(last_used_at)
        .bind(row.original_content)
        .execute(&mut **tx)
        .await
        .context("failed to import item")?
        .rows_affected();
        imported_items += inserted;
        skipped_items += u64::from(inserted == 0);
    }

    Ok(MergeOutcome {
        imported_items,
        skipped_items,
    })
}

#[derive(sqlx::FromRow)]
struct BackupItemRow {
    id: String,
    kind: String,
    sub_kind: Option<String>,
    group_id: Option<String>,
    source_app_id: Option<String>,
    content: String,
    content_hash: String,
    search_text: Option<String>,
    summary: Option<String>,
    file_types: Option<String>,
    size: Option<i64>,
    width: Option<i64>,
    height: Option<i64>,
    use_count: i64,
    is_favorite: bool,
    is_pinned: bool,
    is_sensitive: bool,
    platform: String,
    note: Option<String>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    #[sqlx(default)]
    last_used_at: Option<String>,
    /// 已发布的旧备份没有此列；SELECT * 配合缺列默认值保留旧库合并能力。
    #[sqlx(default)]
    original_content: Option<String>,
}

/// 导入后从数据库重建来源应用内存缓存。
async fn refresh_apps_registry(core: &CoreInner) {
    if let Err(err) = core.apps.load_from_db(core).await {
        log::warn!("refresh apps registry after backup import failed: {err}");
    }
}

/// 导入会整体改设置：通知宿主重注册快捷键、同步托盘与自启并刷新界面；历史设置变了顺带请求一轮清理，
/// 同步设置变了按新设置启停局域网同步。
fn apply_imported_settings(
    core: &CoreInner,
    settings: crate::settings::Settings,
    delta: SettingsDelta,
) {
    if delta.touches("clipboard.ocr") {
        core.ocr.settings_changed();
    }
    if delta.touches("clipboard.history") {
        clipboard::cleanup::request(core);
    }
    if delta.touches("sync") {
        crate::sync::settings_changed(core);
    }
    core.events.emit(CoreEvent::SettingsUpdated {
        settings: Arc::new(settings),
        delta,
    });
}

fn read_json_file(path: &Path) -> Result<serde_json::Value> {
    let content = fs::read_to_string(path).with_context(|| format!("failed to read {path:?}"))?;
    Ok(serde_json::from_str(&content).with_context(|| format!("failed to parse {path:?}"))?)
}

fn copy_file_to(src: &Path, dst: &Path) -> Result<()> {
    let parent = dst
        .parent()
        .ok_or_else(|| anyhow!("target path has no parent"))?;
    fs::create_dir_all(parent).with_context(|| format!("failed to create {parent:?}"))?;
    fs::copy(src, dst).with_context(|| format!("failed to copy {src:?} to {dst:?}"))?;

    Ok(())
}

fn copy_dir_all(src: &Path, dst: &Path) -> Result<u64> {
    if !src.exists() {
        return Ok(0);
    }

    let mut copied = 0;
    for entry in WalkDir::new(src) {
        let entry = entry.with_context(|| format!("failed to enumerate {src:?}"))?;
        let path = entry.path();
        let relative = path
            .strip_prefix(src)
            .with_context(|| format!("failed to strip {src:?} from {path:?}"))?;
        if relative.as_os_str().is_empty() {
            continue;
        }

        let target = dst.join(relative);
        if entry.file_type().is_dir() {
            fs::create_dir_all(&target)
                .with_context(|| format!("failed to create directory {target:?}"))?;
            continue;
        }

        copy_file_to(path, &target)?;
        copied += 1;
    }

    Ok(copied)
}

fn copy_dir_missing(src: &Path, dst: &Path) -> Result<u64> {
    if !src.exists() {
        return Ok(0);
    }

    let mut copied = 0;
    for entry in WalkDir::new(src)
        .into_iter()
        .filter_map(std::result::Result::ok)
    {
        let path = entry.path();
        if !entry.file_type().is_file() {
            continue;
        }

        let relative = path
            .strip_prefix(src)
            .with_context(|| format!("failed to strip {src:?} from {path:?}"))?;
        let target = dst.join(relative);
        if target.exists() {
            continue;
        }

        copy_file_to(path, &target)?;
        copied += 1;
    }

    Ok(copied)
}

fn copy_file_into_writer<W: Write>(path: &Path, writer: &mut W) -> Result<()> {
    let file = File::open(path).with_context(|| format!("failed to open {path:?}"))?;
    let mut reader = BufReader::new(file);
    std::io::copy(&mut reader, writer).with_context(|| format!("failed to copy {path:?}"))?;

    Ok(())
}

fn app_error<T>(message: impl Into<String>) -> Result<T> {
    Err(AppError::Other(anyhow!(message.into())))
}

#[cfg(test)]
mod tests;
