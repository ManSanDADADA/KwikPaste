//! 宽松读取 `settings.json`：某个字段读不懂只让这个字段回落默认值，不让整份设置回落。
//!
//! 整份能严格解析时结果与 1.4.0 完全相同，只有严格解析失败才逐字段处理。不认识的字段直接忽略；
//! 数组里读不懂的元素丢掉，其余元素保留原顺序。

use std::collections::HashSet;
use std::marker::PhantomData;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::model::{RetentionRule, Settings};

/// 读取设置文件时没有采用文件原值的地方。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SettingsLoadReport {
    /// 文件存在，但不是 JSON 对象（或根本不是 JSON），整份按默认值处理。
    pub unreadable: bool,
    /// 回落默认值或被丢弃的字段路径，如 `clipboard.history.maxCount`、`clipboard.content.itemActions[2]`。
    pub fallbacks: Vec<String>,
}

impl SettingsLoadReport {
    /// 历史清理相关的设置有回落，或整份读不懂。
    ///
    /// 回落值可能比用户原来的设置删得更多：清理规则的过滤条件默认是「不限」，存储上限默认 1 GB。
    /// 为真时自动清理（含存储上限清理和 VACUUM）应暂停，直到用户显式保存一次历史设置。
    pub fn history_degraded(&self) -> bool {
        self.unreadable
            || self.fallbacks.iter().any(|path| {
                path == "clipboard"
                    || path == "clipboard.history"
                    || path.starts_with("clipboard.history.")
            })
    }
}

/// 解析设置文件内容，返回设置与回落记录。
pub(super) fn parse(content: &str) -> (Settings, SettingsLoadReport) {
    let mut raw: Value = match serde_json::from_str(content) {
        Ok(raw) => raw,
        Err(err) => {
            log::warn!("settings file is not valid JSON, using defaults: {err}");
            return unreadable();
        }
    };

    let mut custom_fallbacks = Vec::new();
    // 逐条丢弃不可执行规则，避免坏规则重置其他设置或阻止后续无关设置保存。
    if let Some(value) = raw
        .get_mut("clipboard")
        .and_then(|clipboard| clipboard.get_mut("customRules"))
    {
        if let Value::Array(items) = value {
            let mut ids = HashSet::new();
            let mut kept = Vec::new();
            for (index, item) in std::mem::take(items).into_iter().enumerate() {
                let valid =
                    serde_json::from_value::<crate::clipboard::rules::CustomRule>(item.clone())
                        .ok()
                        .filter(|rule| {
                            crate::clipboard::rules::validate_rules(std::slice::from_ref(rule))
                                .is_ok()
                        })
                        .is_some_and(|rule| ids.insert(rule.id));
                if valid && kept.len() < crate::clipboard::rules::MAX_CUSTOM_RULES {
                    kept.push(item);
                } else {
                    custom_fallbacks.push(format!("clipboard.customRules[{index}]"));
                }
            }
            *items = kept;
        } else {
            *value = Value::Array(Vec::new());
            custom_fallbacks.push("clipboard.customRules".to_owned());
        }
    }
    if let Ok(settings) = Settings::deserialize(&raw) {
        return (
            settings,
            SettingsLoadReport {
                unreadable: false,
                fallbacks: custom_fallbacks,
            },
        );
    }

    let Value::Object(mut fields) = raw else {
        log::warn!("settings file is not a JSON object, using defaults");
        return unreadable();
    };

    // 动态字典的键不在默认结构里，不能按固定字段递归，否则其它字段损坏时会丢掉覆盖。
    let panel = fields
        .get_mut("shortcuts")
        .and_then(Value::as_object_mut)
        .and_then(|shortcuts| shortcuts.remove("panel"));

    // 清理规则单独处理：按字段回落会把窄规则放宽，见 [`lenient_rules`]。
    let rules = fields
        .get_mut("clipboard")
        .and_then(|clipboard| clipboard.get_mut("history"))
        .and_then(Value::as_object_mut)
        .and_then(|history| history.remove("rules"));

    let mut merger = Merger::<Settings>::new("");
    merger.fallbacks = custom_fallbacks;
    merger.merge(&mut Vec::new(), Value::Object(fields));

    if let Some(panel) = panel {
        // 默认值不写空字典，先补上槽位才能整体放进去。
        if let Some(shortcuts) = merger
            .accepted
            .get_mut("shortcuts")
            .and_then(Value::as_object_mut)
        {
            shortcuts
                .entry("panel")
                .or_insert_with(|| Value::Object(Default::default()));
        }
        let path = path_of(&["shortcuts", "panel"]);
        if !merger.try_set(&path, panel) {
            merger.fallbacks.push(render("", &path));
        }
    }

    if let Some(rules) = rules {
        let path = path_of(&["clipboard", "history", "rules"]);
        match rules {
            Value::Array(items) => {
                let rules = lenient_rules(items, &mut merger.fallbacks);
                if !merger.try_set(&path, Value::Array(rules)) {
                    merger.fallbacks.push(render("", &path));
                }
            }
            _ => merger.fallbacks.push(render("", &path)),
        }
    }

    let report = SettingsLoadReport {
        unreadable: false,
        fallbacks: merger.fallbacks,
    };
    for path in &report.fallbacks {
        log::warn!("settings field {path:?} unreadable, using its default");
    }

    match Settings::deserialize(&merger.accepted) {
        Ok(settings) => (settings, report),
        Err(err) => {
            log::warn!("lenient settings merge produced invalid settings, using defaults: {err}");
            unreadable()
        }
    }
}

