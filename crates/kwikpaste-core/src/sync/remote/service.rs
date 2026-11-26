//! 远程同步运行时：连服务器、读事件流、串行上传，以及偏好页看到的状态（REMOTE.md §4.7、§7）。
//!
//! 已加入同步组且开着远程同步时跑一个运行时：主循环先 `GET /v1/info`，再开事件流逐帧处理，
//! 断线按 5 秒到 1 分钟退避重连；上传另有一个任务串行执行，排队的帧合进下一个请求。
//! 设备令牌失效、服务器令牌不对、协议不兼容时停下，等设置变化或下次启动再试。

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, Weak};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::{mpsc, watch, Notify};
use tokio::task::JoinHandle;

use super::client::{
    encode_frame, ApiError, Client, FrameReader, PairingAnswer, VersionRange, MAX_REQUEST_BYTES,
    PROTOCOL, STREAM_READ_TIMEOUT,
};
use super::crypto::{parse_hlc, random_key, unpack, DeviceInfo, GroupKeys, HlcClock};
use super::entries::{self, Incoming, MetaEntry, Outgoing};
use super::pairing::{self, JoinGrant};
use super::settings::{self as remote_settings, SettingsEntry};
use super::state::{self, RemoteFile, ServerConfig};
use super::{
    RemoteDeviceView, RemoteImages, RemotePairingCode, RemotePolicy, RemoteServer,
    RemoteServerView, RemoteStatus, RemoteSyncState,
};
use crate::db::models::{ClipboardItem, Platform};
use crate::error::{AppError, Result};
use crate::events::CoreEvent;
use crate::i18n::commands::{label, Key};
use crate::root::{Core, CoreInner};
use crate::settings::RemoteSync;
use crate::sync::engine::{self, ReceiveOptions};
use crate::sync::protocol::{from_hex, to_hex};
use crate::sync::{current_platform, effective_device_name, identity};

const BACKOFF_MIN: Duration = Duration::from_secs(5);
const BACKOFF_MAX: Duration = Duration::from_secs(60);
const UPLOAD_QUEUE: usize = 256;
/// 历史上传每页的记录数。
const HISTORY_PAGE: usize = 50;
/// 远程同步的配对码位数（服务器生成，局域网是 6 位）。
const REMOTE_CODE_DIGITS: usize = 8;

/// 由 `Core` 持有。`remote.json` 随 core 读入；网络部分要宿主调用 [`Core::start_remote_sync`] 后才按设置启动。
pub(crate) struct RemoteService {
    dir: PathBuf,
    core: OnceLock<Weak<CoreInner>>,
    started: AtomicBool,
    inner: Mutex<Inner>,
    lifecycle: tokio::sync::Mutex<Option<Runtime>>,
}

struct Inner {
    file: Option<RemoteFile>,
    view: View,
    clock: HlcClock,
    /// 上次按设置启停时的远程同步设置，用来发现「同步全部历史」「同步设置」刚打开。
    applied: Option<RemoteSync>,
    run: Option<Arc<Run>>,
}

/// 偏好页要看的状态（不落盘）。服务器策略在运行时上（[`Run::policy`]）。
struct View {
    status: RemoteStatus,
    devices: Vec<RemoteDeviceView>,
    pairing: Option<RemotePairingCode>,
    image_quota_until: Option<DateTime<Utc>>,
    last_sync_at: Option<DateTime<Utc>>,
    history_upload: Option<(u64, u64)>,
    /// 本机被移出同步组：凭据已清掉，界面仍显示原来的服务器和「已不在同步组里」，直到退出或重新加入。
    revoked: Option<RemoteServerView>,
}

impl Default for View {
    fn default() -> Self {
        Self {
            status: RemoteStatus::Connecting,
            devices: Vec::new(),
            pairing: None,
            image_quota_until: None,
            last_sync_at: None,
            history_upload: None,
            revoked: None,
        }
    }
}

/// 一次运行（已加入同步组、开着远程同步）共用的东西。
pub(super) struct Run {
    pub client: Client,
    pub keys: GroupKeys,
    pub device_id: String,
    pub uploads: mpsc::Sender<Upload>,
    /// 设置变了：主循环检查设备名与设置差异。
    nudge: Notify,
    /// 事件流连上了：正在退避的上传马上重试。
    connected: Notify,
    /// 服务器策略，第一次连上后才有；上传任务等它。
    policy: watch::Sender<Option<RemotePolicy>>,
    /// 这次运行还没从头收过一遍设置：「同步设置」关着时、或旧版本不认识的设置项当时跳过了。
    settings_pass: AtomicBool,
}

impl Run {
    pub fn nudge(&self) {
        self.nudge.notify_one();
    }

    /// 已经连上过、服务器收元数据。
    pub fn accepts_meta(&self) -> bool {
        self.policy
            .borrow()
            .as_ref()
            .is_some_and(|policy| policy.history && policy.meta)
    }
}

/// 交给上传任务的活。
pub(super) enum Upload {
    Capture {
        item: Box<ClipboardItem>,
        hlc: String,
        at: Instant,
        skip: Vec<String>,
    },
    Frame(Outgoing),
    History,
}

struct Runtime {
    run: Arc<Run>,
    task: JoinHandle<()>,
}

