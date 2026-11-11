//! 局域网同步运行时：监听端口、mDNS 发现、拨号、配对与同步会话（协议 v2，见 `PROTOCOL.md`）。
//!
//! 每台设备既监听也主动连接，没有主机角色；同一对设备只保留一条连接
//! （两边同时连上时，保留设备 id 较小一方发起的那条）。连上后先按水位线互相请求补齐，
//! 之后本机每次复制都实时推给所有在线设备。
//!
//! IPv4 与 IPv6 都监听（同一个端口）；链路本地的 IPv6 地址带 scope id。监听范围、端口与要不要
//! mDNS 由 [`LanSyncNetwork`] 决定：正式运行监听所有网卡并广播，自动测试只用本机回环地址、不广播。

use std::collections::{BTreeSet, HashMap};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV6};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock, Weak};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context};
use chrono::{DateTime, Utc};
use serde::Serialize;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, watch, Notify, OwnedSemaphorePermit, Semaphore};

use super::discovery::{Discovery, DiscoveryEvent};
use super::engine::{self, EchoFilter};
use super::identity::{self, device_id_for, DeviceIdentity};
use super::pairing::{self, PairingCode, Role, Transcript};
use super::peers::{PeerStore, TrustedPeer};
use super::protocol::{
    from_hex, incompatibility, to_hex, DeviceInfo, Incompatibility, Intent, Message, RejectReason,
    VersionRange, MAX_HEADER_BYTES, SUPPORTED,
};
use super::transport::{handshake, Handshaken, SecureReader, SecureWriter, HANDSHAKE_TIMEOUT};
use super::{current_platform, default_device_name, effective_device_name};
use crate::db::models::Platform;
use crate::error::{AppError, Result};
use crate::events::CoreEvent;
use crate::i18n::commands::Key;
use crate::root::CoreInner;

/// 默认监听端口，被占用时改用随机端口（mDNS 广播实际端口）。
pub const DEFAULT_PORT: u16 = 41573;

const DIAL_INTERVAL: Duration = Duration::from_secs(15);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(4);
const PING_INTERVAL: Duration = Duration::from_secs(20);
const OUTGOING_QUEUE: usize = 64;
const MAX_PENDING_HANDSHAKES: usize = 16;
const BACKOFF_BASE_SECS: u64 = 5;
const BACKOFF_MAX_SECS: u64 = 120;
const UNPAIR_FLUSH_DELAY: Duration = Duration::from_millis(300);
const LISTEN_BACKLOG: i32 = 128;

type Outgoing = (Message, Arc<Vec<u8>>);

/// 同步服务的网络行为。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LanSyncNetwork {
    /// 首选端口，被占用时改用随机端口；0 表示直接用随机端口。IPv4 与 IPv6 用同一个端口。
    pub port: u16,
    /// 是否在局域网里用 mDNS 广播本机并发现附近设备。
    pub discovery: bool,
    /// 只监听本机回环地址（127.0.0.1 与 ::1），不对局域网开放。
    pub loopback_only: bool,
}

impl Default for LanSyncNetwork {
    /// 正式运行：监听所有网卡（IPv4 与 IPv6）的 41573 端口并用 mDNS 广播。
    fn default() -> Self {
        Self {
            port: DEFAULT_PORT,
            discovery: true,
            loopback_only: false,
        }
    }
}

impl LanSyncNetwork {
    /// 只在本机回环地址上监听、随机端口、不广播，供自动测试与自测使用。
    pub fn loopback() -> Self {
        Self {
            port: 0,
            discovery: false,
            loopback_only: true,
        }
    }

    fn listen_ips(&self) -> (Ipv4Addr, Ipv6Addr) {
        if self.loopback_only {
            (Ipv4Addr::LOCALHOST, Ipv6Addr::LOCALHOST)
        } else {
            (Ipv4Addr::UNSPECIFIED, Ipv6Addr::UNSPECIFIED)
        }
    }

    /// 界面上给用户看的本机地址：回环模式只有回环地址，否则列出可被别的设备连接的地址。
    fn advertised_addresses(&self) -> Vec<String> {
        if self.loopback_only {
            return vec![
                Ipv4Addr::LOCALHOST.to_string(),
                Ipv6Addr::LOCALHOST.to_string(),
            ];
        }

        local_addresses()
    }
}

