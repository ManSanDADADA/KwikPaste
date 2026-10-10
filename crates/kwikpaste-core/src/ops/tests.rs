//! 记录、分组与应用操作的端到端测试：真实的 core（临时目录 + 内存数据库文件），
//! 内存剪贴板与假的平台层，绝不碰本机剪贴板。

use std::time::Duration;

use serde_json::json;

use super::{ClipboardGroupInput, ClipboardGroupLayoutInput};
use crate::clipboard::watcher::RepeatFilter;
use crate::clipboard::{
    ClipboardBackend, ClipboardFragment, ClipboardReader, MemoryClipboard, MemoryState,
};
use crate::db::models::{ClipboardItemQuery, ClipboardKind};
use crate::db::overview::{ClearScope, ContentCategory};
use crate::events::CoreEvent;
use crate::root::Core;
use crate::testing::{block_on, sample_png, Fixture};

const NO_RETRY: [Duration; 0] = [];

/// 让「别的应用」往内存剪贴板里复制一次，再按监听路径入库，返回生效行 id。
/// 每次都是一次独立的复制（不合并重复通知），合并见 [`notify_in`]。
fn copy_in(core: &Core, state: MemoryState) -> Option<String> {
    notify_in(core, state, &mut RepeatFilter::new(Duration::ZERO))
}

/// 一次剪贴板通知按监听路径入库；`repeats` 跨调用保留时，同一次复制的重复通知会被合并。
fn notify_in(core: &Core, state: MemoryState, repeats: &mut RepeatFilter) -> Option<String> {
    let reader = ClipboardReader::with_backend(MemoryClipboard::with_state(state));
    let (item, source) =
        crate::clipboard::watcher::capture_change(&core.0, &reader, &NO_RETRY, repeats)?;
    let result = block_on(core.hop({
        let core = core.clone();
        async move {
            crate::clipboard::persist::persist_and_notify(&core.0, &item, source.as_ref()).await
        }
    }))
    .unwrap();
    Some(result.id)
}

fn text(value: &str) -> MemoryState {
    MemoryState {
        text: Some(value.to_owned()),
        ..MemoryState::default()
    }
}

fn use_count(core: &Core, id: &str) -> i64 {
    block_on(core.find_item(id)).unwrap().unwrap().use_count
}

#[test]
fn capture_records_source_app_plays_sound_and_dedups() {
    let fixture = Fixture::new();
    let core = fixture.start();
    block_on(core.update_settings(json!({"clipboard": {"feedback": {"copySound": true}}})))
        .unwrap();
    fixture
        .platform
        .set_frontmost("C:/Apps/Editor.exe", "Editor");

    let first = copy_in(&core, text("e2e kwikpaste watcher")).unwrap();
    let again = copy_in(&core, text("e2e kwikpaste watcher")).unwrap();

    assert_eq!(first, again);
    assert_eq!(use_count(&core, &first), 2);
    let stored = block_on(core.find_item(&first)).unwrap().unwrap();
    assert_eq!(stored.content, "e2e kwikpaste watcher");
    assert_eq!(stored.source_app_id.as_deref(), Some("C:/Apps/Editor.exe"));
    assert_eq!(
        block_on(core.list_apps(vec!["C:/Apps/Editor.exe".to_owned()]))
            .unwrap()
            .len(),
        1
    );
    assert_eq!(fixture.platform.sounds(), 2);
    assert_eq!(*fixture.platform.sound_volumes.lock().unwrap(), [100, 100]);

    let upserts: Vec<bool> = fixture
        .take_events()
        .into_iter()
        .filter_map(|event| match event {
            CoreEvent::ClipboardUpserted { deduplicated, .. } => Some(deduplicated),
            _ => None,
        })
        .collect();
    assert_eq!(upserts, [false, true]);
}

#[test]
fn capture_uses_current_volume_and_preview_passes_its_own_clamped_volume() {
    let fixture = Fixture::new();
    let core = fixture.start();
    copy_in(&core, text("sound disabled"));
    assert_eq!(fixture.platform.sounds(), 0);
    for volume in [80, 0, 255] {
        block_on(core.update_settings(json!({"clipboard": {"feedback": {
            "copySound": true, "copySoundVolume": volume
        }}})))
        .unwrap();
        copy_in(&core, text(&format!("volume {volume}")));
    }
    core.play_copy_sound(25);
    core.play_copy_sound(255);
    block_on(core.update_settings(json!({"clipboard": {"feedback": {"copySound": false}}})))
        .unwrap();
    copy_in(&core, text("sound disabled again"));
    assert_eq!(
        *fixture.platform.sound_volumes.lock().unwrap(),
        [80, 0, 100, 25, 100]
    );
    assert_eq!(fixture.platform.sounds(), 5);
}