/// 被丢弃时终止任务：主循环被终止时上传任务跟着停。
struct AbortOnDrop(JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// 一次连接为什么结束。
enum End {
    Revoked,
    ServerToken,
    Incompatible(String),
    Retry {
        status: RemoteStatus,
        wait: Option<Duration>,
    },
}

impl RemoteService {
    pub(crate) fn new(dir: PathBuf) -> Self {
        let file = state::load(&dir);
        Self {
            dir,
            core: OnceLock::new(),
            started: AtomicBool::new(false),
            inner: Mutex::new(Inner {
                file,
                view: View::default(),
                clock: HlcClock::default(),
                applied: None,
                run: None,
            }),
            lifecycle: tokio::sync::Mutex::new(None),
        }
    }

    /// `Core` 建好后登记自己。
    pub(crate) fn bind(&self, core: Weak<CoreInner>) {
        let _ = self.core.set(core);
    }

    pub(super) fn bound_core(&self) -> Option<Arc<CoreInner>> {
        if !self.started.load(Ordering::Acquire) {
            return None;
        }
        self.core.get()?.upgrade()
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn emit(&self, core: &CoreInner) {
        core.events.emit(CoreEvent::RemoteSyncChanged);
    }

    fn update_view(&self, core: &CoreInner, update: impl FnOnce(&mut View)) {
        update(&mut self.lock().view);
        self.emit(core);
    }

    fn set_status(&self, core: &CoreInner, status: RemoteStatus) {
        self.update_view(core, |view| view.status = status);
    }

    /// 改 `remote.json` 并落盘（没有加入同步组时什么都不做）。
    fn update_file(&self, update: impl FnOnce(&mut RemoteFile)) {
        let mut inner = self.lock();
        let Some(file) = inner.file.as_mut() else {
            return;
        };
        update(file);
        if let Err(err) = state::save(&self.dir, file) {
            log::warn!("remote sync state save failed: {err:#}");
        }
    }

    pub(super) fn run(&self) -> Option<Arc<Run>> {
        self.lock().run.clone()
    }

    pub(super) fn tick(&self, device_id: &str) -> String {
        self.lock().clock.tick().wire(device_id)
    }

    /// 加入（或建立）同步组的时间。
    fn joined_at(&self) -> Option<DateTime<Utc>> {
        self.lock().file.as_ref().map(|file| file.joined_at)
    }

    /// 本机的元数据改动要不要记下来同步：加入了自部署服务器的同步组（官方服务器不收元数据）、
    /// 开着「同步全部历史」。不看运行时：断网、刚启动还没连上时的改动也要留着，连上后再发。
    pub(super) fn tracks_meta(&self, core: &CoreInner) -> bool {
        self.meta_keys(core).is_some()
    }

    /// 记元数据改动要用的设备 id 与组密钥（条件见 [`Self::tracks_meta`]）。
    pub(super) fn meta_keys(&self, core: &CoreInner) -> Option<(String, GroupKeys)> {
        if !core.settings.snapshot().sync.remote.full_history {
            return None;
        }
        let inner = self.lock();
        let file = inner
            .file
            .as_ref()
            .filter(|file| matches!(file.server, ServerConfig::SelfHost { .. }))?;
        match GroupKeys::from_base64(&file.group_key) {
            Ok(keys) => Some((file.device_id.clone(), keys)),
            Err(err) => {
                log::warn!("remote sync group key is invalid: {err:#}");
                None
            }
        }
    }

    /// 宿主启用远程同步：之后按设置启停。
    pub(super) async fn start(&self, core: &Arc<CoreInner>) {
        self.started.store(true, Ordering::Release);
        self.apply(core).await;
    }

    /// 按当前设置启动、停止或更新运行时。「同步全部历史」刚打开时从头补齐一次：关着时元数据和
    /// 加入前的记录被跳过了。「同步设置」刚打开时重启运行时，启动时从头收一遍设置，游标不动。
    pub(super) async fn apply(&self, core: &Arc<CoreInner>) {
        if !self.started.load(Ordering::Acquire) {
            return;
        }
        let remote = core.settings.snapshot().sync.remote;
        let mut lifecycle = self.lifecycle.lock().await;
        let (configured, reset) = {
            let mut inner = self.lock();
            let previous = inner.applied.replace(remote.clone());
            let turned_on = |now: bool, before: Option<bool>| now && before == Some(false);
            let history_on = turned_on(
                remote.full_history,
                previous.as_ref().map(|p| p.full_history),
            );
            let settings_on = turned_on(remote.settings, previous.as_ref().map(|p| p.settings));
            if let Some(file) = inner.file.as_mut().filter(|_| history_on) {
                file.cursor = 0;
                file.caught_up = false;
                if let Err(err) = state::save(&self.dir, file) {
                    log::warn!("remote sync state save failed: {err:#}");
                }
            }
            (inner.file.is_some(), history_on || settings_on)
        };

        if !configured || !remote.enabled {
            if let Some(runtime) = lifecycle.take() {
                self.stop_runtime(runtime).await;
            }
            self.emit(core);
            return;
        }
        if let Some(runtime) = lifecycle.as_ref() {
            if !reset && !runtime.task.is_finished() {
                runtime.run.nudge.notify_one();
                return;
            }
        }
        if let Some(runtime) = lifecycle.take() {
            self.stop_runtime(runtime).await;
        }
        match self.start_runtime(core).await {
            Ok(runtime) => *lifecycle = runtime,
            Err(err) => {
                log::error!("remote sync start failed: {err:#}");
                let message = format!("{err:#}");
                self.set_status(core, RemoteStatus::Offline { message });
            }
        }
    }

    /// 断开并按当前状态重新连接（覆盖导入备份后游标归零时用）。
    async fn reconnect(&self, core: &Arc<CoreInner>) {
        if let Some(runtime) = self.lifecycle.lock().await.take() {
            self.stop_runtime(runtime).await;
        }
        self.apply(core).await;
    }

    async fn start_runtime(&self, core: &Arc<CoreInner>) -> anyhow::Result<Option<Runtime>> {
        let device_id = self.device_id(core)?;
        let Some(file) = self.lock().file.clone() else {
            return Ok(None);
        };
        if file.device_id != device_id {
            log::warn!("remote sync credentials belong to another device identity, dropping them");
            self.forget(core, false).await;
            return Ok(None);
        }

        let keys = GroupKeys::from_base64(&file.group_key)?;
        let client = Client::new(
            &file.server,
            Some(file.device_token.clone()),
            &core.info.version,
        )?;
        let pool = core.db.pool().await;
        if let Some((hlc, _)) = crate::db::remote::max_hlc(&pool)
            .await?
            .as_deref()
            .and_then(parse_hlc)
        {
            self.lock().clock.observe(hlc);
        }

        let (uploads, rx) = mpsc::channel(UPLOAD_QUEUE);
        let (policy, _) = watch::channel(None);
        let run = Arc::new(Run {
            client,
            keys,
            device_id,
            uploads,
            nudge: Notify::new(),
            connected: Notify::new(),
            policy,
            settings_pass: AtomicBool::new(true),
        });
        {
            let mut inner = self.lock();
            inner.run = Some(run.clone());
            inner.view.status = RemoteStatus::Connecting;
        }
        self.emit(core);
        let task = tokio::spawn(run_loop(Arc::downgrade(core), run.clone(), rx));
        log::info!("remote sync started for group {}", file.group_id);
        Ok(Some(Runtime { run, task }))
    }

    async fn stop_runtime(&self, runtime: Runtime) {
        {
            let mut inner = self.lock();
            if inner
                .run
                .as_ref()
                .is_some_and(|run| Arc::ptr_eq(run, &runtime.run))
            {
                inner.run = None;
            }
            inner.view.history_upload = None;
        }
        runtime.task.abort();
        let _ = runtime.task.await;
        log::info!("remote sync stopped");
    }

    /// core 关闭时停掉运行时；之后设置变化不再启动它。
    pub(super) async fn stop(&self) {
        self.started.store(false, Ordering::Release);
        if let Some(runtime) = self.lifecycle.lock().await.take() {
            self.stop_runtime(runtime).await;
        }
    }

    /// 本机的设备 id（局域网身份）。身份重新生成时局域网的配对关系一并作废。
    fn device_id(&self, core: &CoreInner) -> anyhow::Result<String> {
        let loaded = identity::load_or_create(&self.dir)?;
        if loaded.regenerated {
            core.sync.peers().forget_all();
        }
        Ok(loaded.identity.device_id().to_owned())
    }

    /// 清掉本机凭据与同步状态；`revoked` 时界面保留服务器并显示「已不在同步组里」。
    async fn forget(&self, core: &CoreInner, revoked: bool) {
        {
            let mut inner = self.lock();
            let server = inner.file.take().map(|file| file.server.view());
            inner.run = None;
            inner.view = View::default();
            if revoked {
                inner.view.revoked = server;
            }
        }
        if let Err(err) = state::remove(&self.dir) {
            log::warn!("{err:#}");
        }
        if let Err(err) = crate::db::remote::clear(&core.db.pool().await).await {
            log::warn!("remote sync state clear failed: {err}");
        }
        self.emit(core);
    }

    pub(super) fn snapshot(&self, core: &CoreInner) -> RemoteSyncState {
        let enabled = core.settings.snapshot().sync.remote.enabled;
        let inner = self.lock();
        let view = &inner.view;
        let status = if view.revoked.is_some() {
            RemoteStatus::Revoked
        } else if inner.file.is_none() {
            RemoteStatus::NotConfigured
        } else if !enabled {
            RemoteStatus::Paused
        } else {
            view.status.clone()
        };
        let now = Utc::now();
        RemoteSyncState {
            server: inner
                .file
                .as_ref()
                .map(|file| file.server.view())
                .or_else(|| view.revoked.clone()),
            policy: inner
                .run
                .as_ref()
                .and_then(|run| run.policy.borrow().clone()),
            devices: view.devices.clone(),
            pairing: view
                .pairing
                .clone()
                .filter(|pairing| pairing.expires_at > now),
            image_quota_until: view.image_quota_until.filter(|until| *until > now),
            last_sync_at: view.last_sync_at,
            history_upload: view
                .history_upload
                .filter(|_| status == RemoteStatus::Connected),
            status,
        }
    }

    /// 本机这次复制要不要发给同步组：运行中、设置允许这种内容、服务器收这种内容、图片额度没用完。
    pub(super) fn accepts(&self, core: &CoreInner, item: &ClipboardItem) -> bool {
        let remote = core.settings.snapshot().sync.remote;
        let inner = self.lock();
        let Some(run) = inner.run.as_ref().filter(|_| remote.enabled) else {
            return false;
        };
        let images = run
            .policy
            .borrow()
            .as_ref()
            .is_none_or(|policy| policy.images != RemoteImages::Off)
            && inner
                .view
                .image_quota_until
                .is_none_or(|until| until <= Utc::now());
        engine::is_syncable_types(item, remote.text, remote.image && images)
    }

    pub(super) fn capture(&self, item: &ClipboardItem, skip: Vec<String>) {
        let Some(run) = self.run() else {
            return;
        };
        let hlc = self.tick(&run.device_id);
        let upload = Upload::Capture {
            item: Box::new(item.clone()),
            hlc,
            at: Instant::now(),
            skip,
        };
        if let Err(mpsc::error::TrySendError::Full(_)) = run.uploads.try_send(upload) {
            log::warn!("remote sync upload queue is full, dropping a record");
        }
    }

    /// 覆盖导入备份后：本机状态表已清空，游标归零从头补齐。
    pub(super) async fn after_import(&self) {
        self.update_file(|file| {
            file.cursor = 0;
            file.caught_up = false;
        });
        if let Some(core) = self.bound_core() {
            self.reconnect(&core).await;
        }
    }

    // ---- 偏好页的操作 ----

    pub(super) async fn check_server(
        &self,
        core: &CoreInner,
        server: RemoteServer,
    ) -> Result<(ServerConfig, Client, RemotePolicy)> {
        let config = match server {
            RemoteServer::Official => ServerConfig::Official,
            RemoteServer::SelfHost { url, token } => ServerConfig::SelfHost {
                url: super::client::normalize_url(&url)
                    .ok_or_else(|| sync_error(core, Key::RemoteInvalidUrl))?,
                token: token.trim().to_owned(),
            },
        };
        let client = Client::new(&config, None, &core.info.version)?;
        let info = client.info().await.map_err(|err| api_error(core, &err))?;
        if let Some(message) = incompatibility(core, &info.protocol) {
            return Err(AppError::Sync(message));
        }
        Ok((config, client, info.policy.into()))
    }

    pub(super) async fn create_group(
        &self,
        core: &Arc<CoreInner>,
        server: RemoteServer,
    ) -> Result<()> {
        self.ensure_not_configured(core)?;
        let (server, client, _) = self.check_server(core, server).await?;
        let device_id = self.device_id(core)?;
        let keys = GroupKeys::new(random_key());
        let name = effective_device_name(&core.settings.snapshot().sync.lan);
        let info = keys.seal_device(&device_id, &device_info(core, &name))?;
        let credentials = client
            .create_group(&device_id, &info)
            .await
            .map_err(|err| api_error(core, &err))?;
        let file = RemoteFile {
            server,
            group_id: credentials.group_id,
            device_id,
            device_token: credentials.device_token,
            group_key: keys.group_base64(),
            cursor: 0,
            caught_up: false,
            device_name: Some(name),
            joined_at: Utc::now(),
        };
        self.adopt(core, file).await
    }

    pub(super) async fn join_group(
        &self,
        core: &Arc<CoreInner>,
        server: RemoteServer,
        code: &str,
    ) -> Result<()> {
        self.ensure_not_configured(core)?;
        let code = crate::sync::pairing::normalize_code(code, REMOTE_CODE_DIGITS)
            .ok_or_else(|| sync_error(core, Key::RemoteInvalidCode))?;
        let (server, client, _) = self.check_server(core, server).await?;
        let device_id = self.device_id(core)?;

        let (joiner, message) = pairing::start_joiner(&code);
        let reply = client
            .join(&code, &device_id, &to_hex(&message))
            .await
            .map_err(|err| api_error(core, &err))?;
        let wrong_code = || sync_error(core, Key::SyncWrongCode);
        let spake = from_hex(&reply.spake).map_err(|_| sync_error(core, Key::RemoteServerError))?;
        let shared = joiner.finish(&spake).map_err(|_| wrong_code())?;
        let key = pairing::pairing_key(&shared, &reply.session);
        let grant =
            pairing::open_grant(&key, &reply.session, &reply.sealed).map_err(|_| wrong_code())?;
        let keys = GroupKeys::from_base64(&grant.group_key).map_err(|_| wrong_code())?;

        let name = effective_device_name(&core.settings.snapshot().sync.lan);
        let info = keys.seal_device(&device_id, &device_info(core, &name))?;
        let credentials = client
            .complete_join(&reply.session, &grant.join_secret, &info)
            .await
            .map_err(|err| api_error(core, &err))?;
        let file = RemoteFile {
            server,
            group_id: credentials.group_id,
            device_id,
            device_token: credentials.device_token,
            group_key: keys.group_base64(),
            cursor: 0,
            caught_up: false,
            device_name: Some(name),
            joined_at: Utc::now(),
        };
        self.adopt(core, file).await
    }

    fn ensure_not_configured(&self, core: &CoreInner) -> Result<()> {
        if self.lock().file.is_some() {
            return Err(sync_error(core, Key::RemoteAlreadyConfigured));
        }
        Ok(())
    }

    /// 建组或加入成功：保存凭据，清掉旧的同步状态，打开远程同步（设置变化随之启动运行时）。
    async fn adopt(&self, core: &Arc<CoreInner>, file: RemoteFile) -> Result<()> {
        state::save(&self.dir, &file)?;
        crate::db::remote::clear(&core.db.pool().await).await?;
        {
            let mut inner = self.lock();
            inner.file = Some(file);
            inner.view = View::default();
        }
        self.emit(core);
        Core(core.clone())
            .update_settings(json!({ "sync": { "remote": { "enabled": true } } }))
            .await?;
        Ok(())
    }

    /// 成员接口的客户端：运行中用运行时的，否则按本机凭据新建。
    fn member(&self, core: &CoreInner) -> Result<(Client, String)> {
        if let Some(run) = self.run() {
            return Ok((run.client.clone(), run.device_id.clone()));
        }
        let Some(file) = self.lock().file.clone() else {
            return Err(sync_error(core, Key::RemoteNotConfigured));
        };
        let client = Client::new(&file.server, Some(file.device_token), &core.info.version)?;
        Ok((client, file.device_id))
    }

    pub(super) async fn request_code(&self, core: &CoreInner) -> Result<RemotePairingCode> {
        let (client, _) = self.member(core)?;
        let reply = client
            .request_pairing()
            .await
            .map_err(|err| api_error(core, &err))?;
        let code = RemotePairingCode {
            code: reply.code,
            expires_at: reply.expires_at,
        };
        let shown = code.clone();
        self.update_view(core, |view| view.pairing = Some(shown));
        self.expire_code_later(&code);
        Ok(code)
    }

    /// 配对码到期时从界面上撤掉（服务器那边同时作废），不用等下一次别的刷新。
    fn expire_code_later(&self, code: &RemotePairingCode) {
        let Some(owner) = self.bound_core() else {
            return;
        };
        let wait = (code.expires_at - Utc::now()).to_std().unwrap_or_default();
        let expiring = code.code.clone();
        let weak = Arc::downgrade(&owner);
        owner.rt.clone().spawn(async move {
            tokio::time::sleep(wait).await;
            let Some(core) = weak.upgrade() else {
                return;
            };
            let shown = core.remote.lock().view.pairing.clone();
            if shown.is_some_and(|pairing| pairing.code == expiring) {
                core.remote.update_view(&core, |view| view.pairing = None);
            }
        });
    }

    /// 作废配对码。本机先不再应答配对请求，通知服务器失败只记日志：码最多 5 分钟后自己过期。
    pub(super) async fn cancel_code(&self, core: &CoreInner) -> Result<()> {
        self.update_view(core, |view| view.pairing = None);
        let (client, _) = self.member(core)?;
        if let Err(err) = client.cancel_pairing().await {
            log::warn!("remote sync pairing cancel failed: {err}");
        }
        Ok(())
    }

    pub(super) async fn remove_device(&self, core: &CoreInner, device_id: &str) -> Result<()> {
        let (client, self_id) = self.member(core)?;
        if device_id == self_id {
            return self.leave(core).await;
        }
        client
            .remove_device(device_id)
            .await
            .map_err(|err| api_error(core, &err))?;
        self.update_view(core, |view| {
            view.devices.retain(|device| device.id != device_id)
        });
        Ok(())
    }

    /// 退出同步组：先停运行时，再请服务器移除本机（失败只记日志，本机照样退出），最后清掉本机状态。
    pub(super) async fn leave(&self, core: &CoreInner) -> Result<()> {
        if let Some(runtime) = self.lifecycle.lock().await.take() {
            self.stop_runtime(runtime).await;
        }
        let file = self.lock().file.clone();
        if let Some(file) = file {
            let removed =
                match Client::new(&file.server, Some(file.device_token), &core.info.version) {
                    Ok(client) => client
                        .remove_device(&file.device_id)
                        .await
                        .map_err(|err| err.to_string()),
                    Err(err) => Err(format!("{err:#}")),
                };
            if let Err(err) = removed {
                log::warn!("remote sync leave request failed: {err}");
            }
        }
        self.forget(core, false).await;
        Ok(())
    }

    // ---- 运行时用 ----

    fn set_policy(&self, core: &CoreInner, run: &Run, policy: RemotePolicy) {
        run.policy.send_replace(Some(policy));
        self.emit(core);
    }

    /// 事件流从游标开始（补齐被打断时从断的地方接着补）。第一次补齐完成之前、不同步全部历史时只要设置；
    /// 之后要全部。
    fn stream_position(&self, history: bool) -> (u64, bool) {
        let inner = self.lock();
        inner.file.as_ref().map_or((0, history), |file| {
            (file.cursor, file.caught_up || history)
        })
    }

    /// 推进游标。`synced` 时直接设成服务器的当前最大序号（服务器数据重置过时会变小）并记下补齐完成。
    fn advance_cursor(&self, core: &CoreInner, seq: u64, synced: bool) {
        self.update_file(|file| {
            if synced {
                file.cursor = seq;
                file.caught_up = true;
            } else {
                file.cursor = file.cursor.max(seq);
            }
        });
        self.update_view(core, |view| view.last_sync_at = Some(Utc::now()));
    }

    /// 正在显示、还没过期的配对码。
    fn active_code(&self) -> Option<String> {
        self.lock()
            .view
            .pairing
            .as_ref()
            .filter(|pairing| pairing.expires_at > Utc::now())
            .map(|pairing| pairing.code.clone())
    }

    fn device_platform(&self, device_id: &str) -> Platform {
        self.lock()
            .view
            .devices
            .iter()
            .find(|device| device.id == device_id)
            .map_or_else(current_platform, |device| device.platform)
    }

    /// 处理一批上传的结果（和发出的帧一一对应）：服务器收下的本机记录与元数据改动不再待发；
    /// 图片额度用完时图片暂停到 `retryAt`，文本的额度不影响图片。
    async fn upload_results(
        &self,
        core: &CoreInner,
        frames: &[&Outgoing],
        results: &[super::client::UploadResult],
    ) {
        let pool = core.db.pool().await;
        let mut quota = None;
        for (frame, result) in frames.iter().zip(results) {
            match (result.status.as_str(), result.error.as_deref()) {
                ("rejected", Some("quota")) if frame.class == "image" => {
                    quota = result.retry_at.or(quota);
                }
                ("rejected", error) => log::info!(
                    "remote sync entry {} rejected: {}",
                    result.entry_id,
                    error.unwrap_or("unknown")
                ),
                _ if matches!(frame.kind, entries::ITEM | entries::META) => {
                    let confirmed =
                        crate::db::remote::confirm(&pool, &frame.entry_id, frame.kind, &frame.hlc)
                            .await;
                    if let Err(err) = confirmed {
                        log::warn!("remote sync state update failed: {err}");
                    }
                }
                _ => {}
            }
        }
        // 服务器不把本机上传的条目发回来：序号正好接着游标时直接推进，否则重连时会把它们整份再补一遍。
        // 中间隔着别的设备的条目就不动，那些还要从事件流收。
        let stored: Vec<u64> = results
            .iter()
            .filter(|result| result.status == "stored")
            .filter_map(|result| result.seq)
            .collect();
        if !stored.is_empty() {
            self.update_file(|file| {
                for seq in stored {
                    if file.caught_up && seq == file.cursor + 1 {
                        file.cursor = seq;
                    }
                }
            });
        }
        self.update_view(core, |view| {
            view.last_sync_at = Some(Utc::now());
            if quota.is_some() {
                view.image_quota_until = quota;
            }
        });
    }
}

fn sync_error(core: &CoreInner, key: Key) -> AppError {
    AppError::Sync(label(core.language(), key).to_owned())
}

fn device_info(core: &CoreInner, name: &str) -> DeviceInfo {
    DeviceInfo {
        name: name.to_owned(),
        platform: current_platform(),
        app_version: core.info.version.to_string(),
    }
}

/// 给用户看的错误：服务器错误码对应的根因，技术细节写日志。
fn api_error(core: &CoreInner, err: &ApiError) -> AppError {
    log::warn!("remote sync request failed: {err}");
    let key = match err {
        ApiError::Network(_) => Key::RemoteUnreachable,
        ApiError::Invalid(_) => Key::RemoteServerError,
        ApiError::Server { code, .. } => match code.as_str() {
            "serverToken" => Key::RemoteServerToken,
            "unauthorized" => Key::RemoteRevoked,
            "codeInvalid" => Key::RemoteCodeInvalid,
            "inviterOffline" => Key::RemoteInviterOffline,
            "pairingTimeout" => Key::RemotePairingTimeout,
            "groupFull" => Key::RemoteGroupFull,
            "rateLimited" | "quota" => Key::RemoteRateLimited,
            "incompatible" => Key::RemoteIncompatible,
            _ => Key::RemoteServerError,
        },
    };
    sync_error(core, key)
}

/// 协议版本没有交集时给用户看的原因：哪边旧按服务器报的范围判断。
fn incompatibility(core: &CoreInner, range: &VersionRange) -> Option<String> {
    if (range.min..=range.max).contains(&PROTOCOL) {
        return None;
    }
    let key = if range.max < PROTOCOL {
        Key::RemoteServerOutdated
    } else {
        Key::SyncSelfOutdated
    };
    Some(label(core.language(), key).to_owned())
}

/// 连接失败怎么办：令牌失效与版本不兼容停下，其余退避重试（`Retry-After` 优先）。
fn end_for(core: &CoreInner, err: &ApiError, backoff: Duration) -> End {
    log::warn!("remote sync connection failed: {err}");
    let offline = |key| RemoteStatus::Offline {
        message: label(core.language(), key).to_owned(),
    };
    match err {
        ApiError::Server { code, .. } if code == "unauthorized" => End::Revoked,
        ApiError::Server { code, .. } if code == "serverToken" => End::ServerToken,
        ApiError::Server { code, .. } if code == "incompatible" => {
            End::Incompatible(label(core.language(), Key::RemoteIncompatible).to_owned())
        }
        ApiError::Server {
            status,
            retry_after,
            ..
        } if status.as_u16() == 429 => {
            let wait = retry_after.unwrap_or(backoff);
            let now = Utc::now();
            let until = chrono::Duration::from_std(wait)
                .ok()
                .and_then(|wait| now.checked_add_signed(wait))
                .unwrap_or(now);
            End::Retry {
                status: RemoteStatus::RateLimited { until },
                wait: Some(wait),
            }
        }
        ApiError::Server { retry_after, .. } => End::Retry {
            status: offline(Key::RemoteServerError),
            wait: *retry_after,
        },
        ApiError::Invalid(_) => End::Retry {
            status: offline(Key::RemoteServerError),
            wait: None,
        },
        ApiError::Network(_) => End::Retry {
            status: offline(Key::RemoteUnreachable),
            wait: None,
        },
    }
}

fn disconnected(core: &CoreInner, reason: &str) -> End {
    log::info!("remote sync stream ended: {reason}");
    End::Retry {
        status: RemoteStatus::Offline {
            message: label(core.language(), Key::RemoteUnreachable).to_owned(),
        },
        wait: None,
    }
}

/// 主循环：连接、读事件流、按结果重连或停下。被终止时上传任务跟着停。
async fn run_loop(core: Weak<CoreInner>, run: Arc<Run>, rx: mpsc::Receiver<Upload>) {
    let _uploader = AbortOnDrop(tokio::spawn(upload_loop(core.clone(), run.clone(), rx)));
    let mut backoff = BACKOFF_MIN;
    loop {
        let Some(core) = core.upgrade() else {
            return;
        };
        core.remote.set_status(&core, RemoteStatus::Connecting);
        let wait = match session(&core, &run, &mut backoff).await {
            End::Revoked => {
                log::warn!("remote sync: this device was removed from the group");
                core.remote.forget(&core, true).await;
                return;
            }
            End::ServerToken => {
                stopped(&core, &run, RemoteStatus::ServerTokenInvalid);
                return;
            }
            End::Incompatible(message) => {
                stopped(&core, &run, RemoteStatus::Incompatible { message });
                return;
            }
            End::Retry { status, wait } => {
                core.remote.set_status(&core, status);
                let wait = wait.unwrap_or(backoff);
                backoff = (backoff * 2).min(BACKOFF_MAX);
                wait
            }
        };
        drop(core);
        tokio::time::sleep(wait).await;
    }
}

/// 停下不再重试：不再接收要上传的内容。
fn stopped(core: &CoreInner, run: &Arc<Run>, status: RemoteStatus) {
    {
        let mut inner = core.remote.lock();
        if inner
            .run
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, run))
        {
            inner.run = None;
        }
        inner.view.status = status;
        inner.view.history_upload = None;
    }
    core.remote.emit(core);
}