/// 偏好页展示的状态快照。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LanSyncState {
    pub running: bool,
    pub error: Option<String>,
    pub device_id: Option<String>,
    pub device_name: String,
    pub default_device_name: String,
    pub platform: Platform,
    pub port: Option<u16>,
    /// 本机可被连接的地址：IPv4 在前，链路本地 IPv6 带 `%scope`，可以直接填进对方的「按地址连接」。
    pub addresses: Vec<String>,
    pub pairing_code: Option<String>,
    pub pairing_attempts_left: u8,
    pub devices: Vec<LanDeviceView>,
    pub nearby: Vec<LanNearbyView>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LanDeviceView {
    pub id: String,
    pub name: String,
    pub platform: Platform,
    pub online: bool,
    pub address: Option<String>,
    pub last_seen_at: Option<DateTime<Utc>>,
    pub paired_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LanNearbyView {
    pub id: String,
    pub name: String,
    pub platform: Platform,
    pub address: String,
    /// 双方有共同的协议版本；为假时界面提示升级（哪边旧了在配对时报出）。
    pub compatible: bool,
}

/// 配对目标：附近设备列表里的一台，或手动输入的地址。
pub enum PairTarget {
    Device(String),
    Address(String),
}

/// 由 `Core` 持有。已配对设备启动时就读入（列表显示「来自 xxx」要用）；网络部分要宿主调用
/// [`crate::Core::start_lan_sync`] 后才按设置启动。
pub struct LanSyncService {
    core: OnceLock<Weak<CoreInner>>,
    network: Mutex<Option<LanSyncNetwork>>,
    peers: Arc<PeerStore>,
    lifecycle: tokio::sync::Mutex<Option<Runtime>>,
    shared: RwLock<Option<Arc<Shared>>>,
    error: Mutex<Option<String>>,
    /// 本机申报的协议版本范围；只有测试会改，用来模拟新旧版本。
    supported: Mutex<VersionRange>,
}

struct Runtime {
    shared: Arc<Shared>,
    discovery: Option<Discovery>,
    shutdown: watch::Sender<bool>,
    advertised_name: String,
}

impl LanSyncService {
    pub fn new(peers: Arc<PeerStore>) -> Self {
        Self {
            core: OnceLock::new(),
            network: Mutex::new(None),
            peers,
            lifecycle: tokio::sync::Mutex::new(None),
            shared: RwLock::new(None),
            error: Mutex::new(None),
            supported: Mutex::new(SUPPORTED),
        }
    }

    /// `Core` 建好后登记自己，运行时的会话据此访问设置、数据库与剪贴板。
    pub(crate) fn bind(&self, core: Weak<CoreInner>) {
        let _ = self.core.set(core);
    }

    pub(crate) fn peers(&self) -> &PeerStore {
        &self.peers
    }

    /// 测试用：假装本机支持另一个版本范围，下次启动运行时生效。
    #[cfg(test)]
    pub(crate) fn override_supported(&self, range: VersionRange) {
        *self.supported.lock().expect("lan sync poisoned") = range;
    }

    pub(super) fn shared(&self) -> Option<Arc<Shared>> {
        self.shared.read().expect("lan sync poisoned").clone()
    }

    /// 宿主启用同步服务，之后按设置启停；返回 `false` 表示之前已经启用过。
    pub(super) fn enable(&self, network: LanSyncNetwork) -> bool {
        let mut current = self.network.lock().expect("lan sync poisoned");
        if current.is_some() {
            return false;
        }
        *current = Some(network);
        true
    }

    fn network(&self) -> Option<LanSyncNetwork> {
        self.network.lock().expect("lan sync poisoned").clone()
    }

    /// 宿主启用了同步服务时返回 core，供设置变化后在后台重新应用。
    pub(super) fn bound_core(&self) -> Option<Arc<CoreInner>> {
        self.network()?;
        self.core.get()?.upgrade()
    }

    /// 按当前设置启动、停止或更新运行时；宿主还没启用同步服务时什么都不做。
    pub(super) async fn apply(&self, core: &Arc<CoreInner>) {
        let Some(network) = self.network() else {
            return;
        };
        let lan = core.settings.snapshot().sync.lan;
        let mut lifecycle = self.lifecycle.lock().await;

        if !lan.enabled {
            if let Some(runtime) = lifecycle.take() {
                self.stop_runtime(runtime).await;
            }
            self.set_error(None);
            return;
        }

        if let Some(runtime) = lifecycle.as_mut() {
            let name = effective_device_name(&lan);
            if runtime.advertised_name != name {
                if let Some(discovery) = runtime.discovery.as_mut() {
                    discovery.update_name(&name, current_platform());
                }
                runtime.advertised_name = name;
            }
            return;
        }

        match self.start_runtime(core, &network).await {
            Ok(runtime) => {
                *lifecycle = Some(runtime);
                self.set_error(None);
            }
            Err(err) => {
                log::error!("lan sync start failed: {err:#}");
                self.set_error(Some(format!("{err:#}")));
            }
        }
    }

    async fn start_runtime(
        &self,
        core: &Arc<CoreInner>,
        network: &LanSyncNetwork,
    ) -> anyhow::Result<Runtime> {
        let dir = core.paths.sync_dir();
        let loaded = identity::load_or_create(&dir)?;
        if loaded.regenerated {
            self.peers.forget_all();
        }

        let (listeners, port) = bind_listeners(network)?;
        let supported = *self.supported.lock().expect("lan sync poisoned");
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let (capture_tx, capture_rx) = mpsc::channel(OUTGOING_QUEUE);
        let shared = Arc::new(Shared {
            core: Arc::downgrade(core),
            app_version: core.info.version.to_string(),
            supported,
            identity: loaded.identity,
            peers: self.peers.clone(),
            port,
            connections: Mutex::new(HashMap::new()),
            nearby: Mutex::new(HashMap::new()),
            pairing: Mutex::new(PairingCode::new()),
            echo: Mutex::new(EchoFilter::default()),
            dials: Mutex::new(HashMap::new()),
            dial_now: Notify::new(),
            handshakes: Arc::new(Semaphore::new(MAX_PENDING_HANDSHAKES)),
            shutdown: shutdown_rx,
            next_conn_id: AtomicU64::new(1),
            captures: capture_tx,
        });

        let sender_shared = shared.clone();
        tokio::spawn(async move {
            let mut shutdown = sender_shared.shutdown.clone();
            tokio::select! {
                _ = super::send_captures(sender_shared, capture_rx) => {}
                _ = shutdown.changed() => {}
            }
        });

        for listener in listeners {
            tokio::spawn(accept_loop(shared.clone(), listener));
        }
        tokio::spawn(dial_loop(shared.clone()));

        let lan = core.settings.snapshot().sync.lan;
        let name = effective_device_name(&lan);
        let weak = Arc::downgrade(&shared);
        // mDNS 起不来（如端口被系统服务独占）不影响手动输入地址配对和已知地址重连。
        let discovery = if network.discovery {
            match Discovery::start(
                shared.identity.device_id(),
                &name,
                current_platform(),
                supported,
                port,
                move |event| {
                    if let Some(shared) = weak.upgrade() {
                        shared.on_discovery(event);
                    }
                },
            ) {
                Ok(discovery) => Some(discovery),
                Err(err) => {
                    log::warn!("lan sync discovery unavailable: {err:#}");
                    None
                }
            }
        } else {
            None
        };

        *self.shared.write().expect("lan sync poisoned") = Some(shared.clone());
        log::info!(
            "lan sync started as {} on port {port}",
            shared.identity.device_id()
        );

        Ok(Runtime {
            shared,
            discovery,
            shutdown: shutdown_tx,
            advertised_name: name,
        })
    }

    /// 关掉运行时（core 关闭时调用）；之后设置变化不再启动它。
    pub(super) async fn stop(&self) {
        *self.network.lock().expect("lan sync poisoned") = None;
        if let Some(runtime) = self.lifecycle.lock().await.take() {
            self.stop_runtime(runtime).await;
        }
    }

    async fn stop_runtime(&self, runtime: Runtime) {
        *self.shared.write().expect("lan sync poisoned") = None;
        let _ = runtime.shutdown.send(true);
        runtime.shared.kill_all();
        if let Some(discovery) = runtime.discovery {
            let _ = tokio::task::spawn_blocking(move || discovery.stop()).await;
        }
        log::info!("lan sync stopped");
    }

    fn set_error(&self, error: Option<String>) {
        *self.error.lock().expect("lan sync poisoned") = error;
    }

    pub(super) fn snapshot(&self, core: &CoreInner) -> LanSyncState {
        let lan = core.settings.snapshot().sync.lan;
        let shared = self.shared();
        let error = self.error.lock().expect("lan sync poisoned").clone();

        let devices = self
            .peers
            .list()
            .into_iter()
            .map(|peer| LanDeviceView {
                online: shared
                    .as_ref()
                    .is_some_and(|shared| shared.is_connected(&peer.device_id)),
                id: peer.device_id,
                name: peer.name,
                platform: peer.platform,
                address: peer.address,
                last_seen_at: peer.last_seen_at,
                paired_at: peer.paired_at,
            })
            .collect();

        let (pairing_code, pairing_attempts_left, nearby) = match &shared {
            Some(shared) => {
                let pairing = shared.pairing.lock().expect("lan sync poisoned");
                let code = pairing.code().map(str::to_owned);
                let attempts = pairing.attempts_left();
                drop(pairing);
                (code, attempts, shared.nearby_views())
            }
            None => (None, 0, Vec::new()),
        };

        LanSyncState {
            running: shared.is_some(),
            error,
            device_id: shared
                .as_ref()
                .map(|shared| shared.identity.device_id().to_owned()),
            device_name: effective_device_name(&lan),
            default_device_name: default_device_name(),
            platform: current_platform(),
            port: shared.as_ref().map(|shared| shared.port),
            addresses: self
                .network()
                .map(|network| network.advertised_addresses())
                .unwrap_or_default(),
            pairing_code,
            pairing_attempts_left,
            devices,
            nearby,
        }
    }

    pub(super) fn refresh_pairing_code(&self, core: &CoreInner) -> Result<()> {
        let shared = self.require_running(core)?;
        *shared.pairing.lock().expect("lan sync poisoned") = PairingCode::new();
        Ok(())
    }

    /// 用对方显示的配对码配对，返回对方设备名。
    pub(super) async fn pair(
        &self,
        core: &CoreInner,
        target: PairTarget,
        code: &str,
    ) -> Result<String> {
        let shared = self.require_running(core)?;
        let Some(code) = pairing::normalize_code(code) else {
            return Err(sync_error(core, Key::SyncInvalidCode));
        };

        let addresses = match target {
            PairTarget::Device(device_id) => shared
                .nearby_addresses(&device_id)
                .ok_or_else(|| sync_error(core, Key::SyncDeviceNotFound))?,
            PairTarget::Address(text) => resolve_address(&text)
                .await
                .ok_or_else(|| sync_error(core, Key::SyncInvalidAddress))?,
        };

        let goal = DialGoal::Pair { code };
        for addr in addresses {
            let key = match dial(&shared, addr, &goal).await {
                Ok(name) => {
                    shared.emit_state();
                    return Ok(name);
                }
                Err(DialError::Unreachable(err)) => {
                    log::info!("lan sync pairing could not reach {addr}: {err:#}");
                    continue;
                }
                Err(DialError::Rejected(RejectReason::NotPaired)) => continue,
                Err(DialError::Other(err)) => {
                    log::info!("lan sync pairing with {addr} failed: {err:#}");
                    continue;
                }
                Err(err) => err.key(),
            };
            return Err(sync_error(core, key));
        }

        Err(sync_error(core, Key::SyncUnreachable))
    }

    /// 按地址连接一台已配对的设备（mDNS 不通、记着的地址也换了时用），返回对方设备名。
    pub(super) async fn connect(&self, core: &CoreInner, address: &str) -> Result<String> {
        let shared = self.require_running(core)?;
        let addresses = resolve_address(address)
            .await
            .ok_or_else(|| sync_error(core, Key::SyncInvalidAddress))?;

        for addr in addresses {
            match dial(&shared, addr, &DialGoal::SyncAny).await {
                Ok(name) => {
                    shared.emit_state();
                    return Ok(name);
                }
                Err(DialError::Unreachable(err)) | Err(DialError::Other(err)) => {
                    log::info!("lan sync connect to {addr} failed: {err:#}");
                }
                Err(err) => return Err(sync_error(core, err.key())),
            }
        }

        Err(sync_error(core, Key::SyncUnreachable))
    }

    /// 取消配对：先从本机移除（避免断开后又自动重连），在线时再通知对方。
    pub(super) async fn remove_device(&self, device_id: &str) {
        let Some(shared) = self.shared() else {
            self.peers.remove(device_id);
            return;
        };
        let Some((tx, kill)) = shared.revoke(device_id) else {
            return;
        };

        let unpair = (Message::Unpair, Arc::new(Vec::new()));
        let _ = tokio::time::timeout(Duration::from_secs(1), tx.send(unpair)).await;
        tokio::time::sleep(UNPAIR_FLUSH_DELAY).await;
        let _ = kill.send(true);
    }

    fn require_running(&self, core: &CoreInner) -> Result<Arc<Shared>> {
        self.shared()
            .ok_or_else(|| sync_error(core, Key::SyncNotRunning))
    }
}

fn sync_error(core: &CoreInner, key: Key) -> AppError {
    AppError::Sync(crate::i18n::commands::label(core.language(), key).to_owned())
}

struct Connection {
    id: u64,
    dialer_is_me: bool,
    tx: mpsc::Sender<Outgoing>,
    kill: Arc<watch::Sender<bool>>,
    /// 对方申报的附件上限，实时推送按它跳过放不下的图片。
    max_attachment: u64,
    live: LiveCursor,
}

/// 实时推送的检查点。本机序号是连续发出的，每个序号对应一次本机采集；补齐的最后一页发出后，
/// 对方已收齐到 `confirmed`，之后每个序号要么推送了、要么按规则不需要推送（敏感、文件、超限），
/// 从 `confirmed` 起连续处理过的部分就可以作为检查点发给对方推进水位线，重连时不必再补发。
#[derive(Default)]
struct LiveCursor {
    /// `(代次, 序号)`：最后一页补齐发出后才知道。
    confirmed: Option<(String, u64)>,
    /// 已经处理过、还没连上 `confirmed` 的序号。
    handled: BTreeSet<u64>,
    /// 有记录因为发送队列满被丢掉（或读图失败）：这次连接不再发检查点，留给下次补齐。
    broken: bool,
}

impl LiveCursor {
    /// 记下一个处理过的序号；能推进时返回检查点。
    fn handle(&mut self, seq: u64) -> Option<Message> {
        self.handled.insert(seq);
        self.advance()
    }

    /// 补齐的最后一页已经发出：对方收齐到 `until`。之后连续处理过的序号随即作为检查点发出。
    fn confirm(&mut self, epoch: String, until: u64) -> Option<Message> {
        match &mut self.confirmed {
            Some((current, confirmed)) if *current == epoch => *confirmed = (*confirmed).max(until),
            _ => self.confirmed = Some((epoch, until)),
        }
        self.advance()
    }

    fn advance(&mut self) -> Option<Message> {
        if self.broken {
            return None;
        }
        let (epoch, until) = self.confirmed.as_mut()?;
        self.handled = self.handled.split_off(&(*until + 1));
        let start = *until;
        while self.handled.remove(&(*until + 1)) {
            *until += 1;
        }

        (*until > start).then(|| Message::CatchUpPage {
            until: *until,
            more: false,
            epoch: epoch.clone(),
        })
    }
}

struct Nearby {
    name: String,
    platform: Platform,
    protocol: VersionRange,
    addresses: Vec<SocketAddr>,
}

#[derive(Default)]
struct DialState {
    dialing: bool,
    failures: u32,
    next_at: Option<Instant>,
}

pub(super) struct Shared {
    /// 弱引用：会话任务不该让已经关闭的 core 一直活着。
    core: Weak<CoreInner>,
    app_version: String,
    supported: VersionRange,
    identity: DeviceIdentity,
    peers: Arc<PeerStore>,
    port: u16,
    connections: Mutex<HashMap<String, Connection>>,
    nearby: Mutex<HashMap<String, Nearby>>,
    pairing: Mutex<PairingCode>,
    echo: Mutex<EchoFilter>,
    dials: Mutex<HashMap<String, DialState>>,
    dial_now: Notify,
    handshakes: Arc<Semaphore>,
    shutdown: watch::Receiver<bool>,
    next_conn_id: AtomicU64,
    pub(super) captures: mpsc::Sender<super::LocalCapture>,
}

impl Shared {
    pub(super) fn core(&self) -> Option<Arc<CoreInner>> {
        self.core.upgrade()
    }

    /// 本机愿意接收的附件上限：跟随设置里的图片大小上限；不收图片时为 0。
    fn max_attachment(&self) -> u64 {
        self.core().map_or(0, |core| {
            let lan = core.settings.snapshot().sync.lan;
            if lan.image {
                lan.max_image_bytes()
            } else {
                0
            }
        })
    }

    fn my_info(&self) -> DeviceInfo {
        let name = self
            .core()
            .map(|core| effective_device_name(&core.settings.snapshot().sync.lan))
            .unwrap_or_else(default_device_name);
        DeviceInfo {
            name,
            platform: current_platform(),
            app_version: self.app_version.clone(),
            protocol: self.supported,
            listen_port: self.port,
            max_attachment_bytes: self.max_attachment(),
        }
    }

    fn emit(&self, event: CoreEvent) {
        if let Some(core) = self.core() {
            core.events.emit(event);
        }
    }

    fn emit_state(&self) {
        self.emit(CoreEvent::LanSyncChanged);
    }

    pub(super) fn has_connections(&self) -> bool {
        !self
            .connections
            .lock()
            .expect("lan sync poisoned")
            .is_empty()
    }

    fn is_connected(&self, device_id: &str) -> bool {
        self.connections
            .lock()
            .expect("lan sync poisoned")
            .contains_key(device_id)
    }

    pub(super) fn should_send(&self, content_hash: &str) -> bool {
        self.echo
            .lock()
            .expect("lan sync poisoned")
            .should_send(content_hash)
    }

    /// 处理一次本机采集：`push` 推给所有在线设备（附件超过对方上限的跳过；某台设备的发送队列满了
    /// 就丢掉这一条，它重连后会补齐），`seq` 是这次采集的本机序号，记为已处理并按需发检查点。
    /// 不需要推送的采集（敏感、文件、超限）也要调用，`push` 传 `None`。
    pub(super) fn deliver(&self, seq: Option<u64>, push: Option<(Message, Arc<Vec<u8>>)>) {
        let mut connections = self.connections.lock().expect("lan sync poisoned");
        for (peer_id, connection) in connections.iter_mut() {
            if let Some((message, attachment)) = &push {
                if attachment.len() as u64 > connection.max_attachment {
                    log::info!("lan sync skips an attachment over the limit of {peer_id}");
                } else if connection
                    .tx
                    .try_send((message.clone(), attachment.clone()))
                    .is_err()
                {
                    log::warn!("lan sync queue to {peer_id} is full, dropping an item");
                    connection.live.broken = true;
                }
            }
            if let Some(checkpoint) = seq.and_then(|seq| connection.live.handle(seq)) {
                let _ = connection.tx.try_send((checkpoint, Arc::new(Vec::new())));
            }
        }
    }

    /// 本机这次采集的记录没法转成线上格式（读图失败）：这次连接不再发检查点，重连时补齐。
    pub(super) fn deliver_failed(&self) {
        let mut connections = self.connections.lock().expect("lan sync poisoned");
        for connection in connections.values_mut() {
            connection.live.broken = true;
        }
    }

    /// 补齐的最后一页已经排进 `connection_id` 的发送队列：记下对方收齐到哪里。
    fn confirm_catch_up(&self, peer_id: &str, connection_id: u64, epoch: String, until: u64) {
        let mut connections = self.connections.lock().expect("lan sync poisoned");
        let Some(connection) = connections
            .get_mut(peer_id)
            .filter(|connection| connection.id == connection_id)
        else {
            return;
        };
        if let Some(checkpoint) = connection.live.confirm(epoch, until) {
            let _ = connection.tx.try_send((checkpoint, Arc::new(Vec::new())));
        }
    }

    /// 登记一条已认证的连接。同一台设备已有连接时，保留设备 id 较小一方发起的那条；
    /// 发起方相同（旧连接还没发现断开）时新的替换旧的。
    fn register(&self, peer_id: &str, public_key: &str, connection: Connection) -> bool {
        let preferred_dialer_is_me = self.identity.device_id() < peer_id;
        let mut connections = self.connections.lock().expect("lan sync poisoned");
        if !self.peers.is_trusted(peer_id, public_key) {
            return false;
        }
        if let Some(existing) = connections.get(peer_id) {
            let existing_preferred = existing.dialer_is_me == preferred_dialer_is_me;
            let new_preferred = connection.dialer_is_me == preferred_dialer_is_me;
            if existing_preferred && !new_preferred {
                return false;
            }
            let _ = existing.kill.send(true);
        }
        connections.insert(peer_id.to_owned(), connection);
        true
    }

    fn unregister(&self, peer_id: &str, connection_id: u64) {
        let mut connections = self.connections.lock().expect("lan sync poisoned");
        if connections
            .get(peer_id)
            .is_some_and(|connection| connection.id == connection_id)
        {
            connections.remove(peer_id);
        }
    }

    /// 移除信任与登记连接共用一把锁，握手期间取消配对也不能再登记会话。
    fn revoke(&self, peer_id: &str) -> Option<(mpsc::Sender<Outgoing>, Arc<watch::Sender<bool>>)> {
        let connections = self.connections.lock().expect("lan sync poisoned");
        self.peers.remove(peer_id);
        connections
            .get(peer_id)
            .map(|connection| (connection.tx.clone(), connection.kill.clone()))
    }

    fn kill_all(&self) {
        let connections = std::mem::take(&mut *self.connections.lock().expect("lan sync poisoned"));
        for connection in connections.into_values() {
            let _ = connection.kill.send(true);
        }
    }

    fn on_discovery(&self, event: DiscoveryEvent) {
        let changed = match event {
            DiscoveryEvent::Found(mut found) => {
                rank_addresses(&mut found.addresses, &local_ipv4s());
                let mut nearby = self.nearby.lock().expect("lan sync poisoned");
                let changed = nearby.get(&found.device_id).is_none_or(|existing| {
                    existing.name != found.name
                        || existing.addresses != found.addresses
                        || existing.protocol != found.protocol
                });
                nearby.insert(
                    found.device_id.clone(),
                    Nearby {
                        name: found.name,
                        platform: found.platform,
                        protocol: found.protocol,
                        addresses: found.addresses,
                    },
                );
                drop(nearby);

                if self.peers.get(&found.device_id).is_some()
                    && !self.is_connected(&found.device_id)
                {
                    self.reset_backoff(&found.device_id);
                    self.dial_now.notify_one();
                }
                changed
            }
            DiscoveryEvent::Lost(device_id) => self
                .nearby
                .lock()
                .expect("lan sync poisoned")
                .remove(&device_id)
                .is_some(),
        };

        if changed {
            self.emit_state();
        }
    }

    fn nearby_views(&self) -> Vec<LanNearbyView> {
        let nearby = self.nearby.lock().expect("lan sync poisoned");
        let mut views: Vec<LanNearbyView> = nearby
            .iter()
            .filter(|(id, _)| self.peers.get(id).is_none())
            .filter_map(|(id, device)| {
                Some(LanNearbyView {
                    id: id.clone(),
                    name: device.name.clone(),
                    platform: device.platform,
                    address: device.addresses.first()?.to_string(),
                    compatible: self.supported.negotiate(device.protocol).is_some(),
                })
            })
            .collect();
        views.sort_by(|a, b| a.name.cmp(&b.name));
        views
    }

    fn nearby_addresses(&self, device_id: &str) -> Option<Vec<SocketAddr>> {
        self.nearby
            .lock()
            .expect("lan sync poisoned")
            .get(device_id)
            .map(|device| device.addresses.clone())
    }

    /// 取出这一轮要拨的设备和候选地址，并标记为拨号中。
    fn dial_candidates(&self) -> Vec<(TrustedPeer, Vec<SocketAddr>)> {
        let peers = self.peers.list();
        let connections = self.connections.lock().expect("lan sync poisoned");
        let nearby = self.nearby.lock().expect("lan sync poisoned");
        let mut dials = self.dials.lock().expect("lan sync poisoned");
        let now = Instant::now();

        peers
            .into_iter()
            .filter_map(|peer| {
                if connections.contains_key(&peer.device_id) {
                    return None;
                }
                let state = dials.entry(peer.device_id.clone()).or_default();
                if state.dialing || state.next_at.is_some_and(|at| at > now) {
                    return None;
                }

                let mut addresses = nearby
                    .get(&peer.device_id)
                    .map(|device| device.addresses.clone())
                    .unwrap_or_default();
                if let Some(address) = peer
                    .address
                    .as_deref()
                    .and_then(|text| text.parse::<SocketAddr>().ok())
                {
                    if !addresses.contains(&address) {
                        addresses.push(address);
                    }
                }
                if addresses.is_empty() {
                    return None;
                }

                state.dialing = true;
                Some((peer, addresses))
            })
            .collect()
    }

    fn finish_dial(&self, device_id: &str, connected: bool) {
        let mut dials = self.dials.lock().expect("lan sync poisoned");
        let state = dials.entry(device_id.to_owned()).or_default();
        state.dialing = false;
        if connected {
            state.failures = 0;
            state.next_at = None;
            return;
        }

        state.failures = state.failures.saturating_add(1);
        let shift = state.failures.saturating_sub(1).min(5);
        let secs = (BACKOFF_BASE_SECS << shift).min(BACKOFF_MAX_SECS);
        state.next_at = Some(Instant::now() + Duration::from_secs(secs));
    }

    fn reset_backoff(&self, device_id: &str) {
        if let Some(state) = self
            .dials
            .lock()
            .expect("lan sync poisoned")
            .get_mut(device_id)
        {
            state.failures = 0;
            state.next_at = None;
        }
    }
}

/// IPv4 与 IPv6 各一个监听，用同一个端口。IPv6 显式设为只收 IPv6（不同系统默认值不同，
/// macOS 默认双栈会和 IPv4 的监听冲突）；IPv6 起不来（系统关了 IPv6）时只用 IPv4。
fn bind_listeners(network: &LanSyncNetwork) -> anyhow::Result<(Vec<TcpListener>, u16)> {
    let (v4, v6) = network.listen_ips();
    let ipv4 = match bind_socket(SocketAddr::new(IpAddr::V4(v4), network.port)) {
        Ok(listener) => listener,
        Err(err) if network.port != 0 => {
            log::info!(
                "lan sync port {} unavailable ({err:#}), using a random port",
                network.port
            );
            bind_socket(SocketAddr::new(IpAddr::V4(v4), 0))?
        }
        Err(err) => return Err(err),
    };
    let port = ipv4.local_addr()?.port();

    let mut listeners = vec![ipv4];
    match bind_socket(SocketAddr::new(IpAddr::V6(v6), port)) {
        Ok(listener) => listeners.push(listener),
        Err(err) => log::info!("lan sync listens on IPv4 only: {err:#}"),
    }
    Ok((listeners, port))
}

fn bind_socket(addr: SocketAddr) -> anyhow::Result<TcpListener> {
    use socket2::{Domain, Protocol, Socket, Type};

    let socket = Socket::new(Domain::for_address(addr), Type::STREAM, Some(Protocol::TCP))
        .context("failed to create listening socket")?;
    if addr.is_ipv6() {
        socket
            .set_only_v6(true)
            .context("failed to restrict the socket to IPv6")?;
    }
    socket
        .bind(&addr.into())
        .with_context(|| format!("failed to bind {addr}"))?;
    socket
        .listen(LISTEN_BACKLOG)
        .with_context(|| format!("failed to listen on {addr}"))?;
    socket
        .set_nonblocking(true)
        .context("failed to make the socket non-blocking")?;
    TcpListener::from_std(socket.into()).context("failed to register the listener")
}

async fn accept_loop(shared: Arc<Shared>, listener: TcpListener) {
    let mut shutdown = shared.shutdown.clone();
    loop {
        tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok((stream, addr)) => {
                    let Ok(permit) = shared.handshakes.clone().try_acquire_owned() else {
                        log::warn!("lan sync drops {addr}: too many pending handshakes");
                        continue;
                    };
                    let shared = shared.clone();
                    tokio::spawn(async move {
                        if let Err(err) = handle_incoming(shared, stream, addr, permit).await {
                            log::info!("lan sync incoming connection from {addr} ended: {err:#}");
                        }
                    });
                }
                Err(err) => {
                    log::warn!("lan sync accept failed: {err}");
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
            },
            _ = shutdown.changed() => break,
        }
    }
}

