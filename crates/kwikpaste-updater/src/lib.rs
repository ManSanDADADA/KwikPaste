//! KwikPaste 2.x 自带的更新器：读 v2 清单、minisign 验签、流式下载、按安装形态交接，
//! 外加随检查更新附带的使用统计与应用内公告。
//!
//! 只依赖 `kwikpaste-core`，不依赖 Tauri 与 GPUI；1.x 的 `src-tauri` 不依赖它。所有网络与文件操作都在
//! core 的 tokio runtime 上执行，宿主（GPUI）在任何执行器里都可以直接 await 公开的 async 方法。
//!
//! 宿主要接的两个接口：
//! - [`UpdaterUi`]（UI 线）：自动检查发现更新时显示更新窗，显示公告对话框并返回用户的选择，打开链接；
//! - [`HandoffHost`]（平台线）：安装交接时停输入、删托盘、释放单实例、退出进程。
//!
//! 与附录 E §5 相比按 v2 路线精简：没有分桶 / 灰度、闸门、canary 与演练渠道，没有自动降级、
//! 「回到经典版」和老更新器兼容，便携版也不做健康检查回滚。

mod announcement;
mod channel;
mod download;
pub mod extensions;
mod handoff;
mod http;
mod manifest;
mod os;
mod overrides;
mod scheduler;
mod target;
mod usage;
mod verify;

#[cfg(test)]
mod testing;

use std::ffi::OsString;
use std::sync::{Arc, Mutex, MutexGuard};

use anyhow::{Context, anyhow};
use chrono::Utc;
use kwikpaste_core::runtime::hop;
use kwikpaste_core::{AppError, Core, Result};
use serde::Serialize;
use tokio::task::JoinHandle;

pub use announcement::{AnnouncementButton, AnnouncementOutcome, AnnouncementPrompt, ButtonRole};
pub use download::{DownloadProgress, MAX_PACKAGE_BYTES};
pub use handoff::{HandoffHost, HandoffRecord, HostFuture, take_handoff};
#[cfg(feature = "e2e-overrides")]
pub use overrides::SENTINEL as E2E_OVERRIDES_SENTINEL;
pub use target::InstallKind;

use announcement::AnnouncementState;
use channel::{Candidate, Criteria};
use download::Package;
use handoff::{Handoff, Launcher, SystemLauncher};
use usage::UsageState;

/// 发行说明页。
const RELEASE_NOTES_URL: &str = "https://github.com/ManSanDADADA/KwikPaste/releases/tag/v";

/// 更新器需要 UI（更新窗、公告对话框）做的事。
pub trait UpdaterUi: Send + Sync + 'static {
    /// 扩展状态变化；在后台线程回调，由宿主送进已有 UI 消息队列。
    fn extensions_changed(&self, _status: Vec<extensions::ExtensionStatus>) {}

    /// 自动检查发现了可以装的新版本：显示更新窗（与 1.x 一样，自动检查找到更新就弹出更新窗）。
    fn update_available(&self, status: UpdateStatus);

    /// 显示一条公告并等用户响应。按 [`AnnouncementPrompt::buttons`] 的顺序摆按钮，点哪个按钮就返回
    /// [`ButtonRole::outcome`]；按 Esc、点标题栏关闭都返回 [`AnnouncementOutcome::Closed`]。
    fn show_announcement(&self, prompt: AnnouncementPrompt) -> HostFuture<'_, AnnouncementOutcome>;

    /// 用系统浏览器打开公告按钮的链接（已校验：https，域名是 fastthree.com / github.com / gitee.com）。
    fn open_url(&self, url: &str);
}

/// 检查是手动发起的还是自动的：自动检查还没到频率设定的时间就直接返回当前状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckMode {
    Manual,
    Auto,
}

/// 更新窗与偏好页展示的状态。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateStatus {
    pub current_version: String,
    /// 这种运行方式能不能自更新（开发构建、手动拷出来的 exe 不能）。
    pub supported: bool,
    pub install_kind: InstallKind,
    pub update: Option<UpdateMetadata>,
}

/// 找到的新版本。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateMetadata {
    pub current_version: String,
    pub version: String,
    pub date: Option<String>,
    pub body: Option<String>,
    /// 清单里用到的平台键。
    pub target: String,
    pub download_url: String,
    pub downloaded: bool,
    pub release_notes_url: String,
}

