//! 更新器整条链路：本机 HTTP 服务上的清单与安装包、一次性密钥验签、假宿主与假启动器交接。
//! 不请求任何线上地址，不运行安装包，不替换本机的任何程序。

use std::sync::Mutex;

use serde_json::json;
use url::Url;

use super::*;
use crate::handoff::testing::{FakeLauncher, Journal, RecordingHost, recorders};
use crate::http::testing::{Reply, Server};
use crate::testing::TestCore;
use crate::verify::testing::TestKey;

#[derive(Default)]
struct NullUi {
    available: Mutex<Vec<UpdateStatus>>,
    prompts: Mutex<Vec<UpdateStatus>>,
    idle: Mutex<Option<Duration>>,
}

impl UpdaterUi for NullUi {
    fn idle_duration(&self) -> HostFuture<'_, Option<Duration>> {
        let idle = *self.idle.lock().unwrap();
        Box::pin(async move { idle })
    }

    fn prompt_update(&self, status: UpdateStatus) {
        self.prompts.lock().unwrap().push(status);
    }

    fn update_available(&self, status: UpdateStatus) {
        self.available.lock().unwrap().push(status);
    }

    fn show_announcement(
        &self,
        _prompt: AnnouncementPrompt,
    ) -> HostFuture<'_, AnnouncementOutcome> {
        Box::pin(async { AnnouncementOutcome::Closed })
    }

    fn open_url(&self, _url: &str) {}
}

fn platform_keys() -> serde_json::Value {
    let mut platforms = serde_json::Map::new();
    for key in InstallKind::Nsis
        .platform_keys()
        .into_iter()
        .chain(InstallKind::Portable.platform_keys())
    {
        platforms.insert(key, json!({}));
    }
    serde_json::Value::Object(platforms)
}

/// 发布流水线给这种形态、这个版本起的产物名，签名的 trusted comment 记的就是它。
fn asset(kind: &InstallKind, version: &str) -> String {
    format!("KwikPaste{}", kind.asset_suffix(version).unwrap())
}

/// 一份清单：所有 Windows 平台键都指向同一个安装包地址。
fn manifest(version: &str, url: &str, signature: &str) -> Vec<u8> {
    let mut platforms = platform_keys();
    for value in platforms.as_object_mut().unwrap().values_mut() {
        *value = json!({ "url": url, "signature": signature });
    }
    serde_json::to_vec(&json!({
        "version": version,
        "notes": "notes",
        "pub_date": "2027-04-05T08:00:00Z",
        "platforms": platforms,
    }))
    .unwrap()
}

struct Setup {
    test: TestCore,
    updater: Updater,
    journal: Journal,
    ui: Arc<NullUi>,
}

fn setup(kind: InstallKind, endpoint: Url, key: &TestKey, fails: bool) -> Setup {
    setup_writable(kind, endpoint, key, fails, false)
}

/// 用临时安装位置测试直接写探测；不可写夹具以普通文件占住父目录，任何账户都不能在其中创建文件。
fn setup_writable(
    kind: InstallKind,
    endpoint: Url,
    key: &TestKey,
    fails: bool,
    writable: bool,
) -> Setup {
    let test = TestCore::start("2.0.0");
    let directory = test.root().join("install");
    if writable {
        std::fs::create_dir(&directory).unwrap();
    } else {
        std::fs::write(&directory, b"not a writable directory").unwrap();
    }
    let (host, launcher, journal): (RecordingHost, FakeLauncher, Journal) = recorders(fails);
    let ui = Arc::new(NullUi::default());
    let updater = Updater::with_parts(
        test.core.clone(),
        ui.clone(),
        Arc::new(host),
        Parts {
            kind,
            exe: directory.join("KwikPaste.exe"),
            launcher: Box::new(launcher),
            endpoints: Box::new(move || Ok(vec![endpoint.clone()])),
            public_key: key.public_key(),
        },
    )
    .unwrap();
    Setup {
        test,
        updater,
        journal,
        ui,
    }
}

