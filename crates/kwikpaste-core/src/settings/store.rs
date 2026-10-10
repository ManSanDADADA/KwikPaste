//! 设置持久化。
//!
//! - 落盘位置：`<app_data_dir>/config/settings.json`（dev/prod 由 [`CorePaths`] 的环境子目录隔离）。
//! - 写入流程：先写到 `settings.json.tmp`，再原子替换主文件，避免中途断电留下半截 JSON。
//! - 缺字段兼容：`Settings` 各结构体都 `#[serde(default)]`，新版本新增字段不影响旧文件。
//! - 坏字段兼容：读盘时某个字段读不懂只让它回落默认值（见 `settings::lenient`），回落记录在
//!   [`SettingsStore::load_report`] 里；历史清理相关的字段回落时自动清理暂停（[`SettingsStore::cleanup_paused`]）。
//! - 整份损坏：文件读不懂时内存里用默认值，下次写盘前先把原文件复制成
//!   `settings.json.corrupt-<UTC 时间>`，用户原来的设置还能找回来。

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, RwLock};

use anyhow::Context;
use chrono::Utc;

use crate::error::{AppError, Result};
use crate::paths::CorePaths;

use super::delta::SettingsDelta;
use super::lenient::{self, SettingsLoadReport};
use super::model::{
    LanSync, Language, RetentionRule, Settings, LAN_SYNC_DEVICE_NAME_MAX_CHARS,
    LAN_SYNC_MAX_IMAGE_MB_MAX, LAN_SYNC_MAX_IMAGE_MB_MIN, MAX_RETENTION_RULES,
    WINDOW_OPEN_GROUP_PREFIX, WINDOW_OPEN_SELECTION_ALL, WINDOW_OPEN_SELECTION_PRESERVE,
};

const FILENAME: &str = "settings.json";

pub struct SettingsStore {
    path: RwLock<PathBuf>,
    current: RwLock<Settings>,
    /// 宿主传入的系统 locale（如 `zh-CN`），首次启动与恢复默认时决定界面语言。
    system_locale: Option<String>,
    disk: Mutex<DiskState>,
}

/// 最近一次读盘留下的、影响之后行为的状态。
#[derive(Debug, Default)]
struct DiskState {
    report: SettingsLoadReport,
    /// 读盘之后用户显式保存过历史设置，或整份替换过设置。
    history_confirmed: bool,
    /// 文件整份读不懂：下次写盘前先备份原文件。
    backup_pending: bool,
}

impl DiskState {
    fn loaded(report: SettingsLoadReport) -> Self {
        Self {
            backup_pending: report.unreadable,
            report,
            history_confirmed: false,
        }
    }
}

impl SettingsStore {
    /// 读取（首次启动时创建）`<app_data_dir>/config/settings.json`。
    ///
    /// `system_locale` 是宿主取到的系统语言标签，与 1.x 用的 `tauri-plugin-os` `locale()`
    /// 同源（sys-locale，BCP 47，如 `zh-CN`）；只在首次启动和恢复默认时决定界面语言。
    pub fn new(paths: &CorePaths, system_locale: Option<String>) -> Result<Self> {
        let dir = paths.config_dir()?;
        fs::create_dir_all(&dir).with_context(|| format!("failed to create dir at {dir:?}"))?;

        let path = dir.join(FILENAME);

        let (current, report) = match load_from_disk(&path) {
            Some(loaded) => loaded,
            None => {
                // 真·首次启动：用系统 locale 推导默认语言并落盘，之后所有读取都走常规分支。
                let settings = default_settings_with_system_locale(system_locale.as_deref());
                if let Err(err) = write_atomic(&path, &settings) {
                    log::warn!("persist first-run settings failed: {err}");
                }
                (settings, SettingsLoadReport::default())
            }
        };
        log::info!("settings store ready at {path:?}");

        Ok(Self {
            path: RwLock::new(path),
            current: RwLock::new(current),
            system_locale,
            disk: Mutex::new(DiskState::loaded(report)),
        })
    }

