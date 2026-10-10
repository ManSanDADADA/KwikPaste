use anyhow::Context;
use blake3::Hasher;
use chrono::Utc;
use sqlx::{QueryBuilder, Sqlite, SqlitePool};

use crate::db::models::{
    ClipboardGroupFilter, ClipboardItem, ClipboardItemQuery, ClipboardItemRef, ClipboardItemSort,
    ClipboardKind,
};
use crate::error::Result;

const SELECT_ITEM: &str = "SELECT id, kind, sub_kind, group_id, source_app_id, content, \
     content_hash, search_text, summary, file_types, size, width, height, use_count, is_favorite, is_pinned, \
     is_sensitive, platform, note, created_at, updated_at, origin_device_id FROM clipboard_items";

/// 列表/单条刷新场景的精简 SELECT：text 类型条目的 `content` 与 `search_text` 一律置空，
/// 由前端用 `summary` 渲染。HTML/RTF/长纯文本可能很大（用户复制整段文档），
/// 整段过 IPC + 进 DOM 是这条链路最昂贵的一环；image/files 的 content 是
/// 文件名 / 路径列表，保留原值。预览/写回走 [`find_item_by_id`] 拿完整 content。
///
/// LEFT JOIN `clipboard_apps` 顺带把来源应用名 / 图标文件名带回，前端直接渲染，
/// 不再额外发 list_clipboard_apps + get_clipboard_app_icon_path 请求。
const LIST_SELECT_ITEM: &str = "SELECT clipboard_items.id, clipboard_items.kind, \
     clipboard_items.sub_kind, clipboard_items.group_id, clipboard_items.source_app_id, \
     CASE WHEN clipboard_items.kind = 'text' THEN '' ELSE clipboard_items.content END AS content, \
     clipboard_items.content_hash, \
     CASE WHEN clipboard_items.kind = 'text' THEN NULL ELSE clipboard_items.search_text END AS search_text, \
     clipboard_items.summary, clipboard_items.file_types, clipboard_items.size, \
     clipboard_items.width, clipboard_items.height, clipboard_items.use_count, \
     clipboard_items.is_favorite, clipboard_items.is_pinned, \
     clipboard_items.is_sensitive, \
     clipboard_items.platform, clipboard_items.note, \
     clipboard_items.created_at, clipboard_items.updated_at, clipboard_items.origin_device_id, \
     clipboard_apps.name AS source_app_name, \
     clipboard_apps.icon_file AS source_app_icon_file \
     FROM clipboard_items \
     LEFT JOIN clipboard_apps ON clipboard_apps.id = clipboard_items.source_app_id";

/// 入库去重的结果：`id` 为生效行的主键（命中时是已有行，未命中时是新插入行），
/// `deduplicated` 表示是否命中了已有内容（命中则只 `use_count + 1` 未插入新行）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpsertResult {
    pub id: String,
    pub deduplicated: bool,
}

/// 可手动排序的本地分区。Favorite 只包含未置顶收藏，置顶收藏始终由 Pinned 分区决定位置。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReorderSection {
    Pinned,
    Favorite,
}

/// 相对于全局分区顺序的插入位置。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReorderAnchor {
    Before(String),
    After(String),
}

/// 计算去重指纹：`blake3("<kind>:<content>")`。
/// 加 `kind` 前缀，避免 text 与 files 恰好同串内容被误判为重复。
/// text 直接哈希内容串即可；image/files 的 `content` 是落盘引用/路径，
/// 调用方持有原始字节时可改为对原始内容字节哈希后写入 `content_hash`。
pub fn content_hash(kind: ClipboardKind, content: &str) -> String {
    let mut hasher = Hasher::new();
    hasher.update(kind_tag(kind).as_bytes());
    hasher.update(b":");
    hasher.update(content.as_bytes());
    hasher.finalize().to_hex().to_string()
}

fn kind_tag(kind: ClipboardKind) -> &'static str {
    match kind {
        ClipboardKind::Text => "text",
        ClipboardKind::Image => "image",
        ClipboardKind::Files => "files",
    }
}

/// 入库主入口：按 `item.content_hash` 去重。
/// 命中已有记录 → 复用 [`increment_item_use_count`] 累加并刷新 `updated_at`，不插入新行；
/// 未命中 → 调用 [`insert_item`] 插入。返回生效行 id 与是否去重。
pub async fn upsert_item(pool: &SqlitePool, item: &ClipboardItem) -> Result<UpsertResult> {
    if let Some(existing) = find_item_by_content_hash(pool, &item.content_hash).await? {
        increment_item_use_count(pool, &existing).await?;
        return Ok(UpsertResult {
            id: existing,
            deduplicated: true,
        });
    }

    insert_item(pool, item).await?;
    Ok(UpsertResult {
        id: item.id.clone(),
        deduplicated: false,
    })
}

/// 按内容哈希只读取最新记录的 ID，避免去重时加载完整正文和搜索文本。
pub async fn find_item_by_content_hash(pool: &SqlitePool, hash: &str) -> Result<Option<String>> {
    let id = sqlx::query_scalar::<_, String>(
        "SELECT id FROM clipboard_items WHERE content_hash = ? ORDER BY created_at DESC LIMIT 1",
    )
    .bind(hash)
    .fetch_optional(pool)
    .await
    .context("failed to find clipboard item by content_hash")?;
    Ok(id)
}

