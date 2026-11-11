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
        lines: if text.is_empty() {
            vec![]
        } else {
            text.split('\n')
                .map(|text| ImageTextLine {
                    text: text.into(),
                    quad: [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]],
                })
                .collect()
        },
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
fn stored_lines_match_text_including_truncation_and_empty_result() {
    let fixture = Fixture::new();
    let core = fixture.start();
    let item = seed(&fixture, &core, "layout", 1_800_000_001);
    enable(&core);
    let text = format!("{}\n尾行\n丢弃", "字".repeat(protocol::MAX_TEXT_CHARS - 2));
    save_outcome(&fixture, &core, &item, &done(&text));
    let lines = block_on(core.image_text_lines(&item.id)).unwrap().unwrap();
    let stored = block_on(core.image_text(&item.id)).unwrap().unwrap();
    assert_eq!(stored.chars().count(), protocol::MAX_TEXT_CHARS);
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[1].text, "尾");
    assert_eq!(
        stored,
        lines
            .iter()
            .map(|line| line.text.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    );
    save_outcome(&fixture, &core, &item, &done(""));
    assert_eq!(
        block_on(core.image_text_lines(&item.id)).unwrap(),
        Some(vec![])
    );
    assert_eq!(
        block_on(core.image_text(&item.id)).unwrap().as_deref(),
        Some("")
    );
    block_on(core.shutdown()).unwrap();
}

/// 暂停真实会话，只观察 API 对单个优先槽的影响。
fn enable_paused(core: &Core) {
    enable(core);
    let mut state = core.0.ocr.state();
    state.suspended = 1;
    state.stopped = false;
}

