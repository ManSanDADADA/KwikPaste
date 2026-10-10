//! 1.x 各版本写下的 `settings.json` 交给 2.0 的 [`SettingsStore`] 读取：全部读得进来，用户设置不丢。
//!
//! `fixtures/settings/` 里：
//! - `v*-windows-first-run.json`：各已发布版本首次启动写盘的完整设置（Windows），用当时的模型代码生成；
//! - `v1.4.0-customized.json`：1.4.0 的每个字段都改成非默认值，覆盖所有取值写法。
//!
//! 仓库的 Biome 规则会给 JSON 的键排序，所以夹具的键序与当时写盘的不同，键和值没有变化。
//! 以后拿到用户真实的设置文件，放进同一目录即可被下面的测试覆盖。

use std::fs;
use std::path::{Path, PathBuf};

use kwikpaste_core::settings::{Settings, SettingsStore};
use kwikpaste_core::{AppEnv, CorePaths};
use serde_json::Value;

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/settings")
}

fn fixtures() -> Vec<(String, String)> {
    let mut files: Vec<(String, String)> = fs::read_dir(fixtures_dir())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .map(|path| {
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            (name, fs::read_to_string(&path).unwrap())
        })
        .collect();
    files.sort();

    assert!(files.len() >= 9, "settings fixtures are missing");
    files
}

/// 把设置文件放进临时数据目录，用真实的 [`SettingsStore`] 读出来。
fn load(content: &str) -> (tempfile::TempDir, CorePaths, SettingsStore) {
    let temp = tempfile::tempdir().unwrap();
    let local = temp.path().join("local");
    let paths = CorePaths::new(AppEnv::Prod, local.clone(), local.join("logs"), None);
    let config = paths.config_dir().unwrap();
    fs::create_dir_all(&config).unwrap();
    fs::write(config.join("settings.json"), content).unwrap();

    let store = SettingsStore::new(&paths, Some("en-US".to_owned())).unwrap();
    (temp, paths, store)
}

/// 收集所有叶子（数组整体算一个叶子）的路径与取值。
fn leaves(value: &Value, path: String, out: &mut Vec<(String, Value)>) {
    match value {
        Value::Object(fields) => {
            for (key, field) in fields {
                let next = if path.is_empty() {
                    key.clone()
                } else {
                    format!("{path}.{key}")
                };
                leaves(field, next, out);
            }
        }
        other => out.push((path, other.clone())),
    }
}

fn pointer(path: &str) -> String {
    format!("/{}", path.replace('.', "/"))
}

#[test]
fn every_released_settings_file_loads_without_fallback() {
    for (name, content) in fixtures() {
        let (_temp, _paths, store) = load(&content);
        let report = store.load_report();

        assert!(
            !report.unreadable && report.fallbacks.is_empty(),
            "{name}: {report:?}"
        );
        assert!(!report.history_degraded(), "{name}");
        assert!(
            serde_json::to_value(store.snapshot()).unwrap()["clipboard"]
                .get("ocr")
                .is_none(),
            "{name}: OCR must not be a setting"
        );
        assert_eq!(
            store.snapshot().clipboard.feedback.copy_sound_volume,
            100,
            "{name}"
        );
    }
}