/// 一次连接：查服务器信息与策略、登记设备名、拉设备列表，然后读事件流直到断开。
async fn session(core: &Arc<CoreInner>, run: &Arc<Run>, backoff: &mut Duration) -> End {
    let info = match run.client.info().await {
        Ok(info) => info,
        Err(err) => return end_for(core, &err, *backoff),
    };
    if let Some(message) = incompatibility(core, &info.protocol) {
        return End::Incompatible(message);
    }
    let policy: RemotePolicy = info.policy.into();
    core.remote.set_policy(core, run, policy.clone());
    if let Err(err) = publish_name(core, run).await {
        return end_for(core, &err, *backoff);
    }
    if let Err(err) = refresh_devices(core, run).await {
        return end_for(core, &err, *backoff);
    }

    let remote = core.settings.snapshot().sync.remote;
    if settings_active(&remote, &policy) && run.settings_pass.load(Ordering::Acquire) {
        if let Some(end) = settings_pass(core, run, &policy, *backoff).await {
            return end;
        }
        run.settings_pass.store(false, Ordering::Release);
    }
    let (after, all) = core
        .remote
        .stream_position(history_active(&remote, &policy));
    let mut response = match run.client.events(after, all).await {
        Ok(response) => response,
        Err(err) => return end_for(core, &err, *backoff),
    };
    core.remote.set_status(core, RemoteStatus::Connected);
    run.connected.notify_waiters();

    let mut reader = frame_reader(&policy);
    let mut progressed = false;
    loop {
        tokio::select! {
            end = read_chunk(core, &mut response, &mut reader) => {
                if let Some(end) = end {
                    return end;
                }
            }
            () = run.nudge.notified() => {
                settings_nudged(core, run, &policy).await;
                continue;
            }
        }
        let mut reload = false;
        loop {
            let frame = match reader.next_frame() {
                Ok(Some(frame)) => frame,
                Ok(None) => break,
                Err(err) => return disconnected(core, &format!("{err:#}")),
            };
            if let Some(end) =
                handle_frame(core, run, &policy, frame, &mut progressed, &mut reload).await
            {
                return end;
            }
            // 补齐有进展就不算连不上：服务器因为积压断开正在补齐的流时，下次马上接着补。
            if progressed {
                *backoff = BACKOFF_MIN;
            }
        }
        if reload {
            core.events.emit(CoreEvent::ClipboardReloaded);
        }
    }
}