/// 同步计数器的当前值与某条记录的序号。
fn sync_numbers(core: &Core, id: &str) -> (i64, Option<i64>) {
    block_on(core.hop({
        let core = core.clone();
        let id = id.to_owned();
        async move {
            let pool = core.0.db.pool().await;
            let seq: Option<i64> =
                sqlx::query_scalar("SELECT sync_seq FROM clipboard_items WHERE id = ?")
                    .bind(&id)
                    .fetch_one(&pool)
                    .await
                    .unwrap();
            Ok((crate::db::sync::sync_counter(&pool).await?, seq))
        }
    }))
    .unwrap()
}

#[test]
fn one_copy_takes_one_sync_number() {
    let fixture = Fixture::new();
    let core = fixture.start();
    block_on(core.update_settings(json!({"clipboard": {"feedback": {"copySound": true}}})))
        .unwrap();
    let mut repeats = RepeatFilter::new(Duration::from_secs(60));

    // .NET 的 Clipboard.SetText 这类写法：一次复制，两次剪贴板通知。
    let first = notify_in(&core, text("copied once"), &mut repeats).unwrap();
    assert_eq!(notify_in(&core, text("copied once"), &mut repeats), None);
    assert_eq!(sync_numbers(&core, &first), (1, Some(1)));
    assert_eq!(use_count(&core, &first), 1);
    assert_eq!(fixture.platform.sounds(), 1);

    let other = notify_in(&core, text("something else"), &mut repeats).unwrap();
    assert_eq!(sync_numbers(&core, &other), (2, Some(2)));

    // 中间复制过别的内容，再复制一次是真的再次复制：重新编号。
    assert_eq!(
        notify_in(&core, text("copied once"), &mut repeats),
        Some(first.clone())
    );
    assert_eq!(sync_numbers(&core, &first), (3, Some(3)));
    assert_eq!(use_count(&core, &first), 2);
    assert_eq!(fixture.platform.sounds(), 3);
}

#[test]
fn writing_back_a_just_copied_record_still_consumes_the_writeback_guard() {
    let fixture = Fixture::new();
    let core = fixture.start();
    let mut repeats = RepeatFilter::new(Duration::from_secs(60));

    let id = notify_in(&core, text("pasted right back"), &mut repeats).unwrap();
    block_on(core.copy_item(&id, false)).unwrap();
    let state = fixture.clipboard.snapshot();
    // 写回的通知既是自身写回，也是刚才那次复制的「重复」。
    assert_eq!(notify_in(&core, state.clone(), &mut repeats), None);

    // 过了合并窗口再复制同样的内容，是真的复制：写回登记已经消费掉，不会把它吞掉。
    assert_eq!(copy_in(&core, state), Some(id));
}

#[test]
fn capture_skips_excluded_apps_self_writes_and_pauses() {
    let fixture = Fixture::new();
    let core = fixture.start();
    block_on(core.update_settings(
        json!({"clipboard": {"filters": {"excludedAppIds": ["C:/Apps/Vault.exe"]}}}),
    ))
    .unwrap();

    fixture.platform.set_frontmost("C:/Apps/Vault.exe", "Vault");
    assert_eq!(copy_in(&core, text("hunter2 secret")), None);

    fixture
        .platform
        .set_frontmost("C:/Apps/Editor.exe", "Editor");
    core.set_capture_paused(true);
    assert_eq!(copy_in(&core, text("while paused")), None);
    core.set_capture_paused(false);

    // 自己写回的内容被回环抑制吞掉一次；再复制一次同样的内容就是用户的真实复制。
    let id = copy_in(&core, text("self writeback content")).unwrap();
    block_on(core.copy_item(&id, false)).unwrap();
    let state = fixture.clipboard.snapshot();
    assert_eq!(copy_in(&core, state.clone()), None);
    assert_eq!(copy_in(&core, state), Some(id));
}

