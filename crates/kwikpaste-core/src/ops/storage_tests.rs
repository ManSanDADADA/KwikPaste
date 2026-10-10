//! 存储位置切换与清理缓存的端到端测试：真实的 core（临时目录），内存剪贴板，绝不碰本机数据目录。

use std::fs;
use std::time::Duration;

use serde_json::json;

use super::PreferenceDirectory;
use crate::clipboard::{MemoryClipboard, MemoryState};
use crate::db::models::ClipboardItemQuery;
use crate::env::AppEnv;
use crate::events::CoreEvent;
use crate::i18n::commands::{label, Key};
use crate::paths::CorePaths;
use crate::root::Core;
use crate::settings::Theme;
use crate::testing::{block_on, sample_png, Fixture};
use crate::window_state::WindowGeometry;

fn store(core: &Core, state: MemoryState) -> String {
    let clipboard = MemoryClipboard::with_state(state);
    let item = core
        .build_item(&core.read_payload(&clipboard).unwrap().unwrap())
        .unwrap()
        .unwrap();
    block_on(core.store_item(item, None)).unwrap().id
}

fn text(value: &str) -> MemoryState {
    MemoryState {
        text: Some(value.to_owned()),
        ..MemoryState::default()
    }
}

fn image() -> MemoryState {
    MemoryState {
        png: Some(sample_png(12, 8)),
        ..MemoryState::default()
    }
}

fn geometry() -> WindowGeometry {
    WindowGeometry {
        x: 100.0,
        y: 120.0,
        width: 420.0,
        height: 640.0,
        scale: 1.25,
    }
}