/// 每次启动运行时先从头收一遍设置条目（`items=none`），游标不动：「同步设置」关着时、
/// 或旧版本不认识的设置项当时被跳过了，这里补上。收到 `synced` 就结束，之后才开正常的事件流。
async fn settings_pass(
    core: &Arc<CoreInner>,
    run: &Arc<Run>,
    policy: &RemotePolicy,
    backoff: Duration,
) -> Option<End> {
    let mut response = match run.client.events(0, false).await {
        Ok(response) => response,
        Err(err) => return Some(end_for(core, &err, backoff)),
    };
    let mut reader = frame_reader(policy);
    loop {
        if let Some(end) = read_chunk(core, &mut response, &mut reader).await {
            return Some(end);
        }
        loop {
            let (header, body) = match reader.next_frame() {
                Ok(Some(frame)) => frame,
                Ok(None) => break,
                Err(err) => return Some(disconnected(core, &format!("{err:#}"))),
            };
            let Ok(header) = serde_json::from_slice::<EventHeader>(&header) else {
                continue;
            };
            match header.event.as_str() {
                "entry" => {
                    if let Err(err) = receive_entry(core, run, policy, &header, &body).await {
                        log::warn!("remote sync skips entry {}: {err:#}", header.entry_id);
                    }
                }
                "synced" => return None,
                "revoked" => return Some(End::Revoked),
                _ => {}
            }
        }
    }
}