async fn handle_incoming(
    shared: Arc<Shared>,
    stream: TcpStream,
    addr: SocketAddr,
    permit: OwnedSemaphorePermit,
) -> anyhow::Result<()> {
    let Handshaken {
        mut reader,
        mut writer,
        remote_public_key,
        handshake_hash,
    } = handshake(stream, &shared.identity, false).await?;
    let remote_id = device_id_for(&remote_public_key);
    if remote_id == shared.identity.device_id() {
        return Err(anyhow!("connected to itself"));
    }

    let (message, _) = recv_within(&mut reader).await?;
    let Message::Hello { device, intent } = message else {
        return Err(anyhow!("expected hello"));
    };
    let Some(version) = shared.supported.negotiate(device.protocol) else {
        reject(
            &mut writer,
            RejectReason::Incompatible,
            Some(shared.supported),
        )
        .await;
        return Err(anyhow!(
            "no common protocol version with {:?}",
            device.protocol
        ));
    };

    match intent {
        Intent::Sync => {
            if !shared
                .peers
                .is_trusted(&remote_id, &to_hex(&remote_public_key))
            {
                reject(&mut writer, RejectReason::NotPaired, None).await;
                return Err(anyhow!("unpaired device {remote_id} asked to sync"));
            }
            writer
                .send_message(
                    &Message::Welcome {
                        device: shared.my_info(),
                        protocol: version,
                    },
                    &[],
                )
                .await?;
        }
        Intent::Pair { spake } => {
            let request = PairRequest {
                remote_id: &remote_id,
                remote_key: &remote_public_key,
                handshake_hash: &handshake_hash,
                device: &device,
                spake: &spake,
                addr,
                version,
            };
            accept_pairing(&shared, &mut reader, &mut writer, request).await?;
        }
        Intent::Unknown => {
            reject(
                &mut writer,
                RejectReason::Incompatible,
                Some(shared.supported),
            )
            .await;
            return Err(anyhow!("unknown intent from {remote_id}"));
        }
    }

    drop(permit);
    run_session(
        shared,
        remote_public_key,
        device,
        reader,
        writer,
        false,
        addr,
    )
    .await;
    Ok(())
}

