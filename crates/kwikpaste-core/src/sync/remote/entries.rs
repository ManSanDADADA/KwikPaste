//! 记录与元数据条目（REMOTE.md §6.1–§6.3）：本机记录转成上传的帧，收到的条目按后写者赢入库。

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Context;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::SqlitePool;

use super::crypto::{hlc_newer, pack, parse_hlc, unpack, GroupKeys};
use super::image::{self, Compress};
use super::RemotePolicy;
use crate::db::items::{set_item_favorite, set_item_pinned, update_item_note};
use crate::db::models::{ClipboardItem, ClipboardKind, Platform};
use crate::db::remote::{self as state, RemoteStateRow};
use crate::root::CoreInner;
use crate::settings::RemoteImageQuality;
use crate::sync::engine::{self, EchoFilter, ReceiveOptions};
use crate::sync::protocol::WireItem;

pub(super) const ITEM: &str = "item";
pub(super) const META: &str = "meta";
pub(super) const FAVORITE: &str = "favorite";
pub(super) const PINNED: &str = "pinned";
pub(super) const NOTE: &str = "note";
pub(super) const DELETED: &str = "deleted";
/// 本机复制在这段时间内发出去才算实时条目；离线时攒下的晚到了就按补齐处理，不写对方剪贴板。
const LIVE_WINDOW: Duration = Duration::from_secs(60);

/// 记录条目的内容。
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ItemEntry {
    pub item: WireItem,
    pub item_key: String,
    pub captured_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<ImageInfo>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct ImageInfo {
    pub format: String,
    pub width: u32,
    pub height: u32,
}

/// 元数据条目的内容：一个字段一个条目。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct MetaEntry {
    pub item_key: String,
    pub field: String,
    pub value: Value,
}

/// 一个待上传的帧。帧头里的 `live` 在发出时才定（见 [`LIVE_WINDOW`]）。
pub(super) struct Outgoing {
    pub entry_id: String,
    pub kind: &'static str,
    pub class: &'static str,
    pub hlc: String,
    /// 实时条目的产生时刻；补齐与历史上传为 `None`，帧头 `live = false`。
    pub live_since: Option<Instant>,
    pub skip: Vec<String>,
    pub body: Vec<u8>,
}

impl Outgoing {
    pub fn header(&self) -> Vec<u8> {
        let live = self
            .live_since
            .is_some_and(|since| since.elapsed() < LIVE_WINDOW);
        json!({
            "entryId": self.entry_id,
            "kind": self.kind,
            "class": self.class,
            "hlc": self.hlc,
            "live": live,
            "skip": self.skip,
        })
        .to_string()
        .into_bytes()
    }
}

/// 图片怎么发：服务器下发了压缩参数就必须照做，否则按用户选的档（「原图」直接发 PNG）。
/// 单张上限是服务器的上限，用户自选档时再取用户设的上限。
fn image_plan(
    policy: &RemotePolicy,
    quality: RemoteImageQuality,
    user_max: u64,
) -> (Option<Compress>, u64) {
    match &policy.image_compress {
        Some(compress) => (
            Some(Compress {
                max_side: compress.max_side,
                quality: compress.quality,
            }),
            policy.image_max_bytes,
        ),
        None => (
            match quality {
                RemoteImageQuality::Original => None,
                RemoteImageQuality::High => Some(image::HIGH),
                RemoteImageQuality::Saver => Some(image::SAVER),
            },
            policy.image_max_bytes.min(user_max),
        ),
    }
}

