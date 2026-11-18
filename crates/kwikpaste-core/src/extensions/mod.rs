//! 本机官方扩展；下载与签名验证由更新器负责。
pub mod process;
#[cfg(test)]
mod tests;

use crate::{Core, CoreEvent, Result};
pub use kwikpaste_ext_protocol::{OCR_PROTOCOL, TRANSLATE_PROTOCOL, WETYPE_PROTOCOL};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::RwLock,
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InstalledExtension {
    pub version: String,
    pub protocol: u32,
    pub enabled: bool,
}

pub type InstalledExtensions = BTreeMap<String, InstalledExtension>;

pub(crate) struct ExtensionStore {
    dir: PathBuf,
    installed: RwLock<InstalledExtensions>,
    operations: tokio::sync::Mutex<()>,
}

#[cfg(target_os = "windows")]
const OCR_TIERS: [&str; 3] = ["ocr-tiny", "ocr-small", "ocr-medium"];

fn is_ocr(id: &str) -> bool {
    id == "ocr" || is_ocr_tier(id)
}

fn is_ocr_tier(id: &str) -> bool {
    #[cfg(target_os = "windows")]
    {
        OCR_TIERS.contains(&id)
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = id;
        false
    }
}

fn known(id: &str) -> anyhow::Result<u32> {
    match id {
        "ocr" => Ok(OCR_PROTOCOL),
        id if is_ocr_tier(id) => Ok(OCR_PROTOCOL),
        "wetype" => Ok(WETYPE_PROTOCOL),
        "translate" => Ok(TRANSLATE_PROTOCOL),
        _ => anyhow::bail!("unknown extension: {id}"),
    }
}

fn validate_version(version: &str) -> anyhow::Result<()> {
    semver::Version::parse(version)?;
    Ok(())
}

/// 只有已知 ID 和合法语义版本可用于拼接扩展目录路径。
pub fn executable_name(id: &str) -> anyhow::Result<String> {
    known(id)?;
    #[cfg(target_os = "windows")]
    let suffix = ".exe";
    #[cfg(target_os = "macos")]
    let suffix = "";
    Ok(format!("kwikpaste-ext-{id}{suffix}"))
}

impl ExtensionStore {
    pub(crate) fn load(dir: PathBuf) -> Self {
        let mut installed: InstalledExtensions = match fs::read(dir.join("installed.json")) {
            Ok(bytes) => match serde_json::from_slice(&bytes) {
                Ok(installed) => installed,
                Err(err) => {
                    log::warn!("failed to load installed extensions: {err}");
                    BTreeMap::new()
                }
            },
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
            Err(err) => {
                log::warn!("failed to read installed extensions: {err}");
                BTreeMap::new()
            }
        };
        installed.retain(|id, entry| {
            if known(id).is_err() {
                return false;
            }
            if let Err(err) = validate_version(&entry.version) {
                log::warn!("ignoring installed extension {id} with invalid version: {err}");
                return false;
            }
            true
        });
        Self {
            dir,
            installed: RwLock::new(installed),
            operations: Default::default(),
        }
    }