fn unreadable() -> (Settings, SettingsLoadReport) {
    (
        Settings::default(),
        SettingsLoadReport {
            unreadable: true,
            fallbacks: Vec::new(),
        },
    )
}

/// 逐条读取自定义清理规则。读不懂的规则不按字段回落（过滤条件回落成「不限」会放宽规则），
/// 而是保留读得懂的字段、整条停用，用户在偏好页能看到并改回来；id 不可用的规则直接丢掉，
/// 否则之后每次保存设置都会因 id 校验失败而被拒绝。
fn lenient_rules(items: Vec<Value>, fallbacks: &mut Vec<String>) -> Vec<Value> {
    let mut rules = Vec::with_capacity(items.len());
    let mut ids = HashSet::new();

    for (index, item) in items.into_iter().enumerate() {
        let prefix = format!("clipboard.history.rules[{index}]");

        if let Ok(rule) = RetentionRule::deserialize(&item) {
            ids.insert(rule.id);
            rules.push(item);
            continue;
        }

        fallbacks.push(prefix.clone());
        if !item.is_object() {
            continue;
        }

        let mut merger = Merger::<RetentionRule>::new(&prefix);
        merger.merge(&mut Vec::new(), item);
        fallbacks.append(&mut merger.fallbacks);

        let Ok(mut rule) = RetentionRule::deserialize(&merger.accepted) else {
            continue;
        };
        if rule.id.trim().is_empty() || !ids.insert(rule.id.clone()) {
            continue;
        }

        rule.enabled = false;
        if let Ok(value) = serde_json::to_value(&rule) {
            rules.push(value);
        }
    }

    rules
}

#[derive(Debug, Clone)]
enum Seg {
    Key(String),
    Index(usize),
}

fn path_of(keys: &[&str]) -> Vec<Seg> {
    keys.iter().map(|key| Seg::Key((*key).to_owned())).collect()
}

/// 把路径写成 `a.b[2].c`，前面拼上 `prefix`。
fn render(prefix: &str, path: &[Seg]) -> String {
    let mut out = prefix.to_owned();

    for seg in path {
        match seg {
            Seg::Key(key) => {
                if !out.is_empty() {
                    out.push('.');
                }
                out.push_str(key);
            }
            Seg::Index(index) => out.push_str(&format!("[{index}]")),
        }
    }

    out
}

fn node<'a>(root: &'a Value, path: &[Seg]) -> Option<&'a Value> {
    path.iter().try_fold(root, |value, seg| match seg {
        Seg::Key(key) => value.get(key),
        Seg::Index(index) => value.get(index),
    })
}

fn node_mut<'a>(root: &'a mut Value, path: &[Seg]) -> Option<&'a mut Value> {
    path.iter().try_fold(root, |value, seg| match seg {
        Seg::Key(key) => value.get_mut(key),
        Seg::Index(index) => value.get_mut(index),
    })
}

/// 以 `T::default()` 为底，把文件里的值逐个放回去；每放一个都检查整份仍能解析成 `T`。
struct Merger<T> {
    prefix: String,
    /// 目前已采纳的整份文档，任何时候都能解析成 `T`。
    accepted: Value,
    fallbacks: Vec<String>,
    target: PhantomData<T>,
}