/// 本机记录转成记录条目的帧。超过服务器上限（按密文算）或原图读不到时返回 `None`。
/// `track`（服务器存储历史）时记下这条记录的 HLC：比它旧的删除不再删掉它；服务器收下之前它是待发的，
/// 没收下（断网、退出）就由补齐后的历史上传重传。
#[allow(clippy::too_many_arguments)]
pub(super) async fn item_frame(
    core: &Arc<CoreInner>,
    keys: &GroupKeys,
    policy: &RemotePolicy,
    item: &ClipboardItem,
    hlc: String,
    live_since: Option<Instant>,
    skip: Vec<String>,
    track: bool,
) -> anyhow::Result<Option<Outgoing>> {
    let remote = core.settings.snapshot().sync.remote;
    let (compress, image_limit) =
        image_plan(policy, remote.image_quality, remote.max_image_bytes());
    let Some((mut wire, mut bytes)) = engine::wire_from_item(core, item, u64::MAX).await? else {
        return Ok(None);
    };

    let image = if item.kind == ClipboardKind::Image {
        let mut info = ImageInfo {
            format: "png".to_owned(),
            width: wire.width.unwrap_or_default().max(0) as u32,
            height: wire.height.unwrap_or_default().max(0) as u32,
        };
        if let Some(params) = compress {
            let compressed = tokio::task::spawn_blocking(move || image::compress(&bytes, params))
                .await
                .context("image compress task failed")??;
            bytes = compressed.bytes;
            info = ImageInfo {
                format: "webp".to_owned(),
                width: compressed.width,
                height: compressed.height,
            };
            wire.width = Some(i64::from(info.width));
            wire.height = Some(i64::from(info.height));
        }
        Some(info)
    } else {
        None
    };

    let item_key = state::key_of(item);
    let entry_id = keys.item_id(&item_key);
    let entry = ItemEntry {
        item: wire,
        item_key: item_key.clone(),
        captured_at: item.updated_at,
        image,
    };
    let plain = pack(&serde_json::to_vec(&entry)?, &bytes)?;
    let body = keys.seal_entry(&entry_id, ITEM, &hlc, &plain)?;
    let limit = if item.kind == ClipboardKind::Image {
        image_limit
    } else {
        policy.max_entry_bytes
    };
    if body.len() as u64 > limit {
        log::info!(
            "remote sync skips a record of {} bytes over the limit of {limit}",
            body.len()
        );
        return Ok(None);
    }

    if track {
        note_item(&core.db.pool().await, keys, &item_key, &hlc, state::PENDING).await?;
    }
    Ok(Some(Outgoing {
        entry_id,
        kind: ITEM,
        class: if item.kind == ClipboardKind::Image {
            "image"
        } else {
            "text"
        },
        hlc,
        live_since,
        skip,
        body,
    }))
}

pub(super) fn meta_frame(
    keys: &GroupKeys,
    entry: &MetaEntry,
    hlc: String,
) -> anyhow::Result<Outgoing> {
    let entry_id = keys.meta_id(&entry.field, &entry.item_key);
    let body = keys.seal_entry(
        &entry_id,
        META,
        &hlc,
        &pack(&serde_json::to_vec(entry)?, &[])?,
    )?;
    Ok(Outgoing {
        entry_id,
        kind: META,
        class: "none",
        hlc,
        live_since: Some(Instant::now()),
        skip: Vec::new(),
        body,
    })
}

/// 记下本机的元数据改动（后写者赢的状态），返回要上传的帧。服务器收下之前这一行是待发的，
/// 断网、退出、被拒时留着，每次补齐结束后重发（[`pending_meta`]）。
pub(super) async fn local_meta(
    pool: &SqlitePool,
    keys: &GroupKeys,
    entry: MetaEntry,
    hlc: String,
) -> anyhow::Result<Outgoing> {
    record_meta(pool, keys, &entry, &hlc, state::PENDING).await?;
    meta_frame(keys, &entry, hlc)
}

/// 还没被服务器收下的本机元数据改动，重新包成帧。
pub(super) async fn pending_meta(
    pool: &SqlitePool,
    keys: &GroupKeys,
) -> anyhow::Result<Vec<Outgoing>> {
    let mut frames = Vec::new();
    for row in state::rows_of_kind(pool, state::PENDING).await? {
        let Some(item_key) = row.item_key.filter(|_| !row.field.is_empty()) else {
            continue;
        };
        let value = row.value.as_deref().map_or(Value::Null, |text| {
            serde_json::from_str(text).unwrap_or(Value::Null)
        });
        let entry = MetaEntry {
            item_key,
            field: row.field,
            value,
        };
        frames.push(meta_frame(keys, &entry, row.hlc)?);
    }
    Ok(frames)
}

async fn record_meta(
    pool: &SqlitePool,
    keys: &GroupKeys,
    entry: &MetaEntry,
    hlc: &str,
    kind: &str,
) -> crate::error::Result<()> {
    state::put(
        pool,
        &RemoteStateRow {
            entry_id: keys.meta_id(&entry.field, &entry.item_key),
            kind: kind.to_owned(),
            item_key: Some(entry.item_key.clone()),
            field: entry.field.clone(),
            value: Some(entry.value.to_string()),
            hlc: hlc.to_owned(),
        },
    )
    .await
}

