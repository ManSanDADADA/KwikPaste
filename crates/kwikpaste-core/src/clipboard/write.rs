//! 剪贴板写回：把 [`ClipboardItem`] 按类型写回系统剪贴板（text / html / rtf / image / files）。
//!
//! 时序约束：[`SystemClipboard`](super::SystemClipboard) 是 `!Send`，调用方需在不跨 await
//! 的同步段内创建后端并完成调用。
//!
//! 回环抑制：写回前向 [`WritebackGuard`] 登记将写入内容的 `content_hash`，
//! OS 监听重新读到同内容时跳过入库，避免「点击粘贴 → 自动新增一条」回环。
//! 哈希必须与 [`crate::clipboard::build_item_with_settings`] 在 watcher 路径上将算出的哈希一致：
//! - text / html / rtf：watcher 拿到的 plain/html/rtf 经 `draft_from_text` 后 `content` 即我们写入的串，
//!   `content_hash(Text, written)` 自然匹配；
//! - files：watcher 把路径列表用 `\n` 连接后哈希，与我们 `item.content` 一致；
//! - image：watcher 走 PNG 直通读回原始字节 → 文件名 → 哈希，所以写回必须把落盘的 PNG
//!   原样放上剪贴板（见 [`ClipboardBackend::set_png`]）。重新编码会让浏览器等来源的图片读回后哈希对不上，
//!   粘贴一次多一条。
//!   当 OS 只回显重编码后的 PNG 时，以短期尺寸/RGBA 指纹回退识别；原图与历史哈希保持不变。
//!
//! 纯文本模式（`plain = true`）：忽略 `sub_kind`，写 `search_text`（OS 提供的纯文本表示），
//! 缺失时退回 `content`。供「纯文本粘贴」快捷路径使用。

use super::backend::{ClipboardBackend, ClipboardWrite};
use super::guard::WritebackGuard;
use super::storage::ImageStore;
use crate::db::items::content_hash;
use crate::db::models::{ClipboardItem, ClipboardKind, ClipboardSubKind};
use crate::error::{AppError, Result};

/// 把 `item` 写回剪贴板；`plain = true` 强制只写纯文本（剥离 HTML/RTF）。
pub fn write_to_clipboard(
    backend: &dyn ClipboardBackend,
    store: &ImageStore,
    guard: &WritebackGuard,
    item: &ClipboardItem,
    plain: bool,
) -> Result<()> {
    match item.kind {
        ClipboardKind::Text => write_text(backend, guard, item, plain)?,
        ClipboardKind::Image => write_image(backend, store, guard, item)?,
        // files + plain：把路径列表当文本写回，供「粘贴为路径」使用。
        ClipboardKind::Files if plain => write_files_as_text(backend, guard, item)?,
        ClipboardKind::Files => write_files(backend, guard, item)?,
    }
    Ok(())
}

/// 把从历史记录里取出的一段纯文本（快捷信息 / 拆词选区）写回剪贴板。
/// 同样登记回环抑制：片段只是这次要粘贴的内容，不另外记成一条新历史。
pub fn write_text_fragment(
    backend: &dyn ClipboardBackend,
    guard: &WritebackGuard,
    text: &str,
) -> Result<()> {
    guard.suppress(content_hash(ClipboardKind::Text, text));
    backend.set(vec![ClipboardWrite::Text(text.to_owned())])
}

fn write_text(
    backend: &dyn ClipboardBackend,
    guard: &WritebackGuard,
    item: &ClipboardItem,
    plain: bool,
) -> Result<()> {
    // 纯文本模式下，OS 提供的 plain 表示优先；缺失时退回 content（plain 文本场景下 content 即纯文本）。
    let (content, sub_kind) = if plain {
        let text = item
            .search_text
            .clone()
            .unwrap_or_else(|| item.content.clone());
        (text, None)
    } else {
        (item.content.clone(), item.sub_kind)
    };

    guard.suppress(content_hash(ClipboardKind::Text, &content));

    match sub_kind {
        // 纯文本模式必须只写 Text flavor，确保清掉剪贴板中可能残留的 HTML/RTF。
        None if plain => backend.set(vec![ClipboardWrite::Text(content)])?,
        // HTML / RTF 必须同时写入纯文本回退：clipboard-rs 的 set_html / set_rich_text
        // 会先 clearContents，单独写时只剩富格式，多数应用读 plain/text 拿不到就拒绝粘贴。
        // 走 set(Vec<..>) 一次写多格式（内部不再相互清空）。
        Some(ClipboardSubKind::Html) => {
            let plain = item.search_text.clone().unwrap_or_else(|| content.clone());
            guard.suppress(content_hash(ClipboardKind::Text, &plain));
            backend.set(vec![
                ClipboardWrite::Text(plain),
                ClipboardWrite::Html(content),
            ])?;
        }
        Some(ClipboardSubKind::Rtf) => {
            let plain = item.search_text.clone().unwrap_or_else(|| content.clone());
            guard.suppress(content_hash(ClipboardKind::Text, &plain));
            backend.set(vec![
                ClipboardWrite::Text(plain),
                ClipboardWrite::Rtf(content),
            ])?;
        }
        // url / email / color / path 及无 sub_kind 都走纯文本通道。
        _ => backend.set_text(content)?,
    }
    Ok(())
}