/// 勾选时存下的是商店应用旧版本的路径；应用升级换了包目录后，新版本复制的内容照样不入库。
#[test]
fn capture_skips_later_versions_of_excluded_apps() {
    let old = r"C:\Program Files\WindowsApps\Claude_2.19675.0.0_x64__pzs8sxrjxfjjc\app\claude.exe";
    let new = r"C:\Program Files\WindowsApps\Claude_2.19675.1.0_x64__pzs8sxrjxfjjc\app\claude.exe";
    let fixture = Fixture::new();
    let core = fixture.start();
    block_on(core.update_settings(json!({"clipboard": {"filters": {"excludedAppIds": [old]}}})))
        .unwrap();

    fixture.platform.set_frontmost(new, "claude");
    assert_eq!(copy_in(&core, text("from the updated app")), None);

    fixture.platform.set_frontmost(
        r"C:\Program Files\WindowsApps\OpenAI.Codex_26.930.3930.0_x64__2p2nqsd0c76g0\app\ChatGPT.exe",
        "ChatGPT",
    );
    assert!(copy_in(&core, text("from another store app")).is_some());
    assert_eq!(core.settings().clipboard.filters.excluded_app_ids, [old]);
}

#[test]
fn captured_images_store_the_origin_and_thumbnail_lazily() {
    let fixture = Fixture::new();
    let core = fixture.start();

    let id = copy_in(
        &core,
        MemoryState {
            png: Some(sample_png(40, 24)),
            ..MemoryState::default()
        },
    )
    .unwrap();
    let item = block_on(core.find_item(&id)).unwrap().unwrap();

    assert_eq!(item.kind, ClipboardKind::Image);
    assert_eq!((item.width, item.height), (Some(40), Some(24)));
    assert!(core.image_origin_path(&item.content).unwrap().is_file());
    assert!(!core.image_store().thumbnail_path(&item.content).exists());
    assert!(block_on(core.ensure_thumbnail(&item.content))
        .unwrap()
        .is_file());
}

#[test]
fn read_clipboard_now_captures_and_dedups() {
    let fixture = Fixture::new();
    let core = fixture.start();

    assert!(block_on(core.read_clipboard_now()).unwrap().is_none());

    ClipboardBackend::set_text(&fixture.clipboard, "manual read".to_owned()).unwrap();
    let first = block_on(core.read_clipboard_now()).unwrap().unwrap();
    let second = block_on(core.read_clipboard_now()).unwrap().unwrap();

    assert!(!first.deduplicated);
    assert!(second.deduplicated);
    assert_eq!(first.id, second.id);
    assert_eq!(first.item.content, "manual read");
}

#[test]
fn copy_and_paste_follow_plain_text_settings_and_reuse() {
    let fixture = Fixture::new();
    let core = fixture.start();
    let id = copy_in(
        &core,
        MemoryState {
            text: Some("Hello World".to_owned()),
            html: Some("<b>Hello</b> World".to_owned()),
            ..MemoryState::default()
        },
    )
    .unwrap();
    fixture.take_events();

    let outcome = block_on(core.copy_item(&id, false)).unwrap();
    assert!(!outcome.hide_window);
    assert_eq!(
        fixture.clipboard.snapshot(),
        MemoryState {
            text: Some("Hello World".to_owned()),
            html: Some("<b>Hello</b> World".to_owned()),
            ..MemoryState::default()
        }
    );
    // 默认不计复用：次数不变、不发入库事件，只记下最后使用时间。
    // 后台清理随时可能发出 CleanupStatus，所以只看入库事件。
    assert_eq!(use_count(&core, &id), 1);
    assert!(!fixture
        .take_events()
        .iter()
        .any(|event| matches!(event, CoreEvent::ClipboardUpserted { .. })));

    block_on(core.update_settings(json!({"clipboard": {"content": {
        "pastePlain": true, "updateOnReuse": true, "copyThenHideWindow": true
    }}})))
    .unwrap();
    fixture.take_events();

    block_on(core.prepare_paste(&id, false)).unwrap();
    assert_eq!(
        fixture.clipboard.snapshot(),
        MemoryState {
            text: Some("Hello World".to_owned()),
            ..MemoryState::default()
        }
    );
    assert_eq!(use_count(&core, &id), 2);
    assert!(fixture.take_events().iter().any(|event| matches!(
        event,
        CoreEvent::ClipboardUpserted {
            deduplicated: true,
            ..
        }
    )));

    assert!(block_on(core.copy_item(&id, false)).unwrap().hide_window);
    assert!(block_on(core.copy_item("missing", false)).is_err());
}

