//! 偏好页「数据概览」的聚合查询：按内容类别、来源应用、分组和日期统计历史记录，
//! 以及按同一套范围批量清理。只读统计不改动记录；清理时收藏与置顶始终保留。

use std::collections::HashMap;

use anyhow::Context;
use chrono::{DateTime, Days, NaiveDate, NaiveTime, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{QueryBuilder, Sqlite, SqlitePool};

use crate::core::Result;
use crate::db::items::{absorb_deleted, CleanupOutcome};
use crate::db::models::{ClipboardKind, ClipboardSubKind};

/// 来源应用排行只回传前几名，其余合并成一行，偏好页不需要全量列表。
const TOP_SOURCE_APPS: usize = 6;

/// 采集趋势覆盖的天数（含今天）。
pub const DAILY_TREND_DAYS: u32 = 30;

/// 数据概览里的内容类别：文本按识别出的子类型拆开，图片、文件各成一类。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ContentCategory {
    Text,
    Html,
    Rtf,
    Url,
    Email,
    Color,
    Path,
    Image,
    Files,
}

impl ContentCategory {
    pub const ALL: [Self; 9] = [
        Self::Text,
        Self::Html,
        Self::Rtf,
        Self::Url,
        Self::Email,
        Self::Color,
        Self::Path,
        Self::Image,
        Self::Files,
    ];

    fn from_kind(kind: ClipboardKind, sub_kind: Option<ClipboardSubKind>) -> Self {
        match (kind, sub_kind) {
            (ClipboardKind::Image, _) => Self::Image,
            (ClipboardKind::Files, _) => Self::Files,
            (ClipboardKind::Text, None) => Self::Text,
            (ClipboardKind::Text, Some(ClipboardSubKind::Html)) => Self::Html,
            (ClipboardKind::Text, Some(ClipboardSubKind::Rtf)) => Self::Rtf,
            (ClipboardKind::Text, Some(ClipboardSubKind::Url)) => Self::Url,
            (ClipboardKind::Text, Some(ClipboardSubKind::Email)) => Self::Email,
            (ClipboardKind::Text, Some(ClipboardSubKind::Color)) => Self::Color,
            (ClipboardKind::Text, Some(ClipboardSubKind::Path)) => Self::Path,
        }
    }

    /// 匹配该类别的 SQL 条件，与 [`Self::from_kind`] 的归类一一对应。
    fn sql_condition(self) -> &'static str {
        match self {
            Self::Text => "kind = 'text' AND sub_kind IS NULL",
            Self::Html => "kind = 'text' AND sub_kind = 'html'",
            Self::Rtf => "kind = 'text' AND sub_kind = 'rtf'",
            Self::Url => "kind = 'text' AND sub_kind = 'url'",
            Self::Email => "kind = 'text' AND sub_kind = 'email'",
            Self::Color => "kind = 'text' AND sub_kind = 'color'",
            Self::Path => "kind = 'text' AND sub_kind = 'path'",
            Self::Image => "kind = 'image'",
            Self::Files => "kind = 'files'",
        }
    }
}

/// 批量清理的范围；收藏与置顶记录不在任何范围内。
#[derive(Debug, Clone, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ClearScope {
    Category {
        category: ContentCategory,
    },
    /// `app_id = None` 表示没有记录到来源应用的条目。
    SourceApp {
        app_id: Option<String>,
    },
}

/// 全部历史记录的计数汇总。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ItemTotals {
    pub total: u64,
    pub favorites: u64,
    pub pinned: u64,
    pub noted: u64,
    pub sensitive: u64,
    pub grouped: u64,
    /// 去重命中与复制 / 粘贴复用累计的次数，即 `use_count` 超出首次采集的部分。
    pub reuses: u64,
}

/// 单个内容类别的条数与内容大小。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CategoryStat {
    pub category: ContentCategory,
    pub count: u64,
    /// 文本按入库内容的 UTF-8 字节、图片按原图字节、文件按路径串字节计。
    pub bytes: u64,
    /// 不是收藏也不是置顶、可以被批量清理的条数。
    pub removable: u64,
}

/// 单个来源应用的记录数；`app_id = None` 汇总未取到来源应用的记录。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceAppStat {
    pub app_id: Option<String>,
    pub name: Option<String>,
    #[serde(skip)]
    pub icon_file: Option<String>,
    /// 命令层用 `AppIconStore` 解析出的图标绝对路径。
    pub icon_path: Option<String>,
    pub count: u64,
    pub bytes: u64,
    pub removable: u64,
}

/// 排行之外其余来源应用的合计。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OtherSourceApps {
    pub apps: u64,
    pub count: u64,
}