/// 更新器句柄，克隆很便宜。
#[derive(Clone)]
pub struct Updater(Arc<Inner>);

struct Inner {
    extensions: extensions::ExtensionManager,
    core: Core,
    ui: Arc<dyn UpdaterUi>,
    handoff: Arc<dyn HandoffHost>,
    launcher: Box<dyn Launcher>,
    kind: InstallKind,
    client: reqwest::Client,
    /// 清单地址；测试换成本机地址。
    endpoints: EndpointSource,
    /// 验签公钥；测试换成一次性密钥。
    public_key: String,
    pending: Mutex<Option<Pending>>,
    usage: Arc<UsageState>,
    announcements: Arc<AnnouncementState>,
    scheduler: Mutex<Option<JoinHandle<()>>>,
}

struct Pending {
    candidate: Candidate,
    package: Option<Package>,
}

type EndpointSource = Box<dyn Fn() -> anyhow::Result<Vec<url::Url>> + Send + Sync>;

/// 正式运行时从环境里取的部分；测试换成假的。
struct Parts {
    kind: InstallKind,
    launcher: Box<dyn Launcher>,
    endpoints: EndpointSource,
    public_key: String,
}

impl Updater {
    /// 按当前 exe 的位置判定安装形态，建好更新器。不发任何请求，[`Updater::start`] 之后才开始调度。
    pub fn new(core: Core, ui: Arc<dyn UpdaterUi>, handoff: Arc<dyn HandoffHost>) -> Result<Self> {
        Self::with_parts(
            core,
            ui,
            handoff,
            Parts {
                kind: InstallKind::detect(),
                launcher: Box::new(SystemLauncher),
                endpoints: Box::new(channel::endpoints),
                public_key: verify::public_key(),
            },
        )
    }

    fn with_parts(
        core: Core,
        ui: Arc<dyn UpdaterUi>,
        handoff: Arc<dyn HandoffHost>,
        parts: Parts,
    ) -> Result<Self> {
        let Parts {
            kind,
            launcher,
            endpoints,
            public_key,
        } = parts;
        let client = http::updater_client(&core.info().version)?;
        let extensions = extensions::ExtensionManager::new(
            core.clone(),
            ui.clone(),
            client.clone(),
            public_key.clone(),
        );
        log::info!("updater ready: {kind:?}");
        Ok(Self(Arc::new(Inner {
            extensions,
            core,
            ui,
            handoff,
            launcher,
            kind,
            client,
            endpoints,
            public_key,
            pending: Mutex::new(None),
            usage: Arc::default(),
            announcements: Arc::default(),
            scheduler: Mutex::new(None),
        })))
    }

    pub fn install_kind(&self) -> &InstallKind {
        &self.0.kind
    }

    /// 扩展页使用的管理器；与应用更新器共用运行时、HTTP 客户端及 UI 宿主。
    pub fn extensions(&self) -> &extensions::ExtensionManager {
        &self.0.extensions
    }

    /// 开始后台调度（重复调用无效）：清理上次更新留下的临时文件，8 秒后拉公告，
    /// 之后按设置的频率自动检查，发现更新时调用 [`UpdaterUi::update_available`]；
    /// 每轮（至多一小时）看一次当天的使用统计是否已送达。
    pub fn start(&self) {
        let mut scheduler = lock(&self.0.scheduler);
        if scheduler.is_some() {
            return;
        }

        let updater = self.clone();
        *scheduler = Some(self.0.core.runtime().spawn(async move {
            updater.clean_leftovers();
            tokio::time::sleep(scheduler::INITIAL_DELAY).await;
            announcement::schedule(
                &updater.0.core,
                &updater.0.announcements,
                &updater.0.ui,
                announcement::Trigger::Launch,
            );

            loop {
                updater.0.extensions.schedule_daily();
                usage::schedule(&updater.0.core, &updater.0.usage, usage::Trigger::Daily);
                let settings = updater.0.core.settings().update;
                let delay = scheduler::next_auto_check_delay(&settings, Utc::now());
                if !delay.is_zero() {
                    tokio::time::sleep(delay).await;
                    continue;
                }

                updater.run_auto_check().await;
                tokio::time::sleep(scheduler::RETRY_AFTER_CHECK).await;
            }
        }));
    }