#[test]
fn missing_copy_sound_volume_preserves_old_feedback_and_defaults_to_full_volume() {
    for sound in [false, true] {
        let json = format!(r#"{{"clipboard":{{"feedback":{{"copySound":{sound}}}}}}}"#);
        let settings: Settings = serde_json::from_str(&json).unwrap();
        let (_temp, _paths, store) = load(&json);
        assert_eq!(settings.clipboard.feedback.copy_sound, sound);
        assert_eq!(settings.clipboard.feedback.copy_sound_volume, 100);
        assert_eq!(
            store.snapshot().clipboard.feedback,
            settings.clipboard.feedback
        );
        assert!(store.load_report().fallbacks.is_empty());
    }
    let settings: Settings = serde_json::from_str("{}").unwrap();
    assert_eq!(settings.clipboard.feedback.copy_sound_volume, 100);
}

#[test]
fn copy_sound_volume_round_trips_as_a_camel_case_number() {
    for volume in [0, 50, 100, 255] {
        let mut settings = Settings::default();
        settings.clipboard.feedback.copy_sound = true;
        settings.clipboard.feedback.copy_sound_volume = volume;
        let json = serde_json::to_value(&settings).unwrap();
        assert_eq!(json["clipboard"]["feedback"]["copySoundVolume"], volume);
        let restored: Settings = serde_json::from_value(json).unwrap();
        assert_eq!(restored, settings);
    }
}

#[test]
fn every_known_value_survives_loading() {
    let current_keys = {
        let mut keys = Vec::new();
        leaves(
            &serde_json::to_value(Settings::default()).unwrap(),
            String::new(),
            &mut keys,
        );
        keys.into_iter().map(|(path, _)| path).collect::<Vec<_>>()
    };

    for (name, content) in fixtures() {
        let (_temp, _paths, store) = load(&content);
        let original: Value = serde_json::from_str(&content).unwrap();
        let loaded = serde_json::to_value(store.snapshot()).unwrap();

        let mut written = Vec::new();
        leaves(&original, String::new(), &mut written);
        for (path, value) in written {
            // 已发布版本删掉的字段（如 1.2.0 的 `cleanupIntervalHours`）不再读取。
            if !current_keys.contains(&path) {
                continue;
            }

            assert_eq!(
                loaded.pointer(&pointer(&path)),
                Some(&value),
                "{name}: {path}"
            );
        }
    }
}

#[test]
fn loading_never_rewrites_the_file() {
    for (name, content) in fixtures() {
        let (_temp, paths, _store) = load(&content);
        let on_disk =
            fs::read_to_string(paths.config_dir().unwrap().join("settings.json")).unwrap();

        assert_eq!(on_disk, content, "{name}");
    }
}

#[test]
fn customized_settings_round_trip_exactly() {
    let content = fs::read_to_string(fixtures_dir().join("v1.4.0-customized.json")).unwrap();
    let (_temp, paths, store) = load(&content);
    let original: Value = serde_json::from_str(&content).unwrap();

    // 旧版夹具没有暂停字段，读取后应补上新版本默认值。
    let mut expected = original.clone();
    expected["shortcuts"]["pauseInFullscreen"] = Value::Bool(true);
    expected["shortcuts"]["pauseAppIds"] = Value::Array(Vec::new());
    expected["shortcuts"]["pastePlain"] = Value::String(String::new());
    expected["clipboard"]["feedback"]["copySoundVolume"] = Value::from(100);
    // 2.x 删掉了 1.x 的更新渠道开关。
    let update = expected["update"].as_object_mut().unwrap();
    update.remove("includeBeta");
    update.remove("includeNightly");
    assert_eq!(serde_json::to_value(store.snapshot()).unwrap(), expected);

    // 夹具的每个叶子都不是默认值，否则它覆盖不到对应字段的取值写法。
    let defaults = serde_json::to_value(Settings::default()).unwrap();
    let mut written = Vec::new();
    leaves(&original, String::new(), &mut written);
    for (path, value) in written {
        assert_ne!(
            defaults.pointer(&pointer(&path)),
            Some(&value),
            "{path} is still the default"
        );
    }

    // 校验通过：保存任意设置都不会因为读进来的值被拒。
    let saved = store
        .update(serde_json::json!({"appearance": {"theme": "light"}}))
        .unwrap();
    let reread: Settings = serde_json::from_str(
        &fs::read_to_string(paths.config_dir().unwrap().join("settings.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(reread, saved);
    assert_eq!(reread.clipboard, store.snapshot().clipboard);
}

#[test]
fn first_run_writes_defaults_in_system_language() {
    let temp = tempfile::tempdir().unwrap();
    let local = temp.path().join("local");
    let paths = CorePaths::new(AppEnv::Dev, local.clone(), local.join("logs"), None);

    let store = SettingsStore::new(&paths, Some("en-US".to_owned())).unwrap();
    let written: Settings =
        serde_json::from_str(&fs::read_to_string(local.join("dev/config/settings.json")).unwrap())
            .unwrap();

    assert_eq!(
        written.appearance.language,
        kwikpaste_core::settings::Language::EnUS
    );
    assert_eq!(written, store.snapshot());
    assert_eq!(store.load_report(), Default::default());
}

#[test]
fn one_unreadable_field_keeps_the_rest_of_the_file() {
    let content = fs::read_to_string(fixtures_dir().join("v1.4.0-customized.json")).unwrap();
    let mut value: Value = serde_json::from_str(&content).unwrap();
    value["appearance"]["theme"] = Value::from("sepia");
    value["clipboard"]["history"]["maxCount"] = Value::from(-5);

    let (_temp, _paths, store) = load(&value.to_string());
    let settings = store.snapshot();
    let report = store.load_report();

    assert_eq!(
        settings.appearance.theme,
        kwikpaste_core::settings::Theme::Auto
    );
    assert_eq!(settings.clipboard.history.max_count, 0);
    assert_eq!(settings.clipboard.history.storage_limit_mb, 4096);
    assert_eq!(settings.shortcuts.open_clipboard, "Control+Shift+V");
    assert_eq!(settings.sync.lan.device_name, "书房电脑");
    assert_eq!(settings.clipboard.history.rules.len(), 5);
    assert!(report.history_degraded());
    assert_eq!(report.fallbacks.len(), 2);
}
