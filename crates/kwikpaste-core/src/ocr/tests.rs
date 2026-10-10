//! OCR 的队列、搜索、复用与 generation 端到端测试，不使用系统剪贴板。
use super::*;
use crate::{
    clipboard::ClipboardPayload,
    db::{
        items,
        models::{ClipboardItem, ClipboardItemQuery, ClipboardKind},
    },
    testing::{block_on, sample_png, Fixture},
};

fn image(core: &Core, id: &str, time: i64) -> crate::db::models::ClipboardItem {
    let payload = ClipboardPayload::Image(crate::clipboard::ImagePayload {
        bytes: sample_png(24, 24),
        width: 24,
        height: 24,
    });
    let mut item = core.build_item(&payload).unwrap().unwrap();
    item.id = id.into();
    item.content_hash = format!("test-{id}");
    item.created_at = chrono::DateTime::from_timestamp(time, 0).unwrap();
    item.updated_at = item.created_at;
    item
}

fn seed(fixture: &Fixture, core: &Core, id: &str, time: i64) -> crate::db::models::ClipboardItem {
    let item = image(core, id, time);
    block_on(core.store_item(item.clone(), None)).unwrap();
    // fixture 保持 runtime 和临时数据目录存活。
    let _ = fixture;
    item
}

fn enable(core: &Core) {
    // 本文件用固定结果填充数据库；调度器的真实生命周期由 extensions::tests 覆盖。
    core.0.ocr.state().stopped = true;
    let staged = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(
        staged.path(),
        b"fixture executable; engine is not run in seeded DB tests",
    )
    .unwrap();
    block_on(core.install_extension("ocr", "1.0.0", protocol::OCR_PROTOCOL, staged.path()))
        .unwrap();
}

fn done(text: &str) -> Outcome {
    Outcome::Done {
        text: text.into(),
        language: "zh-Hans-CN".into(),
    }
}

/// 写入指定识别结果，不启动 OCR helper，也不接触系统剪贴板。
fn save_outcome(fixture: &Fixture, core: &Core, item: &ClipboardItem, outcome: &Outcome) {
    fixture.runtime.handle().block_on(async {
        let mut connection = connect(&core.0).await.unwrap();
        db::save(&mut connection, &item.id, &item.content_hash, outcome)
            .await
            .unwrap();
        connection.close().await.unwrap();
    });
}

#[test]
fn image_text_preview_requires_enabled_ocr_and_completed_nonempty_image_text() {
    let fixture = Fixture::new();
    let core = fixture.start();
    let image = seed(&fixture, &core, "preview", 1_800_000_001);
    save_outcome(&fixture, &core, &image, &done("recognized"));

    assert!(block_on(core.image_text_preview(&image.id))
        .unwrap()
        .is_none());
    enable(&core);
    assert!(block_on(core.image_text_preview(&image.id))
        .unwrap()
        .is_some());
    assert!(block_on(core.image_text_preview("missing"))
        .unwrap()
        .is_none());

    let text = core
        .build_item(&ClipboardPayload::Text(crate::clipboard::TextPayload {
            text: "not an image".into(),
            html: None,
            rtf: None,
        }))
        .unwrap()
        .unwrap();
    block_on(core.store_item(text.clone(), None)).unwrap();
    assert!(block_on(core.image_text_preview(&text.id))
        .unwrap()
        .is_none());

    let pending = seed(&fixture, &core, "pending", 1_800_000_002);
    assert!(block_on(core.image_text_preview(&pending.id))
        .unwrap()
        .is_none());
    for outcome in [
        done(""),
        Outcome::Failed {
            reason: "test".into(),
        },
        Outcome::Skipped {
            reason: "test".into(),
        },
    ] {
        save_outcome(&fixture, &core, &image, &outcome);
        assert!(block_on(core.image_text_preview(&image.id))
            .unwrap()
            .is_none());
    }
    block_on(core.shutdown()).unwrap();
}

