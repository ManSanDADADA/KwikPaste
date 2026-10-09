//! OS 级剪贴板监听：把 clipboard-rs 的 watcher 接到「读取 → 去重入库 → 通知宿主」闭环。
//!
//! clipboard-rs 已实现 macOS（`NSPasteboard.changeCount` 轮询）/ Windows
//! （`AddClipboardFormatListener` → `WM_CLIPBOARDUPDATE`）的平台监听，这里不重复造。
//!
//! 线程模型：`ClipboardWatcherContext::start_watch()` 是阻塞调用，故整个监听跑在独立
//! `std::thread` 上。系统剪贴板句柄**在该线程内构造**，不跨线程移动；只有 `Send` 的数据
//! （记录、来源应用）会被投递到 core runtime 做入库与通知。
//!
//! 一次变化的处理拆成可测试的 [`capture_change`]：自动测试用内存剪贴板驱动它，监听线程本身只能真机验证。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use anyhow::anyhow;
use clipboard_rs::{ClipboardHandler, ClipboardWatcher, ClipboardWatcherContext, WatcherShutdown};

use super::apps_registry::materialize_source;
use super::backend::{ClipboardBackend, SystemClipboard};
use super::ingest::build_item_with_settings;
use super::read::ClipboardReader;
use crate::app_ids::contains_app;
use crate::db::models::{ClipboardApp, ClipboardItem};
use crate::error::{AppError, Result};
use crate::root::CoreInner;

/// macOS 轮询 `changeCount` 的间隔。clipboard-rs 默认 500ms，对复制响应（尤其图片）偏慢；
/// 120ms 跟手且 CPU 开销可忽略。Windows 走事件驱动（`WM_CLIPBOARDUPDATE`），此值被忽略。
const CLIPBOARD_POLL_INTERVAL: Duration = Duration::from_millis(120);

/// 同一次复制的重复通知窗口，见 [`RepeatFilter`]。
const REPEAT_WINDOW: Duration = Duration::from_secs(1);

/// 别的剪贴板监听程序可能短暂占着 Windows 剪贴板，读取失败时在有界时间内重试。
const CLIPBOARD_READ_RETRY_DELAYS: [Duration; 3] = [
    Duration::from_millis(15),
    Duration::from_millis(35),
    Duration::from_millis(75),
];

fn read_with_retry<T, E: std::fmt::Display>(
    retry_delays: &[Duration],
    mut read: impl FnMut() -> std::result::Result<Option<T>, E>,
) -> std::result::Result<Option<T>, E> {
    let mut result = read();
    for delay in retry_delays {
        let Err(err) = &result else {
            return result;
        };
        log::debug!("clipboard watcher: read failed ({err}); retrying in {delay:?}");
        std::thread::sleep(*delay);
        result = read();
    }
    result
}

/// 监听暂停开关。切换存储位置、覆盖导入备份期间置位，回调早返回跳过整条入库链路。
/// 不停 watcher 线程本身，避免反复重建平台句柄。
#[derive(Debug, Default, Clone)]
pub struct WatcherPause(Arc<AtomicBool>);

impl WatcherPause {
    pub fn is_paused(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }

    pub fn set_paused(&self, paused: bool) {
        self.0.store(paused, Ordering::Relaxed);
    }

    /// 暂停采集，返回的 guard drop 时恢复成暂停前的状态（宿主原本就暂停着则保持暂停）。
    pub(crate) fn pause_scoped(&self) -> PauseGuard {
        let previous = self.0.swap(true, Ordering::Relaxed);
        PauseGuard {
            pause: self.clone(),
            previous,
        }
    }
}

/// 一次复制可能触发好几次剪贴板通知：.NET 的 `Clipboard.SetText` 先 `OleSetClipboard` 再
/// `OleFlushClipboard`，两次 `WM_CLIPBOARDUPDATE` 相隔十几毫秒；macOS 上两次写入也可能落在两次轮询里。
/// 内容与上一次采集相同、间隔不到窗口的通知算同一次复制，不再入库：否则去重入库会再发一个同步序号、
/// 再放一次提示音、再推一次给同步设备。中间复制过别的内容、或者隔了窗口再复制，都是真的再次复制。
pub(crate) struct RepeatFilter {
    window: Duration,
    last: Option<(String, Instant)>,
}

