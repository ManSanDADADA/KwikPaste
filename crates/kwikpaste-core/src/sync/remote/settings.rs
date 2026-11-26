//! 设置同步（REMOTE.md §6.4）：设置 JSON 按叶子展开，每个叶子一个条目。
//!
//! 本机改设置后对比 `remote_sync_state` 里「上次同步的值」，只上传变了的叶子；收到的叶子先记下
//! 同步值再写进设置，写入引起的差异检查因此不会把它传回去。

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::crypto::GroupKeys;
use crate::db::remote::{self as state, RemoteStateRow};
use crate::error::Result;
use crate::root::{Core, CoreInner};
use crate::settings::Settings;

pub(super) const KIND: &str = "settings";
const SCOPE_ALL: &str = "all";

/// 只属于本机、不同步的设置（含子项）。规格列出的之外：偏好窗口侧栏的展开状态、分组栏的迁移标记，
/// 以及引用本机自定义分组 id 的自动归组规则和「打开时选中的分组」（分组不同步）。
const EXCLUDED: &[&str] = &[
    "/general/autoStart",
    "/general/runAsAdmin",
    "/onboarding",
    "/update",
    "/sync",
    "/extensions/wetype",
    "/appearance/preferencesSidebarCollapsed",
    "/clipboard/window/groupBarKnowsLink",
    "/clipboard/autoGroupRules",
    "/clipboard/window/selectGroupOnOpen",
];

/// 只在同平台之间同步的设置（含子项）：快捷键、托盘与 Dock、保存应用 id（Windows 上是 exe 路径）的列表，
/// 以及只有 Windows 才有的窗口材质。
const PLATFORM: &[&str] = &[
    "/shortcuts",
    "/general/trayIcon",
    "/general/trayClick",
    "/general/dockIcon",
    "/clipboard/filters/excludedAppIds",
    "/clipboard/capture/mergeLinesAppIds",
    "/clipboard/history/rules",
    "/appearance/material",
];

/// 键值表整体算一个叶子；空表不写进设置文件，展开时补成 `{}`。
const MAPS: &[&str] = &["/shortcuts/panel"];

/// 设置条目的内容。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(super) struct SettingsEntry {
    pub path: String,
    pub scope: String,
    pub value: Value,
}

/// `pointer` 是否等于 `prefix` 或在它下面。
fn under(pointer: &str, prefix: &str) -> bool {
    pointer
        .strip_prefix(prefix)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

/// 叶子的同步范围：`all`、本机平台，或 `None`（不同步）。
pub(super) fn scope_of(pointer: &str) -> Option<&'static str> {
    if EXCLUDED.iter().any(|prefix| under(pointer, prefix)) {
        return None;
    }
    if PLATFORM.iter().any(|prefix| under(pointer, prefix)) {
        return Some(platform_scope());
    }
    Some(SCOPE_ALL)
}

fn platform_scope() -> &'static str {
    match super::super::current_platform() {
        crate::db::models::Platform::Macos => "macos",
        crate::db::models::Platform::Windows => "windows",
    }
}

/// 把设置 JSON 展开成 `JSON 指针 → 值`：对象逐层展开，数组和键值表整体算一个叶子。
pub(super) fn flatten(value: &Value) -> BTreeMap<String, Value> {
    fn walk(value: &Value, pointer: String, out: &mut BTreeMap<String, Value>) {
        match value {
            Value::Object(map) if !pointer.is_empty() && MAPS.contains(&pointer.as_str()) => {
                out.insert(pointer, Value::Object(map.clone()));
            }
            Value::Object(map) => {
                for (key, child) in map {
                    let token = key.replace('~', "~0").replace('/', "~1");
                    walk(child, format!("{pointer}/{token}"), out);
                }
            }
            leaf => {
                out.insert(pointer, leaf.clone());
            }
        }
    }

    let mut out = BTreeMap::new();
    walk(value, String::new(), &mut out);
    for map in MAPS {
        out.entry((*map).to_owned())
            .or_insert_with(|| Value::Object(Map::new()));
    }
    out
}

/// 本机参与同步的叶子：`(指针, 范围, 值)`。
pub(super) fn local_leaves(settings: &Settings) -> Vec<(String, &'static str, Value)> {
    let Ok(json) = serde_json::to_value(settings) else {
        return Vec::new();
    };
    flatten(&json)
        .into_iter()
        .filter_map(|(pointer, value)| scope_of(&pointer).map(|scope| (pointer, scope, value)))
        .collect()
}

/// 和上次同步的值不同（或还没同步过）的叶子：`(条目 id, 叶子)`。
pub(super) async fn changed_leaves(
    pool: &sqlx::SqlitePool,
    keys: &GroupKeys,
    settings: &Settings,
) -> Result<Vec<(String, SettingsEntry)>> {
    let synced: HashMap<String, Option<Value>> = state::rows_of_kind(pool, KIND)
        .await?
        .into_iter()
        .map(|row| {
            let value = row.value.and_then(|text| serde_json::from_str(&text).ok());
            (row.entry_id, value)
        })
        .collect();

    Ok(local_leaves(settings)
        .into_iter()
        .filter_map(|(pointer, scope, value)| {
            let entry_id = keys.settings_id(scope, &pointer);
            let unchanged = synced
                .get(&entry_id)
                .is_some_and(|synced| synced.as_ref() == Some(&value));
            (!unchanged).then(|| {
                (
                    entry_id,
                    SettingsEntry {
                        path: pointer,
                        scope: scope.to_owned(),
                        value,
                    },
                )
            })
        })
        .collect())
}

