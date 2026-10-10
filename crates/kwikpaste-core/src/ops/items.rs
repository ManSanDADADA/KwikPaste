//! 单条记录的操作：写回剪贴板（复制、粘贴前准备、片段、快速粘贴）、手动读取、收藏置顶备注分组、
//! 删除与清空、链接与文件定位、图片另存。

use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use chrono::{DateTime, Local, TimeZone, Utc};
use serde::Serialize;
use sqlx::SqlitePool;

use crate::clipboard::{
    self, build_item_with_settings, materialize_source, resolve_fragment, select_words,
    split_words, validate_image_file_name, ClipboardFragment, ClipboardPayload, ClipboardReader,
    WordSplit,
};
use crate::db::items::{
    clear_items, delete_item, delete_items, find_item_by_id, find_item_id_at,
    increment_item_use_count, list_item_refs, mark_item_favorite, reorder_item,
    toggle_item_favorite, toggle_item_pinned, touch_item_last_used, update_item_group,
    update_item_note, update_item_text_content, ReorderAnchor, ReorderSection,
};
use crate::db::models::{ClipboardItem, ClipboardItemQuery, ClipboardItemRef, ClipboardKind};
use crate::db::overview::{clear_scope, ClearScope};
use crate::error::{AppError, Result};
use crate::events::CoreEvent;
use crate::i18n::commands::{label, Key};
use crate::root::{Core, CoreInner};

/// 复制写回后的结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CopyOutcome {
    /// 设置要求从历史复制后隐藏剪贴板窗口；窗口被用户固定时宿主应忽略。
    pub hide_window: bool,
}

/// 备注更新结果：归一化后的备注（去前后空白，纯空白为 `None`）与是否顺带自动收藏。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateNoteResult {
    pub note: Option<String>,
    pub auto_favorited: bool,
}

/// 图片另存：原图路径与另存为对话框的默认文件名。宿主弹对话框、复制文件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageSave {
    pub source: PathBuf,
    pub default_file_name: String,
}

/// 手动读取剪贴板的结果。
#[derive(Debug, Clone)]
pub struct CapturedItem {
    /// 生效行的 id：命中去重时是已有记录。
    pub id: String,
    /// 这次读到的内容转成的记录（命中去重时它的 id 与 `id` 不同，未入库）。
    pub item: ClipboardItem,
    pub deduplicated: bool,
}

/// 一次快速粘贴的占用凭据：剪贴板已写好，宿主在注入粘贴按键（或放弃）后丢弃它。
/// 凭据存在期间新的快速粘贴触发会被忽略，避免连按叠出多次粘贴。
pub struct QuickPasteTicket {
    core: Arc<CoreInner>,
    pub item_id: String,
    pub kind: ClipboardKind,
}

impl Drop for QuickPasteTicket {
    fn drop(&mut self) {
        self.core
            .quick_paste_running
            .store(false, Ordering::Release);
    }
}

/// 抢占快速粘贴；没抢到（上一次还没粘完）时返回 `None`。抢到后途中出错也会随丢弃释放。
struct QuickPasteGuard(Option<Arc<CoreInner>>);

impl QuickPasteGuard {
    fn acquire(core: &Arc<CoreInner>) -> Option<Self> {
        (!core.quick_paste_running.swap(true, Ordering::AcqRel)).then(|| Self(Some(core.clone())))
    }

    fn into_ticket(mut self, item: &ClipboardItem) -> QuickPasteTicket {
        let core = self.0.take().expect("quick paste guard already consumed");
        QuickPasteTicket {
            core,
            item_id: item.id.clone(),
            kind: item.kind,
        }
    }

    fn into_plain_ticket(mut self, kind: ClipboardKind) -> QuickPasteTicket {
        let core = self.0.take().expect("quick paste guard already consumed");
        QuickPasteTicket {
            core,
            item_id: "clipboard".to_owned(),
            kind,
        }
    }
}

impl Drop for QuickPasteGuard {
    fn drop(&mut self) {
        if let Some(core) = &self.0 {
            core.quick_paste_running.store(false, Ordering::Release);
        }
    }
}

/// 计算复制写回是否强制走纯文本；默认复制纯文本只作用于文本记录。
pub(crate) fn should_write_plain_for_copy(
    force_plain: bool,
    kind: ClipboardKind,
    copy_plain: bool,
) -> bool {
    force_plain || kind == ClipboardKind::Text && copy_plain
}

