//! 远程同步不经网络的部分：设置叶子的应用与差异、元数据后写者赢（含删除与找回、先到的元数据）、迁移。

use std::sync::Mutex;

use chrono::Utc;
use serde_json::{json, Value};
use sqlx::migrate::Migrator;
use sqlx::sqlite::SqlitePoolOptions;

use super::crypto::{pack, GroupKeys, Hlc, KEY_LEN};
use super::entries::{self, Incoming, ItemEntry, MetaEntry};
use super::settings::{self, SettingsEntry};
use crate::db::models::{ClipboardItem, ClipboardKind, Platform};
use crate::db::remote::{self as state, RemoteStateRow};
use crate::root::Core;
use crate::settings::{TextSizeLevel, Theme};
use crate::sync::engine::{EchoFilter, ReceiveOptions};
use crate::sync::protocol::WireItem;
use crate::testing::{block_on, Fixture};

fn keys() -> GroupKeys {
    GroupKeys::new([5; KEY_LEN])
}

fn hlc(value: u64) -> String {
    Hlc(value).wire("peer")
}

/// 在 core runtime 上跑一段测试代码。
fn on_core<F, Fut>(test: F)
where
    F: FnOnce(Core) -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<()>> + Send + 'static,
{
    let fixture = Fixture::new();
    let core = fixture.start();
    let future = test(core.clone());
    block_on(core.hop(async move { future.await.map_err(crate::error::AppError::Other) })).unwrap();
    block_on(core.shutdown()).unwrap();
}

#[test]
fn incoming_settings_apply_without_echo() {
    on_core(|core| async move {
        let keys = keys();
        let pool = core.0.db.pool().await;
        // 已经和组里同步过一轮。
        for (entry_id, entry) in settings::changed_leaves(&pool, &keys, &core.settings()).await? {
            settings::record(&pool, &entry_id, &entry.path, &entry.value, &hlc(1)).await?;
        }
        assert!(settings::changed_leaves(&pool, &keys, &core.settings())
            .await?
            .is_empty());

        let theme_id = keys.settings_id("all", "/appearance/theme");
        let theme = |value: &str| SettingsEntry {
            path: "/appearance/theme".into(),
            scope: "all".into(),
            value: json!(value),
        };
        assert!(settings::apply_incoming(&core.0, &theme_id, &hlc(2), theme("dark")).await?);
        assert_eq!(core.settings().appearance.theme, Theme::Dark);
        assert!(settings::changed_leaves(&pool, &keys, &core.settings())
            .await?
            .is_empty());

        // 旧的、范围和本机划分不一致的、只属于本机的都不应用。
        assert!(!settings::apply_incoming(&core.0, &theme_id, &hlc(1), theme("light")).await?);
        let shortcut = SettingsEntry {
            path: "/shortcuts/openClipboard".into(),
            scope: "all".into(),
            value: json!("Alt+V"),
        };
        let shortcut_id = keys.settings_id("all", &shortcut.path);
        assert!(!settings::apply_incoming(&core.0, &shortcut_id, &hlc(3), shortcut).await?);
        let auto_start = SettingsEntry {
            path: "/general/autoStart".into(),
            scope: "all".into(),
            value: json!(true),
        };
        let auto_start_id = keys.settings_id("all", &auto_start.path);
        assert!(!settings::apply_incoming(&core.0, &auto_start_id, &hlc(3), auto_start).await?);
        let settings_now = core.settings();
        assert_eq!(settings_now.appearance.theme, Theme::Dark);
        assert_eq!(settings_now.shortcuts.open_clipboard, "Alt+C");
        assert!(!settings_now.general.auto_start);

        // 本机改了的叶子才上传。
        core.update_settings(json!({"appearance": {"textSize": "large"}}))
            .await?;
        assert_eq!(core.settings().appearance.text_size, TextSizeLevel::Large);
        let changed = settings::changed_leaves(&pool, &keys, &core.settings()).await?;
        assert_eq!(changed.len(), 1);
        assert_eq!(changed[0].1.path, "/appearance/textSize");
        assert_eq!(changed[0].1.value, json!("large"));
        Ok(())
    });
}

fn item_plain(item_key: &str, text: &str) -> Vec<u8> {
    let entry = ItemEntry {
        item: WireItem {
            kind: ClipboardKind::Text,
            sub_kind: None,
            content: text.to_owned(),
            search_text: None,
            summary: None,
            width: None,
            height: None,
        },
        item_key: item_key.to_owned(),
        captured_at: Utc::now() - chrono::Duration::days(1),
        image: None,
    };
    pack(&serde_json::to_vec(&entry).unwrap(), &[]).unwrap()
}

