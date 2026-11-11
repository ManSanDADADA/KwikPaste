//! 列表卡片的展示数据：在数据库行上补齐缩略图、来源应用图标、文件条目、颜色预览、显示时间、
//! 快捷信息与可用动作，并按设置脱敏。

use anyhow::Context as _;
use chrono::{DateTime, Datelike, Offset as _, TimeZone};
use sqlx::SqlitePool;

use super::files::{is_image_path, resolve_file_icon_path};
use super::image_display_size;
use super::view::{ClipboardAction, ClipboardItemView, FileEntry, FilesPreviewKind};
use crate::clipboard::{
    detect_text_sub_kind, image_file_dimensions, quick_snippets, sanitize_css_color,
    validate_image_file_name, AppIconStore, FileIconStore, ImageStore, SUMMARY_MAX_CHARS,
};
use crate::db::models::{ClipboardItem, ClipboardKind, ClipboardSubKind};
use crate::error::Result;
use crate::settings::Clipboard;
use crate::sync::PeerStore;

/// 一次列表加工用到的存储与设置。`now` 决定显示时间按哪天算「今天」。
pub(crate) struct ListContext<'a, Tz: TimeZone> {
    pub pool: &'a SqlitePool,
    pub images: &'a ImageStore,
    pub app_icons: &'a AppIconStore,
    pub file_icons: &'a FileIconStore,
    pub now: DateTime<Tz>,
    pub file_entry_limit: usize,
    pub image_max_height: u16,
    pub redact_sensitive: bool,
    pub quick_snippets: bool,
    pub rules: crate::clipboard::rules::CompiledRules,
    /// 已配对（含已移除）设备，同步收到的记录据此显示「来自 xxx」。
    pub devices: Option<&'a PeerStore>,
    pub wetype_devices: Option<&'a crate::wetype::DeviceNames>,
}

impl<'a, Tz: TimeZone> ListContext<'a, Tz> {
    /// 按当前设置建上下文。
    pub fn new(
        pool: &'a SqlitePool,
        images: &'a ImageStore,
        app_icons: &'a AppIconStore,
        file_icons: &'a FileIconStore,
        clipboard: &Clipboard,
        now: DateTime<Tz>,
    ) -> Self {
        Self {
            pool,
            images,
            app_icons,
            file_icons,
            now,
            file_entry_limit: clipboard.display.file_entry_limit(),
            image_max_height: clipboard.display.image_max_height,
            redact_sensitive: clipboard.sensitive.redact_secrets,
            quick_snippets: clipboard.display.quick_snippets,
            rules: clipboard.compiled_rules().clone(),
            devices: None,
            wetype_devices: None,
        }
    }

    pub fn with_wetype_devices(mut self, devices: &'a crate::wetype::DeviceNames) -> Self {
        self.wetype_devices = Some(devices);
        self
    }

    pub fn with_devices(mut self, devices: &'a PeerStore) -> Self {
        self.devices = Some(devices);
        self
    }
}

/// 加工一条列表记录，顺序与 1.4.0 的 `list_clipboard_items` 一致：先补齐附加字段，
/// 再按设置脱敏，最后按脱敏后的状态算可用动作。
pub(crate) async fn present_list_item<Tz: TimeZone>(
    ctx: &ListContext<'_, Tz>,
    item: ClipboardItem,
) -> Result<ClipboardItemView>
where
    Tz::Offset: std::fmt::Display,
{
    let mut view = ClipboardItemView::bare(item);

    attach_image_thumbnail_path(ctx.images, &mut view);
    attach_source_app_icon_path(ctx.app_icons, &mut view);
    attach_origin_device_name(ctx.devices, ctx.wetype_devices, &mut view);
    attach_file_entries(ctx.pool, ctx.file_icons, &mut view, ctx.file_entry_limit).await?;
    attach_color_preview(&mut view);
    attach_display_created_at(&mut view, &ctx.now);
    attach_quick_snippets(
        &mut view,
        ctx.quick_snippets,
        ctx.redact_sensitive,
        &ctx.rules,
    );
    let mut span_masked = false;
    if ctx.redact_sensitive && view.item.is_sensitive && view.item.kind == ClipboardKind::Text {
        // 列表查询没有完整正文；按正文定位遮罩区间，避免截断的摘要把凭据前缀漏出来。
        if let Some(summary) = masked_summary(ctx, &view.item).await? {
            view.item.summary = Some(summary);
            view.color_preview = None;
            span_masked = true;
        }
    }
    if !span_masked {
        redact_sensitive_list_item(&mut view, ctx.redact_sensitive);
    }
    view.available_actions = compute_available_actions(&view.item, ctx.redact_sensitive);
    attach_image_display_size(&mut view, ctx.image_max_height);
    attach_file_image_preview(ctx.images, &mut view, ctx.image_max_height);
    Ok(view)
}

