//! app data 目录解析的单一入口。
//!
//! 所有持久化位置都从这里取根，布局与 1.x 完全一致（2.0 直接用 1.x 的数据目录）：
//! - `db/`、`resources/`、`config/`、`state/` 这些语义目录名；
//! - 开发 / 生产环境的数据隔离：dev 数据落 `dev/`、release 数据落 `prod/`，互不污染，
//!   便于导入导出 / 备份 / 迁移按环境整目录操作。环境由宿主经 [`AppEnv`] 传入。
//! - 自定义数据目录：`<app_local_data>/<env>/storage.json` 始终作为启动锚点，
//!   真实数据根由该 bootstrap manifest 指向。
//! - 便携模式（见 [`crate::portable`]）：锚点和数据根都换到 exe 旁的 `data/<env>`，不支持自定义目录。
//!
//! 只解析路径、不建目录：创建行为是各调用方特化的（settings/window/db 建自己的目录、
//! 图片/图标写时懒建），塞进这里会改掉懒建语义。叶子文件名（`clipboard.db` /
//! `settings.json` 等）仍由各模块自己拥有——要统一的是「根在哪」。

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::Context;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::env::AppEnv;
use crate::error::Result;
use crate::portable;

/// SQLite 主库与 WAL / SHM sidecar 所在目录名，挂在 [`CorePaths::app_data_dir`] 下。
const DB_DIR: &str = "db";
/// 资源文件（图片、应用图标）的公共父目录名，挂在 [`CorePaths::app_data_dir`] 下。
const RESOURCES_DIR: &str = "resources";
/// 用户配置目录名，挂在 [`CorePaths::app_data_dir`] 下。
const CONFIG_DIR: &str = "config";
/// 本机运行状态目录名，挂在 [`CorePaths::app_data_dir`] 下。
const STATE_DIR: &str = "state";
/// 局域网同步身份目录名，挂在 [`CorePaths::bootstrap_dir`] 下。
const SYNC_DIR: &str = "sync";
pub(crate) const EXTENSIONS_DIR: &str = "extensions";
/// 安装版的日志目录名，挂在 `<app_local_data>` 下（Windows）。
#[cfg(not(target_os = "macos"))]
const LOGS_DIR: &str = "logs";
/// 1.x 的 WebView2 用户数据目录名（2.0 不用 WebView），见 [`CorePaths::legacy_webview_dir`]。
#[cfg(target_os = "windows")]
const LEGACY_WEBVIEW_DIR: &str = "EBWebView";
/// 固定留在 `<app_local_data>/<env>` 的 bootstrap manifest 文件名。
const STORAGE_MANIFEST_FILENAME: &str = "storage.json";
/// 写入真实数据根的 identity manifest 文件名，用于识别 KwikPaste 数据目录。
const STORAGE_IDENTITY_FILENAME: &str = ".kwikpaste-storage.json";
/// 用户选择父目录后创建的数据子目录名。
const CUSTOM_DATA_DIR_NAME: &str = "KwikPasteData";
/// 存储 manifest 格式版本。
const STORAGE_MANIFEST_VERSION: u16 = 1;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StorageLocation {
    pub current_path: String,
    pub default_path: String,
    pub is_custom: bool,
    /// 启动时不可用的自定义目录；本次运行使用默认目录，但 manifest 保留原位置。
    pub unavailable_custom_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StorageManifest {
    version: u16,
    environment: String,
    data_dir: PathBuf,
    updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StorageIdentity {
    version: u16,
    environment: String,
    created_at: DateTime<Utc>,
}

/// 一次运行里固定不变的根目录；其余目录都由它推导。
#[derive(Debug, Clone)]
pub struct CorePaths {
    env: AppEnv,
    /// 安装版的 `<app_local_data>`：Windows `%LOCALAPPDATA%\<id>`，macOS `~/Library/Application Support/<id>`。
    local_data_root: PathBuf,
    /// 安装版的系统日志目录：Windows `%LOCALAPPDATA%\<id>\logs`，macOS `~/Library/Logs/<id>`。
    log_root: PathBuf,
    /// 便携数据根 `<exe 目录>/data`；非便携模式为 `None`。
    portable_root: Option<PathBuf>,
    resolved: Arc<Mutex<Option<ResolvedStorage>>>,
}

#[derive(Debug, Clone)]
struct ResolvedStorage {
    data_dir: PathBuf,
    unavailable_custom_path: Option<PathBuf>,
}

impl PartialEq for CorePaths {
    fn eq(&self, other: &Self) -> bool {
        self.env == other.env
            && self.local_data_root == other.local_data_root
            && self.log_root == other.log_root
            && self.portable_root == other.portable_root
    }
}

impl Eq for CorePaths {}

impl CorePaths {
    /// 用宿主给定的根目录构造，测试与自定义宿主用。
    pub fn new(
        env: AppEnv,
        local_data_root: PathBuf,
        log_root: PathBuf,
        portable_root: Option<PathBuf>,
    ) -> Self {
        Self {
            env,
            local_data_root,
            log_root,
            portable_root,
            resolved: Arc::default(),
        }
    }

    /// 按 1.x（Tauri 2）同样的规则解析本机根目录，并检测便携模式。只算路径，不碰磁盘上的数据。
    pub fn for_native(identifier: &str, env: AppEnv) -> Result<Self> {
        let local_data_root = dirs::data_local_dir()
            .context("failed to resolve app local data dir")?
            .join(identifier);

        #[cfg(target_os = "macos")]
        let log_root = dirs::home_dir()
            .context("failed to resolve app log dir")?
            .join("Library/Logs")
            .join(identifier);
        #[cfg(not(target_os = "macos"))]
        let log_root = local_data_root.join(LOGS_DIR);

        Ok(Self::new(
            env,
            local_data_root,
            log_root,
            portable::detect(),
        ))
    }

    pub fn env(&self) -> AppEnv {
        self.env
    }

    pub fn is_portable(&self) -> bool {
        self.portable_root.is_some()
    }

    /// 便携数据根 `<exe 目录>/data`；非便携模式返回 `None`。
    pub fn portable_root(&self) -> Option<&Path> {
        self.portable_root.as_deref()
    }

    /// `<app_local_data>/<env>`：固定启动锚点。自定义数据目录启用后，这里仍保留
    /// `storage.json` 用于解析真实数据根。便携模式换成 `<exe 目录>/data/<env>`。
    pub fn bootstrap_dir(&self) -> PathBuf {
        if let Some(root) = &self.portable_root {
            return root.join(self.env.dir_name());
        }

        self.local_data_root.join(self.env.dir_name())
    }

    /// 默认数据根。未启用自定义数据目录时，真实数据仍落在 `<app_local_data>/<env>`。
    pub fn default_data_dir(&self) -> PathBuf {
        self.bootstrap_dir()
    }

    /// 用户选择父目录后创建 KwikPaste 自有数据子目录。
    pub fn custom_data_dir(&self, parent: &Path) -> PathBuf {
        parent.join(CUSTOM_DATA_DIR_NAME).join(self.env.dir_name())
    }

    /// 返回当前真实数据根、默认数据根，以及是否处于自定义目录。
    pub fn storage_location(&self) -> Result<StorageLocation> {
        let resolved = self.resolved_storage()?;
        let current = resolved.data_dir;
        let default = self.default_data_dir();
        Ok(StorageLocation {
            is_custom: current != default,
            current_path: current.to_string_lossy().into_owned(),
            default_path: default.to_string_lossy().into_owned(),
            unavailable_custom_path: resolved
                .unavailable_custom_path
                .map(|path| path.to_string_lossy().into_owned()),
        })
    }

    /// `<data_root>`：当前环境所有持久化位置的根；其下按语义拆分为 db、resources、
    /// config 与 state。真实根由 bootstrap manifest 决定。
    ///
    /// 便携模式固定用 exe 旁的数据根、不读写 manifest：manifest 记的是绝对路径，
    /// U 盘换了盘符就会失效。
    pub fn app_data_dir(&self) -> Result<PathBuf> {
        Ok(self.resolved_storage()?.data_dir)
    }

    /// 克隆共享首次解析结果，避免挂载变化或外部 manifest 改动在运行中移动数据根。
    fn resolved_storage(&self) -> Result<ResolvedStorage> {
        let mut resolved = self.resolved.lock().unwrap_or_else(|err| err.into_inner());
        if let Some(resolved) = resolved.as_ref() {
            return Ok(resolved.clone());
        }
        let next = self.resolve_storage()?;
        *resolved = Some(next.clone());
        Ok(next)
    }

    fn resolve_storage(&self) -> Result<ResolvedStorage> {
        let bootstrap = self.bootstrap_dir();
        let default = self.default_data_dir();
        let manifest_path = storage_manifest_path(&bootstrap);

        fs::create_dir_all(&bootstrap)
            .with_context(|| format!("failed to create bootstrap dir at {bootstrap:?}"))?;

        if self.is_portable() {
            return Ok(ResolvedStorage {
                data_dir: default,
                unavailable_custom_path: None,
            });
        }

        let manifest = match read_storage_manifest(&manifest_path) {
            Ok(Some(manifest)) => manifest,
            Ok(None) => {
                let manifest = self.storage_manifest(default.clone());
                write_storage_manifest(&manifest_path, &manifest)?;
                manifest
            }
            Err(err) => {
                log::warn!("storage manifest unreadable, using default data dir: {err}");
                let manifest = self.storage_manifest(default.clone());
                write_storage_manifest(&manifest_path, &manifest)?;
                manifest
            }
        };

        if manifest.environment != self.env.dir_name()
            || manifest.version != STORAGE_MANIFEST_VERSION
        {
            log::warn!("storage manifest metadata mismatch, using default data dir");
            let manifest = self.storage_manifest(default.clone());
            write_storage_manifest(&manifest_path, &manifest)?;
            return Ok(ResolvedStorage {
                data_dir: default,
                unavailable_custom_path: None,
            });
        }

        if !manifest.data_dir.exists() {
            log::warn!(
                "storage data dir {:?} is missing, falling back to default data dir",
                manifest.data_dir
            );
            return Ok(ResolvedStorage {
                data_dir: default.clone(),
                unavailable_custom_path: (manifest.data_dir != default)
                    .then_some(manifest.data_dir),
            });
        }

        if manifest.data_dir != default {
            match self.has_valid_storage_identity(&manifest.data_dir) {
                Ok(true) => {}
                Ok(false) => {
                    log::warn!(
                        "storage data dir {:?} identity mismatch, falling back to default data dir",
                        manifest.data_dir
                    );
                    let manifest = self.storage_manifest(default.clone());
                    write_storage_manifest(&manifest_path, &manifest)?;
                    return Ok(ResolvedStorage {
                        data_dir: default,
                        unavailable_custom_path: None,
                    });
                }
                Err(err) => {
                    log::warn!(
                        "storage data dir {:?} identity unreadable, falling back to default data dir: {err}",
                        manifest.data_dir
                    );
                    let manifest = self.storage_manifest(default.clone());
                    write_storage_manifest(&manifest_path, &manifest)?;
                    return Ok(ResolvedStorage {
                        data_dir: default,
                        unavailable_custom_path: None,
                    });
                }
            }
        }

        Ok(ResolvedStorage {
            data_dir: manifest.data_dir,
            unavailable_custom_path: None,
        })
    }

    /// 将当前真实数据根切换到指定目录。调用方负责在写入前完成数据迁移。
    pub fn set_app_data_dir(&self, data_dir: PathBuf) -> Result<()> {
        let mut resolved = self.resolved.lock().unwrap_or_else(|err| err.into_inner());
        let bootstrap = self.bootstrap_dir();
        fs::create_dir_all(&bootstrap)
            .with_context(|| format!("failed to create bootstrap dir at {bootstrap:?}"))?;

        if !data_dir.join(STORAGE_IDENTITY_FILENAME).exists() {
            self.write_storage_identity(&data_dir)?;
        }
        write_storage_manifest(
            &storage_manifest_path(&bootstrap),
            &self.storage_manifest(data_dir.clone()),
        )?;
        *resolved = Some(ResolvedStorage {
            data_dir,
            unavailable_custom_path: None,
        });
        Ok(())
    }

    /// 写入真实数据根 identity manifest，供迁移目标目录校验。
    pub fn write_storage_identity(&self, data_dir: &Path) -> Result<()> {
        fs::create_dir_all(data_dir)
            .with_context(|| format!("failed to create storage dir at {data_dir:?}"))?;
        let identity = StorageIdentity {
            version: STORAGE_MANIFEST_VERSION,
            environment: self.env.dir_name().to_owned(),
            created_at: Utc::now(),
        };
        let path = data_dir.join(STORAGE_IDENTITY_FILENAME);
        let json = serde_json::to_string_pretty(&identity)
            .context("failed to serialize storage identity")?;

        fs::write(&path, json)
            .with_context(|| format!("failed to write storage identity {path:?}"))?;
        Ok(())
    }

    /// 校验目标目录是否可作为 KwikPaste 数据根：空目录允许使用，已有 identity 时必须匹配。
    pub fn validate_storage_target(&self, data_dir: &Path) -> Result<()> {
        let identity_path = data_dir.join(STORAGE_IDENTITY_FILENAME);
        if identity_path.exists() {
            let content = fs::read_to_string(&identity_path)
                .with_context(|| format!("failed to read storage identity {identity_path:?}"))?;
            let identity: StorageIdentity =
                serde_json::from_str(&content).context("failed to parse storage identity")?;
            if identity.version == STORAGE_MANIFEST_VERSION
                && identity.environment == self.env.dir_name()
            {
                return Ok(());
            }

            return Err(anyhow::anyhow!("目标目录不是当前环境的 KwikPaste 数据目录").into());
        }

        if data_dir.exists()
            && fs::read_dir(data_dir)
                .with_context(|| format!("failed to read storage target {data_dir:?}"))?
                .next()
                .is_some()
        {
            return Err(anyhow::anyhow!("目标 KwikPaste 数据目录已存在且不是有效数据目录").into());
        }

        Ok(())
    }

    /// 已匹配 identity 的目标仍可能保存另一份历史，不能用当前运行覆盖它。
    pub(crate) fn storage_target_has_data(&self, data_dir: &Path) -> Result<bool> {
        let db = data_dir.join(DB_DIR);
        if !db.exists() {
            return Ok(false);
        }
        Ok(fs::read_dir(&db)
            .with_context(|| format!("failed to read storage database dir {db:?}"))?
            .next()
            .is_some())
    }

    /// 日志目录：便携模式在 `data/logs`，否则是系统日志目录。与 1.x 日志插件的落盘位置一致。
    pub fn log_dir(&self) -> PathBuf {
        if let Some(root) = &self.portable_root {
            return root.join(portable::LOGS_DIR_NAME);
        }

        self.log_root.clone()
    }

    /// `<app_data_dir>/db`：SQLite 主库与 sidecar 的目录。
    pub fn db_dir(&self) -> Result<PathBuf> {
        Ok(self.app_data_dir()?.join(DB_DIR))
    }

    /// `<app_data_dir>/resources`：图片、应用图标等资源文件的公共父目录。
    pub fn resources_dir(&self) -> Result<PathBuf> {
        Ok(self.app_data_dir()?.join(RESOURCES_DIR))
    }

    /// `<app_data_dir>/config`：用户偏好配置目录。
    pub fn config_dir(&self) -> Result<PathBuf> {
        Ok(self.app_data_dir()?.join(CONFIG_DIR))
    }

    /// `<app_data_dir>/state`：窗口位置等本机运行状态目录。
    pub fn state_dir(&self) -> Result<PathBuf> {
        Ok(self.app_data_dir()?.join(STATE_DIR))
    }

    /// 1.x 的 WebView2 用户数据目录：安装版 `<app_local_data>\EBWebView`，便携版 `<exe 目录>\data\EBWebView`
    /// （与 1.x 的位置一致，同 `<env>`、`logs` 同级）。2.0 不用，见 [`crate::legacy`]。
    #[cfg(target_os = "windows")]
    pub fn legacy_webview_dir(&self) -> PathBuf {
        self.portable_root
            .as_deref()
            .unwrap_or(&self.local_data_root)
            .join(LEGACY_WEBVIEW_DIR)
    }

    /// `<bootstrap>/sync`：局域网同步的设备密钥与已配对设备。
    ///
    /// 放在启动锚点而不是数据根：不随自定义数据目录搬走，也不进备份包——
    /// 这些是这台电脑自己的身份，导入到别的电脑就成了冒充。
    pub fn sync_dir(&self) -> PathBuf {
        self.bootstrap_dir().join(SYNC_DIR)
    }

    /// `<bootstrap>/extensions`：扩展可执行文件和自有状态，不随自定义数据迁移，也不进入备份。
    pub fn extensions_dir(&self) -> PathBuf {
        self.bootstrap_dir().join(EXTENSIONS_DIR)
    }

    fn storage_manifest(&self, data_dir: PathBuf) -> StorageManifest {
        StorageManifest {
            version: STORAGE_MANIFEST_VERSION,
            environment: self.env.dir_name().to_owned(),
            data_dir,
            updated_at: Utc::now(),
        }
    }

    fn has_valid_storage_identity(&self, data_dir: &Path) -> Result<bool> {
        let identity_path = data_dir.join(STORAGE_IDENTITY_FILENAME);
        if !identity_path.exists() {
            return Ok(false);
        }

        let content = fs::read_to_string(&identity_path)
            .with_context(|| format!("failed to read storage identity {identity_path:?}"))?;
        let identity: StorageIdentity =
            serde_json::from_str(&content).context("failed to parse storage identity")?;
        Ok(identity.version == STORAGE_MANIFEST_VERSION
            && identity.environment == self.env.dir_name())
    }
}

/// 识别常见云同步目录，不读写目标目录；宿主用于迁移前提示 SQLite 同步风险。
pub fn cloud_sync_provider(path: &Path) -> Option<&'static str> {
    let onedrive: Vec<PathBuf> = ["OneDrive", "OneDriveConsumer", "OneDriveCommercial"]
        .into_iter()
        .filter_map(std::env::var_os)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .collect();
    cloud_sync_provider_with_roots(path, &onedrive, dirs::home_dir().as_deref())
}

/// 将环境根作为输入，匹配按路径组件而不是子串，避免 DropboxBackup 等误报。
fn cloud_sync_provider_with_roots(
    path: &Path,
    onedrive: &[PathBuf],
    home: Option<&Path>,
) -> Option<&'static str> {
    fn components(path: &Path) -> Vec<String> {
        path.components()
            .map(|component| component.as_os_str().to_string_lossy().to_lowercase())
            .collect()
    }
    let parts = components(path);
    if onedrive
        .iter()
        .any(|root| parts.starts_with(&components(root)))
    {
        return Some("OneDrive");
    }
    if home
        .is_some_and(|home| parts.starts_with(&components(&home.join("Library/Mobile Documents"))))
    {
        return Some("iCloud Drive");
    }
    parts.iter().find_map(|part| match part.as_str() {
        "dropbox" => Some("Dropbox"),
        "google drive" => Some("Google Drive"),
        "iclouddrive" => Some("iCloud Drive"),
        _ => None,
    })
}