#[test]
fn fragments_and_word_split_follow_the_record() {
    let fixture = Fixture::new();
    let core = fixture.start();
    let id = copy_in(&core, text("订单 20260924 已发货")).unwrap();

    block_on(core.copy_fragment(
        &id,
        ClipboardFragment::Snippet {
            text: "20260924".to_owned(),
        },
    ))
    .unwrap();
    assert_eq!(
        fixture.clipboard.snapshot().text.as_deref(),
        Some("20260924")
    );

    block_on(core.prepare_paste_fragment(
        &id,
        ClipboardFragment::Words {
            indices: vec![0, 1],
        },
    ))
    .unwrap();
    assert_eq!(fixture.clipboard.snapshot().text.as_deref(), Some("订单"));

    let err = block_on(core.copy_fragment(
        &id,
        ClipboardFragment::Snippet {
            text: "不存在".to_owned(),
        },
    ))
    .unwrap_err();
    assert_eq!(err.to_string(), "所选内容已不在这条记录中");

    let split = block_on(core.split_item(&id)).unwrap();
    assert_eq!(split.tokens[0].text, "订");

    let secret = copy_in(&core, text("sk-abcdefghijklmnopqrstuvwxyzABCDE1234567890")).unwrap();
    assert_eq!(
        block_on(core.split_item(&secret)).unwrap_err().to_string(),
        "敏感内容已脱敏显示，不能拆词"
    );
    let image = copy_in(
        &core,
        MemoryState {
            png: Some(sample_png(2, 2)),
            ..MemoryState::default()
        },
    )
    .unwrap();
    assert_eq!(
        block_on(core.split_item(&image)).unwrap_err().to_string(),
        "只有文本记录可以拆词"
    );
}

#[test]
fn quick_paste_writes_the_nth_record_once_at_a_time() {
    let fixture = Fixture::new();
    let core = fixture.start();
    copy_in(&core, text("older")).unwrap();
    std::thread::sleep(Duration::from_millis(5));
    let newest = copy_in(&core, text("newest")).unwrap();

    let ticket = block_on(core.prepare_quick_paste(0)).unwrap().unwrap();
    assert_eq!(ticket.item_id, newest);
    assert_eq!(fixture.clipboard.snapshot().text.as_deref(), Some("newest"));
    // 上一次还没粘完：新的触发直接忽略。
    assert!(block_on(core.prepare_quick_paste(1)).unwrap().is_none());

    drop(ticket);
    let second = block_on(core.prepare_quick_paste(1)).unwrap().unwrap();
    assert_eq!(fixture.clipboard.snapshot().text.as_deref(), Some("older"));
    drop(second);

    assert!(block_on(core.prepare_quick_paste(9)).unwrap().is_none());
    assert!(block_on(core.prepare_quick_paste(0)).unwrap().is_some());
}

#[test]
fn notes_marks_and_groups() {
    let fixture = Fixture::new();
    let core = fixture.start();
    let id = copy_in(&core, text("group me")).unwrap();

    assert!(block_on(core.toggle_favorite(&id)).unwrap());
    assert!(!block_on(core.toggle_favorite(&id)).unwrap());
    assert!(block_on(core.toggle_pinned(&id)).unwrap());

    let note = block_on(core.update_note(&id, Some("  记一下 ".to_owned()))).unwrap();
    assert_eq!(note.note.as_deref(), Some("记一下"));
    assert!(!note.auto_favorited);
    block_on(core.update_settings(json!({"clipboard": {"content": {"autoFavorite": true}}})))
        .unwrap();
    assert!(
        block_on(core.update_note(&id, Some("再记".to_owned())))
            .unwrap()
            .auto_favorited
    );
    assert!(block_on(core.find_item(&id)).unwrap().unwrap().is_favorite);
    assert_eq!(
        block_on(core.update_note(&id, Some("   ".to_owned())))
            .unwrap()
            .note,
        None
    );

    fixture.take_events();
    let group = block_on(core.create_group(ClipboardGroupInput {
        name: " 工作 ".to_owned(),
        icon: String::new(),
        is_hidden: false,
    }))
    .unwrap();
    assert_eq!(group.name, "工作");
    assert_eq!(group.icon, super::DEFAULT_CLIPBOARD_GROUP_ICON);
    assert!(block_on(core.set_item_group(&id, "nope")).is_err());
    block_on(core.set_item_group(&id, &group.id)).unwrap();
    assert_eq!(
        block_on(core.find_item(&id)).unwrap().unwrap().group_id,
        Some(group.id.clone())
    );

    block_on(core.update_group(
        &group.id,
        ClipboardGroupInput {
            name: "Work".to_owned(),
            icon: "i-lets-icons:star".to_owned(),
            is_hidden: false,
        },
    ))
    .unwrap();
    block_on(core.update_groups_layout(ClipboardGroupLayoutInput {
        order: vec![group.id.clone()],
        visible_ids: Vec::new(),
    }))
    .unwrap();
    let groups = block_on(core.list_groups()).unwrap();
    assert_eq!(groups[0].name, "Work");
    assert!(groups[0].is_hidden);
    block_on(core.delete_group(&group.id)).unwrap();
    assert_eq!(
        block_on(core.find_item(&id)).unwrap().unwrap().group_id,
        None
    );

    let updates = fixture
        .take_events()
        .iter()
        .filter(|event| matches!(event, CoreEvent::GroupsUpdated))
        .count();
    assert_eq!(updates, 4);

    let svg = fixture.root().join("icon.svg");
    std::fs::write(&svg, "<svg><path d='M0 0'/></svg>").unwrap();
    assert!(core.import_group_svg(&svg).is_ok());
    std::fs::write(&svg, "<svg><script/></svg>").unwrap();
    assert!(core.import_group_svg(&svg).is_err());
    assert!(core
        .import_group_svg(&fixture.root().join("icon.png"))
        .is_err());
}