#[test]
fn change_and_reset_storage_location_move_data_and_rebase_every_store() {
    let fixture = Fixture::new();
    let core = fixture.start();
    let default_dir = fixture.paths.default_data_dir();
    let sync_identity = fixture.paths.sync_dir().join("identity.json");
    fs::create_dir_all(sync_identity.parent().unwrap()).unwrap();
    fs::write(&sync_identity, b"{}").unwrap();
    let staged = fixture.root().join("staged-ocr");
    fs::write(&staged, b"disabled fixture extension").unwrap();
    block_on(core.install_extension("ocr", "1.0.0", crate::extensions::OCR_PROTOCOL, &staged))
        .unwrap();
    block_on(core.set_extension_enabled("ocr", false)).unwrap();
    let ext_dir = fixture.paths.extensions_dir();
    let ext_exe = ext_dir
        .join("ocr/1.0.0")
        .join(crate::extensions::executable_name("ocr").unwrap());
    fs::write(ext_dir.join("ocr/state/data"), b"machine-local").unwrap();

    block_on(core.update_settings(json!({"appearance": {"theme": "dark"}}))).unwrap();
    let text_id = store(&core, text("moved with the data dir"));
    let image_id = store(&core, image());
    core.window_state().save("main", geometry()).unwrap();
    fixture.take_events();

    let parent = fixture.root().join("elsewhere");
    let result = block_on(core.change_storage_location(parent.clone())).unwrap();
    let custom = fixture.paths.custom_data_dir(&parent);

    assert!(result.location.is_custom);
    assert_eq!(result.location.current_path, custom.to_string_lossy());
    assert!(result.storage_usage.database_bytes > 0);
    assert!(custom.join("db").join("clipboard.db").is_file());
    assert!(custom.join(".kwikpaste-storage.json").is_file());
    // 启动锚点会保留本机扩展二进制文件、自有状态和同步身份。
    let mut left: Vec<_> = fs::read_dir(&default_dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    left.sort();
    assert_eq!(left, ["extensions", "storage.json", "sync"]);
    assert!(ext_exe.is_file());
    assert_eq!(
        fs::read(ext_dir.join("ocr/state/data")).unwrap(),
        b"machine-local"
    );
    assert!(!custom.join("extensions").exists());
    assert!(!core.installed_extensions()["ocr"].enabled);
    assert!(sync_identity.is_file());

    let listed = block_on(core.list_items(ClipboardItemQuery::default())).unwrap();
    assert_eq!(listed.total, 2);
    let image_item = block_on(core.find_item(&image_id)).unwrap().unwrap();
    let origin = core.image_origin_path(&image_item.content).unwrap();
    assert!(origin.starts_with(&custom));
    assert!(origin.is_file());
    assert_eq!(core.settings().appearance.theme, Theme::Dark);
    assert_eq!(core.window_state().get("main"), Some(geometry()));
    assert!(!core.0.watcher_pause.is_paused());
    assert_eq!(
        core.preference_directory(PreferenceDirectory::Data)
            .unwrap(),
        custom
    );

    let events = fixture.take_events();
    assert!(events.iter().any(|event| matches!(
        event,
        CoreEvent::SettingsUpdated { delta, .. } if delta.touches("appearance.theme")
    )));
    assert!(events
        .iter()
        .any(|event| matches!(event, CoreEvent::ClipboardReloaded)));

    // 切换后采集与设置都写到新位置。
    let after = store(&core, text("captured after the move"));
    block_on(core.update_settings(json!({"appearance": {"theme": "light"}}))).unwrap();
    let saved = fs::read_to_string(custom.join("config").join("settings.json")).unwrap();
    assert!(saved.contains("\"light\""));

    let reset = block_on(core.reset_storage_location()).unwrap();
    assert!(!reset.location.is_custom);
    assert!(!custom.exists());
    assert!(ext_exe.is_file());
    assert_eq!(
        fs::read(ext_dir.join("ocr/state/data")).unwrap(),
        b"machine-local"
    );
    assert!(!parent.join("KwikPasteData").exists());
    for id in [&text_id, &image_id, &after] {
        assert!(block_on(core.find_item(id)).unwrap().is_some());
    }
    let origin = core.image_origin_path(&image_item.content).unwrap();
    assert!(origin.starts_with(&default_dir));
    assert!(origin.is_file());
    assert_eq!(core.settings().appearance.theme, Theme::Light);

    // 已在目标位置时什么都不搬，直接返回当前位置。
    let again = block_on(core.reset_storage_location()).unwrap();
    assert!(!again.location.is_custom);
    block_on(core.shutdown()).unwrap();
}

#[test]
fn storage_switch_keeps_the_hosts_capture_pause() {
    let fixture = Fixture::new();
    let core = fixture.start();
    core.set_capture_paused(true);

    block_on(core.change_storage_location(fixture.root().join("elsewhere"))).unwrap();

    assert!(core.0.watcher_pause.is_paused());
    block_on(core.shutdown()).unwrap();
}

#[test]
fn storage_switch_rejects_nested_and_foreign_targets_without_moving() {
    let fixture = Fixture::new();
    let core = fixture.start();
    let id = store(&core, text("stays put"));
    let current = fixture.paths.app_data_dir().unwrap();

    // 目标在当前数据目录里面。
    assert!(block_on(core.change_storage_location(current.join("nested"))).is_err());

    // 目标已存在、非空，又不是 KwikPaste 数据目录。
    let foreign_parent = fixture.root().join("foreign");
    let foreign = fixture.paths.custom_data_dir(&foreign_parent);
    fs::create_dir_all(&foreign).unwrap();
    fs::write(foreign.join("notes.txt"), b"not KwikPaste").unwrap();
    assert!(block_on(core.change_storage_location(foreign_parent)).is_err());
    assert!(foreign.join("notes.txt").is_file());

    assert_eq!(fixture.paths.app_data_dir().unwrap(), current);
    assert!(block_on(core.find_item(&id)).unwrap().is_some());
    assert!(!core.0.watcher_pause.is_paused());
    block_on(core.shutdown()).unwrap();
}

#[test]
fn storage_switch_rejects_existing_kwikpaste_data_target() {
    let fixture = Fixture::new();
    let core = fixture.start();
    let parent = fixture.root().join("existing");
    let target = fixture.paths.custom_data_dir(&parent);
    fixture.paths.write_storage_identity(&target).unwrap();
    fs::create_dir_all(target.join("db")).unwrap();
    fs::write(target.join("db").join("clipboard.db"), b"existing").unwrap();

    let error = block_on(core.change_storage_location(parent))
        .unwrap_err()
        .to_string();
    assert_eq!(error, label(core.language(), Key::StorageTargetHasData));
    block_on(core.shutdown()).unwrap();
}

#[test]
fn unavailable_custom_storage_cannot_be_moved_until_restart() {
    let mut fixture = Fixture::new();
    let local = fixture.root().join("local");
    let installed = || CorePaths::new(AppEnv::Dev, local.clone(), local.join("logs"), None);
    let custom = installed().custom_data_dir(&fixture.root().join("usb"));
    installed().set_app_data_dir(custom.clone()).unwrap();
    fs::remove_dir_all(&custom).unwrap();
    fixture.paths = installed();
    let core = fixture.start();

    let err = block_on(core.change_storage_location(fixture.root().join("elsewhere")))
        .unwrap_err()
        .to_string();
    assert_eq!(err, label(core.language(), Key::StorageCustomUnavailable));
    assert!(block_on(core.reset_storage_location()).is_err());
    assert_eq!(
        core.storage_location().unwrap().unavailable_custom_path,
        Some(custom.to_string_lossy().into_owned())
    );
    block_on(core.shutdown()).unwrap();
}

#[test]
fn portable_storage_cannot_be_moved() {
    let mut fixture = Fixture::new();
    let local = fixture.root().join("local");
    fixture.paths = CorePaths::new(
        AppEnv::Dev,
        local.clone(),
        local.join("logs"),
        Some(fixture.root().join("usb").join("data")),
    );
    let core = fixture.start();

    let err = block_on(core.change_storage_location(fixture.root().join("elsewhere")))
        .unwrap_err()
        .to_string();
    assert_eq!(err, label(core.language(), Key::PortableStorageFixed));
    assert!(block_on(core.reset_storage_location()).is_err());
    assert_eq!(
        core.preference_directory(PreferenceDirectory::Logs)
            .unwrap(),
        fixture.root().join("usb").join("data").join("logs")
    );
    block_on(core.shutdown()).unwrap();
}

#[test]
fn storage_overview_counts_reclaimable_cache_and_history() {
    let fixture = Fixture::new();
    let core = fixture.start();
    store(&core, text("overview text"));
    store(&core, image());
    let orphan_dir = fixture
        .paths
        .resources_dir()
        .unwrap()
        .join("clipboard-images")
        .join("origin")
        .join("zz");
    fs::create_dir_all(&orphan_dir).unwrap();
    fs::write(orphan_dir.join("orphan.png"), b"orphan").unwrap();

    let overview = block_on(core.storage_overview()).unwrap();

    assert_eq!(overview.history.totals.total, 2);
    assert_eq!(overview.reclaimable.files, 1);
    assert_eq!(overview.reclaimable.bytes, 6);
    assert!(overview.breakdown.image_bytes > 0);
    assert!(orphan_dir.join("orphan.png").is_file());
    block_on(core.shutdown()).unwrap();
}

/// VACUUM 不删记录：历史设置回落、自动清理暂停时照样执行，但要等正在跑的清理结束。
#[test]
fn clean_resource_cache_runs_while_paused_but_waits_for_cleanup() {
    let fixture = Fixture::new();
    fixture.write_settings(
        r#"{"clipboard": {"history": {"retention": {"value": 1, "unit": "minutes"}, "maxCount": "many"}}}"#,
    );
    let core = fixture.start();
    assert!(core.cleanup_paused());
    let image_id = store(&core, image());
    let image_item = block_on(core.find_item(&image_id)).unwrap().unwrap();
    let icons = fixture.paths.resources_dir().unwrap().join("app-icons");
    fs::create_dir_all(&icons).unwrap();
    fs::write(icons.join("stale.png"), b"stale-icon").unwrap();

    let (held_tx, held_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
    let holder = core.0.rt.spawn({
        let core = core.clone();
        async move {
            let _exclusive = core.0.cleanup.exclusive().await;
            held_tx.send(()).unwrap();
            release_rx.await.ok();
        }
    });
    block_on(held_rx).unwrap();

    let cleaning = core.0.rt.spawn({
        let core = core.clone();
        async move { core.clean_resource_cache().await }
    });
    std::thread::sleep(Duration::from_millis(300));
    assert!(!cleaning.is_finished());

    release_tx.send(()).unwrap();
    block_on(holder).unwrap();
    let result = block_on(cleaning).unwrap().unwrap();

    assert_eq!(result.removed_files, 1);
    assert_eq!(result.removed_bytes, 10);
    assert!(!icons.join("stale.png").exists());
    assert!(core
        .image_origin_path(&image_item.content)
        .unwrap()
        .is_file());
    assert!(block_on(core.find_item(&image_id)).unwrap().is_some());
    block_on(core.shutdown()).unwrap();
}

/// WAL / SHM 不计入占用：只往 WAL 里写（不 checkpoint）时，占用不会因为 WAL 变长而变少；
/// 结果等于跳过旁路文件后的目录大小减去 SQLite 可复用的空闲页。
#[test]
fn storage_in_use_skips_the_wal_and_shm() {
    let fixture = Fixture::new();
    let core = fixture.start();
    store(&core, text("first"));
    let before = block_on(core.storage_bytes_in_use()).unwrap();

    for index in 0..40 {
        store(
            &core,
            text(&format!("grows the wal {index} {}", "x".repeat(2000))),
        );
    }
    let db_path = crate::db::db_path(&fixture.paths).unwrap();
    let wal = std::path::PathBuf::from(format!("{}-wal", db_path.display()));
    let shm = std::path::PathBuf::from(format!("{}-shm", db_path.display()));
    assert!(fs::metadata(&wal).unwrap().len() > 0);

    let after = block_on(core.storage_bytes_in_use()).unwrap();
    assert!(after >= before, "{after} < {before}");

    let walked =
        crate::disk::dir_size_excluding(&fixture.paths.app_data_dir().unwrap(), &[wal, shm])
            .unwrap();
    let reusable = block_on(core.hop({
        let core = core.clone();
        async move {
            let pool = core.0.db.pool().await;
            crate::db::items::reusable_page_bytes(&pool).await
        }
    }))
    .unwrap();
    assert_eq!(after, walked - reusable);
    block_on(core.shutdown()).unwrap();
}