/// 计算粘贴写回是否强制走纯文本；文本去格式与文件路径粘贴分别由各自设置控制。
/// 快速粘贴与窗口内默认粘贴用同一套规则。
pub(crate) fn should_write_plain_for_paste(
    force_plain: bool,
    kind: ClipboardKind,
    paste_plain: bool,
    paste_files_as_path: bool,
) -> bool {
    force_plain
        || kind == ClipboardKind::Text && paste_plain
        || kind == ClipboardKind::Files && paste_files_as_path
}

/// 决定一次全局纯文本粘贴要写入的文本；空文本和图片返回 `None`。
pub fn plain_paste_decision(payload: &ClipboardPayload) -> Option<String> {
    match payload {
        ClipboardPayload::Text(text) if !text.text.is_empty() => Some(text.text.clone()),
        ClipboardPayload::Files(files) => {
            let content = files
                .iter()
                .filter(|path| !path.is_empty())
                .cloned()
                .collect::<Vec<_>>()
                .join("\n");
            (!content.is_empty()).then_some(content)
        }
        ClipboardPayload::Text(_) | ClipboardPayload::Image(_) => None,
    }
}

/// 打开链接的目标：`mailto` 时补 `mailto:`，`www.` 开头补 `https://`；内容为空返回 `None`。
fn link_target(item: &ClipboardItem, mailto: bool) -> Option<String> {
    let value = item.content.trim();
    if value.is_empty() {
        return None;
    }

    Some(if mailto {
        format!("mailto:{value}")
    } else if value.starts_with("www.") {
        format!("https://{value}")
    } else {
        value.to_owned()
    })
}

/// 在文件管理器中显示的目标：文件记录取第一条路径，文本记录（路径子类型）取 trim 后的原文。
fn reveal_target(item: &ClipboardItem) -> Option<String> {
    let target = if item.kind == ClipboardKind::Files {
        item.content
            .split('\n')
            .find(|s| !s.is_empty())
            .unwrap_or("")
            .to_owned()
    } else {
        item.content.trim().to_owned()
    };

    (!target.is_empty()).then_some(target)
}

/// 图片另存为对话框的默认文件名，按 `tz` 所在时区的复制时间命名。
fn default_saved_image_file_name<Tz: TimeZone>(created_at: &DateTime<Utc>, tz: &Tz) -> String
where
    Tz::Offset: std::fmt::Display,
{
    let local = created_at.with_timezone(tz);

    format!("KwikPaste-image-{}.png", local.format("%Y%m%d-%H%M%S"))
}

/// 归一化备注：去前后空白，纯空白视为清空。
fn normalize_note(note: Option<&str>) -> Option<&str> {
    note.map(str::trim).filter(|trimmed| !trimmed.is_empty())
}

/// 按 id 取完整记录，不存在时报错。
async fn find_required(pool: &SqlitePool, id: &str) -> Result<ClipboardItem> {
    find_item_by_id(pool, id)
        .await?
        .ok_or_else(|| AppError::Clipboard(format!("clipboard item not found: {id}")))
}

/// 打开剪贴板写回一条记录并登记回环抑制。同步完成，后端不会跨 await 持有。
fn write_item(core: &CoreInner, item: &ClipboardItem, plain: bool) -> Result<()> {
    let backend = core.clipboard()?;
    clipboard::write_to_clipboard(&*backend, &core.images, &core.guard, item, plain)
}

/// 打开剪贴板写回一段纯文本并登记回环抑制。
fn write_fragment(core: &CoreInner, text: &str) -> Result<()> {
    let backend = core.clipboard()?;
    clipboard::write_text_fragment(&*backend, &core.guard, text)
}

/// 按设置决定是否把复制 / 粘贴历史记录计为一次复用：计入时累加次数、刷新 `updated_at` 并通知宿主；
/// 不计入时也记下最后使用时间，自动清理按它判断。
pub(crate) async fn mark_item_reused_if_enabled(
    core: &CoreInner,
    pool: &SqlitePool,
    id: &str,
    kind: ClipboardKind,
) -> Result<()> {
    if !core.settings.snapshot().clipboard.content.update_on_reuse {
        if let Err(err) = touch_item_last_used(pool, id).await {
            log::warn!("touch last used time of item {id} failed: {err}");
        }
        return Ok(());
    }

    increment_item_use_count(pool, id).await?;
    core.events.emit(CoreEvent::ClipboardUpserted {
        id: id.to_owned(),
        kind,
        deduplicated: true,
    });
    Ok(())
}