impl RepeatFilter {
    pub(crate) fn new(window: Duration) -> Self {
        Self { window, last: None }
    }

    /// 记下这次采集的内容；它是上一次采集的重复通知时返回 `true`。连续的重复通知顺延窗口。
    fn is_repeat(&mut self, content_hash: &str, now: Instant) -> bool {
        let repeat = matches!(
            &self.last,
            Some((hash, at)) if hash == content_hash && now.saturating_duration_since(*at) < self.window
        );
        self.last = Some((content_hash.to_owned(), now));
        repeat
    }
}

/// [`WatcherPause::pause_scoped`] 的恢复 guard。
pub(crate) struct PauseGuard {
    pause: WatcherPause,
    previous: bool,
}

impl Drop for PauseGuard {
    fn drop(&mut self) {
        self.pause.set_paused(self.previous);
    }
}

/// 处理一次剪贴板变化的同步部分：识别来源 → 过滤忽略的应用 → 读取（带重试）→ 转成记录
/// （图片在这里落盘）→ 同一次复制的重复通知、自身写回则跳过 → 补齐来源应用。返回 `None` 表示这次不入库。
///
/// **先**抓前台应用：等异步入库再问，前台早就切回别的窗口了。自身写回的事件会在 guard 处丢弃，
/// 但顺序换不得：guard 判定依赖 content_hash，必须先把内容读出来才能判，而读取期间用户可能已经切走前台。
pub(crate) fn capture_change<B: ClipboardBackend>(
    core: &CoreInner,
    reader: &ClipboardReader<B>,
    retry_delays: &[Duration],
    repeats: &mut RepeatFilter,
) -> Option<(ClipboardItem, Option<ClipboardApp>)> {
    if core.watcher_pause.is_paused() {
        return None;
    }

    let source = core.platform().frontmost_app();
    let settings = core.settings.snapshot();

    // 用户在偏好里勾选了「过滤此应用」时整条丢弃，省掉无效的读取与图片解码。按应用比较：
    // 商店、Squirrel 应用升级后路径换了版本号，勾选时存下的旧版本 id 照样命中。
    if let Some(src) = &source {
        if contains_app(&settings.clipboard.filters.excluded_app_ids, &src.id) {
            return None;
        }
    }

    let payload = match read_with_retry(retry_delays, || {
        reader.read_with_capture(&settings.clipboard.capture)
    }) {
        Ok(Some(payload)) => payload,
        Ok(None) => return None,
        Err(err) => {
            log::warn!("clipboard watcher: read failed: {err}");
            return None;
        }
    };

    let mut item = match build_item_with_settings(
        &core.images,
        &payload,
        &settings.clipboard.capture,
        &settings.clipboard.sensitive,
        settings.clipboard.content.copy_plain,
    ) {
        Ok(Some(item)) => item,
        Ok(None) => return None,
        Err(err) => {
            log::warn!("clipboard watcher: build item failed: {err}");
            return None;
        }
    };

    // 同一次复制的重复通知、自身写回触发的变更（避免回环）都跳过入库。两样都要判：
    // 刚复制过的内容马上被写回时，写回的通知也是「重复」，但仍要消费掉写回登记，
    // 不然它会吞掉之后一次真的复制。
    let repeat = repeats.is_repeat(&item.content_hash, Instant::now());
    let own_writeback = core.guard.should_skip(&item.content_hash)
        || match &payload {
            super::payload::ClipboardPayload::Image(image) => {
                core.guard.should_skip_image(&image.bytes)
            }
            _ => false,
        };
    if repeat || own_writeback {
        return None;
    }

    let source_app = source.map(|src| materialize_source(&core.app_icons, Some(&core.apps), src));
    if let Some(src) = &source_app {
        item.source_app_id = Some(src.id.clone());
    }

    Some((item, source_app))
}