#[test]
fn deletes_remove_image_files_and_clears_notify() {
    let fixture = Fixture::new();
    let core = fixture.start();
    let image = copy_in(
        &core,
        MemoryState {
            png: Some(sample_png(8, 8)),
            ..MemoryState::default()
        },
    )
    .unwrap();
    let file = block_on(core.find_item(&image)).unwrap().unwrap().content;
    let origin = core.image_origin_path(&file).unwrap();
    let kept = copy_in(&core, text("favorite")).unwrap();
    block_on(core.toggle_favorite(&kept)).unwrap();
    let a = copy_in(&core, text("a")).unwrap();
    let b = copy_in(&core, text("b")).unwrap();

    let refs = block_on(core.list_item_refs(ClipboardItemQuery::default())).unwrap();
    assert_eq!(refs.len(), 4);
    assert!(refs.iter().any(|r| r.id == kept && r.is_favorite));

    block_on(core.delete_item(&image)).unwrap();
    assert!(!origin.exists());
    assert_eq!(
        block_on(core.delete_items(vec![a, "missing".to_owned()])).unwrap(),
        1
    );

    fixture.take_events();
    assert_eq!(block_on(core.clear_items(false, false)).unwrap(), 1);
    assert!(block_on(core.find_item(&b)).unwrap().is_none());
    assert!(block_on(core.find_item(&kept)).unwrap().is_some());
    assert!(fixture
        .take_events()
        .iter()
        .any(|event| matches!(event, CoreEvent::ClipboardCleaned { removed: 1 })));

    let image = copy_in(
        &core,
        MemoryState {
            png: Some(sample_png(9, 9)),
            ..MemoryState::default()
        },
    )
    .unwrap();
    let origin = core
        .image_origin_path(&block_on(core.find_item(&image)).unwrap().unwrap().content)
        .unwrap();
    assert_eq!(
        block_on(core.clear_items_in_scope(ClearScope::Category {
            category: ContentCategory::Image
        }))
        .unwrap(),
        1
    );
    assert!(!origin.exists());
}

#[test]
fn link_reveal_and_image_save_targets() {
    let fixture = Fixture::new();
    let core = fixture.start();
    let url = copy_in(&core, text("www.example.com")).unwrap();
    let image = copy_in(
        &core,
        MemoryState {
            png: Some(sample_png(3, 3)),
            ..MemoryState::default()
        },
    )
    .unwrap();

    assert_eq!(
        block_on(core.link_target(&url, false)).unwrap().as_deref(),
        Some("https://www.example.com")
    );
    assert_eq!(
        block_on(core.reveal_target(&url)).unwrap().as_deref(),
        Some("www.example.com")
    );

    let save = block_on(core.prepare_image_save(&image)).unwrap();
    assert!(save.source.is_file());
    assert!(save.default_file_name.starts_with("KwikPaste-image-"));
    assert!(block_on(core.prepare_image_save(&url)).is_err());
}

