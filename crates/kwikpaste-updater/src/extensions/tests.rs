use super::*;
use crate::http::testing::{Reply, Server};
use crate::testing::TestCore;
use crate::verify::testing::TestKey;
use crate::{AnnouncementOutcome, AnnouncementPrompt, HostFuture, UpdateStatus};

#[derive(Default)]
struct Ui(Mutex<Vec<Vec<ExtensionStatus>>>);

impl UpdaterUi for Ui {
    fn extensions_changed(&self, status: Vec<ExtensionStatus>) {
        lock(&self.0).push(status);
    }

    fn update_available(&self, _: UpdateStatus) {}

    fn show_announcement(&self, _: AnnouncementPrompt) -> HostFuture<'_, AnnouncementOutcome> {
        Box::pin(async { AnnouncementOutcome::Closed })
    }

    fn open_url(&self, _: &str) {}
}

fn entry(version: &str) -> CatalogEntry {
    CatalogEntry {
        id: "ocr".into(),
        version: version.into(),
        protocol: OCR_PROTOCOL,
        size: 8,
        signature: "signature".into(),
        url: "https://example.invalid/ocr.exe".parse().unwrap(),
        notes_zh: "中文说明".into(),
        notes_en: "English notes".into(),
        published_at: Some("2026-10-11T00:00:00Z".into()),
    }
}

fn catalog(entries: &[CatalogEntry]) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({ "extensions": entries })).unwrap()
}

fn remote(core: &TestCore, server: &Server, key: &TestKey, ui: Arc<Ui>) -> ExtensionManager {
    ExtensionManager::with_source(
        core.core.clone(),
        ui,
        Source::Remote {
            client: crate::http::updater_client(&core.core.info().version).unwrap(),
            public_key: key.public_key(),
            endpoint: server.url("/api/v1/extensions"),
        },
    )
}

fn install_baseline(core: &TestCore, version: &str, enabled: bool) {
    let file = core.root().join("baseline.exe");
    std::fs::write(&file, b"MZ fake baseline").unwrap();
    core.block_on(
        core.core
            .install_extension("ocr", version, OCR_PROTOCOL, &file),
    )
    .unwrap();
    core.block_on(core.core.set_extension_enabled("ocr", enabled))
        .unwrap();
}

