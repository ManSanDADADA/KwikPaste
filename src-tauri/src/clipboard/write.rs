//! 剪贴板写回：把 [`ClipboardItem`] 按类型写回系统剪贴板（text / html / rtf / image / files）。
//!
//! 时序约束：[`ClipboardContext`] 是 `!Send`，调用方需在不跨 await 的同步段内完成调用
//! （命令层照 `read_clipboard` 的写法处理）。
//!
//! 回环抑制：写回前向 [`WritebackGuard`] 登记将写入内容的 `content_hash`，
//! OS 监听重新读到同内容时跳过入库，避免「点击粘贴 → 自动新增一条」回环。
//! 哈希必须与 [`crate::clipboard::ingest::build_item`] 在 watcher 路径上将算出的哈希一致：
//! - text / html / rtf：watcher 拿到的 plain/html/rtf 经 `draft_from_text` 后 `content` 即我们写入的串，
//!   `content_hash(Text, written)` 自然匹配；
//! - files：watcher 把路径列表用 `\n` 连接后哈希，与我们 `item.content` 一致；
//! - image：watcher 走 PNG 直通读回原始字节 → 文件名 → 哈希，所以写回必须把落盘的 PNG
//!   原样放上剪贴板（见 [`set_png`]）。重新编码会让浏览器等来源的图片读回后哈希对不上，粘贴一次多一条。
//!
//! 纯文本模式（`plain = true`）：忽略 `sub_kind`，写 `search_text`（OS 提供的纯文本表示），
//! 缺失时退回 `content`。供「纯文本粘贴」快捷路径使用。

use clipboard_rs::{Clipboard, ClipboardContent, ClipboardContext};

use super::guard::WritebackGuard;
use super::read::PNG_FORMAT;
use super::storage::ImageStore;
use crate::core::{AppError, Result};
use crate::db::items::content_hash;
use crate::db::models::{ClipboardItem, ClipboardKind, ClipboardSubKind};

/// 把 `item` 写回系统剪贴板；`plain = true` 强制只写纯文本（剥离 HTML/RTF）。
pub fn write_to_clipboard(
    store: &ImageStore,
    guard: &WritebackGuard,
    item: &ClipboardItem,
    plain: bool,
) -> Result<()> {
    let ctx = ClipboardContext::new().map_err(clip_err)?;

    match item.kind {
        ClipboardKind::Text => write_text(&ctx, guard, item, plain)?,
        ClipboardKind::Image => write_image(&ctx, store, guard, item)?,
        // files + plain：把路径列表当文本写回，供「粘贴为路径」使用。
        ClipboardKind::Files if plain => write_files_as_text(&ctx, guard, item)?,
        ClipboardKind::Files => write_files(&ctx, guard, item)?,
    }
    Ok(())
}

/// 把从历史记录里取出的一段纯文本（快捷信息 / 拆词选区）写回剪贴板。
/// 同样登记回环抑制：片段只是这次要粘贴的内容，不另外记成一条新历史。
pub fn write_text_fragment(guard: &WritebackGuard, text: &str) -> Result<()> {
    let ctx = ClipboardContext::new().map_err(clip_err)?;

    guard.suppress(content_hash(ClipboardKind::Text, text));
    ctx.set(vec![ClipboardContent::Text(text.to_owned())])
        .map_err(clip_err)
}

fn write_text(
    ctx: &ClipboardContext,
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
        None if plain => ctx
            .set(vec![ClipboardContent::Text(content)])
            .map_err(clip_err)?,
        // HTML / RTF 必须同时写入纯文本回退：clipboard-rs 的 set_html / set_rich_text
        // 会先 clearContents，单独写时只剩富格式，多数应用读 plain/text 拿不到就拒绝粘贴。
        // 走 set(Vec<ClipboardContent>) 一次写多格式（内部不再相互清空）。
        Some(ClipboardSubKind::Html) => {
            let plain = item.search_text.clone().unwrap_or_else(|| content.clone());
            guard.suppress(content_hash(ClipboardKind::Text, &plain));
            ctx.set(vec![
                ClipboardContent::Text(plain),
                ClipboardContent::Html(content),
            ])
            .map_err(clip_err)?;
        }
        Some(ClipboardSubKind::Rtf) => {
            let plain = item.search_text.clone().unwrap_or_else(|| content.clone());
            guard.suppress(content_hash(ClipboardKind::Text, &plain));
            ctx.set(vec![
                ClipboardContent::Text(plain),
                ClipboardContent::Rtf(content),
            ])
            .map_err(clip_err)?;
        }
        // url / email / color / path 及无 sub_kind 都走纯文本通道。
        _ => ctx.set_text(content).map_err(clip_err)?,
    }
    Ok(())
}