#[test]
fn image_lines_disabled_missing_and_non_image_do_not_schedule() {
    let fixture = Fixture::new();
    let core = fixture.start();
    let item = seed(&fixture, &core, "disabled-layout", 1_800_000_001);
    save_outcome(&fixture, &core, &item, &done("text"));
    assert!(block_on(core.image_text_lines(&item.id)).unwrap().is_none());
    assert!(core.0.ocr.state().priority.is_none());
    enable_paused(&core);
    assert!(block_on(core.image_text_lines("missing"))
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
    assert!(block_on(core.image_text_lines(&text.id)).unwrap().is_none());
    assert!(core.0.ocr.state().priority.is_none());
    block_on(core.shutdown()).unwrap();
}

#[test]
fn pending_layout_replaces_priority_and_returns_saved_lines() {
    let fixture = Fixture::new();
    let core = fixture.start();
    let first = seed(&fixture, &core, "first-layout", 1_800_000_001);
    let second = seed(&fixture, &core, "second-layout", 1_800_000_002);
    enable_paused(&core);
    assert!(block_on(core.image_text_lines(&first.id))
        .unwrap()
        .is_none());
    assert_eq!(
        core.0.ocr.state().priority.as_deref(),
        Some(first.id.as_str())
    );
    assert!(block_on(core.image_text_lines(&second.id))
        .unwrap()
        .is_none());
    assert_eq!(
        core.0.ocr.state().priority.as_deref(),
        Some(second.id.as_str())
    );
    save_outcome(&fixture, &core, &second, &done("new layout"));
    finish_priority(&core.0, &first.id);
    assert_eq!(
        core.0.ocr.state().priority.as_deref(),
        Some(second.id.as_str())
    );
    finish_priority(&core.0, &second.id);
    assert_eq!(
        block_on(core.image_text_lines(&second.id))
            .unwrap()
            .unwrap()[0]
            .text,
        "new layout"
    );
    assert!(core.0.ocr.state().priority.is_none());
    block_on(core.shutdown()).unwrap();
}

/// 构造协议 1 的已完成行，保留原文但没有布局。
fn legacy_layout(fixture: &Fixture, core: &Core, item: &ClipboardItem) {
    save_outcome(fixture, core, item, &done("legacy searchable text"));
    fixture.runtime.handle().block_on(async {
        sqlx::query("UPDATE image_texts SET lines = NULL WHERE item_id = ?")
            .bind(&item.id)
            .execute(&core.0.db.pool().await)
            .await
            .unwrap();
    });
}

#[test]
fn legacy_layout_is_priority_only_and_failed_rerun_keeps_text() {
    let fixture = Fixture::new();
    let core = fixture.start();
    let item = seed(&fixture, &core, "legacy-layout", 1_800_000_001);
    legacy_layout(&fixture, &core, &item);
    enable_paused(&core);
    fixture.runtime.handle().block_on(async {
        let mut connection = connect(&core.0).await.unwrap();
        assert!(sqlx::query_as::<_, (String, String, String)>(db::NEXT_JOB)
            .fetch_optional(&mut connection)
            .await
            .unwrap()
            .is_none());
        connection.close().await.unwrap();
    });
    assert!(block_on(core.image_text_lines(&item.id)).unwrap().is_none());
    assert_eq!(
        core.0.ocr.state().priority.as_deref(),
        Some(item.id.as_str())
    );
    save_outcome(
        &fixture,
        &core,
        &item,
        &Outcome::Failed {
            reason: "rerun failed".into(),
        },
    );
    finish_priority(&core.0, &item.id);
    assert_eq!(
        block_on(core.image_text(&item.id)).unwrap().as_deref(),
        Some("legacy searchable text")
    );
    assert_eq!(
        block_on(core.list_items(ClipboardItemQuery {
            keyword: Some("searchable".into()),
            ..Default::default()
        }))
        .unwrap()
        .total,
        1
    );
    assert!(block_on(core.image_text_lines(&item.id)).unwrap().is_none());
    assert_eq!(
        core.0.ocr.state().priority.as_deref(),
        Some(item.id.as_str())
    );
    save_outcome(&fixture, &core, &item, &done("layout recovered"));
    finish_priority(&core.0, &item.id);
    assert!(block_on(core.image_text_lines(&item.id)).unwrap().is_some());
    block_on(core.shutdown()).unwrap();
}

#[test]
fn exhausted_legacy_layout_keeps_text_without_requeuing() {
    let fixture = Fixture::new();
    let core = fixture.start();
    let item = seed(&fixture, &core, "exhausted-legacy-layout", 1_800_000_001);
    legacy_layout(&fixture, &core, &item);
    enable_paused(&core);
    for _ in 0..3 {
        save_outcome(
            &fixture,
            &core,
            &item,
            &Outcome::Failed {
                reason: "missing image".into(),
            },
        );
    }
    for _ in 0..2 {
        finish_priority(&core.0, &item.id);
        assert!(block_on(core.image_text_lines(&item.id)).unwrap().is_none());
        assert!(core.0.ocr.state().priority.is_none());
    }
    assert_eq!(
        block_on(core.image_text(&item.id)).unwrap().as_deref(),
        Some("legacy searchable text")
    );
    block_on(core.shutdown()).unwrap();
}

#[test]
fn terminal_layout_rows_do_not_schedule() {
    let fixture = Fixture::new();
    let core = fixture.start();
    let item = seed(&fixture, &core, "terminal-layout", 1_800_000_001);
    enable_paused(&core);
    let failed = Outcome::Failed {
        reason: "failure".into(),
    };
    save_outcome(&fixture, &core, &item, &failed);
    assert!(block_on(core.image_text_lines(&item.id)).unwrap().is_none());
    assert!(core.0.ocr.state().priority.take().is_some());
    save_outcome(&fixture, &core, &item, &failed);
    save_outcome(&fixture, &core, &item, &failed);
    assert!(block_on(core.image_text_lines(&item.id)).unwrap().is_none());
    assert!(core.0.ocr.state().priority.is_none());
    save_outcome(
        &fixture,
        &core,
        &item,
        &Outcome::Skipped {
            reason: "large".into(),
        },
    );
    assert!(block_on(core.image_text_lines(&item.id)).unwrap().is_none());
    assert!(core.0.ocr.state().priority.is_none());
    block_on(core.shutdown()).unwrap();
}

#[test]
fn image_text_range_copies_unicode_scalars_and_rejects_invalid_ranges() {
    use crate::clipboard::ClipboardFragment::ImageTextRange;
    let fixture = Fixture::new();
    let core = fixture.start();
    let item = seed(&fixture, &core, "range", 1_800_000_001);
    save_outcome(&fixture, &core, &item, &done("中😀\ntext"));
    enable(&core);
    block_on(core.copy_fragment(&item.id, ImageTextRange { start: 1, end: 4 })).unwrap();
    assert_eq!(fixture.clipboard.snapshot().text.as_deref(), Some("😀\nt"));
    block_on(core.prepare_paste_fragment(&item.id, ImageTextRange { start: 0, end: 7 })).unwrap();
    assert_eq!(
        fixture.clipboard.snapshot().text.as_deref(),
        Some("中😀\ntext")
    );
    for (start, end) in [(1, 1), (3, 1), (0, 8), (usize::MAX, usize::MAX)] {
        assert!(block_on(core.copy_fragment(&item.id, ImageTextRange { start, end })).is_err());
        assert!(
            block_on(core.prepare_paste_fragment(&item.id, ImageTextRange { start, end })).is_err()
        );
    }
    block_on(core.set_extension_enabled("ocr", false)).unwrap();
    assert!(block_on(core.copy_fragment(&item.id, ImageTextRange { start: 0, end: 1 })).is_err());
    block_on(core.shutdown()).unwrap();
}

/// 真实 helper 使用当前测试 profile 的产物；安装期间暂停调度，避免提前消费队列。
fn install_real_paused(fixture: &Fixture, core: &Core) {
    core.0.ocr.state().suspended = 1;
    let test = std::env::current_exe().unwrap();
    let profile = test.parent().unwrap().parent().unwrap();
    let source = profile.join(if cfg!(target_os = "windows") {
        "kwikpaste-ext-ocr.exe"
    } else {
        "kwikpaste-ext-ocr"
    });
    let staged = fixture.root().join("real-ocr-staged");
    std::fs::copy(source, &staged).expect("build kwikpaste-ext-ocr before core tests");
    block_on(core.install_extension("ocr", "1.0.0", protocol::OCR_PROTOCOL, &staged)).unwrap();
}

/// 等到会话真正释放连接和 helper，避免测试清理与子进程争用临时文件。
fn wait_idle(core: &Core) {
    let deadline = Instant::now() + Duration::from_secs(90);
    while core.0.ocr.state().running {
        assert!(Instant::now() < deadline, "OCR session did not become idle");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// 装着却起不来的 helper（被杀软拦截、系统不让运行）不能把图片记成识别失败。
#[test]
fn helper_that_cannot_start_leaves_images_pending() {
    let fixture = Fixture::new();
    let core = fixture.start();
    seed(&fixture, &core, "image", 1_800_000_001);
    let staged = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(staged.path(), b"not an executable").unwrap();
    block_on(core.install_extension("ocr", "1.0.0", protocol::OCR_PROTOCOL, staged.path()))
        .unwrap();
    wait_idle(&core);
    let status = block_on(core.ocr_status()).unwrap();
    assert_eq!((status.failed, status.pending), (0, 1));
    block_on(core.shutdown()).unwrap();
}

#[test]
fn priority_starts_idle_session_before_background_queue() {
    let fixture = Fixture::new();
    let core = fixture.start();
    install_real_paused(&fixture, &core);
    let priority = seed(&fixture, &core, "priority-old", 1_800_000_001);
    let normal = seed(&fixture, &core, "normal-new", 1_800_000_002);
    fixture.events.lock().unwrap().clear();
    core.0.ocr.state().suspended = 0;
    assert!(block_on(core.image_text_lines(&priority.id))
        .unwrap()
        .is_none());
    wait_idle(&core);
    assert_eq!(
        block_on(core.image_text_lines(&priority.id)).unwrap(),
        Some(vec![])
    );
    assert_eq!(
        block_on(core.image_text_lines(&normal.id)).unwrap(),
        Some(vec![])
    );
    fixture.runtime.handle().block_on(async {
        let rows: Vec<(String, String)> = sqlx::query_as(
            "SELECT item_id, updated_at FROM image_texts ORDER BY updated_at, rowid",
        )
        .fetch_all(&core.0.db.pool().await)
        .await
        .unwrap();
        assert_eq!(rows[0].0, priority.id);
    });
    assert!(
        fixture
            .events
            .lock()
            .unwrap()
            .iter()
            .filter(|event| matches!(event, CoreEvent::OcrChanged))
            .count()
            >= 3
    );
    assert!(core.0.ocr.state().priority.is_none());
    assert!(core.0.ocr.state().child.is_none());
    block_on(core.shutdown()).unwrap();
}

#[test]
fn real_priority_reruns_legacy_layout_and_retains_failed_text() {
    let fixture = Fixture::new();
    let core = fixture.start();
    install_real_paused(&fixture, &core);
    let item = seed(&fixture, &core, "real-legacy", 1_800_000_001);
    legacy_layout(&fixture, &core, &item);
    core.0.ocr.state().suspended = 0;
    assert!(block_on(core.image_text_lines(&item.id)).unwrap().is_none());
    wait_idle(&core);
    assert_eq!(
        block_on(core.image_text_lines(&item.id)).unwrap(),
        Some(vec![])
    );
    legacy_layout(&fixture, &core, &item);
    std::fs::remove_file(core.0.images.origin_path(&item.content)).unwrap();
    assert!(block_on(core.image_text_lines(&item.id)).unwrap().is_none());
    wait_idle(&core);
    assert_eq!(
        block_on(core.image_text(&item.id)).unwrap().as_deref(),
        Some("legacy searchable text")
    );
    assert!(core.0.ocr.state().priority.is_none());
    block_on(core.shutdown()).unwrap();
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
fn image_words_copy_is_recorded_and_paste_follows_reuse_rules() {
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
    // 复制出来的文字不登记回环：监听会把它当普通复制记成文字记录。
    assert!(!core
        .0
        .guard
        .should_skip(&items::content_hash(ClipboardKind::Text, "hello  world")));
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
    assert_eq!(reused.use_count, image.use_count + 1);
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
fn copy_image_text_is_recorded_as_a_new_copy() {
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
    assert!(!core.0.guard.should_skip(&items::content_hash(
        ClipboardKind::Text,
        "中文\nEnglish text"
    )));
    let after = fixture.runtime.handle().block_on(async {
        items::find_item_by_id(&core.0.db.pool().await, &image.id)
            .await
            .unwrap()
            .unwrap()
    });
    assert_eq!(after.use_count, image.use_count);
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