/// 读取记录并从原文里取出片段文本；记录已删除或片段已对不上原文时返回用户可读错误。
async fn load_fragment_text(
    core: &CoreInner,
    pool: &SqlitePool,
    id: &str,
    fragment: &ClipboardFragment,
) -> Result<(ClipboardKind, String)> {
    let item = find_required(pool, id).await?;
    let text = match fragment {
        ClipboardFragment::ImageWords { indices }
            if item.kind == ClipboardKind::Image && core.extensions.resolve("ocr").is_some() =>
        {
            let recognized: Option<String> = sqlx::query_scalar(
                "SELECT text FROM image_texts WHERE item_id = ? AND status = 'done'",
            )
            .bind(id)
            .fetch_optional(pool)
            .await
            .map_err(anyhow::Error::from)?;
            recognized.and_then(|text| select_words(&text, indices))
        }
        ClipboardFragment::Snippet { .. } | ClipboardFragment::Words { .. }
            if item.kind == ClipboardKind::Text =>
        {
            resolve_fragment(&item, fragment)
        }
        _ => None,
    }
    .ok_or_else(|| {
        AppError::Clipboard(label(core.language(), Key::FragmentUnavailable).to_owned())
    })?;

    Ok((item.kind, text))
}

impl Core {
    /// 把记录写回剪贴板（不粘贴）。`plain = true` 强制纯文本；否则文本记录按「复制时去除格式」设置。
    pub async fn copy_item(&self, id: &str, plain: bool) -> Result<CopyOutcome> {
        let core = self.clone();
        let id = id.to_owned();
        self.hop(async move {
            let pool = core.0.db.pool().await;
            let item = find_required(&pool, &id).await?;
            let content = core.0.settings.snapshot().clipboard.content;
            let write_plain = should_write_plain_for_copy(plain, item.kind, content.copy_plain);

            write_item(&core.0, &item, write_plain)?;
            mark_item_reused_if_enabled(&core.0, &pool, &id, item.kind).await?;
            Ok(CopyOutcome {
                hide_window: content.copy_then_hide_window,
            })
        })
        .await
    }

    /// 将图片识别结果按普通文本复制规则写回；识别写入本身不计复用，用户复制才计。
    pub async fn copy_image_text(&self, id: &str) -> Result<CopyOutcome> {
        let core = self.clone();
        let id = id.to_owned();
        self.hop(async move {
            let settings = core.settings().clipboard;
            if !core.ocr_enabled() {
                return Err(AppError::Clipboard(
                    "image text recognition is disabled".into(),
                ));
            }
            let pool = core.0.db.pool().await;
            let item = find_required(&pool, &id).await?;
            if item.kind != ClipboardKind::Image {
                return Err(AppError::Clipboard("not an image item".into()));
            }
            let text = core
                .image_text(&id)
                .await?
                .filter(|text| !text.is_empty())
                .ok_or_else(|| AppError::Clipboard("no recognized image text".into()))?;
            write_fragment(&core.0, &text)?;
            mark_item_reused_if_enabled(&core.0, &pool, &id, item.kind).await?;
            Ok(CopyOutcome {
                hide_window: settings.content.copy_then_hide_window,
            })
        })
        .await
    }

    /// 粘贴的 core 部分：按「粘贴时去除格式」「粘贴文件为路径」设置写回剪贴板并记一次使用。
    /// 宿主随后隐藏窗口（固定时让出键盘焦点）、等窗口真正让出焦点，再注入粘贴按键。
    pub async fn prepare_paste(&self, id: &str, plain: bool) -> Result<()> {
        let core = self.clone();
        let id = id.to_owned();
        self.hop(async move {
            let pool = core.0.db.pool().await;
            let item = find_required(&pool, &id).await?;
            let content = core.0.settings.snapshot().clipboard.content;
            let write_plain = should_write_plain_for_paste(
                plain,
                item.kind,
                content.paste_plain,
                content.paste_files_as_path,
            );

            write_item(&core.0, &item, write_plain)?;
            mark_item_reused_if_enabled(&core.0, &pool, &id, item.kind).await
        })
        .await
    }

