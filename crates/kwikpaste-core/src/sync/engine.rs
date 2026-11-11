//! 记录与线上格式互转、按水位线补齐，以及收到记录后的入库 / 写剪贴板。
//!
//! 收到的内容先还原成剪贴板载荷，再走本机采集同一条 `build_item_with_settings`：
//! 接收方自己的采集类型、大小上限和敏感内容规则照样生效，去重指纹也与本机复制一致。

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context};
use chrono::Utc;

use super::protocol::{Message, WireItem, MAX_HEADER_BYTES};
use crate::clipboard::{
    build_item_with_settings, png_dimensions, write_to_clipboard, ClipboardPayload, ImagePayload,
    TextPayload,
};
use crate::db::models::{ClipboardItem, ClipboardKind, ClipboardSubKind, Platform};
use crate::root::CoreInner;
use crate::settings::LanSync;

/// 补齐最多回看多久、总共最多补多少条、每页多少条。
const CATCH_UP_DAYS: i64 = 7;
const CATCH_UP_TOTAL: i64 = 500;
const CATCH_UP_PAGE: i64 = 50;
/// 最近一次同步过的内容在这段时间内视为回声。
const ECHO_WINDOW: Duration = Duration::from_secs(60);

/// 回声过滤：记住最近一次同步过（发出或实时收到）的内容。
///
/// 和它相同的内容既不再发出，也不再写回剪贴板。一次复制触发好几次剪贴板事件、
/// 本机写回又被监听到、远程桌面 / 虚拟机的剪贴板共享把内容带回来，都会产生这种回声；
/// 只按时间窗口去重挡不住延迟较长的共享，两台设备会一直互相写。
/// 期间同步过别的内容就不再算回声，再复制回原来的内容照常同步。
#[derive(Default)]
pub struct EchoFilter {
    last: Option<(String, Instant)>,
}

impl EchoFilter {
    /// 本机这次复制要不要发出去；要发时记为最近同步的内容。
    pub fn should_send(&mut self, content_hash: &str) -> bool {
        if self.is_echo(content_hash) {
            return false;
        }

        self.remember(content_hash);
        true
    }

    /// 实时收到一条记录并记为最近同步的内容；返回要不要写入剪贴板。
    pub fn accept_received(&mut self, content_hash: &str) -> bool {
        let fresh = !self.is_echo(content_hash);
        self.remember(content_hash);
        fresh
    }

    pub(crate) fn is_echo(&self, content_hash: &str) -> bool {
        self.last
            .as_ref()
            .is_some_and(|(hash, at)| hash == content_hash && at.elapsed() <= ECHO_WINDOW)
    }

    pub(crate) fn remember(&mut self, content_hash: &str) {
        self.last = Some((content_hash.to_owned(), Instant::now()));
    }

    /// 测试模拟官方客户端数秒后写回，不实际等待或依赖系统时钟。
    #[cfg(test)]
    pub(crate) fn remember_elapsed_for_test(&mut self, hash: &str, elapsed: Duration) {
        self.last = Instant::now()
            .checked_sub(elapsed)
            .map(|at| (hash.to_owned(), at));
    }
}

/// 这条记录按当前设置是否参与同步。文件和敏感内容不同步。
pub fn is_syncable(item: &ClipboardItem, lan: &LanSync) -> bool {
    is_syncable_types(item, lan.text, lan.image)
}

/// 各传输共用的类型与敏感内容规则，不依赖 LAN 设置模型。
pub(crate) fn is_syncable_types(item: &ClipboardItem, text: bool, image: bool) -> bool {
    if item.is_sensitive {
        return false;
    }

    match item.kind {
        ClipboardKind::Text => text,
        ClipboardKind::Image => image,
        ClipboardKind::Files => false,
    }
}