/// 清除本用例下载的临时包，不触碰其他实例的更新目录。
fn remove_package(updater: &Updater) {
    if let Some(package) = lock(&updater.0.pending)
        .as_ref()
        .and_then(|pending| pending.package.as_ref())
    {
        std::fs::remove_dir_all(&package.dir).unwrap();
    }
}

#[test]
fn silent_update_downloads_without_prompt_or_badge_and_waits_for_idle() {
    let key = TestKey::generate();
    let installer = b"MZ silent installer";
    let files = Server::start(vec![Reply::ok(installer.to_vec())]);
    let body = manifest(
        "2.0.1",
        &files.url("/setup.exe"),
        &key.sign_as(installer, &asset(&InstallKind::Nsis, "2.0.1")),
    );
    let manifests = Server::start(vec![Reply::ok(body.clone()), Reply::ok(body)]);
    let Setup {
        test,
        updater,
        journal,
        ui,
    } = setup_writable(
        InstallKind::Nsis,
        Url::parse(&manifests.url("/latest.json")).unwrap(),
        &key,
        false,
        true,
    );

    assert!(test.block_on(updater.run_auto_check()));
    assert!(updater.status().update.unwrap().downloaded);
    assert!(test.block_on(updater.run_auto_check()));
    assert_eq!(
        files.requests().len(),
        1,
        "already verified package is reused"
    );
    assert!(ui.prompts.lock().unwrap().is_empty());
    assert!(
        ui.available
            .lock()
            .unwrap()
            .iter()
            .all(|status| status.update.is_none())
    );
    assert!(test.core.settings().update.prompted_version.is_none());
    assert!(
        std::fs::read_dir(updater.0.exe.parent().unwrap())
            .unwrap()
            .next()
            .is_none()
    );

    // None 表示仍有窗口（或查询失败）；系统即使已空闲也不能安装。
    test.block_on(updater.install_when_idle()).unwrap();
    assert!(journal.lock().unwrap().is_empty());
    *ui.idle.lock().unwrap() = Some(Duration::from_secs(599));
    test.block_on(updater.install_when_idle()).unwrap();
    assert!(journal.lock().unwrap().is_empty());

    *ui.idle.lock().unwrap() = Some(Duration::from_secs(600));
    test.block_on(updater.install_when_idle()).unwrap();
    let events = journal.lock().unwrap();
    assert_eq!(events[0], "host: stop_input");
    assert!(
        events[1].starts_with("shell_execute KwikPaste-2.0.1-installer.exe /S /R /UPDATE /ARGS")
    );
    assert!(events[1].contains(kwikpaste_os::autostart::AUTO_LAUNCH_ARG));
    assert_eq!(events.last().unwrap(), "host: exit(0)");
    assert!(ui.prompts.lock().unwrap().is_empty());
    remove_package(&updater);
}

#[test]
fn non_writable_install_still_prompts_once_per_version_and_keeps_badge() {
    let key = TestKey::generate();
    let body = manifest("2.0.1", "https://dl.example.com/setup.exe", "c2ln");
    let server = Server::start(vec![Reply::ok(body.clone()), Reply::ok(body)]);
    let Setup {
        test,
        updater,
        journal,
        ui,
    } = setup(
        InstallKind::Nsis,
        Url::parse(&server.url("/latest.json")).unwrap(),
        &key,
        false,
    );
    assert!(!updater.0.kind.can_update_silently(&updater.0.exe));
    for _ in 0..2 {
        assert!(test.block_on(updater.run_auto_check()));
    }
    assert_eq!(ui.prompts.lock().unwrap().len(), 1);
    assert!(
        ui.available
            .lock()
            .unwrap()
            .iter()
            .all(|status| status.update.is_some())
    );
    assert_eq!(
        test.core.settings().update.prompted_version.as_deref(),
        Some("2.0.1")
    );
    assert!(!updater.status().update.unwrap().downloaded);
    *ui.idle.lock().unwrap() = Some(Duration::from_secs(600));
    test.block_on(updater.install_when_idle()).unwrap();
    assert!(journal.lock().unwrap().is_empty());
}