    /// 拆词面板的数据。非文本记录、按设置脱敏展示的敏感内容不拆，返回用户可读错误。
    pub async fn split_item(&self, id: &str) -> Result<WordSplit> {
        let core = self.clone();
        let id = id.to_owned();
        self.hop(async move {
            let pool = core.0.db.pool().await;
            let item = find_required(&pool, &id).await?;
            let lang = core.0.language();

            if item.kind != ClipboardKind::Text {
                return Err(AppError::Clipboard(
                    label(lang, Key::SplitTextOnly).to_owned(),
                ));
            }
            let redact = core
                .0
                .settings
                .snapshot()
                .clipboard
                .sensitive
                .redact_secrets;
            if redact && item.is_sensitive {
                return Err(AppError::Clipboard(
                    label(lang, Key::SplitSensitiveRedacted).to_owned(),
                ));
            }

            Ok(split_words(clipboard::fragment_source(&item)))
        })
        .await
    }

    /// 把选中的片段（快捷信息 / 拆词选区）写回剪贴板（不粘贴）。
    pub async fn copy_fragment(
        &self,
        id: &str,
        fragment: ClipboardFragment,
    ) -> Result<CopyOutcome> {
        let core = self.clone();
        let id = id.to_owned();
        self.hop(async move {
            let pool = core.0.db.pool().await;
            let (kind, text) = load_fragment_text(&core.0, &pool, &id, &fragment).await?;

            write_fragment(&core.0, &text)?;
            mark_item_reused_if_enabled(&core.0, &pool, &id, kind).await?;
            Ok(CopyOutcome {
                hide_window: core
                    .0
                    .settings
                    .snapshot()
                    .clipboard
                    .content
                    .copy_then_hide_window,
            })
        })
        .await
    }

    /// 粘贴片段的 core 部分，宿主随后的步骤同 [`Core::prepare_paste`]。
    pub async fn prepare_paste_fragment(
        &self,
        id: &str,
        fragment: ClipboardFragment,
    ) -> Result<()> {
        let core = self.clone();
        let id = id.to_owned();
        self.hop(async move {
            let pool = core.0.db.pool().await;
            let (kind, text) = load_fragment_text(&core.0, &pool, &id, &fragment).await?;

            write_fragment(&core.0, &text)?;
            mark_item_reused_if_enabled(&core.0, &pool, &id, kind).await
        })
        .await
    }

    /// 快速粘贴的 core 部分：取「全部」视图里第 `offset` 条（从 0 起，置顶在前，其余按列表排序设置），
    /// 按默认粘贴规则写回并记一次使用。历史不足或上一次快速粘贴还没结束时返回 `None`。
    ///
    /// 宿主拿到凭据后：等用户松开修饰键（超时就放弃，把内容留在剪贴板上）→ 剪贴板窗口可见时
    /// 先隐藏（固定时让出键盘焦点）并等一拍 → 注入粘贴按键 → 丢弃凭据。
    pub async fn prepare_quick_paste(&self, offset: i64) -> Result<Option<QuickPasteTicket>> {
        let Some(guard) = QuickPasteGuard::acquire(&self.0) else {
            return Ok(None);
        };

        let core = self.clone();
        self.hop(async move {
            let pool = core.0.db.pool().await;
            let content = core.0.settings.snapshot().clipboard.content;
            let Some(id) = find_item_id_at(&pool, content.sort, offset).await? else {
                return Ok(None);
            };
            let Some(item) = find_item_by_id(&pool, &id).await? else {
                return Ok(None);
            };
            let write_plain = should_write_plain_for_paste(
                false,
                item.kind,
                content.paste_plain,
                content.paste_files_as_path,
            );

            write_item(&core.0, &item, write_plain)?;
            mark_item_reused_if_enabled(&core.0, &pool, &id, item.kind).await?;
            Ok(Some(guard.into_ticket(&item)))
        })
        .await
    }

