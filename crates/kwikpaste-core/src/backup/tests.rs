//! 备份的格式测试（从 1.4.0 原样移植）与端到端导出导入测试（真实的 core 与临时目录）。

use std::io::Cursor;

use serde_json::json;
use tempfile::tempdir;
use zip::ZipArchive;

use super::*;
use crate::clipboard::{MemoryClipboard, MemoryState};
use crate::db::models::{ClipboardItemQuery, ClipboardKind};
use crate::ops::ClipboardGroupInput;
use crate::settings::Theme;
use crate::testing::{block_on, sample_png, Fixture};

const PASSWORD: &str = "correct horse battery";

#[test]
fn ecopaste_old_backup_always_merges_history() {
    let archive_fixture = Fixture::new();
    let root = archive_fixture.root();
    let db_path = root.join("EcoPaste.db");
    let created = "2020-01-02 12:34:56";
    archive_fixture.runtime.handle().block_on(async {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(SqliteConnectOptions::new().filename(&db_path).create_if_missing(true))
            .await.unwrap();
        sqlx::query(
            "CREATE TABLE history (id TEXT PRIMARY KEY, type TEXT, \"group\" TEXT, value TEXT, \
             search TEXT, count INTEGER, width INTEGER, height INTEGER, favorite INTEGER, \
             createTime TEXT, note TEXT, subtype TEXT)",
        ).execute(&pool).await.unwrap();
        for (id, kind, value, search, favorite, note) in [
            ("1", "text", "old favorite", "old favorite", 1, "  old note  "),
            ("2", "html", "<b>old html</b>", "old html", 0, ""),
            ("3", "image", r"C:\Users\old\EcoPaste\images\old.png", "", 0, ""),
            ("4", "files", r#"["C:\\old\\document.txt"]"#, "", 0, ""),
            ("5", "unknown", "ignored", "", 0, ""),
        ] {
            sqlx::query(
                "INSERT INTO history (id, type, value, search, favorite, createTime, note, width, height) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, 4, 6)",
            ).bind(id).bind(kind).bind(value).bind(search).bind(favorite).bind(created).bind(note)
                .execute(&pool).await.unwrap();
        }
        pool.close().await;
    });
    fs::create_dir(root.join("images")).unwrap();
    let png = sample_png(4, 6);
    fs::write(root.join("images/old.png"), &png).unwrap();
    fs::write(root.join(".store-backup.json"), b"not KwikPaste settings").unwrap();
    let gzip_path = root.join("old.tar.gz");
    let encoder = flate2::write::GzEncoder::new(
        File::create(&gzip_path).unwrap(),
        flate2::Compression::default(),
    );
    let mut archive = tar::Builder::new(encoder);
    archive
        .append_path_with_name(&db_path, "EcoPaste.db")
        .unwrap();
    archive
        .append_dir_all("images", root.join("images"))
        .unwrap();
    archive
        .append_path_with_name(root.join(".store-backup.json"), ".store-backup.json")
        .unwrap();
    archive.into_inner().unwrap().finish().unwrap();
    let backup = root.join("2020_01_02_12_34_56.EcoPaste-backup");
    fs::rename(gzip_path, &backup).unwrap();
    assert_eq!(
        inspect_backup_file(&backup).unwrap(),
        BackupContainerMode::Plain
    );

    for strategy in [BackupImportStrategy::Merge, BackupImportStrategy::Overwrite] {
        let fixture = Fixture::new();
        let core = fixture.start();
        let local_id = store(&core, text("local history"));
        let local_before = block_on(core.find_item(&local_id)).unwrap().unwrap();
        let settings_before = serde_json::to_value(core.settings()).unwrap();
        let sync_before = sync_state(&core);
        let sounds_before = fixture.platform.sounds();
        let result = block_on(core.import_history_backup(input(&backup, None), strategy)).unwrap();
        assert_eq!((result.imported_items, result.skipped_items), (4, 1));
        assert_eq!(result.strategy, BackupImportStrategy::Merge);
        assert!(!result.imported_settings && !result.requires_restart);
        assert_eq!(
            serde_json::to_value(core.settings()).unwrap(),
            settings_before
        );
        assert_eq!(
            serde_json::to_value(block_on(core.find_item(&local_id)).unwrap().unwrap()).unwrap(),
            serde_json::to_value(&local_before).unwrap()
        );
        assert_eq!(sync_state(&core), sync_before);
        assert_eq!(fixture.platform.sounds(), sounds_before);

        let list = block_on(core.list_items(ClipboardItemQuery::default())).unwrap();
        assert_eq!(list.total, 5);
        let items: Vec<_> = list
            .list
            .iter()
            .map(|view| block_on(core.find_item(&view.item.id)).unwrap().unwrap())
            .collect();
        let favorite = items
            .iter()
            .find(|item| item.content == "old favorite")
            .unwrap();
        assert!(favorite.is_favorite);
        assert_eq!(favorite.note.as_deref(), Some("old note"));
        let timestamp = Local
            .from_local_datetime(
                &NaiveDateTime::parse_from_str(created, "%Y-%m-%d %H:%M:%S").unwrap(),
            )
            .single()
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(favorite.created_at, timestamp);
        assert_eq!(favorite.updated_at, timestamp);
        assert!(!favorite.is_pinned && favorite.source_app_id.is_none());
        let html = items
            .iter()
            .find(|item| item.search_text.as_deref() == Some("old html"))
            .unwrap();
        assert_eq!(html.content, "<b>old html</b>");
        assert_eq!(
            html.sub_kind,
            Some(crate::db::models::ClipboardSubKind::Html)
        );
        let image = items
            .iter()
            .find(|item| item.kind == ClipboardKind::Image)
            .unwrap();
        assert_eq!(
            fs::read(core.image_origin_path(&image.content).unwrap()).unwrap(),
            png
        );
        assert_eq!((image.width, image.height), (Some(4), Some(6)));
        let files = items
            .iter()
            .find(|item| item.kind == ClipboardKind::Files)
            .unwrap();
        assert_eq!(files.content, r"C:\old\document.txt");

        // 重复内容不能覆盖用户后来修改的元数据。
        block_on(core.toggle_favorite(&favorite.id)).unwrap();
        let result = block_on(core.import_history_backup(input(&backup, None), strategy)).unwrap();
        assert_eq!((result.imported_items, result.skipped_items), (0, 5));
        assert!(
            !block_on(core.find_item(&favorite.id))
                .unwrap()
                .unwrap()
                .is_favorite
        );
        assert_eq!(
            block_on(core.list_items(ClipboardItemQuery::default()))
                .unwrap()
                .total,
            5
        );
        block_on(core.shutdown()).unwrap();
    }
}

#[test]
fn ecopaste_encrypted_magic_imports_with_password() {
    let source = source();
    let backup = export(&source, BackupExportMode::Encrypted, Some(PASSWORD));
    let mut bytes = fs::read(&backup).unwrap();
    bytes[..MAGIC.len()].copy_from_slice(ECOPASTE_MAGIC);
    let backup = backup.with_extension("ecopastebak");
    fs::write(&backup, bytes).unwrap();
    assert_eq!(
        inspect_backup_file(&backup).unwrap(),
        BackupContainerMode::Encrypted
    );
    let fixture = Fixture::new();
    let core = fixture.start();
    let result = block_on(
        core.import_history_backup(input(&backup, Some(PASSWORD)), BackupImportStrategy::Merge),
    )
    .unwrap();
    assert_eq!((result.imported_items, result.skipped_items), (3, 0));
    assert_eq!(contents(&core), contents(&source.core));
    assert!(core
        .image_origin_path(&source.image_file)
        .unwrap()
        .is_file());
    block_on(core.shutdown()).unwrap();
    block_on(source.core.shutdown()).unwrap();
}

#[test]
fn backup_extension_validation_accepts_expected_suffix() {
    let path = normalize_backup_path(PathBuf::from("demo.kwikpastebak")).unwrap();
    assert_eq!(path, PathBuf::from("demo.kwikpastebak"));
}

#[test]
fn backup_extension_validation_appends_missing_suffix() {
    let path = normalize_backup_path(PathBuf::from("demo")).unwrap();
    assert_eq!(path, PathBuf::from("demo.kwikpastebak"));
}

#[test]
fn backup_extension_validation_rejects_other_suffix() {
    assert!(normalize_backup_path(PathBuf::from("demo.zip")).is_err());
}

#[test]
fn encrypted_payload_header_round_trips() {
    let encrypted = encrypt_payload(b"hello".to_vec(), "password-123").unwrap();
    let mut bytes = Vec::new();
    write_header(&mut bytes, &encrypted.header).unwrap();
    bytes.extend_from_slice(&encrypted.ciphertext);

    let mut cursor = Cursor::new(bytes);

    assert_eq!(
        inspect_backup_reader(&mut cursor).unwrap(),
        BackupContainerMode::Encrypted
    );
}

#[test]
fn oversized_kdf_parameters_are_rejected_before_deriving() {
    let encrypted = encrypt_payload(b"hello".to_vec(), PASSWORD).unwrap();
    let kdf = encrypted.header.kdf.as_ref().unwrap();
    let cipher = encrypted.header.cipher.as_ref().unwrap();
    assert_eq!(
        decrypt_payload(encrypted.ciphertext.clone(), PASSWORD, kdf, cipher).unwrap(),
        b"hello"
    );

    for (memory_kib, time_cost, parallelism) in [
        (u32::MAX, ARGON2_TIME_COST, ARGON2_PARALLELISM),
        (ARGON2_MEMORY_KIB, u32::MAX, ARGON2_PARALLELISM),
        (ARGON2_MEMORY_KIB, ARGON2_TIME_COST, 0x00ff_ffff),
    ] {
        let crafted = KdfHeader {
            algorithm: kdf.algorithm.clone(),
            memory_kib,
            time_cost,
            parallelism,
            salt: kdf.salt.clone(),
        };
        assert!(decrypt_payload(encrypted.ciphertext.clone(), PASSWORD, &crafted, cipher).is_err());
    }
}

/// 用 `build` 写出一个 zip，返回字节。
fn zip_bytes(build: impl FnOnce(&mut zip::ZipWriter<Cursor<Vec<u8>>>)) -> Vec<u8> {
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    build(&mut writer);
    writer.finish().unwrap().into_inner()
}

#[test]
fn payload_extraction_keeps_files_and_rejects_links_and_escapes() {
    use std::io::Write as _;

    let options = zip::write::SimpleFileOptions::default();
    let payload = zip_bytes(|writer| {
        writer.add_directory("resources/", options).unwrap();
        writer.start_file("db/clipboard.db", options).unwrap();
        writer.write_all(b"db").unwrap();
    });
    let dir = extract_payload_zip(Cursor::new(&payload)).unwrap();
    assert_eq!(
        std::fs::read(dir.path().join("db").join("clipboard.db")).unwrap(),
        b"db"
    );
    assert!(dir.path().join("resources").is_dir());

    let link = zip_bytes(|writer| {
        writer
            .add_symlink("config/settings.json", "/etc/passwd", options)
            .unwrap();
    });
    assert!(extract_payload_zip(Cursor::new(&link)).is_err());

    let escape = zip_bytes(|writer| {
        writer.start_file("../outside.txt", options).unwrap();
        writer.write_all(b"x").unwrap();
    });
    assert!(extract_payload_zip(Cursor::new(&escape)).is_err());
}

#[test]
fn inspect_backup_reader_recognizes_plain_zip() {
    let mut cursor = Cursor::new(b"PK\x03\x04demo".to_vec());

    assert_eq!(
        inspect_backup_reader(&mut cursor).unwrap(),
        BackupContainerMode::Plain
    );
}

#[test]
fn inspect_backup_reader_recognizes_encrypted_container() {
    let encrypted = encrypt_payload(b"hello".to_vec(), "password-123").unwrap();
    let mut bytes = Vec::new();
    write_header(&mut bytes, &encrypted.header).unwrap();
    bytes.extend_from_slice(&encrypted.ciphertext);
    let mut cursor = Cursor::new(bytes);

    assert_eq!(
        inspect_backup_reader(&mut cursor).unwrap(),
        BackupContainerMode::Encrypted
    );
}

#[test]
fn payload_zip_contains_only_backup_whitelist() {
    let temp = tempdir().unwrap();
    let root = temp.path();
    let resources = root.join("resources");
    fs::create_dir_all(resources.join("clipboard-images/origin")).unwrap();
    fs::write(root.join("clipboard.db"), b"db").unwrap();
    fs::write(root.join("settings.json"), b"{}").unwrap();
    fs::write(root.join("window-state.json"), b"skip").unwrap();
    fs::write(root.join("settings.json.bak"), b"skip").unwrap();
    fs::write(resources.join("clipboard-images/origin/demo.png"), b"image").unwrap();
    // 单图文件记录的缩略图是缓存，不进备份。
    fs::create_dir_all(resources.join("clipboard-images/file-thumbnails/ab")).unwrap();
    fs::write(
        resources.join("clipboard-images/file-thumbnails/ab/ab12.png"),
        b"cache",
    )
    .unwrap();

    let payload = root.join("payload.zip");
    let target = root.join("backup.kwikpastebak");
    let manifest = BackupManifest {
        format_version: FORMAT_VERSION,
        app_name: "KwikPaste".to_owned(),
        app_version: "0.0.0".to_owned(),
        exported_at: Utc::now(),
        platform: "macos".to_owned(),
        encryption: ManifestEncryption::None,
        item_count: 1,
        text_count: 1,
        image_count: 0,
        files_count: 0,
        resource_bytes: 5,
        scope: None,
    };
    let source_paths = BackupSourcePaths {
        db_path: root.join("clipboard.db"),
        resources_dir: resources,
        settings_path: root.join("settings.json"),
    };

    write_payload_zip(&source_paths, &payload, &manifest, &target).unwrap();

    let file = File::open(payload).unwrap();
    let mut archive = ZipArchive::new(file).unwrap();
    let mut names = Vec::new();
    for index in 0..archive.len() {
        let file = archive.by_index(index).unwrap();
        names.push(file.name().to_owned());
    }
    names.sort();

    assert_eq!(
        names,
        vec![
            "config/settings.json",
            "db/clipboard.db",
            "manifest.json",
            "resources/clipboard-images/origin/demo.png",
        ]
    );
}

#[tokio::test]
async fn merge_preserves_is_sensitive_flag() {
    use crate::db::items::{content_hash, insert_item};
    use crate::db::models::{ClipboardItem, Platform};
    use crate::db::test_support::memory_pool;

    // The backup pool stands in for a real backup database, which is a
    // snapshot of the live clipboard.db and therefore already carries the
    // `is_sensitive` flag written by the production insert path.
    let backup = memory_pool().await;
    let current = memory_pool().await;

    let ts = DateTime::from_timestamp(1_700_000_000, 0).unwrap();
    let content = "AKIAIOSFODNN7EXAMPLE".to_owned();
    let item = ClipboardItem {
        id: "secret-1".to_owned(),
        kind: ClipboardKind::Text,
        sub_kind: None,
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
        is_sensitive: true,
        platform: Platform::Macos,
        note: None,
        created_at: ts,
        updated_at: ts,
        has_original: false,
        origin_device_id: None,
        source_app_name: None,
        source_app_icon_file: None,
    };
    insert_item(&backup, &item).await.unwrap();

    let outcome = merge_history(&current, &backup).await.unwrap();
    assert_eq!(
        outcome.imported_items, 1,
        "sensitive item should be imported"
    );

    // Regression: the merge SELECT/INSERT used to omit `is_sensitive`, so
    // every imported row silently fell back to the column DEFAULT (0) and
    // secrets were rendered in plaintext. The flag must survive the merge.
    let imported: bool =
        sqlx::query_scalar("SELECT is_sensitive FROM clipboard_items WHERE id = ?")
            .bind("secret-1")
            .fetch_one(&current)
            .await
            .unwrap();
    assert!(
        imported,
        "merged sensitive item must keep is_sensitive = true"
    );
}

// ---- 端到端：两个 core 实例之间导出、导入 ----

#[tokio::test]
async fn merge_preserves_last_use_and_falls_back_for_old_backups() {
    let now = Utc::now();
    let old = now - chrono::Duration::days(30);
    let recent = now.to_rfc3339();
    for (last_used, missing_column) in [
        (Some(recent.as_str()), false),
        (None, false),
        (Some("invalid"), false),
        (None, true),
    ] {
        let current = crate::db::test_support::memory_pool().await;
        let backup = crate::db::test_support::memory_pool().await;
        sqlx::query(
            "INSERT INTO clipboard_items \
             (id, kind, content, content_hash, platform, created_at, updated_at, last_used_at) \
             VALUES ('reused', 'text', 'old text', 'hash', 'windows', ?, ?, ?)",
        )
        .bind(old)
        .bind(old)
        .bind(last_used)
        .execute(&backup)
        .await
        .unwrap();
        if missing_column {
            sqlx::query("DROP INDEX idx_clipboard_items_pinned_last_used")
                .execute(&backup)
                .await
                .unwrap();
            sqlx::query("ALTER TABLE clipboard_items DROP COLUMN last_used_at")
                .execute(&backup)
                .await
                .unwrap();
        }

        assert_eq!(
            merge_history(&current, &backup)
                .await
                .unwrap()
                .imported_items,
            1
        );
        let (updated, used): (DateTime<Utc>, DateTime<Utc>) = sqlx::query_as(
            "SELECT updated_at, last_used_at FROM clipboard_items WHERE id = 'reused'",
        )
        .fetch_one(&current)
        .await
        .unwrap();
        let is_recent = last_used == Some(recent.as_str());
        assert_eq!(updated, old);
        assert_eq!(used, if is_recent { now } else { old });
        let plan = crate::db::retention::AgePlan {
            fallback_cutoff: Some(now - chrono::Duration::days(7)),
            ..Default::default()
        };
        crate::db::retention::delete_expired(&mut current.acquire().await.unwrap(), &plan)
            .await
            .unwrap();
        let remaining: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM clipboard_items")
            .fetch_one(&current)
            .await
            .unwrap();
        assert_eq!(remaining, i64::from(is_recent));
    }
}

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

fn image(width: u32) -> MemoryState {
    MemoryState {
        png: Some(sample_png(width, 6)),
        ..MemoryState::default()
    }
}

fn contents(core: &Core) -> Vec<String> {
    let mut contents: Vec<_> = block_on(core.list_items(ClipboardItemQuery {
        limit: 100,
        ..ClipboardItemQuery::default()
    }))
    .unwrap()
    .list
    .into_iter()
    .filter(|view| view.item.kind == ClipboardKind::Text)
    .map(|view| {
        block_on(core.find_item(&view.item.id))
            .unwrap()
            .unwrap()
            .content
    })
    .collect();
    contents.sort();
    contents
}

/// 带同步序号的记录条数与计数器：只有本机复制的记录有序号。
fn sync_state(core: &Core) -> (i64, i64) {
    block_on(core.hop({
        let core = core.clone();
        async move {
            let pool = core.0.db.pool().await;
            let local: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM clipboard_items WHERE sync_seq IS NOT NULL",
            )
            .fetch_one(&pool)
            .await
            .unwrap();
            Ok((local, crate::db::sync::sync_counter(&pool).await?))
        }
    }))
    .unwrap()
}