#[test]
fn failed_handoff_version_falls_back_to_prompt_for_this_run() {
    let key = TestKey::generate();
    let installer = b"MZ installer";
    let files = Server::start(vec![Reply::ok(installer.to_vec())]);
    let body = manifest(
        "2.0.1",
        &files.url("/setup.exe"),
        &key.sign_as(installer, &asset(&InstallKind::Nsis, "2.0.1")),
    );
    let server = Server::start(vec![Reply::ok(body.clone()), Reply::ok(body)]);
    let Setup {
        test,
        updater,
        journal,
        ui,
    } = setup_writable(
        InstallKind::Nsis,
        Url::parse(&server.url("/latest.json")).unwrap(),
        &key,
        true,
        true,
    );
    assert!(test.block_on(updater.run_auto_check()));
    *ui.idle.lock().unwrap() = Some(Duration::from_secs(600));
    assert!(test.block_on(updater.install_when_idle()).is_err());
    assert!(
        journal.lock().unwrap().iter().any(
            |event| event.starts_with("spawn KwikPaste.exe") && event.contains("--auto-launch")
        )
    );
    let record = take_handoff(test.core.paths()).unwrap();
    assert!(!record.succeeded(&test.core.info().version));
    assert!(take_handoff(test.core.paths()).is_none());
    remove_package(&updater);

    // 新进程消费同一条交接记录，不增加任何持久化设置。
    let Setup {
        test,
        updater,
        journal,
        ui,
    } = setup_writable(
        InstallKind::Nsis,
        Url::parse(&server.url("/latest.json")).unwrap(),
        &key,
        false,
        true,
    );
    updater.record_handoff(record);
    assert!(test.block_on(updater.run_auto_check()));
    assert_eq!(ui.prompts.lock().unwrap().len(), 1);
    assert!(ui.available.lock().unwrap()[0].update.is_some());
    assert_eq!(files.requests().len(), 1);
    assert!(updater.silent_ready_version().is_none());
    assert!(journal.lock().unwrap().is_empty());
}

#[test]
fn silent_nsis_parameters_and_hidden_restart_arguments_preserve_manual_mode() {
    use crate::handoff::nsis;
    use kwikpaste_os::autostart::AUTO_LAUNCH_ARG;

    let original = vec![OsString::from("a b"), OsString::from("/custom")];
    let args = restart_args(original.clone(), true);
    assert_eq!(
        nsis::parameters(&args, true),
        "/S /R /UPDATE /ARGS \"a b\" \"/custom\" --auto-launch"
    );
    assert_eq!(restart_args(args.clone(), true), args);
    assert_eq!(
        restart_args(Vec::new(), true),
        vec![OsString::from(AUTO_LAUNCH_ARG)]
    );
    assert_eq!(restart_args(original.clone(), false), original);
    #[cfg(target_os = "windows")]
    assert_eq!(
        restart_args(
            vec![OsString::from(
                kwikpaste_os::win::admin::ADMIN_RESTARTED_ARG
            )],
            false
        ),
        Vec::<OsString>::new()
    );
    assert_eq!(nsis::parameters(&[], false), "/P /R /UPDATE /ARGS");
    assert_eq!(scheduler::IDLE_POLL, Duration::from_secs(60));
}

#[test]
fn manual_check_on_writable_install_does_not_download_or_install_automatically() {
    let key = TestKey::generate();
    let server = Server::start(vec![Reply::ok(manifest(
        "2.0.1",
        "https://dl.example.com/setup.exe",
        "c2ln",
    ))]);
    let Setup {
        test,
        updater,
        journal,
        ui,
    } = setup_writable(
        InstallKind::Nsis,
        Url::parse(&server.url("/latest.json")).unwrap(),
        &key,
        false,
        true,
    );
    let status = test.block_on(updater.check()).unwrap();
    assert!(!status.update.unwrap().downloaded);
    assert!(ui.available.lock().unwrap()[0].update.is_some());
    assert!(ui.prompts.lock().unwrap().is_empty());
    assert!(updater.silent_ready_version().is_none());
    assert!(journal.lock().unwrap().is_empty());
}

