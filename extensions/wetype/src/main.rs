//! 微信输入法的独立 stdio 桥接；身份、网络协议和 OpenSSL 不进入宿主进程。
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, bail};
use kwikpaste_ext_protocol::{
    self as protocol, WeTypeCommand as Command, WeTypeContent as Content, WeTypeDevice,
    WeTypeEvent as Event,
};
use tokio::sync::{mpsc, watch};
use wx_ime_sdk::{Client, ClientOptions, ClipboardContent, PairingCode, ReceivedId};

struct Bridge<W> {
    state: PathBuf,
    client: Option<Client>,
    paired: bool,
    self_name: String,
    devices: Vec<WeTypeDevice>,
    pending: Option<ReceivedId>,
    acknowledge: bool,
    writer: W,
    buffer: Vec<u8>,
}

impl<W: Write> Bridge<W> {
    fn emit(&mut self, event: Event) -> anyhow::Result<()> {
        protocol::write_frame_with_limit(
            &mut self.writer,
            &event,
            &mut self.buffer,
            protocol::WETYPE_MAX_FRAME_BYTES,
        )?;
        Ok(())
    }

    fn state(&mut self, error: Option<String>) -> anyhow::Result<()> {
        self.emit(Event::State {
            paired: self.paired,
            self_name: self.self_name.clone(),
            devices: self.devices.clone(),
            error,
        })
    }

    /// 重开时只读取设备名；不能用新电脑名替换 SDK 已绑定的身份名称。
    async fn open(&mut self, name: String) -> anyhow::Result<()> {
        let identity = self.state.join("identity.json");
        let name = if identity.exists() {
            let bytes = std::fs::read(identity)?;
            let value: serde_json::Value = serde_json::from_slice(&bytes)?;
            value
                .get("device_name")
                .and_then(|v| v.as_str())
                .context("identity device name missing")?
                .to_owned()
        } else {
            name
        };
        self.client = Some(Client::open(ClientOptions::new(&self.state, &name)).await?);
        self.self_name = name;
        self.refresh().await
    }

    async fn refresh(&mut self) -> anyhow::Result<()> {
        let Some(client) = &self.client else {
            return Ok(());
        };
        self.self_name = client.local_device().await?.name;
        let group = client.group().await?;
        self.paired = group.is_some();
        self.devices = group
            .map(|g| {
                g.devices
                    .into_iter()
                    .map(|d| WeTypeDevice {
                        uin: d.uin,
                        name: d.name,
                    })
                    .collect()
            })
            .unwrap_or_default();
        Ok(())
    }

    async fn shutdown(&mut self) {
        if let Some(client) = self.client.take()
            && let Err(err) = client.shutdown().await
        {
            eprintln!("WeType shutdown failed: {err}");
        }
        self.pending = None;
        self.acknowledge = false;
    }

    /// SDK 缓存在 state/cache；复制到 inbox 后仍保留原缓存，直到宿主提交并确认。
    fn received(&mut self, receipt: wx_ime_sdk::ReceivedClipboard) -> anyhow::Result<()> {
        let content = match receipt.content {
            ClipboardContent::Text(text) => {
                if text.len() > protocol::WETYPE_MAX_TEXT_BYTES {
                    bail!("received text exceeds limit");
                }
                Content::Text { text }
            }
            ClipboardContent::Image(image) => {
                let size = std::fs::metadata(&image.path)?.len();
                if size > protocol::WETYPE_MAX_IMAGE_BYTES {
                    bail!("received image exceeds limit");
                }
                let inbox = self.state.join("inbox");
                std::fs::create_dir_all(&inbox)?;
                #[cfg(target_os = "macos")]
                {
                    use std::os::unix::fs::PermissionsExt as _;
                    std::fs::set_permissions(&inbox, std::fs::Permissions::from_mode(0o700))?;
                }
                let path = inbox.join(format!(
                    "{}-{}.image",
                    receipt.id.group(),
                    receipt.id.version()
                ));
                std::fs::copy(image.path, &path)?;
                Content::Image {
                    path: path.to_str().context("inbox path is not UTF-8")?.to_owned(),
                }
            }
        };
        self.pending = Some(receipt.id);
        self.emit(Event::Received {
            from_uin: receipt.source.uin,
            from_name: receipt.source.name,
            content,
        })
    }