#[test]
fn apps_list_merges_running_apps_and_manual_additions() {
    let fixture = Fixture::new();
    let core = fixture.start();
    fixture
        .platform
        .set_frontmost("C:/Apps/Editor.exe", "Editor");
    copy_in(&core, text("from editor")).unwrap();
    fixture
        .platform
        .running
        .lock()
        .unwrap()
        .push(crate::platform::ScannedApp {
            id: "C:/Apps/Browser.exe".to_owned(),
            name: "browser".to_owned(),
            path: None,
            platform: crate::db::models::Platform::Windows,
        });

    let apps = block_on(core.list_all_apps()).unwrap();
    let names: Vec<_> = apps.iter().map(|app| app.name.as_str()).collect();
    assert_eq!(names, ["browser", "Editor"]);

    let added = block_on(core.add_app_from_path("C:/Tools/viewer.exe".into())).unwrap();
    assert_eq!(added.name, "viewer");
    assert!(block_on(core.add_app_from_path("C:/Tools/readme.txt".into())).is_err());

    let deleted = block_on(
        core.delete_unreferenced_apps(vec![added.id.clone(), "C:/Apps/Editor.exe".to_owned()]),
    )
    .unwrap();
    assert_eq!(deleted, [added.id]);
}

/// 复制的是单个图片文件：卡片按图片记录的规则给显示尺寸（读文件头）和已生成的缩略图；
/// 缩略图是缓存，清理缓存时整个清掉，之后照样能重建。
#[test]
fn single_image_file_records_preview_like_image_records() {
    use crate::presenter::{image_display_size, FilesPreviewKind};

    let fixture = Fixture::new();
    let core = fixture.start();
    let photo = fixture.root().join("photos").join("wide.png");
    std::fs::create_dir_all(photo.parent().unwrap()).unwrap();
    std::fs::write(&photo, sample_png(900, 300)).unwrap();
    let photo_path = photo.to_string_lossy().into_owned();
    let id = copy_in(
        &core,
        MemoryState {
            files: Some(vec![photo_path.clone()]),
            ..MemoryState::default()
        },
    )
    .unwrap();
    let view = || block_on(core.list_item(&id)).unwrap().unwrap();

    let first = view();
    assert_eq!(
        first.files_preview_kind,
        Some(FilesPreviewKind::ImagePreview)
    );
    assert_eq!(
        first.image_display_size,
        Some(image_display_size(Some(900), Some(300), 64))
    );
    assert!(first.image_thumbnail_path.is_none());

    let thumb = block_on(core.ensure_file_thumbnail(&photo_path)).unwrap();
    assert!(thumb.is_file());
    assert_eq!(view().image_thumbnail_path.as_deref(), thumb.to_str());
    assert!(block_on(core.ensure_file_thumbnail("relative/wide.png")).is_err());
    assert!(block_on(core.ensure_file_thumbnail(&format!("{photo_path}.txt"))).is_err());

    let overview = block_on(core.storage_overview()).unwrap();
    assert_eq!(overview.reclaimable.files, 1);
    let cleaned = block_on(core.clean_resource_cache()).unwrap();
    assert_eq!(cleaned.removed_files, 1);
    assert!(!thumb.exists());
    assert!(view().image_thumbnail_path.is_none());
    assert_eq!(
        block_on(core.ensure_file_thumbnail(&photo_path)).unwrap(),
        thumb
    );
    block_on(core.shutdown()).unwrap();
}

/// 清理缓存连带删除路径已全部不在磁盘上的文件记录；还有文件在的、收藏的保留。
#[test]
fn clean_resource_cache_removes_file_records_whose_paths_are_gone() {
    let fixture = Fixture::new();
    let core = fixture.start();
    let dir = fixture.root().join("docs");
    std::fs::create_dir_all(&dir).unwrap();
    let alive = dir.join("alive.txt");
    std::fs::write(&alive, b"x").unwrap();
    let path = |name: &str| dir.join(name).to_string_lossy().into_owned();
    let copy_files = |paths: Vec<String>| {
        copy_in(
            &core,
            MemoryState {
                files: Some(paths),
                ..MemoryState::default()
            },
        )
        .unwrap()
    };

    let all_gone = copy_files(vec![path("a.txt")]);
    let partly_gone = copy_files(vec![path("b.txt"), path("alive.txt")]);
    let favorite = copy_files(vec![path("c.txt")]);
    block_on(core.toggle_favorite(&favorite)).unwrap();

    block_on(core.clean_resource_cache()).unwrap();

    let exists = |id: &str| block_on(core.find_item(id)).unwrap().is_some();
    assert!(!exists(&all_gone));
    assert!(exists(&partly_gone));
    assert!(exists(&favorite));
    block_on(core.shutdown()).unwrap();
}