fn input(path: &Path, password: Option<&str>) -> ImportHistoryBackupInput {
    ImportHistoryBackupInput {
        path: path.to_path_buf(),
        password: password.map(str::to_owned),
        import_settings: true,
    }
}

/// 源实例：一条收藏的文本、一张图片、一个分组，深色主题。
struct Source {
    fixture: Fixture,
    core: Core,
    image_id: String,
    image_file: String,
}

fn source() -> Source {
    let fixture = Fixture::new();
    let core = fixture.start();
    block_on(core.update_settings(json!({"appearance": {"theme": "dark"}}))).unwrap();
    let favorite = store(&core, text("shared text"));
    block_on(core.toggle_favorite(&favorite)).unwrap();
    store(&core, text("only in the backup"));
    let image_id = store(&core, image(10));
    let group = block_on(core.create_group(ClipboardGroupInput {
        name: "Work".to_owned(),
        icon: "folder".to_owned(),
        is_hidden: false,
    }))
    .unwrap();
    block_on(core.set_item_group(&favorite, &group.id)).unwrap();
    let image_file = block_on(core.find_item(&image_id))
        .unwrap()
        .unwrap()
        .content;

    Source {
        fixture,
        core,
        image_id,
        image_file,
    }
}

fn export(source: &Source, mode: BackupExportMode, password: Option<&str>) -> PathBuf {
    let target = source.fixture.root().join("exports").join("history");
    let result = block_on(source.core.export_history_backup(
        target,
        ExportHistoryBackupOptions {
            mode,
            password: password.map(str::to_owned),
            scope: BackupScope::default(),
        },
    ))
    .unwrap();

    assert_eq!(result.mode, mode);
    assert_eq!(result.item_count, 3);
    assert_eq!(result.text_count, 2);
    assert_eq!(result.image_count, 1);
    assert!(result.resource_bytes > 0);
    let path = PathBuf::from(result.path);
    assert_eq!(path.extension().unwrap(), BACKUP_EXTENSION);
    assert_eq!(fs::metadata(&path).unwrap().len(), result.total_bytes);
    path
}