    async fn command(&mut self, command: Command) -> anyhow::Result<()> {
        match command {
            Command::Pair { code, device_name } => {
                let result = async {
                    let code = PairingCode::parse(code)?;
                    if self.paired {
                        bail!("already paired; unpair locally first");
                    }
                    if self.client.is_none() {
                        self.open(device_name).await?;
                    }
                    let client = self.client.as_ref().context("WeType client unavailable")?;
                    let mut pairing = client.join(code).await?;
                    let (group, _) = pairing.wait().await?;
                    self.paired = true;
                    self.devices = group
                        .devices
                        .into_iter()
                        .map(|d| WeTypeDevice {
                            uin: d.uin,
                            name: d.name,
                        })
                        .collect();
                    Ok::<_, anyhow::Error>(())
                };
                let result = match tokio::time::timeout(Duration::from_secs(85), result).await {
                    Ok(result) => result,
                    Err(_) => Err(anyhow::anyhow!("WeType pairing timed out")),
                };
                let reconnect = result.as_ref().err().is_some_and(network_error);
                let error = result.err().map(|e| e.to_string());
                self.state(error.clone())?;
                self.emit(Event::PairResult {
                    ok: error.is_none(),
                    error: error.clone(),
                })?;
                if reconnect {
                    bail!(
                        "{}",
                        error.unwrap_or_else(|| "WeType pairing transport failed".into())
                    );
                }
            }
            Command::Unpair => {
                self.shutdown().await;
                // 仅删本扩展的本地身份，不假装 SDK 能从远程组退出。
                if self.state.exists() {
                    std::fs::remove_dir_all(&self.state)?;
                }
                std::fs::create_dir_all(&self.state)?;
                self.paired = false;
                self.self_name.clear();
                self.devices.clear();
                self.state(None)?;
            }
            Command::Acknowledge => {
                self.acknowledge = self.pending.is_some();
            }
            send => {
                let result = async {
                    if !self.paired {
                        bail!("WeType is not paired");
                    }
                    let client = self.client.as_ref().context("WeType client unavailable")?;
                    match send {
                        Command::SendText { text } => {
                            if text.is_empty() || text.len() > protocol::WETYPE_MAX_TEXT_BYTES {
                                bail!("text exceeds limit or is empty");
                            }
                            client.send_text(&text).await?;
                        }
                        Command::SendImage { path } => {
                            validate_image(Path::new(&path))?;
                            client.send_image(&path).await?;
                        }
                        _ => bail!("unexpected send command"),
                    }
                    Ok::<_, anyhow::Error>(())
                }
                .await;
                let error = result.err().map(|e| e.to_string());
                self.emit(Event::SendResult {
                    ok: error.is_none(),
                    error: error.clone(),
                })?;
                if let Some(error) = error {
                    bail!("{error}");
                }
            }
        }
        Ok(())
    }
}

/// 只允许 SDK 支持的 PNG/JPEG；先检查文件大小，不把整图读入内存。
fn validate_image(path: &Path) -> anyhow::Result<()> {
    let meta = std::fs::metadata(path)?;
    if !meta.is_file() || meta.len() == 0 || meta.len() > protocol::WETYPE_MAX_IMAGE_BYTES {
        bail!("image exceeds limit or is not a file");
    }
    let mut header = [0; 8];
    let mut file = std::fs::File::open(path)?;
    let length = file.read(&mut header)?;
    if !(header.starts_with(&[0xff, 0xd8, 0xff]) || length == 8 && header == *b"\x89PNG\r\n\x1a\n")
    {
        bail!("image must be PNG or JPEG");
    }
    Ok(())
}