impl<T: DeserializeOwned + Serialize + Default> Merger<T> {
    fn new(prefix: &str) -> Self {
        Self {
            prefix: prefix.to_owned(),
            accepted: serde_json::to_value(T::default()).unwrap_or(Value::Null),
            fallbacks: Vec::new(),
            target: PhantomData,
        }
    }

    /// 把 `value` 放到 `path`（必须已存在）上，整份仍能解析才保留。
    fn try_set(&mut self, path: &[Seg], value: Value) -> bool {
        let mut candidate = self.accepted.clone();
        let Some(slot) = node_mut(&mut candidate, path) else {
            return false;
        };
        *slot = value;

        if T::deserialize(&candidate).is_err() {
            return false;
        }

        self.accepted = candidate;
        true
    }

    /// 合并文件里 `path` 处的值。默认值里没有的键是不认识的字段，直接忽略。
    fn merge(&mut self, path: &mut Vec<Seg>, value: Value) {
        let Some(current) = node(&self.accepted, path) else {
            return;
        };

        match value {
            Value::Object(fields) if current.is_object() => {
                for (key, field) in fields {
                    path.push(Seg::Key(key));
                    self.merge(path, field);
                    path.pop();
                }
            }
            Value::Array(items) if current.is_array() => {
                if !self.try_set(path, Value::Array(items.clone())) {
                    self.merge_items(path, items);
                }
            }
            other => {
                if !self.try_set(path, other) {
                    self.fallbacks.push(render(&self.prefix, path));
                }
            }
        }
    }

