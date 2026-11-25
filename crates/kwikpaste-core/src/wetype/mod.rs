//! 微信输入法宿主桥接：监督常驻子进程，共用 LAN 入库和剪贴板写回链路。
mod devices;
#[cfg(test)]
mod tests;

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use anyhow::{bail, Context};
pub use kwikpaste_ext_protocol::WeTypeDevice;
use kwikpaste_ext_protocol::{
    WeTypeCommand as Command, WeTypeContent as Content, WeTypeEvent as Event,
    WETYPE_MAX_IMAGE_BYTES, WETYPE_MAX_TEXT_BYTES,
};
use serde::{Deserialize, Serialize};
use tokio::sync::{oneshot, watch};

use crate::db::models::{ClipboardItem, ClipboardKind, ClipboardSubKind};
use crate::extensions::process::ProcessHost;
use crate::root::CoreInner;
use crate::settings::WeTypeSettings;
use crate::sync::engine::{EchoFilter, ReceiveOptions};
use crate::sync::protocol::WireItem;
use crate::{Core, CoreEvent, Result};
pub(crate) use devices::DeviceNames;

/// UI 的只读快照；状态变化由 WeTypeChanged 通知，不含 SDK 身份或密钥。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WeTypeState {
    pub running: bool,
    pub paired: bool,
    pub self_name: String,
    pub devices: Vec<WeTypeDevice>,
    pub error: Option<String>,
}

type Pending = Mutex<Option<oneshot::Sender<std::result::Result<(), String>>>>;

pub(crate) struct WeTypeService {
    state: Mutex<WeTypeState>,
    sender: Mutex<Option<mpsc::SyncSender<Command>>>,
    pub names: DeviceNames,
    echo: Mutex<EchoFilter>,
    /// 最近发出的内容；和收到的分开记，交错的复制不会互相冲掉。
    sent: Mutex<EchoFilter>,
    pair: Pending,
    unpair: Pending,
    pair_lock: tokio::sync::Mutex<()>,
    lifecycle: tokio::sync::Mutex<()>,
    task: Mutex<Option<(watch::Sender<bool>, tokio::task::JoinHandle<()>)>>,
    closed: AtomicBool,
    ready: AtomicBool,
}

impl WeTypeService {
    pub fn new(dir: PathBuf) -> Self {
        Self {
            state: Default::default(),
            sender: Default::default(),
            names: DeviceNames::load(dir.join("devices.json")),
            echo: Default::default(),
            sent: Default::default(),
            pair: Default::default(),
            unpair: Default::default(),
            pair_lock: Default::default(),
            lifecycle: Default::default(),
            task: Default::default(),
            closed: AtomicBool::new(false),
            ready: AtomicBool::new(false),
        }
    }

    fn snapshot(&self) -> WeTypeState {
        lock(&self.state).clone()
    }

    fn send(&self, command: Command) -> anyhow::Result<()> {
        if !self.ready.load(Ordering::Acquire) {
            bail!("WeType process is not ready");
        }
        lock(&self.sender)
            .as_ref()
            .context("WeType process is not running")?
            .try_send(command)
            .map_err(|err| anyhow::anyhow!("WeType command queue unavailable: {err}"))
    }

    /// 安装操作串行调用；未配对也运行，才能接收 UI 的配对请求。
    pub async fn apply(&self, core: &Arc<CoreInner>) {
        let _lifecycle = self.lifecycle.lock().await;
        if self.closed.load(Ordering::Acquire) || !core.extensions.enabled("wetype") {
            return;
        }
        if lock(&self.task).is_some() {
            return;
        }
        let (stop, receiver) = watch::channel(false);
        let weak = Arc::downgrade(core);
        let task = core.rt.spawn(supervise(weak, receiver));
        *lock(&self.task) = Some((stop, task));
    }

    pub async fn stop(&self) {
        let _lifecycle = self.lifecycle.lock().await;
        let task = lock(&self.task).take();
        if let Some((stop, task)) = task {
            let _ = stop.send(true);
            if let Err(err) = task.await {
                log::warn!("WeType supervisor join failed: {err}");
            }
        }
        lock(&self.sender).take();
        self.disconnected(None);
    }

    pub async fn shutdown(&self) {
        self.closed.store(true, Ordering::Release);
        self.stop().await;
    }