    fn list(&self) -> InstalledExtensions {
        self.installed
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    pub(crate) fn enabled(&self, id: &str) -> bool {
        let installed = self.installed.read().unwrap_or_else(|p| p.into_inner());
        installed
            .get(id)
            .is_some_and(|entry| entry.enabled && Some(entry.protocol) == known(id).ok())
    }

    pub(crate) fn resolve(&self, id: &str) -> Option<PathBuf> {
        let protocol = known(id).ok()?;
        let installed = self.installed.read().unwrap_or_else(|p| p.into_inner());
        let entry = installed.get(id)?;
        if !entry.enabled || entry.protocol != protocol {
            return None;
        }
        let exe = self
            .dir
            .join(id)
            .join(&entry.version)
            .join(executable_name(id).ok()?);
        exe.is_file().then_some(exe)
    }

    /// Tiers choose the engine, but never enable the OCR feature themselves.
    pub(crate) fn resolve_ocr(&self) -> Option<PathBuf> {
        let system = self.resolve("ocr")?;
        #[cfg(target_os = "windows")]
        for id in OCR_TIERS {
            if let Some(exe) = self.resolve(id) {
                return Some(exe);
            }
        }
        Some(system)
    }

    /// 先刷新同目录临时文件，再原子替换 installed.json。
    fn save(&self, installed: InstalledExtensions) -> anyhow::Result<()> {
        fs::create_dir_all(&self.dir)?;
        let mut tmp = tempfile::NamedTempFile::new_in(&self.dir)?;
        tmp.write_all(&serde_json::to_vec_pretty(&installed)?)?;
        tmp.as_file().sync_all()?;
        tmp.persist(self.dir.join("installed.json"))?;
        *self.installed.write().unwrap_or_else(|p| p.into_inner()) = installed;
        Ok(())
    }

    /// 先在目标卷暂存，确保 TEMP 与数据目录位于不同磁盘时也能完成替换。
    fn install(&self, id: &str, version: &str, protocol: u32, staged: &Path) -> anyhow::Result<()> {
        known(id)?;
        validate_version(version)?;
        if !staged.is_file() {
            anyhow::bail!("staged extension is not a file");
        }
        let dir = self.dir.join(id).join(version);
        fs::create_dir_all(&dir)?;
        let exe = dir.join(executable_name(id)?);
        // 同目录临时副本可避免同版本重装时留下不完整文件。
        let tmp = tempfile::NamedTempFile::new_in(&dir)?;
        fs::copy(staged, tmp.path())?;
        #[cfg(target_os = "macos")]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(tmp.path(), fs::Permissions::from_mode(0o755))?;
        }
        tmp.as_file().sync_all()?;
        tmp.persist(&exe)?;
        fs::create_dir_all(self.dir.join(id).join("state"))?;
        let mut installed = self.list();
        let enabled = installed.get(id).is_none_or(|entry| entry.enabled);
        let previous = installed.insert(
            id.to_owned(),
            InstalledExtension {
                version: version.to_owned(),
                protocol,
                enabled,
            },
        );
        // Publish the new choice in the same registry transaction, before cleanup.
        let replaced: Vec<String> = installed
            .keys()
            .filter(|other| is_ocr_tier(id) && other.as_str() != id && is_ocr_tier(other))
            .cloned()
            .collect();
        for other in &replaced {
            installed.remove(other);
        }
        self.save(installed)?;
        for other in replaced {
            let old = self.dir.join(other);
            if old.exists() {
                fs::remove_dir_all(old)?;
            }
        }
        fs::remove_file(staged)?;
        if let Some(previous) = previous.filter(|previous| previous.version != version) {
            let old = self.dir.join(id).join(previous.version);
            if old.exists() {
                fs::remove_dir_all(old)?;
            }
        }
        Ok(())
    }

    fn uninstall(&self, id: &str) -> anyhow::Result<()> {
        known(id)?;
        let mut installed = self.list();
        installed.remove(id);
        self.save(installed)?;
        let dir = self.dir.join(id);
        if dir.exists() {
            fs::remove_dir_all(dir)?;
        }
        Ok(())
    }

    fn set_enabled(&self, id: &str, enabled: bool) -> anyhow::Result<()> {
        known(id)?;
        let mut installed = self.list();
        let entry = installed
            .get_mut(id)
            .ok_or_else(|| anyhow::anyhow!("extension is not installed: {id}"))?;
        entry.enabled = enabled;
        self.save(installed)
    }
}

impl Core {
    pub fn installed_extensions(&self) -> InstalledExtensions {
        self.0.extensions.list()
    }

    /// 仅当扩展已安装、已启用、协议兼容且可执行文件存在时返回路径。
    pub fn resolve_extension(&self, id: &str) -> Option<PathBuf> {
        self.0.extensions.resolve(id)
    }

    pub fn ocr_enabled(&self) -> bool {
        self.resolve_extension("ocr").is_some()
    }

