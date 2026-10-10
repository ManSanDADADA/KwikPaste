//! 官方扩展的目录、安装与后台更新；开发构建只读取应用旁边的二进制。

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use kwikpaste_core::extensions::InstalledExtension;
use kwikpaste_core::runtime::hop;
use kwikpaste_core::{Core, Result};
use kwikpaste_ext_protocol::OCR_PROTOCOL;
use serde::{Deserialize, Serialize};

use crate::{DownloadProgress, UpdaterUi, lock, other};

const KNOWN: &[(&str, u32)] = &[("ocr", OCR_PROTOCOL)];
const DAILY: Duration = Duration::from_secs(24 * 60 * 60);
#[cfg(not(debug_assertions))]
const ENDPOINT: &str = "https://paste.fastthree.com/api/v1/extensions";
#[cfg(any(not(debug_assertions), test))]
const MAX_EXTENSION_BYTES: u64 = 64 * 1024 * 1024;

/// 服务端选出的兼容版本；签名内容与应用更新器的 `.sig` 格式相同。
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogEntry {
    pub id: String,
    pub version: String,
    pub protocol: u32,
    pub size: u64,
    pub signature: String,
    pub url: url::Url,
    pub notes_zh: String,
    pub notes_en: String,
    pub published_at: Option<String>,
}

/// 每个已知扩展的卡片状态；安装信息每次从 Core 读取，不维护第二份持久化数据。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionStatus {
    pub id: String,
    pub catalog: Option<CatalogEntry>,
    pub installed: Option<InstalledExtension>,
    pub operation: OperationState,
    pub update_available: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase", tag = "state")]
pub enum OperationState {
    Idle,
    Downloading { progress: DownloadProgress },
    Installing,
    Error { message: String },
}

/// 克隆共享状态；公开方法可在 GPUI 执行器 await，工作始终跳到 Core runtime。
#[derive(Clone)]
pub struct ExtensionManager(Arc<Inner>);

struct Inner {
    core: Core,
    ui: Arc<dyn UpdaterUi>,
    source: Source,
    cards: Mutex<Vec<Card>>,
    operations: tokio::sync::Mutex<()>,
    last_daily: Mutex<Option<Instant>>,
}

enum Source {
    #[cfg(debug_assertions)]
    Dev,
    #[cfg(any(not(debug_assertions), test))]
    Remote {
        client: reqwest::Client,
        public_key: String,
        endpoint: String,
    },
}

struct Card {
    id: &'static str,
    catalog: Option<CatalogEntry>,
    operation: OperationState,
}

impl ExtensionManager {
    pub(crate) fn new(
        core: Core,
        ui: Arc<dyn UpdaterUi>,
        client: reqwest::Client,
        public_key: String,
    ) -> Self {
        #[cfg(debug_assertions)]
        let source = {
            let _ = (client, public_key);
            Source::Dev
        };
        #[cfg(not(debug_assertions))]
        let source = Source::Remote {
            client,
            public_key,
            endpoint: ENDPOINT.to_owned(),
        };
        Self::with_source(core, ui, source)
    }

    fn with_source(core: Core, ui: Arc<dyn UpdaterUi>, source: Source) -> Self {
        Self(Arc::new(Inner {
            core,
            ui,
            source,
            cards: Mutex::new(
                KNOWN
                    .iter()
                    .map(|(id, _)| Card {
                        id,
                        catalog: None,
                        operation: OperationState::Idle,
                    })
                    .collect(),
            ),
            operations: Default::default(),
            last_daily: Mutex::new(None),
        }))
    }

    pub fn status(&self) -> Vec<ExtensionStatus> {
        let installed = self.0.core.installed_extensions();
        lock(&self.0.cards)
            .iter()
            .map(|card| {
                let installed = installed.get(card.id).cloned();
                ExtensionStatus {
                    id: card.id.to_owned(),
                    update_available: needs_update(card.catalog.as_ref(), installed.as_ref()),
                    catalog: card.catalog.clone(),
                    installed,
                    operation: card.operation.clone(),
                }
            })
            .collect()
    }

    /// 显式刷新立即拉目录；自动检查开启时，将版本不同的已安装扩展排入后台更新。
    pub async fn refresh(&self) -> Result<Vec<ExtensionStatus>> {
        let manager = self.clone();
        hop(self.0.core.runtime(), async move {
            manager.refresh_inner().await
        })
        .await
    }

    /// 安装当前目录中兼容的版本；未刷新或服务器不可用时返回卡片错误。
    pub async fn install(&self, id: String) -> Result<()> {
        self.change(id, Change::Install).await
    }

    pub async fn uninstall(&self, id: String) -> Result<()> {
        self.change(id, Change::Uninstall).await
    }

    pub async fn set_enabled(&self, id: String, enabled: bool) -> Result<()> {
        self.change(id, Change::Enabled(enabled)).await
    }