    pub fn snapshot(&self) -> Settings {
        self.current.read().expect("settings poisoned").clone()
    }

    /// 最近一次从磁盘读取设置时，哪些字段没有采用文件原值。
    pub fn load_report(&self) -> SettingsLoadReport {
        self.disk().report.clone()
    }

    /// 历史清理相关的设置读盘时有回落（或整份读不懂），且用户之后还没有显式保存过历史设置。
    /// 为真时自动清理（含存储上限清理和释放空闲页）暂停：回落值可能比用户原来的设置删得更多。
    pub fn cleanup_paused(&self) -> bool {
        let disk = self.disk();
        disk.report.history_degraded() && !disk.history_confirmed
    }

    /// 恢复默认设置并落盘，返回新的完整快照。
    pub fn reset(&self) -> Result<Settings> {
        let next = default_settings_with_system_locale(self.system_locale.as_deref());

        let path = self.path();
        self.persist(&path, &next, &SettingsDelta::replaced())?;
        *self.current.write().expect("settings poisoned") = next.clone();
        Ok(next)
    }

    /// 用 JSON patch 深度合并到当前设置，落盘后返回新快照。
    /// patch 必须是 object；非 object 视为「整个替换」语义不友好，直接报错。
    pub fn update(&self, patch: serde_json::Value) -> Result<Settings> {
        if !patch.is_object() {
            return Err(AppError::Other(anyhow::anyhow!(
                "settings patch must be a JSON object"
            )));
        }

        let delta = SettingsDelta::from_patch(&patch);
        let mut guard = self.current.write().expect("settings poisoned");

        let mut merged = serde_json::to_value(&*guard)
            .context("failed to serialize current settings for merge")?;
        deep_merge(&mut merged, patch);

        let next: Settings = serde_json::from_value(merged)
            .map_err(|err| AppError::Other(anyhow::anyhow!("invalid settings patch: {err}")))?;

        validate_settings(&next)?;

        let path = self.path();
        self.persist(&path, &next, &delta)?;
        *guard = next.clone();
        Ok(next)
    }

    /// 用完整设置文件替换当前设置；覆盖导入专用。
    pub fn replace_from_file(&self, path: &Path) -> Result<Settings> {
        let next = read_replacement(path)?;
        self.replace(next)
    }

    /// 用一份已校验的完整设置替换当前设置并落盘。
    pub(crate) fn replace(&self, next: Settings) -> Result<Settings> {
        let path = self.path();
        self.persist(&path, &next, &SettingsDelta::replaced())?;
        *self.current.write().expect("settings poisoned") = next.clone();
        Ok(next)
    }

    /// 数据目录热切换后重新绑定设置文件，并把新路径里的设置加载进内存。
    pub fn rebase(&self, paths: &CorePaths) -> Result<Settings> {
        let dir = paths.config_dir()?;
        fs::create_dir_all(&dir).with_context(|| format!("failed to create dir at {dir:?}"))?;
        let path = dir.join(FILENAME);
        let (current, report) = match load_from_disk(&path) {
            Some(loaded) => loaded,
            None => {
                let settings = default_settings_with_system_locale(self.system_locale.as_deref());
                write_atomic(&path, &settings)?;
                (settings, SettingsLoadReport::default())
            }
        };

        *self.path.write().expect("settings path poisoned") = path;
        *self.current.write().expect("settings poisoned") = current.clone();
        *self.disk() = DiskState::loaded(report);
        Ok(current)
    }

    fn path(&self) -> PathBuf {
        self.path.read().expect("settings path poisoned").clone()
    }