    /// 调用方必须先验证暂存二进制；替换文件前会停止 OCR。
    /// 成功后会移除 staged_path；识别文本和扩展自有状态会保留。
    pub async fn install_extension(
        &self,
        id: &str,
        version: &str,
        protocol: u32,
        staged_path: &Path,
    ) -> Result<()> {
        known(id)?;
        validate_version(version)?;
        let core = self.clone();
        let (id, version, staged_path) =
            (id.to_owned(), version.to_owned(), staged_path.to_owned());
        self.hop(async move {
            let _operation = core.0.extensions.operations.lock().await;
            let _probe = if is_ocr(&id) {
                Some(core.0.ocr.probe_lock.lock().await)
            } else {
                None
            };
            let pause = if is_ocr(&id) {
                Some(core.0.ocr.suspend().await)
            } else {
                None
            };
            if id == "wetype" {
                core.0.wetype.stop().await;
            }
            let result = core
                .0
                .extensions
                .install(&id, &version, protocol, &staged_path);
            if is_ocr(&id) {
                core.0.ocr.clear_support();
            }
            drop(pause);
            core.0.events.emit(CoreEvent::ExtensionsChanged);
            if is_ocr(&id) {
                core.0.ocr.nudge();
            }
            if id == "wetype" {
                core.0.wetype.apply(&core.0).await;
            }
            result.map_err(Into::into)
        })
        .await
    }

    /// 先停止子进程，再删除所有版本目录和扩展自有状态，但不删除数据库中的识别文本。
    pub async fn uninstall_extension(&self, id: &str) -> Result<()> {
        known(id)?;
        let core = self.clone();
        let id = id.to_owned();
        self.hop(async move {
            let _operation = core.0.extensions.operations.lock().await;
            let _probe = if is_ocr(&id) {
                Some(core.0.ocr.probe_lock.lock().await)
            } else {
                None
            };
            let pause = if is_ocr(&id) {
                Some(core.0.ocr.suspend().await)
            } else {
                None
            };
            if id == "wetype" {
                core.0.wetype.stop().await;
            }
            let result = core.0.extensions.uninstall(&id);
            if id == "wetype" && result.is_ok() {
                core.0.wetype.uninstalled();
                core.0.events.emit(CoreEvent::WeTypeChanged);
            }
            if is_ocr(&id) {
                core.0.ocr.clear_support();
            }
            drop(pause);
            core.0.events.emit(CoreEvent::ExtensionsChanged);
            if is_ocr(&id) {
                core.0.ocr.nudge();
            }
            if id == "wetype" {
                core.0.wetype.apply(&core.0).await;
            }
            result.map_err(Into::into)
        })
        .await
    }

    /// 立即生效；禁用会停止任务，启用会唤醒持久化的 OCR 队列。
    pub async fn set_extension_enabled(&self, id: &str, enabled: bool) -> Result<()> {
        known(id)?;
        let core = self.clone();
        let id = id.to_owned();
        self.hop(async move {
            let _operation = core.0.extensions.operations.lock().await;
            let _probe = if is_ocr(&id) {
                Some(core.0.ocr.probe_lock.lock().await)
            } else {
                None
            };
            let pause = if is_ocr(&id) {
                Some(core.0.ocr.suspend().await)
            } else {
                None
            };
            if id == "wetype" {
                core.0.wetype.stop().await;
            }
            let result = core.0.extensions.set_enabled(&id, enabled);
            if is_ocr(&id) {
                core.0.ocr.clear_support();
            }
            drop(pause);
            core.0.events.emit(CoreEvent::ExtensionsChanged);
            if is_ocr(&id) {
                core.0.ocr.nudge();
            }
            if id == "wetype" {
                core.0.wetype.apply(&core.0).await;
            }
            result.map_err(Into::into)
        })
        .await
    }

    /// 仅调试构建可用的本地无签名安装；不下载文件，也不会改动旁边的开发版二进制。
    #[cfg(debug_assertions)]
    pub async fn install_extension_from_dev(
        &self,
        id: &str,
        version: &str,
        protocol: u32,
    ) -> Result<()> {
        known(id)?;
        validate_version(version)?;
        let exe = std::env::current_exe().map_err(anyhow::Error::from)?;
        let sibling = exe
            .parent()
            .ok_or_else(|| anyhow::anyhow!("application executable has no parent"))?
            .join(executable_name(id)?);
        let staged = tempfile::NamedTempFile::new().map_err(anyhow::Error::from)?;
        fs::copy(sibling, staged.path()).map_err(anyhow::Error::from)?;
        self.install_extension(id, version, protocol, staged.path())
            .await
    }
}