    /// 沿用更新器的检查循环，但扩展的每日检查不受应用周/月更新频率影响。
    pub(crate) fn schedule_daily(&self) {
        if !self.0.core.settings().update.auto_check {
            return;
        }
        let mut last = lock(&self.0.last_daily);
        if last.is_some_and(|last| last.elapsed() < DAILY) {
            return;
        }
        *last = Some(Instant::now());
        let manager = self.clone();
        self.0.core.runtime().spawn(async move {
            if let Err(err) = manager.refresh_inner().await {
                log::warn!("extension catalog refresh failed: {err}");
            }
        });
    }

    /// 串行刷新避免目录与安装交错；网络失败只清空可安装项，不改变 Core 安装状态。
    async fn refresh_inner(&self) -> Result<Vec<ExtensionStatus>> {
        let _guard = self.0.operations.lock().await;
        let fetched = self.catalog().await;
        {
            let mut cards = lock(&self.0.cards);
            for card in cards.iter_mut() {
                match &fetched {
                    Ok(entries) => {
                        card.catalog = entries.iter().find(|entry| entry.id == card.id).cloned();
                        card.operation = OperationState::Idle;
                    }
                    Err(err) => {
                        card.catalog = None;
                        card.operation = OperationState::Error {
                            message: err.to_string(),
                        };
                    }
                }
            }
        }
        self.notify();
        fetched?;
        let status = self.status();
        if self.0.core.settings().update.auto_check {
            for card in status.iter().filter(|card| card.update_available) {
                let manager = self.clone();
                let id = card.id.clone();
                self.0.core.runtime().spawn(async move {
                    if let Err(err) = manager.change_inner(&id, Change::AutoUpdate).await {
                        log::warn!("extension {id} update failed: {err}");
                    }
                });
            }
        }
        Ok(status)
    }

    async fn catalog(&self) -> Result<Vec<CatalogEntry>> {
        match &self.0.source {
            #[cfg(debug_assertions)]
            Source::Dev => dev_catalog(&std::env::current_exe().map_err(anyhow::Error::from)?),
            #[cfg(any(not(debug_assertions), test))]
            Source::Remote {
                client, endpoint, ..
            } => {
                let mut endpoint = url::Url::parse(endpoint).map_err(anyhow::Error::from)?;
                endpoint
                    .query_pairs_mut()
                    .append_pair("target", &crate::target::extension_target());
                let response = client
                    .get(endpoint)
                    .timeout(crate::http::REQUEST_TIMEOUT)
                    .send()
                    .await
                    .map_err(anyhow::Error::from)?
                    .error_for_status()
                    .map_err(anyhow::Error::from)?;
                let body = crate::http::read_limited(response, 1024 * 1024).await?;
                Ok(select_catalog(&body)?)
            }
        }
    }

    async fn change(&self, id: String, change: Change) -> Result<()> {
        let manager = self.clone();
        hop(self.0.core.runtime(), async move {
            manager.change_inner(&id, change).await
        })
        .await
    }

    /// 自动更新在取得操作锁后再次检查，防止排队期间的卸载或设置变更被覆盖。
    async fn change_inner(&self, id: &str, change: Change) -> Result<()> {
        let _guard = self.0.operations.lock().await;
        if !KNOWN.iter().any(|(known, _)| *known == id) {
            return Err(other("unknown extension"));
        }
        if matches!(change, Change::AutoUpdate)
            && (!self.0.core.settings().update.auto_check
                || !self
                    .status()
                    .iter()
                    .any(|card| card.id == id && card.update_available))
        {
            return Ok(());
        }
        let result = match change {
            Change::Install | Change::AutoUpdate => self.install_inner(id).await,
            Change::Uninstall => {
                self.set_operation(id, OperationState::Installing);
                self.0.core.uninstall_extension(id).await
            }
            Change::Enabled(enabled) => self.0.core.set_extension_enabled(id, enabled).await,
        };
        self.set_operation(
            id,
            match &result {
                Ok(()) => OperationState::Idle,
                Err(err) => OperationState::Error {
                    message: err.to_string(),
                },
            },
        );
        result
    }

