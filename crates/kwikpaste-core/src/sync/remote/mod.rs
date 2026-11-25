//! 远程同步：通过官方服务器或自部署的同步服务器互通，协议见上一级目录的 `REMOTE.md`。
//!
//! - [`state`]：本机状态 `remote.json`（服务器、凭据、组密钥、游标）；
//! - [`client`]：HTTP 接口与帧；
//! - [`crypto`]：组密钥派生、条目 id、加解密与 HLC；
//! - [`pairing`]：用配对码加入同步组；
//! - [`service`]：运行时（事件流、串行上传）与偏好页状态；
//! - [`entries`]：记录与元数据条目；
//! - [`settings`]：设置条目；
//! - [`image`]：图片压缩。
//!
//! 和局域网同步共用设备身份（设备 id 相同）、入库流程与回声过滤。

mod client;
mod crypto;
mod entries;
mod image;
mod pairing;
mod service;
mod settings;
mod state;
#[cfg(test)]
mod tests;

use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::SqlitePool;

use crate::db::models::{ClipboardItem, Platform};
use crate::error::Result;
use crate::root::{Core, CoreInner};
use entries::MetaEntry;
use service::Upload;

pub(crate) use service::RemoteService;

/// 建组或加入时选的服务器。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteServer {
    Official,
    SelfHost { url: String, token: String },
}

/// 偏好页显示的服务器（不含令牌）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteServerView {
    Official,
    SelfHost { url: String },
}

/// 服务器怎么处理图片：不收、只实时转发给在线设备、存储。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteImages {
    Off,
    Live,
    Stored,
}

/// 服务器要求的图片压缩参数。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RemoteImageCompress {
    pub max_side: u32,
    pub quality: u8,
}

/// 服务器下发的策略（`GET /v1/info` 的 `policy`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemotePolicy {
    pub history: bool,
    pub meta: bool,
    pub settings: bool,
    /// 文本保留多久；`None` 为永久。
    pub text_retention_hours: Option<u32>,
    pub images: RemoteImages,
    pub image_max_bytes: u64,
    pub image_compress: Option<RemoteImageCompress>,
    pub max_entry_bytes: u64,
    pub max_devices: u32,
}

impl From<client::PolicyWire> for RemotePolicy {
    fn from(wire: client::PolicyWire) -> Self {
        Self {
            history: wire.history,
            meta: wire.meta,
            settings: wire.settings,
            text_retention_hours: wire.text_retention_hours,
            images: match wire.images.as_str() {
                "live" => RemoteImages::Live,
                "stored" => RemoteImages::Stored,
                _ => RemoteImages::Off,
            },
            image_max_bytes: wire.image_max_bytes,
            image_compress: wire.image_compress.map(|compress| RemoteImageCompress {
                max_side: compress.max_side,
                quality: compress.quality,
            }),
            max_entry_bytes: wire.max_entry_bytes,
            max_devices: wire.max_devices,
        }
    }
}

/// 连接状态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteStatus {
    /// 还没加入同步组。
    NotConfigured,
    /// 加入了，但远程同步关着。
    Paused,
    Connecting,
    Connected,
    /// 连不上，正在退避重连；`message` 是给用户看的原因。
    Offline {
        message: String,
    },
    RateLimited {
        until: DateTime<Utc>,
    },
    /// 本机被移出同步组，凭据已清掉。
    Revoked,
    /// 自部署服务器的令牌不对，不再重试。
    ServerTokenInvalid,
    /// 协议版本不兼容，不再重试；`message` 说明哪边旧。
    Incompatible {
        message: String,
    },
}

/// 同步组里的一台设备。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteDeviceView {
    pub id: String,
    pub name: String,
    pub platform: Platform,
    pub online: bool,
    pub last_seen_at: Option<DateTime<Utc>>,
    pub is_self: bool,
}

/// 本机正在显示的配对码。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemotePairingCode {
    pub code: String,
    pub expires_at: DateTime<Utc>,
}

/// 偏好页的远程同步状态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteSyncState {
    pub server: Option<RemoteServerView>,
    pub status: RemoteStatus,
    pub policy: Option<RemotePolicy>,
    pub devices: Vec<RemoteDeviceView>,
    pub pairing: Option<RemotePairingCode>,
    /// 图片额度用完，到这个时间之前不发图片（文本照常）。
    pub image_quota_until: Option<DateTime<Utc>>,
    pub last_sync_at: Option<DateTime<Utc>>,
    /// 正在上传整个历史：`(已传, 总数)`。
    pub history_upload: Option<(u64, u64)>,
}

impl Core {
    /// 启用远程同步服务：已加入同步组且 `sync.remote.enabled` 时连上服务器，之后随设置启停。
    /// 重复调用只返回当前状态。
    pub async fn start_remote_sync(&self) -> RemoteSyncState {
        let core = self.clone();
        let started = self
            .hop(async move {
                core.0.remote.start(&core.0).await;
                Ok(())
            })
            .await;
        if let Err(err) = started {
            log::warn!("remote sync start failed: {err}");
        }
        self.remote_sync_state()
    }

    pub fn remote_sync_state(&self) -> RemoteSyncState {
        self.0.remote.snapshot(&self.0)
    }

    /// 查一台服务器能不能用（地址、令牌、协议版本），返回它的策略。
    pub async fn check_remote_server(&self, server: RemoteServer) -> Result<RemotePolicy> {
        let core = self.clone();
        self.hop(async move {
            let (_, _, policy) = core.0.remote.check_server(&core.0, server).await?;
            Ok(policy)
        })
        .await
    }

