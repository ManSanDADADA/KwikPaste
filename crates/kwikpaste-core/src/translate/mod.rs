//! 文本记录的翻译：扩展进程负责联网，这里管队列、译文表和给界面的数据。
//! 扩展没装或停用时不起线程，也不读译文表。

mod config;
mod queue;
#[cfg(test)]
mod tests;

use sqlx::SqlitePool;

pub use config::TranslateAiConfig;
pub use kwikpaste_ext_protocol::TranslateFailure;
pub(crate) use queue::TranslateService;

use crate::clipboard::{detect_text_sub_kind, SUMMARY_MAX_CHARS};
use crate::db::items::{content_hash, find_item_by_id};
use crate::db::models::{ClipboardItem, ClipboardKind, ClipboardSubKind};
use crate::error::AppError;
use crate::i18n::commands::{label, Key};
use crate::ops::CopyOutcome;
use crate::presenter::{ClipboardAction, ClipboardItemView};
use crate::root::CoreInner;
use crate::settings::{Language, TranslateEngineKind, TranslateSettings};
use crate::{Core, CoreEvent, Result};

/// 第 1 期支持的语言（BCP 47），顺序就是设置页下拉的顺序。
pub const LANGUAGES: [&str; 11] = [
    "zh-Hans", "zh-Hant", "en", "ja", "ko", "fr", "de", "es", "ru", "pt", "it",
];
/// 可翻译文本的字符数上限；三家服务单次都能翻这么长，所以不分段。
pub const MAX_CHARS: usize = 5000;
/// 卡片上译文的字符数上限，与摘要一致。
const CARD_CHARS: i64 = 512;

/// 一条记录的译文全文（预览窗用）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemTranslation {
    pub text: String,
    /// 服务报告的原文语种；AI 接口没有。
    pub source_lang: Option<String>,
    pub target_lang: String,
    /// `volc`、`transmart`，或 AI 接口的模型名。
    pub provider: String,
}

/// 翻译中和失败只记在内存里，重启后清空。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum TranslationState {
    #[default]
    Idle,
    Pending,
    Failed {
        kind: TranslateFailure,
        message: String,
    },
}

/// 预览窗要的全部：能不能翻译、已有的译文、当前状态。扩展没启用时是默认值。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TranslationView {
    pub translatable: bool,
    pub translation: Option<ItemTranslation>,
    pub state: TranslationState,
}

/// 实际使用的首选语言和第二语言。首选语言留空时跟随界面语言；第二语言留空、不在列表里
/// 或与首选语言相同时取英语，首选语言是英语时取简体中文。
pub fn languages(settings: &TranslateSettings, ui: Language) -> (String, String) {
    let listed = |code: &str| LANGUAGES.contains(&code);
    let first = if listed(&settings.first_language) {
        settings.first_language.clone()
    } else {
        match ui {
            Language::ZhCN => "zh-Hans",
            Language::EnUS => "en",
        }
        .to_owned()
    };
    let second = if listed(&settings.second_language) && settings.second_language != first {
        settings.second_language.clone()
    } else if first == "en" {
        "zh-Hans".to_owned()
    } else {
        "en".to_owned()
    };
    (first, second)
}

/// 类型上能不能翻译；长度另算。
pub(crate) fn eligible(item: &ClipboardItem) -> bool {
    if item.kind != ClipboardKind::Text || item.is_sensitive {
        return false;
    }
    let mut sub_kind = item.sub_kind;
    if matches!(
        sub_kind,
        Some(ClipboardSubKind::Html | ClipboardSubKind::Rtf)
    ) {
        // 和列表卡片一样：纯文本整段就是链接、邮箱等的富文本按那个类型算。
        sub_kind = item
            .summary
            .as_deref()
            .filter(|summary| summary.chars().count() < SUMMARY_MAX_CHARS)
            .and_then(detect_text_sub_kind);
    }
    !matches!(
        sub_kind,
        Some(
            ClipboardSubKind::Url
                | ClipboardSubKind::Email
                | ClipboardSubKind::Color
                | ClipboardSubKind::Path
        )
    )
}