/// 记下一条记录最后一次的 HLC（只往新的走）。`kind` 是 [`ITEM`]（服务器上已经有这一版），
/// 或本机刚采集、还没被服务器收下的 [`state::PENDING`]。
async fn note_item(
    pool: &SqlitePool,
    keys: &GroupKeys,
    item_key: &str,
    hlc: &str,
    kind: &str,
) -> crate::error::Result<()> {
    let entry_id = keys.item_id(item_key);
    if let Some(row) = state::get(pool, &entry_id).await? {
        if !hlc_newer(hlc, &row.hlc) {
            return Ok(());
        }
    }
    state::put(
        pool,
        &RemoteStateRow {
            entry_id,
            kind: kind.to_owned(),
            item_key: Some(item_key.to_owned()),
            field: String::new(),
            value: None,
            hlc: hlc.to_owned(),
        },
    )
    .await
}

/// 补齐时服务器把本机自己传过的记录发回来（重新加入、覆盖导入之后）：只记下它的 HLC，
/// 之后的历史上传不再重传它。不入库：本机删掉的不该被它找回来。
pub(super) async fn note_own_item(
    pool: &SqlitePool,
    keys: &GroupKeys,
    plain: &[u8],
    hlc: &str,
) -> anyhow::Result<()> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Key {
        item_key: String,
    }

    let (json, _) = unpack(plain)?;
    let key: Key = serde_json::from_slice(json).context("invalid record entry")?;
    // 上传其实成功了、只是没等到响应（退出、断网）时，本机这一版还记着待发。
    state::confirm(pool, &keys.item_id(&key.item_key), ITEM, hlc).await?;
    note_item(pool, keys, &key.item_key, hlc, ITEM).await?;
    Ok(())
}

/// 收到的条目在线上的身份。
pub(super) struct Incoming<'a> {
    pub hlc: &'a str,
    pub from: &'a str,
    pub live: bool,
    /// 本机不同步全部历史时是加入同步组的时间：补齐的记录只收这之后采集的。
    pub since: Option<DateTime<Utc>>,
}

/// 入库一条收到的记录。`live` 的按实时推送处理（写剪贴板、回声过滤），补齐的只在本机还没有时插入；
/// 本机已有同一记录且更新时间不早于 `capturedAt`（局域网先送到了）时跳过。`meta` 打开时比它新的
/// 删除墓碑挡住它，入库后补上先到的元数据；`track`（服务器存储历史）时记下它的 HLC。
/// 返回本机记录的元数据是否变了。
#[allow(clippy::too_many_arguments)]
pub(super) async fn receive_item(
    core: &Arc<CoreInner>,
    keys: &GroupKeys,
    echo: &Mutex<EchoFilter>,
    incoming: &Incoming<'_>,
    plain: &[u8],
    mut options: ReceiveOptions,
    platform: Platform,
    track: bool,
    meta: bool,
) -> anyhow::Result<bool> {
    let (json, binary) = unpack(plain)?;
    let entry: ItemEntry = serde_json::from_slice(json).context("invalid record entry")?;
    if !incoming.live
        && incoming
            .since
            .is_some_and(|since| entry.captured_at < since)
    {
        return Ok(false);
    }
    let pool = core.db.pool().await;
    if meta {
        let tombstone = state::get(&pool, &keys.meta_id(DELETED, &entry.item_key)).await?;
        if tombstone.is_some_and(|row| {
            row.value.as_deref() == Some("true") && !hlc_newer(incoming.hlc, &row.hlc)
        }) {
            return Ok(false);
        }
    }

    let existing = state::find_by_item_key(&pool, &entry.item_key).await?;
    let known = if incoming.live {
        existing
            .iter()
            .any(|(_, updated_at)| *updated_at >= entry.captured_at)
    } else {
        !existing.is_empty()
    };
    let mut changed = false;
    if !known {
        options.captured_at = (!incoming.live).then(|| entry.captured_at.min(Utc::now()));
        let item_key = entry.item_key.clone();
        let stored = engine::receive_with_options(
            core,
            echo,
            incoming.from,
            platform,
            entry.item,
            binary.to_vec(),
            incoming.live,
            options,
        )
        .await?;
        if let Some(id) = stored {
            state::set_sync_key(&pool, &id, &item_key).await?;
            if meta {
                for row in state::meta_rows(&pool, &item_key).await? {
                    let value = row.value.as_deref().map_or(Value::Null, |text| {
                        serde_json::from_str(text).unwrap_or(Value::Null)
                    });
                    changed |= apply_field(&pool, &id, &row.field, &value).await?;
                }
            }
        }
    }
    if track || meta {
        note_item(&pool, keys, &entry.item_key, incoming.hlc, ITEM).await?;
    }
    Ok(changed)
}

