//! 展示层：把数据库记录加工成列表卡片、预览面板直接用的数据。
//!
//! 逻辑从 1.4.0 的命令层（`commands/clipboard.rs`）下沉而来，输出与 1.4.0 发给前端的 JSON 一致，
//! `tests/fixtures/presenter/` 下的 golden 夹具就是按 1.4.0 的加工规则录的。

mod files;
mod html;
mod list;
mod preview;
mod text;
mod view;

pub use files::{is_image_path, FileIconResult};
pub use html::{
    parse_preview_html, PreviewBlock, PreviewDocument, PreviewInline, PreviewInlineStyle,
};
pub use list::display_created_at;
pub use preview::{
    ClipboardPreviewFileEntry, ClipboardPreviewPayload, PreviewContentMetrics, PreviewWordChip,
    PREVIEW_FILE_ENTRY_LIMIT,
};
pub use text::{mask_sensitive_line, mask_sensitive_text};
pub use view::{
    ClipboardAction, ClipboardItemPage, ClipboardItemView, FileEntry, FilesPreviewKind,
};

pub(crate) use files::{resolve_file_icon_path, single_image_file};
pub(crate) use list::{present_list_item, ListContext};
pub(crate) use preview::{
    build_image_text_preview, build_preview_payload, preview_content_metrics,
};

use crate::clipboard::THUMBNAIL_MAX;

/// 图片卡片的显示尺寸（逻辑像素）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageDisplaySize {
    pub width: u32,
    pub height: u32,
}

/// 按缩略图规则（最长边不超过 [`THUMBNAIL_MAX`]）和显示高度上限算出图片卡片的显示尺寸，
/// 与 1.x 前端 `ImageCard` 的占位尺寸一致。缩略图到达前后布局不变，列表不跳动。
/// 记录缺宽高时按 `max_height` 见方。
pub fn image_display_size(
    width: Option<i64>,
    height: Option<i64>,
    max_height: u16,
) -> ImageDisplaySize {
    let max_height = f64::from(max_height);
    let (Some(width), Some(height)) = (width.filter(|w| *w > 0), height.filter(|h| *h > 0)) else {
        return ImageDisplaySize {
            width: max_height as u32,
            height: max_height as u32,
        };
    };

    let (width, height) = (width as f64, height as f64);
    let scale = (f64::from(THUMBNAIL_MAX) / width.max(height)).min(1.0);
    let thumb_height = height * scale;
    let display_height = max_height.min(thumb_height);
    let display_width = width * scale * display_height / thumb_height;

    ImageDisplaySize {
        width: display_width.round() as u32,
        height: display_height.round() as u32,
    }
}

#[cfg(test)]
mod golden;

#[cfg(test)]
pub(crate) mod tests {
    use chrono::Utc;

    use super::*;
    use crate::db::items::content_hash;
    use crate::db::models::{ClipboardItem, ClipboardKind, ClipboardSubKind, Platform};

    pub(crate) fn text_item(
        sub_kind: Option<ClipboardSubKind>,
        is_sensitive: bool,
    ) -> ClipboardItem {
        let content = "<b>secret</b>".to_owned();

        ClipboardItem {
            id: "item".to_owned(),
            kind: ClipboardKind::Text,
            sub_kind,
            group_id: None,
            source_app_id: None,
            content_hash: content_hash(ClipboardKind::Text, &content),
            content,
            search_text: None,
            summary: None,
            file_types: None,
            size: None,
            width: None,
            height: None,
            use_count: 1,
            is_favorite: false,
            is_pinned: false,
            is_sensitive,
            platform: Platform::Macos,
            note: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            has_original: false,
            origin_device_id: None,
            sync_key: None,
            source_app_name: None,
            source_app_icon_file: None,
        }
    }

    pub(crate) fn image_item() -> ClipboardItem {
        let content = "abcdef0123.png".to_owned();
        let mut item = text_item(None, false);

        item.kind = ClipboardKind::Image;
        item.content_hash = content_hash(ClipboardKind::Image, &content);
        item.content = content;
        item
    }

    pub(crate) fn sample_png(w: u32, h: u32) -> Vec<u8> {
        use std::io::Cursor;

        let buf = image::RgbaImage::from_pixel(w, h, image::Rgba([90, 60, 30, 255]));
        let mut out = Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(buf)
            .write_to(&mut out, image::ImageFormat::Png)
            .unwrap();
        out.into_inner()
    }

    #[test]
    fn image_display_size_matches_the_card_placeholder() {
        // 小图不放大，只受显示高度限制。
        assert_eq!(
            image_display_size(Some(120), Some(40), 64),
            ImageDisplaySize {
                width: 120,
                height: 40
            }
        );
        // 大图先按缩略图最长边 300 缩小，再按显示高度 64 等比缩放。
        assert_eq!(
            image_display_size(Some(1920), Some(1080), 64),
            ImageDisplaySize {
                width: 114,
                height: 64
            }
        );
        assert_eq!(
            image_display_size(Some(400), Some(1600), 120),
            ImageDisplaySize {
                width: 30,
                height: 120
            }
        );
        assert_eq!(
            image_display_size(None, Some(10), 64),
            ImageDisplaySize {
                width: 64,
                height: 64
            }
        );
    }
}