fn frame_reader(policy: &RemotePolicy) -> FrameReader {
    let max_body = policy.max_entry_bytes.max(policy.image_max_bytes);
    FrameReader::new(usize::try_from(max_body).unwrap_or(usize::MAX))
}

/// 读事件流的下一块进 `reader`；流断了返回为什么断。
async fn read_chunk(
    core: &CoreInner,
    response: &mut reqwest::Response,
    reader: &mut FrameReader,
) -> Option<End> {
    match tokio::time::timeout(STREAM_READ_TIMEOUT, response.chunk()).await {
        Ok(Ok(Some(bytes))) => {
            reader.push(&bytes);
            None
        }
        Ok(Ok(None)) => Some(disconnected(core, "closed by the server")),
        Ok(Err(err)) => Some(disconnected(core, &err.to_string())),
        Err(_) => Some(disconnected(core, "read timed out")),
    }
}

/// 事件流的帧头；不同类型用到的字段不同，缺的取默认值。
#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct EventHeader {
    #[serde(rename = "type")]
    event: String,
    seq: Option<u64>,
    entry_id: String,
    kind: String,
    hlc: String,
    from: String,
    live: bool,
    session: String,
    spake: String,
}

async fn handle_frame(
    core: &Arc<CoreInner>,
    run: &Arc<Run>,
    policy: &RemotePolicy,
    (header, body): (Vec<u8>, Vec<u8>),
    progressed: &mut bool,
    reload: &mut bool,
) -> Option<End> {
    let header: EventHeader = match serde_json::from_slice(&header) {
        Ok(header) => header,
        Err(err) => {
            log::debug!("remote sync skips an unreadable frame: {err}");
            return None;
        }
    };
    match header.event.as_str() {
        "entry" => {
            match receive_entry(core, run, policy, &header, &body).await {
                Ok(changed) => *reload |= changed,
                Err(err) => log::warn!("remote sync skips entry {}: {err:#}", header.entry_id),
            }
            // 补齐按序号从小到大发，实时条目要等 `synced` 之后才来：带序号的条目处理完就推进游标，
            // 补齐被打断（睡眠、断网、退出）时从这里接着补。
            match header.seq {
                Some(seq) => {
                    core.remote.advance_cursor(core, seq, false);
                    *progressed = true;
                }
                None if header.live => core
                    .remote
                    .update_view(core, |view| view.last_sync_at = Some(Utc::now())),
                None => {}
            }
        }
        "synced" => {
            core.remote
                .advance_cursor(core, header.seq.unwrap_or_default(), true);
            *progressed = true;
            after_catch_up(core, run, policy).await;
        }
        "pairRequest" => answer_pairing(core, run, &header).await,
        "devices" => {
            if let Err(err) = refresh_devices(core, run).await {
                log::warn!("remote sync device refresh failed: {err}");
            }
        }
        "revoked" => return Some(End::Revoked),
        _ => {}
    }
    None
}

