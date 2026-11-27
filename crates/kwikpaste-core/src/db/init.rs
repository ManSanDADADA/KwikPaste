use anyhow::Context;
use sqlx::migrate::Migrator;
use sqlx::sqlite::{
    SqliteAutoVacuum, SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous,
};
use sqlx::ConnectOptions;
use sqlx::SqlitePool;

use crate::db::db_path;
use crate::error::Result;
use crate::paths::CorePaths;

/// 与 1.x 共用的那一本迁移账（sqlx 默认的 `_sqlx_migrations`）。
///
/// `migrations/` 里 0001–0005 是 src-tauri 已发布迁移的原样副本，git blob 必须一致
/// （`scripts/ci/check-migration-parity.mjs`）。校验和按检出的字节算：Windows 检出 CRLF、
/// macOS 检出 LF，与各平台已发布版本写进用户库的值相同，所以 `*.sql` 不能加任何换行规则。
/// 新迁移从 0006 开始，只加在这里。用了 0006 起的迁移的库，1.4.0 打不开、也不能覆盖导入它的备份
/// （F8 接受：不承诺能从 2.0 退回 1.x）。
pub static MIGRATOR: Migrator = sqlx::migrate!("./migrations");

/// 已发布（含 2.0 新增）迁移的 sha384：LF 是 macOS 检出的字节，CRLF 是 Windows 检出的字节。
/// 用户库的 `_sqlx_migrations` 里存的就是这两种之一，对不上的话存量用户启动即失败；
/// 跨平台覆盖导入备份时按这张表换算（[`adopt_published_checksums`]）。新增迁移要在这里补上两种值。
pub(crate) const PUBLISHED: [(i64, &str, &str); 12] = [
    (
        1,
        "bfa656aa68eed66f8dab5bacb9efa4bafeb11713fd417239e05f7b4c5757330fae5eafeeb138771c6d18785670c44b05",
        "ff50e7101ee04305eaa30b28054f33ee98204cae357373ff8c9f7b3850017f7f036ec3e113a0222c3c4da7367fd48820",
    ),
    (
        2,
        "11fbad1c9935f98df0f59912e665d60c383c4e3493758f1d777f800b7e2ffd7f30db1c15e75f6b65c24c73f9d5f1c8dd",
        "f7fa0713ccdbff6d6ee41782cceeef4d22fd4825ee732a7c0bbc04dabb1d410af910e88d3f602e8055e510767251724d",
    ),
    (
        3,
        "dad289792fafd02b4fc95888648d05e215147288e7f53f65e59cbeef4a8d9ccf88578cd85640a0f7c719d4bfff959e97",
        "3a0141f5726c8046cf8c60cadf4098fb030be414fa0f899f96f505a2db7f1f8d64c94d25def8fc9383cc6c710b7fd86e",
    ),
    (
        4,
        "28ffe65f6fb7569acbf9e1707efb294345c1b1e237cc383d4f37e62fce3e6f4cbbd09c30f3c5ab3f9f7b0d1a1f2d6b35",
        "6144496b34a23e28f56622baa83d4c8262a6cfac21cd670478bf2d2644086a5ed38b89d16124de4dc8c4c6947f999ba9",
    ),
    (
        5,
        "6fbf1887b4c8d108c6fb6fa3c237a1486970f4056bb19b1490fc6c7225cd8473808cb1388c5833ea52f3377ba135e7ad",
        "0a444601708a31b745efa22d573e93ca11ed53a6e637d28027a17403040a612f8317ac7352849933340a223066aacbd9",
    ),
    (
        6,
        "907b97f834b3e60f65f1b80987bad3fd5f8d258bf2a5b7cfff1fd38ed5fb79b4c8913162ba30f51f8e3ba6d80fac0a31",
        "cfe2f21fbf619b6c763709a1f882668e9891a0dd875cd5e92b1fcf8e4589df08cbd87530a79cf80f47f7b954f5765586",
    ),
    (
        7,
        "0766fc81a02997e2e9aa5d5b63be0e2d3371a5c08377c5f7e8c67277361706dc91399874fc65df55de3c3529d8c614ed",
        "1362c23e2e06f89ea2e0265702cd12c7e69722fc3890562108970452a205a25a15d27318778fb10aae41a5e9ba293fbe",
    ),
    (8, "e9a4905e3f6285520158e89fd59054ad5beb1549c5cd2ee9bd099e7efc64c6cbd7d9a30500171243a6a4091f26ddd958", "32ae6ae06370708b427782341654b48bdefa38d232320db81581b3a4d9122ac34e7c0b4396544c7561f71f95dd790920"),
    (9, "d6eaf11e575215e774a7d9f253d72825ef510acbd306fcd987d015a632acf7b3e9b0b09fd7a1eb797ca59372c8ecb013", "d8bd6edaf64504cb6b9888f66f9cd3b54d4bdf5b47416d54d91b0e2238b35efc880575b1c2b5b9b2ce81af7e89ef204c"),
    (10, "63a530790e242038e85438c22231ae5a4c060933c74b8805cf570e9a04545cc5ecaf627eb8e4ce413770dd4cfb66cd2c", "492c7ed37d773eafb11c932dab38f91a7bbc92b93c642b89e25881cd9d28dfc2d9e60be19475ce5c26cf772d45fbe163"),
    (11, "8ad7abd5b78288ce841f574c94235849375ea9cf73698c26928b9ca124a29f1ce42172dffbbab4f803eab186c3c466d0", "5488a40f3acd7c879f5f1de680a70d4a6b40461acc3a9a53dd3bc4d385501fef61c07b700cc1771888af60e814b2581b"),
    (12, "7a0c92b56c80f948e4b4378c7150d33ca58a2331370e1c9b3004d5112956f36912b194b2d86e7a06b9936107e3218855", "200bba841c2b5794fc24c41704be347529af64fc42a64f06946f1da63b4b594ea624084899c8cd42f0425454ab48a5e2"),
];