/// 敏感摘要先只按正文开头这么多字符遮罩：摘要只取结果的前 [`SUMMARY_MAX_CHARS`] 个字符。
const MASK_PREFIX_CHARS: usize = 64 * 1024;

/// 敏感文本的遮罩摘要，正文取法同 [`crate::clipboard::fragment_source`]；记录已删除时返回 `None`。
/// 先只读正文开头一段。这段截断了正文、而遮罩退回了整段遮罩（敏感区间可能在后面）或遮完不够一条摘要
/// （长区间跨过了截断处）时才读全文重算，摘要与按全文遮罩一致。
async fn masked_summary<Tz: TimeZone>(
    ctx: &ListContext<'_, Tz>,
    item: &ClipboardItem,
) -> Result<Option<String>> {
    let query = match item.sub_kind {
        Some(ClipboardSubKind::Html | ClipboardSubKind::Rtf) => {
            "SELECT substr(COALESCE(search_text, content), 1, ?) FROM clipboard_items WHERE id = ?"
        }
        _ => "SELECT substr(content, 1, ?) FROM clipboard_items WHERE id = ?",
    };
    let Some(prefix) = sqlx::query_scalar::<_, String>(query)
        .bind(MASK_PREFIX_CHARS as i64)
        .bind(&item.id)
        .fetch_optional(ctx.pool)
        .await
        .context("failed to read sensitive item text")?
    else {
        return Ok(None);
    };

    let mut masked = super::text::mask_with_rules(&prefix, &ctx.rules);
    if prefix.chars().count() >= MASK_PREFIX_CHARS
        && (masked.trim().chars().count() < SUMMARY_MAX_CHARS
            || masked == super::text::mask_sensitive_text(&prefix))
    {
        let Some(full) = crate::db::items::find_item_by_id(ctx.pool, &item.id).await? else {
            return Ok(None);
        };
        masked = super::text::mask_with_rules(crate::clipboard::fragment_source(&full), &ctx.rules);
    }
    Ok(Some(
        masked.trim().chars().take(SUMMARY_MAX_CHARS).collect(),
    ))
}

/// 同步收到的记录：按来源设备 id 回填 LAN 或 WeType 设备名，已取消配对的设备也保留名称。
fn attach_origin_device_name(
    devices: Option<&PeerStore>,
    wetype: Option<&crate::wetype::DeviceNames>,
    view: &mut ClipboardItemView,
) {
    view.origin_device_name = view.item.origin_device_id.as_deref().and_then(|id| {
        if let Some(uin) = id.strip_prefix("wetype:") {
            wetype?.display_name(uin)
        } else {
            devices?.display_name(id)
        }
    });
}

/// 为 image 条目补齐**已存在**的缩略图绝对路径。
/// 缩略图尚未生成时返回 `None`：界面先显示同尺寸占位，再按需生成；
/// 不回退到原图路径，避免为一个几十像素高的卡片解码整张原图。
/// 历史脏数据（非 `<hash>.png`）同样降级为 `None`，不影响列表返回。
fn attach_image_thumbnail_path(store: &ImageStore, view: &mut ClipboardItemView) {
    let item = &view.item;
    if item.kind != ClipboardKind::Image {
        return;
    }

    if validate_image_file_name(&item.content).is_err() {
        view.image_thumbnail_path = None;
        return;
    }

    let thumb_path = store.thumbnail_path(&item.content);
    view.image_thumbnail_path = if thumb_path.exists() {
        thumb_path.to_str().map(str::to_owned)
    } else {
        None
    };
}