#[test]
fn backup_preserves_original_content_for_merge_and_overwrite() {
    let source = source();
    let id = store(&source.core, text("shared text"));
    let before = block_on(source.core.find_item(&id)).unwrap().unwrap();
    block_on(source.core.update_text_content(&id, "edited text".into())).unwrap();
    let backup = export(&source, BackupExportMode::Plain, None);

    for strategy in [BackupImportStrategy::Merge, BackupImportStrategy::Overwrite] {
        let fixture = Fixture::new();
        let core = fixture.start();
        block_on(core.import_history_backup(input(&backup, None), strategy)).unwrap();
        assert_eq!(
            block_on(core.find_item(&id)).unwrap().unwrap().content,
            "edited text"
        );
        assert!(block_on(core.find_item(&id)).unwrap().unwrap().has_original);
        block_on(core.restore_original_content(&id)).unwrap();
        assert_eq!(
            serde_json::to_value(block_on(core.find_item(&id)).unwrap().unwrap()).unwrap(),
            serde_json::to_value(&before).unwrap()
        );
        assert!(!block_on(core.find_item(&id)).unwrap().unwrap().has_original);
        block_on(core.shutdown()).unwrap();
    }
    block_on(source.core.shutdown()).unwrap();
}

#[test]
fn merge_keeps_edited_versions_with_the_same_id() {
    let source = source();
    let id = store(&source.core, text("shared text"));
    block_on(source.core.update_text_content(&id, "version B".into())).unwrap();
    let backup = export(&source, BackupExportMode::Plain, None);
    block_on(source.core.update_text_content(&id, "version C".into())).unwrap();

    let result = block_on(
        source
            .core
            .import_history_backup(input(&backup, None), BackupImportStrategy::Merge),
    )
    .unwrap();
    assert_eq!((result.imported_items, result.skipped_items), (1, 2));
    assert_eq!(
        contents(&source.core),
        ["only in the backup", "version B", "version C"]
    );
    assert_eq!(
        block_on(source.core.find_item(&id))
            .unwrap()
            .unwrap()
            .content,
        "version C"
    );
    let again = block_on(
        source
            .core
            .import_history_backup(input(&backup, None), BackupImportStrategy::Merge),
    )
    .unwrap();
    assert_eq!((again.imported_items, again.skipped_items), (0, 3));
    block_on(source.core.shutdown()).unwrap();
}