/// 图片扩展名的文件读不出图片头（没有读取权限、内容不是支持的格式）：按普通文件行显示，不画坏图。
#[test]
fn unreadable_image_file_records_preview_as_a_file_row() {
    use crate::presenter::FilesPreviewKind;

    let fixture = Fixture::new();
    let core = fixture.start();
    let photo = fixture.root().join("photos").join("broken.png");
    std::fs::create_dir_all(photo.parent().unwrap()).unwrap();
    std::fs::write(&photo, b"not an image").unwrap();
    let id = copy_in(
        &core,
        MemoryState {
            files: Some(vec![photo.to_string_lossy().into_owned()]),
            ..MemoryState::default()
        },
    )
    .unwrap();

    let view = block_on(core.list_item(&id)).unwrap().unwrap();
    assert_eq!(view.files_preview_kind, Some(FilesPreviewKind::List));
    assert_eq!(view.image_display_size, None);
    block_on(core.shutdown()).unwrap();
}

/// 数据模型外的本地排序、最后使用时间和同步序号也必须原样保留。
fn edit_metadata(core: &Core, id: &str) -> (String, Option<i64>, Option<i64>, Option<i64>) {
    block_on(core.hop({
        let core = core.clone();
        let id = id.to_owned();
        async move {
            Ok(sqlx::query_as("SELECT last_used_at, sync_seq, pin_order, favorite_order FROM clipboard_items WHERE id = ?")
                .bind(id).fetch_one(&core.0.db.pool().await).await.unwrap())
        }
    })).unwrap()
}

#[test]
fn text_edit_rebuilds_content_and_search_without_changing_metadata() {
    let fixture = Fixture::new();
    let core = fixture.start();
    let id = copy_in(&core, text("oldneedle old record")).unwrap();
    let group = block_on(core.create_group(ClipboardGroupInput {
        name: "Edits".into(),
        icon: "folder".into(),
        is_hidden: false,
    }))
    .unwrap();
    block_on(core.set_item_group(&id, &group.id)).unwrap();
    block_on(core.update_note(&id, Some("kept note".into()))).unwrap();
    block_on(core.toggle_favorite(&id)).unwrap();
    block_on(core.toggle_pinned(&id)).unwrap();
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
    let before = block_on(core.find_item(&id)).unwrap().unwrap();
    let metadata = edit_metadata(&core, &id);
    let sync = sync_numbers(&core, &id);
    fixture.take_events();
    // 编辑不是采集：禁用文本采集不能阻止修改现有记录。
    block_on(core.update_settings(json!({"clipboard": {"capture": {"text": false}}}))).unwrap();
    let content = "  https://example.com/newneedle\n";
    block_on(core.update_text_content(&id, content.to_owned())).unwrap();
    let after = block_on(core.find_item(&id)).unwrap().unwrap();
    assert_eq!(after.content, content);
    assert_eq!(
        after.content_hash,
        crate::db::items::content_hash(ClipboardKind::Text, content)
    );
    assert_eq!(after.search_text.as_deref(), Some(content));
    assert_eq!(after.summary.as_deref(), Some(content.trim()));
    assert_eq!(
        after.sub_kind,
        Some(crate::db::models::ClipboardSubKind::Url)
    );
    assert_eq!(after.size, Some(content.len() as i64));
    assert_eq!(
        (after.file_types.as_ref(), after.width, after.height),
        (None, None, None)
    );
    let mut expected = before;
    crate::clipboard::rewrite_text_content(&mut expected, content);
    assert_eq!(
        serde_json::to_value(&after).unwrap(),
        serde_json::to_value(expected).unwrap()
    );
    assert_eq!(edit_metadata(&core, &id), metadata);
    assert_eq!(sync_numbers(&core, &id), sync);
    for (keyword, total) in [("oldneedle", 0), ("newneedle", 1)] {
        assert_eq!(
            block_on(core.list_items(ClipboardItemQuery {
                keyword: Some(keyword.into()),
                ..Default::default()
            }))
            .unwrap()
            .total,
            total
        );
    }
    assert!(!fixture
        .take_events()
        .iter()
        .any(|event| matches!(event, CoreEvent::ClipboardUpserted { .. })));
}