/// EOF 独立于命令队列通知，配对、上传或重连中的网络等待也能立即取消。
async fn run(state: PathBuf) -> anyhow::Result<()> {
    let (input, mut commands) = mpsc::channel(16);
    let (closed, mut eof) = watch::channel(false);
    std::thread::Builder::new()
        .name("wetype-stdin".into())
        .spawn(move || {
            let mut stdin = std::io::stdin().lock();
            let mut buffer = Vec::new();
            loop {
                match protocol::read_frame_with_limit::<Command>(
                    &mut stdin,
                    &mut buffer,
                    protocol::WETYPE_MAX_FRAME_BYTES,
                ) {
                    // 队列满时阻塞读取，背压沿管道传回宿主；只有主循环退出才会失败。
                    Ok(Some(command)) => {
                        if input.blocking_send(command).is_err() {
                            break;
                        }
                    }
                    Ok(None) => break,
                    Err(err) => {
                        eprintln!("WeType stdin failed: {err}");
                        break;
                    }
                }
            }
            let _ = closed.send(true);
        })?;
    let mut bridge = Bridge {
        state,
        client: None,
        paired: false,
        self_name: String::new(),
        devices: Vec::new(),
        pending: None,
        acknowledge: false,
        writer: std::io::stdout(),
        buffer: Vec::new(),
    };
    let work = async {
        let mut retry = Duration::from_secs(1);
        let mut reconnect_at = None;
        let mut status = tokio::time::interval(Duration::from_secs(30));
        status.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        if bridge.state.join("identity.json").exists() {
            if let Err(err) = bridge.open("KwikPaste".into()).await {
                bridge.state(Some(err.to_string()))?;
                reconnect_at = Some(tokio::time::Instant::now() + retry);
            } else {
                bridge.state(None)?;
            }
        } else {
            bridge.state(None)?;
        }
        loop {
            let result = if let Some(at) = reconnect_at {
                tokio::select! {
                    command = commands.recv() => {
                        let Some(command) = command else { break; };
                        bridge.command(command).await
                    }
                    _ = tokio::time::sleep_until(at) => {
                        let result = if let Some(client) = &bridge.client { client.reconnect().await.map_err(anyhow::Error::from) }
                            else { bridge.open("KwikPaste".into()).await };
                        let result = match result { Ok(()) => bridge.refresh().await, Err(err) => Err(err) };
                        match result {
                            Ok(()) => { bridge.state(None)?; reconnect_at = None; Ok(()) }
                            Err(err) => Err(err),
                        }
                    }
                }
            } else if bridge.acknowledge {
                let client = bridge
                    .client
                    .as_ref()
                    .context("WeType client unavailable")?;
                let id = bridge.pending.clone().context("pending receipt missing")?;
                match client.acknowledge(id).await {
                    Ok(()) => {
                        bridge.pending = None;
                        bridge.acknowledge = false;
                        Ok(())
                    }
                    Err(err) => Err(err.into()),
                }
            } else {
                let client = bridge.client.clone();
                let paired = bridge.paired;
                let pending = bridge.pending.is_some();
                tokio::select! {
                    _ = status.tick(), if client.is_some() => {
                        match bridge.refresh().await {
                            Ok(()) => bridge.state(None),
                            Err(err) => Err(err),
                        }
                    }
                    command = commands.recv() => {
                        let Some(command) = command else { break; };
                        bridge.command(command).await
                    }
                    event = async {
                        if paired && !pending && let Some(client) = &client {
                            return client.next_event().await;
                        }
                        std::future::pending().await
                    } => {
                        match event {
                            Ok(wx_ime_sdk::Event::ClipboardReceived(receipt)) => {
                                retry = Duration::from_secs(1);
                                // 复制缓存失败不能继续 next_event：SDK 已标记本次投递，重启才会重投。
                                bridge.received(receipt)?;
                                Ok(())
                            }
                            Ok(wx_ime_sdk::Event::GroupChanged(group)) => {
                                retry = Duration::from_secs(1);
                                bridge.paired = true;
                                bridge.devices = group.devices.into_iter().map(|d| WeTypeDevice { uin: d.uin, name: d.name }).collect();
                                bridge.state(None)
                            }
                            Err(err) => Err(err.into()),
                        }
                    }
                }
            };
            if let Err(err) = result {
                bridge.state(Some(err.to_string()))?;
                reconnect_at = Some(tokio::time::Instant::now() + retry);
                retry = (retry * 2).min(Duration::from_secs(60));
            } else if bridge.client.is_none() {
                reconnect_at = None;
            }
        }
        Ok::<_, anyhow::Error>(())
    };
    let result = tokio::select! { result = work => result, _ = eof.changed() => Ok(()), _ = parent_gone() => Ok(()) };
    bridge.shutdown().await;
    result
}

fn network_error(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<wx_ime_sdk::Error>()
        .is_some_and(|error| {
            matches!(
                error,
                wx_ime_sdk::Error::Transport
                    | wx_ime_sdk::Error::Http(_)
                    | wx_ime_sdk::Error::CosTransport
                    | wx_ime_sdk::Error::CosHttp(_)
                    | wx_ime_sdk::Error::Timeout(_)
                    | wx_ime_sdk::Error::OutcomeUnknown
            )
        })
}