/// 单个自定义分组的记录数。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GroupStat {
    pub id: String,
    pub name: String,
    pub icon: String,
    pub is_hidden: bool,
    pub count: u64,
}

/// 某个本地日期新采集的记录数。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DailyCount {
    pub date: NaiveDate,
    pub count: u64,
}

/// 历史记录维度的数据概览。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryOverview {
    pub totals: ItemTotals,
    /// 固定按 [`ContentCategory::ALL`] 顺序，包含条数为 0 的类别。
    pub categories: Vec<CategoryStat>,
    /// 最近 [`DAILY_TREND_DAYS`] 天，按日期升序、缺失日期补 0，最后一项是今天。
    pub daily: Vec<DailyCount>,
    pub source_apps: Vec<SourceAppStat>,
    pub other_source_apps: OtherSourceApps,
    pub groups: Vec<GroupStat>,
    /// 最早一条记录的本地日期；没有记录时为 `None`。
    pub oldest_date: Option<NaiveDate>,
}

/// 汇总历史记录的全部统计维度；日期按 `tz` 所在时区的自然日划分。
pub async fn load_history_overview<Tz: TimeZone>(
    pool: &SqlitePool,
    now: DateTime<Utc>,
    tz: &Tz,
) -> Result<HistoryOverview> {
    let today = now.with_timezone(tz).date_naive();
    let (totals, oldest) = load_totals(pool).await?;
    let (source_apps, other_source_apps) = load_source_apps(pool).await?;

    Ok(HistoryOverview {
        totals,
        categories: load_categories(pool).await?,
        daily: load_daily(pool, today, DAILY_TREND_DAYS, tz).await?,
        source_apps,
        other_source_apps,
        groups: load_groups(pool).await?,
        oldest_date: oldest.map(|created_at| created_at.with_timezone(tz).date_naive()),
    })
}

/// 清理指定范围内的普通记录，返回删除行数与被删图片文件名。
pub async fn clear_scope(pool: &SqlitePool, scope: &ClearScope) -> Result<CleanupOutcome> {
    let mut qb: QueryBuilder<Sqlite> = QueryBuilder::new(
        "DELETE FROM clipboard_items WHERE is_favorite = 0 AND is_pinned = 0 AND ",
    );
    match scope {
        ClearScope::Category { category } => {
            qb.push(category.sql_condition());
        }
        ClearScope::SourceApp {
            app_id: Some(app_id),
        } => {
            qb.push("source_app_id = ").push_bind(app_id.clone());
        }
        ClearScope::SourceApp { app_id: None } => {
            qb.push("source_app_id IS NULL");
        }
    }
    qb.push(" RETURNING kind, content");

    let rows = qb
        .build_query_as::<(ClipboardKind, String)>()
        .fetch_all(pool)
        .await
        .context("failed to clear clipboard items in scope")?;

    let mut outcome = CleanupOutcome::default();
    absorb_deleted(&mut outcome, rows);
    Ok(outcome)
}

async fn load_totals(pool: &SqlitePool) -> Result<(ItemTotals, Option<DateTime<Utc>>)> {
    let (total, favorites, pinned, noted, sensitive, grouped, reuses, oldest): (
        i64,
        i64,
        i64,
        i64,
        i64,
        i64,
        i64,
        Option<String>,
    ) = sqlx::query_as(
        "SELECT COUNT(*), \
             COALESCE(SUM(is_favorite), 0), \
             COALESCE(SUM(is_pinned), 0), \
             COALESCE(SUM(CASE WHEN note IS NOT NULL AND note <> '' THEN 1 ELSE 0 END), 0), \
             COALESCE(SUM(is_sensitive), 0), \
             COALESCE(SUM(CASE WHEN group_id IS NOT NULL THEN 1 ELSE 0 END), 0), \
             COALESCE(SUM(MAX(use_count - 1, 0)), 0), \
             MIN(created_at) \
         FROM clipboard_items",
    )
    .fetch_one(pool)
    .await
    .context("failed to count clipboard items")?;

    let totals = ItemTotals {
        total: non_negative(total),
        favorites: non_negative(favorites),
        pinned: non_negative(pinned),
        noted: non_negative(noted),
        sensitive: non_negative(sensitive),
        grouped: non_negative(grouped),
        reuses: non_negative(reuses),
    };
    let oldest = oldest
        .as_deref()
        .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
        .map(|value| value.with_timezone(&Utc));

    Ok((totals, oldest))
}