    fn disk(&self) -> MutexGuard<'_, DiskState> {
        self.disk
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// 写盘：原文件整份读不懂时先备份一次；改到历史设置后解除自动清理的暂停。
    fn persist(&self, path: &Path, settings: &Settings, delta: &SettingsDelta) -> Result<()> {
        let mut disk = self.disk();
        if disk.backup_pending {
            backup_corrupt_file(path)?;
            disk.backup_pending = false;
        }

        write_atomic(path, settings)?;
        if delta.touches("clipboard.history") {
            disk.history_confirmed = true;
        }
        Ok(())
    }
}

/// 把读不懂的设置文件复制成 `settings.json.corrupt-<UTC 时间>`；文件已不存在时什么都不做。
fn backup_corrupt_file(path: &Path) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }

    let stamp = Utc::now().format("%Y%m%d-%H%M%S");
    let backup = path.with_file_name(format!("{FILENAME}.corrupt-{stamp}"));
    fs::copy(path, &backup)
        .with_context(|| format!("failed to back up unreadable settings to {backup:?}"))?;
    log::warn!("unreadable settings file backed up to {backup:?} before overwriting");
    Ok(())
}

/// 生成默认设置，并沿用首次启动的系统语言推导规则。
fn default_settings_with_system_locale(system_locale: Option<&str>) -> Settings {
    let mut settings = Settings::default();
    if let Some(tag) = system_locale {
        settings.appearance.language = Language::from_system_locale(tag);
        log::info!(
            "default settings language from locale {tag}: {:?}",
            settings.appearance.language
        );
    }
    settings
}

/// 返回 `None` 表示主文件不存在（首次启动），调用方据此走「初始化默认」分支；
/// 文件读不出来或整份不是 JSON 对象时打 warn，并返回 `Settings::default()` 包装在 `Some` 里——
/// 这条路径表示「文件存在但坏了」，不要当成首次启动覆盖系统 locale。
/// 个别字段读不懂时只有这些字段回落默认值。
fn load_from_disk(path: &Path) -> Option<(Settings, SettingsLoadReport)> {
    if !path.exists() {
        return None;
    }

    match fs::read_to_string(path) {
        Ok(content) => Some(lenient::parse(&content)),
        Err(err) => {
            log::warn!("settings file {path:?} unreadable, using defaults: {err}");
            Some((
                Settings::default(),
                SettingsLoadReport {
                    unreadable: true,
                    fallbacks: Vec::new(),
                },
            ))
        }
    }
}

/// 写入策略：把新内容写到 tmp 后 rename 成主文件；rename 在同一文件系统下是原子的。
fn write_atomic(path: &Path, settings: &Settings) -> Result<()> {
    let json = serde_json::to_string_pretty(settings).context("failed to serialize settings")?;

    let tmp = path.with_extension("json.tmp");
    {
        let mut file = fs::File::create(&tmp)
            .with_context(|| format!("failed to create tmp settings at {tmp:?}"))?;
        file.write_all(json.as_bytes())
            .with_context(|| format!("failed to write tmp settings at {tmp:?}"))?;
        file.sync_all().ok();
    }
    fs::rename(&tmp, path)
        .with_context(|| format!("failed to promote tmp settings to {path:?}"))?;
    Ok(())
}

/// 读一份完整设置文件（覆盖导入用）：整份严格解析并校验，不做逐字段回落。
pub(crate) fn read_replacement(path: &Path) -> Result<Settings> {
    let content = fs::read_to_string(path).with_context(|| format!("failed to read {path:?}"))?;
    let next: Settings = serde_json::from_str(&content)
        .map_err(|err| AppError::Other(anyhow::anyhow!("invalid settings file: {err}")))?;

    validate_settings(&next)?;
    Ok(next)
}

/// 校验设置之间的跨字段约束，避免非法配置写入磁盘。
fn validate_settings(settings: &Settings) -> Result<()> {
    validate_window_open_group(&settings.clipboard.window.select_group_on_open)?;
    validate_retention_rules(&settings.clipboard.history.rules)?;
    validate_lan_sync(&settings.sync.lan)?;

    let mut shortcuts = std::collections::HashSet::new();
    for value in [
        &settings.shortcuts.open_clipboard,
        &settings.shortcuts.open_preference,
        &settings.shortcuts.paste_plain,
    ] {
        let normalized = normalize_shortcut_value(value);
        if !normalized.is_empty() && !shortcuts.insert(normalized) {
            return Err(AppError::Other(anyhow::anyhow!(
                "global shortcuts must be unique"
            )));
        }
    }
    Ok(())
}