/// 把 `clipboard_apps.icon_file` 解析为磁盘绝对路径；utf-8 转换失败时置 `None`。
fn attach_source_app_icon_path(store: &AppIconStore, view: &mut ClipboardItemView) {
    view.source_app_icon_path = view
        .item
        .source_app_icon_file
        .as_deref()
        .and_then(|name| store.icon_path(name).to_str().map(str::to_owned));
}

/// 仅当 `sub_kind = Color` 时，把 `summary`（或 `content` 兜底）规范化为可信 CSS 颜色串。
fn attach_color_preview(view: &mut ClipboardItemView) {
    let item = &view.item;
    if item.sub_kind != Some(ClipboardSubKind::Color) {
        return;
    }

    let source = item.summary.as_deref().unwrap_or(&item.content);
    view.color_preview = sanitize_css_color(source);
}

/// 按设置为文本列表项提取快捷信息；脱敏展示的敏感条目不提取，片段会绕过遮罩露出凭据。
fn attach_quick_snippets(
    view: &mut ClipboardItemView,
    enabled: bool,
    redact_sensitive: bool,
    rules: &crate::clipboard::rules::CompiledRules,
) {
    if !enabled || redact_sensitive && view.item.is_sensitive {
        return;
    }

    view.quick_snippets = quick_snippets(&view.item, rules);
}

/// 按当前设置对敏感文本列表项返回脱敏摘要，避免列表暴露完整凭据。
fn redact_sensitive_list_item(view: &mut ClipboardItemView, redact_sensitive: bool) {
    let item = &mut view.item;
    if !redact_sensitive || !item.is_sensitive || item.kind != ClipboardKind::Text {
        return;
    }

    if let Some(summary) = item.summary.as_mut() {
        *summary = super::text::mask_sensitive_text(summary);
    }
    view.color_preview = None;
}

/// 图片卡片的显示尺寸，见 [`image_display_size`]。
fn attach_image_display_size(view: &mut ClipboardItemView, max_height: u16) {
    if view.item.kind != ClipboardKind::Image {
        return;
    }

    view.image_display_size = Some(image_display_size(
        view.item.width,
        view.item.height,
        max_height,
    ));
}

/// 单图文件记录按图片卡片的规则展示：显示尺寸用文件头里的宽高算（不解码整图），
/// 缩略图同样只给**已生成**的，没有时界面先画同尺寸占位，再经 `Core::ensure_file_thumbnail` 生成。
/// 文件头读不出来（没有读取权限、SVG 等不支持的格式、超出解码上限）时按普通文件行显示，不画坏图。
fn attach_file_image_preview(store: &ImageStore, view: &mut ClipboardItemView, max_height: u16) {
    if view.files_preview_kind != Some(FilesPreviewKind::ImagePreview) {
        return;
    }
    let Some(path) = view
        .file_entries
        .as_deref()
        .and_then(<[FileEntry]>::first)
        .map(|entry| std::path::PathBuf::from(&entry.path))
    else {
        return;
    };

    let Some((width, height)) = image_file_dimensions(&path) else {
        view.files_preview_kind = Some(FilesPreviewKind::List);
        return;
    };
    view.image_display_size = Some(image_display_size(
        Some(i64::from(width)),
        Some(i64::from(height)),
        max_height,
    ));
    view.image_thumbnail_path = store
        .file_thumbnail_path(&path)
        .filter(|thumb| thumb.exists())
        .and_then(|thumb| thumb.to_str().map(str::to_owned));
}

/// 把 `created_at`（UTC）按 `now` 所在时区做三档格式化：
/// 今天 → `HH:mm`，今年内 → `MM-DD HH:mm`，跨年 → `YYYY-MM-DD HH:mm`。
fn attach_display_created_at<Tz: TimeZone>(view: &mut ClipboardItemView, now: &DateTime<Tz>)
where
    Tz::Offset: std::fmt::Display,
{
    view.display_created_at = display_created_at(&view.item.created_at, now);
}