    /// 读取当前系统剪贴板并准备一次纯文本粘贴，不访问历史数据库。
    /// 当前剪贴板的可粘贴内容统一写成单一纯文本，再交给粘贴注入。
    pub async fn prepare_plain_paste_from_clipboard(&self) -> Result<Option<QuickPasteTicket>> {
        let Some(guard) = QuickPasteGuard::acquire(&self.0) else {
            return Ok(None);
        };

        let core = self.clone();
        self.hop(async move {
            let payload = {
                let backend = core.0.clipboard()?;
                ClipboardReader::with_backend(&*backend).read_current()?
            };
            let Some(payload) = payload else {
                return Ok(None);
            };
            let Some(text) = plain_paste_decision(&payload) else {
                return Ok(None);
            };
            write_fragment(&core.0, &text)?;
            let kind = match payload {
                ClipboardPayload::Files(_) => ClipboardKind::Files,
                _ => ClipboardKind::Text,
            };
            Ok(Some(guard.into_plain_ticket(kind)))
        })
        .await
    }

    /// 手动读取当前剪贴板并入库（「重新读取」）。与监听走同一条入库路径；
    /// 不按忽略列表过滤，也不做回环抑制判断（与 1.x 相同）。剪贴板为空或按设置不收录时返回 `None`。
    pub async fn read_clipboard_now(&self) -> Result<Option<CapturedItem>> {
        let core = self.clone();
        self.hop(async move {
            let source = core.0.platform().frontmost_app();
            let settings = core.0.settings.snapshot();
            let item = {
                let backend = core.0.clipboard()?;
                let payload = ClipboardReader::with_backend(&*backend)
                    .read_with_capture(&settings.clipboard.capture)?;
                match payload {
                    Some(payload) => build_item_with_settings(
                        &core.0.images,
                        &payload,
                        &settings.clipboard.capture,
                        &settings.clipboard.sensitive,
                        settings.clipboard.content.copy_plain,
                    )?,
                    None => None,
                }
            };
            let Some(mut item) = item else {
                return Ok(None);
            };

            let source_app =
                source.map(|src| materialize_source(&core.0.app_icons, Some(&core.0.apps), src));
            if let Some(src) = &source_app {
                item.source_app_id = Some(src.id.clone());
            }

            let result =
                clipboard::persist::persist_and_notify(&core.0, &item, source_app.as_ref()).await?;
            Ok(Some(CapturedItem {
                id: result.id,
                item,
                deduplicated: result.deduplicated,
            }))
        })
        .await
    }

    /// 翻转收藏，返回新状态。不发事件，界面自己更新。
    pub async fn toggle_favorite(&self, id: &str) -> Result<bool> {
        let core = self.clone();
        let id = id.to_owned();
        self.hop(async move { toggle_item_favorite(&core.0.db.pool().await, &id).await })
            .await
    }

    /// 翻转置顶，返回新状态。不刷新 `updated_at`，不污染最近使用排序。
    pub async fn toggle_pinned(&self, id: &str) -> Result<bool> {
        let core = self.clone();
        let id = id.to_owned();
        self.hop(async move { toggle_item_pinned(&core.0.db.pool().await, &id).await })
            .await
    }

    /// 把记录放到置顶或收藏分区中锚点记录的前面 / 后面，并通知所有列表刷新。
    pub async fn reorder_item(
        &self,
        section: ReorderSection,
        id: &str,
        anchor: ReorderAnchor,
    ) -> Result<()> {
        let core = self.clone();
        let id = id.to_owned();
        self.hop(async move {
            reorder_item(&core.0.db.pool().await, section, &id, anchor).await?;
            core.0.events.emit(CoreEvent::ClipboardReloaded);
            Ok(())
        })
        .await
    }

    /// 写入备注：去前后空白，空串清空。写入非空备注且开了「备注自动收藏」时顺带收藏。
    pub async fn update_note(&self, id: &str, note: Option<String>) -> Result<UpdateNoteResult> {
        let core = self.clone();
        let id = id.to_owned();
        self.hop(async move {
            let pool = core.0.db.pool().await;
            let normalized = normalize_note(note.as_deref());
            update_item_note(&pool, &id, normalized).await?;

            let mut auto_favorited = false;
            if normalized.is_some() && core.0.settings.snapshot().clipboard.content.auto_favorite {
                mark_item_favorite(&pool, &id).await?;
                auto_favorited = true;
            }
            Ok(UpdateNoteResult {
                note: normalized.map(str::to_owned),
                auto_favorited,
            })
        })
        .await
    }

