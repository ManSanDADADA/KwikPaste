//! KwikPaste 2.x 自带的更新器：读 v2 清单、minisign 验签、流式下载、按安装形态交接，
//! 外加随检查更新附带的使用统计与应用内公告。
//!
//! 依赖 `kwikpaste-core` 与 `kwikpaste-os`，不依赖 Tauri 与 GPUI；1.x 的 `src-tauri` 不依赖它。所有网络与文件操作都在
//! core 的 tokio runtime 上执行，宿主（GPUI）在任何执行器里都可以直接 await 公开的 async 方法。
//!
//! 宿主要接的两个接口：
//! - [`UpdaterUi`]（UI 线）：可静默更新时后台下载、等空闲后安装，不弹窗或显示更新提示；否则每版本提示一次；
//!   宿主提供无可见窗口时的系统空闲时长，并处理公告与链接；
//! - [`HandoffHost`]（平台线）：安装交接时停输入、删托盘、释放单实例、退出进程。
//!
//! 与附录 E §5 相比按 v2 路线精简：没有分桶 / 灰度、闸门、canary 与演练渠道，没有自动降级、
//! 「回到经典版」和老更新器兼容，便携版也不做健康检查回滚。

mod announcement;
mod channel;
mod download;
pub mod extensions;
pub mod feedback;
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
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

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

/// 更新器需要宿主做的事：静默更新的空闲查询，以及非静默更新与公告的 UI。
pub trait UpdaterUi: Send + Sync + 'static {
    /// 所有应用窗口（含原生对话框）均不可见时返回系统空闲时长；有窗口或查询失败返回 None。
    fn idle_duration(&self) -> HostFuture<'_, Option<Duration>> {
        Box::pin(async { None })
    }

    /// 扩展状态变化；在后台线程回调，由宿主送进已有 UI 消息队列。
    fn extensions_changed(&self, _status: Vec<extensions::ExtensionStatus>) {}

    /// 发布面板提示状态（静默候选隐藏 update 字段），不自动打开更新窗。
    fn update_available(&self, status: UpdateStatus);

    /// 不可静默更新时，首次发现该版本弹更新窗；关掉后只留徽标，同版本不再弹。
    fn prompt_update(&self, _status: UpdateStatus) {}

    /// 显示一条公告并等用户响应。按 [`AnnouncementPrompt::buttons`] 的顺序摆按钮，点哪个按钮就返回
    /// [`ButtonRole::outcome`]；按 Esc、点标题栏关闭都返回 [`AnnouncementOutcome::Closed`]。
    fn show_announcement(&self, prompt: AnnouncementPrompt) -> HostFuture<'_, AnnouncementOutcome>;

    /// 用系统浏览器打开公告按钮的链接（已校验：https，域名是 fastthree.com / github.com / gitee.com）。
    fn open_url(&self, url: &str);
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
    exe: PathBuf,
    client: reqwest::Client,
    /// 清单地址；测试换成本机地址。
    endpoints: EndpointSource,
    /// 验签公钥；测试换成一次性密钥。
    public_key: String,
    pending: Mutex<Option<Pending>>,
    failed_silent_version: Mutex<Option<String>>,
    installing: tokio::sync::Mutex<()>,
    usage: Arc<UsageState>,
    announcements: Arc<AnnouncementState>,
    scheduler: Mutex<Option<JoinHandle<()>>>,
}

struct Pending {
    candidate: Candidate,
    package: Option<Package>,
    silent: bool,
}

type EndpointSource = Box<dyn Fn() -> anyhow::Result<Vec<url::Url>> + Send + Sync>;

/// 正式运行时从环境里取的部分；测试换成假的。
struct Parts {
    kind: InstallKind,
    exe: PathBuf,
    launcher: Box<dyn Launcher>,
    endpoints: EndpointSource,
    public_key: String,
}