fn write_image(
    backend: &dyn ClipboardBackend,
    store: &ImageStore,
    guard: &WritebackGuard,
    item: &ClipboardItem,
) -> Result<()> {
    let path = store.origin_path(&item.content);
    let bytes = std::fs::read(&path).map_err(|err| {
        log::error!("read image {path:?} failed: {err}");
        AppError::Clipboard(err.to_string())
    })?;

    guard.suppress_image(item.content_hash.clone(), &bytes);
    backend.set_png(bytes)
}

fn write_files(
    backend: &dyn ClipboardBackend,
    guard: &WritebackGuard,
    item: &ClipboardItem,
) -> Result<()> {
    let paths: Vec<String> = item
        .content
        .split('\n')
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect();
    if paths.is_empty() {
        return Err(AppError::Clipboard("no files to write".to_owned()));
    }

    guard.suppress(item.content_hash.clone());
    backend.set_files(paths)?;
    Ok(())
}

/// 把 files 条目的路径列表当文本写回（换行分隔，多文件按行展开）。
/// 与 `write_files` 共用 `content_hash` 抑制——OS 监听不会拿到与原文本完全一致的回环。
fn write_files_as_text(
    backend: &dyn ClipboardBackend,
    guard: &WritebackGuard,
    item: &ClipboardItem,
) -> Result<()> {
    let text = item.content.clone();

    guard.suppress(content_hash(ClipboardKind::Text, &text));
    backend.set_text(text)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::backend::{MemoryClipboard, MemoryState};
    use super::super::payload::ImagePayload;
    use super::super::read::ClipboardReader;
    use super::*;
    use crate::clipboard::{build_item, ImageStore, WritebackGuard};
    use crate::db::models::Platform;
    use chrono::Utc;

    fn text_item(
        content: &str,
        sub: Option<ClipboardSubKind>,
        search: Option<&str>,
    ) -> ClipboardItem {
        ClipboardItem {
            id: uuid::Uuid::new_v4().to_string(),
            kind: ClipboardKind::Text,
            sub_kind: sub,
            group_id: None,
            source_app_id: None,
            content_hash: content_hash(ClipboardKind::Text, content),
            content: content.to_owned(),
            search_text: search.map(str::to_owned),
            summary: None,
            file_types: None,
            size: None,
            width: None,
            height: None,
            use_count: 1,
            is_favorite: false,
            is_pinned: false,
            is_sensitive: false,
            platform: Platform::Macos,
            note: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            origin_device_id: None,
            source_app_name: None,
            source_app_icon_file: None,
        }
    }

    fn temp_store() -> (TempDir, ImageStore) {
        let dir = TempDir::new();
        let store = ImageStore::for_test(dir.path().join("resources").join("clipboard-images"));
        (dir, store)
    }

    /// 写回后按监听路径读回并入库，返回读回的记录。
    fn read_back(clipboard: &MemoryClipboard, store: &ImageStore) -> ClipboardItem {
        let state = clipboard.snapshot();
        let payload = ClipboardReader::with_backend(MemoryClipboard::with_state(state))
            .read_with_capture(&crate::settings::Capture::default())
            .unwrap()
            .expect("should read");
        build_item(store, &payload).unwrap().unwrap()
    }

    // 写入纯文本 → 读回应为同串，且 guard 已登记本次哈希。
    #[test]
    fn writes_plain_text_and_arms_guard() {
        let (_dir, store) = temp_store();
        let guard = WritebackGuard::new();
        let clipboard = MemoryClipboard::new();

        let item = text_item("hello write", None, None);
        write_to_clipboard(&clipboard, &store, &guard, &item, false).unwrap();

        let read_item = read_back(&clipboard, &store);
        assert_eq!(read_item.content, "hello write");
        assert!(guard.should_skip(&read_item.content_hash));
    }

    // 纯文本模式：强制丢弃 HTML，写 search_text。
    #[test]
    fn plain_mode_strips_html() {
        let (_dir, store) = temp_store();
        let guard = WritebackGuard::new();
        let clipboard = MemoryClipboard::with_state(MemoryState {
            html: Some("<i>left over</i>".to_owned()),
            ..MemoryState::default()
        });

        let item = text_item(
            "<b>Hello</b> World",
            Some(ClipboardSubKind::Html),
            Some("Hello World"),
        );
        write_to_clipboard(&clipboard, &store, &guard, &item, true).unwrap();

        assert_eq!(
            clipboard.snapshot(),
            MemoryState {
                text: Some("Hello World".to_owned()),
                ..MemoryState::default()
            }
        );
        let read_item = read_back(&clipboard, &store);
        assert_eq!(read_item.kind, ClipboardKind::Text);
        assert_eq!(read_item.sub_kind, None);
        assert_eq!(read_item.content, "Hello World");
        assert!(guard.should_skip(&read_item.content_hash));
    }

    // HTML 写回同时带纯文本回退，两种表示的哈希都登记，读回 HTML 时被抑制。
    #[test]
    fn html_is_written_with_plain_fallback() {
        let (_dir, store) = temp_store();
        let guard = WritebackGuard::new();
        let clipboard = MemoryClipboard::new();

        let item = text_item(
            "<b>Hello</b> World",
            Some(ClipboardSubKind::Html),
            Some("Hello World"),
        );
        write_to_clipboard(&clipboard, &store, &guard, &item, false).unwrap();

        assert_eq!(
            clipboard.snapshot(),
            MemoryState {
                text: Some("Hello World".to_owned()),
                html: Some("<b>Hello</b> World".to_owned()),
                ..MemoryState::default()
            }
        );
        let read_item = read_back(&clipboard, &store);
        assert_eq!(read_item.content_hash, item.content_hash);
        assert!(guard.should_skip(&read_item.content_hash));
        assert!(guard.should_skip(&content_hash(ClipboardKind::Text, "Hello World")));
    }

    #[test]
    fn files_are_written_as_files_or_as_path_text() {
        let (_dir, store) = temp_store();
        let guard = WritebackGuard::new();
        let clipboard = MemoryClipboard::new();
        let mut item = text_item("", None, None);
        item.kind = ClipboardKind::Files;
        item.content = "C:/a.txt\nC:/b".to_owned();
        item.content_hash = content_hash(ClipboardKind::Files, &item.content);

        write_to_clipboard(&clipboard, &store, &guard, &item, false).unwrap();
        assert_eq!(
            clipboard.snapshot().files,
            Some(vec!["C:/a.txt".to_owned(), "C:/b".to_owned()])
        );
        assert!(guard.should_skip(&item.content_hash));

        write_to_clipboard(&clipboard, &store, &guard, &item, true).unwrap();
        assert_eq!(
            clipboard.snapshot(),
            MemoryState {
                text: Some("C:/a.txt\nC:/b".to_owned()),
                ..MemoryState::default()
            }
        );
        assert!(guard.should_skip(&content_hash(ClipboardKind::Text, "C:/a.txt\nC:/b")));
    }

    #[test]
    fn fragment_is_written_as_plain_text_and_suppressed() {
        let guard = WritebackGuard::new();
        let clipboard = MemoryClipboard::new();

        write_text_fragment(&clipboard, &guard, "W2000").unwrap();

        assert_eq!(clipboard.snapshot().text.as_deref(), Some("W2000"));
        assert!(guard.should_skip(&content_hash(ClipboardKind::Text, "W2000")));
    }

    // 图片往返：写盘上的 PNG → 写剪贴板 → 读回 → 落盘的文件名应一致（去重哈希命中）。
    #[test]
    fn round_trip_image_matches_hash() {
        let (_dir, store) = temp_store();
        let guard = WritebackGuard::new();
        let clipboard = MemoryClipboard::new();

        // 先落盘一张原图（模拟历史记录里的 image item）。
        let png = sample_png(48, 32);
        let stored = store
            .store(&ImagePayload {
                bytes: png,
                width: 48,
                height: 32,
            })
            .unwrap();
        let mut item = text_item("", None, None);
        item.kind = ClipboardKind::Image;
        item.content_hash = content_hash(ClipboardKind::Image, &stored.file_name);
        item.content = stored.file_name.clone();
        item.size = Some(stored.size);
        item.width = Some(stored.width);
        item.height = Some(stored.height);

        write_to_clipboard(&clipboard, &store, &guard, &item, false).unwrap();

        let read_item = read_back(&clipboard, &store);
        assert_eq!(read_item.kind, ClipboardKind::Image);
        // 往返期望 PNG 字节哈希一致 → 同 content_hash → guard 抑制。
        assert_eq!(read_item.content_hash, item.content_hash);
        assert!(guard.should_skip(&read_item.content_hash));
        assert!(!guard.should_skip_image(&clipboard.snapshot().png.unwrap()));
    }

    // 浏览器等来源的 PNG 与本地编码器的产物字节不同：写回必须原样放回，读回哈希才对得上。
    #[test]
    fn round_trip_foreign_png_keeps_bytes() {
        let (_dir, store) = temp_store();
        let guard = WritebackGuard::new();
        let clipboard = MemoryClipboard::new();

        let png = foreign_png(40, 30);
        assert_ne!(
            png,
            sample_png(40, 30),
            "fixture must differ from the local encoder"
        );
        let stored = store
            .store(&ImagePayload {
                bytes: png.clone(),
                width: 40,
                height: 30,
            })
            .unwrap();
        let mut item = text_item("", None, None);
        item.kind = ClipboardKind::Image;
        item.content_hash = content_hash(ClipboardKind::Image, &stored.file_name);
        item.content = stored.file_name.clone();

        write_to_clipboard(&clipboard, &store, &guard, &item, false).unwrap();

        assert_eq!(clipboard.snapshot().png, Some(png));
        let read_item = read_back(&clipboard, &store);
        assert_eq!(read_item.content_hash, item.content_hash);
        assert!(guard.should_skip(&read_item.content_hash));
    }

    #[test]
    fn image_fingerprint_failure_does_not_block_original_write() {
        let (_dir, store) = temp_store();
        let original = sample_png(48, 32);
        let stored = store
            .store(&ImagePayload {
                bytes: original,
                width: 48,
                height: 32,
            })
            .unwrap();
        let mut item = text_item("", None, None);
        item.kind = ClipboardKind::Image;
        item.content = stored.file_name;
        item.content_hash = content_hash(ClipboardKind::Image, &item.content);

        // MemoryClipboard 接受原始字节，只验证辅助解码不会新增写回失败。
        for bytes in [b"invalid png".to_vec(), vec![0; 20 * 1024 * 1024 + 1]] {
            std::fs::write(store.origin_path(&item.content), &bytes).unwrap();
            let guard = WritebackGuard::new();
            let clipboard = MemoryClipboard::new();
            write_to_clipboard(&clipboard, &store, &guard, &item, false).unwrap();
            assert_eq!(clipboard.snapshot().png.as_ref(), Some(&bytes));
            assert!(guard.should_skip(&item.content_hash));
        }
    }

    /// 用与本地默认不同的压缩和滤波编码，模拟浏览器复制来的 PNG。
    fn foreign_png(w: u32, h: u32) -> Vec<u8> {
        use image::codecs::png::{CompressionType, FilterType, PngEncoder};
        let buf = image::RgbaImage::from_fn(w, h, |x, y| {
            image::Rgba([(x * 6) as u8, (y * 8) as u8, 90, 255])
        });
        let mut out = Vec::new();
        let encoder =
            PngEncoder::new_with_quality(&mut out, CompressionType::Best, FilterType::NoFilter);
        image::DynamicImage::ImageRgba8(buf)
            .write_with_encoder(encoder)
            .unwrap();
        out
    }

    fn sample_png(w: u32, h: u32) -> Vec<u8> {
        use std::io::Cursor;
        let buf = image::RgbaImage::from_pixel(w, h, image::Rgba([4, 5, 6, 255]));
        let mut out = Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(buf)
            .write_to(&mut out, image::ImageFormat::Png)
            .unwrap();
        out.into_inner()
    }

    struct TempDir(std::path::PathBuf);
    impl TempDir {
        fn new() -> Self {
            let p = std::env::temp_dir().join(format!("kwikpaste-write-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&p).unwrap();
            Self(p)
        }
        fn path(&self) -> &std::path::Path {
            &self.0
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).ok();
        }
    }
}