    /// 在这台服务器上新建同步组（本机是第一台设备），并打开远程同步。
    pub async fn create_remote_group(&self, server: RemoteServer) -> Result<()> {
        let core = self.clone();
        self.hop(async move { core.0.remote.create_group(&core.0, server).await })
            .await
    }

    /// 用组里另一台设备显示的配对码加入同步组，并打开远程同步。
    pub async fn join_remote_group(&self, server: RemoteServer, code: String) -> Result<()> {
        let core = self.clone();
        self.hop(async move { core.0.remote.join_group(&core.0, server, &code).await })
            .await
    }

    /// 申请一个配对码给新设备用（5 分钟有效）；本机在线时自动应答用它发起的加入。
    pub async fn request_remote_pairing_code(&self) -> Result<RemotePairingCode> {
        let core = self.clone();
        self.hop(async move { core.0.remote.request_code(&core.0).await })
            .await
    }

    pub async fn cancel_remote_pairing(&self) -> Result<()> {
        let core = self.clone();
        self.hop(async move { core.0.remote.cancel_code(&core.0).await })
            .await
    }

    /// 把一台设备移出同步组；移除本机就是退出。
    pub async fn remove_remote_device(&self, device_id: String) -> Result<()> {
        let core = self.clone();
        self.hop(async move { core.0.remote.remove_device(&core.0, &device_id).await })
            .await
    }

    /// 退出同步组：清掉本机凭据与同步状态，已经收到的记录保留。
    pub async fn leave_remote_group(&self) -> Result<()> {
        let core = self.clone();
        self.hop(async move { core.0.remote.leave(&core.0).await })
            .await
    }
}

/// 用户手动改了一条记录的元数据。
pub(crate) enum MetaChange {
    Favorite(bool),
    Pinned(bool),
    Note(Option<String>),
}

/// 本机这次复制要不要发给同步组（见 [`RemoteService::accepts`]）。
pub(crate) fn accepts_capture(core: &CoreInner, item: &ClipboardItem) -> bool {
    core.remote.accepts(core, item)
}

/// 把本机复制交给上传任务；`skip` 是已经通过局域网直连、不必再经服务器转发的设备。
pub(crate) fn capture(core: &CoreInner, item: &ClipboardItem, skip: Vec<String>) {
    core.remote.capture(item, skip);
}

/// 同步全部历史时上传用户的元数据改动（DB 已经改好之后调用）。失败只记日志，不影响本机操作。
pub(crate) async fn meta_changed(
    core: &CoreInner,
    pool: &SqlitePool,
    id: &str,
    change: MetaChange,
) {
    let (field, value) = match change {
        MetaChange::Favorite(favorite) => (entries::FAVORITE, Value::Bool(favorite)),
        MetaChange::Pinned(pinned) => (entries::PINNED, Value::Bool(pinned)),
        MetaChange::Note(note) => (entries::NOTE, note.map_or(Value::Null, Value::String)),
    };
    let item_key = match crate::db::remote::item_key(pool, id).await {
        Ok(key) => key,
        Err(err) => {
            log::warn!("remote sync metadata skipped: {err}");
            return;
        }
    };
    upload_meta(core, pool, item_key, field, value).await;
}

/// 同步全部历史时上传删除；`item_key` 要在删除之前取。
pub(crate) async fn item_deleted(core: &CoreInner, item_key: Option<String>) {
    let pool = core.db.pool().await;
    upload_meta(core, &pool, item_key, entries::DELETED, Value::Bool(true)).await;
}

async fn upload_meta(
    core: &CoreInner,
    pool: &SqlitePool,
    item_key: Option<String>,
    field: &str,
    value: Value,
) {
    let (Some(item_key), Some(run), Some(policy)) =
        (item_key, core.remote.run(), core.remote.policy())
    else {
        return;
    };
    let remote = core.settings.snapshot().sync.remote;
    if !(remote.enabled && remote.full_history && policy.history && policy.meta) {
        return;
    }
    let entry = MetaEntry {
        item_key,
        field: field.to_owned(),
        value,
    };
    let hlc = core.remote.tick(&run.device_id);
    match entries::local_meta(pool, &run.keys, entry, hlc).await {
        Ok(frame) => {
            if let Err(err) = run.uploads.try_send(Upload::Frame(frame)) {
                log::warn!("remote sync metadata not queued: {err}");
            }
        }
        Err(err) => log::warn!("remote sync metadata skipped: {err:#}"),
    }
}

/// 同步设置变了（或整份替换）：在后台按新设置启停远程同步。
pub(crate) fn settings_changed(core: &CoreInner) {
    let Some(core) = core.remote.bound_core() else {
        return;
    };
    core.rt.clone().spawn(async move {
        core.remote.apply(&core).await;
    });
}

/// 任何设置落盘后调用：运行中的远程同步检查设备名和要上传的设置叶子。
pub(crate) fn settings_updated(core: &CoreInner) {
    if let Some(run) = core.remote.run() {
        run.nudge();
    }
}

/// 覆盖导入备份后调用：游标归零，从头补齐。
pub(crate) async fn after_import(core: &CoreInner) {
    core.remote.after_import().await;
}

pub(crate) async fn shutdown(core: &CoreInner) {
    core.remote.stop().await;
}