async fn receive(core: &Core, keys: &GroupKeys, item_key: &str, at: u64) -> anyhow::Result<bool> {
    let hlc = hlc(at);
    let incoming = Incoming {
        hlc: &hlc,
        from: "peer",
        live: false,
    };
    let options = ReceiveOptions {
        text: true,
        image: true,
        write_clipboard: false,
        max_image_bytes: u64::MAX,
        allow_jpeg: false,
        allow_webp: true,
        captured_at: None,
    };
    entries::receive_item(
        &core.0,
        keys,
        &Mutex::new(EchoFilter::default()),
        &incoming,
        &item_plain(item_key, "remote hello"),
        options,
        Platform::Macos,
        true,
        true,
    )
    .await
}

async fn find(core: &Core, item_key: &str) -> anyhow::Result<Option<ClipboardItem>> {
    let pool = core.0.db.pool().await;
    let Some((id, _)) = state::find_by_item_key(&pool, item_key).await?.pop() else {
        return Ok(None);
    };
    Ok(crate::db::items::find_item_by_id(&pool, &id).await?)
}

#[test]
fn metadata_is_last_writer_wins_with_delete_and_resurrect() {
    on_core(|core| async move {
        let keys = keys();
        let key = "text:origin-hash";
        let meta = |field: &str, value: Value| MetaEntry {
            item_key: key.to_owned(),
            field: field.to_owned(),
            value,
        };

        // 记录还没到：元数据先记下，记录到了补上。
        assert!(
            !entries::apply_meta(&core.0, &keys, &hlc(20), meta("favorite", json!(true))).await?
        );
        assert!(!entries::apply_meta(&core.0, &keys, &hlc(20), meta("note", json!("备注"))).await?);
        assert!(receive(&core, &keys, key, 10).await?);
        let item = find(&core, key).await?.expect("received record");
        assert!(item.is_favorite);
        assert_eq!(item.note.as_deref(), Some("备注"));
        assert_eq!(item.sync_key.as_deref(), Some(key));
        assert_eq!(item.origin_device_id.as_deref(), Some("peer"));
        assert!(item.updated_at < Utc::now() - chrono::Duration::hours(23));

        // 后写者赢：旧的不生效，新的生效。
        assert!(
            !entries::apply_meta(&core.0, &keys, &hlc(15), meta("favorite", json!(false))).await?
        );
        assert!(find(&core, key).await?.unwrap().is_favorite);
        assert!(
            entries::apply_meta(&core.0, &keys, &hlc(30), meta("favorite", json!(false))).await?
        );
        assert!(!find(&core, key).await?.unwrap().is_favorite);

        // 删除比记录新：删掉，旧的记录条目重放找不回来，有人再次复制（更新的记录条目）就重新插入。
        assert!(entries::apply_meta(&core.0, &keys, &hlc(40), meta("deleted", json!(true))).await?);
        assert!(find(&core, key).await?.is_none());
        assert!(!receive(&core, &keys, key, 35).await?);
        assert!(find(&core, key).await?.is_none());
        receive(&core, &keys, key, 50).await?;
        assert!(find(&core, key).await?.is_some());

        // 比记录最后一次的 HLC 旧的删除不生效。
        assert!(
            !entries::apply_meta(&core.0, &keys, &hlc(45), meta("deleted", json!(true))).await?
        );
        assert!(find(&core, key).await?.is_some());
        Ok(())
    });
}

/// 0012 加在已有库上：旧记录原样保留，`sync_key` 为空，状态表可用。
#[tokio::test]
async fn remote_sync_migration_keeps_existing_items() {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    let old = Migrator {
        migrations: std::borrow::Cow::Owned(
            crate::db::MIGRATOR
                .iter()
                .filter(|migration| migration.version <= 11)
                .cloned()
                .collect(),
        ),
        ..Migrator::DEFAULT
    };
    old.run(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO clipboard_items (id, kind, content, content_hash, search_text, platform, created_at, updated_at) \
         VALUES ('old', 'text', 'hello', 'hash', 'hello', 'windows', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
    )
    .execute(&pool)
    .await
    .unwrap();

    crate::db::MIGRATOR.run(&pool).await.unwrap();

    let item = crate::db::items::find_item_by_id(&pool, "old")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(item.content, "hello");
    assert!(item.sync_key.is_none());
    assert_eq!(
        state::item_key(&pool, "old").await.unwrap().as_deref(),
        Some("text:hash")
    );
    let row = RemoteStateRow {
        entry_id: "e".into(),
        kind: "meta".into(),
        item_key: Some("text:hash".into()),
        field: "favorite".into(),
        value: Some("true".into()),
        hlc: hlc(1),
    };
    state::put(&pool, &row).await.unwrap();
    assert_eq!(state::get(&pool, "e").await.unwrap(), Some(row));
}
