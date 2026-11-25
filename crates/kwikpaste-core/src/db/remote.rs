//! 远程同步的本机状态（迁移 0012）。
//!
//! `remote_sync_state` 按条目 id 记后写者赢的最新值与 HLC：元数据与设置条目、删除墓碑、
//! 还没收到记录的元数据，以及每条记录最后一次的 HLC。收到的记录把来源设备的 itemKey 存进
//! `clipboard_items.sync_key`，本机采集的记录这一列为空，itemKey 就是自己的 `kind:content_hash`。

use anyhow::Context;
use chrono::{DateTime, Utc};
use sqlx::SqlitePool;

use crate::error::Result;

/// `remote_sync_state` 的一行。`kind` 是 `item` / `meta` / `settings`：记录行的 `field` 为空串、
/// 没有 `value`；设置行没有 `item_key`，`field` 是 JSON 指针。`value` 是 JSON 文本。
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct RemoteStateRow {
    pub entry_id: String,
    pub kind: String,
    pub item_key: Option<String>,
    pub field: String,
    pub value: Option<String>,
    pub hlc: String,
}

macro_rules! select_row {
    () => {
        "SELECT entry_id, kind, item_key, field, value, hlc FROM remote_sync_state"
    };
}

pub async fn get(pool: &SqlitePool, entry_id: &str) -> Result<Option<RemoteStateRow>> {
    Ok(
        sqlx::query_as(concat!(select_row!(), " WHERE entry_id = ?"))
            .bind(entry_id)
            .fetch_optional(pool)
            .await
            .context("failed to read remote sync state")?,
    )
}

/// 写入或替换一行。
pub async fn put(pool: &SqlitePool, row: &RemoteStateRow) -> Result<()> {
    let now = Utc::now();
    sqlx::query(
        "INSERT INTO remote_sync_state (entry_id, kind, item_key, field, value, hlc, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?) \
         ON CONFLICT (entry_id) DO UPDATE SET kind = excluded.kind, item_key = excluded.item_key, \
         field = excluded.field, value = excluded.value, hlc = excluded.hlc, updated_at = excluded.updated_at",
    )
    .bind(&row.entry_id)
    .bind(&row.kind)
    .bind(row.item_key.as_deref())
    .bind(&row.field)
    .bind(row.value.as_deref())
    .bind(&row.hlc)
    .bind(now)
    .bind(now)
    .execute(pool)
    .await
    .context("failed to write remote sync state")?;
    Ok(())
}

pub async fn rows_of_kind(pool: &SqlitePool, kind: &str) -> Result<Vec<RemoteStateRow>> {
    Ok(sqlx::query_as(concat!(select_row!(), " WHERE kind = ?"))
        .bind(kind)
        .fetch_all(pool)
        .await
        .context("failed to read remote sync state")?)
}

/// 一条记录的全部元数据行（含删除墓碑）。
pub async fn meta_rows(pool: &SqlitePool, item_key: &str) -> Result<Vec<RemoteStateRow>> {
    Ok(sqlx::query_as(concat!(
        select_row!(),
        " WHERE item_key = ? AND kind = 'meta'"
    ))
    .bind(item_key)
    .fetch_all(pool)
    .await
    .context("failed to read remote sync metadata")?)
}

/// 离开同步组、覆盖导入备份后清空。
pub async fn clear(pool: &SqlitePool) -> Result<()> {
    sqlx::query("DELETE FROM remote_sync_state")
        .execute(pool)
        .await
        .context("failed to clear remote sync state")?;
    Ok(())
}

/// 见过的最大 HLC（线上格式按字符串比较即可：前 16 位是定长十六进制）。重启后时钟从这里接着走。
pub async fn max_hlc(pool: &SqlitePool) -> Result<Option<String>> {
    Ok(sqlx::query_scalar("SELECT MAX(hlc) FROM remote_sync_state")
        .fetch_one(pool)
        .await
        .context("failed to read the latest remote hlc")?)
}

/// 按 itemKey 找本机记录：收到的比 `sync_key`，本机采集的比 `kind:content_hash`。
/// 返回 `(id, updated_at)`。
pub async fn find_by_item_key(
    pool: &SqlitePool,
    item_key: &str,
) -> Result<Vec<(String, DateTime<Utc>)>> {
    let (kind, hash) = item_key.split_once(':').unwrap_or_default();
    Ok(sqlx::query_as(
        "SELECT id, updated_at FROM clipboard_items \
         WHERE sync_key = ? OR (sync_key IS NULL AND content_hash = ? AND kind = ?)",
    )
    .bind(item_key)
    .bind(hash)
    .bind(kind)
    .fetch_all(pool)
    .await
    .context("failed to find clipboard item by sync key")?)
}

/// 一条本机记录的 itemKey。
pub async fn item_key(pool: &SqlitePool, id: &str) -> Result<Option<String>> {
    Ok(sqlx::query_scalar(
        "SELECT COALESCE(sync_key, kind || ':' || content_hash) FROM clipboard_items WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
    .context("failed to read clipboard item sync key")?)
}

/// 收到的记录记下来源设备的 itemKey；与自己的 `kind:content_hash` 相同或已经记过时不动。
pub async fn set_sync_key(pool: &SqlitePool, id: &str, item_key: &str) -> Result<()> {
    sqlx::query(
        "UPDATE clipboard_items SET sync_key = ? \
         WHERE id = ? AND sync_key IS NULL AND kind || ':' || content_hash <> ?",
    )
    .bind(item_key)
    .bind(id)
    .bind(item_key)
    .execute(pool)
    .await
    .context("failed to store clipboard item sync key")?;
    Ok(())
}

/// 全部可以作为历史上传的记录（文本与图片、不是敏感内容），按最后使用时间从旧到新。
pub async fn history_item_ids(pool: &SqlitePool) -> Result<Vec<String>> {
    Ok(sqlx::query_scalar(
        "SELECT id FROM clipboard_items WHERE kind IN ('text', 'image') AND is_sensitive = 0 \
         ORDER BY updated_at ASC",
    )
    .fetch_all(pool)
    .await
    .context("failed to list history for remote sync")?)
}