    /// 停掉后台调度（退出前调用）。
    pub fn stop(&self) {
        if let Some(task) = lock(&self.0.scheduler).take() {
            task.abort();
        }
    }

    pub fn status(&self) -> UpdateStatus {
        self.0.status()
    }

    /// 检查更新：读 v2 清单（只有一个渠道），顺带上报统计、拉公告。
    pub async fn check(&self, mode: CheckMode) -> Result<UpdateStatus> {
        let updater = self.clone();
        hop(self.0.core.runtime(), async move {
            updater.check_inner(mode).await
        })
        .await
    }

    /// 下载并验签当前找到的版本；`on_progress` 在后台线程上回调。
    pub async fn download<F>(&self, version: String, on_progress: F) -> Result<UpdateMetadata>
    where
        F: Fn(DownloadProgress) + Send + Sync + 'static,
    {
        let updater = self.clone();
        hop(self.0.core.runtime(), async move {
            updater.download_inner(&version, &on_progress).await
        })
        .await
    }

    /// 安装已下载的版本：交接给安装包 / 换 exe / 换 `.app`，成功时经 [`HandoffHost::exit`] 退出进程。
    /// 返回错误时（如安装包损坏）当前进程照常运行；NSIS 安装包没能启动时会先重启当前版本再退出。
    pub async fn install(&self, version: String) -> Result<()> {
        let updater = self.clone();
        hop(self.0.core.runtime(), async move {
            updater.install_inner(&version).await
        })
        .await
    }

    /// 跳过这个版本：记进 `update.skippedVersion`，之后的检查不再提供它。
    pub async fn skip(&self, version: String) -> Result<UpdateStatus> {
        self.0
            .core
            .update_settings(serde_json::json!({ "update": { "skippedVersion": version } }))
            .await?;
        *lock(&self.0.pending) = None;
        Ok(self.status())
    }

    async fn check_inner(&self, mode: CheckMode) -> Result<UpdateStatus> {
        let inner = &self.0;
        let settings = inner.core.settings().update;
        if mode == CheckMode::Auto
            && !scheduler::next_auto_check_delay(&settings, Utc::now()).is_zero()
        {
            return Ok(inner.status());
        }

        usage::schedule(&inner.core, &inner.usage, usage::Trigger::Check);
        announcement::schedule(
            &inner.core,
            &inner.announcements,
            &inner.ui,
            announcement::Trigger::Check,
        );

        if inner.kind.is_managed() {
            let mirrors = (inner.endpoints)()?;
            let criteria = Criteria {
                current: inner.core.info().version.clone(),
                skipped: settings.skipped_version.clone(),
                os: os::current(),
                platform_keys: inner.kind.platform_keys(),
            };
            let found = channel::check(&inner.client, &mirrors, &criteria).await?;
            inner.set_candidate(found);
        }

        let checked_at =
            serde_json::json!({ "update": { "lastCheckedAt": Utc::now().to_rfc3339() } });
        if let Err(err) = inner.core.update_settings(checked_at).await {
            log::warn!("persist update check timestamp failed: {err}");
        }
        Ok(inner.status())
    }

    async fn download_inner(
        &self,
        version: &str,
        on_progress: &(dyn Fn(DownloadProgress) + Send + Sync),
    ) -> Result<UpdateMetadata> {
        let inner = &self.0;
        let candidate = inner.candidate(version)?;
        let package = download::download(
            &inner.client,
            &candidate.platform.url,
            &candidate.platform.signature,
            &inner.public_key,
            &candidate.version.to_string(),
            &inner.kind,
            on_progress,
        )
        .await?;
        log::info!("update {version} downloaded to {:?}", package.dir);

        let mut pending = lock(&inner.pending);
        let current = pending
            .as_mut()
            .filter(|pending| pending.candidate.version.to_string() == version)
            .ok_or_else(|| other("the selected update is no longer current"))?;
        current.package = Some(package);
        Ok(inner.metadata(current))
    }