#[test]
fn silent_download_still_verifies_the_signature() {
    let key = TestKey::generate();
    let files = Server::start(vec![Reply::ok(b"MZ tampered".to_vec())]);
    let server = Server::start(vec![Reply::ok(manifest(
        "2.0.1",
        &files.url("/setup.exe"),
        &key.sign(b"MZ genuine", true),
    ))]);
    let Setup {
        test,
        updater,
        journal,
        ui,
    } = setup_writable(
        InstallKind::Nsis,
        Url::parse(&server.url("/latest.json")).unwrap(),
        &key,
        false,
        true,
    );
    assert!(test.block_on(updater.run_auto_check()));
    assert!(!updater.status().update.unwrap().downloaded);
    *ui.idle.lock().unwrap() = Some(Duration::from_secs(600));
    test.block_on(updater.install_when_idle()).unwrap();
    assert!(journal.lock().unwrap().is_empty());
    assert!(ui.prompts.lock().unwrap().is_empty());
    assert!(test.core.settings().update.prompted_version.is_none());
}

/// 检查 → 下载（带进度、验签）→ 交接给 NSIS 安装包：假启动器只记下要执行的命令。
#[test]
fn check_download_and_install_through_the_nsis_handoff() {
    let key = TestKey::generate();
    let installer = [b"MZ".as_slice(), &[1u8; 50_000]].concat();
    let files = Server::start(vec![Reply::ok(installer.clone())]);
    let installer_url = files.url("/KwikPaste_2.0.1_x64-setup.exe");
    let manifests = Server::start(vec![Reply::ok(manifest(
        "2.0.1",
        &installer_url,
        &key.sign_as(&installer, &asset(&InstallKind::Nsis, "2.0.1")),
    ))]);
    let stable = Url::parse(&manifests.url("/v2/stable/latest.json")).unwrap();
    let Setup {
        test,
        updater,
        journal,
        ..
    } = setup(InstallKind::Nsis, stable, &key, false);

    let status = test.block_on(updater.check()).unwrap();
    let update = status.update.unwrap();
    assert!(status.supported);
    assert_eq!(update.version, "2.0.1");
    assert_eq!(update.download_url, installer_url);
    assert!(!update.downloaded);
    assert_eq!(
        update.release_notes_url,
        "https://github.com/ManSanDADADA/KwikPaste/releases/tag/v2.0.1"
    );
    assert!(test.core.settings().update.last_checked_at.is_some());

    let progress = Arc::new(Mutex::new(Vec::new()));
    let seen = progress.clone();
    let downloaded = test
        .block_on(updater.download("2.0.1".to_owned(), move |step| {
            seen.lock().unwrap().push(step)
        }))
        .unwrap();
    assert!(downloaded.downloaded);
    assert!(updater.status().update.unwrap().downloaded);
    assert_eq!(
        progress.lock().unwrap().last().unwrap().downloaded,
        installer.len() as u64
    );

    test.block_on(updater.install("2.0.1".to_owned())).unwrap();
    let journal = journal.lock().unwrap().clone();
    assert_eq!(journal[0], "host: stop_input");
    assert!(
        journal[1].starts_with("shell_execute KwikPaste-2.0.1-installer.exe /P /R /UPDATE /ARGS")
    );
    assert_eq!(
        &journal[2..],
        [
            "host: remove_tray",
            "host: release_single_instance",
            "host: exit(0)"
        ]
    );
    let record = take_handoff(test.core.paths()).unwrap();
    assert_eq!(
        (
            record.from.as_str(),
            record.to.as_str(),
            record.kind.as_str()
        ),
        ("2.0.0", "2.0.1", "nsis")
    );

    let package = updater
        .0
        .pending
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .package
        .clone()
        .unwrap();
    std::fs::remove_dir_all(package.dir).ok();
}