fn history_active(remote: &RemoteSync, policy: &RemotePolicy) -> bool {
    remote.full_history && policy.history
}

fn meta_active(remote: &RemoteSync, policy: &RemotePolicy) -> bool {
    remote.full_history && policy.history && policy.meta
}

fn settings_active(remote: &RemoteSync, policy: &RemotePolicy) -> bool {
    remote.settings && policy.settings
}

/// 收到的记录按远程同步的设置入库：图片上限是服务器的上限与（自选档时）用户设的上限。
fn receive_options(remote: &RemoteSync, policy: &RemotePolicy) -> ReceiveOptions {
    let max_image_bytes = if policy.image_compress.is_some() {
        policy.image_max_bytes
    } else {
        policy.image_max_bytes.min(remote.max_image_bytes())
    };
    ReceiveOptions {
        text: remote.text,
        image: remote.image && policy.images != RemoteImages::Off,
        write_clipboard: remote.write_clipboard,
        max_image_bytes,
        allow_jpeg: false,
        allow_webp: true,
        captured_at: None,
    }
}

/// 解开一个条目并按类型处理；返回本机记录是否有变化（列表要刷新）。
async fn receive_entry(
    core: &Arc<CoreInner>,
    run: &Arc<Run>,
    policy: &RemotePolicy,
    header: &EventHeader,
    body: &[u8],
) -> anyhow::Result<bool> {
    let plain = run
        .keys
        .open_entry(&header.entry_id, &header.kind, &header.hlc, body)?;
    if let Some((hlc, _)) = parse_hlc(&header.hlc) {
        core.remote.lock().clock.observe(hlc);
    }
    let remote = core.settings.snapshot().sync.remote;
    match header.kind.as_str() {
        entries::ITEM => {
            if header.from == run.device_id {
                if policy.history {
                    let pool = core.db.pool().await;
                    entries::note_own_item(&pool, &run.keys, &plain, &header.hlc).await?;
                }
                return Ok(false);
            }
            let history = history_active(&remote, policy);
            let incoming = Incoming {
                hlc: &header.hlc,
                from: &header.from,
                live: header.live,
                since: if history {
                    None
                } else {
                    core.remote.joined_at()
                },
            };
            entries::receive_item(
                core,
                &run.keys,
                core.sync.echo(),
                &incoming,
                &plain,
                receive_options(&remote, policy),
                core.remote.device_platform(&header.from),
                policy.history,
                meta_active(&remote, policy),
            )
            .await
        }
        entries::META if meta_active(&remote, policy) => {
            let (json, _) = unpack(&plain)?;
            let entry: MetaEntry = serde_json::from_slice(json).context("invalid meta entry")?;
            entries::apply_meta(core, &run.keys, &header.hlc, entry).await
        }
        remote_settings::KIND if settings_active(&remote, policy) => {
            let (json, _) = unpack(&plain)?;
            let entry: SettingsEntry =
                serde_json::from_slice(json).context("invalid settings entry")?;
            remote_settings::apply_incoming(core, &header.entry_id, &header.hlc, entry).await?;
            Ok(false)
        }
        _ => Ok(false),
    }
}