struct PairRequest<'a> {
    remote_id: &'a str,
    remote_key: &'a [u8; 32],
    handshake_hash: &'a [u8],
    device: &'a DeviceInfo,
    spake: &'a str,
    addr: SocketAddr,
    version: u16,
}

/// 显示配对码的一方：校验对方的确认值，通过后记为已配对设备并换一个新配对码。
async fn accept_pairing(
    shared: &Arc<Shared>,
    reader: &mut SecureReader,
    writer: &mut SecureWriter,
    request: PairRequest<'_>,
) -> anyhow::Result<()> {
    let code = shared
        .pairing
        .lock()
        .expect("lan sync poisoned")
        .begin_attempt();
    shared.emit_state();
    let Some(code) = code else {
        reject(writer, RejectReason::PairingUnavailable, None).await;
        return Err(anyhow!("pairing attempts used up"));
    };

    let (spake, outbound) = pairing::start_listener(&code);
    let Ok(key) = spake.finish(&from_hex(request.spake)?) else {
        reject(writer, RejectReason::WrongCode, None).await;
        return Err(anyhow!("invalid spake message"));
    };
    writer
        .send_message(
            &Message::PairChallenge {
                device: shared.my_info(),
                protocol: request.version,
                spake: to_hex(&outbound),
            },
            &[],
        )
        .await?;

    let (message, _) = recv_within(reader).await?;
    let Message::PairConfirm { mac } = message else {
        return Err(anyhow!("expected pairing confirmation"));
    };
    let transcript = Transcript {
        handshake_hash: request.handshake_hash,
        dialer_key: request.remote_key,
        listener_key: shared.identity.public_key(),
    };
    let expected = pairing::confirmation(&key, Role::Dialer, &transcript);
    if !pairing::verify_confirmation(&expected, &mac) {
        reject(writer, RejectReason::WrongCode, None).await;
        return Err(anyhow!("wrong pairing code from {}", request.addr));
    }

    let ours = pairing::confirmation(&key, Role::Listener, &transcript);
    writer
        .send_message(
            &Message::PairDone {
                mac: to_hex(ours.as_bytes()),
            },
            &[],
        )
        .await?;

    let now = Utc::now();
    shared.peers.add(TrustedPeer {
        device_id: request.remote_id.to_owned(),
        public_key: to_hex(request.remote_key),
        name: request.device.name.clone(),
        platform: request.device.platform,
        address: Some(with_port(request.addr, request.device.listen_port).to_string()),
        paired_at: now,
        last_seen_at: Some(now),
        watermark: 0,
        epoch: None,
    });
    *shared.pairing.lock().expect("lan sync poisoned") = PairingCode::new();
    log::info!(
        "lan sync paired with {} ({})",
        request.device.name,
        request.remote_id
    );
    shared.emit(CoreEvent::LanDevicePaired {
        name: request.device.name.clone(),
    });
    Ok(())
}

enum DialGoal {
    /// 连一台指定的已配对设备。
    Sync {
        device_id: String,
    },
    /// 按地址连接，对方是任何一台已配对设备都行。
    SyncAny,
    Pair {
        code: String,
    },
}