/// 这条带全文的记录能不能翻译。富文本按它的纯文本算。
pub(crate) fn translatable(item: &ClipboardItem) -> bool {
    let chars = crate::clipboard::fragment_source(item).chars().count();
    eligible(item) && (1..=MAX_CHARS).contains(&chars)
}

/// 存一份译文，覆盖旧的。只在记录还在、内容没变时写入，返回是否写了。从不更新原记录。
async fn save(
    pool: &SqlitePool,
    id: &str,
    hash: &str,
    text: &str,
    source: Option<&str>,
    target: &str,
    provider: &str,
) -> sqlx::Result<bool> {
    let result = sqlx::query(
        "INSERT INTO item_translations(item_id, source_hash, text, source_lang, target_lang, provider, created_at, updated_at) \
         SELECT id, content_hash, ?, ?, ?, ?, strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now') \
         FROM clipboard_items WHERE id = ? AND content_hash = ? \
         ON CONFLICT(item_id) DO UPDATE SET source_hash = excluded.source_hash, text = excluded.text, \
         source_lang = excluded.source_lang, target_lang = excluded.target_lang, provider = excluded.provider, \
         updated_at = excluded.updated_at",
    )
    .bind(text)
    .bind(source)
    .bind(target)
    .bind(provider)
    .bind(id)
    .bind(hash)
    .execute(pool)
    .await?;
    Ok(result.rows_affected() > 0)
}

/// 读译文；`hash` 是记录现在的 `content_hash`，原文改过以后旧译文读不出来。
pub(crate) async fn load(
    pool: &SqlitePool,
    id: &str,
    hash: &str,
) -> sqlx::Result<Option<ItemTranslation>> {
    let row: Option<(String, Option<String>, String, String)> = sqlx::query_as(
        "SELECT text, source_lang, target_lang, provider FROM item_translations WHERE item_id = ? AND source_hash = ?",
    )
    .bind(id)
    .bind(hash)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(
        |(text, source_lang, target_lang, provider)| ItemTranslation {
            text,
            source_lang,
            target_lang,
            provider,
        },
    ))
}

/// 列表视图补上译文开头、翻译中标记和翻译动作。扩展没启用时不查库。
pub(crate) async fn attach_view(
    core: &CoreInner,
    pool: &SqlitePool,
    view: &mut ClipboardItemView,
) -> Result<()> {
    if !eligible(&view.item) || core.extensions.resolve("translate").is_none() {
        return Ok(());
    }
    // 列表行不带全文：字符数和译文开头在库里取。
    let row: Option<(i64, Option<String>)> = sqlx::query_as(
        "SELECT LENGTH(CASE WHEN i.sub_kind IN ('html', 'rtf') THEN COALESCE(i.search_text, i.content) ELSE i.content END), \
         (SELECT substr(t.text, 1, ?) FROM item_translations t WHERE t.item_id = i.id AND t.source_hash = i.content_hash) \
         FROM clipboard_items i WHERE i.id = ?",
    )
    .bind(CARD_CHARS)
    .bind(&view.item.id)
    .fetch_optional(pool)
    .await
    .map_err(anyhow::Error::from)?;
    let Some((chars, translation)) = row else {
        return Ok(());
    };
    if !(1..=MAX_CHARS as i64).contains(&chars) {
        return Ok(());
    }

    view.translating = core.translate.pending(&view.item.id);
    let actions: &[ClipboardAction] = if translation.is_some() {
        &[
            ClipboardAction::CopyTranslation,
            ClipboardAction::PasteTranslation,
        ]
    } else if view.translating {
        &[]
    } else {
        &[ClipboardAction::Translate]
    };
    view.translation = translation;
    // 接在第一组末尾：拆词之后；没有拆词时接在复制之后。
    let at = [ClipboardAction::SplitWords, ClipboardAction::Copy]
        .iter()
        .find_map(|anchor| {
            view.available_actions
                .iter()
                .position(|action| action == anchor)
        })
        .map_or(view.available_actions.len(), |index| index + 1);
    view.available_actions
        .splice(at..at, actions.iter().copied());
    Ok(())
}