/// 显示时间的三档格式化，见 [`attach_display_created_at`]。
pub fn display_created_at<Tz: TimeZone>(
    created_at: &DateTime<chrono::Utc>,
    now: &DateTime<Tz>,
) -> String
where
    Tz::Offset: std::fmt::Display,
{
    let local = created_at.with_timezone(&now.timezone());
    // date_naive 在本地时间超出 chrono 范围时会 panic；created_at 可能来自同步或导入，取值不可信。
    let local_date = local
        .naive_utc()
        .checked_add_offset(local.offset().fix())
        .map(|naive| naive.date());
    let today = local_date == Some(now.date_naive());
    let same_year = now.year() == local.year();

    if today {
        local.format("%H:%M").to_string()
    } else if same_year {
        local.format("%m-%d %H:%M").to_string()
    } else {
        local.format("%Y-%m-%d %H:%M").to_string()
    }
}

/// 按 `kind` / `sub_kind` 计算右键菜单可用动作，按建议展示顺序返回。
/// 脱敏展示的敏感文本不提供拆词，与拆词接口的拒绝条件一致。
pub(crate) fn compute_available_actions(
    item: &ClipboardItem,
    redact_sensitive: bool,
) -> Vec<ClipboardAction> {
    let mut actions = Vec::with_capacity(12);

    actions.push(ClipboardAction::Paste);

    match item.kind {
        ClipboardKind::Text => actions.push(ClipboardAction::PasteAsPlainText),
        ClipboardKind::Files => actions.push(ClipboardAction::PasteAsPath),
        ClipboardKind::Image => {}
    }

    actions.push(ClipboardAction::Copy);
    if item.kind == ClipboardKind::Image {
        actions.push(ClipboardAction::SaveImage);
    }
    if item.kind == ClipboardKind::Text && !(redact_sensitive && item.is_sensitive) {
        actions.push(ClipboardAction::SplitWords);
    }

    let sub_kind = if item.kind == ClipboardKind::Text
        && matches!(
            item.sub_kind,
            Some(ClipboardSubKind::Html | ClipboardSubKind::Rtf)
        ) {
        // 截断摘要可能只是段落开头的链接；脱敏摘要也不代表原始纯文本。
        item.summary
            .as_deref()
            .filter(|summary| {
                !(redact_sensitive && item.is_sensitive)
                    && summary.chars().count() < SUMMARY_MAX_CHARS
            })
            .and_then(detect_text_sub_kind)
    } else {
        item.sub_kind
    };
    match sub_kind {
        Some(ClipboardSubKind::Url) => actions.push(ClipboardAction::OpenLink),
        Some(ClipboardSubKind::Email) => actions.push(ClipboardAction::SendEmail),
        _ => {}
    }

    // 图片记录显示落盘的原图。
    let can_reveal = matches!(item.kind, ClipboardKind::Files | ClipboardKind::Image)
        || item.sub_kind == Some(ClipboardSubKind::Path);
    if can_reveal {
        #[cfg(target_os = "macos")]
        actions.push(ClipboardAction::RevealInFinder);
        #[cfg(target_os = "windows")]
        actions.push(ClipboardAction::RevealInExplorer);
    }

    actions.push(ClipboardAction::ToggleFavorite);
    actions.push(ClipboardAction::TogglePinned);
    actions.push(ClipboardAction::EditNote);
    if item.kind == ClipboardKind::Text && item.has_original {
        actions.push(ClipboardAction::RestoreOriginal);
    }
    actions.push(ClipboardAction::Select);
    actions.push(ClipboardAction::Delete);

    actions
}