#[test]
fn image_text_preview_returns_plain_text_payload_and_soft_wrapped_rows() {
    use crate::clipboard::word_spans;
    use crate::presenter::PreviewContentMetrics;

    let fixture = Fixture::new();
    let core = fixture.start();
    let image = seed(&fixture, &core, "plain-preview", 1_800_000_001);
    let text = format!(
        "{}🙏\n订单 20260924 已发货，联系 138 1234 5678\n",
        "a".repeat(31)
    );
    save_outcome(&fixture, &core, &image, &done(&text));
    enable(&core);
    block_on(
        core.update_settings(serde_json::json!({"clipboard":{"preview":{"textView":"plain"}}})),
    )
    .unwrap();

    let (payload, metrics) = block_on(core.image_text_preview(&image.id))
        .unwrap()
        .unwrap();
    assert_eq!(payload.id, image.id);
    assert_eq!(payload.kind, ClipboardKind::Text);
    assert_eq!(payload.sub_kind, None);
    assert_eq!(payload.updated_at, image.updated_at);
    assert_eq!(payload.text.as_deref(), Some(text.as_str()));
    assert_eq!((payload.words, payload.words_truncated), word_spans(&text));
    assert!(payload.image_path.is_none());
    assert!(payload.image_width.is_none());
    assert!(payload.image_height.is_none());
    assert!(payload.size.is_none());
    assert!(!payload.is_sensitive);
    assert!(!payload.image_exists);
    assert!(payload.files.is_empty());
    assert_eq!(payload.total_files, 0);
    assert_eq!(metrics, PreviewContentMetrics::Text { rows: 4 });
    assert_eq!(
        block_on(core.find_item(&image.id))
            .unwrap()
            .unwrap()
            .updated_at,
        image.updated_at
    );
    block_on(core.shutdown()).unwrap();
}

#[test]
fn image_text_preview_words_share_text_record_metrics_and_limits() {
    use crate::clipboard::{word_spans, MAX_SPLIT_CHARS};
    use crate::presenter::{preview_content_metrics, PreviewContentMetrics, PreviewWordChip};
    use crate::settings::PreviewTextView;

    let fixture = Fixture::new();
    let core = fixture.start();
    let image = seed(&fixture, &core, "words-preview", 1_800_000_001);
    enable(&core);
    block_on(
        core.update_settings(serde_json::json!({"clipboard":{"preview":{"textView":"words"}}})),
    )
    .unwrap();

    for text in [
        "订单 20260924\nhello world".to_owned(),
        "字".repeat(MAX_SPLIT_CHARS + 10),
        " \n ".to_owned(),
    ] {
        save_outcome(&fixture, &core, &image, &done(&text));
        let (payload, metrics) = block_on(core.image_text_preview(&image.id))
            .unwrap()
            .unwrap();
        assert_eq!(payload.text.as_deref(), Some(text.as_str()));
        assert_eq!((payload.words, payload.words_truncated), word_spans(&text));
        let split = crate::clipboard::split_words(&text);
        let expected = if split.tokens.is_empty() {
            PreviewContentMetrics::Text { rows: 2 }
        } else {
            PreviewContentMetrics::Words {
                chips: split
                    .tokens
                    .iter()
                    .map(|token| PreviewWordChip::new(&token.text, token.line_break))
                    .collect(),
            }
        };
        assert_eq!(metrics, expected);

        let mut text_item = image.clone();
        text_item.kind = ClipboardKind::Text;
        text_item.content = text;
        let mut clipboard = core.settings().clipboard;
        for view in [PreviewTextView::Plain, PreviewTextView::Words] {
            clipboard.preview.text_view = view;
            block_on(
                core.update_settings(
                    serde_json::json!({"clipboard":{"preview":{"textView":view}}}),
                ),
            )
            .unwrap();
            let (_, image_metrics) = block_on(core.image_text_preview(&image.id))
                .unwrap()
                .unwrap();
            assert_eq!(
                image_metrics,
                preview_content_metrics(&text_item, &clipboard)
            );
        }
    }
    block_on(core.shutdown()).unwrap();
}