    async fn install_inner(&self, version: &str) -> Result<()> {
        let inner = &self.0;
        let package = {
            let pending = lock(&inner.pending);
            let current = pending
                .as_ref()
                .filter(|pending| pending.candidate.version.to_string() == version)
                .ok_or_else(|| other("no update is ready"))?;
            current
                .package
                .clone()
                .ok_or_else(|| other("update is not downloaded"))?
        };

        let exe = std::env::current_exe().context("failed to resolve current executable")?;
        let handoff = Handoff {
            core: &inner.core,
            host: inner.handoff.as_ref(),
            launcher: inner.launcher.as_ref(),
            exe,
            args: std::env::args_os().skip(1).collect::<Vec<OsString>>(),
            from: inner.core.info().version.to_string(),
            to: version.to_owned(),
        };
        log::info!("installing update {version} ({:?})", inner.kind);
        self.stop();

        let installed = match &inner.kind {
            #[cfg(any(target_os = "windows", test))]
            InstallKind::Nsis => handoff.nsis(&package.file).await,
            #[cfg(any(target_os = "windows", test))]
            InstallKind::Portable => handoff.portable(&package.file).await,
            #[cfg(any(target_os = "macos", test))]
            InstallKind::MacApp { bundle } => {
                handoff.mac_app(bundle, &package.file, &package.dir).await
            }
            other_kind => Err(anyhow!("{other_kind:?} cannot install updates here")),
        };
        installed.map_err(AppError::Other)
    }

    async fn run_auto_check(&self) {
        match self.check_inner(CheckMode::Auto).await {
            Ok(status) if status.update.is_some() => self.0.ui.update_available(status),
            Ok(_) => {}
            Err(err) => log::warn!("automatic update check failed: {err}"),
        }
    }

    /// 上次更新留下的临时目录（一天以前的）和便携版的 `.old` / `.new`。
    fn clean_leftovers(&self) {
        let removed =
            download::remove_stale_dirs(&std::env::temp_dir(), std::time::SystemTime::now());
        if removed > 0 {
            log::info!("removed {removed} stale update director(ies)");
        }
        #[cfg(target_os = "windows")]
        if self.0.kind == InstallKind::Portable
            && let Ok(exe) = std::env::current_exe()
        {
            handoff::portable::cleanup_leftovers(&exe);
        }
    }
}

impl Inner {
    fn status(&self) -> UpdateStatus {
        let pending = lock(&self.pending);
        UpdateStatus {
            current_version: self.core.info().version.to_string(),
            supported: self.kind.is_managed(),
            install_kind: self.kind.clone(),
            update: pending.as_ref().map(|pending| self.metadata(pending)),
        }
    }

    fn metadata(&self, pending: &Pending) -> UpdateMetadata {
        let candidate = &pending.candidate;
        UpdateMetadata {
            current_version: self.core.info().version.to_string(),
            version: candidate.version.to_string(),
            date: candidate.pub_date.clone(),
            body: candidate.notes.clone(),
            target: candidate.target.clone(),
            download_url: candidate.platform.url.to_string(),
            downloaded: pending.package.is_some(),
            release_notes_url: format!("{RELEASE_NOTES_URL}{}", candidate.version),
        }
    }

    /// 换上新找到的版本；还是同一个版本、同一个地址时保留已下载的安装包。
    fn set_candidate(&self, found: Option<Candidate>) {
        let mut pending = lock(&self.pending);
        let keep = |current: &Pending, next: &Candidate| {
            current.candidate.version == next.version && current.candidate.platform == next.platform
        };
        *pending = match (pending.take(), found) {
            (Some(current), Some(next)) if keep(&current, &next) => Some(Pending {
                candidate: next,
                package: current.package,
            }),
            (_, Some(next)) => Some(Pending {
                candidate: next,
                package: None,
            }),
            (_, None) => None,
        };
    }

    fn candidate(&self, version: &str) -> Result<Candidate> {
        lock(&self.pending)
            .as_ref()
            .filter(|pending| pending.candidate.version.to_string() == version)
            .map(|pending| pending.candidate.clone())
            .ok_or_else(|| other("the selected update is no longer current"))
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn other(message: &str) -> AppError {
    AppError::Other(anyhow!(message.to_owned()))
}

#[cfg(test)]
mod tests;