/// 为 files 条目按设置组装前若干项 [`FileEntry`]：路径 / 文件名 / 目录标记 / 图片标记 / icon 绝对路径。
/// 非 files 条目或无路径时保持 `file_entries = None`。
async fn attach_file_entries(
    pool: &SqlitePool,
    store: &FileIconStore,
    view: &mut ClipboardItemView,
    limit: usize,
) -> Result<()> {
    let item = &view.item;
    if item.kind != ClipboardKind::Files {
        return Ok(());
    }

    let mut paths: Vec<&str> = item.content.split('\n').filter(|p| !p.is_empty()).collect();
    let single_file = paths.len() == 1;
    paths.truncate(limit);
    if paths.is_empty() {
        return Ok(());
    }

    let types: Vec<&str> = item
        .file_types
        .as_deref()
        .unwrap_or("")
        .split(',')
        .collect();

    let mut entries = Vec::with_capacity(paths.len());
    for (index, path) in paths.iter().enumerate() {
        let (icon_path, exists, modified) =
            resolve_file_icon_path(pool, store, path, item.file_types.as_deref(), index).await?;
        let is_dir = types.get(index).copied() == Some("d");
        let name = std::path::Path::new(path)
            .file_name()
            .and_then(|n| n.to_str())
            .map(str::to_owned)
            .unwrap_or_else(|| (*path).to_owned());
        let is_image = !is_dir && is_image_path(path);

        entries.push(FileEntry {
            path: (*path).to_owned(),
            name,
            is_dir,
            is_image,
            exists,
            modified,
            icon_path,
        });
    }

    view.file_entries = Some(entries);
    view.files_preview_kind = Some(match view.file_entries.as_deref() {
        Some([only]) if single_file && only.is_image && only.exists => {
            FilesPreviewKind::ImagePreview
        }
        _ => FilesPreviewKind::List,
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use chrono::{FixedOffset, Utc};

    use super::*;
    use crate::presenter::tests::{image_item, text_item};

    fn view(item: ClipboardItem) -> ClipboardItemView {
        ClipboardItemView::bare(item)
    }

    #[tokio::test]
    async fn one_displayed_entry_does_not_make_a_multi_file_payload_an_image() {
        let temp = tempfile::tempdir().unwrap();
        let image = temp.path().join("a.png");
        std::fs::write(&image, crate::presenter::tests::sample_png(8, 8)).unwrap();
        let text = temp.path().join("b.txt");
        std::fs::write(&text, "text").unwrap();
        let pool = crate::db::test_support::memory_pool().await;
        let icons = FileIconStore::for_test(temp.path().join("icons"));
        let mut item = text_item(None, false);
        item.kind = ClipboardKind::Files;
        for (content, expected) in [
            (
                format!("{}\n{}", image.display(), text.display()),
                FilesPreviewKind::List,
            ),
            (
                format!("\n{}\n", image.display()),
                FilesPreviewKind::ImagePreview,
            ),
        ] {
            item.content = content;
            let mut view = view(item.clone());
            attach_file_entries(&pool, &icons, &mut view, 1)
                .await
                .unwrap();
            assert_eq!(view.file_entries.as_ref().unwrap().len(), 1);
            assert!(view.file_entries.as_ref().unwrap()[0].modified.is_some());
            assert_eq!(view.files_preview_kind, Some(expected));
        }
    }

    // 只读正文开头时，摘要仍与按全文遮罩一致：密钥在开头、只在开头这段之后、区间跨过截断处、短文本。
    #[tokio::test]
    async fn sensitive_summary_matches_masking_the_full_text() {
        let temp = tempfile::tempdir().unwrap();
        let pool = crate::db::test_support::memory_pool().await;
        let images = ImageStore::for_test(temp.path().join("images"));
        let app_icons = AppIconStore::for_test(temp.path().join("app-icons"));
        let file_icons = FileIconStore::for_test(temp.path().join("file-icons"));
        let clipboard = Clipboard::default();
        let ctx = ListContext::new(
            &pool,
            &images,
            &app_icons,
            &file_icons,
            &clipboard,
            Utc::now(),
        );
        let key = "sk-abcdefghijklmnopqrstuvwxyzABCDE1234567890";
        let filler = "log line\n".repeat(MASK_PREFIX_CHARS / 8);
        let contents = [
            format!("{key}\n{filler}"),
            format!("{filler}{key}"),
            format!("token {key}{} tail", "x".repeat(MASK_PREFIX_CHARS)),
            format!("token {key}"),
        ];

        for (index, content) in contents.into_iter().enumerate() {
            let mut item = text_item(None, true);
            item.id = format!("secret-{index}");
            item.content_hash = crate::db::items::content_hash(ClipboardKind::Text, &content);
            item.content = content;
            crate::db::items::insert_item(&pool, &item).await.unwrap();

            let expected: String = super::super::text::mask_with_rules(&item.content, &ctx.rules)
                .trim()
                .chars()
                .take(SUMMARY_MAX_CHARS)
                .collect();
            assert_eq!(
                masked_summary(&ctx, &item).await.unwrap(),
                Some(expected),
                "case {index}"
            );
        }
    }

    #[test]
    fn image_actions_include_save_image() {
        let actions = compute_available_actions(&image_item(), false);

        assert!(actions.contains(&ClipboardAction::SaveImage));
    }

    #[test]
    fn text_actions_do_not_include_save_image() {
        let actions = compute_available_actions(&text_item(None, false), false);

        assert!(!actions.contains(&ClipboardAction::SaveImage));
    }

    #[test]
    fn restore_original_requires_a_text_snapshot() {
        let mut item = text_item(None, false);
        assert!(
            !compute_available_actions(&item, false).contains(&ClipboardAction::RestoreOriginal)
        );
        item.has_original = true;
        assert!(compute_available_actions(&item, false).contains(&ClipboardAction::RestoreOriginal));
        for kind in [ClipboardKind::Image, ClipboardKind::Files] {
            item.kind = kind;
            assert!(!compute_available_actions(&item, false)
                .contains(&ClipboardAction::RestoreOriginal));
        }
        assert_eq!(
            serde_json::to_value(ClipboardAction::RestoreOriginal).unwrap(),
            "restoreOriginal"
        );
    }

    #[test]
    fn rich_text_link_actions_use_only_complete_plain_summaries() {
        for sub_kind in [ClipboardSubKind::Html, ClipboardSubKind::Rtf] {
            let mut item = text_item(Some(sub_kind), false);
            for (plain, action) in [
                ("  https://example.com  ", ClipboardAction::OpenLink),
                ("user@example.com", ClipboardAction::SendEmail),
            ] {
                item.summary = Some(plain.to_owned());
                assert!(compute_available_actions(&item, false).contains(&action));
                assert_eq!(item.sub_kind, Some(sub_kind));
            }
            for plain in [
                "ordinary paragraph".to_owned(),
                "https://example.com and more text".to_owned(),
                "#fff".to_owned(),
                std::env::temp_dir().to_string_lossy().into_owned(),
                format!("https://example.com/{}", "a".repeat(SUMMARY_MAX_CHARS)),
            ] {
                item.summary = Some(plain);
                let actions = compute_available_actions(&item, false);
                assert!(!actions.contains(&ClipboardAction::OpenLink));
                assert!(!actions.contains(&ClipboardAction::SendEmail));
                assert!(!actions.contains(&ClipboardAction::RevealInExplorer));
                assert!(!actions.contains(&ClipboardAction::RevealInFinder));
            }
            item.summary = Some("https://example.com".to_owned());
            item.is_sensitive = true;
            assert!(!compute_available_actions(&item, true).contains(&ClipboardAction::OpenLink));
        }
    }

    // 拆词只对文本开放；脱敏展示的敏感文本不拆，避免面板把凭据逐词摊开。
    #[test]
    fn split_words_follows_kind_and_redaction() {
        let has_split = |item: &ClipboardItem, redact: bool| {
            compute_available_actions(item, redact).contains(&ClipboardAction::SplitWords)
        };

        assert!(has_split(&text_item(None, false), true));
        assert!(has_split(&text_item(None, true), false));
        assert!(!has_split(&text_item(None, true), true));
        assert!(!has_split(&image_item(), false));
    }

    // 快捷信息同样受脱敏约束，并且可以整体关闭。
    #[test]
    fn quick_snippets_respect_setting_and_redaction() {
        let mut item = text_item(None, true);
        item.summary = Some("订单 20260924 已发货".to_owned());
        let mut item = view(item);

        attach_quick_snippets(
            &mut item,
            true,
            true,
            &crate::clipboard::rules::CompiledRules::default(),
        );
        assert!(item.quick_snippets.is_empty());

        attach_quick_snippets(
            &mut item,
            false,
            false,
            &crate::clipboard::rules::CompiledRules::default(),
        );
        assert!(item.quick_snippets.is_empty());

        attach_quick_snippets(
            &mut item,
            true,
            false,
            &crate::clipboard::rules::CompiledRules::default(),
        );
        assert_eq!(item.quick_snippets.len(), 1);
        assert_eq!(item.quick_snippets[0].text, "20260924");
        assert_eq!(
            item.quick_snippets[0].kind,
            crate::clipboard::SnippetKind::Number
        );
    }

    #[test]
    fn redact_sensitive_list_item_masks_summary_when_enabled() {
        let mut item = text_item(None, true);
        item.summary = Some("sk-abcdefghijklmnopqrstuvwxyzABCDE1234567890".to_owned());
        let mut item = view(item);

        redact_sensitive_list_item(&mut item, true);

        assert_eq!(item.item.summary.as_deref(), Some("sk-a********7890"));
    }

    #[test]
    fn redact_sensitive_list_item_keeps_summary_when_disabled() {
        let mut item = text_item(None, true);
        item.summary = Some("sk-abcdefghijklmnopqrstuvwxyzABCDE1234567890".to_owned());
        let mut item = view(item);

        redact_sensitive_list_item(&mut item, false);

        assert_eq!(
            item.item.summary.as_deref(),
            Some("sk-abcdefghijklmnopqrstuvwxyzABCDE1234567890")
        );
    }

    // 列表只携带已生成的缩略图路径；缺失时返回 None，绝不回退到原图路径。
    #[test]
    fn list_item_only_carries_generated_thumbnails() {
        let dir = tempfile::tempdir().unwrap();
        let store = ImageStore::for_test(dir.path().join("clipboard-images"));
        let stored = store
            .store(&crate::clipboard::ImagePayload {
                bytes: crate::presenter::tests::sample_png(32, 16),
                width: 32,
                height: 16,
            })
            .unwrap();
        let mut item = image_item();
        item.content = stored.file_name.clone();
        let mut item = view(item);

        attach_image_thumbnail_path(&store, &mut item);
        assert_eq!(
            item.image_thumbnail_path, None,
            "missing thumbnail must not fall back to the origin path"
        );

        store.ensure_thumbnail(&stored.file_name).unwrap();
        attach_image_thumbnail_path(&store, &mut item);
        assert_eq!(
            item.image_thumbnail_path.as_deref(),
            store.thumbnail_path(&stored.file_name).to_str()
        );
    }

    #[test]
    fn display_time_has_three_granularities() {
        let tz = FixedOffset::east_opt(8 * 3600).unwrap();
        let now = tz.with_ymd_and_hms(2026, 10, 2, 15, 30, 0).unwrap();
        let at = |y, mo, d, h, mi| {
            tz.with_ymd_and_hms(y, mo, d, h, mi, 0)
                .unwrap()
                .with_timezone(&Utc)
        };

        assert_eq!(display_created_at(&at(2026, 10, 2, 0, 5), &now), "00:05");
        assert_eq!(
            display_created_at(&at(2026, 10, 1, 23, 59), &now),
            "10-01 23:59"
        );
        assert_eq!(
            display_created_at(&at(2025, 12, 31, 8, 0), &now),
            "2025-12-31 08:00"
        );
    }

    #[test]
    fn display_time_survives_dates_at_the_edge_of_the_range() {
        let east = FixedOffset::east_opt(8 * 3600).unwrap();
        let west = FixedOffset::west_opt(8 * 3600).unwrap();
        for created_at in [DateTime::<Utc>::MAX_UTC, DateTime::<Utc>::MIN_UTC] {
            for tz in [east, west] {
                let now = tz.with_ymd_and_hms(2026, 10, 2, 15, 30, 0).unwrap();
                assert!(!display_created_at(&created_at, &now).is_empty());
            }
        }
    }
}