fn write_image(
    ctx: &ClipboardContext,
    store: &ImageStore,
    guard: &WritebackGuard,
    item: &ClipboardItem,
) -> Result<()> {
    let path = store.origin_path(&item.content);
    let bytes = std::fs::read(&path).map_err(|err| {
        log::error!("read image {path:?} failed: {err}");
        AppError::Clipboard(err.to_string())
    })?;

    guard.suppress(item.content_hash.clone());
    set_png(ctx, bytes)
}

/// 把落盘的 PNG 原样放上剪贴板（与 clipboard-rs `set_image` 写的是同一种类型，只是不重新编码）。
#[cfg(target_os = "macos")]
fn set_png(ctx: &ClipboardContext, bytes: Vec<u8>) -> Result<()> {
    ctx.set(vec![ClipboardContent::Other(PNG_FORMAT.to_owned(), bytes)])
        .map_err(clip_err)
}

/// 把落盘的 PNG 原样放上剪贴板，另附一份 `CF_DIB` 给只认位图的应用；
/// 系统会按需从它合成 `CF_BITMAP` / `CF_DIBV5`。解码放在打开剪贴板之前，占用剪贴板只做两次拷贝。
#[cfg(target_os = "windows")]
fn set_png(_ctx: &ClipboardContext, bytes: Vec<u8>) -> Result<()> {
    let dib = png_to_dib(&bytes)?;

    let _clipboard = open_clipboard()?;
    clipboard_win::empty().map_err(clip_err)?;
    let png_format = clipboard_win::register_format(PNG_FORMAT)
        .ok_or_else(|| AppError::Clipboard("PNG clipboard format unavailable".to_owned()))?;
    clipboard_win::raw::set_without_clear(png_format.get(), &bytes).map_err(clip_err)?;
    clipboard_win::raw::set_without_clear(clipboard_win::formats::CF_DIB, &dib)
        .map_err(clip_err)?;
    Ok(())
}

/// 打开剪贴板失败后的退避间隔。别的剪贴板监听程序可能正读着上一份（大图）内容，
/// `new_attempts` 自带的重试只让出时间片、几微秒就耗尽，与读取侧一样给一段有界的等待。
#[cfg(target_os = "windows")]
const OPEN_RETRY_DELAYS: [std::time::Duration; 3] = [
    std::time::Duration::from_millis(15),
    std::time::Duration::from_millis(35),
    std::time::Duration::from_millis(75),
];

#[cfg(target_os = "windows")]
fn open_clipboard() -> Result<clipboard_win::Clipboard> {
    let mut result = clipboard_win::Clipboard::new_attempts(10);
    for delay in OPEN_RETRY_DELAYS {
        if result.is_ok() {
            break;
        }
        std::thread::sleep(delay);
        result = clipboard_win::Clipboard::new_attempts(10);
    }
    result.map_err(clip_err)
}

/// `BITMAPINFOHEADER` 的字节长度。
#[cfg(target_os = "windows")]
const DIB_HEADER_LEN: usize = 40;

/// 解码 PNG 为 `CF_DIB` 数据：`BITMAPINFOHEADER` + 自下而上的 32 位 BGRA 行（`BI_RGB`）。
#[cfg(target_os = "windows")]
fn png_to_dib(png: &[u8]) -> Result<Vec<u8>> {
    let rgba = image::load_from_memory_with_format(png, image::ImageFormat::Png)
        .map_err(clip_err)?
        .into_rgba8();
    let (width, height) = rgba.dimensions();
    let row_len = width as usize * 4;
    let pixels_len = row_len * height as usize;

    let mut dib = Vec::with_capacity(DIB_HEADER_LEN + pixels_len);
    dib.extend_from_slice(&(DIB_HEADER_LEN as u32).to_le_bytes());
    dib.extend_from_slice(&(width as i32).to_le_bytes());
    // 正高度表示行自下而上存放。
    dib.extend_from_slice(&(height as i32).to_le_bytes());
    dib.extend_from_slice(&1u16.to_le_bytes());
    dib.extend_from_slice(&32u16.to_le_bytes());
    dib.extend_from_slice(&0u32.to_le_bytes());
    dib.extend_from_slice(&(pixels_len as u32).to_le_bytes());
    // 分辨率、调色板计数：全 0。
    dib.extend_from_slice(&[0; 16]);

    for row in rgba.as_raw().chunks_exact(row_len).rev() {
        for pixel in row.chunks_exact(4) {
            dib.extend_from_slice(&[pixel[2], pixel[1], pixel[0], pixel[3]]);
        }
    }
    Ok(dib)
}