#[test]
fn a_tampered_package_is_never_installed() {
    let key = TestKey::generate();
    let installer = b"MZ genuine".to_vec();
    let files = Server::start(vec![Reply::ok(b"MZ tampered".to_vec())]);
    let manifests = Server::start(vec![Reply::ok(manifest(
        "2.0.1",
        &files.url("/setup.exe"),
        &key.sign(&installer, true),
    ))]);
    let stable = Url::parse(&manifests.url("/latest.json")).unwrap();
    let Setup {
        test,
        updater,
        journal,
        ..
    } = setup(InstallKind::Nsis, stable, &key, false);

    test.block_on(updater.check()).unwrap();
    assert!(
        test.block_on(updater.download("2.0.1".to_owned(), |_| {}))
            .is_err()
    );
    assert!(test.block_on(updater.install("2.0.1".to_owned())).is_err());
    assert!(journal.lock().unwrap().is_empty());
    assert!(!updater.status().update.unwrap().downloaded);
}

/// 测试版与正式版走同一个渠道，每次检查都提供当前候选版本。
#[test]
fn prereleases_remain_pending_until_manifest_changes() {
    let key = TestKey::generate();
    let server = Server::start(vec![
        Reply::ok(manifest(
            "2.1.0-beta.1",
            "https://dl.example.com/b.exe",
            "c2ln",
        )),
        Reply::ok(manifest(
            "2.1.0-beta.1",
            "https://dl.example.com/b.exe",
            "c2ln",
        )),
        Reply::ok(manifest("2.1.0", "https://dl.example.com/a.exe", "c2ln")),
    ]);
    let Setup { test, updater, .. } = setup(
        InstallKind::Nsis,
        Url::parse(&server.url("/latest.json")).unwrap(),
        &key,
        false,
    );

    let status = test.block_on(updater.check()).unwrap();
    assert_eq!(status.update.unwrap().version, "2.1.0-beta.1");

    let status = test.block_on(updater.check()).unwrap();
    assert_eq!(status.update.unwrap().version, "2.1.0-beta.1");

    let status = test.block_on(updater.check()).unwrap();
    assert_eq!(status.update.unwrap().version, "2.1.0");
}

/// 自动检查不受最近检查时间影响。
#[test]
fn automatic_checks_ignore_recent_check_timestamp() {
    let key = TestKey::generate();
    let server = Server::start(vec![Reply::ok(manifest(
        "2.0.1",
        "https://dl.example.com/a.exe",
        "c2ln",
    ))]);
    let Setup { test, updater, .. } = setup(
        InstallKind::Nsis,
        Url::parse(&server.url("/latest.json")).unwrap(),
        &key,
        false,
    );
    let recently = json!({"update": {"lastCheckedAt": Utc::now().to_rfc3339()}});
    test.block_on(test.core.update_settings(recently)).unwrap();

    test.block_on(updater.run_auto_check());
    let status = updater.status();

    assert_eq!(status.update.unwrap().version, "2.0.1");
    assert_eq!(server.requests().len(), 1);
}

/// 开发构建、手动拷出来的 exe：不检查也不安装，只说明不支持。
#[test]
fn unmanaged_builds_never_check_or_install() {
    let key = TestKey::generate();
    let server = Server::start(vec![Reply::ok(manifest(
        "2.0.1",
        "https://dl.example.com/a.exe",
        "c2ln",
    ))]);
    let Setup {
        test,
        updater,
        journal,
        ..
    } = setup(
        InstallKind::Unmanaged,
        Url::parse(&server.url("/latest.json")).unwrap(),
        &key,
        false,
    );

    let status = test.block_on(updater.check()).unwrap();

    assert!(!status.supported);
    assert!(status.update.is_none());
    assert!(server.requests().is_empty());
    assert!(test.block_on(updater.install("2.0.1".to_owned())).is_err());
    assert!(journal.lock().unwrap().is_empty());
}

/// 测试进程本身不在安装目录里：按真实位置判定为开发构建，更新器不工作。
#[test]
fn the_test_binary_is_detected_as_unmanaged() {
    let test = TestCore::start("2.0.0");
    let (host, _, _) = recorders(false);
    let updater = Updater::new(
        test.core.clone(),
        Arc::new(NullUi::default()),
        Arc::new(host),
    )
    .unwrap();

    // 不调用 start()：调度器会清理系统临时目录里一天以前的更新残留，测试不碰本机的这些文件。
    assert_eq!(updater.install_kind(), &InstallKind::Unmanaged);
    assert!(!updater.status().supported);
}