    /// 编辑文本记录：按采集规则重建内容字段，保留元数据，不发布同步或复用事件。
    pub async fn update_text_content(&self, id: &str, content: String) -> Result<()> {
        let core = self.clone();
        let id = id.to_owned();
        self.hop(async move {
            let pool = core.0.db.pool().await;
            let mut item = find_required(&pool, &id).await?;
            if item.kind != ClipboardKind::Text {
                return Err(AppError::Clipboard(
                    "Only text records can be edited".to_owned(),
                ));
            }
            if content.trim().is_empty() {
                return Err(AppError::Clipboard("Content cannot be empty".to_owned()));
            }
            let current = if matches!(
                item.sub_kind,
                Some(
                    crate::db::models::ClipboardSubKind::Html
                        | crate::db::models::ClipboardSubKind::Rtf
                )
            ) {
                item.search_text.as_deref().unwrap_or(&item.content)
            } else {
                &item.content
            };
            if content == current {
                return Ok(());
            }

            clipboard::rewrite_text_content(&mut item, &content);
            update_item_text_content(&pool, &item).await
        })
        .await
    }

    /// 把记录移到自定义分组；分组不存在时报错。
    pub async fn set_item_group(&self, id: &str, group_id: &str) -> Result<()> {
        let core = self.clone();
        let id = id.to_owned();
        let group_id = group_id.to_owned();
        self.hop(async move {
            let pool = core.0.db.pool().await;
            let exists = crate::db::groups::list_groups(&pool)
                .await?
                .iter()
                .any(|group| group.id == group_id);
            if !exists {
                return Err(AppError::Clipboard("分组不存在".to_owned()));
            }

            update_item_group(&pool, &id, Some(&group_id)).await
        })
        .await
    }

    /// 把记录移出自定义分组（回到未分组）。不发事件。
    pub async fn clear_item_group(&self, id: &str) -> Result<()> {
        let core = self.clone();
        let id = id.to_owned();
        self.hop(async move { update_item_group(&core.0.db.pool().await, &id, None).await })
            .await
    }

    /// 删除一条记录；图片记录连带删除原图与缩略图（删文件失败只记日志）。不发事件。
    pub async fn delete_item(&self, id: &str) -> Result<()> {
        let core = self.clone();
        let id = id.to_owned();
        self.hop(async move {
            let _ocr = core.0.ocr.suspend().await;
            let pool = core.0.db.pool().await;
            if let Some(file_name) = delete_item(&pool, &id).await? {
                if let Err(err) = core.0.images.remove(&file_name) {
                    log::warn!("remove deleted image {file_name} failed: {err}");
                }
            }
            Ok(())
        })
        .await
    }

    /// 批量删除多选的记录，连带删除图片文件，返回实际删除条数。不发事件；
    /// 收藏 / 置顶保护由界面在选择时过滤。
    pub async fn delete_items(&self, ids: Vec<String>) -> Result<u64> {
        let core = self.clone();
        self.hop(async move {
            let _ocr = core.0.ocr.suspend().await;
            let pool = core.0.db.pool().await;
            let outcome = delete_items(&pool, &ids).await?;

            for file_name in &outcome.image_files {
                if let Err(err) = core.0.images.remove(file_name) {
                    log::warn!("remove deleted image {file_name} failed: {err}");
                }
            }
            Ok(outcome.removed)
        })
        .await
    }

    /// 清空历史（可选连收藏、置顶一起），删除图片文件，发 [`CoreEvent::ClipboardCleaned`]。
    pub async fn clear_items(&self, delete_favorites: bool, delete_pinned: bool) -> Result<u64> {
        let core = self.clone();
        self.hop(async move {
            let _ocr = core.0.ocr.suspend().await;
            let pool = core.0.db.pool().await;
            let outcome = clear_items(&pool, delete_favorites, delete_pinned).await?;

            for file_name in &outcome.image_files {
                if let Err(err) = core.0.images.remove(file_name) {
                    log::warn!("remove cleared image {file_name} failed: {err}");
                }
            }
            core.0.events.emit(CoreEvent::ClipboardCleaned {
                removed: outcome.removed,
            });
            Ok(outcome.removed)
        })
        .await
    }