/// 把本机记录转成线上格式；图片读原图作为附件。附件超过 `attachment_limit`（本机设置与
/// 对方申报的上限中较小的那个）或文本超过 JSON 头上限时返回 `None`。
pub async fn wire_from_item(
    core: &CoreInner,
    item: &ClipboardItem,
    attachment_limit: u64,
) -> anyhow::Result<Option<(WireItem, Vec<u8>)>> {
    match item.kind {
        ClipboardKind::Text => {
            // 接收方（含已发布的各版本）纯文本只用 content、摘要自己重算：
            // 只有富文本带纯文本表示，摘要不发，可同步的文本不再被重复字段压掉一半。
            let rich = matches!(
                item.sub_kind,
                Some(ClipboardSubKind::Html | ClipboardSubKind::Rtf)
            );
            let wire = WireItem {
                kind: item.kind,
                sub_kind: item.sub_kind,
                content: item.content.clone(),
                search_text: if rich { item.search_text.clone() } else { None },
                summary: None,
                width: None,
                height: None,
            };
            // false 比 true 多一个字节，按补齐的头大小检查也覆盖实时推送。
            let header_bytes = serde_json::to_vec(&Message::Push {
                item: wire.clone(),
                live: false,
            })?
            .len();
            if header_bytes > MAX_HEADER_BYTES {
                log::info!("lan sync skips text with a header of {header_bytes} bytes over the message limit");
                return Ok(None);
            }
            Ok(Some((wire, Vec::new())))
        }
        ClipboardKind::Image => {
            if crate::clipboard::validate_image_file_name(&item.content).is_err() {
                log::warn!("lan sync skips an image record with an invalid file name");
                return Ok(None);
            }
            let path = core.images.origin_path(&item.content);
            let bytes = match tokio::task::spawn_blocking(move || std::fs::read(&path))
                .await
                .context("image read task failed")?
            {
                Ok(bytes) => bytes,
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                    log::warn!("lan sync skips missing image {}", item.content);
                    return Ok(None);
                }
                Err(err) => return Err(err).context("failed to read image for sync"),
            };
            if bytes.len() as u64 > attachment_limit {
                log::info!(
                    "lan sync skips image of {} bytes over the size limit",
                    bytes.len()
                );
                return Ok(None);
            }

            Ok(Some((
                WireItem {
                    kind: item.kind,
                    sub_kind: None,
                    content: String::new(),
                    search_text: None,
                    summary: None,
                    width: item.width,
                    height: item.height,
                },
                bytes,
            )))
        }
        ClipboardKind::Files => Ok(None),
    }
}

/// 一页补齐。
pub struct CatchUpPage {
    /// 参与同步的记录。图片的原图发送时才逐张读：一页最多 50 张，不能一起读进内存。
    pub items: Vec<ClipboardItem>,
    /// 附件上限：本机设置与对方申报的上限中较小的那个。
    pub attachment_limit: u64,
    /// 这一页覆盖到的本机序号：对方收齐这页后把水位线推进到这里。
    pub until: u64,
    pub more: bool,
    /// 本机序号的代次。
    pub epoch: String,
}

/// 对方请求本机序号大于 `after` 的记录：最近 7 天、最新的 500 条以内，按序号从小到大分页。
/// 对方的水位线属于另一代序号（本机数据库重建过）或还没有水位线时从头发。
/// 最后一页的 `until` 是本机当前的计数器，不参与同步的记录（敏感、文件）也一并跳过。
pub async fn collect_catch_up(
    core: &CoreInner,
    after: u64,
    epoch: Option<&str>,
    attachment_limit: u64,
) -> anyhow::Result<CatchUpPage> {
    let lan = core.settings.snapshot().sync.lan;
    let pool = core.db.pool().await;
    let own_epoch = crate::db::sync::sync_epoch(&pool).await?;
    let after = if epoch == Some(own_epoch.as_str()) {
        after
    } else {
        0
    };
    // 先读计数器再取这一页：取页期间新复制的记录序号比它大，留到下一次补齐，不会被跳过。
    let counter = crate::db::sync::sync_counter(&pool).await?.max(0) as u64;
    let cutoff = Utc::now() - chrono::Duration::days(CATCH_UP_DAYS);
    let after_signed = i64::try_from(after).unwrap_or(i64::MAX);
    let (rows, more) =
        crate::db::sync::catch_up_page(&pool, after_signed, cutoff, CATCH_UP_TOTAL, CATCH_UP_PAGE)
            .await?;

    let last = rows.last().map_or(after, |(_, seq)| (*seq).max(0) as u64);
    let until = if more { last } else { counter.max(last) };
    let mut items = Vec::with_capacity(rows.len());
    for (id, _) in rows {
        let Some(item) = crate::db::items::find_item_by_id(&pool, &id).await? else {
            continue;
        };
        if is_syncable(&item, &lan) {
            items.push(item);
        }
    }

    Ok(CatchUpPage {
        items,
        attachment_limit: attachment_limit.min(lan.max_image_bytes()),
        until,
        more,
        epoch: own_epoch,
    })
}