#[derive(Debug)]
enum DialError {
    /// 连不上或握手失败，可以换下一个地址再试。
    Unreachable(anyhow::Error),
    SelfConnection,
    WrongCode,
    /// 按地址连接时，对方不是已配对设备。
    NotPaired,
    Rejected(RejectReason),
    /// 没有共同的协议版本；`None` 是对方没说明自己支持哪些版本。
    Incompatible(Option<Incompatibility>),
    Other(anyhow::Error),
}

impl DialError {
    /// 给用户看的原因。
    fn key(&self) -> Key {
        match self {
            Self::SelfConnection => Key::SyncSelfPairing,
            Self::WrongCode | Self::Rejected(RejectReason::WrongCode) => Key::SyncWrongCode,
            Self::Rejected(RejectReason::PairingUnavailable) => Key::SyncPairingUnavailable,
            Self::NotPaired | Self::Rejected(RejectReason::NotPaired) => Key::SyncNotPaired,
            Self::Incompatible(Some(Incompatibility::PeerOutdated)) => Key::SyncPeerOutdated,
            Self::Incompatible(Some(Incompatibility::SelfOutdated)) => Key::SyncSelfOutdated,
            Self::Incompatible(None) | Self::Rejected(RejectReason::Incompatible) => {
                Key::SyncIncompatible
            }
            Self::Rejected(RejectReason::Unknown) | Self::Unreachable(_) | Self::Other(_) => {
                Key::SyncUnreachable
            }
        }
    }
}

/// 对方拒绝时的错误：版本不兼容要分清是哪边旧了。
fn rejection(shared: &Shared, reason: RejectReason, supported: Option<VersionRange>) -> DialError {
    if reason == RejectReason::Incompatible {
        return DialError::Incompatible(
            supported.and_then(|theirs| incompatibility(shared.supported, theirs)),
        );
    }
    DialError::Rejected(reason)
}

/// 主动连接 `addr`；成功时已在后台开始同步会话，返回对方设备名。
async fn dial(
    shared: &Arc<Shared>,
    addr: SocketAddr,
    goal: &DialGoal,
) -> std::result::Result<String, DialError> {
    let stream = tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(addr))
        .await
        .map_err(|_| DialError::Unreachable(anyhow!("connect timed out")))?
        .map_err(|err| DialError::Unreachable(err.into()))?;
    let Handshaken {
        mut reader,
        mut writer,
        remote_public_key,
        handshake_hash,
    } = handshake(stream, &shared.identity, true)
        .await
        .map_err(DialError::Unreachable)?;
    let remote_id = device_id_for(&remote_public_key);
    if remote_id == shared.identity.device_id() {
        return Err(DialError::SelfConnection);
    }

    let device = match goal {
        DialGoal::Sync { .. } | DialGoal::SyncAny => {
            let trusted = shared
                .peers
                .is_trusted(&remote_id, &to_hex(&remote_public_key));
            match goal {
                DialGoal::Sync { device_id } if &remote_id != device_id || !trusted => {
                    return Err(DialError::Other(anyhow!(
                        "{addr} now belongs to another device"
                    )));
                }
                DialGoal::SyncAny if !trusted => return Err(DialError::NotPaired),
                _ => {}
            }

            let hello = Message::Hello {
                device: shared.my_info(),
                intent: Intent::Sync,
            };
            writer
                .send_message(&hello, &[])
                .await
                .map_err(DialError::Unreachable)?;
            match recv_within(&mut reader)
                .await
                .map_err(DialError::Unreachable)?
                .0
            {
                Message::Welcome { device, protocol } if shared.supported.contains(protocol) => {
                    device
                }
                Message::Welcome { protocol, .. } => {
                    return Err(DialError::Other(anyhow!(
                        "peer chose unsupported protocol {protocol}"
                    )));
                }
                Message::Reject { reason, supported } => {
                    return Err(rejection(shared, reason, supported));
                }
                other => return Err(DialError::Other(anyhow!("unexpected reply {other:?}"))),
            }
        }
        DialGoal::Pair { code } => {
            let device = pair_as_dialer(
                shared,
                &mut reader,
                &mut writer,
                code,
                &remote_public_key,
                &handshake_hash,
            )
            .await?;
            let now = Utc::now();
            shared.peers.add(TrustedPeer {
                device_id: remote_id.clone(),
                public_key: to_hex(&remote_public_key),
                name: device.name.clone(),
                platform: device.platform,
                address: Some(with_port(addr, device.listen_port).to_string()),
                paired_at: now,
                last_seen_at: Some(now),
                watermark: 0,
                epoch: None,
            });
            log::info!("lan sync paired with {} ({remote_id})", device.name);
            device
        }
    };

    let name = device.name.clone();
    let shared = shared.clone();
    tokio::spawn(async move {
        run_session(
            shared,
            remote_public_key,
            device,
            reader,
            writer,
            true,
            addr,
        )
        .await;
    });
    Ok(name)
}

/// 输入配对码的一方：发 SPAKE2 首条消息，先交出自己的确认值，再校验对方的。
async fn pair_as_dialer(
    shared: &Arc<Shared>,
    reader: &mut SecureReader,
    writer: &mut SecureWriter,
    code: &str,
    remote_key: &[u8; 32],
    handshake_hash: &[u8],
) -> std::result::Result<DeviceInfo, DialError> {
    let (spake, outbound) = pairing::start_dialer(code);
    let hello = Message::Hello {
        device: shared.my_info(),
        intent: Intent::Pair {
            spake: to_hex(&outbound),
        },
    };
    writer
        .send_message(&hello, &[])
        .await
        .map_err(DialError::Unreachable)?;

    let (device, spake_hex) = match recv_within(reader).await.map_err(DialError::Unreachable)?.0 {
        Message::PairChallenge {
            device,
            protocol,
            spake,
        } if shared.supported.contains(protocol) => (device, spake),
        Message::PairChallenge { protocol, .. } => {
            return Err(DialError::Other(anyhow!(
                "peer chose unsupported protocol {protocol}"
            )));
        }
        Message::Reject { reason, supported } => return Err(rejection(shared, reason, supported)),
        other => return Err(DialError::Other(anyhow!("unexpected reply {other:?}"))),
    };
    let inbound = from_hex(&spake_hex).map_err(DialError::Other)?;
    let key = spake.finish(&inbound).map_err(|_| DialError::WrongCode)?;

    let transcript = Transcript {
        handshake_hash,
        dialer_key: shared.identity.public_key(),
        listener_key: remote_key,
    };
    let ours = pairing::confirmation(&key, Role::Dialer, &transcript);
    writer
        .send_message(
            &Message::PairConfirm {
                mac: to_hex(ours.as_bytes()),
            },
            &[],
        )
        .await
        .map_err(DialError::Unreachable)?;

    match recv_within(reader).await.map_err(DialError::Unreachable)?.0 {
        Message::PairDone { mac } => {
            let expected = pairing::confirmation(&key, Role::Listener, &transcript);
            if pairing::verify_confirmation(&expected, &mac) {
                Ok(device)
            } else {
                Err(DialError::WrongCode)
            }
        }
        Message::Reject { reason, supported } => Err(rejection(shared, reason, supported)),
        other => Err(DialError::Other(anyhow!("unexpected reply {other:?}"))),
    }
}

async fn dial_loop(shared: Arc<Shared>) {
    let mut shutdown = shared.shutdown.clone();
    loop {
        for (peer, addresses) in shared.dial_candidates() {
            let shared = shared.clone();
            tokio::spawn(async move {
                dial_peer(shared, peer, addresses).await;
            });
        }

        tokio::select! {
            _ = tokio::time::sleep(DIAL_INTERVAL) => {}
            _ = shared.dial_now.notified() => {}
            _ = shutdown.changed() => return,
        }
    }
}

async fn dial_peer(shared: Arc<Shared>, peer: TrustedPeer, addresses: Vec<SocketAddr>) {
    let goal = DialGoal::Sync {
        device_id: peer.device_id.clone(),
    };
    let mut connected = false;
    for addr in addresses {
        match dial(&shared, addr, &goal).await {
            Ok(_) => {
                connected = true;
                break;
            }
            Err(DialError::Rejected(RejectReason::NotPaired)) => {
                // 对方已经取消了和本机的配对（离线时发不了 Unpair），本机也跟着移除。
                log::info!("{} no longer trusts this device, removing it", peer.name);
                shared.revoke(&peer.device_id);
                shared.emit_state();
                break;
            }
            Err(err) => log::debug!("lan sync dial {} at {addr} failed: {err:?}", peer.name),
        }
    }
    shared.finish_dial(&peer.device_id, connected);
}