async fn load_categories(pool: &SqlitePool) -> Result<Vec<CategoryStat>> {
    // 文本与图片入库时都写了 size；只有文件记录没有，才退回路径串的字节数。
    let rows: Vec<(ClipboardKind, Option<ClipboardSubKind>, i64, i64, i64)> = sqlx::query_as(
        "SELECT kind, sub_kind, COUNT(*), \
             COALESCE(SUM(COALESCE(size, length(CAST(content AS BLOB)))), 0), \
             COALESCE(SUM(CASE WHEN is_favorite = 0 AND is_pinned = 0 THEN 1 ELSE 0 END), 0) \
         FROM clipboard_items \
         GROUP BY kind, sub_kind",
    )
    .fetch_all(pool)
    .await
    .context("failed to group clipboard items by category")?;

    let mut by_category: HashMap<ContentCategory, CategoryStat> = HashMap::new();
    for (kind, sub_kind, count, bytes, removable) in rows {
        let category = ContentCategory::from_kind(kind, sub_kind);
        let stat = by_category
            .entry(category)
            .or_insert_with(|| empty_category(category));
        stat.count += non_negative(count);
        stat.bytes += non_negative(bytes);
        stat.removable += non_negative(removable);
    }

    Ok(ContentCategory::ALL
        .iter()
        .map(|category| {
            by_category
                .remove(category)
                .unwrap_or_else(|| empty_category(*category))
        })
        .collect())
}

fn empty_category(category: ContentCategory) -> CategoryStat {
    CategoryStat {
        category,
        count: 0,
        bytes: 0,
        removable: 0,
    }
}

/// 来源应用分组查询的一行：应用 id、名称、图标文件名、条数、内容字节、可清理条数。
type SourceAppRow = (
    Option<String>,
    Option<String>,
    Option<String>,
    i64,
    i64,
    i64,
);

async fn load_source_apps(pool: &SqlitePool) -> Result<(Vec<SourceAppStat>, OtherSourceApps)> {
    let rows: Vec<SourceAppRow> = sqlx::query_as(
        "SELECT clipboard_items.source_app_id, clipboard_apps.name, clipboard_apps.icon_file, \
                 COUNT(*), \
                 COALESCE(SUM(COALESCE(clipboard_items.size, 0)), 0), \
                 COALESCE(SUM(CASE WHEN clipboard_items.is_favorite = 0 \
                     AND clipboard_items.is_pinned = 0 THEN 1 ELSE 0 END), 0) \
             FROM clipboard_items \
             LEFT JOIN clipboard_apps ON clipboard_apps.id = clipboard_items.source_app_id \
             GROUP BY clipboard_items.source_app_id \
             ORDER BY COUNT(*) DESC, clipboard_apps.name COLLATE NOCASE ASC",
    )
    .fetch_all(pool)
    .await
    .context("failed to group clipboard items by source app")?;

    let mut top = Vec::with_capacity(TOP_SOURCE_APPS.min(rows.len()));
    let mut others = OtherSourceApps::default();
    for (app_id, name, icon_file, count, bytes, removable) in rows {
        if top.len() < TOP_SOURCE_APPS {
            top.push(SourceAppStat {
                app_id,
                name,
                icon_file,
                icon_path: None,
                count: non_negative(count),
                bytes: non_negative(bytes),
                removable: non_negative(removable),
            });
            continue;
        }

        others.apps += 1;
        others.count += non_negative(count);
    }

    Ok((top, others))
}

async fn load_groups(pool: &SqlitePool) -> Result<Vec<GroupStat>> {
    let rows: Vec<(String, String, String, bool, i64)> = sqlx::query_as(
        "SELECT clipboard_groups.id, clipboard_groups.name, clipboard_groups.icon, \
             clipboard_groups.is_hidden, COUNT(clipboard_items.id) \
         FROM clipboard_groups \
         LEFT JOIN clipboard_items ON clipboard_items.group_id = clipboard_groups.id \
         GROUP BY clipboard_groups.id \
         ORDER BY clipboard_groups.sort_order ASC, clipboard_groups.created_at ASC",
    )
    .fetch_all(pool)
    .await
    .context("failed to count clipboard items by group")?;

    Ok(rows
        .into_iter()
        .map(|(id, name, icon, is_hidden, count)| GroupStat {
            id,
            name,
            icon,
            is_hidden,
            count: non_negative(count),
        })
        .collect())
}