#[tokio::test]
async fn merge_does_not_count_ignored_rows_as_imported() {
    let current = crate::db::test_support::memory_pool().await;
    let backup = crate::db::test_support::memory_pool().await;
    sqlx::query("PRAGMA ignore_check_constraints = ON")
        .execute(&backup)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO clipboard_items (id, kind, content, content_hash, platform, created_at, updated_at) \
         VALUES ('empty', 'files', '', 'hash', 'windows', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
    ).execute(&backup).await.unwrap();

    let result = merge_history(&current, &backup).await.unwrap();
    assert_eq!((result.imported_items, result.skipped_items), (0, 1));
}

#[test]
fn plain_export_merges_into_another_instance() {
    let source = source();
    let backup = export(&source, BackupExportMode::Plain, None);
    assert_eq!(
        inspect_backup_file(&backup).unwrap(),
        BackupContainerMode::Plain
    );

    let fixture = Fixture::new();
    let core = fixture.start();
    store(&core, text("shared text"));
    store(&core, text("only here"));
    fixture.take_events();

    let result =
        block_on(core.import_history_backup(input(&backup, None), BackupImportStrategy::Merge))
            .unwrap();

    assert_eq!(result.strategy, BackupImportStrategy::Merge);
    assert_eq!(result.imported_items, 2);
    assert_eq!(result.skipped_items, 1);
    assert!(result.imported_resources >= 1);
    assert_eq!(
        contents(&core),
        ["only here", "only in the backup", "shared text"]
    );
    assert!(core
        .image_origin_path(&source.image_file)
        .unwrap()
        .is_file());
    assert_eq!(block_on(core.list_groups()).unwrap().len(), 1);
    assert_eq!(core.settings().appearance.theme, Theme::Dark);
    // 合并进来的记录不是这台电脑复制的，没有同步序号，不会补齐给已配对设备。
    assert_eq!(sync_state(&core), (2, 2));

    let events = fixture.take_events();
    assert!(events
        .iter()
        .any(|event| matches!(event, CoreEvent::ClipboardReloaded)));
    assert!(events.iter().any(|event| matches!(
        event,
        CoreEvent::SettingsUpdated { delta, .. } if delta.touches("appearance.theme")
    )));
    block_on(core.shutdown()).unwrap();
    block_on(source.core.shutdown()).unwrap();
}