#[test]
fn rich_text_edit_drops_formats_and_unchanged_save_is_a_noop() {
    let fixture = Fixture::new();
    let core = fixture.start();
    for state in [
        MemoryState {
            text: Some("HTML original".into()),
            html: Some("<b>HTML original</b>".into()),
            ..Default::default()
        },
        MemoryState {
            text: Some("RTF original".into()),
            rtf: Some(r"{\rtf1 RTF original}".into()),
            ..Default::default()
        },
    ] {
        let id = copy_in(&core, state).unwrap();
        let before = block_on(core.find_item(&id)).unwrap().unwrap();
        assert!(matches!(
            before.sub_kind,
            Some(
                crate::db::models::ClipboardSubKind::Html
                    | crate::db::models::ClipboardSubKind::Rtf
            )
        ));
        block_on(core.update_text_content(&id, before.search_text.clone().unwrap())).unwrap();
        assert_eq!(
            serde_json::to_value(block_on(core.find_item(&id)).unwrap()).unwrap(),
            serde_json::to_value(Some(before)).unwrap()
        );
        block_on(core.update_text_content(&id, "  edited plain text  ".into())).unwrap();
        let after = block_on(core.find_item(&id)).unwrap().unwrap();
        assert_eq!(after.sub_kind, None);
        assert_eq!(after.content, "  edited plain text  ");
        block_on(core.copy_item(&id, false)).unwrap();
        assert_eq!(fixture.clipboard.snapshot(), text("  edited plain text  "));
        block_on(core.copy_item(&id, true)).unwrap();
        assert_eq!(fixture.clipboard.snapshot(), text("  edited plain text  "));
        block_on(core.prepare_paste(&id, false)).unwrap();
        assert_eq!(fixture.clipboard.snapshot(), text("  edited plain text  "));
    }
}

#[test]
fn text_edit_rejects_empty_and_non_text_records() {
    let fixture = Fixture::new();
    let core = fixture.start();
    let id = copy_in(&core, text("keep me")).unwrap();
    for empty in ["", " \n\t", "\u{3000}"] {
        let error = block_on(core.update_text_content(&id, empty.into())).unwrap_err();
        assert!(matches!(error, crate::error::AppError::Clipboard(_)));
        assert_eq!(error.to_string(), "Content cannot be empty");
    }
    assert_eq!(
        block_on(core.find_item(&id)).unwrap().unwrap().content,
        "keep me"
    );
    block_on(core.update_text_content(&id, "keep me".into())).unwrap();
    for state in [
        MemoryState {
            png: Some(sample_png(2, 2)),
            ..Default::default()
        },
        MemoryState {
            files: Some(vec!["C:/fixture.txt".into()]),
            ..Default::default()
        },
    ] {
        let id = copy_in(&core, state).unwrap();
        let before = block_on(core.find_item(&id)).unwrap().unwrap();
        assert_eq!(
            block_on(core.update_text_content(&id, "text".into()))
                .unwrap_err()
                .to_string(),
            "Only text records can be edited"
        );
        assert_eq!(
            serde_json::to_value(block_on(core.find_item(&id)).unwrap()).unwrap(),
            serde_json::to_value(Some(before)).unwrap()
        );
    }
}

#[test]
fn text_edit_allows_duplicate_hashes_and_capture_reuses_newest_created() {
    let fixture = Fixture::new();
    let core = fixture.start();
    let first = copy_in(&core, text("first unique")).unwrap();
    let second = copy_in(&core, text("shared content")).unwrap();
    // 明确创建时间以避免同一时钟精度下的并列。
    block_on(core.hop({
        let core = core.clone();
        let first = first.clone();
        async move {
            sqlx::query(
                "UPDATE clipboard_items SET created_at = '2000-01-01T00:00:00Z' WHERE id = ?",
            )
            .bind(first)
            .execute(&core.0.db.pool().await)
            .await
            .unwrap();
            Ok(())
        }
    }))
    .unwrap();
    block_on(core.update_text_content(&first, "shared content".into())).unwrap();
    assert_eq!(
        block_on(core.find_item(&first))
            .unwrap()
            .unwrap()
            .content_hash,
        block_on(core.find_item(&second))
            .unwrap()
            .unwrap()
            .content_hash
    );
    assert_eq!(copy_in(&core, text("shared content")).unwrap(), second);
    assert_eq!(use_count(&core, &first), 1);
    assert_eq!(use_count(&core, &second), 2);
    assert_eq!(
        block_on(core.list_items(ClipboardItemQuery::default()))
            .unwrap()
            .total,
        2
    );
}