/// 已认证连接上的同步会话：登记连接、按水位线请求补齐，然后收发直到断开。
async fn run_session(
    shared: Arc<Shared>,
    remote_public_key: [u8; 32],
    device: DeviceInfo,
    mut reader: SecureReader,
    writer: SecureWriter,
    dialer_is_me: bool,
    addr: SocketAddr,
) {
    let peer_id = device_id_for(&remote_public_key);
    let connection_id = shared.next_conn_id.fetch_add(1, Ordering::Relaxed);
    let (tx, rx) = mpsc::channel::<Outgoing>(OUTGOING_QUEUE);
    let (kill_tx, kill_rx) = watch::channel(false);
    let kill_tx = Arc::new(kill_tx);
    let registered = shared.register(
        &peer_id,
        &to_hex(&remote_public_key),
        Connection {
            id: connection_id,
            dialer_is_me,
            tx: tx.clone(),
            kill: kill_tx.clone(),
            max_attachment: device.max_attachment_bytes,
            live: LiveCursor::default(),
        },
    );
    if !registered {
        log::debug!("lan sync declined session registration for {peer_id}");
        return;
    }

    // 认证完成：放宽到 JSON 头上限加本机的附件上限。
    let own_limit = usize::try_from(shared.max_attachment()).unwrap_or(usize::MAX);
    reader.set_limit(4 + MAX_HEADER_BYTES.saturating_add(own_limit));

    let address = with_port(addr, device.listen_port).to_string();
    shared
        .peers
        .touch(&peer_id, &device.name, device.platform, Some(address));
    shared.reset_backoff(&peer_id);
    log::info!(
        "lan sync connected to {} ({peer_id}) via {addr}",
        device.name
    );
    shared.emit_state();

    let writer_task = tokio::spawn(write_loop(
        writer,
        rx,
        kill_tx.clone(),
        kill_rx.clone(),
        shared.shutdown.clone(),
    ));

    let (after, epoch) = shared
        .peers
        .get(&peer_id)
        .map_or((0, None), |peer| (peer.watermark, peer.epoch));
    let request = Message::CatchUp { after, epoch };
    let _ = tx.send((request, Arc::new(Vec::new()))).await;

    let session = Session {
        peer_id: &peer_id,
        connection_id,
        platform: device.platform,
        peer_max_attachment: device.max_attachment_bytes,
    };
    read_loop(&shared, &session, &mut reader, &tx, kill_rx).await;

    let _ = kill_tx.send(true);
    drop(tx);
    let _ = writer_task.await;
    shared.unregister(&peer_id, connection_id);
    log::info!("lan sync disconnected from {} ({peer_id})", device.name);
    shared.emit_state();
    shared.dial_now.notify_one();
}

struct Session<'a> {
    peer_id: &'a str,
    connection_id: u64,
    platform: Platform,
    peer_max_attachment: u64,
}

async fn read_loop(
    shared: &Arc<Shared>,
    session: &Session<'_>,
    reader: &mut SecureReader,
    tx: &mpsc::Sender<Outgoing>,
    mut kill: watch::Receiver<bool>,
) {
    let peer_id = session.peer_id;
    let mut shutdown = shared.shutdown.clone();
    // 每次补齐要把一整页（最多几十张图）读进内存：同一条连接同时只答一个请求，
    // 对方连发的请求不会把内存堆上去。正常的对端收到一页才要下一页。
    let catching_up = Arc::new(AtomicBool::new(false));
    loop {
        let received = tokio::select! {
            received = reader.recv_message() => received,
            _ = kill.changed() => return,
            _ = shutdown.changed() => return,
        };
        let (message, attachment) = match received {
            Ok(message) => message,
            Err(err) => {
                log::info!("lan sync connection to {peer_id} closed: {err:#}");
                return;
            }
        };

        match message {
            Message::Ping => {}
            Message::Push { item, live } => {
                let Some(core) = shared.core() else {
                    return;
                };
                if let Err(err) = engine::receive(
                    &core,
                    &shared.echo,
                    peer_id,
                    session.platform,
                    item,
                    attachment,
                    live,
                )
                .await
                {
                    log::warn!("lan sync receive from {peer_id} failed: {err:#}");
                    return;
                }
            }
            Message::CatchUp { after, epoch } => {
                let Some(core) = shared.core() else {
                    return;
                };
                if catching_up.swap(true, Ordering::AcqRel) {
                    log::debug!(
                        "lan sync ignores a catch-up request from {peer_id} while one runs"
                    );
                    continue;
                }
                let busy = catching_up.clone();
                let request = CatchUpRequest {
                    shared: shared.clone(),
                    peer_id: peer_id.to_owned(),
                    connection_id: session.connection_id,
                    tx: tx.clone(),
                    attachment_limit: session.peer_max_attachment,
                };
                tokio::spawn(async move {
                    send_catch_up(core, request, after, epoch).await;
                    busy.store(false, Ordering::Release);
                });
            }
            // 这一页（或检查点）之前的记录都已经在上面逐条入库，可以推进水位线。
            Message::CatchUpPage { until, more, epoch } => {
                shared.peers.advance_watermark(peer_id, &epoch, until);
                if more {
                    let next = Message::CatchUp {
                        after: until,
                        epoch: Some(epoch),
                    };
                    if tx.send((next, Arc::new(Vec::new()))).await.is_err() {
                        return;
                    }
                }
            }
            Message::Unpair => {
                log::info!("lan sync peer {peer_id} removed this device");
                shared.revoke(peer_id);
                shared.emit_state();
                return;
            }
            Message::Unknown => log::debug!("lan sync skips a message it does not understand"),
            other => log::debug!("lan sync ignores unexpected {other:?}"),
        }
    }
}

/// 对方的一次补齐请求要回到哪条连接。
struct CatchUpRequest {
    shared: Arc<Shared>,
    peer_id: String,
    connection_id: u64,
    tx: mpsc::Sender<Outgoing>,
    attachment_limit: u64,
}

async fn send_catch_up(
    core: Arc<CoreInner>,
    request: CatchUpRequest,
    after: u64,
    epoch: Option<String>,
) {
    let page =
        match engine::collect_catch_up(&core, after, epoch.as_deref(), request.attachment_limit)
            .await
        {
            Ok(page) => page,
            Err(err) => {
                log::warn!("lan sync catch-up failed: {err:#}");
                return;
            }
        };

    for (item, attachment) in page.items {
        let push = Message::Push { item, live: false };
        if request.tx.send((push, Arc::new(attachment))).await.is_err() {
            return;
        }
    }
    let done = Message::CatchUpPage {
        until: page.until,
        more: page.more,
        epoch: page.epoch.clone(),
    };
    if request.tx.send((done, Arc::new(Vec::new()))).await.is_err() {
        return;
    }
    if !page.more {
        request.shared.confirm_catch_up(
            &request.peer_id,
            request.connection_id,
            page.epoch,
            page.until,
        );
    }
}

async fn write_loop(
    mut writer: SecureWriter,
    mut rx: mpsc::Receiver<Outgoing>,
    kill_tx: Arc<watch::Sender<bool>>,
    mut kill: watch::Receiver<bool>,
    mut shutdown: watch::Receiver<bool>,
) {
    let mut ping = tokio::time::interval(PING_INTERVAL);
    ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    ping.tick().await;

    loop {
        let (message, attachment) = tokio::select! {
            outgoing = rx.recv() => match outgoing {
                Some(outgoing) => outgoing,
                None => break,
            },
            _ = ping.tick() => (Message::Ping, Arc::new(Vec::new())),
            _ = kill.changed() => break,
            _ = shutdown.changed() => break,
        };
        // 写到一半取消或超时后直接丢弃流，不能继续复用部分写出的消息。
        let result = tokio::select! {
            biased;
            _ = kill.changed() => break,
            _ = shutdown.changed() => break,
            result = writer.send_message(&message, &attachment) => result,
        };
        if let Err(err) = result {
            log::info!("lan sync write failed: {err:#}");
            break;
        }
    }
    let _ = kill_tx.send(true);
}

async fn recv_within(reader: &mut SecureReader) -> anyhow::Result<(Message, Vec<u8>)> {
    tokio::time::timeout(HANDSHAKE_TIMEOUT, reader.recv_message())
        .await
        .map_err(|_| anyhow!("peer did not answer in time"))?
}

async fn reject(writer: &mut SecureWriter, reason: RejectReason, supported: Option<VersionRange>) {
    if let Err(err) = writer
        .send_message(&Message::Reject { reason, supported }, &[])
        .await
    {
        log::debug!("lan sync reject not delivered: {err:#}");
    }
}

/// 换成对方的监听端口；IPv6 保留 scope id（链路本地地址少了它连不上）。
fn with_port(addr: SocketAddr, port: u16) -> SocketAddr {
    match addr {
        SocketAddr::V4(v4) => SocketAddr::new(IpAddr::V4(*v4.ip()), port),
        SocketAddr::V6(v6) => SocketAddr::V6(SocketAddrV6::new(*v6.ip(), port, 0, v6.scope_id())),
    }
}

/// 解析手动输入的地址：`ip`、`ip:端口`、`[ipv6]:端口`、带 `%网卡` 的链路本地 IPv6
/// （网卡写编号或名字都行）、`主机名[:端口]`。缺端口时用默认端口。
async fn resolve_address(text: &str) -> Option<Vec<SocketAddr>> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    if let Some(addr) = parse_literal_address(text) {
        return Some(vec![addr]);
    }
    // 像 IPv6 字面量却没解析出来（网卡不存在、括号不配对）：主机名里不会有这些字符，不必再查 DNS。
    if text.contains(['%', '[', ']']) {
        return None;
    }

    let with_port = if text.contains(':') {
        text.to_owned()
    } else {
        format!("{text}:{DEFAULT_PORT}")
    };
    let addresses: Vec<SocketAddr> = tokio::net::lookup_host(with_port).await.ok()?.collect();
    (!addresses.is_empty()).then_some(addresses)
}

/// 不查 DNS 就能解析的地址字面量。
fn parse_literal_address(text: &str) -> Option<SocketAddr> {
    if let Ok(addr) = text.parse::<SocketAddr>() {
        return Some(addr);
    }
    if let Ok(ip) = text.parse::<IpAddr>() {
        return Some(SocketAddr::new(ip, DEFAULT_PORT));
    }

    // 带 scope 的 IPv6：`[fe80::1%en0]:41573` 或 `fe80::1%12`。
    let (host, port) = match text.strip_prefix('[') {
        Some(rest) => {
            let (host, tail) = rest.split_once(']')?;
            let port = match tail.strip_prefix(':') {
                Some(port) => port.parse().ok()?,
                None if tail.is_empty() => DEFAULT_PORT,
                None => return None,
            };
            (host, port)
        }
        None => (text, DEFAULT_PORT),
    };
    let (ip, scope) = host.split_once('%')?;
    let ip: Ipv6Addr = ip.parse().ok()?;
    let scope = scope
        .parse::<u32>()
        .ok()
        .or_else(|| interface_index(scope))?;
    Some(SocketAddr::V6(SocketAddrV6::new(ip, port, 0, scope)))
}