    fn disconnected(&self, error: Option<String>) {
        self.ready.store(false, Ordering::Release);
        lock(&self.sender).take();
        let mut state = lock(&self.state);
        state.running = false;
        state.paired = false;
        state.error = error;
        complete(&self.pair, Err("WeType process stopped".into()));
        complete(&self.unpair, Err("WeType process stopped".into()));
    }

    pub fn uninstalled(&self) {
        self.names.clear();
        *lock(&self.state) = WeTypeState::default();
        *lock(&self.echo) = EchoFilter::default();
        *lock(&self.sent) = EchoFilter::default();
    }

    /// State 和配对结果走同一分发入口，假的扩展端也能直接验证它。
    async fn event(&self, core: &Arc<CoreInner>, event: Event) -> anyhow::Result<()> {
        match event {
            Event::State {
                paired,
                self_name,
                devices,
                error,
            } => {
                self.names.update(&devices)?;
                let identity_dropped = !paired && self_name.is_empty();
                self.ready.store(true, Ordering::Release);
                let next = WeTypeState {
                    running: true,
                    paired,
                    self_name,
                    devices,
                    error: error.clone(),
                };
                // 扩展每次定时刷新都会上报一遍，没变就不让界面重绘。
                let changed = {
                    let mut state = lock(&self.state);
                    let changed = *state != next;
                    *state = next;
                    changed
                };
                if let Some(error) = error {
                    complete(&self.unpair, Err(error));
                } else if identity_dropped {
                    complete(&self.unpair, Ok(()));
                }
                if changed {
                    core.events.emit(CoreEvent::WeTypeChanged);
                }
            }
            Event::PairResult { ok, error } => {
                complete(&self.pair, outcome(ok, error));
                core.events.emit(CoreEvent::WeTypeChanged);
            }
            Event::Received {
                from_uin,
                from_name,
                content,
            } => {
                // 本机官方客户端上传的是这台电脑自己的复制，监听已经记过，不再记一遍。
                let own_client = from_name == crate::sync::default_device_name();
                if self.names.update(&[WeTypeDevice {
                    uin: from_uin,
                    name: from_name,
                }])? {
                    core.events.emit(CoreEvent::WeTypeChanged);
                }
                self.receive(core, from_uin, content, own_client).await?;
                // 成功提交或明确跳过才确认；失败使监督器重启，SDK 从持久化待收记录重投。
                self.send(Command::Acknowledge)?;
            }
            Event::SendResult { ok, error } => {
                if !ok {
                    let error = error.unwrap_or_else(|| "WeType send failed".into());
                    log::warn!("WeType send failed: {error}");
                    lock(&self.state).error = Some(error);
                    core.events.emit(CoreEvent::WeTypeChanged);
                }
            }
        }
        Ok(())
    }

    async fn receive(
        &self,
        core: &Arc<CoreInner>,
        uin: u64,
        content: Content,
        own_client: bool,
    ) -> anyhow::Result<()> {
        let options = core.settings.snapshot().extensions.wetype;
        let (wire, attachment, cleanup) = match content {
            Content::Text { text } => {
                if own_client || text.len() > WETYPE_MAX_TEXT_BYTES || text.trim().is_empty() {
                    return Ok(());
                }
                (wire(ClipboardKind::Text, text), Vec::new(), None)
            }
            Content::Image { path } => {
                let inbox = core.paths.extensions_dir().join("wetype/state/inbox");
                let path = validated_inbox_path(&inbox, Path::new(&path))?;
                if own_client {
                    if let Err(err) = std::fs::remove_file(&path) {
                        log::warn!("WeType skipped inbox cleanup failed: {err}");
                    }
                    return Ok(());
                }
                let load = path.clone();
                let bytes = tokio::task::spawn_blocking(move || read_image(&load))
                    .await
                    .context("WeType image read task failed")??;
                let Some(bytes) = bytes else {
                    if let Err(err) = std::fs::remove_file(path) {
                        log::warn!("WeType skipped inbox cleanup failed: {err}");
                    }
                    return Ok(());
                };
                (wire(ClipboardKind::Image, String::new()), bytes, Some(path))
            }
        };
        crate::sync::engine::receive_with_options(
            core,
            &self.echo,
            &format!("wetype:{uin}"),
            crate::sync::current_platform(),
            wire,
            attachment,
            true,
            ReceiveOptions {
                text: true,
                image: true,
                // 官方客户端在运行时它会自己写剪贴板，再写一次会被它当成本机复制传回去。
                write_clipboard: options.write_clipboard && !official_client_running(core),
                max_image_bytes: WETYPE_MAX_IMAGE_BYTES,
                allow_jpeg: true,
                allow_webp: false,
                captured_at: None,
            },
        )
        .await?;
        if let Some(path) = cleanup {
            // 已提交的记录不能因为临时文件清理失败而反复重收。
            if let Err(err) = std::fs::remove_file(path) {
                log::warn!("WeType inbox cleanup failed: {err}");
            }
        }
        Ok(())
    }
}