#[test]
fn image_words_copy_and_paste_use_memory_clipboard_and_normal_reuse_rules() {
    use crate::clipboard::ClipboardFragment;

    let fixture = Fixture::new();
    let core = fixture.start();
    let image = seed(&fixture, &core, "image-words", 1_800_000_001);
    save_outcome(
        &fixture,
        &core,
        &image,
        &done("订单 20260924 已发货\nhello  world again"),
    );
    enable(&core);
    block_on(core.update_settings(serde_json::json!({"clipboard":{"content":{"copyThenHideWindow":true,"updateOnReuse":true}}}))).unwrap();

    assert!(
        block_on(core.copy_fragment(
            &image.id,
            ClipboardFragment::ImageWords {
                indices: vec![7, 6, 7]
            }
        ))
        .unwrap()
        .hide_window
    );
    assert_eq!(
        fixture.clipboard.snapshot().text.as_deref(),
        Some("hello  world")
    );
    block_on(core.prepare_paste_fragment(
        &image.id,
        ClipboardFragment::ImageWords {
            indices: vec![2, 8],
        },
    ))
    .unwrap();
    assert_eq!(
        fixture.clipboard.snapshot().text.as_deref(),
        Some("20260924 again")
    );
    let reused = block_on(core.find_item(&image.id)).unwrap().unwrap();
    assert_eq!(reused.use_count, image.use_count + 2);
    assert_eq!(reused.kind, ClipboardKind::Image);
    assert_eq!(reused.content, image.content);
    assert_eq!(
        block_on(core.list_items(ClipboardItemQuery::default()))
            .unwrap()
            .total,
        1
    );

    block_on(
        core.update_settings(serde_json::json!({"clipboard":{"content":{"updateOnReuse":false}}})),
    )
    .unwrap();
    block_on(core.copy_fragment(
        &image.id,
        ClipboardFragment::ImageWords {
            indices: vec![0, 1, 2],
        },
    ))
    .unwrap();
    assert_eq!(
        fixture.clipboard.snapshot().text.as_deref(),
        Some("订单 20260924")
    );
    assert_eq!(
        block_on(core.find_item(&image.id))
            .unwrap()
            .unwrap()
            .use_count,
        reused.use_count
    );
    block_on(core.shutdown()).unwrap();
}

#[test]
fn image_words_reject_invalid_indices_non_images_and_unavailable_text() {
    use crate::clipboard::{ClipboardFragment, TextPayload};

    let fixture = Fixture::new();
    let core = fixture.start();
    let image = seed(&fixture, &core, "invalid-image-words", 1_800_000_001);
    save_outcome(&fixture, &core, &image, &done("hello world"));
    enable(&core);
    let text = core
        .build_item(&ClipboardPayload::Text(TextPayload {
            text: "hello world".into(),
            html: None,
            rtf: None,
        }))
        .unwrap()
        .unwrap();
    block_on(core.store_item(text.clone(), None)).unwrap();
    let expected =
        block_on(core.copy_fragment(&text.id, ClipboardFragment::Words { indices: vec![] }))
            .unwrap_err();
    assert!(matches!(expected, crate::AppError::Clipboard(_)));
    let expected_message = expected.to_string();
    let check = |id: &str, indices: Vec<usize>| {
        let error = block_on(core.copy_fragment(
            id,
            ClipboardFragment::ImageWords {
                indices: indices.clone(),
            },
        ))
        .unwrap_err();
        assert!(matches!(error, crate::AppError::Clipboard(_)));
        assert_eq!(error.to_string(), expected_message);
        let error =
            block_on(core.prepare_paste_fragment(id, ClipboardFragment::ImageWords { indices }))
                .unwrap_err();
        assert!(matches!(error, crate::AppError::Clipboard(_)));
        assert_eq!(error.to_string(), expected_message);
        assert!(fixture.clipboard.snapshot().text.is_none());
    };
    check(&image.id, vec![]);
    check(&image.id, vec![0, 2]);
    check(&image.id, vec![usize::MAX]);
    check(&text.id, vec![0]);
    let pending = seed(&fixture, &core, "no-image-text", 1_800_000_002);
    check(&pending.id, vec![0]);
    for outcome in [
        done(""),
        Outcome::Failed {
            reason: "test".into(),
        },
        Outcome::Skipped {
            reason: "test".into(),
        },
    ] {
        save_outcome(&fixture, &core, &image, &outcome);
        check(&image.id, vec![0]);
    }
    save_outcome(&fixture, &core, &image, &done("hello world"));
    block_on(core.set_extension_enabled("ocr", false)).unwrap();
    check(&image.id, vec![0]);
    assert_eq!(
        block_on(core.find_item(&image.id))
            .unwrap()
            .unwrap()
            .use_count,
        image.use_count
    );
    block_on(core.shutdown()).unwrap();
}