/// 插入一条剪贴板记录（不做去重；去重请走 [`upsert_item`]）。最后使用时间取 `updated_at`。
pub async fn insert_item(pool: &SqlitePool, item: &ClipboardItem) -> Result<()> {
    sqlx::query(
        "INSERT INTO clipboard_items \
         (id, kind, sub_kind, group_id, source_app_id, content, content_hash, search_text, \
          summary, file_types, size, width, height, use_count, is_favorite, is_pinned, favorite_order, pin_order, is_sensitive, platform, note, \
          created_at, updated_at, last_used_at, origin_device_id) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?,
                 CASE WHEN ? THEN 1 + COALESCE((SELECT MAX(favorite_order) FROM clipboard_items), 0) END,
                 CASE WHEN ? THEN 1 + COALESCE((SELECT MAX(pin_order) FROM clipboard_items), 0) END,
                 ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(item.id.as_str())
    .bind(item.kind)
    .bind(item.sub_kind)
    .bind(item.group_id.as_deref())
    .bind(item.source_app_id.as_deref())
    .bind(item.content.as_str())
    .bind(item.content_hash.as_str())
    .bind(item.search_text.as_deref())
    .bind(item.summary.as_deref())
    .bind(item.file_types.as_deref())
    .bind(item.size)
    .bind(item.width)
    .bind(item.height)
    .bind(item.use_count)
    .bind(item.is_favorite)
    .bind(item.is_pinned)
    .bind(item.is_favorite)
    .bind(item.is_pinned)
    .bind(item.is_sensitive)
    .bind(item.platform)
    .bind(item.note.as_deref())
    .bind(item.created_at)
    .bind(item.updated_at)
    .bind(item.updated_at)
    .bind(item.origin_device_id.as_deref())
    .execute(pool)
    .await
    .context("failed to insert clipboard item")?;
    Ok(())
}

/// 仅返回项的轻量查询：生产路径走 [`query_items_page`]（顺带返回 total），
/// 本函数留给单元测试做断言。
#[cfg(test)]
pub async fn query_items(pool: &SqlitePool, q: &ClipboardItemQuery) -> Result<Vec<ClipboardItem>> {
    fetch_items(pool, q, KeywordFilter::from_keyword(q.keyword.as_deref())).await
}

/// 列表 + 总数一次返回，供命令层组装 [`ClipboardItemPage`]：
/// 一次 IPC 拿到「本页项 / 当前过滤下的总数 / 是否还有下一页」。
/// `keyword` 按字符长度分流：≥3 走 FTS5（trigram 分词），1–2 走 `LIKE '%kw%'`
/// （兜底短词；trigram 索引最短 3 字符，对 1–2 字符词永远 0 命中）。
pub async fn query_items_page(
    pool: &SqlitePool,
    q: &ClipboardItemQuery,
) -> Result<(Vec<ClipboardItem>, i64)> {
    let keyword = KeywordFilter::from_keyword(q.keyword.as_deref());
    let items = fetch_items(pool, q, keyword.clone()).await?;
    let total = fetch_items_count(pool, q, keyword).await?;
    Ok((items, total))
}

/// 按剪贴板窗口「全部」视图的顺序（置顶在前，其余按 `sort`）取第 `offset` 条（从 0 起）的 id，
/// 超出历史条数时返回 `None`。
pub async fn find_item_id_at(
    pool: &SqlitePool,
    sort: ClipboardItemSort,
    offset: i64,
) -> Result<Option<String>> {
    let query = ClipboardItemQuery {
        group: Some(ClipboardGroupFilter::All),
        sort,
        limit: 1,
        offset,
        ..ClipboardItemQuery::default()
    };
    let items = fetch_items(pool, &query, KeywordFilter::None).await?;

    Ok(items.into_iter().next().map(|item| item.id))
}

/// 按 `id` 查找单条记录，不存在时返回 `None`。
pub async fn find_item_by_id(pool: &SqlitePool, id: &str) -> Result<Option<ClipboardItem>> {
    let mut qb: QueryBuilder<Sqlite> = QueryBuilder::new(SELECT_ITEM);
    qb.push(" WHERE id = ").push_bind(id.to_owned());

    let item = qb
        .build_query_as::<ClipboardItem>()
        .fetch_optional(pool)
        .await
        .context("failed to find clipboard item by id")?;
    Ok(item)
}

/// 按 `id` 查找单条记录的「列表视图」副本——与 [`fetch_items`] 走同款 [`LIST_SELECT_ITEM`] 裁剪：
/// text 类型条目的 `content` / `search_text` 一律置空，由前端用 `summary` 渲染。
/// 供前端响应 `clipboard://updated` 事件时按 id 拉取使用，避免事件驱动刷新整页 refetch
/// 时回传整段 HTML/RTF。需要完整 `content` 的写回 / 预览路径请走 [`find_item_by_id`]。
pub async fn find_item_for_list_by_id(
    pool: &SqlitePool,
    id: &str,
) -> Result<Option<ClipboardItem>> {
    let mut qb: QueryBuilder<Sqlite> = QueryBuilder::new(LIST_SELECT_ITEM);
    qb.push(" WHERE clipboard_items.id = ")
        .push_bind(id.to_owned());

    let item = qb
        .build_query_as::<ClipboardItem>()
        .fetch_optional(pool)
        .await
        .context("failed to find clipboard item (list view) by id")?;
    Ok(item)
}

/// 翻转 `is_favorite`（收藏 / 取消收藏），返回翻转后的新状态。
pub async fn toggle_item_favorite(pool: &SqlitePool, id: &str) -> Result<bool> {
    let new_value: bool = sqlx::query_scalar(
        "UPDATE clipboard_items
         SET is_favorite = NOT is_favorite,
             favorite_order = CASE WHEN is_favorite THEN NULL
                 ELSE 1 + COALESCE((SELECT MAX(favorite_order) FROM clipboard_items), 0) END
         WHERE id = ? RETURNING is_favorite",
    )
    .bind(id)
    .fetch_one(pool)
    .await
    .context("failed to toggle clipboard item favorite")?;
    Ok(new_value)
}

/// 幂等地将 `is_favorite` 置为 true（已收藏的无变化）。auto-favorite 场景用。
pub async fn mark_item_favorite(pool: &SqlitePool, id: &str) -> Result<()> {
    sqlx::query(
        "UPDATE clipboard_items
         SET is_favorite = 1,
             favorite_order = CASE WHEN is_favorite AND favorite_order IS NOT NULL THEN favorite_order
                 ELSE 1 + COALESCE((SELECT MAX(favorite_order) FROM clipboard_items), 0) END
         WHERE id = ?",
    )
    .bind(id)
    .execute(pool)
    .await
    .context("failed to mark clipboard item favorite")?;
    Ok(())
}

/// 翻转 `is_pinned`（置顶 / 取消置顶），返回翻转后的新状态。
pub async fn toggle_item_pinned(pool: &SqlitePool, id: &str) -> Result<bool> {
    let new_value: bool = sqlx::query_scalar(
        "UPDATE clipboard_items
         SET is_pinned = NOT is_pinned,
             pin_order = CASE WHEN is_pinned THEN NULL
                 ELSE 1 + COALESCE((SELECT MAX(pin_order) FROM clipboard_items), 0) END
         WHERE id = ? RETURNING is_pinned",
    )
    .bind(id)
    .fetch_one(pool)
    .await
    .context("failed to toggle clipboard item pinned")?;
    Ok(new_value)
}

/// 在一个手动排序分区内把 `id` 放到 `anchor` 指定记录之前或之后。
/// 过滤视图只显示分区的子集，因此这里始终按全局分区顺序重排并整体重新编号。
pub async fn reorder_item(
    pool: &SqlitePool,
    section: ReorderSection,
    id: &str,
    anchor: ReorderAnchor,
) -> Result<()> {
    let (anchor_id, position) = match &anchor {
        ReorderAnchor::Before(anchor) => (anchor.as_str(), ReorderAnchor::Before(anchor.clone())),
        ReorderAnchor::After(anchor) => (anchor.as_str(), ReorderAnchor::After(anchor.clone())),
    };
    let (select_sql, update_sql) = match section {
        ReorderSection::Pinned => (
            "SELECT id FROM clipboard_items WHERE is_pinned = 1 ORDER BY pin_order DESC, updated_at DESC, created_at DESC",
            "UPDATE clipboard_items SET pin_order = ? WHERE id = ?",
        ),
        ReorderSection::Favorite => (
            "SELECT id FROM clipboard_items WHERE is_favorite = 1 AND is_pinned = 0 ORDER BY favorite_order DESC, updated_at DESC, created_at DESC",
            "UPDATE clipboard_items SET favorite_order = ? WHERE id = ?",
        ),
    };
    let mut tx = pool
        .begin()
        .await
        .context("failed to begin item reorder transaction")?;
    let mut ids: Vec<String> = sqlx::query_scalar(select_sql)
        .fetch_all(&mut *tx)
        .await
        .context("failed to read item reorder section")?;
    let Some(source_ix) = ids.iter().position(|candidate| candidate == id) else {
        return Err(anyhow::anyhow!("item to reorder is not in the requested section").into());
    };
    if ids
        .iter()
        .position(|candidate| candidate == anchor_id)
        .is_none()
    {
        return Err(anyhow::anyhow!("reorder anchor is not in the requested section").into());
    }
    if id == anchor_id {
        return Ok(());
    }

    let source = ids.remove(source_ix);
    let mut insert_ix = ids
        .iter()
        .position(|candidate| candidate == anchor_id)
        .ok_or_else(|| anyhow::anyhow!("reorder anchor is not in the requested section"))?;
    if matches!(position, ReorderAnchor::After(_)) {
        insert_ix += 1;
    }
    ids.insert(insert_ix, source);

    for (ix, item_id) in ids.iter().enumerate() {
        let rank = i64::try_from(ids.len() - ix).unwrap_or(1);
        sqlx::query(update_sql)
            .bind(rank)
            .bind(item_id)
            .execute(&mut *tx)
            .await
            .context("failed to write item reorder")?;
    }
    tx.commit()
        .await
        .context("failed to commit item reorder transaction")?;
    Ok(())
}

/// 更新备注，传 `None` 清空备注。
pub async fn update_item_note(pool: &SqlitePool, id: &str, note: Option<&str>) -> Result<()> {
    sqlx::query("UPDATE clipboard_items SET note = ? WHERE id = ?")
        .bind(note)
        .bind(id)
        .execute(pool)
        .await
        .context("failed to update clipboard item note")?;
    Ok(())
}

/// 更新条目所属分组；不刷新 `updated_at`，避免污染最近使用排序。
pub async fn update_item_group(pool: &SqlitePool, id: &str, group_id: Option<&str>) -> Result<()> {
    sqlx::query("UPDATE clipboard_items SET group_id = ? WHERE id = ?")
        .bind(group_id)
        .bind(id)
        .execute(pool)
        .await
        .context("failed to update clipboard item group")?;
    Ok(())
}

/// `use_count + 1` 并刷新 `updated_at` 与最后使用时间（命中去重时复用）。
pub async fn increment_item_use_count(pool: &SqlitePool, id: &str) -> Result<()> {
    let now = Utc::now();
    sqlx::query(
        "UPDATE clipboard_items SET use_count = use_count + 1, updated_at = ?, last_used_at = ? \
         WHERE id = ?",
    )
    .bind(now)
    .bind(now)
    .bind(id)
    .execute(pool)
    .await
    .context("failed to increment clipboard item use_count")?;
    Ok(())
}

/// 只刷新最后使用时间：「复用时更新」关闭时从历史复制 / 粘贴也算用过，
/// 自动清理据此判断，列表排序与使用次数保持不变。
pub async fn touch_item_last_used(pool: &SqlitePool, id: &str) -> Result<()> {
    sqlx::query("UPDATE clipboard_items SET last_used_at = ? WHERE id = ?")
        .bind(Utc::now())
        .bind(id)
        .execute(pool)
        .await
        .context("failed to touch clipboard item last used time")?;
    Ok(())
}

/// 删除单条记录。若删除的是图片记录，返回其落盘文件名（`<hash>.png`），供调用方删图；
/// 否则返回 `None`。记录不存在时也返回 `None`。
///
/// image 去重指纹源自 PNG 字节、落盘文件名即字节哈希，故库里同图至多一行，
/// 删行后该文件必为孤儿，调用方可直接删，无需引用计数。
pub async fn delete_item(pool: &SqlitePool, id: &str) -> Result<Option<String>> {
    let row = sqlx::query_as::<_, (ClipboardKind, String)>(
        "DELETE FROM clipboard_items WHERE id = ? RETURNING kind, content",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
    .context("failed to delete clipboard item")?;
    Ok(row.and_then(|(kind, content)| image_file_name(kind, content)))
}

/// 取被删行里需要连带删除的图片文件名：kind 为 image 时 `content` 即文件名，否则 `None`。
fn image_file_name(kind: ClipboardKind, content: String) -> Option<String> {
    (kind == ClipboardKind::Image).then_some(content)
}

/// 批量删除单条 `DELETE` 最多绑定的 id 数，远低于 SQLite 的绑定变量上限。
const DELETE_BATCH_SIZE: usize = 500;

/// 没有被收藏或置顶的文件记录的 id 与路径串（换行分隔），供清理缓存时判断文件是否还在。
pub async fn unprotected_file_items(pool: &SqlitePool) -> Result<Vec<(String, String)>> {
    Ok(sqlx::query_as(
        "SELECT id, content FROM clipboard_items \
         WHERE kind = 'files' AND is_favorite = 0 AND is_pinned = 0",
    )
    .fetch_all(pool)
    .await
    .context("failed to list file items")?)
}

/// 按 id 批量删除，返回删除行数与被删图片文件名（供调用方删图）；不存在的 id 忽略。
/// 所有分批在同一事务里完成，中途失败时一条都不删。
pub async fn delete_items(pool: &SqlitePool, ids: &[String]) -> Result<CleanupOutcome> {
    let mut outcome = CleanupOutcome::default();
    if ids.is_empty() {
        return Ok(outcome);
    }

    let mut tx = pool
        .begin()
        .await
        .context("failed to begin delete clipboard items transaction")?;
    for chunk in ids.chunks(DELETE_BATCH_SIZE) {
        let mut qb: QueryBuilder<Sqlite> =
            QueryBuilder::new("DELETE FROM clipboard_items WHERE id IN (");
        let mut separated = qb.separated(", ");
        for id in chunk {
            separated.push_bind(id.as_str());
        }
        qb.push(") RETURNING kind, content, size");

        let rows = qb
            .build_query_as::<DeletedRow>()
            .fetch_all(&mut *tx)
            .await
            .context("failed to delete clipboard items")?;
        absorb_deleted(&mut outcome, rows);
    }
    tx.commit()
        .await
        .context("failed to commit delete clipboard items transaction")?;

    Ok(outcome)
}

/// 按列表同款过滤与排序（置顶在前）取出全部匹配记录的 id 与收藏 / 置顶标记，忽略分页参数。
/// 供列表多选的全选与区间选择使用：只取三列，几万条也只是一次轻量查询。
pub async fn list_item_refs(
    pool: &SqlitePool,
    q: &ClipboardItemQuery,
) -> Result<Vec<ClipboardItemRef>> {
    let keyword = KeywordFilter::from_keyword(q.keyword.as_deref());
    let mut qb: QueryBuilder<Sqlite> = QueryBuilder::new(
        "SELECT clipboard_items.id, clipboard_items.is_favorite, clipboard_items.is_pinned \
         FROM clipboard_items WHERE 1 = 1",
    );
    push_filter_clauses(&mut qb, q, &keyword);
    push_order_clause(
        &mut qb,
        q.sort,
        q.group == Some(ClipboardGroupFilter::Favorite) || q.favorite == Some(true),
    );

    let refs = qb
        .build_query_as::<ClipboardItemRef>()
        .fetch_all(pool)
        .await
        .context("failed to list clipboard item refs")?;
    Ok(refs)
}

/// 一批删除的结果：删除行数、其中图片记录的落盘文件名（供调用方删图），
/// 以及非图片记录的内容字节数（图片的实际占用由调用方按落盘文件统计）。
#[derive(Debug, Default)]
pub struct CleanupOutcome {
    pub removed: u64,
    pub image_files: Vec<String>,
    pub content_bytes: u64,
}

impl CleanupOutcome {
    /// 合并另一批删除结果。
    pub fn merge(&mut self, other: CleanupOutcome) {
        self.removed += other.removed;
        self.image_files.extend(other.image_files);
        self.content_bytes += other.content_bytes;
    }
}

/// SQLite 空闲页占用的字节数。删行只会把页挂回空闲列表、不缩小数据库文件，
/// 这部分会被后续写入复用，按存储上限计量时应当扣除。
pub async fn reusable_page_bytes(pool: &SqlitePool) -> Result<u64> {
    let (bytes,): (i64,) = sqlx::query_as(
        "SELECT (SELECT freelist_count FROM pragma_freelist_count()) \
              * (SELECT page_size FROM pragma_page_size())",
    )
    .fetch_one(pool)
    .await
    .context("failed to read sqlite free pages")?;

    Ok(bytes.max(0) as u64)
}

/// 删除语句 `RETURNING kind, content, size` 返回的一行。
pub(crate) type DeletedRow = (ClipboardKind, String, Option<i64>);

/// 把一批被删行计入 outcome：累加行数与非图片内容字节，并收集其中的图片文件名。
pub(crate) fn absorb_deleted(outcome: &mut CleanupOutcome, rows: Vec<DeletedRow>) {
    outcome.removed += rows.len() as u64;
    for (kind, content, size) in rows {
        match image_file_name(kind, content) {
            Some(file_name) => outcome.image_files.push(file_name),
            None => outcome.content_bytes += size.map_or(0, |size| size.max(0) as u64),
        }
    }
}

/// 清空记录，返回删除行数与被删图片文件名；未显式删除的收藏 / 置顶项会保留。
pub async fn clear_items(
    pool: &SqlitePool,
    delete_favorites: bool,
    delete_pinned: bool,
) -> Result<CleanupOutcome> {
    let mut qb: QueryBuilder<Sqlite> = QueryBuilder::new("DELETE FROM clipboard_items");

    let mut has_condition = false;
    if !delete_favorites {
        qb.push(" WHERE is_favorite = 0");
        has_condition = true;
    }
    if !delete_pinned {
        qb.push(if has_condition { " AND " } else { " WHERE " });
        qb.push("is_pinned = 0");
    }
    qb.push(" RETURNING kind, content, size");

    let rows = qb
        .build_query_as::<DeletedRow>()
        .fetch_all(pool)
        .await
        .context("failed to clear clipboard items")?;

    let mut outcome = CleanupOutcome::default();
    absorb_deleted(&mut outcome, rows);
    Ok(outcome)
}

/// 关键词过滤分流：≥3 字符走 FTS5（trigram），1–2 字符走 LIKE 兜底，空 / 仅空白不过滤。
#[derive(Debug, Clone, PartialEq, Eq)]
enum KeywordFilter {
    None,
    /// `clipboard_items_fts MATCH ?` 的表达式，已含前缀通配。
    Fts(String),
    /// 已转义 `% _ \` 的关键词；下游统一拼成 `%<kw>%` 多列模糊匹配。
    Like(String),
}

impl KeywordFilter {
    /// 按字符数（非字节）判定走 FTS 还是 LIKE。CJK 一个字符也算 1，
    /// 与 trigram 的 3 字符门槛保持一致。
    ///
    /// 注意：门槛按**每个空白分词**判定，而非整串字符数。trigram 索引对 <3 字符的
    /// token 永远 0 命中，而 FTS5 表达式里多 token 之间默认是 AND —— 只要有一个短词，
    /// 整条表达式就被拖成 0 结果（例如 `a b` 会生成 `"a"* "b"*`，二者皆 <3 字符）。
    /// 因此仅当所有分词均 ≥3 字符时才走 FTS，否则降级到 LIKE 兜底。
    fn from_keyword(keyword: Option<&str>) -> Self {
        let Some(trimmed) = keyword.map(str::trim).filter(|s| !s.is_empty()) else {
            return Self::None;
        };

        let fts_viable = trimmed
            .split_whitespace()
            .all(|token| token.chars().count() >= 3);

        if fts_viable {
            if let Some(expr) = build_fts_expr(trimmed) {
                return Self::Fts(expr);
            }
        }

        Self::Like(escape_like(trimmed))
    }
}

/// 把用户关键词拆成 FTS5 前缀匹配表达式（如 `foo bar` -> `"foo"* "bar"*`）。
/// 双引号包裹 + 转义，避免关键词中的 FTS5 语法字符被当作运算符。空白关键词返回 `None`。
fn build_fts_expr(keyword: &str) -> Option<String> {
    let expr = keyword
        .split_whitespace()
        .map(|token| format!("\"{}\"*", token.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" ");

    (!expr.is_empty()).then_some(expr)
}

/// 转义 LIKE 的特殊字符（`\ % _`），配合 SQL 端 `ESCAPE '\\'`。
fn escape_like(keyword: &str) -> String {
    let mut out = String::with_capacity(keyword.len());
    for ch in keyword.chars() {
        if matches!(ch, '\\' | '%' | '_') {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

/// 按 [`push_list_query`] 取一页列表项。
async fn fetch_items(
    pool: &SqlitePool,
    q: &ClipboardItemQuery,
    keyword: KeywordFilter,
) -> Result<Vec<ClipboardItem>> {
    let mut qb: QueryBuilder<Sqlite> = QueryBuilder::new("");
    push_list_query(&mut qb, q, &keyword);

    let items = qb
        .build_query_as::<ClipboardItem>()
        .fetch_all(pool)
        .await
        .context("failed to query clipboard items")?;
    Ok(items)
}

/// 拼装列表查询：过滤（含可选关键词匹配） + 排序（置顶恒前置） + 分页。
/// 排序列与 `0003_list_sort_indexes` 的复合索引一一对应，改 ORDER BY 时同步改索引。
/// 所有 bind 均传入拥有所有权/Copy 的值，避免 `QueryBuilder` 借用 `q` 引发的生命周期问题。
fn push_list_query(qb: &mut QueryBuilder<Sqlite>, q: &ClipboardItemQuery, keyword: &KeywordFilter) {
    qb.push(LIST_SELECT_ITEM);
    qb.push(" WHERE 1 = 1");
    push_filter_clauses(qb, q, keyword);
    push_order_clause(
        qb,
        q.sort,
        q.group == Some(ClipboardGroupFilter::Favorite) || q.favorite == Some(true),
    );

    qb.push(" LIMIT ").push_bind(q.limit);
    qb.push(" OFFSET ").push_bind(q.offset);
}

/// 列表排序：置顶恒前置，其余按 `sort`。列表分页与 [`list_item_refs`] 共用，保证两边顺序一致。
fn push_order_clause(qb: &mut QueryBuilder<Sqlite>, sort: ClipboardItemSort, favorite_tab: bool) {
    qb.push(" ORDER BY clipboard_items.is_pinned DESC, clipboard_items.pin_order DESC, ");
    if favorite_tab {
        qb.push("clipboard_items.favorite_order DESC, ");
    }
    match sort {
        ClipboardItemSort::CreatedAt => {
            qb.push("clipboard_items.created_at DESC");
        }
        ClipboardItemSort::UpdatedAt => {
            qb.push("clipboard_items.updated_at DESC, clipboard_items.created_at DESC");
        }
        ClipboardItemSort::UseCount => {
            qb.push("clipboard_items.use_count DESC, clipboard_items.created_at DESC");
        }
    }
}

/// 统计满足同样过滤条件的总条数（不参与排序 / 分页），与 [`fetch_items`] 共用 [`push_filter_clauses`]。
async fn fetch_items_count(
    pool: &SqlitePool,
    q: &ClipboardItemQuery,
    keyword: KeywordFilter,
) -> Result<i64> {
    let mut qb: QueryBuilder<Sqlite> = QueryBuilder::new("SELECT COUNT(*) FROM clipboard_items");
    qb.push(" WHERE 1 = 1");
    push_filter_clauses(&mut qb, q, &keyword);

    let row: (i64,) = qb
        .build_query_as()
        .fetch_one(pool)
        .await
        .context("failed to count filtered clipboard items")?;
    Ok(row.0)
}

/// 按扩展名（小写）认作图片的文件格式，列表的图片文件预览与「图片」分组共用。
pub const IMAGE_FILE_EXTENSIONS: [&str; 13] = [
    "jpg", "jpeg", "png", "webp", "avif", "gif", "svg", "bmp", "ico", "tif", "tiff", "heic", "apng",
];

/// 「图片」分组：图片记录，加上只复制了一个图片文件的文件记录（卡片按图片显示）。
/// SQLite 的 `LIKE` 对 ASCII 不区分大小写，扩展名是常量，直接拼进 SQL。
fn push_image_group_clause(qb: &mut QueryBuilder<Sqlite>) {
    qb.push(
        " AND (clipboard_items.kind = 'image' OR (clipboard_items.kind = 'files' \
         AND instr(clipboard_items.content, char(10)) = 0 AND (",
    );
    for (index, ext) in IMAGE_FILE_EXTENSIONS.iter().enumerate() {
        if index > 0 {
            qb.push(" OR ");
        }
        qb.push(format!("clipboard_items.content LIKE '%.{ext}'"));
    }
    qb.push(")))");
}

/// 把当前查询的过滤条件追加到 `qb`（不含 ORDER BY / LIMIT / OFFSET），供列表查询与计数共用。
fn push_filter_clauses(
    qb: &mut QueryBuilder<Sqlite>,
    q: &ClipboardItemQuery,
    keyword: &KeywordFilter,
) {
    if *keyword != KeywordFilter::None {
        qb.push(" AND (");
        match keyword {
            KeywordFilter::Fts(expr) => {
                qb.push("clipboard_items.rowid IN (SELECT rowid FROM clipboard_items_fts WHERE clipboard_items_fts MATCH ")
                    .push_bind(expr.clone()).push(")");
            }
            KeywordFilter::Like(kw) => {
                let pattern = format!("%{kw}%");
                qb.push("clipboard_items.search_text LIKE ")
                    .push_bind(pattern.clone())
                    .push(" ESCAPE '\\' OR clipboard_items.note LIKE ")
                    .push_bind(pattern)
                    .push(" ESCAPE '\\'");
            }
            KeywordFilter::None => {}
        }
        if q.ocr_enabled {
            qb.push(" OR ");
            push_ocr_match(qb, keyword);
        }
        qb.push(")");
    }
    // group（UI Tab）覆盖显式 kind；为 None 时回退到显式 kind（单测使用）。
    // 收藏可与分类叠加，所以显式 favorite 在各 Tab 下都生效。
    let (effective_kind, effective_favorite) = match q.group {
        Some(ClipboardGroupFilter::All) => (None, q.favorite),
        Some(ClipboardGroupFilter::Text) => (Some(ClipboardKind::Text), q.favorite),
        Some(ClipboardGroupFilter::Image) => {
            push_image_group_clause(qb);
            (None, q.favorite)
        }
        Some(ClipboardGroupFilter::Files) => (Some(ClipboardKind::Files), q.favorite),
        Some(ClipboardGroupFilter::Favorite) => (None, Some(true)),
        None => (q.kind, q.favorite),
    };
    if let Some(kind) = effective_kind {
        qb.push(" AND clipboard_items.kind = ").push_bind(kind);
    }
    if let Some(group_id) = &q.group_id {
        qb.push(" AND clipboard_items.group_id = ")
            .push_bind(group_id.clone());
    }
    if let Some(favorite) = effective_favorite {
        qb.push(" AND clipboard_items.is_favorite = ")
            .push_bind(favorite);
    }
    if let Some(pinned) = q.pinned {
        qb.push(" AND clipboard_items.is_pinned = ")
            .push_bind(pinned);
    }
}

/// OCR 匹配作为非相关子查询，FTS 一次求命中集合后用 item_id 主键关联。
fn push_ocr_match(qb: &mut QueryBuilder<Sqlite>, keyword: &KeywordFilter) {
    qb.push("clipboard_items.kind = 'image' AND clipboard_items.id IN (SELECT item_id FROM image_texts WHERE status = 'done' AND ");
    match keyword {
        KeywordFilter::Fts(expr) => {
            qb.push("rowid IN (SELECT rowid FROM image_texts_fts WHERE image_texts_fts MATCH ")
                .push_bind(expr.clone())
                .push(")");
        }
        KeywordFilter::Like(kw) => {
            qb.push("text LIKE ")
                .push_bind(format!("%{kw}%"))
                .push(" ESCAPE '\\'");
        }
        KeywordFilter::None => {
            qb.push("0");
        }
    }
    qb.push(")");
}

/// 展示层仅查询布尔值，不把识别正文装入列表；短词与分页/全选共享同一谓词。
pub(crate) async fn image_text_flags(
    pool: &SqlitePool,
    id: &str,
    keyword: Option<&str>,
) -> Result<(bool, bool)> {
    let keyword = KeywordFilter::from_keyword(keyword);
    let mut qb = QueryBuilder::<Sqlite>::new("SELECT EXISTS(SELECT 1 FROM image_texts WHERE item_id = clipboard_items.id AND status = 'done' AND text <> ''), (");
    if keyword == KeywordFilter::None {
        qb.push("0");
    } else {
        push_ocr_match(&mut qb, &keyword);
    }
    qb.push(") FROM clipboard_items WHERE id = ")
        .push_bind(id.to_owned());
    Ok(qb
        .build_query_as()
        .fetch_one(pool)
        .await
        .context("failed to read image text flags")?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::groups::insert_group;
    use crate::db::models::{ClipboardGroup, ClipboardKind, Platform};
    use crate::db::test_support::memory_pool;
    use chrono::DateTime;

    fn sample_item(id: &str) -> ClipboardItem {
        let ts = DateTime::from_timestamp(1_700_000_000, 0).unwrap();
        let content = format!("content-{id}");
        ClipboardItem {
            id: id.to_owned(),
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
            is_sensitive: false,
            platform: Platform::Macos,
            note: None,
            created_at: ts,
            updated_at: ts,
            origin_device_id: None,
            source_app_name: None,
            source_app_icon_file: None,
        }
    }

    fn ids(items: &[ClipboardItem]) -> Vec<&str> {
        items.iter().map(|item| item.id.as_str()).collect()
    }

    #[tokio::test]
    async fn item_id_at_follows_list_order_with_pinned_first() {
        let pool = memory_pool().await;
        let oldest = sample_item("oldest");
        let mut middle = sample_item("middle");
        middle.created_at = oldest.created_at + chrono::Duration::seconds(1);
        middle.updated_at = middle.created_at;
        let mut newest = sample_item("newest");
        newest.created_at = oldest.created_at + chrono::Duration::seconds(2);
        newest.updated_at = newest.created_at;
        let mut pinned = sample_item("pinned");
        pinned.is_pinned = true;
        for item in [&oldest, &middle, &newest, &pinned] {
            insert_item(&pool, item).await.unwrap();
        }

        let mut ordered = Vec::new();
        for offset in 0..5 {
            ordered.push(
                find_item_id_at(&pool, ClipboardItemSort::UpdatedAt, offset)
                    .await
                    .unwrap(),
            );
        }

        assert_eq!(
            ordered,
            vec![
                Some("pinned".to_owned()),
                Some("newest".to_owned()),
                Some("middle".to_owned()),
                Some("oldest".to_owned()),
                None,
            ]
        );
    }

    #[tokio::test]
    async fn dedup_lookup_returns_latest_id_and_none_for_missing() {
        let pool = memory_pool().await;
        let older = sample_item("older");
        let mut newer = older.clone();
        newer.id = "newer".to_owned();
        newer.created_at = older.created_at + chrono::Duration::seconds(1);
        insert_item(&pool, &older).await.unwrap();
        insert_item(&pool, &newer).await.unwrap();

        assert_eq!(
            find_item_by_content_hash(&pool, &older.content_hash)
                .await
                .unwrap(),
            Some(newer.id)
        );
        assert!(find_item_by_content_hash(&pool, "missing")
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn dedup_preserves_large_payload_and_metadata() {
        let pool = memory_pool().await;
        let mut first = sample_item("first");
        first.content = "x".repeat(1024 * 1024);
        first.content_hash = content_hash(first.kind, &first.content);
        first.search_text = Some(first.content.clone());
        first.is_sensitive = true;
        first.is_favorite = true;
        first.is_pinned = true;
        first.note = Some("keep".to_owned());
        insert_item(&pool, &first).await.unwrap();
        let mut duplicate = first.clone();
        duplicate.id = "duplicate".to_owned();
        duplicate.is_sensitive = false;
        duplicate.is_favorite = false;
        duplicate.is_pinned = false;
        duplicate.note = None;

        let result = upsert_item(&pool, &duplicate).await.unwrap();
        let saved = find_item_by_id(&pool, &first.id).await.unwrap().unwrap();

        assert!(result.deduplicated);
        assert_eq!(result.id, first.id);
        assert_eq!(saved.use_count, first.use_count + 1);
        assert!(saved.updated_at > first.updated_at);
        assert_eq!(saved.created_at, first.created_at);
        assert_eq!(saved.content, first.content);
        assert_eq!(saved.search_text, first.search_text);
        assert_eq!(saved.is_sensitive, first.is_sensitive);
        assert_eq!(saved.is_favorite, first.is_favorite);
        assert_eq!(saved.is_pinned, first.is_pinned);
        assert_eq!(saved.note, first.note);
        assert!(find_item_by_id(&pool, &duplicate.id)
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn insert_and_find_by_id_roundtrip() {
        let pool = memory_pool().await;
        insert_item(&pool, &sample_item("a")).await.unwrap();

        let found = find_item_by_id(&pool, "a")
            .await
            .unwrap()
            .expect("item should exist");
        assert_eq!(found.id, "a");
        assert_eq!(found.content, "content-a");
        assert_eq!(found.kind, ClipboardKind::Text);
        assert_eq!(found.use_count, 1);

        assert!(find_item_by_id(&pool, "missing").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn upsert_inserts_when_content_is_new() {
        let pool = memory_pool().await;
        let result = upsert_item(&pool, &sample_item("a")).await.unwrap();
        assert_eq!(
            result,
            UpsertResult {
                id: "a".to_owned(),
                deduplicated: false,
            }
        );
        assert_eq!(
            find_item_by_id(&pool, "a")
                .await
                .unwrap()
                .unwrap()
                .use_count,
            1
        );
    }

    #[tokio::test]
    async fn upsert_dedups_same_content_bumping_use_count() {
        let pool = memory_pool().await;
        let first = sample_item("first");
        upsert_item(&pool, &first).await.unwrap();

        // 不同 id，但内容相同 → content_hash 相同 → 命中去重，不插入新行。
        let mut dup = sample_item("second");
        dup.content = first.content.clone();
        dup.content_hash = first.content_hash.clone();
        let result = upsert_item(&pool, &dup).await.unwrap();

        assert_eq!(
            result,
            UpsertResult {
                id: "first".to_owned(),
                deduplicated: true,
            }
        );
        // 仍只有一行，且命中行 use_count 累加到 2。
        let all = query_items(&pool, &ClipboardItemQuery::default())
            .await
            .unwrap();
        assert_eq!(ids(&all), ["first"]);
        assert_eq!(all[0].use_count, 2);
        assert!(find_item_by_id(&pool, "second").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn upsert_keeps_distinct_content_separate() {
        let pool = memory_pool().await;
        upsert_item(&pool, &sample_item("a")).await.unwrap();
        upsert_item(&pool, &sample_item("b")).await.unwrap();

        let all = query_items(&pool, &ClipboardItemQuery::default())
            .await
            .unwrap();
        assert_eq!(all.len(), 2);
    }

    #[test]
    fn query_default_sorts_by_updated_at() {
        assert_eq!(
            ClipboardItemQuery::default().sort,
            ClipboardItemSort::UpdatedAt
        );
    }

    #[test]
    fn content_hash_is_stable_and_kind_scoped() {
        // 同 kind 同内容 → 同哈希（稳定、可去重）。
        assert_eq!(
            content_hash(ClipboardKind::Text, "hello"),
            content_hash(ClipboardKind::Text, "hello")
        );
        // 内容不同 → 哈希不同。
        assert_ne!(
            content_hash(ClipboardKind::Text, "hello"),
            content_hash(ClipboardKind::Text, "world")
        );
        // 内容相同但 kind 不同 → 哈希不同（kind 前缀隔离）。
        assert_ne!(
            content_hash(ClipboardKind::Text, "same"),
            content_hash(ClipboardKind::Files, "same")
        );
    }

    #[tokio::test]
    async fn query_filters_by_kind() {
        let pool = memory_pool().await;
        let mut text = sample_item("text");
        text.kind = ClipboardKind::Text;
        let mut image = sample_item("image");
        image.kind = ClipboardKind::Image;
        insert_item(&pool, &text).await.unwrap();
        insert_item(&pool, &image).await.unwrap();

        let q = ClipboardItemQuery {
            kind: Some(ClipboardKind::Image),
            ..Default::default()
        };
        assert_eq!(ids(&query_items(&pool, &q).await.unwrap()), ["image"]);
    }

    #[tokio::test]
    async fn image_group_includes_single_image_files() {
        let pool = memory_pool().await;
        let mut image = sample_item("image");
        image.kind = ClipboardKind::Image;
        insert_item(&pool, &image).await.unwrap();
        for (id, content) in [
            ("photo", "C:\\pics\\a.PNG"),
            ("many", "C:\\pics\\a.png\nC:\\pics\\b.png"),
            ("doc", "C:\\docs\\a.txt"),
        ] {
            let mut files = sample_item(id);
            files.kind = ClipboardKind::Files;
            files.content = content.to_owned();
            files.content_hash = content_hash(ClipboardKind::Files, content);
            insert_item(&pool, &files).await.unwrap();
        }

        let q = |group| ClipboardItemQuery {
            group: Some(group),
            ..Default::default()
        };
        let images = query_items(&pool, &q(ClipboardGroupFilter::Image))
            .await
            .unwrap();
        let mut image_ids = ids(&images);
        image_ids.sort_unstable();
        assert_eq!(image_ids, ["image", "photo"]);
        let files = query_items(&pool, &q(ClipboardGroupFilter::Files))
            .await
            .unwrap();
        assert_eq!(files.len(), 3);
    }

    #[tokio::test]
    async fn favorite_combines_with_category_tab() {
        let pool = memory_pool().await;
        let mut text_fav = sample_item("text-fav");
        text_fav.is_favorite = true;
        let mut image_fav = sample_item("image-fav");
        image_fav.kind = ClipboardKind::Image;
        image_fav.is_favorite = true;
        let mut image_plain = sample_item("image-plain");
        image_plain.kind = ClipboardKind::Image;
        for item in [&text_fav, &image_fav, &image_plain] {
            insert_item(&pool, item).await.unwrap();
        }

        let favorite_images = query_items(
            &pool,
            &ClipboardItemQuery {
                group: Some(ClipboardGroupFilter::Image),
                favorite: Some(true),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(ids(&favorite_images), ["image-fav"]);
    }

    #[tokio::test]
    async fn query_filters_by_favorite_and_pinned() {
        let pool = memory_pool().await;
        let plain = sample_item("plain");
        let mut fav = sample_item("fav");
        fav.is_favorite = true;
        let mut pin = sample_item("pin");
        pin.is_pinned = true;
        for item in [&plain, &fav, &pin] {
            insert_item(&pool, item).await.unwrap();
        }

        let favs = query_items(
            &pool,
            &ClipboardItemQuery {
                favorite: Some(true),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(ids(&favs), ["fav"]);

        let pins = query_items(
            &pool,
            &ClipboardItemQuery {
                pinned: Some(true),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(ids(&pins), ["pin"]);
    }

    #[tokio::test]
    async fn query_filters_by_group() {
        let pool = memory_pool().await;
        let group = ClipboardGroup {
            id: "g1".to_owned(),
            name: "G1".to_owned(),
            icon: "i-lets-icons:folder".to_owned(),
            is_hidden: false,
            sort_order: 0,
            created_at: DateTime::from_timestamp(1_700_000_000, 0).unwrap(),
            updated_at: DateTime::from_timestamp(1_700_000_000, 0).unwrap(),
        };
        insert_group(&pool, &group).await.unwrap();

        let mut grouped = sample_item("grouped");
        grouped.group_id = Some("g1".to_owned());
        insert_item(&pool, &grouped).await.unwrap();
        insert_item(&pool, &sample_item("ungrouped")).await.unwrap();

        let q = ClipboardItemQuery {
            group_id: Some("g1".to_owned()),
            ..Default::default()
        };
        assert_eq!(ids(&query_items(&pool, &q).await.unwrap()), ["grouped"]);
    }

    #[tokio::test]
    async fn update_item_group_does_not_refresh_updated_at() {
        let pool = memory_pool().await;
        let group = ClipboardGroup {
            id: "g1".to_owned(),
            name: "G1".to_owned(),
            icon: "i-lets-icons:folder".to_owned(),
            is_hidden: false,
            sort_order: 0,
            created_at: DateTime::from_timestamp(1_700_000_000, 0).unwrap(),
            updated_at: DateTime::from_timestamp(1_700_000_000, 0).unwrap(),
        };
        insert_group(&pool, &group).await.unwrap();

        let item = sample_item("item");
        let original_updated_at = item.updated_at;
        insert_item(&pool, &item).await.unwrap();

        update_item_group(&pool, "item", Some("g1")).await.unwrap();

        let moved = find_item_by_id(&pool, "item").await.unwrap().unwrap();
        assert_eq!(moved.group_id, Some("g1".to_owned()));
        assert_eq!(moved.updated_at, original_updated_at);
    }

    #[tokio::test]
    async fn query_orders_pinned_first_then_by_sort() {
        let pool = memory_pool().await;
        let mut a = sample_item("a");
        a.created_at = DateTime::from_timestamp(1_700_000_000, 0).unwrap();
        a.updated_at = DateTime::from_timestamp(1_700_000_010, 0).unwrap();
        a.use_count = 9;
        let mut b = sample_item("b");
        b.created_at = DateTime::from_timestamp(1_700_000_010, 0).unwrap();
        b.updated_at = DateTime::from_timestamp(1_700_000_020, 0).unwrap();
        b.is_pinned = true;
        b.use_count = 1;
        let mut c = sample_item("c");
        c.created_at = DateTime::from_timestamp(1_700_000_020, 0).unwrap();
        c.updated_at = DateTime::from_timestamp(1_700_000_000, 0).unwrap();
        c.use_count = 5;
        for item in [&a, &b, &c] {
            insert_item(&pool, item).await.unwrap();
        }

        // 置顶项 b 恒前置；其余按时间倒序 c(20) > a(0)。
        let by_time = query_items(
            &pool,
            &ClipboardItemQuery {
                sort: ClipboardItemSort::CreatedAt,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(ids(&by_time), ["b", "c", "a"]);

        // 置顶项 b 恒前置；其余按更新时间倒序 a(10) > c(0)。
        let by_updated = query_items(
            &pool,
            &ClipboardItemQuery {
                sort: ClipboardItemSort::UpdatedAt,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(ids(&by_updated), ["b", "a", "c"]);

        // 置顶项 b 恒前置；其余按使用次数倒序 a(9) > c(5)。
        let by_use = query_items(
            &pool,
            &ClipboardItemQuery {
                sort: ClipboardItemSort::UseCount,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(ids(&by_use), ["b", "a", "c"]);
    }

    #[tokio::test]
    async fn manual_orders_assign_top_clear_and_reorder() {
        let pool = memory_pool().await;
        for id in ["a", "b", "c"] {
            insert_item(&pool, &sample_item(id)).await.unwrap();
        }
        assert!(toggle_item_pinned(&pool, "a").await.unwrap());
        assert!(toggle_item_pinned(&pool, "b").await.unwrap());
        let pin_orders: Vec<(String, i64)> = sqlx::query_as(
            "SELECT id, pin_order FROM clipboard_items WHERE is_pinned = 1 ORDER BY pin_order DESC",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(
            pin_orders
                .iter()
                .map(|row| row.0.as_str())
                .collect::<Vec<_>>(),
            ["b", "a"]
        );

        reorder_item(
            &pool,
            ReorderSection::Pinned,
            "a",
            ReorderAnchor::Before("b".to_owned()),
        )
        .await
        .unwrap();
        let q = ClipboardItemQuery {
            group: Some(ClipboardGroupFilter::All),
            ..Default::default()
        };
        assert_eq!(ids(&query_items(&pool, &q).await.unwrap()), ["a", "b", "c"]);
        toggle_item_pinned(&pool, "a").await.unwrap();
        assert!(sqlx::query_scalar::<_, Option<i64>>(
            "SELECT pin_order FROM clipboard_items WHERE id = 'a'"
        )
        .fetch_one(&pool)
        .await
        .unwrap()
        .is_none());
        toggle_item_pinned(&pool, "b").await.unwrap();

        assert!(toggle_item_favorite(&pool, "a").await.unwrap());
        assert!(toggle_item_favorite(&pool, "b").await.unwrap());
        reorder_item(
            &pool,
            ReorderSection::Favorite,
            "a",
            ReorderAnchor::After("b".to_owned()),
        )
        .await
        .unwrap();
        let favorite = query_items(
            &pool,
            &ClipboardItemQuery {
                group: Some(ClipboardGroupFilter::Favorite),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(ids(&favorite), ["b", "a"]);
    }

    #[tokio::test]
    async fn query_paginates_with_limit_and_offset() {
        let pool = memory_pool().await;
        for n in 0..5i64 {
            let mut item = sample_item(&format!("id{n}"));
            item.created_at = DateTime::from_timestamp(1_700_000_000 + n, 0).unwrap();
            insert_item(&pool, &item).await.unwrap();
        }

        let page = |limit, offset| ClipboardItemQuery {
            limit,
            offset,
            ..Default::default()
        };
        // created_at 倒序：id4, id3, id2, id1, id0
        assert_eq!(
            ids(&query_items(&pool, &page(2, 0)).await.unwrap()),
            ["id4", "id3"]
        );
        assert_eq!(
            ids(&query_items(&pool, &page(2, 2)).await.unwrap()),
            ["id2", "id1"]
        );
        assert_eq!(
            ids(&query_items(&pool, &page(2, 4)).await.unwrap()),
            ["id0"]
        );
    }

    #[tokio::test]
    async fn search_fts_matches_prefix_across_columns() {
        let pool = memory_pool().await;
        let mut a = sample_item("a");
        a.content = "hello rustacean".to_owned();
        a.search_text = Some("hello rustacean".to_owned());
        let mut b = sample_item("b");
        b.content = "goodbye world".to_owned();
        b.search_text = Some("searchable token".to_owned());
        let mut c = sample_item("c");
        c.content = "plain".to_owned();
        c.note = Some("annotated".to_owned());
        for item in [&a, &b, &c] {
            insert_item(&pool, item).await.unwrap();
        }

        let search = |kw: &str| ClipboardItemQuery {
            keyword: Some(kw.to_owned()),
            ..Default::default()
        };
        assert_eq!(
            ids(&query_items(&pool, &search("rust")).await.unwrap()),
            ["a"]
        );
        assert_eq!(
            ids(&query_items(&pool, &search("token")).await.unwrap()),
            ["b"]
        );
        assert_eq!(
            ids(&query_items(&pool, &search("annot")).await.unwrap()),
            ["c"]
        );
        assert!(query_items(&pool, &search("zzz")).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn search_fts_still_applies_filters() {
        let pool = memory_pool().await;
        let mut x = sample_item("x");
        x.content = "shared text".to_owned();
        x.search_text = Some("shared text".to_owned());
        x.is_favorite = true;
        let mut y = sample_item("y");
        y.content = "shared note".to_owned();
        y.search_text = Some("shared note".to_owned());
        insert_item(&pool, &x).await.unwrap();
        insert_item(&pool, &y).await.unwrap();

        let q = ClipboardItemQuery {
            keyword: Some("shared".to_owned()),
            favorite: Some(true),
            ..Default::default()
        };
        assert_eq!(ids(&query_items(&pool, &q).await.unwrap()), ["x"]);
    }

    #[tokio::test]
    async fn toggle_favorite_and_pinned_flip_flags() {
        let pool = memory_pool().await;
        insert_item(&pool, &sample_item("a")).await.unwrap();

        toggle_item_favorite(&pool, "a").await.unwrap();
        assert!(
            find_item_by_id(&pool, "a")
                .await
                .unwrap()
                .unwrap()
                .is_favorite
        );
        toggle_item_favorite(&pool, "a").await.unwrap();
        assert!(
            !find_item_by_id(&pool, "a")
                .await
                .unwrap()
                .unwrap()
                .is_favorite
        );

        toggle_item_pinned(&pool, "a").await.unwrap();
        assert!(
            find_item_by_id(&pool, "a")
                .await
                .unwrap()
                .unwrap()
                .is_pinned
        );
        toggle_item_pinned(&pool, "a").await.unwrap();
        assert!(
            !find_item_by_id(&pool, "a")
                .await
                .unwrap()
                .unwrap()
                .is_pinned
        );
    }

    #[tokio::test]
    async fn update_note_sets_and_clears() {
        let pool = memory_pool().await;
        insert_item(&pool, &sample_item("a")).await.unwrap();

        update_item_note(&pool, "a", Some("my note")).await.unwrap();
        assert_eq!(
            find_item_by_id(&pool, "a")
                .await
                .unwrap()
                .unwrap()
                .note
                .as_deref(),
            Some("my note")
        );

        update_item_note(&pool, "a", None).await.unwrap();
        assert_eq!(
            find_item_by_id(&pool, "a").await.unwrap().unwrap().note,
            None
        );
    }

    #[tokio::test]
    async fn increment_use_count_bumps_count_and_updated_at() {
        let pool = memory_pool().await;
        let item = sample_item("a");
        let original_updated_at = item.updated_at;
        insert_item(&pool, &item).await.unwrap();

        increment_item_use_count(&pool, "a").await.unwrap();

        let after = find_item_by_id(&pool, "a").await.unwrap().unwrap();
        assert_eq!(after.use_count, 2);
        assert!(after.updated_at > original_updated_at);
    }

    async fn last_used_at(pool: &SqlitePool, id: &str) -> chrono::DateTime<Utc> {
        sqlx::query_scalar("SELECT last_used_at FROM clipboard_items WHERE id = ?")
            .bind(id)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn last_used_time_starts_at_capture_and_follows_reuse() {
        let pool = memory_pool().await;
        let item = sample_item("a");
        insert_item(&pool, &item).await.unwrap();
        assert_eq!(last_used_at(&pool, "a").await, item.updated_at);

        increment_item_use_count(&pool, "a").await.unwrap();
        let reused = find_item_by_id(&pool, "a").await.unwrap().unwrap();
        assert_eq!(last_used_at(&pool, "a").await, reused.updated_at);

        // 只记使用时间：列表排序用的 updated_at 与使用次数不变。
        touch_item_last_used(&pool, "a").await.unwrap();
        let touched = find_item_by_id(&pool, "a").await.unwrap().unwrap();
        assert!(last_used_at(&pool, "a").await > reused.updated_at);
        assert_eq!(touched.updated_at, reused.updated_at);
        assert_eq!(touched.use_count, reused.use_count);
    }

    #[tokio::test]
    async fn delete_item_removes_row() {
        let pool = memory_pool().await;
        insert_item(&pool, &sample_item("a")).await.unwrap();

        // 文本行删除：无图片文件名返回。
        assert_eq!(delete_item(&pool, "a").await.unwrap(), None);
        assert!(find_item_by_id(&pool, "a").await.unwrap().is_none());

        // 记录不存在：同样返回 None，不报错。
        assert_eq!(delete_item(&pool, "missing").await.unwrap(), None);
    }

    #[tokio::test]
    async fn delete_image_item_returns_file_name() {
        let pool = memory_pool().await;
        let mut img = sample_item("img");
        img.kind = ClipboardKind::Image;
        img.content = "deadbeef.png".to_owned();
        img.content_hash = content_hash(ClipboardKind::Image, "deadbeef.png");
        insert_item(&pool, &img).await.unwrap();

        // 图片行删除：返回落盘文件名供调用方删图。
        assert_eq!(
            delete_item(&pool, "img").await.unwrap().as_deref(),
            Some("deadbeef.png")
        );
        assert!(find_item_by_id(&pool, "img").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn delete_items_returns_count_and_ignores_empty() {
        let pool = memory_pool().await;
        for id in ["a", "b", "c"] {
            insert_item(&pool, &sample_item(id)).await.unwrap();
        }
        let mut img = sample_item("img");
        img.kind = ClipboardKind::Image;
        img.content = "deadbeef.png".to_owned();
        img.content_hash = content_hash(ClipboardKind::Image, "deadbeef.png");
        insert_item(&pool, &img).await.unwrap();

        assert_eq!(delete_items(&pool, &[]).await.unwrap().removed, 0);

        let outcome = delete_items(
            &pool,
            &[
                "a".to_owned(),
                "img".to_owned(),
                "b".to_owned(),
                "missing".to_owned(),
            ],
        )
        .await
        .unwrap();
        assert_eq!(outcome.removed, 3);
        assert_eq!(outcome.image_files, ["deadbeef.png"]);
        assert_eq!(
            ids(&query_items(&pool, &ClipboardItemQuery::default())
                .await
                .unwrap()),
            ["c"]
        );
    }

    #[tokio::test]
    async fn delete_items_spans_several_batches() {
        let pool = memory_pool().await;
        let all: Vec<String> = (0..DELETE_BATCH_SIZE + 3)
            .map(|index| format!("item-{index}"))
            .collect();
        for id in &all {
            insert_item(&pool, &sample_item(id)).await.unwrap();
        }

        let outcome = delete_items(&pool, &all[1..]).await.unwrap();
        assert_eq!(outcome.removed, (all.len() - 1) as u64);
        assert_eq!(
            ids(&query_items(&pool, &ClipboardItemQuery::default())
                .await
                .unwrap()),
            ["item-0"]
        );
    }

    #[tokio::test]
    async fn list_item_refs_follows_list_filters_and_order() {
        let pool = memory_pool().await;
        let mut a = sample_item("a");
        a.created_at = DateTime::from_timestamp(1_700_000_000, 0).unwrap();
        a.is_favorite = true;
        let mut b = sample_item("b");
        b.created_at = DateTime::from_timestamp(1_700_000_010, 0).unwrap();
        b.is_pinned = true;
        let mut c = sample_item("c");
        c.created_at = DateTime::from_timestamp(1_700_000_020, 0).unwrap();
        let mut img = sample_item("img");
        img.kind = ClipboardKind::Image;
        img.created_at = DateTime::from_timestamp(1_700_000_030, 0).unwrap();
        for item in [&a, &b, &c, &img] {
            insert_item(&pool, item).await.unwrap();
        }

        // 分页参数不影响结果：置顶 b 在前，其余按创建时间倒序。
        let refs = list_item_refs(
            &pool,
            &ClipboardItemQuery {
                sort: ClipboardItemSort::CreatedAt,
                limit: 1,
                offset: 1,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let ref_ids: Vec<&str> = refs.iter().map(|item| item.id.as_str()).collect();
        assert_eq!(ref_ids, ["b", "img", "c", "a"]);
        assert!(refs[0].is_pinned && !refs[0].is_favorite);
        assert!(refs[3].is_favorite && !refs[3].is_pinned);

        let text_refs = list_item_refs(
            &pool,
            &ClipboardItemQuery {
                kind: Some(ClipboardKind::Text),
                sort: ClipboardItemSort::CreatedAt,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let text_ids: Vec<&str> = text_refs.iter().map(|item| item.id.as_str()).collect();
        assert_eq!(text_ids, ["b", "c", "a"]);
    }

    #[tokio::test]
    async fn clear_items_optionally_keeps_favorites_and_pinned() {
        let pool = memory_pool().await;
        let mut fav = sample_item("fav");
        fav.is_favorite = true;
        fav.created_at = DateTime::from_timestamp(1_700_000_001, 0).unwrap();
        insert_item(&pool, &fav).await.unwrap();
        let mut pin = sample_item("pin");
        pin.is_pinned = true;
        pin.created_at = DateTime::from_timestamp(1_700_000_002, 0).unwrap();
        insert_item(&pool, &pin).await.unwrap();
        let mut both = sample_item("both");
        both.is_favorite = true;
        both.is_pinned = true;
        both.created_at = DateTime::from_timestamp(1_700_000_003, 0).unwrap();
        insert_item(&pool, &both).await.unwrap();
        let mut plain = sample_item("plain");
        plain.created_at = DateTime::from_timestamp(1_700_000_004, 0).unwrap();
        insert_item(&pool, &plain).await.unwrap();

        assert_eq!(clear_items(&pool, false, false).await.unwrap().removed, 1);
        assert_eq!(
            ids(&query_items(&pool, &ClipboardItemQuery::default())
                .await
                .unwrap()),
            ["both", "pin", "fav"]
        );

        assert_eq!(clear_items(&pool, true, false).await.unwrap().removed, 1);
        assert_eq!(
            ids(&query_items(&pool, &ClipboardItemQuery::default())
                .await
                .unwrap()),
            ["both", "pin"]
        );

        assert_eq!(clear_items(&pool, true, true).await.unwrap().removed, 2);
        assert!(query_items(&pool, &ClipboardItemQuery::default())
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn deleted_rows_count_as_reusable_page_bytes() {
        let pool = memory_pool().await;
        for n in 0..40 {
            let mut it = sample_item(&format!("big{n}"));
            it.content = format!("{n}-{}", "x".repeat(16 * 1024));
            it.content_hash = content_hash(ClipboardKind::Text, &it.content);
            insert_item(&pool, &it).await.unwrap();
        }
        let before = reusable_page_bytes(&pool).await.unwrap();

        clear_items(&pool, true, true).await.unwrap();

        // 删掉约 640 KB 文本后，文件不缩小但空闲页至少覆盖这部分正文。
        let after = reusable_page_bytes(&pool).await.unwrap();
        assert!(
            after >= before + 40 * 16 * 1024,
            "before={before} after={after}"
        );
    }

    #[test]
    fn keyword_filter_classifies_by_char_length() {
        assert_eq!(KeywordFilter::from_keyword(None), KeywordFilter::None);
        assert_eq!(
            KeywordFilter::from_keyword(Some("   ")),
            KeywordFilter::None
        );

        // 1–2 字符走 LIKE（含 CJK）。
        assert_eq!(
            KeywordFilter::from_keyword(Some("a")),
            KeywordFilter::Like("a".to_owned())
        );
        assert_eq!(
            KeywordFilter::from_keyword(Some("中文")),
            KeywordFilter::Like("中文".to_owned())
        );

        // ≥3 字符走 FTS。
        assert_eq!(
            KeywordFilter::from_keyword(Some("foo")),
            KeywordFilter::Fts("\"foo\"*".to_owned())
        );
        assert_eq!(
            KeywordFilter::from_keyword(Some("foo bar")),
            KeywordFilter::Fts("\"foo\"* \"bar\"*".to_owned())
        );
        assert_eq!(
            KeywordFilter::from_keyword(Some("a\"b")),
            KeywordFilter::Fts("\"a\"\"b\"*".to_owned())
        );
    }

    #[test]
    fn keyword_filter_drops_multi_token_short_words_to_like() {
        // 整串 ≥3 字符，但每个空白分词都 <3 字符：trigram 对 <3 字符 token 永远 0 命中，
        // 且 FTS5 多 token 默认 AND，整条表达式会被任一短词拖成 0 结果 → 必须降级 LIKE。
        assert_eq!(
            KeywordFilter::from_keyword(Some("a b")),
            KeywordFilter::Like("a b".to_owned())
        );
        assert_eq!(
            KeywordFilter::from_keyword(Some("ab cd")),
            KeywordFilter::Like("ab cd".to_owned())
        );
        // 混合长度：只要有一个分词 <3 字符，整条 FTS 表达式就会被拖成 0 命中 → LIKE。
        assert_eq!(
            KeywordFilter::from_keyword(Some("foo b")),
            KeywordFilter::Like("foo b".to_owned())
        );
        // 所有分词均 ≥3 字符仍走 FTS（回归保障，语义不变）。
        assert_eq!(
            KeywordFilter::from_keyword(Some("foo bar baz")),
            KeywordFilter::Fts("\"foo\"* \"bar\"* \"baz\"*".to_owned())
        );
    }

    #[tokio::test]
    async fn multi_token_short_keyword_falls_back_to_like() {
        let pool = memory_pool().await;
        let mut item = sample_item("short");
        item.content = "ab cd ef".to_owned();
        item.content_hash = content_hash(ClipboardKind::Text, &item.content);
        item.search_text = Some("ab cd ef".to_owned());
        insert_item(&pool, &item).await.unwrap();

        let q = ClipboardItemQuery {
            keyword: Some("ab cd".to_owned()),
            ..Default::default()
        };

        // 旧逻辑按整串字符数（5 ≥ 3）判走 FTS：`"ab"* "cd"*`，两个 token 均 <3 字符，
        // trigram 返回 0 行 → 搜不到刚插入的记录。现按分词长度降级 LIKE `%ab cd%`，命中。
        let found = query_items(&pool, &q).await.unwrap();
        assert_eq!(ids(&found), ["short"]);
    }

    #[test]
    fn escape_like_escapes_special_chars() {
        assert_eq!(escape_like("100%"), "100\\%");
        assert_eq!(escape_like("a_b"), "a\\_b");
        assert_eq!(escape_like("c:\\path"), "c:\\\\path");
    }

    /// 同一连接上的累计行变更数（含触发器与 FTS5 影子表写入）；memory_pool 为单连接。
    async fn total_changes(pool: &SqlitePool) -> i64 {
        sqlx::query_scalar::<_, i64>("SELECT total_changes()")
            .fetch_one(pool)
            .await
            .unwrap()
    }

    /// FTS5 integrity-check；rank = 1 时额外对照外部内容表 clipboard_items 校验索引。
    async fn fts_integrity_check(pool: &SqlitePool) -> sqlx::Result<()> {
        sqlx::query(
            "INSERT INTO clipboard_items_fts(clipboard_items_fts, rank) \
             VALUES('integrity-check', 1)",
        )
        .execute(pool)
        .await
        .map(|_| ())
    }

    /// 先跑 integrity-check，再逐个关键词（均 ≥3 字符，走 FTS）断言命中的条目 id。
    async fn assert_fts_state(
        pool: &SqlitePool,
        step: &str,
        expected: &std::collections::BTreeMap<&str, Vec<&str>>,
    ) {
        if let Err(err) = fts_integrity_check(pool).await {
            panic!("{step}: FTS integrity-check failed: {err}");
        }
        for (&keyword, want) in expected {
            let query = ClipboardItemQuery {
                keyword: Some(keyword.to_owned()),
                ..Default::default()
            };
            let found = query_items(pool, &query).await.unwrap();
            let mut got = ids(&found);
            got.sort_unstable();
            assert_eq!(&got, want, "{step}: keyword {keyword}");
        }
    }

    /// 升级前的旧表还没有后来新增的列，列表查询跑不了；直接用 FTS 表按同样的表达式核对。
    async fn assert_legacy_fts_state(
        pool: &SqlitePool,
        step: &str,
        expected: &std::collections::BTreeMap<&str, Vec<&str>>,
    ) {
        if let Err(err) = fts_integrity_check(pool).await {
            panic!("{step}: FTS integrity-check failed: {err}");
        }
        for (&keyword, want) in expected {
            let expr = build_fts_expr(keyword).unwrap();
            let mut got: Vec<String> = sqlx::query_scalar(
                "SELECT id FROM clipboard_items WHERE rowid IN \
                 (SELECT rowid FROM clipboard_items_fts WHERE clipboard_items_fts MATCH ?)",
            )
            .bind(expr)
            .fetch_all(pool)
            .await
            .unwrap();
            got.sort_unstable();
            assert_eq!(&got, want, "{step}: keyword {keyword}");
        }
    }

    #[tokio::test]
    async fn non_text_updates_do_not_rewrite_fts_index() {
        let pool = memory_pool().await;
        let group = ClipboardGroup {
            id: "g1".to_owned(),
            name: "G1".to_owned(),
            icon: "i-lets-icons:folder".to_owned(),
            is_hidden: false,
            sort_order: 0,
            created_at: DateTime::from_timestamp(1_700_000_000, 0).unwrap(),
            updated_at: DateTime::from_timestamp(1_700_000_000, 0).unwrap(),
        };
        insert_group(&pool, &group).await.unwrap();
        let mut item = sample_item("a");
        item.search_text = Some("searchable text".to_owned());
        item.note = Some("original note".to_owned());
        insert_item(&pool, &item).await.unwrap();

        // 只写非索引列：total_changes 增量应恰好是被更新的 1 行，没有 FTS 删除 / 重建写入。
        let before = total_changes(&pool).await;
        increment_item_use_count(&pool, "a").await.unwrap();
        let delta = total_changes(&pool).await - before;
        assert_eq!(delta, 1, "increment_item_use_count");

        let before = total_changes(&pool).await;
        assert!(toggle_item_favorite(&pool, "a").await.unwrap());
        let delta = total_changes(&pool).await - before;
        assert_eq!(delta, 1, "toggle_item_favorite");

        let before = total_changes(&pool).await;
        mark_item_favorite(&pool, "a").await.unwrap();
        let delta = total_changes(&pool).await - before;
        assert_eq!(delta, 1, "mark_item_favorite");

        let before = total_changes(&pool).await;
        assert!(toggle_item_pinned(&pool, "a").await.unwrap());
        let delta = total_changes(&pool).await - before;
        assert_eq!(delta, 1, "toggle_item_pinned");

        let before = total_changes(&pool).await;
        update_item_group(&pool, "a", Some("g1")).await.unwrap();
        let delta = total_changes(&pool).await - before;
        assert_eq!(delta, 1, "update_item_group");

        // 备注是索引列：仍要先删除旧 FTS 条目再写入新条目。
        let before = total_changes(&pool).await;
        update_item_note(&pool, "a", Some("memo")).await.unwrap();
        let delta = total_changes(&pool).await - before;
        assert!(delta > 1, "update_item_note changed only {delta} rows");
    }

    #[tokio::test]
    async fn fts_index_stays_consistent_across_item_mutations() {
        let pool = memory_pool().await;
        let group = ClipboardGroup {
            id: "g1".to_owned(),
            name: "G1".to_owned(),
            icon: "i-lets-icons:folder".to_owned(),
            is_hidden: false,
            sort_order: 0,
            created_at: DateTime::from_timestamp(1_700_000_000, 0).unwrap(),
            updated_at: DateTime::from_timestamp(1_700_000_000, 0).unwrap(),
        };
        insert_group(&pool, &group).await.unwrap();
        // 检索词互不为子串（trigram 按子串匹配）；created_at 各不相同，按条数清理的结果确定。
        let rows = [
            ("a", "orchid", None),
            ("b", "falcon", Some("harbor")),
            ("c", "cobalt", None),
            ("d", "glacier", None),
            ("e", "meadow", Some("lantern")),
            ("f", "lantern", None),
        ];
        for ((id, search_text, note), secs) in rows.into_iter().zip(1_700_000_001..) {
            let mut item = sample_item(id);
            item.search_text = Some(search_text.to_owned());
            item.note = note.map(str::to_owned);
            item.created_at = DateTime::from_timestamp(secs, 0).unwrap();
            item.updated_at = item.created_at;
            insert_item(&pool, &item).await.unwrap();
        }
        let mut expected = std::collections::BTreeMap::from([
            ("orchid", vec!["a"]),
            ("falcon", vec!["b"]),
            ("harbor", vec!["b"]),
            ("cobalt", vec!["c"]),
            ("glacier", vec!["d"]),
            ("meadow", vec!["e"]),
            ("lantern", vec!["e", "f"]),
            ("walrus", vec![]),
            ("zephyr", vec![]),
        ]);
        assert_fts_state(&pool, "seed", &expected).await;

        // 非索引列更新不改变检索结果。
        increment_item_use_count(&pool, "a").await.unwrap();
        assert_fts_state(&pool, "increment_item_use_count", &expected).await;
        assert!(toggle_item_favorite(&pool, "a").await.unwrap());
        assert_fts_state(&pool, "toggle_item_favorite", &expected).await;
        mark_item_favorite(&pool, "b").await.unwrap();
        assert_fts_state(&pool, "mark_item_favorite", &expected).await;
        assert!(toggle_item_pinned(&pool, "c").await.unwrap());
        assert_fts_state(&pool, "toggle_item_pinned", &expected).await;
        update_item_group(&pool, "d", Some("g1")).await.unwrap();
        assert_fts_state(&pool, "update_item_group", &expected).await;

        update_item_note(&pool, "b", Some("walrus")).await.unwrap();
        expected.insert("harbor", vec![]);
        expected.insert("walrus", vec!["b"]);
        assert_fts_state(&pool, "update_item_note set", &expected).await;

        update_item_note(&pool, "b", None).await.unwrap();
        expected.insert("walrus", vec![]);
        assert_fts_state(&pool, "update_item_note clear", &expected).await;

        // 当前没有生产路径改写 search_text；直接 SQL 覆盖触发器合约。
        sqlx::query("UPDATE clipboard_items SET search_text = ? WHERE id = ?")
            .bind("zephyr")
            .bind("c")
            .execute(&pool)
            .await
            .unwrap();
        expected.insert("cobalt", vec![]);
        expected.insert("zephyr", vec!["c"]);
        assert_fts_state(&pool, "raw search_text update", &expected).await;

        assert_eq!(delete_item(&pool, "d").await.unwrap(), None);
        expected.insert("glacier", vec![]);
        assert_fts_state(&pool, "delete_item", &expected).await;

        // a、b 已收藏，c 已置顶，e 有备注；自动清理只删普通记录 f。
        let everything = crate::db::retention::AgePlan {
            rules: Vec::new(),
            fallback_cutoff: Some(DateTime::from_timestamp(1_900_000_000, 0).unwrap()),
        };
        let cleanup =
            crate::db::retention::delete_expired(&mut pool.acquire().await.unwrap(), &everything)
                .await
                .unwrap();
        assert_eq!(cleanup.removed, 1);
        expected.insert("lantern", vec!["e"]);
        assert_fts_state(&pool, "retention cleanup", &expected).await;

        assert_eq!(clear_items(&pool, false, false).await.unwrap().removed, 1);
        expected.insert("meadow", vec![]);
        expected.insert("lantern", vec![]);
        assert_fts_state(&pool, "clear_items keep favorite/pinned", &expected).await;

        assert_eq!(clear_items(&pool, true, true).await.unwrap().removed, 3);
        expected.insert("orchid", vec![]);
        expected.insert("falcon", vec![]);
        expected.insert("zephyr", vec![]);
        assert_fts_state(&pool, "clear_items all", &expected).await;
    }

    #[tokio::test]
    async fn fts_update_trigger_upgrade_keeps_index_consistent() {
        let migrations = tempfile::tempdir().unwrap();
        std::fs::write(
            migrations.path().join("0001_init.sql"),
            include_str!("../../migrations/0001_init.sql"),
        )
        .unwrap();
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate::Migrator::new(migrations.path())
            .await
            .unwrap()
            .run(&pool)
            .await
            .unwrap();

        // 旧库：在 AFTER UPDATE 触发器下写入、改写备注并更新非索引列。
        // 旧表结构还没有 last_used_at，写入与计数只能按当时的列来。
        for (id, search_text, note) in [("a", "quartz", Some("amber")), ("b", "velvet", None)] {
            sqlx::query(
                "INSERT INTO clipboard_items \
                 (id, kind, content, content_hash, search_text, use_count, platform, note, \
                  created_at, updated_at) \
                 VALUES (?, 'text', ?, ?, ?, 1, 'macos', ?, ?, ?)",
            )
            .bind(id)
            .bind(format!("content-{id}"))
            .bind(id)
            .bind(search_text)
            .bind(note)
            .bind("2026-01-01T00:00:00+00:00")
            .bind(format!(
                "2026-01-0{}T00:00:00+00:00",
                if id == "a" { 2 } else { 3 }
            ))
            .execute(&pool)
            .await
            .unwrap();
        }
        update_item_note(&pool, "a", Some("tundra")).await.unwrap();
        update_item_note(&pool, "b", Some("amber")).await.unwrap();
        sqlx::query("UPDATE clipboard_items SET use_count = use_count + 1 WHERE id = 'a'")
            .execute(&pool)
            .await
            .unwrap();
        // 旧 schema 还没有 favorite_order；这里必须用旧库可执行的原始 SQL，
        // 而不是调用依赖 0007 列的现行 toggle_item_favorite。
        sqlx::query("UPDATE clipboard_items SET is_favorite = NOT is_favorite WHERE id = 'b'")
            .execute(&pool)
            .await
            .unwrap();
        let mut expected = std::collections::BTreeMap::from([
            ("quartz", vec!["a"]),
            ("tundra", vec!["a"]),
            ("velvet", vec!["b"]),
            ("amber", vec!["b"]),
            ("zircon", vec![]),
        ]);
        assert_legacy_fts_state(&pool, "before upgrade", &expected).await;

        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        let trigger: String = sqlx::query_scalar(
            "SELECT sql FROM sqlite_master \
             WHERE type = 'trigger' AND name = 'clipboard_items_au'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(
            trigger.contains("AFTER UPDATE OF search_text, note ON clipboard_items"),
            "clipboard_items_au is not limited to indexed columns: {trigger}"
        );
        // 旧触发器维护的索引本来一致，升级无需 rebuild，检索结果不变。
        assert_fts_state(&pool, "after upgrade", &expected).await;
        // 升级前的记录以 updated_at 作为最后使用时间。
        let last_used: Vec<(String, String)> =
            sqlx::query_as("SELECT id, last_used_at FROM clipboard_items ORDER BY id")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(
            last_used,
            [
                ("a".to_owned(), "2026-01-02T00:00:00+00:00".to_owned()),
                ("b".to_owned(), "2026-01-03T00:00:00+00:00".to_owned()),
            ]
        );

        // 升级后继续变更：非索引列只改 1 行，备注、search_text 与删除仍保持一致。
        let before = total_changes(&pool).await;
        increment_item_use_count(&pool, "b").await.unwrap();
        let delta = total_changes(&pool).await - before;
        assert_eq!(delta, 1, "increment_item_use_count after upgrade");
        assert_fts_state(&pool, "upgraded increment_item_use_count", &expected).await;

        update_item_note(&pool, "a", None).await.unwrap();
        expected.insert("tundra", vec![]);
        assert_fts_state(&pool, "upgraded update_item_note clear", &expected).await;

        sqlx::query("UPDATE clipboard_items SET search_text = ? WHERE id = ?")
            .bind("zircon")
            .bind("b")
            .execute(&pool)
            .await
            .unwrap();
        expected.insert("velvet", vec![]);
        expected.insert("zircon", vec!["b"]);
        assert_fts_state(&pool, "upgraded raw search_text update", &expected).await;

        assert_eq!(delete_item(&pool, "a").await.unwrap(), None);
        expected.insert("quartz", vec![]);
        assert_fts_state(&pool, "upgraded delete_item", &expected).await;
    }

    #[tokio::test]
    async fn fts_integrity_check_detects_unsynced_index() {
        let pool = memory_pool().await;
        let mut item = sample_item("a");
        item.search_text = Some("original words".to_owned());
        insert_item(&pool, &item).await.unwrap();
        fts_integrity_check(&pool).await.unwrap();

        // 负向对照：去掉更新触发器后改写 search_text，索引与内容表不再一致，检查必须报错。
        sqlx::query("DROP TRIGGER clipboard_items_au")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("UPDATE clipboard_items SET search_text = ? WHERE id = ?")
            .bind("replaced words")
            .bind("a")
            .execute(&pool)
            .await
            .unwrap();

        let err = fts_integrity_check(&pool).await.unwrap_err();
        assert!(matches!(err, sqlx::Error::Database(_)), "{err}");
    }

    /// 返回 `EXPLAIN QUERY PLAN` 的 detail 列，逐行一个计划节点。
    async fn query_plan(pool: &SqlitePool, q: &ClipboardItemQuery) -> Vec<String> {
        let mut qb: QueryBuilder<Sqlite> = QueryBuilder::new("EXPLAIN QUERY PLAN ");
        push_list_query(&mut qb, q, &KeywordFilter::None);
        let rows: Vec<(i64, i64, i64, String)> = qb.build_query_as().fetch_all(pool).await.unwrap();
        rows.into_iter().map(|row| row.3).collect()
    }

    #[tokio::test]
    async fn ocr_query_plan_uses_fts_and_unified_predicate_on_large_history() {
        let pool = memory_pool().await;
        sqlx::query("WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<20000) INSERT INTO clipboard_items(id,kind,content,content_hash,platform,created_at,updated_at) SELECT printf('plan-%d',x),CASE WHEN x%4=0 THEN 'image' ELSE 'text' END, 'fixture.png',printf('hash-%d',x),'windows','2026-10-09T00:00:00Z','2026-10-09T00:00:00Z' FROM n")
            .execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO image_texts(item_id,status,text,attempts,created_at,updated_at) SELECT id,'done','中文识别 English words',1,created_at,created_at FROM clipboard_items WHERE kind='image'")
            .execute(&pool).await.unwrap();
        let q = ClipboardItemQuery {
            ocr_enabled: true,
            keyword: Some("English".into()),
            ..Default::default()
        };
        let mut qb = QueryBuilder::<Sqlite>::new("EXPLAIN QUERY PLAN ");
        push_list_query(
            &mut qb,
            &q,
            &KeywordFilter::from_keyword(q.keyword.as_deref()),
        );
        let rows: Vec<(i64, i64, i64, String)> =
            qb.build_query_as().fetch_all(&pool).await.unwrap();
        let plan = rows
            .iter()
            .map(|row| row.3.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        println!("OCR 20k/5k query plan:\n{plan}");
        assert!(plan.contains("VIRTUAL TABLE INDEX"), "{plan}");
        assert!(!plan.contains("CORRELATED"), "{plan}");
        let (rows, total) = query_items_page(&pool, &q).await.unwrap();
        assert_eq!(rows.len(), 20);
        assert_eq!(total, 5000);
        assert_eq!(list_item_refs(&pool, &q).await.unwrap().len(), 5000);
    }

    #[tokio::test]
    async fn list_queries_walk_sort_indexes() {
        let pool = memory_pool().await;
        let cases = [
            (
                "all",
                ClipboardItemQuery {
                    group: Some(ClipboardGroupFilter::All),
                    ..ClipboardItemQuery::default()
                },
                "idx_clipboard_items_pinned_updated_manual",
            ),
            (
                "created sort",
                ClipboardItemQuery {
                    group: Some(ClipboardGroupFilter::All),
                    sort: ClipboardItemSort::CreatedAt,
                    ..ClipboardItemQuery::default()
                },
                "idx_clipboard_items_pinned_created_manual",
            ),
            // 图片分组还收单个图片文件，跨两种 kind，排序必须走临时 B 树；用文本分组检查 kind 索引。
            (
                "text tab",
                ClipboardItemQuery {
                    group: Some(ClipboardGroupFilter::Text),
                    ..ClipboardItemQuery::default()
                },
                "idx_clipboard_items_kind_pinned_updated_manual",
            ),
            (
                "custom group",
                ClipboardItemQuery {
                    group: Some(ClipboardGroupFilter::All),
                    group_id: Some("g1".to_owned()),
                    ..ClipboardItemQuery::default()
                },
                "idx_clipboard_items_group_pinned_updated_manual",
            ),
            (
                "favorite tab",
                ClipboardItemQuery {
                    group: Some(ClipboardGroupFilter::Favorite),
                    ..ClipboardItemQuery::default()
                },
                "idx_clipboard_items_favorite_manual_updated",
            ),
        ];

        for (label, q, index) in cases {
            let plan = query_plan(&pool, &q).await;
            let uses_expected_index = plan.iter().any(|step| {
                let mut previous = "";
                step.split_whitespace().any(|token| {
                    let matched = previous == "INDEX" && token == index;
                    previous = token;
                    matched
                })
            });
            assert!(
                uses_expected_index,
                "{label}: expected {index}, got {plan:?}"
            );
            assert!(
                !plan.iter().any(|step| step.contains("TEMP B-TREE")),
                "{label}: list query sorts in a temp b-tree: {plan:?}"
            );
        }
    }
}