fn lock<T>(value: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    value.lock().unwrap_or_else(|p| p.into_inner())
}

fn complete(pending: &Pending, result: std::result::Result<(), String>) {
    if let Some(reply) = lock(pending).take() {
        let _ = reply.send(result);
    }
}

fn outcome(ok: bool, error: Option<String>) -> std::result::Result<(), String> {
    if ok {
        Ok(())
    } else {
        Err(error.unwrap_or_else(|| "WeType operation failed".into()))
    }
}

fn wire(kind: ClipboardKind, content: String) -> WireItem {
    WireItem {
        kind,
        sub_kind: None,
        content,
        search_text: None,
        summary: None,
        width: None,
        height: None,
    }
}

/// 常驻扩展只允许导入自己 inbox 下的普通文件，不接收任意路径或符号链接。
fn validated_inbox_path(inbox: &Path, path: &Path) -> anyhow::Result<PathBuf> {
    let inbox = std::fs::canonicalize(inbox)?;
    if std::fs::symlink_metadata(path)?.file_type().is_symlink() {
        bail!("WeType inbox symlink rejected");
    }
    let path = std::fs::canonicalize(path)?;
    if path.parent() != Some(inbox.as_path()) {
        bail!("WeType image is outside inbox");
    }
    Ok(path)
}

fn read_image(path: &Path) -> anyhow::Result<Option<Vec<u8>>> {
    let file = std::fs::File::open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        bail!("WeType image is not a file");
    }
    if metadata.len() == 0 || metadata.len() > WETYPE_MAX_IMAGE_BYTES {
        return Ok(None);
    }
    let mut bytes = Vec::new();
    file.take(WETYPE_MAX_IMAGE_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > WETYPE_MAX_IMAGE_BYTES {
        return Ok(None);
    }
    Ok(Some(bytes))
}

/// 只在仍启用且非正常停机时重启，连续失败的退避不超过一分钟。
fn restart_delay(enabled: bool, stopping: bool, failures: u32) -> Option<Duration> {
    (enabled && !stopping)
        .then(|| Duration::from_secs(1u64 << failures.min(6)).min(Duration::from_secs(60)))
}