/// 把另一个平台写下的已发布迁移校验和改成本平台的值，返回改了几条。
///
/// 迁移账按检出字节算校验和：macOS 建的库记的是 LF 版本，Windows 记的是 CRLF 版本。
/// 跨平台覆盖导入备份、或本地构建（检出换行与 CI 不同）用过的库，原样打开会因校验和不符而迁移失败，
/// 所以启动和导入时都先换算。这里只改那些恰好等于
/// 「已发布迁移的另一平台版本」的行，其它不认识的值保持原样，仍由迁移检查报错。
pub(crate) async fn adopt_published_checksums(pool: &SqlitePool) -> Result<u64> {
    let has_ledger: Option<i64> = sqlx::query_scalar(
        "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = '_sqlx_migrations'",
    )
    .fetch_optional(pool)
    .await
    .context("failed to look up the migration ledger")?;
    if has_ledger.is_none() {
        return Ok(0);
    }

    let rows: Vec<(i64, Vec<u8>)> =
        sqlx::query_as("SELECT version, checksum FROM _sqlx_migrations")
            .fetch_all(pool)
            .await
            .context("failed to read the migration ledger")?;

    let mut adopted = 0;
    for (version, stored) in rows {
        let Some(embedded) = MIGRATOR
            .iter()
            .find(|migration| migration.version == version)
        else {
            continue;
        };
        let Some((_, lf, crlf)) = PUBLISHED
            .iter()
            .find(|(published, _, _)| *published == version)
        else {
            continue;
        };
        let stored_hex = hex(&stored);
        if stored[..] == embedded.checksum[..] || (stored_hex != *lf && stored_hex != *crlf) {
            continue;
        }

        sqlx::query("UPDATE _sqlx_migrations SET checksum = ? WHERE version = ?")
            .bind(&embedded.checksum[..])
            .bind(version)
            .execute(pool)
            .await
            .context("failed to update the migration ledger")?;
        adopted += 1;
    }

    Ok(adopted)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// 打开（必要时创建）`<data_root>/db/clipboard.db` 并跑迁移。必须在 tokio 上下文里调用。
///
/// `max_connections` 是连接池上限，见 [`crate::CoreOptions::db_max_connections`]。
pub async fn init(paths: &CorePaths, max_connections: u32) -> Result<SqlitePool> {
    let path = db_path(paths)?;

    // 新库建表前就开启增量 auto_vacuum，自动清理删行后可以随时收缩文件；
    // 老库要等偏好页「清理缓存」整库 VACUUM 一次才会切过去，在此之前这条 pragma 不生效也不报错。
    let options = SqliteConnectOptions::new()
        .filename(&path)
        .create_if_missing(true)
        .auto_vacuum(SqliteAutoVacuum::Incremental)
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Normal)
        .foreign_keys(true)
        .disable_statement_logging();

    let pool = SqlitePoolOptions::new()
        .max_connections(max_connections.max(1))
        .connect_with(options)
        .await
        .with_context(|| format!("failed to open sqlite database at {path:?}"))?;

    let adopted = adopt_published_checksums(&pool).await?;
    if adopted > 0 {
        log::info!("adopted {adopted} migration checksum(s) written with the other line endings");
    }
    MIGRATOR
        .run(&pool)
        .await
        .context("failed to run sqlite migrations")?;

    log::info!("sqlite pool ready at {path:?}");
    Ok(pool)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use super::*;
    use crate::env::AppEnv;

    #[test]
    fn published_migrations_keep_released_checksums() {
        let embedded: Vec<_> = MIGRATOR.iter().collect();

        for (version, lf, crlf) in PUBLISHED {
            let migration = embedded
                .iter()
                .find(|migration| migration.version == version)
                .unwrap_or_else(|| panic!("published migration {version} is missing"));
            let checksum = hex(&migration.checksum);

            assert!(
                checksum == lf || checksum == crlf,
                "migration {version} checksum {checksum} matches neither released variant"
            );
        }

        let last_published = PUBLISHED[PUBLISHED.len() - 1].0;
        for migration in &embedded {
            assert!(
                migration.version > last_published
                    || PUBLISHED
                        .iter()
                        .any(|(version, _, _)| *version == migration.version),
                "unexpected migration {} below the published range",
                migration.version
            );
        }
    }

    /// 另一平台建的库原样打开会迁移失败；换算后通过，不认识的校验和保持原样。
    #[tokio::test]
    async fn foreign_platform_checksums_are_adopted() {
        let pool = crate::db::test_support::memory_pool().await;
        for (version, lf, crlf) in PUBLISHED {
            let embedded = MIGRATOR
                .iter()
                .find(|migration| migration.version == version)
                .unwrap();
            let other = if hex(&embedded.checksum) == lf {
                crlf
            } else {
                lf
            };
            let bytes: Vec<u8> = (0..other.len())
                .step_by(2)
                .map(|index| u8::from_str_radix(&other[index..index + 2], 16).unwrap())
                .collect();
            sqlx::query("UPDATE _sqlx_migrations SET checksum = ? WHERE version = ?")
                .bind(bytes)
                .bind(version)
                .execute(&pool)
                .await
                .unwrap();
        }
        assert!(MIGRATOR.run(&pool).await.is_err());

        assert_eq!(
            adopt_published_checksums(&pool).await.unwrap(),
            PUBLISHED.len() as u64
        );
        MIGRATOR.run(&pool).await.unwrap();
        assert_eq!(adopt_published_checksums(&pool).await.unwrap(), 0);

        sqlx::query("UPDATE _sqlx_migrations SET checksum = x'00' WHERE version = 1")
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(adopt_published_checksums(&pool).await.unwrap(), 0);
        assert!(MIGRATOR.run(&pool).await.is_err());
    }

    #[tokio::test]
    async fn original_content_migration_preserves_existing_items() {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        let old = Migrator {
            migrations: std::borrow::Cow::Owned(
                MIGRATOR
                    .iter()
                    .filter(|migration| migration.version <= 8)
                    .cloned()
                    .collect(),
            ),
            ..Migrator::DEFAULT
        };
        old.run(&pool).await.unwrap();
        sqlx::query(
            "INSERT INTO clipboard_items (id, kind, content, content_hash, search_text, platform, created_at, updated_at) \
             VALUES ('old', 'text', 'originalneedle', 'hash', 'originalneedle', 'windows', \
                     '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
        )
        .execute(&pool).await.unwrap();
        let before = sqlx::query_as::<_, crate::db::models::ClipboardItem>(
            "SELECT * FROM clipboard_items WHERE id = 'old'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();

        MIGRATOR.run(&pool).await.unwrap();
        let after = crate::db::items::find_item_by_id(&pool, "old")
            .await
            .unwrap()
            .unwrap();
        assert!(!after.has_original);
        assert_eq!(
            serde_json::to_value(before).unwrap(),
            serde_json::to_value(after).unwrap()
        );
        let hits: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM clipboard_items_fts WHERE clipboard_items_fts MATCH 'originalneedle'",
        ).fetch_one(&pool).await.unwrap();
        assert_eq!(hits, 1);
    }

    #[tokio::test]
    async fn init_creates_database_under_db_dir() {
        let root = std::env::temp_dir().join(format!("kwikpaste-db-init-{}", uuid::Uuid::new_v4()));
        let local = root.join("local");
        let paths = CorePaths::new(AppEnv::Prod, local.clone(), local.join("logs"), None);

        let pool = init(&paths, 3).await.unwrap();
        let applied: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM _sqlx_migrations")
            .fetch_one(&pool)
            .await
            .unwrap();
        pool.close().await;

        let expected: PathBuf = local.join("prod").join("db").join("clipboard.db");
        assert!(expected.is_file());
        assert_eq!(applied, MIGRATOR.iter().count() as i64);

        fs::remove_dir_all(&root).ok();
    }

    /// 库里记的是另一种换行的已发布校验和（本地构建写的）时，启动照常完成。
    #[tokio::test]
    async fn init_adopts_checksums_of_the_other_line_endings() {
        let root =
            std::env::temp_dir().join(format!("kwikpaste-db-adopt-{}", uuid::Uuid::new_v4()));
        let local = root.join("local");
        let paths = CorePaths::new(AppEnv::Prod, local.clone(), local.join("logs"), None);
        let (version, lf, crlf) = PUBLISHED[PUBLISHED.len() - 1];
        let embedded = MIGRATOR
            .iter()
            .find(|migration| migration.version == version)
            .unwrap();
        let other = if hex(&embedded.checksum) == lf {
            crlf
        } else {
            lf
        };

        let pool = init(&paths, 1).await.unwrap();
        sqlx::query("UPDATE _sqlx_migrations SET checksum = unhex(?) WHERE version = ?")
            .bind(other)
            .bind(version)
            .execute(&pool)
            .await
            .unwrap();
        pool.close().await;

        init(&paths, 1).await.unwrap().close().await;
        fs::remove_dir_all(&root).ok();
    }
}