/// 把译文写进剪贴板。这是新内容，不登记回环抑制：监听会把它记成一条新的文本记录，
/// 先记下指纹，自动模式不会把它再翻回去。
async fn write_translation(core: &Core, id: &str) -> Result<()> {
    let Some(translation) = core.translation_view(id).await?.translation else {
        return Err(AppError::Clipboard(
            label(core.0.language(), Key::TranslationMissing).to_owned(),
        ));
    };
    core.0
        .translate
        .mark_written(content_hash(ClipboardKind::Text, translation.text.trim()));
    core.0.clipboard()?.set_text(translation.text)
}

impl Core {
    pub fn translate_enabled(&self) -> bool {
        self.resolve_extension("translate").is_some()
    }

    /// 手动翻译一条记录；已有译文时是重新翻译。这里只校验和排队，
    /// 结果经 [`CoreEvent::TranslationChanged`] 通知。
    pub async fn translate_item(&self, id: &str) -> Result<()> {
        let core = self.clone();
        let id = id.to_owned();
        self.hop(async move {
            let lang = core.0.language();
            let refuse = |key: Key| AppError::Clipboard(label(lang, key).to_owned());
            if !core.translate_enabled() {
                return Err(refuse(Key::TranslateUnavailable));
            }
            let pool = core.0.db.pool().await;
            let item = find_item_by_id(&pool, &id).await?;
            if !item.as_ref().is_some_and(translatable) {
                return Err(refuse(Key::TranslateNotTranslatable));
            }
            let settings = core.0.settings.snapshot().extensions.translate;
            if settings.engine == TranslateEngineKind::Ai && !core.translate_ai_config().complete()
            {
                return Err(refuse(Key::TranslateNotConfigured));
            }
            core.0.translate.enqueue(id, false);
            Ok(())
        })
        .await
    }

    /// 预览窗用的译文全文和状态。扩展没启用、记录不存在时是默认值。
    pub async fn translation_view(&self, id: &str) -> Result<TranslationView> {
        let core = self.clone();
        let id = id.to_owned();
        self.hop(async move {
            if !core.translate_enabled() {
                return Ok(TranslationView::default());
            }
            let pool = core.0.db.pool().await;
            let Some(item) = find_item_by_id(&pool, &id).await? else {
                return Ok(TranslationView::default());
            };
            // 翻译后才变成敏感的记录（新加的敏感规则）也不再给出译文，与卡片一致。
            if !eligible(&item) {
                return Ok(TranslationView::default());
            }
            Ok(TranslationView {
                translatable: translatable(&item),
                translation: load(&pool, &id, &item.content_hash)
                    .await
                    .map_err(anyhow::Error::from)?,
                state: core.0.translate.status(&id),
            })
        })
        .await
    }

    /// 复制译文（不粘贴）。返回设置是否要求随后隐藏窗口。
    pub async fn copy_translation(&self, id: &str) -> Result<CopyOutcome> {
        let core = self.clone();
        let id = id.to_owned();
        self.hop(async move {
            write_translation(&core, &id).await?;
            Ok(CopyOutcome {
                hide_window: core.settings().clipboard.content.copy_then_hide_window,
            })
        })
        .await
    }

    /// 粘贴译文的 core 部分，宿主随后的步骤同 [`Core::prepare_paste`]。
    pub async fn prepare_paste_translation(&self, id: &str) -> Result<()> {
        let core = self.clone();
        let id = id.to_owned();
        self.hop(async move { write_translation(&core, &id).await })
            .await
    }

    /// 删掉所有已存的译文。
    pub async fn clear_translations(&self) -> Result<()> {
        let core = self.clone();
        self.hop(async move {
            sqlx::query("DELETE FROM item_translations")
                .execute(&core.0.db.pool().await)
                .await
                .map_err(anyhow::Error::from)?;
            core.0
                .events
                .emit(CoreEvent::TranslationChanged { id: None });
            Ok(())
        })
        .await
    }

    /// AI 接口的配置；还没填过时是空的。
    pub fn translate_ai_config(&self) -> TranslateAiConfig {
        config::load(&self.0.paths)
    }

    pub async fn set_translate_ai_config(&self, ai: TranslateAiConfig) -> Result<()> {
        let core = self.clone();
        self.hop(async move { Ok(config::save(&core.0.paths, &ai)?) })
            .await
    }
}