async fn supervise(core: Weak<CoreInner>, mut stop: watch::Receiver<bool>) {
    let mut failures = 0;
    loop {
        let Some(owner) = core.upgrade() else {
            break;
        };
        if *stop.borrow() || !owner.extensions.enabled("wetype") {
            break;
        }
        let state_dir = owner.paths.extensions_dir().join("wetype/state");
        let started = owner
            .extensions
            .resolve("wetype")
            .context("WeType executable missing")
            .and_then(|exe| {
                ProcessHost::<Command, Event>::start_duplex(
                    "wetype",
                    &exe,
                    &["--state".into(), state_dir.into_os_string()],
                    kwikpaste_ext_protocol::WETYPE_MAX_FRAME_BYTES,
                )
                .and_then(|mut host| {
                    let events = host.take_events()?;
                    Ok((host, events))
                })
                .map_err(Into::into)
            });
        let mut error = None;
        if let Ok((mut host, mut rx)) = started {
            *lock(&owner.wetype.sender) = host.sender().ok();
            lock(&owner.wetype.state).running = true;
            owner.events.emit(CoreEvent::WeTypeChanged);
            let at = Instant::now();
            drop(owner);
            loop {
                if *stop.borrow() {
                    break;
                }
                let event = tokio::select! {
                    _ = stop.changed() => break,
                    event = rx.recv() => event,
                };
                match event {
                    Some(Ok(event)) => {
                        let Some(owner) = core.upgrade() else {
                            break;
                        };
                        // 完成正在进行的提交后停机，不中途取消数据库写入。
                        if let Err(err) = owner.wetype.event(&owner, event).await {
                            error = Some(err.to_string());
                            break;
                        }
                    }
                    Some(Err(err)) => {
                        error = Some(err.to_string());
                        break;
                    }
                    None => {
                        error = Some("extension events disconnected".to_owned());
                        break;
                    }
                }
            }
            // 先解除事件通道背压，再等待读写线程退出。
            drop(rx);
            if let Some(owner) = core.upgrade() {
                lock(&owner.wetype.sender).take();
            }
            // 不在 core runtime 上阻塞等待管道线程或子进程退出。
            if let Err(err) = tokio::task::spawn_blocking(move || host.stop()).await {
                log::warn!("WeType stop task failed: {err}");
            }
            if at.elapsed() >= Duration::from_secs(60) {
                failures = 0;
            }
        } else if let Err(err) = started {
            error = Some(err.to_string());
        }
        let Some(owner) = core.upgrade() else {
            break;
        };
        if let Some(error) = &error {
            log::warn!("WeType process failed: {error}");
        }
        owner.wetype.disconnected(error);
        owner.events.emit(CoreEvent::WeTypeChanged);
        let delay = restart_delay(owner.extensions.enabled("wetype"), *stop.borrow(), failures);
        drop(owner);
        let Some(delay) = delay else {
            break;
        };
        failures = failures.saturating_add(1);
        tokio::select! { _ = stop.changed() => break, _ = tokio::time::sleep(delay) => {} }
    }
}

impl Core {
    pub fn wetype_state(&self) -> WeTypeState {
        self.0.wetype.snapshot()
    }

    /// 同一台电脑上的官方微信输入法是否在运行；运行时本扩展只记录，不写剪贴板也不发送。
    pub fn wetype_official_client_running(&self) -> bool {
        official_client_running(&self.0)
    }

    /// 等待 PairResult，最多 90 秒；UI 可在任何执行器调用，不需要 tokio 上下文。
    pub async fn wetype_pair(&self, code: String) -> Result<()> {
        // 手机上的配对码常被读成「123 456」输入。
        let code: String = code.chars().filter(|c| !c.is_whitespace()).collect();
        if code.len() != 6 || !code.bytes().all(|b| b.is_ascii_digit()) {
            return Err(anyhow::anyhow!("pairing code must contain six digits").into());
        }
        let core = self.clone();
        self.hop(async move {
            let service = &core.0.wetype;
            let _pairing = service
                .pair_lock
                .try_lock()
                .map_err(|_| anyhow::anyhow!("WeType operation already pending"))?;
            if service.snapshot().paired {
                return Err(anyhow::anyhow!("WeType is already paired").into());
            }
            let (reply, result) = oneshot::channel();
            *lock(&service.pair) = Some(reply);
            let name = format!("KwikPaste · {}", crate::sync::default_device_name());
            // SDK 的名字上限是 128 个 UTF-8 字节，不能在多字节字符中间截断。
            let name = truncate_name(name, 128);
            if let Err(err) = service.send(Command::Pair {
                code,
                device_name: name,
            }) {
                lock(&service.pair).take();
                return Err(err.into());
            }
            match tokio::time::timeout(Duration::from_secs(90), result).await {
                Ok(Ok(result)) => result.map_err(|e| anyhow::anyhow!(e).into()),
                other => {
                    lock(&service.pair).take();
                    // 没有请求 ID：超时必须结束本次会话，不能让迟到结果完成下一次配对。
                    service.stop().await;
                    service.apply(&core.0).await;
                    let reason = if other.is_err() {
                        "WeType pairing timed out"
                    } else {
                        "WeType process stopped"
                    };
                    Err(anyhow::anyhow!(reason).into())
                }
            }
        })
        .await
    }