#[test]
fn queue_is_newest_first_and_retries_twice_across_connections() {
    let fixture = Fixture::new();
    let core = fixture.start();
    let old = seed(&fixture, &core, "old", 1_800_000_001);
    let newest = seed(&fixture, &core, "new", 1_800_000_002);
    fixture.runtime.handle().block_on(async {
        let mut connection = connect(&core.0).await.unwrap();
        let next: (String, String, String) = sqlx::query_as(db::NEXT_JOB)
            .fetch_one(&mut connection)
            .await
            .unwrap();
        assert_eq!(next.0, newest.id);
        db::save(
            &mut connection,
            &newest.id,
            &newest.content_hash,
            &Outcome::Failed {
                reason: "failure".into(),
            },
        )
        .await
        .unwrap();
        connection.close().await.unwrap();
        let mut connection = connect(&core.0).await.unwrap();
        for _ in 0..2 {
            let next: (String, String, String) = sqlx::query_as(db::NEXT_JOB)
                .fetch_one(&mut connection)
                .await
                .unwrap();
            assert_eq!(next.0, newest.id);
            db::save(
                &mut connection,
                &newest.id,
                &newest.content_hash,
                &Outcome::Failed {
                    reason: "failure".into(),
                },
            )
            .await
            .unwrap();
        }
        let next: (String, String, String) = sqlx::query_as(db::NEXT_JOB)
            .fetch_one(&mut connection)
            .await
            .unwrap();
        assert_eq!(next.0, old.id);
        db::save(
            &mut connection,
            &old.id,
            &old.content_hash,
            &Outcome::Skipped {
                reason: "large".into(),
            },
        )
        .await
        .unwrap();
        let next: Option<(String, String, String)> = sqlx::query_as(db::NEXT_JOB)
            .fetch_optional(&mut connection)
            .await
            .unwrap();
        assert!(next.is_none());
        connection.close().await.unwrap();
    });
    let status = block_on(core.ocr_status()).unwrap();
    assert_eq!(
        (status.total_images, status.failed, status.pending),
        (2, 2, 0)
    );
    assert!(!status.running);
    block_on(core.shutdown()).unwrap();
}

#[test]
fn ocr_search_matches_rows_totals_select_all_and_flags_with_same_predicate() {
    let fixture = Fixture::new();
    let core = fixture.start();
    let image = seed(&fixture, &core, "image", 1_800_000_001);
    fixture.runtime.handle().block_on(async {
        let mut connection = connect(&core.0).await.unwrap();
        db::save(
            &mut connection,
            &image.id,
            &image.content_hash,
            &done("中文识别 English words 50%_test"),
        )
        .await
        .unwrap();
        connection.close().await.unwrap();
    });
    let query = |keyword: &str| ClipboardItemQuery {
        keyword: Some(keyword.into()),
        ..Default::default()
    };
    assert_eq!(
        block_on(core.list_items(query("English"))).unwrap().total,
        0
    );
    enable(&core);
    for keyword in [
        "中文",
        "中文识别",
        "English",
        "words",
        "50%_",
        "English words",
        "sh wo",
    ] {
        let page = block_on(core.list_items(query(keyword))).unwrap();
        let refs = block_on(core.list_item_refs(query(keyword))).unwrap();
        assert_eq!(page.total, 1, "{keyword}");
        assert_eq!(refs.len(), 1, "{keyword}");
        assert_eq!(page.list[0].item.id, refs[0].id);
        assert!(page.list[0].has_image_text);
        assert!(page.list[0].image_text_matched);
        let actions = &page.list[0].available_actions;
        let save = actions
            .iter()
            .position(|action| *action == crate::presenter::ClipboardAction::SaveImage)
            .unwrap();
        assert_eq!(
            actions[save + 1],
            crate::presenter::ClipboardAction::CopyImageText
        );
    }
    let all = block_on(core.list_items(ClipboardItemQuery::default())).unwrap();
    assert!(!all.list[0].image_text_matched);
    assert!(
        block_on(core.list_item(&image.id))
            .unwrap()
            .unwrap()
            .has_image_text
    );
    block_on(core.set_extension_enabled("ocr", false)).unwrap();
    assert_eq!(
        block_on(core.list_items(query("English"))).unwrap().total,
        0
    );
    assert!(block_on(core.image_text(&image.id)).unwrap().is_some());
    block_on(core.shutdown()).unwrap();
}

#[test]
fn copy_image_text_uses_memory_clipboard_and_existing_reuse_rules() {
    let fixture = Fixture::new();
    let core = fixture.start();
    let image = seed(&fixture, &core, "copy", 1_800_000_001);
    enable(&core);
    fixture.runtime.handle().block_on(async {
        let mut connection = connect(&core.0).await.unwrap();
        db::save(
            &mut connection,
            &image.id,
            &image.content_hash,
            &done("中文\nEnglish text"),
        )
        .await
        .unwrap();
        connection.close().await.unwrap();
    });
    let unchanged = fixture.runtime.handle().block_on(async {
        items::find_item_by_id(&core.0.db.pool().await, &image.id)
            .await
            .unwrap()
            .unwrap()
    });
    assert_eq!(unchanged.updated_at, image.updated_at);
    block_on(core.update_settings(serde_json::json!({"clipboard":{"content":{"copyThenHideWindow":true,"updateOnReuse":true}}}))).unwrap();
    assert!(
        block_on(core.copy_image_text(&image.id))
            .unwrap()
            .hide_window
    );
    assert_eq!(
        fixture.clipboard.snapshot().text.as_deref(),
        Some("中文\nEnglish text")
    );
    let reused = fixture.runtime.handle().block_on(async {
        items::find_item_by_id(&core.0.db.pool().await, &image.id)
            .await
            .unwrap()
            .unwrap()
    });
    assert_eq!(reused.use_count, image.use_count + 1);
    block_on(core.shutdown()).unwrap();
}