/// macOS 没有 Job Object，父进程消失时取消网络等待并释放 SDK 身份锁。
#[cfg(target_os = "macos")]
async fn parent_gone() {
    let parent = unsafe { libc::getppid() };
    loop {
        if parent <= 1 || unsafe { libc::getppid() } != parent {
            return;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

#[cfg(target_os = "windows")]
async fn parent_gone() {
    std::future::pending::<()>().await;
}

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args_os().skip(1);
    if args.next().as_deref() != Some(std::ffi::OsStr::new("--state")) {
        bail!("usage: kwikpaste-ext-wetype --state <dir>");
    }
    let state = PathBuf::from(args.next().context("state directory missing")?);
    if args.next().is_some() {
        bail!("unexpected argument");
    }
    std::fs::create_dir_all(&state)?;
    let state = std::fs::canonicalize(state)?;
    #[cfg(target_os = "macos")]
    unsafe {
        libc::setpriority(libc::PRIO_DARWIN_PROCESS, 0, libc::PRIO_DARWIN_BG);
    }
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(run(state))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bridge(state: PathBuf) -> Bridge<Vec<u8>> {
        Bridge {
            state,
            client: None,
            paired: false,
            self_name: String::new(),
            devices: Vec::new(),
            pending: None,
            acknowledge: false,
            writer: Vec::new(),
            buffer: Vec::new(),
        }
    }

    #[test]
    fn sdk_image_cache_is_retained_until_host_acknowledges() -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let state = temp.path().join("state");
        let cache = state.join("cache");
        std::fs::create_dir_all(&cache)?;
        let source = cache.join("original.image");
        let bytes = b"\xff\xd8\xfforiginal-jpeg";
        std::fs::write(&source, bytes)?;
        let receipt: wx_ime_sdk::ReceivedClipboard = serde_json::from_value(serde_json::json!({
            "id":{"group":12,"version":3,"from_uin":42,"trace_id":"test"},
            "source":{"uin":42,"name":"Phone","platform":2,"supports_file_transfer":false},
            "content":{"Image":{"path":source,"bytes":bytes.len(),"sha256":"fixture"}},
            "expire_at":1000
        }))?;
        let mut bridge = bridge(state);
        bridge.received(receipt)?;
        assert!(bridge.pending.is_some());
        assert!(!bridge.acknowledge);
        assert!(source.exists());
        let mut wire = std::io::Cursor::new(&bridge.writer);
        let event = protocol::read_frame_with_limit::<Event>(
            &mut wire,
            &mut Vec::new(),
            protocol::WETYPE_MAX_FRAME_BYTES,
        )?
        .context("receipt event missing")?;
        if let Event::Received {
            from_uin,
            from_name,
            content: Content::Image { path },
        } = event
        {
            assert_eq!(from_uin, 42);
            assert_eq!(from_name, "Phone");
            assert_eq!(std::fs::read(path)?, bytes);
        } else {
            bail!("unexpected fixture event");
        }
        Ok(())
    }

    #[test]
    fn image_validation_accepts_png_jpeg_and_rejects_other_formats_or_oversize()
    -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let path = temp.path().join("image");
        for bytes in [
            b"\x89PNG\r\n\x1a\n".as_slice(),
            b"\xff\xd8\xffjpeg".as_slice(),
        ] {
            std::fs::write(&path, bytes)?;
            validate_image(&path)?;
        }
        std::fs::write(&path, b"GIF89a")?;
        assert!(validate_image(&path).is_err());
        std::fs::OpenOptions::new()
            .write(true)
            .open(&path)?
            .set_len(protocol::WETYPE_MAX_IMAGE_BYTES + 1)?;
        assert!(validate_image(&path).is_err());
        assert!(validate_image(temp.path()).is_err());
        Ok(())
    }

    #[test]
    fn only_network_failures_trigger_pair_reconnection() {
        assert!(network_error(&wx_ime_sdk::Error::Transport.into()));
        assert!(network_error(&wx_ime_sdk::Error::OutcomeUnknown.into()));
        assert!(!network_error(&wx_ime_sdk::Error::CodeUnavailable.into()));
        assert!(!network_error(&anyhow::anyhow!("invalid code")));
    }
}