    /// 只解除本地身份；成功需等扩展回报未配对，远程设备仍需用户在手机上移除。
    pub async fn wetype_unpair(&self) -> Result<()> {
        let core = self.clone();
        self.hop(async move {
            let service = &core.0.wetype;
            let _pairing = service
                .pair_lock
                .try_lock()
                .map_err(|_| anyhow::anyhow!("WeType operation already pending"))?;
            let (reply, result) = oneshot::channel();
            *lock(&service.unpair) = Some(reply);
            if let Err(err) = service.send(Command::Unpair) {
                lock(&service.unpair).take();
                return Err(err.into());
            }
            match tokio::time::timeout(Duration::from_secs(90), result).await {
                Ok(Ok(result)) => result.map_err(|e| anyhow::anyhow!(e).into()),
                _ => {
                    lock(&service.unpair).take();
                    service.stop().await;
                    service.apply(&core.0).await;
                    Err(anyhow::anyhow!("WeType unpair timed out or process stopped").into())
                }
            }
        })
        .await
    }
}

fn truncate_name(mut name: String, bytes: usize) -> String {
    let mut end = bytes.min(name.len());
    while !name.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    name.truncate(end);
    name
}

/// 同一台电脑上的官方微信输入法会把收到的内容写进剪贴板（文字原样，图片以文件形式）。
/// 本扩展就绪且已配对时会收到这些内容并带上来源，不再当作本机复制记录和转发：
/// 否则会多一条重复记录，转发出去还会顶掉设备组里手机发来的那条最新内容。
pub(crate) fn written_by_official_client(core: &CoreInner, owner: Option<&Path>) -> bool {
    if !owner.is_some_and(is_official_client_exe)
        || !core.extensions.enabled("wetype")
        || !core.wetype.ready.load(Ordering::Acquire)
    {
        return false;
    }
    let state = lock(&core.wetype.state);
    state.running && state.paired
}

const OFFICIAL_CLIENT_PROCESS: &str = "wetype_server.exe";

/// 同一台电脑上的官方微信输入法在运行：剪贴板互传交给它，本扩展只把手机发来的内容记进历史。
pub(crate) fn official_client_running(core: &CoreInner) -> bool {
    core.platform().process_running(OFFICIAL_CLIENT_PROCESS)
}

fn is_official_client_exe(path: &Path) -> bool {
    path.to_string_lossy()
        .to_ascii_lowercase()
        .replace('/', "\\")
        .contains(r"\tencent\wetype\")
}

/// 采集的同步序号是本机来源证明；LAN/WeType 的入库不会走这个发送入口。
pub(crate) fn on_local_capture(core: &CoreInner, item: &ClipboardItem, seq: Option<i64>) {
    let state = core.wetype.snapshot();
    let settings = core.settings.snapshot().extensions.wetype;
    // 官方客户端在运行时本机复制由它发给手机，本扩展再发就是重复的一条。
    if !sendable(item, seq, &state, &settings)
        || official_client_running(core)
        || lock(&core.wetype.echo).is_echo(&item.content_hash)
        || !lock(&core.wetype.sent).should_send(&item.content_hash)
    {
        return;
    }
    let command = match item.kind {
        ClipboardKind::Text => {
            let text = if matches!(
                item.sub_kind,
                Some(ClipboardSubKind::Html | ClipboardSubKind::Rtf)
            ) {
                item.search_text.as_deref().unwrap_or_default()
            } else {
                &item.content
            };
            if text.is_empty() || text.len() > WETYPE_MAX_TEXT_BYTES {
                return;
            }
            Command::SendText {
                text: text.to_owned(),
            }
        }
        ClipboardKind::Image => {
            if crate::clipboard::validate_image_file_name(&item.content).is_err() {
                return;
            }
            let path = core.images.origin_path(&item.content);
            if !std::fs::metadata(&path)
                .is_ok_and(|m| m.is_file() && m.len() > 0 && m.len() <= WETYPE_MAX_IMAGE_BYTES)
            {
                return;
            }
            let Some(path) = path.to_str() else {
                return;
            };
            Command::SendImage {
                path: path.to_owned(),
            }
        }
        ClipboardKind::Files => return,
    };
    if let Err(err) = core.wetype.send(command) {
        log::warn!("WeType capture send failed: {err}");
    }
}

fn sendable(
    item: &ClipboardItem,
    seq: Option<i64>,
    state: &WeTypeState,
    settings: &WeTypeSettings,
) -> bool {
    state.running
        && state.paired
        && seq.is_some()
        && crate::sync::engine::is_syncable_types(item, settings.send_text, settings.send_image)
}