    /// 整个数组读不懂时逐个元素追加，读不懂的元素丢掉。
    fn merge_items(&mut self, path: &mut Vec<Seg>, items: Vec<Value>) {
        if !self.try_set(path, Value::Array(Vec::new())) {
            self.fallbacks.push(render(&self.prefix, path));
            return;
        }

        for (index, item) in items.into_iter().enumerate() {
            let mut candidate = self.accepted.clone();
            if let Some(Value::Array(list)) = node_mut(&mut candidate, path) {
                list.push(item);
            }

            if T::deserialize(&candidate).is_ok() {
                self.accepted = candidate;
                continue;
            }

            path.push(Seg::Index(index));
            self.fallbacks.push(render(&self.prefix, path));
            path.pop();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::{CaptureKind, ItemAction, RemoteImageQuality, RetentionUnit, Theme};

    #[test]
    fn panel_overrides_survive_unrelated_field_fallback() {
        let (settings, report) = parse(
            r#"{"general":{"autoStart":"bad"},"shortcuts":{"panel":{"favoriteSelected":"secondary-b","copySelected":"","futureAction":"future-key"}}}"#,
        );
        assert_eq!(
            settings
                .shortcuts
                .panel
                .get("favoriteSelected")
                .map(String::as_str),
            Some("secondary-b")
        );
        assert_eq!(
            settings
                .shortcuts
                .panel
                .get("copySelected")
                .map(String::as_str),
            Some("")
        );
        assert!(settings.shortcuts.panel.contains_key("futureAction"));
        assert_eq!(report.fallbacks, ["general.autoStart"]);
    }

    /// 回落路径按字典序比较：工作区里 gpui 打开了 serde_json 的 `preserve_order`，
    /// 对象键的遍历顺序随构建方式不同。
    fn sorted(report: &SettingsLoadReport) -> Vec<&str> {
        let mut paths: Vec<&str> = report.fallbacks.iter().map(String::as_str).collect();
        paths.sort_unstable();
        paths
    }

    #[test]
    fn custom_rules_round_trip_and_broken_entries_do_not_reset_settings() {
        let (settings, report) = parse(
            r#"{"general":{"autoStart":true},"clipboard":{"customRules":[
            {"id":"good","name":"工单","pattern":"ABC-[0-9]+","action":"snippet","future":42},
            {"id":"unknown","name":"unknown","pattern":"abc","action":"futureAction"},
            {"id":"bad","name":"broken","pattern":"[","action":"ignore"},
            {"id":"typed","name":42,"pattern":"abc","action":"snippet"},
            {"id":"empty","name":"empty","pattern":"a*","action":"sensitive"}
        ]}}"#,
        );
        assert!(settings.general.auto_start);
        assert_eq!(settings.clipboard.custom_rules.len(), 1);
        assert!(settings.clipboard.custom_rules[0].enabled);
        assert_eq!(report.fallbacks.len(), 4);
        let json = serde_json::to_string(&settings).unwrap();
        assert_eq!(parse(&json).0, settings);
        assert!(parse(&json).1.fallbacks.is_empty());
        assert!(parse("{}").0.clipboard.custom_rules.is_empty());
    }

    #[test]
    fn strict_files_are_read_as_is() {
        let (settings, report) = parse(r#"{"appearance": {"theme": "dark"}}"#);

        assert_eq!(settings.appearance.theme, Theme::Dark);
        assert_eq!(report, SettingsLoadReport::default());
    }

    #[test]
    fn double_click_view_is_read_strictly() {
        let (settings, report) =
            parse(r#"{"clipboard": {"content": {"autoPaste": "doubleClickView"}}}"#);

        assert_eq!(
            settings.clipboard.content.auto_paste,
            crate::settings::AutoPaste::DoubleClickView
        );
        assert_eq!(report, SettingsLoadReport::default());
    }

    #[test]
    fn unknown_fields_are_ignored() {
        let (settings, report) = parse(
            r#"{
                "appearance": {"theme": "dark", "futureKnob": 3},
                "futureSection": {"a": 1}
            }"#,
        );

        assert_eq!(settings.appearance.theme, Theme::Dark);
        assert!(report.fallbacks.is_empty());
    }

    #[test]
    fn removed_update_settings_are_ignored_without_degrading_other_values() {
        for theme in ["dark", "invalid"] {
            let content = serde_json::json!({
                "general": {"autoStart": true},
                "appearance": {"theme": theme},
                "update": {
                    "autoCheck": false, "frequency": "monthly", "skippedVersion": "2.1.0",
                    "lastCheckedAt": "2026-10-01T00:00:00Z"
                }
            });
            let (settings, report) = parse(&content.to_string());
            assert!(!report.unreadable);
            assert!(settings.general.auto_start);
            assert_eq!(
                settings.update.last_checked_at.as_deref(),
                Some("2026-10-01T00:00:00Z")
            );
            assert_eq!(
                report.fallbacks,
                if theme == "dark" {
                    vec![]
                } else {
                    vec!["appearance.theme"]
                }
            );
            let saved = serde_json::to_value(&settings).unwrap();
            for key in ["autoCheck", "frequency", "skippedVersion"] {
                assert!(saved["update"].get(key).is_none());
            }
        }
    }

    #[test]
    fn one_bad_value_only_resets_that_field() {
        let (settings, report) = parse(
            r#"{
                "general": {"autoStart": true, "trayClick": "somewhereNew"},
                "appearance": {"theme": "dark", "language": "en-US"},
                "clipboard": {"display": {"textMaxLines": "many", "fileMaxCount": 5}}
            }"#,
        );

        assert!(settings.general.auto_start);
        assert_eq!(
            settings.general.tray_click,
            crate::settings::TrayClick::Clipboard
        );
        assert_eq!(settings.appearance.theme, Theme::Dark);
        assert_eq!(
            settings.appearance.language,
            crate::settings::Language::EnUS
        );
        assert_eq!(settings.clipboard.display.text_max_lines, 3);
        assert_eq!(settings.clipboard.display.file_max_count, 5);
        assert_eq!(
            sorted(&report),
            ["clipboard.display.textMaxLines", "general.trayClick"]
        );
        assert!(!report.history_degraded());
    }

    #[test]
    fn malformed_remote_sync_field_falls_back_without_resetting_other_remote_values() {
        let (settings, report) = parse(
            r#"{
                "sync": {"remote": {
                    "enabled": true,
                    "text": false,
                    "imageQuality": "unsupported",
                    "maxImageMb": "large"
                }}
            }"#,
        );

        assert!(settings.sync.remote.enabled);
        assert!(!settings.sync.remote.text);
        assert_eq!(settings.sync.remote.image_quality, RemoteImageQuality::High);
        assert_eq!(settings.sync.remote.max_image_mb, 20);
        assert_eq!(
            sorted(&report),
            ["sync.remote.imageQuality", "sync.remote.maxImageMb"]
        );
    }

    #[test]
    fn unknown_array_items_are_dropped_in_order() {
        let (settings, report) = parse(
            r#"{
                "clipboard": {
                    "capture": {"order": ["text", "hologram", "html"]},
                    "content": {"itemActions": ["copy", "teleport", "delete"]}
                }
            }"#,
        );

        assert_eq!(
            settings.clipboard.capture.order,
            [CaptureKind::Text, CaptureKind::Html]
        );
        assert_eq!(
            settings.clipboard.content.item_actions,
            [ItemAction::Copy, ItemAction::Delete]
        );
        assert_eq!(
            sorted(&report),
            [
                "clipboard.capture.order[1]",
                "clipboard.content.itemActions[1]"
            ]
        );
    }

    #[test]
    fn default_link_quick_action_does_not_extend_saved_lists() {
        let (defaults, _) = parse("{}");
        assert_eq!(
            &defaults.clipboard.content.item_actions[..2],
            &[ItemAction::Copy, ItemAction::OpenLink]
        );
        for saved in ["[\"copy\",\"delete\"]", "[]"] {
            let (settings, report) = parse(&format!(
                "{{\"clipboard\":{{\"content\":{{\"itemActions\":{saved}}}}}}}"
            ));
            assert_eq!(
                serde_json::to_string(&settings.clipboard.content.item_actions).unwrap(),
                saved
            );
            assert!(report.fallbacks.is_empty());
        }
    }

    #[test]
    fn wrong_container_types_fall_back() {
        let (settings, report) = parse(
            r#"{"general": "yes", "shortcuts": {"openClipboard": "Alt+V"}, "update": {"lastCheckedAt": 5}}"#,
        );

        assert_eq!(settings.general, crate::settings::General::default());
        assert_eq!(settings.shortcuts.open_clipboard, "Alt+V");
        assert_eq!(settings.update.last_checked_at, None);
        assert_eq!(sorted(&report), ["general", "update.lastCheckedAt"]);
    }

    // 顶层是数组时 serde 按字段顺序读（`[]` 就是全默认），严格解析能过，与 1.4.0 相同，这里不列。
    #[test]
    fn invalid_json_or_non_object_is_unreadable() {
        for content in ["{not json", "42", r#""settings""#, ""] {
            let (settings, report) = parse(content);

            assert_eq!(settings, Settings::default());
            assert!(report.unreadable);
            assert!(report.history_degraded());
        }
    }

    #[test]
    fn unreadable_rule_is_kept_but_disabled() {
        let (settings, report) = parse(
            r#"{
                "clipboard": {
                    "history": {
                        "maxCount": 200,
                        "rules": [
                            {"id": "secrets", "sensitiveOnly": true, "keep": {"value": 1, "unit": "days"}},
                            {"id": "odd", "categories": ["image", "hologram"], "sensitiveOnly": true, "keep": {"value": 2, "unit": "hours"}}
                        ]
                    }
                }
            }"#,
        );
        let history = &settings.clipboard.history;

        assert_eq!(history.max_count, 200);
        assert_eq!(history.rules.len(), 2);
        assert!(history.rules[0].enabled);
        assert_eq!(history.rules[1].id, "odd");
        assert!(!history.rules[1].enabled);
        assert!(history.rules[1].sensitive_only);
        assert_eq!(history.rules[1].keep.unit, RetentionUnit::Hours);
        assert_eq!(
            sorted(&report),
            [
                "clipboard.history.rules[1]",
                "clipboard.history.rules[1].categories[1]"
            ]
        );
        assert!(report.history_degraded());
    }

    #[test]
    fn rules_without_usable_id_are_dropped() {
        let (settings, report) = parse(
            r#"{
                "clipboard": {
                    "history": {
                        "rules": [
                            {"id": "a", "keep": {"value": 1, "unit": "days"}},
                            {"id": "a", "enabled": "yes"},
                            {"id": 7, "enabled": false},
                            "not a rule"
                        ]
                    }
                }
            }"#,
        );
        let rules = &settings.clipboard.history.rules;

        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].id, "a");
        assert!(rules[0].enabled);
        assert!(report.history_degraded());
    }

    #[test]
    fn history_fallback_marks_cleanup_unsafe() {
        let (settings, report) =
            parse(r#"{"clipboard": {"history": {"storageLimitMb": -1, "maxCount": 50}}}"#);

        assert_eq!(settings.clipboard.history.max_count, 50);
        assert_eq!(
            settings.clipboard.history.storage_limit_mb,
            crate::settings::DEFAULT_STORAGE_LIMIT_MB
        );
        assert!(report.history_degraded());
    }
}