impl Updater {
    /// 按当前 exe 的位置判定安装形态，建好更新器。不发任何请求，[`Updater::start`] 之后才开始调度。
    pub fn new(core: Core, ui: Arc<dyn UpdaterUi>, handoff: Arc<dyn HandoffHost>) -> Result<Self> {
        let exe = std::env::current_exe().context("failed to resolve current executable")?;
        Self::with_parts(
            core,
            ui,
            handoff,
            Parts {
                kind: InstallKind::for_exe(&exe),
                exe,
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
            exe,
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
            exe,
            client,
            endpoints,
            public_key,
            pending: Mutex::new(None),
            failed_silent_version: Mutex::new(None),
            installing: tokio::sync::Mutex::new(()),
            usage: Arc::default(),
            announcements: Arc::default(),
            scheduler: Mutex::new(None),
        })))
    }

    pub fn install_kind(&self) -> &InstallKind {
        &self.0.kind
    }

    /// 启动时消费交接结果；失败版本只在本进程内禁用静默安装，不显示任何完成通知。
    pub fn record_handoff(&self, record: HandoffRecord) {
        if !record.succeeded(&self.0.core.info().version) {
            log::warn!(
                "update to {} did not take; requiring manual confirmation",
                record.to
            );
            *lock(&self.0.failed_silent_version) = Some(record.to);
        }
    }

    /// 扩展页使用的管理器；与应用更新器共用运行时、HTTP 客户端及 UI 宿主。
    pub fn extensions(&self) -> &extensions::ExtensionManager {
        &self.0.extensions
    }

    /// 开始后台调度（重复调用无效）：清理上次更新留下的临时文件，8 秒后检查并发布状态（顺带拉公告），
    /// 之后每小时维护扩展、统计与检查；已下载的静默更新每分钟等一次空闲（见 [`scheduler`]）。
    pub fn start(&self) {
        let mut scheduler = lock(&self.0.scheduler);
        if scheduler.is_some() {
            return;
        }

        let updater = self.clone();
        *scheduler = Some(self.0.core.runtime().spawn(async move {
            updater.clean_leftovers();
            tokio::time::sleep(scheduler::INITIAL_DELAY).await;
            let mut last_success = None;
            let mut checks = tokio::time::interval(scheduler::TICK);
            let mut idle = tokio::time::interval(scheduler::IDLE_POLL);
            checks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            idle.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    _ = checks.tick() => {
                        updater.0.extensions.schedule_daily();
                        if scheduler::check_due(last_success, Utc::now()) {
                            if updater.run_auto_check().await {
                                last_success = Some(Utc::now());
                            }
                        } else {
                            usage::schedule(&updater.0.core, &updater.0.usage);
                        }
                    }
                    _ = idle.tick(), if updater.silent_ready_version().is_some() => {
                        // 交接会 stop() 调度器，另起任务避免把交接自身取消在关库中途。
                        let installer = updater.clone();
                        if let Err(err) = hop(updater.0.core.runtime(), async move {
                            if let Err(err) = installer.install_when_idle().await {
                                log::warn!("silent update install failed: {err}");
                            }
                            Ok(())
                        }).await {
                            log::warn!("silent update task failed: {err}");
                        }
                    }
                }
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
    pub async fn check(&self) -> Result<UpdateStatus> {
        let updater = self.clone();
        hop(self.0.core.runtime(), async move {
            updater.check_inner(false).await
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
            updater.install_inner(&version, false).await
        })
        .await
    }

    async fn check_inner(&self, automatic: bool) -> Result<UpdateStatus> {
        let inner = &self.0;
        usage::schedule(&inner.core, &inner.usage);
        announcement::schedule(&inner.core, &inner.announcements, &inner.ui);

        if inner.kind.is_managed() {
            let mirrors = (inner.endpoints)()?;
            let criteria = Criteria {
                current: inner.core.info().version.clone(),
                os: os::current(),
                platform_keys: inner.kind.platform_keys(),
            };
            let found = channel::check(&inner.client, &mirrors, &criteria).await?;
            inner.set_candidate(found);
        }

        if automatic {
            let silent = inner.kind.can_update_silently(&inner.exe);
            let failed = lock(&inner.failed_silent_version);
            if let Some(pending) = lock(&inner.pending).as_mut() {
                pending.silent = silent
                    && failed.as_deref() != Some(pending.candidate.version.to_string().as_str());
            }
        }

        let checked_at =
            serde_json::json!({ "update": { "lastCheckedAt": Utc::now().to_rfc3339() } });
        if let Err(err) = inner.core.update_settings(checked_at).await {
            log::warn!("persist update check timestamp failed: {err}");
        }
        let status = inner.status();
        inner.publish_status();
        Ok(status)
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

    async fn install_inner(&self, version: &str, silent: bool) -> Result<()> {
        let inner = &self.0;
        let _installing = inner.installing.lock().await;
        if silent {
            if self.silent_ready_version().as_deref() != Some(version) {
                return Ok(());
            }
            if !inner.kind.can_update_silently(&inner.exe) {
                if let Some(pending) = lock(&inner.pending).as_mut() {
                    pending.silent = false;
                }
                inner.publish_status();
                self.prompt_once(inner.status()).await;
                return Ok(());
            }
            if inner
                .ui
                .idle_duration()
                .await
                .is_none_or(|idle| idle < scheduler::INSTALL_IDLE)
            {
                return Ok(());
            }
        }
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

        let handoff = Handoff {
            core: &inner.core,
            host: inner.handoff.as_ref(),
            launcher: inner.launcher.as_ref(),
            exe: inner.exe.clone(),
            args: restart_args(std::env::args_os().skip(1).collect(), silent),
            silent,
            from: inner.core.info().version.to_string(),
            to: version.to_owned(),
            stop_scheduler: &|| self.stop(),
        };
        log::info!("installing update {version} ({:?})", inner.kind);

        let installed = match &inner.kind {
            #[cfg(any(target_os = "windows", test))]
            InstallKind::Nsis => handoff.nsis(&package).await,
            #[cfg(any(target_os = "windows", test))]
            InstallKind::Portable => handoff.portable(&package).await,
            #[cfg(any(target_os = "macos", test))]
            InstallKind::MacApp { bundle } => handoff.mac_app(bundle, &package).await,
            other_kind => Err(anyhow!("{other_kind:?} cannot install updates here")),
        };
        if installed.is_err() {
            *lock(&inner.failed_silent_version) = Some(version.to_owned());
            if let Some(pending) = lock(&inner.pending).as_mut()
                && pending.candidate.version.to_string() == version
            {
                pending.silent = false;
            }
            inner.publish_status();
        }
        installed.map_err(AppError::Other)
    }

    /// 自动检查成功返回 true：可静默更新就后台下载验签，不记录提示版本或显示徽标；否则沿用每版本提示一次。
    async fn run_auto_check(&self) -> bool {
        let status = match self.check_inner(true).await {
            Ok(status) => status,
            Err(err) => {
                log::warn!("automatic update check failed: {err}");
                return false;
            }
        };
        let silent = lock(&self.0.pending)
            .as_ref()
            .is_some_and(|pending| pending.silent);
        if let Some(update) = &status.update
            && silent
        {
            if !update.downloaded
                && let Err(err) = self.download_inner(&update.version, &|_| {}).await
            {
                log::warn!("silent update download failed: {err}");
            }
            return true;
        }
        self.prompt_once(status).await;
        true
    }

    /// 非静默路径沿用持久化的每版本提示标记。
    async fn prompt_once(&self, status: UpdateStatus) {
        let inner = &self.0;
        if let Some(update) = &status.update
            && inner.core.settings().update.prompted_version.as_deref()
                != Some(update.version.as_str())
        {
            let prompted = serde_json::json!({ "update": { "promptedVersion": update.version } });
            if let Err(err) = inner.core.update_settings(prompted).await {
                log::warn!("persist prompted update version failed: {err}");
            }
            inner.ui.prompt_update(status);
        }
    }

    fn silent_ready_version(&self) -> Option<String> {
        lock(&self.0.pending)
            .as_ref()
            .filter(|pending| pending.silent && pending.package.is_some())
            .map(|pending| pending.candidate.version.to_string())
    }

    /// 下载后轮询；只有宿主确认所有窗口不可见且连续十分钟没有输入才交接。
    async fn install_when_idle(&self) -> Result<()> {
        if let Some(version) = self.silent_ready_version() {
            self.install_inner(&version, true).await?;
        }
        Ok(())
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
    /// 手动查询仍能取得候选；仅发给面板的快照隐藏静默更新。
    fn publish_status(&self) {
        let mut status = self.status();
        if lock(&self.pending)
            .as_ref()
            .is_some_and(|pending| pending.silent)
        {
            status.update = None;
        }
        self.ui.update_available(status);
    }

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

    /// 换上新找到的版本；还是同一个版本、同一个签名时保留已下载的安装包。不比下载地址：主镜像和
    /// GitHub 兜底给的是同一个文件、不同的地址。
    fn set_candidate(&self, found: Option<Candidate>) {
        let mut pending = lock(&self.pending);
        let keep = |current: &Pending, next: &Candidate| {
            current.candidate.version == next.version
                && current.candidate.platform.signature == next.platform.signature
        };
        *pending = match (pending.take(), found) {
            (Some(current), Some(next)) if keep(&current, &next) => Some(Pending {
                candidate: next,
                package: current.package,
                silent: current.silent,
            }),
            (_, Some(next)) => Some(Pending {
                candidate: next,
                package: None,
                silent: false,
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

/// 静默重启保留参数并补上托盘启动标记；手动更新保持其余参数不变。
///
/// 去掉崩溃重启的参数：新版本带着它会当成崩溃后拉起的进程，进入降级模式。冷启动时带的备份文件也去掉，
/// 否则新版本会再导入一次。Windows 还去掉提权重启标记：安装器拉起的新进程不带管理员令牌，带着标记它就
/// 不再按设置提权。
fn restart_args(args: Vec<OsString>, silent: bool) -> Vec<OsString> {
    use kwikpaste_os::single_instance::{CRASH_DEGRADED_ARG, RELAUNCHED_AFTER_CRASH_ARG};

    let mut kept = Vec::new();
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        if arg == RELAUNCHED_AFTER_CRASH_ARG {
            args.next();
            continue;
        }
        #[cfg(target_os = "windows")]
        if arg == kwikpaste_os::win::admin::ADMIN_RESTARTED_ARG {
            continue;
        }
        if arg != CRASH_DEGRADED_ARG && !kwikpaste_core::backup::is_backup_path(Path::new(&arg)) {
            kept.push(arg);
        }
    }
    if silent
        && !kept
            .iter()
            .any(|arg| arg == kwikpaste_os::autostart::AUTO_LAUNCH_ARG)
    {
        kept.push(kwikpaste_os::autostart::AUTO_LAUNCH_ARG.into());
    }
    kept
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