    /// 清理某个内容类别或来源应用下的记录（收藏与置顶保留），返回删除条数；
    /// 删到记录时连带删除图片文件并发 [`CoreEvent::ClipboardCleaned`]。
    pub async fn clear_items_in_scope(&self, scope: ClearScope) -> Result<u64> {
        let core = self.clone();
        self.hop(async move {
            let _ocr = core.0.ocr.suspend().await;
            let pool = core.0.db.pool().await;
            let outcome = clear_scope(&pool, &scope).await?;
            clipboard::cleanup::apply_outcome(&core.0, &outcome, "scoped");
            Ok(outcome.removed)
        })
        .await
    }

    /// 全选 / 区间选择用：按列表同款过滤与排序返回全部匹配记录的 id 与收藏 / 置顶标记。
    pub async fn list_item_refs(&self, query: ClipboardItemQuery) -> Result<Vec<ClipboardItemRef>> {
        let core = self.clone();
        self.hop(async move {
            let mut query = query;
            query.ocr_enabled = core.ocr_enabled();
            list_item_refs(&core.0.db.pool().await, &query).await
        })
        .await
    }

    /// 「打开链接」/「发送邮件」的目标；内容为空时返回 `None`。宿主用系统默认浏览器 / 邮件客户端打开。
    pub async fn link_target(&self, id: &str, mailto: bool) -> Result<Option<String>> {
        let core = self.clone();
        let id = id.to_owned();
        self.hop(async move {
            let item = find_required(&core.0.db.pool().await, &id).await?;
            Ok(link_target(&item, mailto))
        })
        .await
    }

    /// 「在文件管理器中显示」的目标路径；内容为空时返回 `None`。
    pub async fn reveal_target(&self, id: &str) -> Result<Option<String>> {
        let core = self.clone();
        let id = id.to_owned();
        self.hop(async move {
            let item = find_required(&core.0.db.pool().await, &id).await?;
            Ok(reveal_target(&item))
        })
        .await
    }