/// 网卡名对应的编号。
fn interface_index(name: &str) -> Option<u32> {
    if_addrs::get_if_addrs()
        .ok()?
        .into_iter()
        .find(|interface| interface.name == name)
        .and_then(|interface| interface.index)
}

fn local_addresses() -> Vec<String> {
    let mut addresses: Vec<String> = local_ipv4s().into_iter().map(|ip| ip.to_string()).collect();
    addresses.extend(local_ipv6s());
    addresses
}

/// 本机可被别的设备连接的 IPv4 地址，默认路由所在网卡排第一。
/// Hyper-V、WSL、VMware 等虚拟网卡的地址别的电脑连不到，只在没有别的地址时才列出。
fn local_ipv4s() -> Vec<Ipv4Addr> {
    let Ok(interfaces) = if_addrs::get_if_addrs() else {
        return Vec::new();
    };

    let mut physical = Vec::new();
    let mut virtual_adapters = Vec::new();
    for interface in interfaces {
        let IpAddr::V4(ip) = interface.ip() else {
            continue;
        };
        if ip.is_loopback() || ip.is_link_local() {
            continue;
        }
        if is_virtual_adapter(&interface.name) {
            virtual_adapters.push(ip);
        } else {
            physical.push(ip);
        }
    }

    let mut addresses = if physical.is_empty() {
        virtual_adapters
    } else {
        physical
    };
    let primary = primary_ipv4();
    addresses.sort_by_key(|ip| (Some(*ip) != primary, *ip));
    addresses.dedup();
    addresses
}

/// 物理网卡上的 IPv6 地址：全局地址在前，链路本地地址带 `%scope`（Windows 写网卡编号，
/// macOS 写网卡名，与系统自己的写法一致）。
fn local_ipv6s() -> Vec<String> {
    let Ok(interfaces) = if_addrs::get_if_addrs() else {
        return Vec::new();
    };

    let mut global = Vec::new();
    let mut link_local = Vec::new();
    for interface in interfaces {
        let IpAddr::V6(ip) = interface.ip() else {
            continue;
        };
        if ip.is_loopback() || ip.is_unspecified() || is_virtual_adapter(&interface.name) {
            continue;
        }
        if ip.is_unicast_link_local() {
            let scope = if cfg!(target_os = "windows") {
                interface.index.map(|index| index.to_string())
            } else {
                Some(interface.name.clone())
            };
            if let Some(scope) = scope {
                link_local.push(format!("{ip}%{scope}"));
            }
        } else {
            global.push(ip.to_string());
        }
    }
    global.sort();
    global.dedup();
    link_local.sort();
    link_local.dedup();
    global.extend(link_local);
    global
}

fn is_virtual_adapter(name: &str) -> bool {
    const MARKERS: [&str; 7] = [
        "vethernet",
        "wsl",
        "hyper-v",
        "vmware",
        "vmnet",
        "virtualbox",
        "vboxnet",
    ];
    let name = name.to_lowercase();
    MARKERS.iter().any(|marker| name.contains(marker))
}

/// 默认路由出口的地址：对 UDP 套接字 connect 只选路由、不发包。
fn primary_ipv4() -> Option<Ipv4Addr> {
    let socket = std::net::UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok()?;
    socket.connect((Ipv4Addr::new(8, 8, 8, 8), 53)).ok()?;
    match socket.local_addr().ok()?.ip() {
        IpAddr::V4(ip) if !ip.is_unspecified() => Some(ip),
        _ => None,
    }
}