async fn load_daily<Tz: TimeZone>(
    pool: &SqlitePool,
    today: NaiveDate,
    days: u32,
    tz: &Tz,
) -> Result<Vec<DailyCount>> {
    let first_day = today - Days::new(u64::from(days.saturating_sub(1)));
    // 任意时区的本地零点都晚于前一天的 UTC 零点，多取的几小时在分桶时丢掉，省去处理夏令时的本地零点。
    let since = Utc.from_utc_datetime(&(first_day - Days::new(1)).and_time(NaiveTime::MIN));
    let created: Vec<DateTime<Utc>> =
        sqlx::query_scalar("SELECT created_at FROM clipboard_items WHERE created_at >= ?")
            .bind(since)
            .fetch_all(pool)
            .await
            .context("failed to list recent clipboard item timestamps")?;

    Ok(bucket_daily(&created, first_day, days, tz))
}

/// 按 `tz` 的自然日把采集时间分桶，返回从 `first_day` 起连续 `days` 天的计数。
fn bucket_daily<Tz: TimeZone>(
    created: &[DateTime<Utc>],
    first_day: NaiveDate,
    days: u32,
    tz: &Tz,
) -> Vec<DailyCount> {
    let mut counts = vec![0_u64; days as usize];
    for created_at in created {
        let day = created_at.with_timezone(tz).date_naive();
        let Ok(offset) = usize::try_from((day - first_day).num_days()) else {
            continue;
        };
        if let Some(count) = counts.get_mut(offset) {
            *count += 1;
        }
    }

    counts
        .into_iter()
        .zip(first_day.iter_days())
        .map(|(count, date)| DailyCount { date, count })
        .collect()
}

fn non_negative(value: i64) -> u64 {
    value.max(0) as u64
}

#[cfg(test)]
mod tests {
    use chrono::FixedOffset;

    use super::*;
    use crate::db::test_support::memory_pool;