/// 成功检查（含撤回）都发布 UI 状态；网络失败保留候选，下一轮仍可重试。
#[test]
fn background_checks_publish_pending_and_withdrawn_updates_and_keep_failures() {
    let key = TestKey::generate();
    let server = Server::start(vec![
        Reply::ok(manifest("2.0.1", "https://dl.example.com/a.exe", "c2ln")),
        Reply::status("503 Service Unavailable"),
        Reply::ok(manifest("2.0.1", "https://dl.example.com/a.exe", "c2ln")),
        Reply::status("204 No Content"),
    ]);
    let Setup {
        test, updater, ui, ..
    } = setup(
        InstallKind::Nsis,
        Url::parse(&server.url("/latest.json")).unwrap(),
        &key,
        false,
    );
    test.block_on(updater.run_auto_check());
    assert_eq!(updater.status().update.unwrap().version, "2.0.1");
    assert_eq!(ui.available.lock().unwrap().len(), 1);
    test.block_on(updater.run_auto_check());
    assert_eq!(updater.status().update.unwrap().version, "2.0.1");
    assert_eq!(ui.available.lock().unwrap().len(), 1);
    test.block_on(updater.run_auto_check());
    assert_eq!(updater.status().update.unwrap().version, "2.0.1");
    test.block_on(updater.run_auto_check());
    assert!(updater.status().update.is_none());
    let snapshots = ui.available.lock().unwrap();
    assert_eq!(snapshots.len(), 3);
    assert!(snapshots[0].update.is_some());
    assert!(snapshots[1].update.is_some());
    assert!(snapshots[2].update.is_none());
}

#[test]
fn staging_failure_keeps_scheduler_alive_and_exposes_a_manual_retry() {
    use std::io::Write;
    let key = TestKey::generate();
    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    zip.start_file(
        "KwikPaste/KwikPaste.exe",
        zip::write::SimpleFileOptions::default(),
    )
    .unwrap();
    zip.write_all(b"MZ new").unwrap();
    let bytes = zip.finish().unwrap().into_inner();
    let files = Server::start(vec![Reply::ok(bytes.clone())]);
    let body = manifest(
        "2.0.1",
        &files.url("/portable.zip"),
        &key.sign_as(&bytes, &asset(&InstallKind::Portable, "2.0.1")),
    );
    let manifests = Server::start(vec![Reply::ok(body.clone()), Reply::ok(body)]);
    let Setup {
        test,
        updater,
        journal,
        ui,
    } = setup_writable(
        InstallKind::Portable,
        Url::parse(&manifests.url("/latest.json")).unwrap(),
        &key,
        false,
        true,
    );
    std::fs::write(&updater.0.exe, b"MZ old").unwrap();
    assert!(test.block_on(updater.run_auto_check()));
    *ui.idle.lock().unwrap() = Some(Duration::from_secs(600));
    *lock(&updater.0.scheduler) = Some(test.core.runtime().spawn(std::future::pending()));
    let staged = updater.0.exe.with_extension("exe.new");
    std::fs::create_dir(&staged).unwrap();
    assert!(test.block_on(updater.install_when_idle()).is_err());
    assert!(
        lock(&updater.0.scheduler)
            .as_ref()
            .is_some_and(|task| !task.is_finished())
    );
    assert!(journal.lock().unwrap().is_empty());
    assert_eq!(std::fs::read(&updater.0.exe).unwrap(), b"MZ old");
    assert!(test.block_on(test.core.storage_usage()).is_ok());
    assert!(
        ui.available
            .lock()
            .unwrap()
            .last()
            .unwrap()
            .update
            .as_ref()
            .unwrap()
            .downloaded
    );
    assert!(updater.silent_ready_version().is_none());
    assert!(test.block_on(updater.run_auto_check()));
    assert!(updater.silent_ready_version().is_none());
    std::fs::remove_dir(&staged).unwrap();
    test.block_on(updater.install("2.0.1".into())).unwrap();
    assert!(lock(&updater.0.scheduler).is_none());
    assert_eq!(std::fs::read(&updater.0.exe).unwrap(), b"MZ new");
    remove_package(&updater);
}