/// 拨号顺序：和本机同一网段（/24）的 IPv4 最先，其次别的 IPv4，再是全局 IPv6，最后链路本地 IPv6。
/// 界面上也显示排第一的那个。
fn rank_addresses(addresses: &mut [SocketAddr], local: &[Ipv4Addr]) {
    addresses.sort_by_key(|addr| match addr.ip() {
        IpAddr::V4(ip) => {
            let same_subnet = local
                .iter()
                .any(|local| local.octets()[..3] == ip.octets()[..3]);
            if same_subnet {
                0
            } else {
                1
            }
        }
        IpAddr::V6(ip) if ip.is_unicast_link_local() => 3,
        IpAddr::V6(_) => 2,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn connection(id: u64, max_attachment: u64) -> (Connection, mpsc::Receiver<Outgoing>) {
        let (tx, rx) = mpsc::channel(4);
        let connection = Connection {
            id,
            dialer_is_me: true,
            tx,
            kill: Arc::new(watch::channel(false).0),
            max_attachment,
            live: LiveCursor::default(),
        };
        (connection, rx)
    }

    fn test_shared(dir: &std::path::Path) -> Shared {
        let (_shutdown, shutdown) = watch::channel(false);
        Shared {
            core: Weak::new(),
            app_version: "test".to_owned(),
            supported: SUPPORTED,
            identity: identity::load_or_create(dir).unwrap().identity,
            peers: Arc::new(PeerStore::load(dir)),
            port: 0,
            connections: Mutex::new(HashMap::new()),
            nearby: Mutex::new(HashMap::new()),
            pairing: Mutex::new(PairingCode::new()),
            echo: Mutex::new(EchoFilter::default()),
            dials: Mutex::new(HashMap::new()),
            dial_now: Notify::new(),
            handshakes: Arc::new(Semaphore::new(1)),
            shutdown,
            next_conn_id: AtomicU64::new(1),
            captures: mpsc::channel(OUTGOING_QUEUE).0,
        }
    }

    fn trust(shared: &Shared, id: &str) {
        shared.peers.add(TrustedPeer {
            device_id: id.to_owned(),
            public_key: "key".to_owned(),
            name: id.to_owned(),
            platform: Platform::Windows,
            address: None,
            paired_at: Utc::now(),
            last_seen_at: None,
            watermark: 0,
            epoch: None,
        });
    }

    #[test]
    fn registration_rechecks_pinned_key_after_unpairing() {
        let dir = tempfile::tempdir().unwrap();
        let shared = test_shared(dir.path());
        for dialer_is_me in [true, false] {
            trust(&shared, "peer");
            let (mut pending, _rx) = connection(1, 0);
            pending.dialer_is_me = dialer_is_me;
            assert!(!shared.register("peer", "wrong-key", pending));
            assert!(shared.revoke("peer").is_none());

            let (mut pending, _rx) = connection(2, 0);
            pending.dialer_is_me = dialer_is_me;
            assert!(!shared.register("peer", "key", pending));
            assert!(!shared.is_connected("peer"));

            trust(&shared, "peer");
            let (registered, _rx) = connection(3, 0);
            assert!(shared.register("peer", "key", registered));
            let (_, kill) = shared.revoke("peer").unwrap();
            kill.send_replace(true);
            assert!(*kill.borrow());
            shared.unregister("peer", 3);
        }
    }

    async fn secure_pair() -> (Handshaken, Handshaken) {
        let dir = tempfile::tempdir().unwrap();
        let identity = identity::load_or_create(dir.path()).unwrap().identity;
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let (client, server) = tokio::join!(
            TcpStream::connect(listener.local_addr().unwrap()),
            listener.accept(),
        );
        let (sender, receiver) = tokio::join!(
            handshake(client.unwrap(), &identity, true),
            handshake(server.unwrap().0, &identity, false),
        );
        (sender.unwrap(), receiver.unwrap())
    }

    #[test]
    fn live_captures_and_skips_wait_for_image_preparation() {
        use crate::clipboard::{ClipboardPayload, ImagePayload};
        use crate::db::models::ClipboardKind;
        use crate::testing::{block_on, sample_png, Fixture};

        let fixture = Fixture::new();
        let core = fixture.start();
        let image = core
            .build_item(&ClipboardPayload::Image(ImagePayload {
                bytes: sample_png(2, 2),
                width: 2,
                height: 2,
            }))
            .unwrap()
            .unwrap();
        let mut text = image.clone();
        text.kind = ClipboardKind::Text;
        text.content = "latest".to_owned();
        let mut shared = test_shared(fixture.root());
        shared.core = Arc::downgrade(&core.0);
        let (capture_tx, capture_rx) = mpsc::channel(3);
        shared.captures = capture_tx;
        trust(&shared, "peer");
        let (mut connection, _) = connection(1, u64::MAX);
        let (tx, mut messages) = mpsc::channel(8);
        connection.tx = tx;
        assert!(shared.register("peer", "key", connection));
        shared.confirm_catch_up("peer", 1, "e".to_owned(), 0);
        let shared = Arc::new(shared);
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .max_blocking_threads(1)
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let (started_tx, started_rx) = tokio::sync::oneshot::channel();
            let (release_tx, release_rx) = std::sync::mpsc::channel();
            let blocker = tokio::task::spawn_blocking(move || {
                started_tx.send(()).unwrap();
                release_rx.recv().unwrap();
            });
            started_rx.await.unwrap();
            let sender = tokio::spawn(super::super::send_captures(shared.clone(), capture_rx));
            shared
                .captures
                .try_send((Some(1), Some(image), u64::MAX))
                .unwrap();
            shared.captures.try_send((Some(2), None, u64::MAX)).unwrap();
            shared
                .captures
                .try_send((Some(3), Some(text), u64::MAX))
                .unwrap();
            tokio::time::sleep(Duration::from_millis(50)).await;
            let overtook_image = messages.try_recv().is_ok();
            release_tx.send(()).unwrap();
            blocker.await.unwrap();
            assert!(!overtook_image);

            assert!(matches!(messages.recv().await.unwrap().0,
                Message::Push { item, live: true } if item.kind == ClipboardKind::Image));
            assert_eq!(checkpoint(Some(messages.recv().await.unwrap().0)), Some(1));
            assert_eq!(checkpoint(Some(messages.recv().await.unwrap().0)), Some(2));
            assert!(matches!(messages.recv().await.unwrap().0,
                Message::Push { item, live: true } if item.content == "latest"));
            assert_eq!(checkpoint(Some(messages.recv().await.unwrap().0)), Some(3));
            sender.abort();
        });
        block_on(core.shutdown()).unwrap();
    }

    #[test]
    fn failed_receive_does_not_advance_watermark() {
        use crate::testing::{block_on, Fixture};

        let fixture = Fixture::new();
        let core = fixture.start();
        block_on(core.hop({
            let core = core.clone();
            let mut shared = test_shared(fixture.root());
            shared.core = Arc::downgrade(&core.0);
            trust(&shared, "peer");
            async move {
                sqlx::query(
                    "CREATE TRIGGER fail_sync_receive BEFORE INSERT ON clipboard_items \
                    BEGIN SELECT RAISE(ABORT, 'storage temporarily unavailable'); END",
                )
                .execute(&core.0.db.pool().await)
                .await
                .unwrap();
                let (mut sender, mut receiver) = secure_pair().await;
                let push = Message::Push {
                    item: super::super::protocol::WireItem {
                        kind: crate::db::models::ClipboardKind::Text,
                        sub_kind: None,
                        content: "received while storage is unavailable".to_owned(),
                        search_text: None,
                        summary: None,
                        width: None,
                        height: None,
                    },
                    live: false,
                };
                sender.writer.send_message(&push, &[]).await.unwrap();
                sender
                    .writer
                    .send_message(
                        &Message::CatchUpPage {
                            until: 1,
                            more: false,
                            epoch: "e".to_owned(),
                        },
                        &[],
                    )
                    .await
                    .unwrap();
                drop(sender);
                let (tx, _rx) = mpsc::channel(1);
                let (_kill_tx, kill) = watch::channel(false);
                let (_shutdown_tx, shutdown) = watch::channel(false);
                shared.shutdown = shutdown;
                let shared = Arc::new(shared);
                let session = Session {
                    peer_id: "peer",
                    connection_id: 1,
                    platform: Platform::Windows,
                    peer_max_attachment: 0,
                };
                read_loop(&shared, &session, &mut receiver.reader, &tx, kill).await;
                assert_eq!(shared.peers.get("peer").unwrap().watermark, 0);
                Ok(())
            }
        }))
        .unwrap();
        block_on(core.shutdown()).unwrap();
    }

    #[tokio::test]
    async fn blocked_writer_stops_on_kill_and_shutdown() {
        for stop_service in [false, true] {
            let (sender, _receiver) = secure_pair().await;
            let (tx, rx) = mpsc::channel(1);
            let (kill_tx, kill) = watch::channel(false);
            let kill_tx = Arc::new(kill_tx);
            let (shutdown_tx, shutdown) = watch::channel(false);
            tx.send((Message::Ping, Arc::new(vec![0; 32 * 1024 * 1024])))
                .await
                .unwrap();
            let writer = tokio::spawn(write_loop(
                sender.writer,
                rx,
                kill_tx.clone(),
                kill,
                shutdown,
            ));
            tokio::time::sleep(Duration::from_millis(100)).await;
            assert!(!writer.is_finished());

            if stop_service {
                shutdown_tx.send(true).unwrap();
            } else {
                kill_tx.send(true).unwrap();
            }

            tokio::time::timeout(Duration::from_secs(2), writer)
                .await
                .expect("blocked write ignored cancellation")
                .unwrap();
            assert!(*kill_tx.borrow());
            assert!(tx.is_closed());
        }
    }

    fn checkpoint(message: Option<Message>) -> Option<u64> {
        match message? {
            Message::CatchUpPage {
                until,
                more: false,
                epoch,
            } if epoch == "e" => Some(until),
            other => panic!("unexpected checkpoint {other:?}"),
        }
    }

    /// 补齐确认之前处理过的序号先记着；确认之后连续的部分随即成为检查点，有空洞时停在空洞前。
    #[test]
    fn live_checkpoints_follow_contiguous_sequences() {
        let mut cursor = LiveCursor::default();

        assert_eq!(checkpoint(cursor.handle(11)), None);
        assert_eq!(checkpoint(cursor.handle(9)), None);
        assert_eq!(checkpoint(cursor.confirm("e".to_owned(), 10)), Some(11));
        assert_eq!(checkpoint(cursor.handle(13)), None);
        assert_eq!(checkpoint(cursor.handle(12)), Some(13));

        cursor.broken = true;
        assert_eq!(checkpoint(cursor.handle(14)), None);
    }

    /// 实时推送按每台设备申报的附件上限分别跳过，不因为一台设备的上限小而影响别的设备。
    #[test]
    fn broadcast_skips_attachments_over_each_peers_limit() {
        let dir = tempfile::tempdir().unwrap();
        let shared = test_shared(dir.path());
        let (small, mut small_rx) = connection(1, 10);
        let (large, mut large_rx) = connection(2, 100);
        trust(&shared, "small");
        trust(&shared, "large");
        assert!(shared.register("small", "key", small));
        assert!(shared.register("large", "key", large));

        shared.deliver(None, Some((Message::Ping, Arc::new(vec![0; 50]))));
        shared.deliver(None, Some((Message::Unpair, Arc::new(vec![0; 10]))));

        assert!(matches!(small_rx.try_recv(), Ok((Message::Unpair, _))));
        assert!(small_rx.try_recv().is_err());
        assert!(matches!(large_rx.try_recv(), Ok((Message::Ping, _))));
        assert!(matches!(large_rx.try_recv(), Ok((Message::Unpair, _))));
    }

    #[tokio::test]
    async fn resolves_manual_addresses() {
        assert_eq!(
            resolve_address("192.168.1.8").await,
            Some(vec!["192.168.1.8:41573".parse().unwrap()])
        );
        assert_eq!(
            resolve_address(" 192.168.1.8:5000 ").await,
            Some(vec!["192.168.1.8:5000".parse().unwrap()])
        );
        assert_eq!(resolve_address("").await, None);
    }

    #[test]
    fn parses_ipv6_literals_with_and_without_scope() {
        assert_eq!(
            parse_literal_address("::1"),
            Some("[::1]:41573".parse().unwrap())
        );
        assert_eq!(
            parse_literal_address("[2001:db8::5]:5000"),
            Some("[2001:db8::5]:5000".parse().unwrap())
        );
        let scoped = SocketAddr::V6(SocketAddrV6::new("fe80::1".parse().unwrap(), 41573, 0, 12));
        assert_eq!(parse_literal_address("fe80::1%12"), Some(scoped));
        assert_eq!(parse_literal_address("[fe80::1%12]"), Some(scoped));
        assert_eq!(
            parse_literal_address("[fe80::1%12]:5000"),
            Some(with_port(scoped, 5000))
        );
        // scope id 要原样带回界面和 peers.json，写回去还能再解析出来。
        assert_eq!(
            with_port(scoped, 5000)
                .to_string()
                .parse::<SocketAddr>()
                .ok(),
            Some(with_port(scoped, 5000))
        );
        assert_eq!(parse_literal_address("fe80::1%no-such-interface"), None);
        assert_eq!(parse_literal_address("[fe80::1%12]x"), None);
    }

    #[test]
    fn addresses_rank_ipv4_first_then_global_then_link_local_ipv6() {
        let mut addresses: Vec<SocketAddr> = vec![
            SocketAddr::V6(SocketAddrV6::new("fe80::9".parse().unwrap(), 41573, 0, 7)),
            "[2001:db8::7]:41573".parse().unwrap(),
            "172.30.16.1:41573".parse().unwrap(),
            "192.168.1.20:41573".parse().unwrap(),
        ];

        rank_addresses(&mut addresses, &[Ipv4Addr::new(192, 168, 1, 8)]);

        assert_eq!(addresses[0], "192.168.1.20:41573".parse().unwrap());
        assert_eq!(addresses[1], "172.30.16.1:41573".parse().unwrap());
        assert_eq!(addresses[2], "[2001:db8::7]:41573".parse().unwrap());
        assert!(addresses[3].ip().is_ipv6());
    }

    #[test]
    fn recognizes_virtual_adapters() {
        assert!(is_virtual_adapter("vEthernet (WSL (Hyper-V firewall))"));
        assert!(is_virtual_adapter("VMware Network Adapter VMnet8"));
        assert!(!is_virtual_adapter("以太网"));
        assert!(!is_virtual_adapter("en0"));
    }

    /// 回环模式在 127.0.0.1 和 ::1 上用同一个随机端口监听。
    #[tokio::test]
    async fn loopback_listens_on_both_families_with_one_port() {
        let (listeners, port) = bind_listeners(&LanSyncNetwork::loopback()).unwrap();

        assert_ne!(port, 0);
        let addresses: Vec<SocketAddr> = listeners
            .iter()
            .map(|listener| listener.local_addr().unwrap())
            .collect();
        assert_eq!(
            addresses[0],
            SocketAddr::new(Ipv4Addr::LOCALHOST.into(), port)
        );
        if let Some(v6) = addresses.get(1) {
            assert_eq!(*v6, SocketAddr::new(Ipv6Addr::LOCALHOST.into(), port));
        }
    }
}