#[test]
fn late_results_are_rejected_after_delete_clear_import_and_storage_switch() {
    use crate::backup::{
        BackupExportMode, BackupImportStrategy, BackupScope, ExportHistoryBackupOptions,
        ImportHistoryBackupInput,
    };
    let fixture = Fixture::new();
    let core = fixture.start();
    let item = seed(&fixture, &core, "late", 1_800_000_001);
    enable(&core);
    let try_late = |generation| {
        assert_ne!(core.0.ocr.state().generation, generation);
        fixture.runtime.handle().block_on(async {
            let mut connection = connect(&core.0).await.unwrap();
            assert!(!write_if_current(
                &core.0,
                generation,
                &mut connection,
                &item.id,
                &item.content_hash,
                &done("obsolete")
            )
            .await
            .unwrap());
            connection.close().await.unwrap();
            assert!(core.image_text(&item.id).await.unwrap().is_none());
        })
    };
    let generation = core.0.ocr.state().generation;
    block_on(core.delete_item(&item.id)).unwrap();
    block_on(core.store_item(item.clone(), None)).unwrap();
    try_late(generation);
    let generation = core.0.ocr.state().generation;
    block_on(core.clear_ocr_data()).unwrap();
    try_late(generation);
    let backup = block_on(core.export_history_backup(
        fixture.root().join("backup.kwikpastebak"),
        ExportHistoryBackupOptions {
            mode: BackupExportMode::Plain,
            password: None,
            scope: BackupScope::default(),
        },
    ))
    .unwrap();
    let generation = core.0.ocr.state().generation;
    block_on(core.import_history_backup(
        ImportHistoryBackupInput {
            path: backup.path.into(),
            password: None,
            import_settings: false,
        },
        BackupImportStrategy::Overwrite,
    ))
    .unwrap();
    try_late(generation);
    let generation = core.0.ocr.state().generation;
    block_on(core.change_storage_location(fixture.root().join("switched"))).unwrap();
    try_late(generation);
    block_on(core.shutdown()).unwrap();
}

#[test]
fn old_test_build_ocr_setting_is_ignored_without_installing_an_extension() {
    let fixture = Fixture::new();
    fixture.write_settings(include_str!(
        "../../tests/fixtures/settings/legacy-ocr.json"
    ));
    let core = fixture.start();
    assert!(!core.ocr_enabled());
    assert_eq!(core.settings_load_report(), Default::default());
    assert_eq!(
        block_on(core.ocr_support()).unwrap(),
        OcrSupport::NotInstalled
    );
    let value = serde_json::to_value(core.settings()).unwrap();
    assert!(value["clipboard"].get("ocr").is_none());
    assert!(core.settings().clipboard.capture.image);
    block_on(core.shutdown()).unwrap();
}

/// 片段围绕第一次命中，换行压成空格，命中范围正好框住关键词。
#[test]
fn snippet_centers_on_the_first_match() {
    let text =
        "增值税电子普通发票\n开票日期 2026-10-02\n购买方 名称：快贴科技有限公司 纳税人识别号 9131";
    let found = snippet(text, "开票").unwrap();
    assert!(!found.text.starts_with('…'));
    assert_eq!(&found.text[found.matched.clone()], "开票");
    assert!(!found.text.contains('\n'));

    let long = format!("{}Invoice NUMBER 0402{}", "前".repeat(40), "后".repeat(80));
    let found = snippet(&long, "number").unwrap();
    assert!(found.text.starts_with('…') && found.text.ends_with('…'));
    assert_eq!(&found.text[found.matched.clone()], "NUMBER");

    let fallback = snippet("只有开头", "不存在").unwrap();
    assert_eq!(
        (fallback.text.as_str(), fallback.matched),
        ("只有开头", 0..0)
    );
    assert!(snippet(" \n ", "x").is_none());
}