/// 入库一条收到的记录。`live` 且设置允许时写入系统剪贴板；补齐的历史只在本机还没有这条内容时
/// 插入，已有的不动（不刷新时间、不增加使用次数），重连后重复补到的记录不会把列表顺序打乱。
pub async fn receive(
    core: &Arc<CoreInner>,
    echo: &std::sync::Mutex<EchoFilter>,
    peer_id: &str,
    peer_platform: Platform,
    wire: WireItem,
    attachment: Vec<u8>,
    live: bool,
) -> anyhow::Result<()> {
    let lan = core.settings.snapshot().sync.lan;
    receive_with_options(
        core,
        echo,
        peer_id,
        peer_platform,
        wire,
        attachment,
        live,
        ReceiveOptions {
            text: lan.text,
            image: lan.image,
            write_clipboard: lan.write_clipboard,
            max_image_bytes: lan.max_image_bytes(),
            allow_jpeg: false,
        },
    )
    .await
}

/// 接收策略独立于传输；LAN 与 WeType 共用构建、入库、回声和剪贴板写回。
#[derive(Clone, Copy)]
pub(crate) struct ReceiveOptions {
    pub text: bool,
    pub image: bool,
    pub write_clipboard: bool,
    pub max_image_bytes: u64,
    pub allow_jpeg: bool,
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn receive_with_options(
    core: &Arc<CoreInner>,
    echo: &std::sync::Mutex<EchoFilter>,
    peer_id: &str,
    peer_platform: Platform,
    wire: WireItem,
    attachment: Vec<u8>,
    live: bool,
    options: ReceiveOptions,
) -> anyhow::Result<()> {
    let settings = core.settings.snapshot();
    let payload = match wire.kind {
        ClipboardKind::Text if options.text => match text_payload(&wire) {
            Ok(payload) => payload,
            Err(err) => {
                log::warn!("lan sync skips invalid text: {err:#}");
                return Ok(());
            }
        },
        ClipboardKind::Image if options.image => {
            if attachment.len() as u64 > options.max_image_bytes {
                log::info!("lan sync drops image over the size limit");
                return Ok(());
            }
            let allow_jpeg = options.allow_jpeg;
            match tokio::task::spawn_blocking(move || received_image(attachment, allow_jpeg))
                .await
                .context("received image decode task failed")?
            {
                Ok(image) => ClipboardPayload::Image(image),
                Err(err) => {
                    log::warn!("lan sync skips invalid image: {err:#}");
                    return Ok(());
                }
            }
        }
        _ => return Ok(()),
    };

    let publication = core.cleanup.image_publication().await;
    let capture = settings.clipboard.capture.clone();
    let sensitive = settings.clipboard.sensitive;
    let plain_only = settings.clipboard.content.copy_plain;
    let built = {
        let core = core.clone();
        tokio::task::spawn_blocking(move || {
            build_item_with_settings(
                &core.images,
                &payload,
                &capture,
                &sensitive,
                plain_only,
                settings.clipboard.compiled_rules(),
            )
        })
        .await
        .context("build received item task failed")??
    };
    let Some(mut item) = built else {
        return Ok(());
    };
    item.origin_device_id = Some(peer_id.to_owned());
    item.platform = peer_platform;

    if !live {
        let pool = core.db.pool().await;
        if crate::db::items::find_item_by_content_hash(&pool, &item.content_hash)
            .await?
            .is_some()
        {
            return Ok(());
        }
    }

    // 补齐的是历史记录，不代表对方剪贴板的现状，不参与回声判断，也不写剪贴板。
    // 实时记录先记回声再写剪贴板：写回被本机监听到时要能认出来。
    crate::clipboard::persist::store_and_emit(core, &item).await?;
    drop(publication);

    let fresh = live
        && echo
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .accept_received(&item.content_hash);

    let write_clipboard = fresh && options.write_clipboard;

    if write_clipboard {
        let core = core.clone();
        tokio::task::spawn_blocking(move || {
            let written = core.clipboard().and_then(|backend| {
                write_to_clipboard(backend.as_ref(), &core.images, &core.guard, &item, false)
            });
            if let Err(err) = written {
                log::warn!("lan sync write clipboard failed: {err}");
            }
        })
        .await
        .context("write received clipboard task failed")?;
    }

    Ok(())
}

/// PNG 保留原始字节；WeType 的 JPEG 受统一解码上限保护后转为本机 PNG 存储。
fn received_image(bytes: Vec<u8>, allow_jpeg: bool) -> anyhow::Result<ImagePayload> {
    if let Some((width, height)) = png_dimensions(&bytes) {
        crate::imaging::check_rgba_size(width, height)?;
        return Ok(ImagePayload {
            bytes,
            width,
            height,
        });
    }
    if !allow_jpeg || image::guess_format(&bytes)? != image::ImageFormat::Jpeg {
        return Err(anyhow!("received image must be PNG or JPEG"));
    }
    let image = crate::imaging::from_bytes(&bytes)?.decode()?;
    let (width, height) = (image.width(), image.height());
    let mut png = std::io::Cursor::new(Vec::new());
    image.write_to(&mut png, image::ImageFormat::Png)?;
    Ok(ImagePayload {
        bytes: png.into_inner(),
        width,
        height,
    })
}

/// 文本记录还原成剪贴板文本载荷：富文本带上 OS 同时提供的纯文本表示。
fn text_payload(wire: &WireItem) -> anyhow::Result<ClipboardPayload> {
    let rich = matches!(
        wire.sub_kind,
        Some(ClipboardSubKind::Html) | Some(ClipboardSubKind::Rtf)
    );
    let text = if rich {
        wire.search_text.clone().unwrap_or_default()
    } else {
        wire.content.clone()
    };
    if text.trim().is_empty() {
        return Err(anyhow!("received text is empty"));
    }

    Ok(ClipboardPayload::Text(TextPayload {
        text,
        html: (wire.sub_kind == Some(ClipboardSubKind::Html)).then(|| wire.content.clone()),
        rtf: (wire.sub_kind == Some(ClipboardSubKind::Rtf)).then(|| wire.content.clone()),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{block_on, sample_png, Fixture};

    #[test]
    fn receive_waits_for_clipboard_writeback() {
        use crate::clipboard::{ClipboardBackend, ClipboardProvider, MemoryClipboard};

        struct DelayedClipboard {
            clipboard: MemoryClipboard,
            started: std::sync::mpsc::Sender<()>,
            release: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
        }

        impl ClipboardProvider for DelayedClipboard {
            fn open(&self) -> crate::error::Result<Box<dyn ClipboardBackend>> {
                self.started.send(()).unwrap();
                self.release.lock().unwrap().recv().unwrap();
                Ok(Box::new(self.clipboard.clone()))
            }
        }

        let fixture = Fixture::new();
        let core = fixture.start();
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        core.set_clipboard_provider(Arc::new(DelayedClipboard {
            clipboard: fixture.clipboard.clone(),
            started: started_tx,
            release: std::sync::Mutex::new(release_rx),
        }));
        let receive = core.0.rt.spawn({
            let core = core.clone();
            async move {
                let mut image = wire(None, "", None);
                image.kind = ClipboardKind::Image;
                receive(
                    &core.0,
                    &std::sync::Mutex::new(EchoFilter::default()),
                    "peer",
                    Platform::Windows,
                    image,
                    sample_png(2, 2),
                    true,
                )
                .await
            }
        });
        started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        std::thread::sleep(Duration::from_millis(50));
        let returned_early = receive.is_finished();
        release_tx.send(()).unwrap();
        block_on(receive).unwrap().unwrap();
        assert!(!returned_early);
        assert_eq!(fixture.clipboard.snapshot().png, Some(sample_png(2, 2)));
        block_on(core.shutdown()).unwrap();
    }

    #[test]
    fn catch_up_skips_missing_images_but_retries_other_read_failures() {
        let fixture = Fixture::new();
        let core = fixture.start();
        let bytes = sample_png(2, 2);
        let item = core
            .build_item(&ClipboardPayload::Image(ImagePayload {
                bytes: bytes.clone(),
                width: 2,
                height: 2,
            }))
            .unwrap()
            .unwrap();
        let path = core.image_origin_path(&item.content).unwrap();
        block_on(core.store_item(item.clone(), None)).unwrap();
        std::fs::remove_file(&path).unwrap();

        block_on(core.hop({
            let core = core.clone();
            async move {
                assert!(wire_from_item(&core.0, &item, u64::MAX)
                    .await
                    .unwrap()
                    .is_none());
                let page = collect_catch_up(&core.0, 0, None, u64::MAX).await.unwrap();
                assert_eq!(page.items.len(), 1);
                assert_eq!(page.until, 1);

                std::fs::create_dir(&path).unwrap();
                assert!(wire_from_item(&core.0, &item, u64::MAX).await.is_err());
                std::fs::remove_dir(&path).unwrap();
                std::fs::write(path, bytes).unwrap();
                assert!(wire_from_item(&core.0, &item, u64::MAX)
                    .await
                    .unwrap()
                    .is_some());
                Ok(())
            }
        }))
        .unwrap();
        block_on(core.shutdown()).unwrap();
    }

    #[test]
    fn invalid_received_payloads_are_skipped() {
        let fixture = Fixture::new();
        let core = fixture.start();
        block_on(core.hop({
            let core = core.clone();
            async move {
                let mut image = wire(None, "", None);
                image.kind = ClipboardKind::Image;
                let echo = std::sync::Mutex::new(EchoFilter::default());
                for (item, attachment) in [
                    (wire(None, "", None), Vec::new()),
                    (
                        wire(Some(ClipboardSubKind::Html), "<b>hi</b>", None),
                        Vec::new(),
                    ),
                    (image, vec![0; 4]),
                ] {
                    receive_with_options(
                        &core.0,
                        &echo,
                        "peer",
                        Platform::Windows,
                        item,
                        attachment,
                        false,
                        ReceiveOptions {
                            text: true,
                            image: true,
                            write_clipboard: false,
                            max_image_bytes: u64::MAX,
                            allow_jpeg: true,
                        },
                    )
                    .await
                    .unwrap();
                }
                let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM clipboard_items")
                    .fetch_one(&core.0.db.pool().await)
                    .await
                    .unwrap();
                assert_eq!(count, 0);
                Ok(())
            }
        }))
        .unwrap();
        block_on(core.shutdown()).unwrap();
    }

    #[test]
    fn escaped_text_over_header_limit_is_skipped_with_its_checkpoint() {
        let fixture = Fixture::new();
        let core = fixture.start();
        let mut item = core
            .build_item(&ClipboardPayload::Text(TextPayload {
                text: "\\".to_owned(),
                html: None,
                rtf: None,
            }))
            .unwrap()
            .unwrap();
        // 转义后翻倍，超过 JSON 头上限。
        item.content = "\\".repeat(9 * 1024 * 1024);
        block_on(core.store_item(item.clone(), None)).unwrap();

        block_on(core.hop({
            let core = core.clone();
            async move {
                assert!(wire_from_item(&core.0, &item, 0).await.unwrap().is_none());
                let page = collect_catch_up(&core.0, 0, None, 0).await.unwrap();
                assert_eq!(page.until, 1);

                let mut plain = item;
                plain.content = "a".repeat(4 * 1024 * 1024);
                plain.search_text = Some(plain.content.clone());
                let (wire, attachment) = wire_from_item(&core.0, &plain, 0).await.unwrap().unwrap();
                assert!(wire.search_text.is_none() && wire.summary.is_none());
                for live in [true, false] {
                    assert!(super::super::protocol::encode(
                        &Message::Push {
                            item: wire.clone(),
                            live,
                        },
                        &attachment
                    )
                    .is_ok());
                }
                Ok(())
            }
        }))
        .unwrap();
        block_on(core.shutdown()).unwrap();
    }

    fn wire(sub_kind: Option<ClipboardSubKind>, content: &str, search: Option<&str>) -> WireItem {
        WireItem {
            kind: ClipboardKind::Text,
            sub_kind,
            content: content.to_owned(),
            search_text: search.map(str::to_owned),
            summary: None,
            width: None,
            height: None,
        }
    }

    #[test]
    fn html_keeps_source_and_plain_text() {
        let payload =
            text_payload(&wire(Some(ClipboardSubKind::Html), "<b>hi</b>", Some("hi"))).unwrap();

        let ClipboardPayload::Text(text) = payload else {
            panic!("expected text payload");
        };
        assert_eq!(text.text, "hi");
        assert_eq!(text.html.as_deref(), Some("<b>hi</b>"));
        assert!(text.rtf.is_none());
    }

    #[test]
    fn plain_text_uses_content() {
        let payload =
            text_payload(&wire(Some(ClipboardSubKind::Url), "https://a.b", None)).unwrap();

        let ClipboardPayload::Text(text) = payload else {
            panic!("expected text payload");
        };
        assert_eq!(text.text, "https://a.b");
        assert!(text.html.is_none());
    }

    #[test]
    fn empty_text_is_rejected() {
        assert!(text_payload(&wire(None, "   ", None)).is_err());
    }

    #[test]
    fn received_content_is_not_sent_back() {
        let mut filter = EchoFilter::default();

        assert!(filter.accept_received("a"));
        assert!(!filter.should_send("a"));
    }

    #[test]
    fn repeated_events_for_one_copy_send_once() {
        let mut filter = EchoFilter::default();

        assert!(filter.should_send("a"));
        assert!(!filter.should_send("a"));
    }

    #[test]
    fn content_echoed_back_is_not_written_again() {
        let mut filter = EchoFilter::default();
        filter.should_send("a");

        assert!(!filter.accept_received("a"));
    }

    #[test]
    fn copying_back_after_other_content_syncs_again() {
        let mut filter = EchoFilter::default();
        filter.should_send("a");

        assert!(filter.accept_received("b"));
        assert!(filter.should_send("a"));
    }
}