/// 校验打开剪贴板窗口时选中分组的字符串编码，避免非法设置值落盘。
fn validate_window_open_group(value: &str) -> Result<()> {
    if value == WINDOW_OPEN_SELECTION_PRESERVE || value == WINDOW_OPEN_SELECTION_ALL {
        return Ok(());
    }

    let Some(group_id) = value.strip_prefix(WINDOW_OPEN_GROUP_PREFIX) else {
        return Err(AppError::Other(anyhow::anyhow!(
            "open group selection is invalid"
        )));
    };

    if group_id.trim().is_empty() {
        return Err(AppError::Other(anyhow::anyhow!(
            "open group selection is invalid"
        )));
    }

    Ok(())
}

/// 校验自定义清理规则：条数有上限，id 非空且互不重复（前端按 id 对应逐条统计）。
fn validate_retention_rules(rules: &[RetentionRule]) -> Result<()> {
    if rules.len() > MAX_RETENTION_RULES {
        return Err(AppError::Other(anyhow::anyhow!(
            "at most {MAX_RETENTION_RULES} cleanup rules are allowed"
        )));
    }

    let mut ids = std::collections::HashSet::new();
    for rule in rules {
        if rule.id.trim().is_empty() || !ids.insert(rule.id.as_str()) {
            return Err(AppError::Other(anyhow::anyhow!(
                "cleanup rule ids must be unique and non-empty"
            )));
        }
    }

    Ok(())
}

/// 校验局域网同步：设备名会进 mDNS 广播和对端界面，限制长度；图片上限限定在可控范围。
fn validate_lan_sync(lan: &LanSync) -> Result<()> {
    if lan.device_name.chars().count() > LAN_SYNC_DEVICE_NAME_MAX_CHARS {
        return Err(AppError::Other(anyhow::anyhow!(
            "device name must be at most {LAN_SYNC_DEVICE_NAME_MAX_CHARS} characters"
        )));
    }

    if !(LAN_SYNC_MAX_IMAGE_MB_MIN..=LAN_SYNC_MAX_IMAGE_MB_MAX).contains(&lan.max_image_mb) {
        return Err(AppError::Other(anyhow::anyhow!(
            "image size limit must be between {LAN_SYNC_MAX_IMAGE_MB_MIN} and {LAN_SYNC_MAX_IMAGE_MB_MAX} MB"
        )));
    }

    Ok(())
}

/// 归一化快捷键字面量，供跨字段校验忽略大小写和多余空白。
fn normalize_shortcut_value(value: &str) -> String {
    let mut keys = value
        .split('+')
        .map(str::trim)
        .filter(|key| !key.is_empty())
        .map(str::to_ascii_lowercase)
        .collect::<Vec<_>>();
    keys.sort_unstable();
    keys.join("+")
}