    async fn install_inner(&self, id: &str) -> Result<()> {
        let entry = lock(&self.0.cards)
            .iter()
            .find(|card| card.id == id)
            .and_then(|card| card.catalog.clone())
            .ok_or_else(|| other("extension is not available; refresh the catalog"))?;
        match &self.0.source {
            #[cfg(debug_assertions)]
            Source::Dev => {
                self.set_operation(id, OperationState::Installing);
                self.0
                    .core
                    .install_extension_from_dev(id, "0.0.0-dev", entry.protocol)
                    .await
            }
            #[cfg(any(not(debug_assertions), test))]
            Source::Remote {
                client, public_key, ..
            } => {
                self.set_operation(
                    id,
                    OperationState::Downloading {
                        progress: DownloadProgress {
                            downloaded: 0,
                            total: Some(entry.size),
                            progress: Some(0.0),
                        },
                    },
                );
                let dir = tempfile::Builder::new()
                    .prefix("KwikPaste-extension-updater-")
                    .tempdir()
                    .map_err(anyhow::Error::from)?;
                let staged = dir.path().join("extension.download");
                download_verified(client, &entry, public_key, &staged, &|progress| {
                    self.set_operation(id, OperationState::Downloading { progress });
                })
                .await?;
                self.set_operation(id, OperationState::Installing);
                self.0
                    .core
                    .install_extension(id, &entry.version, entry.protocol, &staged)
                    .await
            }
        }
    }

    fn set_operation(&self, id: &str, operation: OperationState) {
        if let Some(card) = lock(&self.0.cards).iter_mut().find(|card| card.id == id) {
            card.operation = operation;
        }
        self.notify();
    }

    fn notify(&self) {
        self.0.ui.extensions_changed(self.status());
    }
}

enum Change {
    Install,
    AutoUpdate,
    Uninstall,
    Enabled(bool),
}

/// 不比较版本大小：撤回后的旧版本也必须替换；未知 ID 或不同协议永远不更新。
fn needs_update(entry: Option<&CatalogEntry>, installed: Option<&InstalledExtension>) -> bool {
    match (entry, installed) {
        (Some(entry), Some(installed)) => {
            KNOWN.contains(&(entry.id.as_str(), entry.protocol))
                && entry.version != installed.version
        }
        _ => false,
    }
}

#[cfg(any(not(debug_assertions), test))]
/// 先筛选 ID 与协议，未知扩展的未来字段不影响当前应用。
fn select_catalog(body: &[u8]) -> anyhow::Result<Vec<CatalogEntry>> {
    #[derive(Deserialize)]
    struct Catalog {
        extensions: Vec<serde_json::Value>,
    }
    let catalog: Catalog = serde_json::from_slice(body)?;
    let mut entries = Vec::new();
    for value in catalog.extensions {
        let known = KNOWN.iter().any(|(id, protocol)| {
            value["id"].as_str() == Some(id)
                && value["protocol"].as_u64() == Some(u64::from(*protocol))
        });
        if !known {
            continue;
        }
        let entry: CatalogEntry = serde_json::from_value(value)?;
        semver::Version::parse(&entry.version)?;
        if entry.size == 0 || entry.size > MAX_EXTENSION_BYTES {
            anyhow::bail!("invalid extension size");
        }
        if entry.url.scheme() != "https"
            && !(cfg!(test)
                && entry.url.scheme() == "http"
                && entry.url.host_str() == Some("127.0.0.1"))
        {
            anyhow::bail!("extension download URL must use HTTPS");
        }
        if entries
            .iter()
            .any(|previous: &CatalogEntry| previous.id == entry.id)
        {
            anyhow::bail!("duplicate compatible extension entry");
        }
        entries.push(entry);
    }
    Ok(entries)
}

#[cfg(debug_assertions)]
/// 开发目录只包含实际存在的同级二进制，不生成任何服务器请求。
fn dev_catalog(exe: &std::path::Path) -> Result<Vec<CatalogEntry>> {
    let parent = exe
        .parent()
        .ok_or_else(|| other("application executable has no parent"))?;
    let mut entries = Vec::new();
    for (id, protocol) in KNOWN {
        let sibling = parent.join(kwikpaste_core::extensions::executable_name(id)?);
        if !sibling.is_file() {
            continue;
        }
        entries.push(CatalogEntry {
            id: (*id).to_owned(),
            version: "0.0.0-dev".to_owned(),
            protocol: *protocol,
            size: sibling.metadata().map_err(anyhow::Error::from)?.len(),
            signature: String::new(),
            url: url::Url::from_file_path(sibling)
                .map_err(|_| other("invalid development extension path"))?,
            notes_zh: String::new(),
            notes_en: String::new(),
            published_at: None,
        });
    }
    Ok(entries)
}

#[cfg(any(not(debug_assertions), test))]
/// 大小与签名均通过后才能进入 Core；调用方持有临时目录，所有返回路径都会清理。
async fn download_verified(
    client: &reqwest::Client,
    entry: &CatalogEntry,
    public_key: &str,
    staged: &std::path::Path,
    progress: &(dyn Fn(DownloadProgress) + Send + Sync),
) -> anyhow::Result<()> {
    let downloaded =
        crate::download::fetch_to_file(client, &entry.url, staged, MAX_EXTENSION_BYTES, progress)
            .await?;
    crate::verify::verify(&std::fs::read(staged)?, &entry.signature, public_key)?;
    if downloaded != entry.size {
        anyhow::bail!(
            "extension size mismatch: expected {}, got {downloaded}",
            entry.size
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests;