/// 把 [`capture_change`] 的结果交给 core runtime 入库并通知宿主；失败只记日志（监听场景无人接收结果）。
fn persist_captured(core: Arc<CoreInner>, item: ClipboardItem, source_app: Option<ClipboardApp>) {
    let rt = core.rt.clone();
    rt.spawn(async move {
        if let Err(err) =
            super::persist::persist_and_notify(&core, &item, source_app.as_ref()).await
        {
            log::error!("clipboard watcher: persist failed: {err}");
        }
    });
}

/// 在独立线程上启动 OS 级监听，返回停止句柄：句柄被丢弃时监听线程退出。
/// 平台句柄创建失败时返回错误，不会留下半启动的线程。
pub(crate) fn spawn(core: &Arc<CoreInner>) -> Result<WatcherShutdown> {
    let weak = Arc::downgrade(core);
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();

    std::thread::Builder::new()
        .name("clipboard-watcher".to_owned())
        .spawn(move || {
            // 平台剪贴板句柄在本线程内构造，不跨线程移动。
            let reader = match SystemClipboard::new() {
                Ok(backend) => ClipboardReader::with_backend(backend),
                Err(err) => {
                    let _ = ready_tx.send(Err(err));
                    return;
                }
            };
            let mut watcher =
                match ClipboardWatcherContext::new_with_interval(CLIPBOARD_POLL_INTERVAL) {
                    Ok(watcher) => watcher,
                    Err(err) => {
                        let _ = ready_tx.send(Err(AppError::Clipboard(err.to_string())));
                        return;
                    }
                };

            watcher.add_handler(ClipboardChangeHandler {
                reader,
                core: weak,
                repeats: RepeatFilter::new(REPEAT_WINDOW),
            });
            if ready_tx.send(Ok(watcher.get_shutdown_channel())).is_err() {
                return;
            }

            log::info!("clipboard watcher started");
            // 阻塞直至停止句柄被丢弃。
            watcher.start_watch();
            log::info!("clipboard watcher stopped");
        })
        .map_err(|err| AppError::Other(anyhow!("failed to spawn clipboard watcher: {err}")))?;

    ready_rx
        .recv()
        .map_err(|_| AppError::Other(anyhow!("clipboard watcher exited before starting")))?
}

struct ClipboardChangeHandler {
    reader: ClipboardReader<SystemClipboard>,
    core: Weak<CoreInner>,
    repeats: RepeatFilter,
}