#[test]
fn full_export_excludes_cleanup_and_skips_missing_images() {
    use std::future::{poll_fn, Future};
    use std::task::Poll;

    let source = source();
    fs::remove_file(source.core.image_origin_path(&source.image_file).unwrap()).unwrap();
    source.fixture.runtime.handle().block_on(async {
        let core = &source.core.0;
        let guard = core.cleanup.exclusive().await;
        let mut export = std::pin::pin!(export_history_backup(
            core,
            source.fixture.root().join("locked-export"),
            ExportHistoryBackupOptions {
                mode: BackupExportMode::Plain,
                password: None,
                scope: BackupScope::default(),
            },
        ));
        poll_fn(|cx| {
            assert!(export.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
        let mut cleanup = std::pin::pin!(core.cleanup.exclusive());
        poll_fn(|cx| {
            assert!(cleanup.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
        drop(guard);
        poll_fn(|cx| {
            assert!(export.as_mut().poll(cx).is_pending());
            assert!(
                cleanup.as_mut().poll(cx).is_pending(),
                "export keeps cleanup excluded"
            );
            Poll::Ready(())
        })
        .await;
        assert_eq!(export.await.unwrap().image_count, 1);
        drop(cleanup.await);
    });
    block_on(source.core.shutdown()).unwrap();
}

#[test]
fn encrypted_export_overwrites_another_instance() {
    let source = source();
    let backup = export(&source, BackupExportMode::Encrypted, Some(PASSWORD));
    assert_eq!(
        inspect_backup_file(&backup).unwrap(),
        BackupContainerMode::Encrypted
    );
    let bytes = fs::read(&backup).unwrap();
    assert!(bytes.starts_with(MAGIC));

    let fixture = Fixture::new();
    let core = fixture.start();
    store(&core, text("replaced by the backup"));
    let local_image = store(&core, image(20));
    let local_image_file = block_on(core.find_item(&local_image))
        .unwrap()
        .unwrap()
        .content;

    let missing =
        block_on(core.import_history_backup(input(&backup, None), BackupImportStrategy::Overwrite))
            .unwrap_err();
    assert_eq!(missing.to_string(), "请输入备份密码");
    let wrong = block_on(core.import_history_backup(
        input(&backup, Some("wrong password")),
        BackupImportStrategy::Overwrite,
    ))
    .unwrap_err();
    assert_eq!(wrong.to_string(), "备份密码不正确或文件已损坏");
    assert_eq!(contents(&core), ["replaced by the backup"]);

    fixture.take_events();
    let result = block_on(core.import_history_backup(
        input(&backup, Some(PASSWORD)),
        BackupImportStrategy::Overwrite,
    ))
    .unwrap();

    assert_eq!(result.strategy, BackupImportStrategy::Overwrite);
    assert_eq!(contents(&core), ["only in the backup", "shared text"]);
    assert!(core
        .image_origin_path(&source.image_file)
        .unwrap()
        .is_file());
    assert!(!core.image_origin_path(&local_image_file).unwrap().exists());
    assert_eq!(core.settings().appearance.theme, Theme::Dark);
    assert!(!core.0.watcher_pause.is_paused());
    let db_dir = fixture.paths.db_dir().unwrap();
    let mut db_files: Vec<_> = fs::read_dir(&db_dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.contains(".importing") || name.contains(".previous"))
        .collect();
    db_files.sort();
    assert!(db_files.is_empty(), "leftover files: {db_files:?}");

    let events = fixture.take_events();
    assert!(events
        .iter()
        .any(|event| matches!(event, CoreEvent::ClipboardReloaded)));
    assert!(events.iter().any(|event| matches!(
        event,
        CoreEvent::SettingsUpdated { delta, .. } if delta.touches("shortcuts")
    )));

    // 覆盖进来的记录都没有同步序号；计数器取两边较大的，不回退。
    assert_eq!(sync_state(&source.core).1, 3);
    assert_eq!(sync_state(&core), (0, 3));

    // 换上的库照常可写。
    store(&core, text("after overwrite"));
    assert_eq!(contents(&core).len(), 3);
    assert_eq!(sync_state(&core), (1, 4));
    block_on(core.shutdown()).unwrap();
    block_on(source.core.shutdown()).unwrap();
}

#[test]
fn merge_can_leave_settings_alone() {
    let source = source();
    let backup = export(&source, BackupExportMode::Plain, None);

    let fixture = Fixture::new();
    let core = fixture.start();
    fixture.take_events();
    let result = block_on(core.import_history_backup(
        ImportHistoryBackupInput {
            import_settings: false,
            ..input(&backup, None)
        },
        BackupImportStrategy::Merge,
    ))
    .unwrap();

    assert_eq!(result.imported_items, 3);
    assert!(!result.imported_settings);
    assert_eq!(core.settings().appearance.theme, Theme::default());
    assert!(!fixture
        .take_events()
        .iter()
        .any(|event| matches!(event, CoreEvent::SettingsUpdated { .. })));
    block_on(core.shutdown()).unwrap();
    block_on(source.core.shutdown()).unwrap();
}

#[test]
fn overwrite_can_leave_settings_alone() {
    let source = source();
    let backup = export(&source, BackupExportMode::Plain, None);

    let fixture = Fixture::new();
    let core = fixture.start();
    store(&core, text("replaced"));
    let result = block_on(core.import_history_backup(
        ImportHistoryBackupInput {
            import_settings: false,
            ..input(&backup, None)
        },
        BackupImportStrategy::Overwrite,
    ))
    .unwrap();

    assert!(!result.imported_settings);
    assert_eq!(contents(&core), ["only in the backup", "shared text"]);
    assert_eq!(core.settings().appearance.theme, Theme::default());
    block_on(core.shutdown()).unwrap();
    block_on(source.core.shutdown()).unwrap();
}

fn export_scoped(
    source: &Source,
    name: &str,
    scope: BackupScope,
) -> Result<ExportHistoryBackupResult> {
    block_on(source.core.export_history_backup(
        source.fixture.root().join("exports").join(name),
        ExportHistoryBackupOptions {
            mode: BackupExportMode::Plain,
            password: None,
            scope,
        },
    ))
}

fn favorites() -> BackupScope {
    BackupScope {
        favorites_only: true,
        ..BackupScope::default()
    }
}

/// 只导出收藏：别的记录、它们的分组和设置都不进备份，删掉的内容也不留在库文件或全文索引里。
#[test]
fn favorites_backup_carries_only_favorites() {
    let source = source();
    block_on(source.core.toggle_favorite(&source.image_id)).unwrap();
    store(&source.core, text("ungrouped secret note"));

    let result = export_scoped(&source, "favorites", favorites()).unwrap();
    assert_eq!(result.item_count, 2);
    assert_eq!((result.text_count, result.image_count), (1, 1));

    let backup = PathBuf::from(&result.path);
    let extracted = extract_payload_zip(File::open(&backup).unwrap()).unwrap();
    let manifest = read_json_file(&extracted.path().join(MANIFEST_FILENAME)).unwrap();
    assert_eq!(manifest["scope"]["favoritesOnly"], json!(true));
    assert_eq!(
        read_json_file(&extracted.path().join("config").join("settings.json")).unwrap(),
        json!({})
    );
    let db_bytes = fs::read(extracted.path().join("db").join(DB_FILENAME)).unwrap();
    for removed in ["only in the backup", "ungrouped secret note"] {
        assert!(
            !db_bytes
                .windows(removed.len())
                .any(|window| window == removed.as_bytes()),
            "{removed:?} left in the partial backup database"
        );
    }
    let rt = crate::runtime::CoreRuntime::new().unwrap();
    let fts_hits: i64 = rt.handle().block_on(async {
        let pool = open_backup_db(&extracted.path().join("db").join(DB_FILENAME))
            .await
            .unwrap();
        let hits = sqlx::query_scalar(
            "SELECT COUNT(*) FROM clipboard_items_fts WHERE clipboard_items_fts MATCH 'secret'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        pool.close().await;
        hits
    });
    assert_eq!(fts_hits, 0);

    let fixture = Fixture::new();
    let core = fixture.start();
    store(&core, text("local only"));
    let imported =
        block_on(core.import_history_backup(input(&backup, None), BackupImportStrategy::Merge))
            .unwrap();

    assert_eq!(imported.imported_items, 2);
    assert!(!imported.imported_settings);
    assert_eq!(contents(&core), ["local only", "shared text"]);
    assert!(core
        .image_origin_path(&source.image_file)
        .unwrap()
        .is_file());
    assert_eq!(block_on(core.list_groups()).unwrap().len(), 1);
    assert_eq!(core.settings().appearance.theme, Theme::default());
    block_on(core.shutdown()).unwrap();
    block_on(source.core.shutdown()).unwrap();
}

#[test]
fn group_backup_keeps_only_the_chosen_groups() {
    let source = source();
    let group_id = block_on(source.core.list_groups()).unwrap()[0].id.clone();

    let only_group = export_scoped(
        &source,
        "work",
        BackupScope {
            group_ids: Some(vec![group_id]),
            ..BackupScope::default()
        },
    )
    .unwrap();
    assert_eq!(only_group.item_count, 1);
    let ungrouped = export_scoped(
        &source,
        "ungrouped",
        BackupScope {
            group_ids: Some(Vec::new()),
            include_ungrouped: true,
            ..BackupScope::default()
        },
    )
    .unwrap();
    assert_eq!((ungrouped.text_count, ungrouped.image_count), (1, 1));

    let nothing = export_scoped(
        &source,
        "nothing",
        BackupScope {
            group_ids: Some(Vec::new()),
            ..BackupScope::default()
        },
    )
    .unwrap_err();
    assert_eq!(nothing.to_string(), "请选择至少一个分组或未分组");
    block_on(source.core.shutdown()).unwrap();
}

#[test]
fn favorites_backup_without_favorites_is_refused() {
    let fixture = Fixture::new();
    let core = fixture.start();
    store(&core, text("not a favorite"));
    let target = fixture.root().join("empty.kwikpastebak");

    let err = block_on(core.export_history_backup(
        target.clone(),
        ExportHistoryBackupOptions {
            mode: BackupExportMode::Plain,
            password: None,
            scope: favorites(),
        },
    ))
    .unwrap_err();

    assert_eq!(err.to_string(), "所选范围没有可导出的记录");
    assert!(!target.exists());
    block_on(core.shutdown()).unwrap();
}

/// 部分备份不能覆盖导入：那会把范围外的记录全丢掉。
#[test]
fn partial_backup_refuses_overwrite() {
    let source = source();
    let backup = PathBuf::from(
        export_scoped(&source, "favorites", favorites())
            .unwrap()
            .path,
    );

    let fixture = Fixture::new();
    let core = fixture.start();
    store(&core, text("must survive"));
    let err =
        block_on(core.import_history_backup(input(&backup, None), BackupImportStrategy::Overwrite))
            .unwrap_err();

    assert_eq!(err.to_string(), "这个备份只包含部分记录，请用合并导入");
    assert_eq!(contents(&core), ["must survive"]);
    block_on(core.shutdown()).unwrap();
    block_on(source.core.shutdown()).unwrap();
}

#[test]
fn export_validates_password_and_target() {
    let fixture = Fixture::new();
    let core = fixture.start();
    let target = fixture.root().join("history.kwikpastebak");
    let export = |mode, password: Option<&str>, target: PathBuf| {
        block_on(core.export_history_backup(
            target,
            ExportHistoryBackupOptions {
                mode,
                password: password.map(str::to_owned),
                scope: BackupScope::default(),
            },
        ))
        .map(|_| ())
        .unwrap_err()
        .to_string()
    };

    assert_eq!(
        export(BackupExportMode::Encrypted, None, target.clone()),
        "请输入备份密码"
    );
    assert_eq!(
        export(BackupExportMode::Encrypted, Some("short"), target.clone()),
        "备份密码至少需要 8 个字符"
    );
    assert_eq!(
        export(BackupExportMode::Plain, Some(PASSWORD), target.clone()),
        "明文备份不应包含密码"
    );
    assert_eq!(
        export(
            BackupExportMode::Plain,
            None,
            fixture.root().join("history.zip")
        ),
        "备份文件后缀必须是 .kwikpastebak"
    );
    assert!(!target.exists());
    block_on(core.shutdown()).unwrap();
}

#[test]
fn backup_paths_are_recognized_from_args() {
    let args = [
        "KwikPaste.exe".to_owned(),
        "--flag".to_owned(),
        "D:/Backups/History.KWIKPASTEBAK".to_owned(),
    ];

    assert_eq!(
        backup_path_from_args(&args),
        Some(PathBuf::from("D:/Backups/History.KWIKPASTEBAK"))
    );
    assert_eq!(backup_path_from_args(&args[..2]), None);
    assert!(inspect_backup_file(Path::new("history.zip")).is_err());
}

#[test]
fn passwords_never_show_up_in_debug_output() {
    let options = ExportHistoryBackupOptions {
        mode: BackupExportMode::Encrypted,
        password: Some(PASSWORD.to_owned()),
        scope: BackupScope::default(),
    };
    let input = input(Path::new("a.kwikpastebak"), Some(PASSWORD));

    assert!(!format!("{options:?}").contains(PASSWORD));
    assert!(!format!("{input:?}").contains(PASSWORD));
}

// ---- 覆盖导入的兼容与失败回滚 ----

/// 按 1.x 的布局手工打一个明文备份：`prepare` 可以在打包前改动备份库。
fn craft_backup<F>(dir: &Path, prepare: F) -> PathBuf
where
    F: FnOnce(&SqlitePool) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + '_>>,
{
    let rt = crate::runtime::CoreRuntime::new().unwrap();
    let source = dir.join("source");
    let db_path = source.join("clipboard.db");
    fs::create_dir_all(source.join("resources")).unwrap();
    fs::write(
        source.join("settings.json"),
        serde_json::to_string(&json!({"appearance": {"theme": "light"}})).unwrap(),
    )
    .unwrap();

    rt.handle().block_on(async {
        let options = SqliteConnectOptions::new()
            .filename(&db_path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        crate::db::MIGRATOR.run(&pool).await.unwrap();
        sqlx::query(
            "INSERT INTO clipboard_items (id, kind, content, content_hash, platform, created_at, updated_at) \
             VALUES ('crafted', 'text', 'crafted text', 'crafted-hash', 'windows', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
        )
        .execute(&pool)
        .await
        .unwrap();
        prepare(&pool).await;
        checkpoint_database(&pool).await.unwrap();
        pool.close().await;
    });

    let manifest = BackupManifest {
        format_version: FORMAT_VERSION,
        app_name: "KwikPaste".to_owned(),
        app_version: "1.4.0".to_owned(),
        exported_at: Utc::now(),
        platform: "macos".to_owned(),
        encryption: ManifestEncryption::None,
        item_count: 1,
        text_count: 1,
        image_count: 0,
        files_count: 0,
        resource_bytes: 0,
        scope: None,
    };
    let target = dir.join("crafted.kwikpastebak");
    let payload = dir.join("payload.zip");
    write_payload_zip(
        &BackupSourcePaths {
            db_path,
            resources_dir: source.join("resources"),
            settings_path: source.join("settings.json"),
        },
        &payload,
        &manifest,
        &target,
    )
    .unwrap();
    write_container(&target, &payload, BackupExportMode::Plain, None).unwrap();
    target
}

/// macOS 导出的库记的是 LF 校验和，Windows 记的是 CRLF：覆盖导入时换成本平台的值，否则迁移直接失败。
#[test]
fn overwrite_adopts_migration_checksums_of_the_other_platform() {
    let temp = tempdir().unwrap();
    let backup = craft_backup(temp.path(), |pool| {
        Box::pin(async move {
            for embedded in crate::db::MIGRATOR.iter() {
                let &(version, lf, crlf) = crate::db::init::PUBLISHED
                    .iter()
                    .find(|(version, _, _)| *version == embedded.version)
                    .unwrap();
                let embedded_hex: String = embedded
                    .checksum
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect();
                let other = if embedded_hex == lf { crlf } else { lf };
                let bytes: Vec<u8> = (0..other.len())
                    .step_by(2)
                    .map(|index| u8::from_str_radix(&other[index..index + 2], 16).unwrap())
                    .collect();
                sqlx::query("UPDATE _sqlx_migrations SET checksum = ? WHERE version = ?")
                    .bind(bytes)
                    .bind(version)
                    .execute(pool)
                    .await
                    .unwrap();
            }
        })
    });

    let fixture = Fixture::new();
    let core = fixture.start();
    store(&core, text("local"));

    block_on(core.import_history_backup(input(&backup, None), BackupImportStrategy::Overwrite))
        .unwrap();

    assert_eq!(contents(&core), ["crafted text"]);
    assert_eq!(core.settings().appearance.theme, Theme::Light);
    block_on(core.shutdown()).unwrap();
}

/// 备份来自更新的版本（多了本版本不认识的迁移）：导入失败，当前数据与连接池都原样可用。
#[test]
fn overwrite_failure_keeps_the_live_database() {
    let temp = tempdir().unwrap();
    let backup = craft_backup(temp.path(), |pool| {
        Box::pin(async move {
            sqlx::query(
                "INSERT INTO _sqlx_migrations (version, description, success, checksum, execution_time) \
                 VALUES (9999, 'from the future', 1, x'00', 0)",
            )
            .execute(pool)
            .await
            .unwrap();
        })
    });

    let fixture = Fixture::new();
    let core = fixture.start();
    block_on(core.update_settings(json!({"appearance": {"theme": "dark"}}))).unwrap();
    store(&core, text("still here"));

    let err =
        block_on(core.import_history_backup(input(&backup, None), BackupImportStrategy::Overwrite))
            .unwrap_err();

    assert!(err.to_string().contains("不兼容"), "{err}");
    assert_eq!(contents(&core), ["still here"]);
    assert_eq!(core.settings().appearance.theme, Theme::Dark);
    assert!(!core.0.watcher_pause.is_paused());
    let staged = sibling(
        &crate::db::db_path(&fixture.paths).unwrap(),
        STAGED_DB_SUFFIX,
    );
    assert!(!staged.exists());
    store(&core, text("still writable"));
    assert_eq!(contents(&core).len(), 2);
    block_on(core.shutdown()).unwrap();
}

#[test]
fn overwrite_with_unreadable_settings_changes_nothing() {
    let temp = tempdir().unwrap();
    let backup = craft_backup(temp.path(), |_| Box::pin(async {}));
    // 把设置换成读不懂的内容后重新打包。
    let extracted = extract_payload_zip(File::open(&backup).unwrap()).unwrap();
    fs::write(
        extracted.path().join("config").join("settings.json"),
        b"[not settings",
    )
    .unwrap();
    let broken = temp.path().join("broken.kwikpastebak");
    let payload = temp.path().join("broken.zip");
    write_payload_zip(
        &BackupSourcePaths {
            db_path: extracted.path().join("db").join("clipboard.db"),
            resources_dir: extracted.path().join("resources"),
            settings_path: extracted.path().join("config").join("settings.json"),
        },
        &payload,
        &BackupManifest {
            format_version: FORMAT_VERSION,
            app_name: "KwikPaste".to_owned(),
            app_version: "1.4.0".to_owned(),
            exported_at: Utc::now(),
            platform: "windows".to_owned(),
            encryption: ManifestEncryption::None,
            item_count: 1,
            text_count: 1,
            image_count: 0,
            files_count: 0,
            resource_bytes: 0,
            scope: None,
        },
        &broken,
    )
    .unwrap();
    write_container(&broken, &payload, BackupExportMode::Plain, None).unwrap();

    let fixture = Fixture::new();
    let core = fixture.start();
    store(&core, text("untouched"));

    assert!(block_on(
        core.import_history_backup(input(&broken, None), BackupImportStrategy::Overwrite,)
    )
    .is_err());
    assert_eq!(contents(&core), ["untouched"]);
    block_on(core.shutdown()).unwrap();
}

#[test]
fn overwrite_settings_write_failure_restores_database_and_resources() {
    let source = source();
    let backup = export(&source, BackupExportMode::Plain, None);
    let fixture = Fixture::new();
    let core = fixture.start();
    let image_id = store(&core, image(20));
    let image_file = block_on(core.find_item(&image_id))
        .unwrap()
        .unwrap()
        .content;
    let image_path = core.image_origin_path(&image_file).unwrap();
    let before = fs::read(&image_path).unwrap();
    store(&core, text("local history"));
    let sync_before = sync_state(&core);
    let tmp = fixture
        .paths
        .config_dir()
        .unwrap()
        .join("settings.json.tmp");
    fs::create_dir(&tmp).unwrap();

    assert!(block_on(
        core.import_history_backup(input(&backup, None), BackupImportStrategy::Overwrite,)
    )
    .is_err());
    assert_eq!(contents(&core), ["local history"]);
    assert_eq!(fs::read(&image_path).unwrap(), before);
    assert!(!core.image_origin_path(&source.image_file).unwrap().exists());
    assert_eq!(sync_state(&core), sync_before);
    assert_eq!(core.settings().appearance.theme, Theme::Auto);
    assert!(!core.0.watcher_pause.is_paused());
    fs::remove_dir(&tmp).unwrap();
    store(&core, text("still writable"));
    block_on(core.shutdown()).unwrap();
    block_on(source.core.shutdown()).unwrap();
}

// ---- 1.4.0 写出的备份 ----

/// `tests/fixtures/backup/` 里的两份备份由 1.4.0 的导出代码在 Windows 上写出（明文与加密，
/// 密码 `compat-password-1`）：5 条记录，覆盖收藏 + 备注 + 分组 + 来源应用、置顶的敏感内容、
/// 链接、图片（原图与缩略图）、文件，另有文件类型图标与深色主题。
fn v1_fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("backup")
        .join(name)
}

const V1_PASSWORD: &str = "compat-password-1";
const V1_IMAGE: &str = "b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2.png";

fn assert_v1_history(fixture: &Fixture, core: &Core) {
    let text = block_on(core.find_item("v1-text")).unwrap().unwrap();
    assert_eq!(text.content, "v1 favorite text in a group");
    assert!(text.is_favorite);
    assert_eq!(text.note.as_deref(), Some("a v1 note"));
    assert_eq!(text.group_id.as_deref(), Some("group-work"));
    assert_eq!(
        text.source_app_id.as_deref(),
        Some("C:\\Program Files\\Editor\\editor.exe")
    );
    let secret = block_on(core.find_item("v1-secret")).unwrap().unwrap();
    assert!(secret.is_sensitive && secret.is_pinned);
    let url = block_on(core.find_item("v1-url")).unwrap().unwrap();
    assert_eq!(url.sub_kind, Some(crate::db::models::ClipboardSubKind::Url));
    let image = block_on(core.find_item("v1-image")).unwrap().unwrap();
    assert_eq!(image.content, V1_IMAGE);
    assert!(core.image_origin_path(V1_IMAGE).unwrap().is_file());
    assert!(block_on(core.find_item("v1-files")).unwrap().is_some());

    let listed = block_on(core.list_items(ClipboardItemQuery::default())).unwrap();
    assert_eq!(listed.total, 5);
    let with_app = listed
        .list
        .iter()
        .find(|view| view.item.id == "v1-text")
        .unwrap();
    assert_eq!(with_app.item.source_app_name.as_deref(), Some("Editor"));
    assert!(with_app
        .source_app_icon_path
        .as_deref()
        .is_some_and(|path| Path::new(path).is_file()));
    assert_eq!(block_on(core.list_groups()).unwrap()[0].name, "Work");
    assert!(fixture
        .paths
        .resources_dir()
        .unwrap()
        .join("file-icons")
        .read_dir()
        .unwrap()
        .next()
        .is_some());
    assert_eq!(core.settings().appearance.theme, Theme::Dark);
}

#[test]
fn v1_plain_backup_merges() {
    let backup = v1_fixture("v1-plain.kwikpastebak");
    assert_eq!(
        inspect_backup_file(&backup).unwrap(),
        BackupContainerMode::Plain
    );
    let fixture = Fixture::new();
    let core = fixture.start();

    let result =
        block_on(core.import_history_backup(input(&backup, None), BackupImportStrategy::Merge))
            .unwrap();

    assert_eq!((result.imported_items, result.skipped_items), (5, 0));
    assert_v1_history(&fixture, &core);
    block_on(core.shutdown()).unwrap();
}

/// 加密备份覆盖导入。夹具在 Windows 上生成，迁移账里是 CRLF 校验和：macOS 上跑这条测试
/// 同时验证了跨平台校验和的换算。
#[test]
fn v1_encrypted_backup_overwrites() {
    let backup = v1_fixture("v1-encrypted.kwikpastebak");
    assert_eq!(
        inspect_backup_file(&backup).unwrap(),
        BackupContainerMode::Encrypted
    );
    let fixture = Fixture::new();
    let core = fixture.start();
    store(&core, text("replaced by the 1.x backup"));

    block_on(core.import_history_backup(
        input(&backup, Some(V1_PASSWORD)),
        BackupImportStrategy::Overwrite,
    ))
    .unwrap();

    assert_v1_history(&fixture, &core);
    assert_eq!(
        block_on(core.list_items(ClipboardItemQuery::default()))
            .unwrap()
            .total,
        5
    );
    block_on(core.shutdown()).unwrap();
}

/// 给 1.x 读的样本：在 `KWIKPASTE_V2_BACKUP_DIR` 写出 2.0 导出的明文与加密备份（密码见 `PASSWORD`），
/// 再用 1.4.0 的导入代码去读，确认 2.0 的备份 1.x 能导入。2.0 新增迁移或设置字段后要重跑。
#[test]
#[ignore = "manual 1.x compatibility check"]
fn export_samples_for_v1() {
    let Some(dir) = std::env::var_os("KWIKPASTE_V2_BACKUP_DIR") else {
        return;
    };
    let dir = PathBuf::from(dir);
    let source = source();
    for (name, mode, password) in [
        ("v2-plain", BackupExportMode::Plain, None),
        ("v2-encrypted", BackupExportMode::Encrypted, Some(PASSWORD)),
    ] {
        block_on(source.core.export_history_backup(
            dir.join(name),
            ExportHistoryBackupOptions {
                mode,
                password: password.map(str::to_owned),
                scope: BackupScope::default(),
            },
        ))
        .unwrap();
    }
    block_on(source.core.shutdown()).unwrap();
}

/// 完整备份原样带上识别文字；部分备份删掉它，正文和索引里都找不到。
#[test]
fn partial_backup_strips_ocr_text_while_full_backup_keeps_it() {
    let source = source();
    block_on(source.core.toggle_favorite(&source.image_id)).unwrap();
    block_on(source.core.hop({
        let core = source.core.clone();
        async move {
            sqlx::query("INSERT INTO image_texts(item_id,status,text,lines,attempts,created_at,updated_at) SELECT id,'done','private OCR sentinel',?,1,created_at,created_at FROM clipboard_items WHERE kind='image'")
                .bind(r#"[{"text":"private OCR sentinel","quad":[[0,0],[1,0],[1,1],[0,1]]}]"#)
                .execute(&core.0.db.pool().await).await.unwrap();
            Ok(())
        }
    })).unwrap();
    let full = export(&source, BackupExportMode::Plain, None);
    let partial = PathBuf::from(
        export_scoped(&source, "favorites", favorites())
            .unwrap()
            .path,
    );
    for (path, expected) in [(full, 1), (partial, 0)] {
        let root = extract_payload_zip(File::open(&path).unwrap()).unwrap();
        source.fixture.runtime.handle().block_on(async {
            let pool = SqlitePoolOptions::new()
                .max_connections(1)
                .connect_with(
                    SqliteConnectOptions::new()
                        .filename(root.path().join(DB_ARCHIVE_DIR).join(DB_FILENAME)),
                )
                .await
                .unwrap();
            let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM image_texts")
                .fetch_one(&pool)
                .await
                .unwrap();
            let hits: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM image_texts_fts WHERE image_texts_fts MATCH 'sentinel'",
            )
            .fetch_one(&pool)
            .await
            .unwrap();
            assert_eq!((rows, hits), (expected, expected));
            let lines: Option<String> = sqlx::query_scalar("SELECT lines FROM image_texts")
                .fetch_optional(&pool)
                .await
                .unwrap();
            if expected == 1 {
                let lines: Vec<crate::ocr::ImageTextLine> =
                    serde_json::from_str(&lines.unwrap()).unwrap();
                assert_eq!(lines[0].text, "private OCR sentinel");
                assert_eq!(lines[0].quad, [[0., 0.], [1., 0.], [1., 1.], [0., 1.]]);
            } else {
                assert!(lines.is_none());
            }
            pool.close().await;
        });
    }
    block_on(source.core.shutdown()).unwrap();
}

#[test]
fn overwrite_old_backup_adds_null_ocr_layout() {
    let temp = tempdir().unwrap();
    let backup = craft_backup(temp.path(), |pool| {
        Box::pin(async move {
            sqlx::query("ALTER TABLE image_texts DROP COLUMN lines")
                .execute(pool)
                .await
                .unwrap();
            sqlx::query("DELETE FROM _sqlx_migrations WHERE version = 10")
                .execute(pool)
                .await
                .unwrap();
            sqlx::query("UPDATE clipboard_items SET kind = 'image', content = 'legacy.png' WHERE id = 'crafted'")
            .execute(pool).await.unwrap();
            sqlx::query("INSERT INTO image_texts(item_id,status,text,attempts,created_at,updated_at) SELECT id,'done','legacy text',1,created_at,updated_at FROM clipboard_items")
            .execute(pool).await.unwrap();
        })
    });
    let fixture = Fixture::new();
    let core = fixture.start();
    block_on(core.import_history_backup(input(&backup, None), BackupImportStrategy::Overwrite))
        .unwrap();
    fixture.runtime.handle().block_on(async {
        let row: (String, Option<String>) =
            sqlx::query_as("SELECT text, lines FROM image_texts WHERE item_id = 'crafted'")
                .fetch_one(&core.0.db.pool().await)
                .await
                .unwrap();
        assert_eq!(row, ("legacy text".into(), None));
    });
    block_on(core.shutdown()).unwrap();
}