    struct Row<'a> {
        id: &'a str,
        kind: &'a str,
        sub_kind: Option<&'a str>,
        size: Option<i64>,
        source_app_id: Option<&'a str>,
        is_favorite: bool,
        use_count: i64,
        created_at: &'a str,
    }

    impl<'a> Row<'a> {
        fn text(id: &'a str) -> Self {
            Self {
                id,
                kind: "text",
                sub_kind: None,
                size: Some(10),
                source_app_id: None,
                is_favorite: false,
                use_count: 1,
                created_at: "2026-09-20T08:00:00+00:00",
            }
        }
    }

    async fn insert(pool: &SqlitePool, row: Row<'_>) {
        sqlx::query(
            "INSERT INTO clipboard_items \
                 (id, kind, sub_kind, source_app_id, content, content_hash, size, use_count, \
                  is_favorite, platform, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, 'windows', ?, ?)",
        )
        .bind(row.id)
        .bind(row.kind)
        .bind(row.sub_kind)
        .bind(row.source_app_id)
        .bind(format!("{}.png", row.id))
        .bind(row.id)
        .bind(row.size)
        .bind(row.use_count)
        .bind(row.is_favorite)
        .bind(row.created_at)
        .bind(row.created_at)
        .execute(pool)
        .await
        .unwrap();
    }

    async fn insert_app(pool: &SqlitePool, id: &str, name: &str) {
        sqlx::query(
            "INSERT INTO clipboard_apps (id, name, icon_file, platform, created_at, updated_at) \
             VALUES (?, ?, NULL, 'windows', '2026-01-01T00:00:00+00:00', '2026-01-01T00:00:00+00:00')",
        )
        .bind(id)
        .bind(name)
        .execute(pool)
        .await
        .unwrap();
    }

    fn category(overview: &HistoryOverview, category: ContentCategory) -> &CategoryStat {
        overview
            .categories
            .iter()
            .find(|stat| stat.category == category)
            .unwrap()
    }

    fn date(value: &str) -> NaiveDate {
        NaiveDate::parse_from_str(value, "%Y-%m-%d").unwrap()
    }

    #[tokio::test]
    async fn overview_splits_categories_and_counts_reuses() {
        let pool = memory_pool().await;
        insert(&pool, Row::text("plain")).await;
        insert(
            &pool,
            Row {
                sub_kind: Some("url"),
                use_count: 4,
                ..Row::text("link")
            },
        )
        .await;
        insert(
            &pool,
            Row {
                kind: "image",
                size: Some(2048),
                is_favorite: true,
                created_at: "2026-09-01T00:00:00+00:00",
                ..Row::text("img")
            },
        )
        .await;
        insert(
            &pool,
            Row {
                kind: "files",
                size: None,
                ..Row::text("f")
            },
        )
        .await;

        let now = DateTime::parse_from_rfc3339("2026-09-25T10:00:00+00:00")
            .unwrap()
            .with_timezone(&Utc);
        let overview = load_history_overview(&pool, now, &Utc).await.unwrap();

        assert_eq!(overview.totals.total, 4);
        assert_eq!(overview.totals.favorites, 1);
        assert_eq!(overview.totals.reuses, 3);
        assert_eq!(overview.oldest_date, Some(date("2026-09-01")));
        assert_eq!(overview.categories.len(), ContentCategory::ALL.len());
        assert_eq!(category(&overview, ContentCategory::Text).count, 1);
        assert_eq!(category(&overview, ContentCategory::Url).count, 1);
        assert_eq!(category(&overview, ContentCategory::Html).count, 0);

        let image = category(&overview, ContentCategory::Image);
        assert_eq!((image.count, image.bytes, image.removable), (1, 2048, 0));
        // 文件记录没有 size，按路径串 "f.png" 的字节数计。
        assert_eq!(category(&overview, ContentCategory::Files).bytes, 5);
    }

    #[tokio::test]
    async fn overview_ranks_source_apps_and_folds_the_rest() {
        let pool = memory_pool().await;
        for index in 0..8 {
            let app_id = format!("app-{index}");
            insert_app(&pool, &app_id, &format!("App {index}")).await;
            for copy in 0..=index {
                let id = format!("{app_id}-{copy}");
                insert(
                    &pool,
                    Row {
                        source_app_id: Some(&app_id),
                        ..Row::text(&id)
                    },
                )
                .await;
            }
        }
        insert(&pool, Row::text("unknown")).await;

        let now = Utc::now();
        let overview = load_history_overview(&pool, now, &Utc).await.unwrap();

        assert_eq!(overview.source_apps.len(), TOP_SOURCE_APPS);
        assert_eq!(overview.source_apps[0].app_id.as_deref(), Some("app-7"));
        assert_eq!(overview.source_apps[0].name.as_deref(), Some("App 7"));
        assert_eq!(overview.source_apps[0].count, 8);
        // 前六名是 app-7..app-2，剩下 app-1（2 条）、app-0（1 条）和未知来源（1 条）。
        assert_eq!(
            overview.other_source_apps,
            OtherSourceApps { apps: 3, count: 4 }
        );
    }

    #[tokio::test]
    async fn clear_scope_keeps_favorites_and_pinned() {
        let pool = memory_pool().await;
        insert(&pool, Row::text("plain")).await;
        insert(
            &pool,
            Row {
                is_favorite: true,
                ..Row::text("kept")
            },
        )
        .await;
        insert(
            &pool,
            Row {
                kind: "image",
                ..Row::text("img")
            },
        )
        .await;

        let outcome = clear_scope(
            &pool,
            &ClearScope::Category {
                category: ContentCategory::Text,
            },
        )
        .await
        .unwrap();
        assert_eq!(outcome.removed, 1);
        assert!(outcome.image_files.is_empty());

        let outcome = clear_scope(&pool, &ClearScope::SourceApp { app_id: None })
            .await
            .unwrap();
        assert_eq!(outcome.removed, 1);
        assert_eq!(outcome.image_files, vec!["img.png".to_owned()]);

        let left: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM clipboard_items")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(left, 1);
    }

    #[test]
    fn clear_scope_deserializes_from_frontend_shape() {
        let scope: ClearScope =
            serde_json::from_str(r#"{"type":"category","category":"image"}"#).unwrap();
        assert!(matches!(
            scope,
            ClearScope::Category {
                category: ContentCategory::Image
            }
        ));

        let scope: ClearScope =
            serde_json::from_str(r#"{"type":"sourceApp","appId":"C:\\a.exe"}"#).unwrap();
        assert!(
            matches!(scope, ClearScope::SourceApp { app_id: Some(ref id) } if id == "C:\\a.exe")
        );
    }

    #[test]
    fn bucket_daily_uses_local_calendar_days() {
        let tz = FixedOffset::east_opt(8 * 3600).unwrap();
        let at = |value: &str| {
            DateTime::parse_from_rfc3339(value)
                .unwrap()
                .with_timezone(&Utc)
        };
        let created = [
            // UTC 9/22 16:30 在东八区已是 9/23。
            at("2026-09-22T16:30:00+00:00"),
            at("2026-09-23T01:00:00+00:00"),
            at("2026-09-25T15:59:00+00:00"),
            // 早于统计窗口，丢弃。
            at("2026-09-21T15:00:00+00:00"),
        ];

        let daily = bucket_daily(&created, date("2026-09-23"), 3, &tz);

        assert_eq!(
            daily,
            vec![
                DailyCount {
                    date: date("2026-09-23"),
                    count: 2
                },
                DailyCount {
                    date: date("2026-09-24"),
                    count: 0
                },
                DailyCount {
                    date: date("2026-09-25"),
                    count: 1
                },
            ]
        );
    }
}