fn deep_merge(base: &mut serde_json::Value, patch: serde_json::Value) {
    match (base, patch) {
        (serde_json::Value::Object(base_map), serde_json::Value::Object(patch_map)) => {
            for (k, v) in patch_map {
                match base_map.get_mut(&k) {
                    Some(existing) if existing.is_object() && v.is_object() => {
                        deep_merge(existing, v);
                    }
                    _ => {
                        base_map.insert(k, v);
                    }
                }
            }
        }
        (slot, patch) => {
            *slot = patch;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deep_merge_overrides_leaves_and_recurses_objects() {
        let mut base = serde_json::json!({
            "general": {"autoStart": false, "trayIcon": true},
            "clipboard": {"history": {"maxCount": 0}},
        });
        let patch = serde_json::json!({
            "general": {"autoStart": true},
            "clipboard": {"history": {"maxCount": 500}},
        });
        deep_merge(&mut base, patch);
        assert_eq!(
            base,
            serde_json::json!({
                "general": {"autoStart": true, "trayIcon": true},
                "clipboard": {"history": {"maxCount": 500}},
            })
        );
    }

    #[test]
    fn deep_merge_replaces_arrays_wholesale() {
        let mut base = serde_json::json!({"itemActions": ["copy", "star", "delete"]});
        let patch = serde_json::json!({"itemActions": ["copy", "pastePlain"]});
        deep_merge(&mut base, patch);
        assert_eq!(
            base,
            serde_json::json!({"itemActions": ["copy", "pastePlain"]})
        );
    }

    #[test]
    fn validate_settings_rejects_duplicate_global_shortcuts() {
        let mut settings = Settings::default();
        settings.shortcuts.open_preference = settings.shortcuts.open_clipboard.clone();

        assert!(validate_settings(&settings).is_err());
    }

    #[test]
    fn validate_settings_allows_empty_global_shortcuts() {
        let mut settings = Settings::default();
        settings.shortcuts.open_preference = String::new();

        assert!(validate_settings(&settings).is_ok());
    }

    #[test]
    fn validate_settings_rejects_duplicate_plain_paste_shortcut() {
        let mut settings = Settings::default();
        settings.shortcuts.paste_plain = settings.shortcuts.open_clipboard.clone();

        assert!(validate_settings(&settings).is_err());
    }

    #[test]
    fn validate_settings_allows_empty_plain_paste_shortcut() {
        let mut settings = Settings::default();
        settings.shortcuts.paste_plain = String::new();

        assert!(validate_settings(&settings).is_ok());
    }

    #[test]
    fn missing_fields_fall_back_to_defaults() {
        let partial = r#"{
            "general": {"autoStart": true},
            "appearance": {"theme": "dark", "language": "en-US"}
        }"#;
        let parsed: Settings = serde_json::from_str(partial).unwrap();
        assert!(parsed.general.auto_start);
        assert!(!parsed.general.run_as_admin);
        assert!(parsed.general.tray_icon, "default kept");
        assert_eq!(parsed.shortcuts.open_clipboard, "Alt+C");
        assert_eq!(parsed.appearance.theme, crate::settings::Theme::Dark);
        assert_eq!(
            parsed.appearance.material,
            crate::settings::Material::Default
        );
        assert_eq!(
            parsed.update.frequency,
            crate::settings::UpdateFrequency::Daily
        );
        assert_eq!(
            parsed.clipboard.content.sort,
            crate::db::models::ClipboardItemSort::UpdatedAt
        );
        assert!(!parsed.clipboard.content.copy_then_hide_window);
        assert!(serde_json::to_value(&parsed).unwrap()["clipboard"]
            .get("ocr")
            .is_none());
        assert!(
            parsed
                .clipboard
                .content
                .delete_favorite_items_only_in_favorite_group
        );
        assert!(!parsed.clipboard.content.delete_favorite_items);
        assert!(parsed.clipboard.content.delete_favorite_confirm);
        assert!(!parsed.clipboard.content.delete_pinned_items);
        assert!(parsed.clipboard.content.delete_pinned_confirm);
        assert!(!parsed.clipboard.content.update_on_reuse);
        assert!(parsed.clipboard.history.rules.is_empty());
        assert_eq!(
            parsed.clipboard.history.storage_limit_mb,
            crate::settings::DEFAULT_STORAGE_LIMIT_MB
        );
        assert_eq!(
            parsed.clipboard.history.storage_limit_action,
            crate::settings::StorageLimitAction::Remind
        );
        assert!(parsed.clipboard.window.scroll_to_top_on_open);
        assert_eq!(
            parsed.clipboard.window.select_range_on_open,
            crate::settings::WindowOpenRangeSelection::All
        );
        assert_eq!(
            parsed.clipboard.window.select_category_on_open,
            crate::settings::WindowOpenCategorySelection::All
        );
        assert_eq!(
            parsed.clipboard.window.select_group_on_open,
            crate::settings::WINDOW_OPEN_SELECTION_ALL
        );
    }

    #[test]
    fn released_window_settings_keep_saved_open_selection() {
        let released = r#"{
            "clipboard": {
                "window": {
                    "selectRangeOnOpen": "preserve",
                    "selectCategoryOnOpen": "preserve",
                    "selectGroupOnOpen": "preserve"
                }
            }
        }"#;
        let parsed: Settings = serde_json::from_str(released).unwrap();
        let window = parsed.clipboard.window;

        assert_eq!(
            window.select_range_on_open,
            crate::settings::WindowOpenRangeSelection::Preserve
        );
        assert_eq!(
            window.select_category_on_open,
            crate::settings::WindowOpenCategorySelection::Preserve
        );
        assert_eq!(
            window.select_group_on_open,
            crate::settings::WINDOW_OPEN_SELECTION_PRESERVE
        );
    }

    #[test]
    fn released_history_settings_gain_storage_limit_defaults() {
        let released = r#"{
            "clipboard": {
                "history": {
                    "retention": {"value": 7, "unit": "days"},
                    "maxCount": 500,
                    "cleanupIntervalHours": 6
                }
            }
        }"#;
        let parsed: Settings = serde_json::from_str(released).unwrap();
        let history = parsed.clipboard.history;

        assert_eq!(history.max_count, 500);
        assert_eq!(
            history.retention,
            crate::settings::Retention {
                value: 7,
                unit: crate::settings::RetentionUnit::Days
            }
        );
        assert!(history.rules.is_empty());
        assert_eq!(
            history.storage_limit_mb,
            crate::settings::DEFAULT_STORAGE_LIMIT_MB
        );
        assert_eq!(
            history.storage_limit_action,
            crate::settings::StorageLimitAction::Remind
        );
        // 已废弃的清理周期不再写回设置文件。
        let json = serde_json::to_value(&history).unwrap();
        assert!(json.get("cleanupIntervalHours").is_none());
    }

    #[test]
    fn retention_rules_round_trip_with_defaults_for_missing_fields() {
        let parsed: Settings = serde_json::from_str(
            r#"{
                "clipboard": {
                    "history": {
                        "rules": [
                            {
                                "id": "big-images",
                                "categories": ["image"],
                                "minSizeKb": 5120,
                                "keep": {"value": 3, "unit": "days"}
                            },
                            {"id": "secrets", "sensitiveOnly": true, "keep": {"value": 30, "unit": "minutes"}}
                        ]
                    }
                }
            }"#,
        )
        .unwrap();
        let rules = &parsed.clipboard.history.rules;

        assert_eq!(rules.len(), 2);
        assert!(rules[0].enabled);
        assert_eq!(
            rules[0].categories,
            [crate::db::overview::ContentCategory::Image]
        );
        assert_eq!(rules[0].min_size_kb, 5120);
        assert!(rules[1].sensitive_only);
        assert_eq!(rules[1].keep.unit, crate::settings::RetentionUnit::Minutes);
        assert!(validate_settings(&parsed).is_ok());
    }

    #[test]
    fn validate_settings_rejects_duplicate_or_empty_rule_ids() {
        let mut settings = Settings::default();
        settings.clipboard.history.rules = vec![
            RetentionRule {
                id: "a".to_owned(),
                ..RetentionRule::default()
            },
            RetentionRule {
                id: "a".to_owned(),
                ..RetentionRule::default()
            },
        ];
        assert!(validate_settings(&settings).is_err());

        settings.clipboard.history.rules = vec![RetentionRule::default()];
        assert!(validate_settings(&settings).is_err());

        settings.clipboard.history.rules = (0..=MAX_RETENTION_RULES)
            .map(|index| RetentionRule {
                id: index.to_string(),
                ..RetentionRule::default()
            })
            .collect();
        assert!(validate_settings(&settings).is_err());
    }

    #[test]
    fn released_display_settings_enable_quick_snippets() {
        let released = r#"{
            "clipboard": {
                "display": {"textMaxLines": 2, "imageMaxHeight": 80, "fileMaxCount": 4},
                "content": {
                    "itemActions": ["copy", "star"],
                    "itemActionOrder": ["copy", "star", "delete"]
                }
            }
        }"#;
        let parsed: Settings = serde_json::from_str(released).unwrap();
        let display = parsed.clipboard.display;

        assert_eq!(display.text_max_lines, 2);
        assert!(display.quick_snippets);
        // 已保存的悬停动作保持原样，拆词只在偏好页里作为未勾选项追加。
        assert_eq!(
            parsed.clipboard.content.item_actions,
            [
                crate::settings::ItemAction::Copy,
                crate::settings::ItemAction::Star
            ]
        );
    }

    // 存量文件没有列表风格与密度字段：升级后卡片风格不变，密度按「标准」读取。
    #[test]
    fn released_display_settings_use_standard_card_list() {
        let released = r#"{
            "clipboard": {
                "display": {"textMaxLines": 2, "imageMaxHeight": 80, "fileMaxCount": 4, "quickSnippets": false}
            }
        }"#;
        let parsed: Settings = serde_json::from_str(released).unwrap();
        let display = parsed.clipboard.display;

        assert_eq!(display.list_style, crate::settings::ListStyle::Card);
        assert_eq!(display.density, crate::settings::ListDensity::Standard);
        assert_eq!(
            display.custom_layout,
            crate::settings::CustomListLayout::default()
        );
        assert!(!display.quick_snippets);
    }

    #[test]
    fn released_preview_settings_show_plain_text() {
        let released = r#"{
            "clipboard": {
                "preview": {"hoverEnabled": true, "hoverDelayMs": "ms300", "spaceEnabled": false}
            }
        }"#;
        let parsed: Settings = serde_json::from_str(released).unwrap();
        let preview = parsed.clipboard.preview;

        assert!(preview.hover_enabled);
        assert!(!preview.space_enabled);
        assert_eq!(preview.text_view, crate::settings::PreviewTextView::Plain);
    }

    // 已发布版本首次启动就把完整设置落盘，存量用户文件里一定带 hoverEnabled / spaceEnabled：
    // 默认值调整只影响新装与恢复默认。
    #[test]
    fn preview_defaults_change_but_keep_released_choice() {
        let defaults = crate::settings::Preview::default();
        assert!(defaults.hover_enabled);
        assert_eq!(
            defaults.hover_delay_ms,
            crate::settings::PreviewHoverDelayMs::Ms500
        );
        assert!(!defaults.space_enabled);
        assert_eq!(defaults.text_view, crate::settings::PreviewTextView::Words);

        let released = r#"{
            "clipboard": {
                "preview": {"hoverEnabled": false, "hoverDelayMs": "ms500", "spaceEnabled": true, "textView": "plain"}
            }
        }"#;
        let parsed: Settings = serde_json::from_str(released).unwrap();
        let preview = parsed.clipboard.preview;

        assert!(!preview.hover_enabled);
        assert!(preview.space_enabled);
        assert_eq!(preview.text_view, crate::settings::PreviewTextView::Plain);
    }

    #[test]
    fn released_shortcut_settings_keep_quick_paste_disabled() {
        let released = r#"{
            "shortcuts": {
                "openClipboard": "Control+Shift+V",
                "openPreference": "Alt+X",
                "winV": true
            }
        }"#;
        let parsed: Settings = serde_json::from_str(released).unwrap();
        let shortcuts = parsed.shortcuts;

        assert_eq!(shortcuts.open_clipboard, "Control+Shift+V");
        assert!(shortcuts.win_v);
        assert!(!shortcuts.quick_paste.enabled);
        assert_eq!(
            shortcuts.quick_paste.modifiers,
            crate::settings::QuickPasteModifiers::ControlShift
        );
    }

    #[test]
    fn released_shortcut_settings_keep_mouse_trigger_disabled() {
        let released = r#"{
            "shortcuts": {
                "openClipboard": "Alt+C",
                "openPreference": "Alt+X",
                "winV": true,
                "quickPaste": {"enabled": true, "modifiers": "alt"}
            }
        }"#;
        let parsed: Settings = serde_json::from_str(released).unwrap();

        assert!(parsed.shortcuts.win_v);
        assert_eq!(
            parsed.shortcuts.mouse_trigger,
            crate::settings::MouseTrigger::Disabled
        );
        assert!(parsed.shortcuts.pause_in_fullscreen);
        assert!(parsed.shortcuts.pause_app_ids.is_empty());
    }

    #[test]
    fn pause_shortcut_settings_default_and_round_trip() {
        let parsed: Settings =
            serde_json::from_str(r#"{"shortcuts":{"openClipboard":"Alt+C"}}"#).unwrap();
        assert!(parsed.shortcuts.pause_in_fullscreen);
        assert!(parsed.shortcuts.pause_app_ids.is_empty());

        let mut changed = parsed;
        changed.shortcuts.pause_in_fullscreen = false;
        changed.shortcuts.pause_app_ids = vec!["C:\\Games\\Game.exe".to_owned()];
        let json = serde_json::to_value(&changed).unwrap();
        assert_eq!(json["shortcuts"]["pauseInFullscreen"], false);
        assert_eq!(json["shortcuts"]["pauseAppIds"][0], "C:\\Games\\Game.exe");
        let round_trip: Settings = serde_json::from_value(json).unwrap();
        assert_eq!(round_trip.shortcuts, changed.shortcuts);
    }

    #[test]
    fn released_general_settings_keep_tray_click_on_clipboard() {
        let released = r#"{
            "general": {"autoStart": true, "runAsAdmin": false, "trayIcon": true, "dockIcon": false}
        }"#;
        let parsed: Settings = serde_json::from_str(released).unwrap();

        assert!(parsed.general.auto_start);
        assert_eq!(
            parsed.general.tray_click,
            crate::settings::TrayClick::Clipboard
        );
    }

    #[test]
    fn tray_click_round_trips_preference() {
        let parsed: Settings =
            serde_json::from_str(r#"{"general": {"trayClick": "preference"}}"#).unwrap();
        assert_eq!(
            parsed.general.tray_click,
            crate::settings::TrayClick::Preference
        );

        let json = serde_json::to_value(&parsed).unwrap();
        assert_eq!(json["general"]["trayClick"], "preference");
    }

    #[test]
    fn mouse_trigger_round_trips_side_buttons() {
        let parsed: Settings =
            serde_json::from_str(r#"{"shortcuts": {"mouseTrigger": "forward"}}"#).unwrap();
        assert_eq!(
            parsed.shortcuts.mouse_trigger,
            crate::settings::MouseTrigger::Forward
        );

        let json = serde_json::to_value(&parsed).unwrap();
        assert_eq!(json["shortcuts"]["mouseTrigger"], "forward");
    }

    #[test]
    fn storage_limit_bytes_never_drops_below_minimum() {
        let mut history = crate::settings::History::default();
        assert_eq!(history.storage_limit_bytes(), 1024 * 1024 * 1024);

        history.storage_limit_mb = 0;
        assert_eq!(
            history.storage_limit_bytes(),
            u64::from(crate::settings::MIN_STORAGE_LIMIT_MB) * 1024 * 1024
        );
    }

    #[test]
    fn validate_settings_rejects_invalid_open_group_selection() {
        let mut settings = Settings::default();
        settings.clipboard.window.select_group_on_open = "invalid".to_owned();

        assert!(validate_settings(&settings).is_err());
    }
}