/// 应用收到的元数据：比本机记下的新才生效；对应记录还没收到时只记下来，记录到了再补上。
/// `deleted = true` 比这条记录最后一次的 HLC 新时删掉本机记录。返回本机记录是否变了。
pub(super) async fn apply_meta(
    core: &Arc<CoreInner>,
    keys: &GroupKeys,
    hlc: &str,
    entry: MetaEntry,
) -> anyhow::Result<bool> {
    if ![FAVORITE, PINNED, NOTE, DELETED].contains(&entry.field.as_str()) {
        return Ok(false);
    }
    let pool = core.db.pool().await;
    let entry_id = keys.meta_id(&entry.field, &entry.item_key);
    if let Some(row) = state::get(&pool, &entry_id).await? {
        if !hlc_newer(hlc, &row.hlc) {
            return Ok(false);
        }
    }
    record_meta(&pool, keys, &entry, hlc, META).await?;

    let rows = state::find_by_item_key(&pool, &entry.item_key).await?;
    if entry.field == DELETED {
        if entry.value != Value::Bool(true) {
            return Ok(false);
        }
        let item = state::get(&pool, &keys.item_id(&entry.item_key)).await?;
        if item.as_ref().is_some_and(|row| !hlc_newer(hlc, &row.hlc)) {
            // 删除之后又有人复制了这条记录。
            return Ok(false);
        }
        // 本机还没记过这条记录的 HLC（加入同步组之前复制、还没传上去的）：只删比这次删除早的，
        // 删除之后本机又复制的留着。
        let deleted_at = parse_hlc(hlc).map(|(clock, _)| (clock.0 >> 16) as i64);
        let mut removed = false;
        for (id, updated_at) in &rows {
            let newer =
                item.is_none() && deleted_at.is_some_and(|at| updated_at.timestamp_millis() >= at);
            if !newer {
                crate::ops::delete_one(core, id).await?;
                removed = true;
            }
        }
        return Ok(removed);
    }
    let ids: Vec<String> = rows.into_iter().map(|(id, _)| id).collect();

    let mut changed = false;
    for id in &ids {
        changed |= apply_field(&pool, id, &entry.field, &entry.value).await?;
    }
    Ok(changed)
}

/// 把一个元数据字段写进本机记录。取值读不懂的跳过。
async fn apply_field(
    pool: &SqlitePool,
    id: &str,
    field: &str,
    value: &Value,
) -> crate::error::Result<bool> {
    match (field, value) {
        (FAVORITE, Value::Bool(favorite)) => set_item_favorite(pool, id, *favorite).await?,
        (PINNED, Value::Bool(pinned)) => set_item_pinned(pool, id, *pinned).await?,
        (NOTE, Value::String(note)) => update_item_note(pool, id, Some(note)).await?,
        (NOTE, Value::Null) => update_item_note(pool, id, None).await?,
        _ => return Ok(false),
    }
    Ok(true)
}

/// 历史上传的一条记录：服务器还没收下的记录条目，加上本机的收藏 / 置顶 / 备注里组里还没有的。
/// 按本机规则发不了的记录（超过上限、原图没了）也记下 HLC，之后的历史上传不再一遍遍重试它。
pub(super) async fn history_frames(
    core: &Arc<CoreInner>,
    keys: &GroupKeys,
    policy: &RemotePolicy,
    item: &ClipboardItem,
    meta: bool,
    mut tick: impl FnMut() -> String,
) -> anyhow::Result<Vec<Outgoing>> {
    let pool = core.db.pool().await;
    let item_key = state::key_of(item);
    let mut frames = Vec::new();
    let uploaded = state::get(&pool, &keys.item_id(&item_key))
        .await?
        .is_some_and(|row| row.kind == ITEM);
    if !uploaded {
        let hlc = tick();
        match item_frame(
            core,
            keys,
            policy,
            item,
            hlc.clone(),
            None,
            Vec::new(),
            true,
        )
        .await?
        {
            Some(frame) => frames.push(frame),
            None => note_item(&pool, keys, &item_key, &hlc, ITEM).await?,
        }
    }
    if !meta {
        return Ok(frames);
    }

    let fields = [
        (FAVORITE, item.is_favorite.then_some(Value::Bool(true))),
        (PINNED, item.is_pinned.then_some(Value::Bool(true))),
        (NOTE, item.note.clone().map(Value::String)),
    ];
    for (field, value) in fields {
        let Some(value) = value else {
            continue;
        };
        if state::get(&pool, &keys.meta_id(field, &item_key))
            .await?
            .is_some()
        {
            continue;
        }
        let entry = MetaEntry {
            item_key: item_key.clone(),
            field: field.to_owned(),
            value,
        };
        let mut frame = local_meta(&pool, keys, entry, tick()).await?;
        frame.live_since = None;
        frames.push(frame);
    }
    Ok(frames)
}