fn write_files(ctx: &ClipboardContext, guard: &WritebackGuard, item: &ClipboardItem) -> Result<()> {
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
    ctx.set_files(paths).map_err(clip_err)?;
    Ok(())
}

/// 把 files 条目的路径列表当文本写回（换行分隔，多文件按行展开）。
/// 与 `write_files` 共用 `content_hash` 抑制——OS 监听不会拿到与原文本完全一致的回环。
fn write_files_as_text(
    ctx: &ClipboardContext,
    guard: &WritebackGuard,
    item: &ClipboardItem,
) -> Result<()> {
    let text = item.content.clone();

    guard.suppress(content_hash(ClipboardKind::Text, &text));
    ctx.set_text(text).map_err(clip_err)?;
    Ok(())
}

fn clip_err<E: std::fmt::Display>(err: E) -> AppError {
    AppError::Clipboard(err.to_string())
}

#[cfg(test)]
mod tests {
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
            source_app_name: None,
            source_app_icon_file: None,
            source_app_icon_path: None,
            image_thumbnail_path: None,
            file_entries: None,
            files_preview_kind: None,
            available_actions: Vec::new(),
            color_preview: None,
            display_created_at: String::new(),
            quick_snippets: Vec::new(),
        }
    }

    fn temp_store() -> (TempDir, ImageStore) {
        let dir = TempDir::new();
        let store = ImageStore::for_test(dir.path().join("resources").join("clipboard-images"));
        (dir, store)
    }

    // 触碰真实剪贴板：写入纯文本 → 读回应为同串，且 guard 已登记本次哈希。
    #[test]
    #[ignore = "touches the real system clipboard; run with --ignored on a desktop session"]
    fn writes_plain_text_and_arms_guard() {
        let _serial = crate::clipboard::test_lock::serial();
        let (_dir, store) = temp_store();
        let guard = WritebackGuard::new();

        let item = text_item("hello write", None, None);
        write_to_clipboard(&store, &guard, &item, false).unwrap();

        let reader = ClipboardReader::new().unwrap();
        let payload = reader
            .read_with_capture(&crate::settings::Capture::default())
            .unwrap()
            .expect("should read");
        let read_item = build_item(&store, &payload).unwrap().unwrap();
        assert_eq!(read_item.content, "hello write");
        assert!(guard.should_skip(&read_item.content_hash));
    }

    // 纯文本模式：强制丢弃 HTML，写 search_text。
    #[test]
    #[ignore = "touches the real system clipboard; run with --ignored on a desktop session"]
    fn plain_mode_strips_html() {
        let _serial = crate::clipboard::test_lock::serial();
        let (_dir, store) = temp_store();
        let guard = WritebackGuard::new();

        let item = text_item(
            "<b>Hello</b> World",
            Some(ClipboardSubKind::Html),
            Some("Hello World"),
        );
        write_to_clipboard(&store, &guard, &item, true).unwrap();

        let reader = ClipboardReader::new().unwrap();
        let payload = reader
            .read_with_capture(&crate::settings::Capture::default())
            .unwrap()
            .expect("should read");
        let read_item = build_item(&store, &payload).unwrap().unwrap();
        assert_eq!(read_item.kind, ClipboardKind::Text);
        assert_eq!(read_item.sub_kind, None);
        assert_eq!(read_item.content, "Hello World");
    }

    // 图片往返：写盘上的 PNG → 写剪贴板 → 读回 → 落盘的文件名应一致（去重哈希命中）。
    #[test]
    #[ignore = "touches the real system clipboard; run with --ignored on a desktop session"]
    fn round_trip_image_matches_hash() {
        let _serial = crate::clipboard::test_lock::serial();
        let (_dir, store) = temp_store();
        let guard = WritebackGuard::new();

        // 先落盘一张原图（模拟历史记录里的 image item）。
        let png = sample_png(48, 32);
        let stored = store
            .store(&ImagePayload {
                bytes: png,
                width: 48,
                height: 32,
            })
            .unwrap();
        let item = ClipboardItem {
            id: uuid::Uuid::new_v4().to_string(),
            kind: ClipboardKind::Image,
            sub_kind: None,
            group_id: None,
            source_app_id: None,
            content_hash: content_hash(ClipboardKind::Image, &stored.file_name),
            content: stored.file_name.clone(),
            search_text: None,
            summary: None,
            file_types: None,
            size: Some(stored.size),
            width: Some(stored.width),
            height: Some(stored.height),
            use_count: 1,
            is_favorite: false,
            is_pinned: false,
            is_sensitive: false,
            platform: Platform::Macos,
            note: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            source_app_name: None,
            source_app_icon_file: None,
            source_app_icon_path: None,
            image_thumbnail_path: None,
            file_entries: None,
            files_preview_kind: None,
            available_actions: Vec::new(),
            color_preview: None,
            display_created_at: String::new(),
            quick_snippets: Vec::new(),
        };

        write_to_clipboard(&store, &guard, &item, false).unwrap();

        let reader = ClipboardReader::new().unwrap();
        let payload = reader
            .read_with_capture(&crate::settings::Capture::default())
            .unwrap()
            .expect("should read image");
        let read_item = build_item(&store, &payload).unwrap().unwrap();
        assert_eq!(read_item.kind, ClipboardKind::Image);
        // 往返期望 PNG 字节哈希一致 → 同 content_hash → guard 抑制。
        assert_eq!(read_item.content_hash, item.content_hash);
        assert!(guard.should_skip(&read_item.content_hash));
    }

    // 浏览器等来源的 PNG 与本地编码器的产物字节不同：写回必须原样放回，读回哈希才对得上。
    #[test]
    #[ignore = "touches the real system clipboard; run with --ignored on a desktop session"]
    fn round_trip_foreign_png_keeps_bytes() {
        let _serial = crate::clipboard::test_lock::serial();
        let (_dir, store) = temp_store();
        let guard = WritebackGuard::new();

        let png = foreign_png(40, 30);
        assert_ne!(
            png,
            sample_png(40, 30),
            "fixture must differ from the local encoder"
        );
        let stored = store
            .store(&ImagePayload {
                bytes: png,
                width: 40,
                height: 30,
            })
            .unwrap();
        let mut item = text_item("", None, None);
        item.kind = ClipboardKind::Image;
        item.content_hash = content_hash(ClipboardKind::Image, &stored.file_name);
        item.content = stored.file_name.clone();

        write_to_clipboard(&store, &guard, &item, false).unwrap();

        let reader = ClipboardReader::new().unwrap();
        let payload = reader
            .read_with_capture(&crate::settings::Capture::default())
            .unwrap()
            .expect("should read image");
        let read_item = build_item(&store, &payload).unwrap().unwrap();
        assert_eq!(read_item.content_hash, item.content_hash);
        assert!(guard.should_skip(&read_item.content_hash));

        #[cfg(target_os = "windows")]
        {
            let dib: Vec<u8> = clipboard_win::get_clipboard(clipboard_win::formats::RawData(
                clipboard_win::formats::CF_DIB,
            ))
            .expect("CF_DIB should be on the clipboard");
            assert_eq!(i32::from_le_bytes(dib[4..8].try_into().unwrap()), 40);
            assert_eq!(i32::from_le_bytes(dib[8..12].try_into().unwrap()), 30);
        }
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn png_to_dib_writes_bottom_up_bgra() {
        // 2×2：上排红、绿，下排蓝、半透明白。
        let image = image::RgbaImage::from_raw(
            2,
            2,
            vec![
                255, 0, 0, 255, 0, 255, 0, 255, //
                0, 0, 255, 255, 255, 255, 255, 128,
            ],
        )
        .unwrap();
        let mut png = Vec::new();
        image::DynamicImage::ImageRgba8(image)
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();

        let dib = png_to_dib(&png).unwrap();

        assert_eq!(dib.len(), DIB_HEADER_LEN + 16);
        assert_eq!(u32::from_le_bytes(dib[0..4].try_into().unwrap()), 40);
        assert_eq!(i32::from_le_bytes(dib[4..8].try_into().unwrap()), 2);
        assert_eq!(i32::from_le_bytes(dib[8..12].try_into().unwrap()), 2);
        assert_eq!(u16::from_le_bytes(dib[14..16].try_into().unwrap()), 32);
        assert_eq!(
            &dib[DIB_HEADER_LEN..],
            &[
                255, 0, 0, 255, 255, 255, 255, 128, // 下排：蓝、半透明白
                0, 0, 255, 255, 0, 255, 0, 255, // 上排：红、绿
            ]
        );
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