fn storage_manifest_path(bootstrap: &Path) -> PathBuf {
    bootstrap.join(STORAGE_MANIFEST_FILENAME)
}

fn read_storage_manifest(path: &Path) -> Result<Option<StorageManifest>> {
    if !path.exists() {
        return Ok(None);
    }

    let content = fs::read_to_string(path).with_context(|| format!("failed to read {path:?}"))?;
    Ok(Some(
        serde_json::from_str(&content).with_context(|| format!("failed to parse {path:?}"))?,
    ))
}

fn write_storage_manifest(path: &Path, manifest: &StorageManifest) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create manifest dir at {parent:?}"))?;
    }

    let json =
        serde_json::to_string_pretty(manifest).context("failed to serialize storage manifest")?;
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, json).with_context(|| format!("failed to write {tmp:?}"))?;
    fs::rename(&tmp, path).with_context(|| format!("failed to promote {tmp:?} to {path:?}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::env::APP_IDENTIFIER;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            let path = std::env::temp_dir()
                .join(format!("kwikpaste-storage-paths-{}", uuid::Uuid::new_v4()));
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

    /// 安装版布局：本机根目录都放在临时目录里，测试不碰真实数据目录。
    fn installed(temp: &TempDir, env: AppEnv) -> CorePaths {
        let local = temp.path().join("local").join(APP_IDENTIFIER);
        CorePaths::new(env, local.clone(), local.join("logs"), None)
    }

    fn paths() -> CorePaths {
        CorePaths::new(
            AppEnv::Prod,
            PathBuf::from("unused-local"),
            PathBuf::from("unused-logs"),
            None,
        )
    }

    #[test]
    fn empty_storage_target_is_allowed() {
        let temp = TempDir::new();

        paths().validate_storage_target(temp.path()).unwrap();
    }

    #[test]
    fn custom_data_dir_uses_named_data_container() {
        let temp = TempDir::new();
        let paths = paths();

        assert_eq!(
            paths.custom_data_dir(temp.path()),
            temp.path()
                .join("KwikPasteData")
                .join(paths.env().dir_name())
        );
    }

    #[test]
    fn storage_target_with_matching_identity_is_allowed() {
        let temp = TempDir::new();
        let paths = paths();

        paths.write_storage_identity(temp.path()).unwrap();

        paths.validate_storage_target(temp.path()).unwrap();
    }

    #[test]
    fn storage_target_with_mismatched_identity_is_rejected() {
        let temp = TempDir::new();
        let identity = StorageIdentity {
            version: STORAGE_MANIFEST_VERSION,
            environment: "other".to_owned(),
            created_at: Utc::now(),
        };
        fs::write(
            temp.path().join(STORAGE_IDENTITY_FILENAME),
            serde_json::to_string_pretty(&identity).unwrap(),
        )
        .unwrap();

        assert!(paths().validate_storage_target(temp.path()).is_err());
    }

    #[test]
    fn non_empty_storage_target_without_identity_is_rejected() {
        let temp = TempDir::new();
        fs::write(temp.path().join("random.txt"), "not KwikPaste").unwrap();

        assert!(paths().validate_storage_target(temp.path()).is_err());
    }

    #[test]
    fn cloud_sync_provider_matches_known_roots_and_components() {
        let home = PathBuf::from("C:/Users/test");
        let onedrive = [PathBuf::from("C:/Users/test/OneDrive")];
        assert_eq!(
            cloud_sync_provider_with_roots(
                &PathBuf::from("C:/Users/test/OneDrive/KwikPaste"),
                &onedrive,
                Some(&home)
            ),
            Some("OneDrive")
        );
        assert_eq!(
            cloud_sync_provider_with_roots(
                &home.join("Library/Mobile Documents/com~apple~CloudDocs"),
                &[],
                Some(&home)
            ),
            Some("iCloud Drive")
        );
        assert_eq!(
            cloud_sync_provider_with_roots(
                &PathBuf::from("D:/Dropbox/KwikPaste"),
                &[],
                Some(&home)
            ),
            Some("Dropbox")
        );
        assert_eq!(
            cloud_sync_provider_with_roots(
                &PathBuf::from("D:/DropboxBackup/KwikPaste"),
                &[],
                Some(&home)
            ),
            None
        );
    }

    #[test]
    fn installed_layout_separates_dev_and_prod() {
        let temp = TempDir::new();
        let local = temp.path().join("local").join(APP_IDENTIFIER);
        let prod = installed(&temp, AppEnv::Prod);
        let dev = installed(&temp, AppEnv::Dev);

        assert_eq!(prod.bootstrap_dir(), local.join("prod"));
        assert_eq!(dev.bootstrap_dir(), local.join("dev"));
        assert_eq!(prod.sync_dir(), local.join("prod").join("sync"));
        assert_eq!(prod.log_dir(), local.join("logs"));
        assert_eq!(
            prod.config_dir().unwrap(),
            local.join("prod").join("config")
        );
        assert_eq!(dev.db_dir().unwrap(), local.join("dev").join("db"));
        assert!(!prod.is_portable());
    }

    #[test]
    fn first_resolution_writes_manifest_pointing_at_default_dir() {
        let temp = TempDir::new();
        let paths = installed(&temp, AppEnv::Prod);

        let data_dir = paths.app_data_dir().unwrap();
        let manifest = read_storage_manifest(&paths.bootstrap_dir().join("storage.json"))
            .unwrap()
            .unwrap();

        assert_eq!(data_dir, paths.default_data_dir());
        assert_eq!(manifest.data_dir, data_dir);
        assert_eq!(manifest.environment, "prod");
        assert_eq!(manifest.version, 1);
        assert!(!paths.storage_location().unwrap().is_custom);
    }

    #[test]
    fn custom_data_dir_with_identity_is_used() {
        let temp = TempDir::new();
        let paths = installed(&temp, AppEnv::Prod);
        let custom = paths.custom_data_dir(&temp.path().join("elsewhere"));

        paths.set_app_data_dir(custom.clone()).unwrap();

        assert_eq!(paths.app_data_dir().unwrap(), custom);
        assert_eq!(paths.resources_dir().unwrap(), custom.join("resources"));
        assert_eq!(paths.state_dir().unwrap(), custom.join("state"));
        assert!(paths.storage_location().unwrap().is_custom);
        // 同步身份留在启动锚点，不跟着数据根走。
        assert_eq!(paths.sync_dir(), paths.bootstrap_dir().join("sync"));
    }

    #[test]
    fn custom_data_dir_of_other_env_falls_back_to_default() {
        let temp = TempDir::new();
        let prod = installed(&temp, AppEnv::Prod);
        let dev = installed(&temp, AppEnv::Dev);
        let custom = temp.path().join("shared");

        dev.write_storage_identity(&custom).unwrap();
        write_storage_manifest(
            &prod.bootstrap_dir().join("storage.json"),
            &prod.storage_manifest(custom),
        )
        .unwrap();

        assert_eq!(prod.app_data_dir().unwrap(), prod.default_data_dir());
    }

    #[test]
    fn missing_custom_data_dir_falls_back_to_default() {
        let temp = TempDir::new();
        let paths = installed(&temp, AppEnv::Prod);
        let custom = paths.custom_data_dir(&temp.path().join("usb"));

        let manifest = paths.storage_manifest(custom.clone());
        fs::create_dir_all(paths.bootstrap_dir()).unwrap();
        write_storage_manifest(
            &paths.bootstrap_dir().join(STORAGE_MANIFEST_FILENAME),
            &manifest,
        )
        .unwrap();
        fs::remove_dir_all(custom).ok();

        assert_eq!(paths.app_data_dir().unwrap(), paths.default_data_dir());
        let persisted =
            read_storage_manifest(&paths.bootstrap_dir().join(STORAGE_MANIFEST_FILENAME))
                .unwrap()
                .unwrap();
        assert_eq!(
            persisted.data_dir,
            paths.custom_data_dir(&temp.path().join("usb"))
        );
    }

    #[test]
    fn missing_custom_root_stays_fallback_when_drive_reappears() {
        let temp = TempDir::new();
        let paths = installed(&temp, AppEnv::Prod);
        let custom = paths.custom_data_dir(&temp.path().join("usb"));
        let manifest = paths.storage_manifest(custom.clone());
        fs::create_dir_all(paths.bootstrap_dir()).unwrap();
        write_storage_manifest(
            &paths.bootstrap_dir().join(STORAGE_MANIFEST_FILENAME),
            &manifest,
        )
        .unwrap();
        assert_eq!(paths.app_data_dir().unwrap(), paths.default_data_dir());
        paths.write_storage_identity(&custom).unwrap();
        assert_eq!(paths.app_data_dir().unwrap(), paths.default_data_dir());
    }

    #[test]
    fn portable_layout_lives_next_to_exe_and_ignores_manifest() {
        let temp = TempDir::new();
        let local = temp.path().join("local").join(APP_IDENTIFIER);
        let portable_root = temp.path().join("usb").join("data");
        let paths = CorePaths::new(
            AppEnv::Prod,
            local.clone(),
            local.join("logs"),
            Some(portable_root.clone()),
        );

        assert!(paths.is_portable());
        assert_eq!(paths.bootstrap_dir(), portable_root.join("prod"));
        assert_eq!(paths.log_dir(), portable_root.join("logs"));
        assert_eq!(paths.app_data_dir().unwrap(), portable_root.join("prod"));
        assert!(!portable_root.join("prod").join("storage.json").exists());
        assert!(!local.exists());
    }

    // 只算路径，不调用会建目录、写 manifest 的方法，避免碰到本机已安装版本的数据。
    #[test]
    fn native_layout_matches_tauri_resolution() {
        let paths = CorePaths::for_native(APP_IDENTIFIER, AppEnv::Prod).unwrap();
        let local = dirs::data_local_dir().unwrap().join(APP_IDENTIFIER);

        assert!(!paths.is_portable());
        assert_eq!(paths.bootstrap_dir(), local.join("prod"));
        #[cfg(target_os = "windows")]
        assert_eq!(paths.log_dir(), local.join("logs"));
        #[cfg(target_os = "macos")]
        assert_eq!(
            paths.log_dir(),
            dirs::home_dir()
                .unwrap()
                .join("Library/Logs")
                .join(APP_IDENTIFIER)
        );
    }
}