/// 补齐结束：上传设置差异（新加入的设备这时已经采用了组里的值）；同步全部历史时重发待发的元数据改动，
/// 并上传服务器还没收下的记录（第一次是整个历史，之后是断网、退出时没发出去的）。
async fn after_catch_up(core: &Arc<CoreInner>, run: &Arc<Run>, policy: &RemotePolicy) {
    let remote = core.settings.snapshot().sync.remote;
    if settings_active(&remote, policy) {
        if let Err(err) = upload_settings(core, run).await {
            log::warn!("remote sync settings upload failed: {err:#}");
        }
    }
    if history_active(&remote, policy) {
        let _ = run.uploads.send(Upload::History).await;
    }
}

/// 设置变了：登记新的设备名；补齐过之后上传变了的设置叶子。
async fn settings_nudged(core: &Arc<CoreInner>, run: &Arc<Run>, policy: &RemotePolicy) {
    if let Err(err) = publish_name(core, run).await {
        log::warn!("remote sync device name update failed: {err}");
    }
    let caught_up = core
        .remote
        .lock()
        .file
        .as_ref()
        .is_some_and(|file| file.caught_up);
    if caught_up && settings_active(&core.settings.snapshot().sync.remote, policy) {
        if let Err(err) = upload_settings(core, run).await {
            log::warn!("remote sync settings upload failed: {err:#}");
        }
    }
}

/// 上传和上次同步的值不同的设置叶子。
async fn upload_settings(core: &Arc<CoreInner>, run: &Arc<Run>) -> anyhow::Result<()> {
    let pool = core.db.pool().await;
    let settings = core.settings.snapshot();
    for (entry_id, entry) in remote_settings::changed_leaves(&pool, &run.keys, &settings).await? {
        let hlc = core.remote.tick(&run.device_id);
        remote_settings::record(&pool, &entry_id, &entry.path, &entry.value, &hlc).await?;
        let plain = super::crypto::pack(&serde_json::to_vec(&entry)?, &[])?;
        let body = run
            .keys
            .seal_entry(&entry_id, remote_settings::KIND, &hlc, &plain)?;
        let frame = Outgoing {
            entry_id,
            kind: remote_settings::KIND,
            class: "none",
            hlc,
            live_since: Some(Instant::now()),
            skip: Vec::new(),
            body,
        };
        run.uploads
            .send(Upload::Frame(frame))
            .await
            .map_err(|_| anyhow!("upload task stopped"))?;
    }
    Ok(())
}

/// 设备名和服务器上登记的不同时更新。
async fn publish_name(core: &CoreInner, run: &Run) -> std::result::Result<(), ApiError> {
    let name = effective_device_name(&core.settings.snapshot().sync.lan);
    let published = core
        .remote
        .lock()
        .file
        .as_ref()
        .and_then(|file| file.device_name.clone());
    if published.as_deref() == Some(name.as_str()) {
        return Ok(());
    }
    let info = run
        .keys
        .seal_device(&run.device_id, &device_info(core, &name))
        .map_err(|err| ApiError::Invalid(format!("{err:#}")))?;
    run.client.update_self(&info).await?;
    core.remote
        .update_file(|file| file.device_name = Some(name));
    Ok(())
}

/// 重新拉设备列表。有新设备加入时，本机显示的配对码已经用掉了，不再显示。
async fn refresh_devices(core: &CoreInner, run: &Run) -> std::result::Result<(), ApiError> {
    let rows = run.client.devices().await?;
    let mut devices: Vec<RemoteDeviceView> = rows
        .into_iter()
        .map(|row| {
            let info = run.keys.open_device(&row.device_id, &row.info).ok();
            let is_self = row.device_id == run.device_id;
            RemoteDeviceView {
                name: info
                    .as_ref()
                    .map_or_else(|| row.device_id.clone(), |info| info.name.clone()),
                platform: info.map_or_else(current_platform, |info| info.platform),
                online: row.online || is_self,
                last_seen_at: row
                    .last_seen_at
                    .and_then(|time| DateTime::parse_from_rfc3339(&time).ok())
                    .map(|time| time.with_timezone(&Utc)),
                is_self,
                id: row.device_id,
            }
        })
        .collect();
    devices.sort_by(|a, b| b.is_self.cmp(&a.is_self).then_with(|| a.name.cmp(&b.name)));
    core.remote.update_view(core, |view| {
        let joined = devices
            .iter()
            .any(|device| !view.devices.iter().any(|known| known.id == device.id));
        if joined && !view.devices.is_empty() {
            view.pairing = None;
        }
        view.devices = devices;
    });
    Ok(())
}

/// 有设备用本机申请的配对码加入：本机正显示一个有效的码时自动应答（REMOTE.md §4.4）。
/// 本机不知道码对不对，码不对时加入方解不开密封的内容。
async fn answer_pairing(core: &CoreInner, run: &Run, header: &EventHeader) {
    let Some(code) = core.remote.active_code() else {
        log::info!("remote sync ignores a pairing request without an active code");
        return;
    };
    let answered = async {
        let joiner_message = from_hex(&header.spake)?;
        let (member, message) = pairing::start_member(&code);
        let shared = member
            .finish(&joiner_message)
            .map_err(|err| anyhow!("spake2 failed: {err}"))?;
        let key = pairing::pairing_key(&shared, &header.session);
        let secret = random_key();
        let grant = JoinGrant {
            group_key: run.keys.group_base64(),
            join_secret: to_hex(&secret),
        };
        let answer = PairingAnswer {
            spake: to_hex(&message),
            sealed: pairing::seal_grant(&key, &header.session, &grant)?,
            join_proof: pairing::join_proof(&secret),
        };
        run.client
            .answer_pairing(&header.session, &answer)
            .await
            .map_err(|err| anyhow!("{err}"))
    }
    .await;
    if let Err(err) = answered {
        log::warn!("remote sync pairing answer failed: {err:#}");
    }
}