/// 记下一个叶子的值与 HLC。`kind` 是 [`KIND`]（组里已经是这个值），或本机刚改、服务器还没收下的
/// [`state::PENDING`]：待发的叶子不算同步过，下次差异检查（每次补齐结束、设置再变）会重发。
pub(super) async fn record(
    pool: &sqlx::SqlitePool,
    entry_id: &str,
    pointer: &str,
    value: &Value,
    hlc: &str,
    kind: &str,
) -> Result<()> {
    state::put(
        pool,
        &RemoteStateRow {
            entry_id: entry_id.to_owned(),
            kind: kind.to_owned(),
            item_key: None,
            field: pointer.to_owned(),
            value: Some(value.to_string()),
            hlc: hlc.to_owned(),
        },
    )
    .await
}

/// 应用收到的设置叶子：范围和本机的划分一致、比本机记下的新才写进设置。返回是否写了。
/// 本机版本不认识的设置项什么都不记：升级后启动时从头收一遍设置（`settings_pass`）再应用，
/// 不会把本机的默认值当成改动传上去。
pub(super) async fn apply_incoming(
    core: &Arc<CoreInner>,
    entry_id: &str,
    hlc: &str,
    entry: SettingsEntry,
) -> Result<bool> {
    if scope_of(&entry.path) != Some(entry.scope.as_str()) {
        return Ok(false);
    }
    let pool = core.db.pool().await;
    if let Some(row) = state::get(&pool, entry_id).await? {
        if !super::crypto::hlc_newer(hlc, &row.hlc) {
            return Ok(false);
        }
    }
    let current = flatten(&serde_json::to_value(core.settings.snapshot()).unwrap_or_default());
    let Some(local) = current.get(&entry.path) else {
        return Ok(false);
    };

    if *local != entry.value {
        // 先记同步值：写入设置会触发差异检查，看到的已经是这个值。
        record(&pool, entry_id, &entry.path, &entry.value, hlc, KIND).await?;
        let patch = patch_for(&entry.path, &entry.value, Some(local));
        if let Err(err) = Core(core.clone()).update_settings(patch).await {
            log::warn!("remote sync skips setting {}: {err}", entry.path);
        }
    }

    // 本机的取值（写入被拒绝或被收进范围时与收到的不同）记为同步值，不拿它去和组里的值来回覆盖。
    let actual = flatten(&serde_json::to_value(core.settings.snapshot()).unwrap_or_default())
        .remove(&entry.path)
        .unwrap_or(Value::Null);
    record(&pool, entry_id, &entry.path, &actual, hlc, KIND).await?;
    Ok(true)
}

/// 把一个叶子包成设置 patch。键值表要把本机多出来的键置空，否则深合并会留下它们。
fn patch_for(pointer: &str, value: &Value, current: Option<&Value>) -> Value {
    let mut value = value.clone();
    if let (Value::Object(incoming), Some(Value::Object(local))) = (&mut value, current) {
        for key in local.keys() {
            incoming.entry(key.clone()).or_insert(Value::Null);
        }
    }
    let tokens: Vec<&str> = pointer.split('/').skip(1).collect();
    tokens.into_iter().rev().fold(value, |inner, token| {
        let key = token.replace("~1", "/").replace("~0", "~");
        Value::Object(Map::from_iter([(key, inner)]))
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn flatten_keeps_arrays_and_maps_whole() {
        let leaves = flatten(&json!({
            "appearance": {"theme": "dark"},
            "clipboard": {"capture": {"order": ["text", "image"]}},
            "shortcuts": {"panel": {"paste": "Enter"}},
            "a/b": {"c~d": 1}
        }));

        assert_eq!(leaves["/appearance/theme"], json!("dark"));
        assert_eq!(leaves["/clipboard/capture/order"], json!(["text", "image"]));
        assert_eq!(leaves["/shortcuts/panel"], json!({"paste": "Enter"}));
        assert_eq!(leaves["/a~1b/c~0d"], json!(1));
        assert_eq!(flatten(&json!({}))["/shortcuts/panel"], json!({}));
    }

    #[test]
    fn exclusions_and_platform_scope() {
        let platform = platform_scope();
        for excluded in [
            "/general/autoStart",
            "/general/runAsAdmin",
            "/onboarding/completed",
            "/update/lastCheckedAt",
            "/sync/remote/enabled",
            "/sync/lan/deviceName",
            "/extensions/wetype/sendText",
            "/clipboard/autoGroupRules",
        ] {
            assert_eq!(scope_of(excluded), None, "{excluded}");
        }
        for scoped in [
            "/shortcuts/openClipboard",
            "/shortcuts/panel",
            "/general/trayIcon",
            "/clipboard/filters/excludedAppIds",
        ] {
            assert_eq!(scope_of(scoped), Some(platform), "{scoped}");
        }
        assert_eq!(scope_of("/appearance/theme"), Some("all"));
        assert_eq!(scope_of("/extensions/translate/auto"), Some("all"));
        assert_eq!(scope_of("/generalX"), Some("all"));

        let leaves = local_leaves(&Settings::default());
        assert!(leaves.iter().any(
            |(pointer, scope, _)| pointer == "/clipboard/history/maxCount" && *scope == "all"
        ));
        assert!(leaves
            .iter()
            .all(|(pointer, _, _)| !pointer.starts_with("/sync")));
    }

    #[test]
    fn patches_nest_and_clear_dropped_map_keys() {
        assert_eq!(
            patch_for("/appearance/theme", &json!("dark"), None),
            json!({"appearance": {"theme": "dark"}})
        );
        assert_eq!(
            patch_for(
                "/shortcuts/panel",
                &json!({"a": "X"}),
                Some(&json!({"a": "Y", "b": "Z"}))
            ),
            json!({"shortcuts": {"panel": {"a": "X", "b": null}}})
        );
    }
}