impl ClipboardHandler for ClipboardChangeHandler {
    fn on_clipboard_change(&mut self) {
        let Some(core) = self.core.upgrade() else {
            return;
        };

        if let Some((item, source_app)) = capture_change(
            &core,
            &self.reader,
            &CLIPBOARD_READ_RETRY_DELAYS,
            &mut self.repeats,
        ) {
            persist_captured(core, item, source_app);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;

    const ZERO_DELAY_RETRIES: [Duration; 3] = [Duration::ZERO; 3];

    #[test]
    fn reencoded_image_writeback_is_skipped_once_without_changing_original() {
        use super::super::backend::{MemoryClipboard, MemoryState};
        use super::super::payload::{ClipboardPayload, ImagePayload};
        use image::ImageEncoder;

        let fixture = crate::testing::Fixture::new();
        let core = fixture.start();
        let original = crate::testing::sample_png(64, 48);
        let pixels = image::load_from_memory(&original).unwrap().into_rgba8();
        let mut reencoded = Vec::new();
        image::codecs::png::PngEncoder::new_with_quality(
            &mut reencoded,
            image::codecs::png::CompressionType::Best,
            image::codecs::png::FilterType::NoFilter,
        )
        .write_image(pixels.as_raw(), 64, 48, image::ExtendedColorType::Rgba8)
        .unwrap();
        assert_ne!(original, reencoded);
        let payload = ClipboardPayload::Image(ImagePayload {
            bytes: original.clone(),
            width: 64,
            height: 48,
        });
        let item = super::super::ingest::build_item(&core.0.images, &payload)
            .unwrap()
            .unwrap();
        let original_hash = item.content_hash.clone();
        let clipboard = MemoryClipboard::new();
        super::super::write::write_to_clipboard(
            &clipboard,
            &core.0.images,
            &core.0.guard,
            &item,
            false,
        )
        .unwrap();
        assert_eq!(clipboard.snapshot().png, Some(original.clone()));

        // TIFF/DIB 回退已经转成 PNG，使用重编码 PNG 模拟这种 OS 回显。
        let reader = ClipboardReader::with_backend(MemoryClipboard::with_state(MemoryState {
            png: Some(reencoded),
            ..MemoryState::default()
        }));
        let mut repeats = RepeatFilter::new(Duration::ZERO);
        assert!(capture_change(&core.0, &reader, &[], &mut repeats).is_none());
        assert!(!core.0.guard.should_skip(&original_hash));

        let (genuine, _) = capture_change(&core.0, &reader, &[], &mut repeats).unwrap();
        assert_ne!(genuine.content_hash, original_hash);
        assert_eq!(item.content_hash, original_hash);
        assert_eq!(
            std::fs::read(core.0.images.origin_path(&item.content)).unwrap(),
            original
        );
    }

    #[test]
    fn repeat_filter_merges_notifications_of_one_copy() {
        let mut repeats = RepeatFilter::new(Duration::from_secs(1));
        let start = Instant::now();

        assert!(!repeats.is_repeat("a", start));
        assert!(repeats.is_repeat("a", start + Duration::from_millis(17)));
        // 连续的重复通知顺延窗口。
        assert!(repeats.is_repeat("a", start + Duration::from_millis(900)));
        assert!(repeats.is_repeat("a", start + Duration::from_millis(1800)));
        // 隔了窗口、或者中间复制过别的内容，都是真的再次复制。
        assert!(!repeats.is_repeat("a", start + Duration::from_millis(2800)));
        assert!(!repeats.is_repeat("b", start + Duration::from_millis(2810)));
        assert!(!repeats.is_repeat("a", start + Duration::from_millis(2820)));
    }

    #[test]
    fn clipboard_read_retry_returns_immediate_success() {
        let attempts = Cell::new(0);

        let result = read_with_retry(&ZERO_DELAY_RETRIES, || {
            attempts.set(attempts.get() + 1);
            Ok::<_, &'static str>(Some("captured"))
        });

        assert_eq!(result, Ok(Some("captured")));
        assert_eq!(attempts.get(), 1);
    }

    #[test]
    fn clipboard_read_retry_recovers_after_transient_error() {
        let attempts = Cell::new(0);

        let result = read_with_retry(&ZERO_DELAY_RETRIES, || {
            attempts.set(attempts.get() + 1);
            if attempts.get() == 1 {
                Err("clipboard busy")
            } else {
                Ok(Some("captured"))
            }
        });

        assert_eq!(result, Ok(Some("captured")));
        assert_eq!(attempts.get(), 2);
    }

    #[test]
    fn clipboard_read_retry_does_not_retry_empty_content() {
        let attempts = Cell::new(0);

        let result = read_with_retry(&ZERO_DELAY_RETRIES, || {
            attempts.set(attempts.get() + 1);
            Ok::<Option<&'static str>, &'static str>(None)
        });

        assert_eq!(result, Ok(None));
        assert_eq!(attempts.get(), 1);
    }

    #[test]
    fn clipboard_read_retry_returns_final_error_after_exhaustion() {
        let attempts = Cell::new(0);

        let result = read_with_retry(&ZERO_DELAY_RETRIES, || {
            attempts.set(attempts.get() + 1);
            Err::<Option<&'static str>, _>(attempts.get())
        });

        assert_eq!(result, Err(4));
        assert_eq!(attempts.get(), 4);
    }
}