    /// 图片另存：校验记录是图片且原图还在，返回原图路径与默认文件名（本机时区的复制时间）。
    pub async fn prepare_image_save(&self, id: &str) -> Result<ImageSave> {
        let core = self.clone();
        let id = id.to_owned();
        self.hop(async move {
            let item = find_required(&core.0.db.pool().await, &id).await?;
            if item.kind != ClipboardKind::Image {
                return Err(AppError::Clipboard(
                    "selected item is not an image".to_owned(),
                ));
            }
            validate_image_file_name(&item.content)?;

            let source = core.0.images.origin_path(&item.content);
            if !source.is_file() {
                return Err(AppError::Clipboard("image file does not exist".to_owned()));
            }
            Ok(ImageSave {
                source,
                default_file_name: default_saved_image_file_name(&item.created_at, &Local),
            })
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use chrono::FixedOffset;

    use super::*;
    use crate::presenter::tests::{image_item, text_item};

    #[test]
    fn copy_plain_default_only_affects_text_items() {
        assert!(should_write_plain_for_copy(
            false,
            ClipboardKind::Text,
            true
        ));
        assert!(!should_write_plain_for_copy(
            false,
            ClipboardKind::Files,
            true
        ));
        assert!(!should_write_plain_for_copy(
            false,
            ClipboardKind::Image,
            true
        ));
        assert!(!should_write_plain_for_copy(
            false,
            ClipboardKind::Text,
            false
        ));
    }

    #[test]
    fn force_plain_copy_overrides_item_kind() {
        assert!(should_write_plain_for_copy(
            true,
            ClipboardKind::Files,
            false
        ));
        assert!(should_write_plain_for_copy(
            true,
            ClipboardKind::Image,
            false
        ));
    }

    #[test]
    fn paste_plain_defaults_follow_item_kind() {
        assert!(should_write_plain_for_paste(
            false,
            ClipboardKind::Text,
            true,
            false,
        ));
        assert!(should_write_plain_for_paste(
            false,
            ClipboardKind::Files,
            false,
            true,
        ));
        assert!(!should_write_plain_for_paste(
            false,
            ClipboardKind::Files,
            true,
            false,
        ));
        assert!(!should_write_plain_for_paste(
            false,
            ClipboardKind::Image,
            true,
            true,
        ));
    }

    #[test]
    fn force_plain_paste_overrides_item_kind() {
        assert!(should_write_plain_for_paste(
            true,
            ClipboardKind::Image,
            false,
            false,
        ));
    }

    // 快速粘贴与窗口内默认粘贴一致：文本按「粘贴时去除格式」、文件按「粘贴为路径」决定是否写纯文本。
    #[test]
    fn plain_paste_follows_default_paste_settings() {
        let mut content = crate::settings::Content::default();
        let writes_plain = |content: &crate::settings::Content, kind| {
            should_write_plain_for_paste(
                false,
                kind,
                content.paste_plain,
                content.paste_files_as_path,
            )
        };
        assert!(!writes_plain(&content, ClipboardKind::Text));
        assert!(!writes_plain(&content, ClipboardKind::Files));

        content.paste_plain = true;
        assert!(writes_plain(&content, ClipboardKind::Text));
        assert!(!writes_plain(&content, ClipboardKind::Image));

        content.paste_files_as_path = true;
        assert!(writes_plain(&content, ClipboardKind::Files));
    }

    #[test]
    fn plain_paste_decision_covers_text_rich_text_files_and_empty_payloads() {
        use crate::clipboard::{ImagePayload, TextPayload};

        let plain = ClipboardPayload::Text(TextPayload {
            text: "hello".to_owned(),
            html: None,
            rtf: None,
        });
        assert_eq!(plain_paste_decision(&plain), Some("hello".to_owned()));

        for rich in [Some("<b>hello</b>".to_owned()), None] {
            let payload = ClipboardPayload::Text(TextPayload {
                text: "hello".to_owned(),
                html: rich,
                rtf: Some(r"{\rtf1 hello}".to_owned()),
            });
            assert_eq!(plain_paste_decision(&payload), Some("hello".to_owned()));
        }

        let files = ClipboardPayload::Files(vec!["C:/a.txt".to_owned(), "C:/b.txt".to_owned()]);
        assert_eq!(
            plain_paste_decision(&files),
            Some("C:/a.txt\nC:/b.txt".to_owned())
        );
        assert_eq!(
            plain_paste_decision(&ClipboardPayload::Text(TextPayload {
                text: String::new(),
                html: Some("<b>".to_owned()),
                rtf: None,
            })),
            None
        );
        assert_eq!(
            plain_paste_decision(&ClipboardPayload::Image(ImagePayload {
                bytes: vec![1],
                width: 1,
                height: 1,
            })),
            None
        );
    }

    #[test]
    fn link_targets_are_normalized() {
        let mut item = text_item(None, false);

        item.content = "  www.example.com ".to_owned();
        assert_eq!(
            link_target(&item, false).as_deref(),
            Some("https://www.example.com")
        );
        item.content = "user@example.com".to_owned();
        assert_eq!(
            link_target(&item, true).as_deref(),
            Some("mailto:user@example.com")
        );
        item.content = "https://a.b/c".to_owned();
        assert_eq!(link_target(&item, false).as_deref(), Some("https://a.b/c"));
        item.content = "   ".to_owned();
        assert_eq!(link_target(&item, false), None);
    }

    #[test]
    fn reveal_targets_use_the_first_file_or_the_trimmed_path() {
        let mut files = image_item();
        files.kind = ClipboardKind::Files;
        files.content = "\nC:/a.txt\nC:/b.txt".to_owned();
        assert_eq!(reveal_target(&files).as_deref(), Some("C:/a.txt"));

        let mut path = text_item(None, false);
        path.content = " C:/Users/demo ".to_owned();
        assert_eq!(reveal_target(&path).as_deref(), Some("C:/Users/demo"));

        path.content = String::new();
        assert_eq!(reveal_target(&path), None);
    }

    #[test]
    fn saved_image_names_use_the_local_copy_time() {
        let tz = FixedOffset::east_opt(8 * 3600).unwrap();
        let created_at = DateTime::parse_from_rfc3339("2026-10-01T16:05:09Z")
            .unwrap()
            .with_timezone(&Utc);

        assert_eq!(
            default_saved_image_file_name(&created_at, &tz),
            "KwikPaste-image-20261002-000509.png"
        );
    }

    #[test]
    fn notes_are_trimmed_and_blank_notes_clear() {
        assert_eq!(normalize_note(Some("  备注 ")), Some("备注"));
        assert_eq!(normalize_note(Some(" \n ")), None);
        assert_eq!(normalize_note(None), None);
    }
}