#[test]
fn parses_catalog_and_selects_only_known_compatible_protocol() {
    let compatible = entry("1.0.0");
    let mut mismatch = entry("9.0.0");
    mismatch.protocol += 1;
    let mut unknown = entry("2.0.0");
    unknown.id = "unknown".into();
    let picked = select_catalog(&catalog(&[mismatch, unknown, compatible])).unwrap();
    assert_eq!(picked.len(), 1);
    assert_eq!(picked[0].version, "1.0.0");
    assert_eq!(picked[0].notes_zh, "中文说明");
    assert!(select_catalog(br#"{"extensions":[]}"#).unwrap().is_empty());
    assert!(select_catalog(b"not json").is_err());
    let unknown = br#"{"extensions":[{"id":"future","protocol":9}]}"#;
    assert!(select_catalog(unknown).unwrap().is_empty());
}

#[test]
fn decision_updates_newer_and_withdrawn_older_but_not_same_or_incompatible() {
    let installed = InstalledExtension {
        version: "2.0.0".into(),
        protocol: OCR_PROTOCOL,
        enabled: false,
    };
    for (version, expected) in [("3.0.0", true), ("1.0.0", true), ("2.0.0", false)] {
        assert_eq!(
            needs_update(Some(&entry(version)), Some(&installed)),
            expected
        );
    }
    let mut incompatible = entry("3.0.0");
    incompatible.protocol += 1;
    assert!(!needs_update(Some(&incompatible), Some(&installed)));
    assert!(!needs_update(None, Some(&installed)));
    assert!(!needs_update(Some(&entry("3.0.0")), None));
}

#[test]
fn invalid_sizes_versions_and_duplicate_picks_are_rejected() {
    let mut invalid = entry("../escape");
    assert!(select_catalog(&catalog(&[invalid.clone()])).is_err());
    invalid.version = "1.0.0".into();
    for size in [0, MAX_EXTENSION_BYTES + 1] {
        invalid.size = size;
        assert!(select_catalog(&catalog(&[invalid.clone()])).is_err());
    }
    assert!(select_catalog(&catalog(&[entry("1.0.0"), entry("2.0.0")])).is_err());
}

#[test]
fn refresh_without_auto_check_reports_update_and_unreachable_preserves_installed() {
    let core = TestCore::start("2.0.0");
    core.block_on(
        core.core
            .update_settings(serde_json::json!({"update":{"autoCheck":false}})),
    )
    .unwrap();
    install_baseline(&core, "2.0.0", false);
    let server = Server::start(vec![
        Reply::ok(catalog(&[entry("1.0.0")])),
        Reply::status("503 Service Unavailable"),
    ]);
    let manager = remote(&core, &server, &TestKey::generate(), Arc::default());
    let status = core.block_on(manager.refresh()).unwrap();
    assert!(status[0].update_available);
    assert_eq!(core.core.installed_extensions()["ocr"].version, "2.0.0");
    assert!(!core.core.installed_extensions()["ocr"].enabled);
    assert!(
        server.requests()[0]
            .line
            .contains(&format!("target={}", crate::target::extension_target()))
    );
    assert!(core.block_on(manager.refresh()).is_err());
    let status = manager.status();
    assert!(matches!(status[0].operation, OperationState::Error { .. }));
    assert!(status[0].catalog.is_none());
    assert_eq!(status[0].installed.as_ref().unwrap().version, "2.0.0");
    assert_eq!(server.requests().len(), 2);
}

#[test]
fn signature_failure_leaves_nothing_installed_and_size_failure_also_fails_closed() {
    for bad_signature in [true, false] {
        let core = TestCore::start("2.0.0");
        core.block_on(
            core.core
                .update_settings(serde_json::json!({"update":{"autoCheck":false}})),
        )
        .unwrap();
        let key = TestKey::generate();
        let wrong_key = TestKey::generate();
        let bytes = b"MZ fake extension".to_vec();
        let binary = Server::start(vec![Reply::ok(bytes.clone())]);
        let mut offered = entry("1.0.0");
        offered.url = binary.url("/binary").parse().unwrap();
        offered.signature = if bad_signature {
            wrong_key.sign(&bytes, true)
        } else {
            key.sign(&bytes, true)
        };
        offered.size = bytes.len() as u64 + u64::from(!bad_signature);
        let server = Server::start(vec![Reply::ok(catalog(&[offered]))]);
        let ui = Arc::new(Ui::default());
        let manager = remote(&core, &server, &key, ui.clone());
        core.block_on(manager.refresh()).unwrap();
        let err = core.block_on(manager.install("ocr".into())).unwrap_err();
        assert!(err.to_string().contains(if bad_signature {
            "signature"
        } else {
            "size mismatch"
        }));
        assert!(core.core.installed_extensions().is_empty());
        assert!(!core.core.paths().extensions_dir().join("ocr").exists());
        assert!(matches!(
            manager.status()[0].operation,
            OperationState::Error { .. }
        ));
        assert!(
            lock(&ui.0)
                .iter()
                .any(|cards| matches!(cards[0].operation, OperationState::Downloading { .. }))
        );
        assert!(
            !lock(&ui.0)
                .iter()
                .any(|cards| matches!(cards[0].operation, OperationState::Installing))
        );
    }
}

#[test]
fn refresh_auto_updates_in_both_directions_preserving_disabled_state() {
    for version in ["1.0.0", "3.0.0"] {
        let core = TestCore::start("2.0.0");
        install_baseline(&core, "2.0.0", false);
        let key = TestKey::generate();
        let bytes = b"MZ replacement".to_vec();
        let binary = Server::start(vec![Reply::ok(bytes.clone())]);
        let mut offered = entry(version);
        offered.url = binary.url("/binary").parse().unwrap();
        offered.signature = key.sign(&bytes, true);
        offered.size = bytes.len() as u64;
        let server = Server::start(vec![Reply::ok(catalog(&[offered]))]);
        let manager = remote(&core, &server, &key, Arc::default());
        core.block_on(manager.refresh()).unwrap();
        core.block_on(async {
            tokio::time::timeout(Duration::from_secs(10), async {
                loop {
                    if core.core.installed_extensions()["ocr"].version == version
                        && matches!(manager.status()[0].operation, OperationState::Idle)
                    {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap();
        });
        assert!(!core.core.installed_extensions()["ocr"].enabled);
        assert!(!manager.status()[0].update_available);
        core.block_on(manager.set_enabled("ocr".into(), true))
            .unwrap();
        assert!(manager.status()[0].installed.as_ref().unwrap().enabled);
        core.block_on(manager.uninstall("ocr".into())).unwrap();
        assert!(manager.status()[0].installed.is_none());
        // 已排队的自动更新不得在用户卸载后重新安装。
        core.block_on(manager.change_inner("ocr", Change::AutoUpdate))
            .unwrap();
        assert!(core.core.installed_extensions().is_empty());
    }
}

#[cfg(debug_assertions)]
#[test]
fn debug_catalog_only_reads_sibling_binaries() {
    let temp = tempfile::tempdir().unwrap();
    let exe = temp.path().join("KwikPaste.exe");
    assert!(dev_catalog(&exe).unwrap().is_empty());
    let sibling = temp
        .path()
        .join(kwikpaste_core::extensions::executable_name("ocr").unwrap());
    std::fs::write(sibling, b"dev binary").unwrap();
    let entries = dev_catalog(&exe).unwrap();
    assert_eq!(entries[0].version, "0.0.0-dev");
    assert_eq!(entries[0].size, 10);
    let core = TestCore::start("2.0.0");
    let manager = ExtensionManager::new(
        core.core.clone(),
        Arc::new(Ui::default()),
        crate::http::updater_client(&core.core.info().version).unwrap(),
        "unused".into(),
    );
    assert!(matches!(manager.0.source, Source::Dev));
    core.block_on(manager.refresh()).unwrap();
}

#[test]
fn failed_update_keeps_the_previous_extension_usable() {
    let core = TestCore::start("2.0.0");
    install_baseline(&core, "2.0.0", true);
    let previous = core.core.resolve_extension("ocr").unwrap();
    let key = TestKey::generate();
    let binary = Server::start(vec![Reply::ok(b"tampered".to_vec())]);
    let mut offered = entry("3.0.0");
    offered.url = binary.url("/binary").parse().unwrap();
    offered.signature = key.sign(b"original", true);
    let server = Server::start(vec![Reply::ok(catalog(&[offered]))]);
    let manager = remote(&core, &server, &key, Arc::default());
    core.block_on(manager.refresh()).unwrap();
    core.block_on(async {
        tokio::time::timeout(Duration::from_secs(10), async {
            while !matches!(manager.status()[0].operation, OperationState::Error { .. }) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
    });
    assert_eq!(core.core.resolve_extension("ocr"), Some(previous.clone()));
    assert_eq!(std::fs::read(previous).unwrap(), b"MZ fake baseline");
}
