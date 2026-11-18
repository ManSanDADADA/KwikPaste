use super::*;
use crate::clipboard::{ClipboardPayload, TextPayload};
use crate::testing::{block_on, Fixture};

fn text_item(core: &Core, text: &str) -> ClipboardItem {
    let payload = ClipboardPayload::Text(TextPayload {
        text: text.into(),
        html: None,
        rtf: None,
    });
    core.build_item(&payload).unwrap().unwrap()
}

/// 放一个占位文件当扩展：解析路径只看文件在不在，这些测试不会启动它。
fn install(fixture: &Fixture, core: &Core) {
    let staged = fixture.root().join("translate-staged");
    std::fs::write(&staged, b"stub").unwrap();
    block_on(core.install_extension(
        "translate",
        "1.0.0",
        crate::extensions::TRANSLATE_PROTOCOL,
        &staged,
    ))
    .unwrap();
}

fn save_translation(core: &Core, id: &str, text: &'static str) {
    let id = id.to_owned();
    block_on(core.hop({
        let core = core.clone();
        async move {
            let pool = core.0.db.pool().await;
            let item = find_item_by_id(&pool, &id).await?.unwrap();
            let saved = save(
                &pool,
                &id,
                &item.content_hash,
                text,
                Some("en"),
                "zh-Hans",
                "volc",
            )
            .await
            .unwrap();
            assert!(saved);
            Ok(())
        }
    }))
    .unwrap();
}

#[test]
fn only_plain_non_sensitive_text_within_the_limit_is_translatable() {
    let fixture = Fixture::new();
    let core = fixture.start();
    assert!(translatable(&text_item(&core, "Hello there, general.")));
    assert!(translatable(&text_item(&core, &"字".repeat(MAX_CHARS))));
    assert!(!translatable(&text_item(
        &core,
        &"字".repeat(MAX_CHARS + 1)
    )));
    assert!(!translatable(&text_item(&core, "https://example.com/path")));
    assert!(!translatable(&text_item(&core, "#ff8800")));
    let mut sensitive = text_item(&core, "plain words");
    sensitive.is_sensitive = true;
    assert!(!translatable(&sensitive));
    block_on(core.shutdown()).unwrap();
}

#[test]
fn translation_shows_only_for_the_current_content_while_enabled() {
    let fixture = Fixture::new();
    let core = fixture.start();
    let item = text_item(&core, "The clipboard manager keeps everything you copy.");
    let id = block_on(core.store_item(item, None)).unwrap().id;
    save_translation(&core, &id, "剪贴板管理器会保存你复制的所有内容。");

    // 扩展没装：不显示译文，没有翻译动作，手动翻译直接报错。
    let card = block_on(core.list_item(&id)).unwrap().unwrap();
    assert_eq!(card.translation, None);
    assert!(!card.available_actions.contains(&ClipboardAction::Translate));
    assert_eq!(
        block_on(core.translation_view(&id)).unwrap(),
        TranslationView::default()
    );
    assert!(block_on(core.translate_item(&id)).is_err());

    install(&fixture, &core);
    let card = block_on(core.list_item(&id)).unwrap().unwrap();
    assert_eq!(
        card.translation.as_deref(),
        Some("剪贴板管理器会保存你复制的所有内容。")
    );
    let split = card
        .available_actions
        .iter()
        .position(|action| *action == ClipboardAction::SplitWords)
        .unwrap();
    assert_eq!(
        card.available_actions[split + 1..split + 3],
        [
            ClipboardAction::CopyTranslation,
            ClipboardAction::PasteTranslation
        ]
    );
    let view = block_on(core.translation_view(&id)).unwrap();
    assert!(view.translatable);
    assert_eq!(view.state, TranslationState::Idle);
    assert_eq!(view.translation.unwrap().provider, "volc");

    // 复制译文：写进剪贴板的是译文全文。
    block_on(core.copy_translation(&id)).unwrap();
    assert_eq!(
        fixture.clipboard.snapshot().text.as_deref(),
        Some("剪贴板管理器会保存你复制的所有内容。")
    );

    // 改了原文：旧译文不再显示，动作变回「翻译」；恢复原文后又对得上。
    block_on(core.update_text_content(&id, "Something else entirely.".into())).unwrap();
    let card = block_on(core.list_item(&id)).unwrap().unwrap();
    assert_eq!(card.translation, None);
    assert!(card.available_actions.contains(&ClipboardAction::Translate));
    assert!(block_on(core.copy_translation(&id)).is_err());
    block_on(core.restore_original_content(&id)).unwrap();
    assert!(block_on(core.list_item(&id))
        .unwrap()
        .unwrap()
        .translation
        .is_some());

    // 清除译文。
    block_on(core.clear_translations()).unwrap();
    assert_eq!(
        block_on(core.translation_view(&id)).unwrap().translation,
        None
    );

    // 翻译后才变成敏感的记录：预览和复制都不再给出译文。
    save_translation(&core, &id, "剪贴板管理器会保存你复制的所有内容。");
    block_on(core.hop({
        let core = core.clone();
        let id = id.clone();
        async move {
            sqlx::query("UPDATE clipboard_items SET is_sensitive = 1 WHERE id = ?")
                .bind(id)
                .execute(&core.0.db.pool().await)
                .await
                .unwrap();
            Ok(())
        }
    }))
    .unwrap();
    assert_eq!(
        block_on(core.translation_view(&id)).unwrap(),
        TranslationView::default()
    );
    assert!(block_on(core.copy_translation(&id)).is_err());
    block_on(core.shutdown()).unwrap();
}

#[test]
fn stopped_queue_drops_new_jobs_until_released() {
    let fixture = Fixture::new();
    let core = fixture.start();
    let _paused = block_on(core.0.translate.stop());
    core.0.translate.enqueue("held".into(), false);
    assert!(!core.0.translate.pending("held"));
    block_on(core.shutdown()).unwrap();
}

#[test]
fn languages_fall_back_to_the_interface_language_and_stay_distinct() {
    let mut settings = TranslateSettings::default();
    assert_eq!(
        languages(&settings, Language::ZhCN),
        ("zh-Hans".into(), "en".into())
    );
    assert_eq!(
        languages(&settings, Language::EnUS),
        ("en".into(), "zh-Hans".into())
    );
    settings.first_language = "ja".into();
    settings.second_language = "ja".into();
    assert_eq!(
        languages(&settings, Language::ZhCN),
        ("ja".into(), "en".into())
    );
    settings.second_language = "zh-Hant".into();
    assert_eq!(
        languages(&settings, Language::ZhCN),
        ("ja".into(), "zh-Hant".into())
    );
}