/// 上传任务：串行发出，排队的帧合进下一个请求；有历史要传时每传一页先让排队的实时内容插进来。
async fn upload_loop(core: Weak<CoreInner>, run: Arc<Run>, mut rx: mpsc::Receiver<Upload>) {
    // 图片怎么压缩、上限多少，要等第一次连上服务器拿到策略。
    let mut policy_rx = run.policy.subscribe();
    if policy_rx.wait_for(Option::is_some).await.is_err() {
        return;
    }
    let mut history: Option<HistoryUpload> = None;
    loop {
        let mut jobs = Vec::new();
        if history.is_none() {
            let Some(job) = rx.recv().await else {
                return;
            };
            jobs.push(job);
        }
        while let Ok(job) = rx.try_recv() {
            jobs.push(job);
        }
        let Some(core) = core.upgrade() else {
            return;
        };
        let Some(policy) = run.policy.borrow().clone() else {
            return;
        };

        let mut frames = Vec::new();
        for job in jobs {
            match job {
                Upload::Frame(frame) => frames.push(frame),
                Upload::Capture {
                    item,
                    hlc,
                    at,
                    skip,
                } => {
                    let track = policy.history;
                    let at = Some(at);
                    match entries::item_frame(
                        &core, &run.keys, &policy, &item, hlc, at, skip, track,
                    )
                    .await
                    {
                        Ok(Some(frame)) => frames.push(frame),
                        Ok(None) => {}
                        Err(err) => log::warn!("remote sync skips a record: {err:#}"),
                    }
                }
                Upload::History => {
                    let meta = meta_active(&core.settings.snapshot().sync.remote, &policy);
                    if meta {
                        let pool = core.db.pool().await;
                        match entries::pending_meta(&pool, &run.keys).await {
                            Ok(pending) => frames.extend(pending),
                            Err(err) => log::warn!("remote sync pending metadata skipped: {err:#}"),
                        }
                    }
                    if history.is_none() {
                        match HistoryUpload::start(&core, meta).await {
                            Ok(upload) => history = Some(upload),
                            Err(err) => {
                                log::warn!("remote sync history upload failed to start: {err:#}")
                            }
                        }
                    }
                }
            }
        }
        if !post(&core, &run, frames).await {
            return;
        }
        if let Some(upload) = history.as_mut() {
            match upload.next_page(&core, &run, &policy).await {
                Ok(false) => {}
                Ok(true) => history = None,
                Err(err) => {
                    log::warn!("remote sync history upload stopped: {err:#}");
                    core.remote
                        .update_view(&core, |view| view.history_upload = None);
                    history = None;
                }
            }
        }
    }
}

/// 把这些帧按 8 MiB 分批串行上传；网络或服务器暂时出错时退避重试，请求本身不合法时丢掉这批。
/// 返回 `false` 表示凭据或版本出了问题，上传停下（主循环会从事件流上发现同样的问题）。
async fn post(core: &CoreInner, run: &Run, frames: Vec<Outgoing>) -> bool {
    let mut batches: Vec<Vec<Outgoing>> = Vec::new();
    let mut size = 0;
    for frame in frames {
        let frame_size = frame.body.len() + 1024;
        if batches.is_empty() || size + frame_size > MAX_REQUEST_BYTES {
            batches.push(Vec::new());
            size = 0;
        }
        size += frame_size;
        if let Some(batch) = batches.last_mut() {
            batch.push(frame);
        }
    }

    for batch in batches {
        let mut backoff = BACKOFF_MIN;
        loop {
            let mut body = Vec::new();
            let mut sent = Vec::new();
            for frame in &batch {
                match encode_frame(&mut body, &frame.header(), &frame.body) {
                    Ok(()) => sent.push(frame),
                    Err(err) => log::warn!("remote sync skips an entry: {err:#}"),
                }
            }
            log::debug!("remote sync uploads {} entries", sent.len());
            let err = match run.client.upload(body).await {
                Ok(results) => {
                    core.remote.upload_results(core, &sent, &results).await;
                    break;
                }
                Err(err) => err,
            };
            log::warn!("remote sync upload failed: {err}");
            let wait = match &err {
                ApiError::Server { code, .. }
                    if matches!(
                        code.as_str(),
                        "unauthorized" | "serverToken" | "incompatible"
                    ) =>
                {
                    return false;
                }
                ApiError::Server {
                    status,
                    retry_after,
                    ..
                } if status.as_u16() == 429 || status.is_server_error() => {
                    retry_after.unwrap_or(backoff)
                }
                ApiError::Server { .. } => break,
                ApiError::Network(_) | ApiError::Invalid(_) => backoff,
            };
            tokio::select! {
                () = tokio::time::sleep(wait) => backoff = (backoff * 2).min(BACKOFF_MAX),
                () = run.connected.notified() => backoff = BACKOFF_MIN,
            }
        }
    }
    true
}

/// 同步全部历史时上传服务器还没收下的记录，按页进行。
struct HistoryUpload {
    ids: Vec<String>,
    next: usize,
}

impl HistoryUpload {
    async fn start(core: &CoreInner, meta: bool) -> anyhow::Result<Self> {
        let remote = core.settings.snapshot().sync.remote;
        let pool = core.db.pool().await;
        let ids =
            crate::db::remote::history_item_ids(&pool, remote.text, remote.image, meta).await?;
        if !ids.is_empty() {
            core.remote.update_view(core, |view| {
                view.history_upload = Some((0, ids.len() as u64));
            });
        }
        Ok(Self { ids, next: 0 })
    }

    /// 准备并上传下一页（最多 [`HISTORY_PAGE`] 条、约一个请求的体积，页与页之间排队的实时内容先发）；
    /// 返回是否结束（传完，或「同步全部历史」已经关掉）。
    async fn next_page(
        &mut self,
        core: &Arc<CoreInner>,
        run: &Run,
        policy: &RemotePolicy,
    ) -> anyhow::Result<bool> {
        let remote = core.settings.snapshot().sync.remote;
        if !history_active(&remote, policy) {
            core.remote
                .update_view(core, |view| view.history_upload = None);
            return Ok(true);
        }
        let meta = meta_active(&remote, policy);
        let pool = core.db.pool().await;
        let page_end = (self.next + HISTORY_PAGE).min(self.ids.len());
        let mut frames = Vec::new();
        let mut bytes = 0;
        while self.next < page_end && bytes < MAX_REQUEST_BYTES {
            let id = &self.ids[self.next];
            self.next += 1;
            let Some(item) = crate::db::items::find_item_by_id(&pool, id).await? else {
                continue;
            };
            if !engine::is_syncable_types(&item, remote.text, remote.image) {
                continue;
            }
            let tick = || core.remote.tick(&run.device_id);
            match entries::history_frames(core, &run.keys, policy, &item, meta, tick).await {
                Ok(item_frames) => {
                    bytes += item_frames
                        .iter()
                        .map(|frame| frame.body.len())
                        .sum::<usize>();
                    frames.extend(item_frames);
                }
                Err(err) => log::warn!("remote sync skips a history record: {err:#}"),
            }
        }
        if !post(core, run, frames).await {
            return Err(anyhow!("upload stopped"));
        }

        let total = self.ids.len() as u64;
        let done = self.next == self.ids.len();
        let next = self.next as u64;
        core.remote.update_view(core, |view| {
            view.history_upload = (!done).then_some((next, total));
        });
        Ok(done)
    }
}
