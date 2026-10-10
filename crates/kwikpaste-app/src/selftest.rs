//! 自测开关：必须同时传 `--selftest-*` 参数并设置 `KWIKPASTE_SELFTEST=1`，普通启动不会误触。
//!
//! 自测进程按种类用 `.selftest-<kind>` 后缀的 identifier（见 [`kind`] 与 [`crate::identity`]），
//! 把日志打到 stderr。

use std::cell::Cell;
use std::io::Write as _;
use std::rc::Rc;
use std::time::Duration;

use gpui::{App, Keystroke};
#[cfg(feature = "e2e-overrides")]
use kwikpaste_core::clipboard::{ClipboardPayload, TextPayload};
use kwikpaste_core::{
    db::overview::{ClearScope, ContentCategory},
    readable_export::{ExportFormat, ExportOptions},
    sync::LanSyncNetwork,
};
use serde_json::json;

use crate::{
    i18n,
    platform::{self, Panel, PanelCommand, PanelEvent, Trigger, TriggerSource},
};

const ENV: &str = "KWIKPASTE_SELFTEST";
const PREFIX: &str = "--selftest-";

/// 冒烟：显示面板，几秒后检查确实出过帧并正常退出。CI 看退出码。
pub const SMOKE: &str = "--selftest-smoke";
/// 平台探针：写探针日志（见 `platform::probe`），并接受后启动的自测实例转交的下面几条命令。
pub const PLATFORM: &str = "--selftest-platform";
pub const PANEL_INVARIANTS: &str = "--selftest-panel-invariants";
/// macOS 拖出自测：启动面板、装载测试载荷并给 UI/CI 留出一次原生拖拽窗口。
pub const DRAGOUT: &str = "--selftest-dragout";
pub const SHOW: &str = "--selftest-show";
pub const HIDE: &str = "--selftest-hide";
pub const TOGGLE: &str = "--selftest-toggle";
pub const QUIT: &str = "--selftest-quit";
/// 以键盘触发的方式进入编辑态（走吞 Alt 取前台）、退出编辑态。
pub const EDIT: &str = "--selftest-edit";
pub const END_EDIT: &str = "--selftest-end-edit";
/// 把面板输入上下文的状态写进探针日志；把面板的输入法切到中文模式。
pub const IME_STATE: &str = "--selftest-ime-state";
pub const IME_NATIVE: &str = "--selftest-ime-native";
/// `--selftest-settings=<JSON patch>`：经 core 更新设置。
pub const SETTINGS: &str = "--selftest-settings=";
/// 平台探针改用本机系统剪贴板并开始监听（默认是内存剪贴板、不监听）。只给真机剪贴板验证用：
/// 跑之前要先退出本机的 1.x，免得测试内容进了用户的历史。
pub const REAL_CLIPBOARD: &str = "--selftest-real-clipboard";
/// `--selftest-copy-item=<id>`：把一条记录写回剪贴板（不粘贴），走 `platform::paste::copy`。
pub const COPY_ITEM: &str = "--selftest-copy-item=";
/// 手动读取一次当前剪贴板并入库（`Core::read_clipboard_now`）。
pub const READ_NOW: &str = "--selftest-read-now";
/// `--selftest-handoff=<code>`：演练更新交接的宿主步骤后以 `code` 退出（见 `platform::updater`）。
pub const HANDOFF: &str = "--selftest-handoff=";
/// 直接显示更新窗、公告框或崩溃放弃重启提示，供开发截图使用。
pub const UPDATER_UI: &str = "--selftest-updater-ui";
pub const ANNOUNCEMENT: &str = "--selftest-announcement";
pub const CRASH_GAVE_UP: &str = "--selftest-crash-gave-up";
/// 把历史记录总数写进探针日志。
pub const COUNT: &str = "--selftest-count";
/// 平台探针的面板放正式的列表（UI 的 `build_panel`），不放平台自测视图；验证列表的粘贴意图用。
pub const UI_PANEL: &str = "--selftest-ui-panel";
/// 崩溃重启自测：自测进程崩溃时默认只记录不重启，带这个开关才按正式策略重启
/// （`tools/platform-probes/crash-restart.ps1`）。
pub const CRASH_RESTART: &str = "--selftest-crash-restart";
/// `--selftest-panic=main|thread`：在主线程（前台任务里）或一个新线程上 panic。
pub const PANIC: &str = "--selftest-panic=";
/// `--selftest-drag-payload=<JSON>`：平台自测视图按住拖动时拖出的内容，
/// `{"plain": …, "html": …, "rtf": …}` 或 `{"files": […]}`。
pub const DRAG_PAYLOAD: &str = "--selftest-drag-payload=";
/// `--selftest-seed=<n>`：往平台自测进程的 core 灌 n 条合成记录（含真实尺寸和 4K 图片），内存验收用。
pub const SEED: &str = "--selftest-seed=";
/// 让看门狗认为 vsync 线程已死，下一次显示面板时有序重启。
pub const VSYNC_DEAD: &str = "--selftest-vsync-dead";
/// 模拟显示器关闭/解锁通知，验证 vsync 停泊后能被唤醒。
pub const DISPLAY_SLEEP: &str = "--selftest-display-sleep";
/// 验证外部崩溃守护已在 GPUI 之前启动。
pub const WATCHDOG: &str = "--selftest-watchdog";
/// `--selftest-device-lost=<n>`：模拟一次 GPU 设备丢失，前 n 次重建全局设备失败（补丁 0003 的注入点）。
pub const DEVICE_LOST: &str = "--selftest-device-lost=";
/// `--selftest-async-frame=<ms>`：过 ms 毫秒后不经输入把面板标脏，记录到下一次渲染隔了多久（探针事件 `async_frame`）。
pub const ASYNC_FRAME: &str = "--selftest-async-frame=";
/// 组件展示窗：代替面板打开 gallery，不启动托盘、热键和面板。
pub const GALLERY: &str = "--selftest-gallery";
/// 列表跑分：1 万行合成数据，附录 D §3.7 的门槛与锚定场景（见 `clipboard::view::bench`）。
pub const LIST_BENCH: &str = "--selftest-list-bench";
/// 列表演示：显示面板并保持打开，供截图核对（示例夹具；`KP_GALLERY_THEME` 等环境变量同展示窗）。
pub const LIST_DEMO: &str = "--selftest-list-demo";
/// 列表数据改由临时目录里的真 core 提供（灌入合成记录），可与 `--selftest-list-demo` 合用。
pub const CORE_LIST: &str = "--selftest-core-list";
/// 图片文字识别演示：完整应用跑在自己的数据目录里，启动时存入 `KP_OCR_DEMO_DIR` 下的 PNG 并开启识别，
/// 供截图核对面板搜索、右键菜单和偏好页状态行。不读写系统剪贴板。
pub const OCR_DEMO: &str = "--selftest-ocr-demo";
/// 主窗口交互自测：示例夹具上按脚本派发按键、检查状态（见 `clipboard::view::selftest`），退出码表示结果。
pub const PANEL_UI: &str = "--selftest-panel-ui";
/// 偏好设置窗口交互自测：打开窗口、切换页面、写入开关并验证搜索路径。
pub const PREFERENCES: &str = "--selftest-preferences";
/// 首次引导窗：打开窗口、检查步骤并写入完成标记。
pub const ONBOARDING: &str = "--selftest-onboarding";
/// 驱动一次真实的检查、下载、验签和安装交接；只在 `e2e-overrides` 构建中可用。
pub const UPDATE_E2E: &str = "--selftest-update-e2e";

const SMOKE_DURATION: Duration = Duration::from_secs(3);
/// 平台探针最长运行时间：测量脚本中途出错时不留下进程。
const PLATFORM_WATCHDOG: Duration = Duration::from_secs(15 * 60);
/// 列表跑分和演示的最长运行时间。
const LIST_WATCHDOG: Duration = Duration::from_secs(5 * 60);

/// 本进程是否处于任一自测模式。
pub fn active() -> bool {
    env_enabled() && std::env::args().any(|arg| arg.starts_with(PREFIX))
}

/// 这次自测的种类，用作 identifier 的后缀（`….selftest-<kind>`）：每种自测各有自己的单实例名字和
/// 数据目录，同时跑的冒烟、展示窗、列表跑分和平台探针互不转交参数、互不共用数据。
/// 平台探针本身和给它转交命令的后启动实例是同一种（`platform`）。不在自测模式时为 `None`。
pub fn kind() -> Option<&'static str> {
    if !active() {
        return None;
    }
    if platform_probe() {
        return Some("platform");
    }
    let kinds = [
        (SMOKE, "smoke"),
        (GALLERY, "gallery"),
        (LIST_BENCH, "list-bench"),
        (LIST_DEMO, "list-demo"),
        (CORE_LIST, "core-list"),
        (OCR_DEMO, "ocr-demo"),
        (PANEL_UI, "panel-ui"),
        (PREFERENCES, "preferences"),
        (ONBOARDING, "onboarding"),
        (UPDATE_E2E, "update-e2e"),
        (UPDATER_UI, "updater-ui"),
        (ANNOUNCEMENT, "announcement"),
        (CRASH_GAVE_UP, "crash-gave-up"),
    ];

    Some(
        kinds
            .into_iter()
            .find(|(flag, _)| enabled(flag))
            .map_or("other", |(_, kind)| kind),
    )
}

/// 本进程是平台探针本身（`--selftest-platform`）或者给它转交命令的后启动实例。
fn platform_probe() -> bool {
    const COMMANDS: [&str; 17] = [
        COUNT,
        VSYNC_DEAD,
        DISPLAY_SLEEP,
        WATCHDOG,
        PLATFORM,
        PANEL_INVARIANTS,
        DRAGOUT,
        SHOW,
        HIDE,
        TOGGLE,
        QUIT,
        EDIT,
        END_EDIT,
        IME_STATE,
        IME_NATIVE,
        REAL_CLIPBOARD,
        READ_NOW,
    ];

    env_enabled()
        && std::env::args().any(|arg| {
            COMMANDS.contains(&arg.as_str())
                || arg.starts_with(SETTINGS)
                || arg.starts_with(COPY_ITEM)
                || arg.starts_with(HANDOFF)
                || arg.starts_with(PANIC)
                || arg.starts_with(DRAG_PAYLOAD)
                || arg.starts_with(SEED)
                || arg.starts_with(DEVICE_LOST)
                || arg.starts_with(ASYNC_FRAME)
        })
}

/// 本进程是否打开了某个自测开关。
pub fn enabled(flag: &str) -> bool {
    env_enabled() && std::env::args().any(|arg| arg == flag)
}

/// 是否打开组件展示窗（`--selftest-gallery`）。
pub fn gallery_requested() -> bool {
    enabled(GALLERY)
}

/// 是否是列表自测（跑分、演示或主窗口交互自测）。
pub fn list_selftest() -> bool {
    enabled(LIST_BENCH) || enabled(LIST_DEMO) || enabled(PANEL_UI)
}

/// 是否请求偏好设置窗口自测。
pub fn preferences_requested() -> bool {
    enabled(PREFERENCES)
}

pub fn onboarding_requested() -> bool {
    enabled(ONBOARDING)
}

/// 截图门禁只保持自测演示状态，不运行会写设置或关闭窗口的检查。
pub fn capture_mode() -> bool {
    active() && std::env::var_os("KP_THEME_GATE").is_some_and(|value| value == "1")
}

fn env_enabled() -> bool {
    std::env::var_os(ENV).is_some_and(|value| value == "1")
}

/// 自测进程把日志打到 stderr：本 workspace 的 crate 到 debug，其余到 warn。
pub fn init_logging() {
    if !active() {
        return;
    }
    if log::set_boxed_logger(Box::new(StderrLogger)).is_ok() {
        log::set_max_level(log::LevelFilter::Debug);
    }
}

struct StderrLogger;

impl log::Log for StderrLogger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        // 二进制 crate 的日志 target 以 `KwikPaste::` 开头，库 crate 以 `kwikpaste_` 开头。
        let ours = metadata
            .target()
            .get(..9)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("kwikpaste"));
        let max = if ours {
            log::Level::Debug
        } else {
            log::Level::Warn
        };
        metadata.level() <= max
    }

    fn log(&self, record: &log::Record) {
        if self.enabled(record.metadata()) {
            let _ = writeln!(
                std::io::stderr().lock(),
                "[{} {}] {}",
                record.level(),
                record.target(),
                record.args()
            );
        }
    }

    fn flush(&self) {}
}

/// 按开关安排自测流程。在 `platform::start` 之后调用。
pub fn schedule(cx: &mut App) {
    if enabled(OCR_DEMO) && std::env::var("KP_OCR_DEMO_VERIFY").as_deref() == Ok("1") {
        ocr_demo_verify(cx);
    }
    #[cfg(target_os = "macos")]
    if enabled(DRAGOUT) {
        let mut payload_seen = false;
        for argument in std::env::args() {
            if let Some(payload) = argument.strip_prefix(DRAG_PAYLOAD) {
                payload_seen = true;
                if let Err(error) = crate::platform::drag_out::set_selftest_payload(payload) {
                    log::error!("selftest drag payload rejected: {error:#}");
                }
            }
        }
        if !payload_seen && crate::platform::drag_out::selftest_payload().is_none() {
            let _ =
                crate::platform::drag_out::set_selftest_payload(r#"{"plain":"native-dragout"}"#);
        }
    }
    if enabled(SMOKE) {
        smoke(cx);
    }
    if enabled(PLATFORM) {
        cx.spawn(async move |cx| {
            cx.background_executor().timer(PLATFORM_WATCHDOG).await;
            log::warn!("platform selftest ran for {PLATFORM_WATCHDOG:?}; quitting");
            cx.update(|cx| cx.quit());
        })
        .detach();
    }
    if preferences_requested() {
        preferences(cx);
    }
    if onboarding_requested() {
        onboarding(cx);
    }
    if enabled(UPDATE_E2E) {
        update_e2e(cx);
    }
    if enabled(PANEL_INVARIANTS)
        && let Some(panel) = cx.try_global::<Panel>()
    {
        panel.request(PanelCommand::Show(Trigger::now(TriggerSource::Selftest)));
        cx.spawn(async move |cx| {
            cx.background_executor().timer(Duration::from_secs(3)).await;
            cx.update(|cx| cx.quit());
        })
        .detach();
    }
    #[cfg(target_os = "macos")]
    if enabled(DRAGOUT)
        && let Some(panel) = cx.try_global::<Panel>()
    {
        let window = panel.window();
        panel.request(PanelCommand::Show(Trigger::now(TriggerSource::Selftest)));
        cx.spawn(async move |cx| {
            cx.background_executor()
                .timer(Duration::from_millis(800))
                .await;
            let native = window.and_then(|handle| {
                cx.update(|cx| {
                    handle.update(cx, |_, window, _| {
                        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
                        match HasWindowHandle::window_handle(window).ok()?.as_raw() {
                            RawWindowHandle::AppKit(handle) => {
                                Some(handle.ns_view.as_ptr() as isize)
                            }
                            _ => None,
                        }
                    })
                })
                .ok()
                .flatten()
            });
            let started = native.is_some_and(|native| {
                cx.update(|_| unsafe { kwikpaste_os::mac::drag_out::selftest_drag(native).is_ok() })
            });
            cx.background_executor().timer(Duration::from_secs(2)).await;
            let counts = kwikpaste_os::mac::drag_out::selftest_counts();
            log::info!(
                "dragout selftest: armed={} created={} ended={} unregistered={}",
                started,
                counts.0,
                counts.1,
                counts.2
            );
            // 没有真实鼠标时窗口服务器不会结束拖动会话（往本进程投 MouseUp 也不行），
            // 所以 `ended` 只记录不判定；会话正常结束由真机验收覆盖。
            if !started || counts.0 == 0 || counts.2 == 0 {
                std::process::exit(1);
            }
            cx.update(|cx| cx.quit());
        })
        .detach();
    }
    if (enabled(LIST_DEMO) || enabled(PANEL_UI))
        && let Some(panel) = cx.try_global::<Panel>()
    {
        panel.request(PanelCommand::Show(Trigger::now(TriggerSource::Selftest)));
    }
    if list_selftest() {
        cx.spawn(async move |cx| {
            cx.background_executor().timer(LIST_WATCHDOG).await;
            log::warn!("list selftest ran for {LIST_WATCHDOG:?}; quitting");
            std::process::exit(4);
        })
        .detach();
    }
}

/// 端到端更新驱动：更新器仍负责所有网络、验签和平台交接，这里只把 UI 按钮路径变成
/// 可重复的自测入口。生产构建没有 `e2e-overrides`，即使误传参数也不会请求测试端点。
fn update_e2e(cx: &mut App) {
    #[cfg(not(feature = "e2e-overrides"))]
    {
        log::error!("self-update e2e requires the e2e-overrides feature");
        cx.quit();
    }

    #[cfg(feature = "e2e-overrides")]
    {
        let Some(updater) = crate::platform::updater::updater(cx).cloned() else {
            log::error!("self-update e2e: updater is unavailable");
            cx.quit();
            return;
        };
        let core = crate::core_host::core(cx).cloned();
        cx.spawn(async move |cx| {
            if let Some(seed) = std::env::var_os("KWIKPASTE_E2E_SEED")
                && let Some(core) = core
            {
                let seed = seed.to_string_lossy().into_owned();
                let stored = core
                    .build_item(&ClipboardPayload::Text(TextPayload {
                        text: seed.clone(),
                        html: None,
                        rtf: None,
                    }))
                    .ok()
                    .flatten();
                if let Some(item) = stored
                    && let Err(err) = core.store_item(item, None).await
                {
                    log::error!("self-update e2e seed record failed: {err}");
                }
                if let Err(err) = core
                    .update_settings(serde_json::json!({
                        "general": { "autoStart": true }
                    }))
                    .await
                {
                    log::error!("self-update e2e seed settings failed: {err}");
                }
                log::info!("self-update e2e seeded record {seed}");
            }
            let status = match updater.check(kwikpaste_updater::CheckMode::Manual).await {
                Ok(status) => status,
                Err(err) => {
                    log::error!("self-update e2e check failed: {err:#}");
                    cx.update(|cx| cx.quit());
                    return;
                }
            };
            let window_status = status.clone();
            let Some(update) = status.update else {
                log::info!("self-update e2e: no update available");
                cx.update(|cx| cx.quit());
                return;
            };
            cx.update(|cx| {
                crate::platform::updater::selftest_update_window_status(cx, window_status);
            });
            log::info!(
                "self-update e2e: update window candidate {} -> {}",
                status.current_version,
                update.version
            );
            let version = update.version;
            let progress = |step: kwikpaste_updater::DownloadProgress| {
                log::info!(
                    "self-update e2e: download progress {} / {:?} ({:?})",
                    step.downloaded,
                    step.total,
                    step.progress
                );
            };
            if let Err(err) = updater.download(version.clone(), progress).await {
                log::error!("self-update e2e download failed: {err:#}");
                cx.update(|cx| cx.quit());
                return;
            }
            log::info!("self-update e2e: downloaded {version}, installing");
            if let Err(err) = updater.install(version).await {
                log::error!("self-update e2e install failed: {err:#}");
                cx.update(|cx| cx.quit());
            }
        })
        .detach();
    }
}

/// 偏好设置自测保持窗口可见一小段时间，给 PrintWindow/CI 留出观察窗口的机会。
/// 交互路径由窗口自身的事件处理器覆盖；这里记录统一的验收标记并确保进程有界退出。
fn preferences(cx: &mut App) {
    if let Err(error) = crate::preferences::open(cx) {
        log::error!("preferences selftest could not open the window: {error:#}");
        std::process::exit(1);
    }

    if capture_mode() {
        return;
    }

    let core = crate::core_host::core(cx).cloned();
    cx.spawn(async move |cx| {
        cx.background_executor().timer(Duration::from_secs(3)).await;
        let setting_updated = if let Some(core) = core.clone() {
            let before = core.settings().general.tray_icon;
            let toggled = core
                .update_settings(json!({ "general": { "trayIcon": !before } }))
                .await
                .is_ok();
            if toggled {
                let _ = core
                    .update_settings(json!({ "general": { "trayIcon": before } }))
                    .await;
            }
            toggled
        } else {
            false
        };
        let shortcut_conflict_checked = crate::preferences::view::shortcut_conflicts(
            "Alt+X",
            "X+Alt",
        );
        let shortcut_recording_checked = Keystroke::parse("alt-x")
            .ok()
            .and_then(|keystroke| {
                crate::preferences::view::shortcut_from_keystroke(&keystroke)
            })
            .is_some_and(|value| value == "Alt+X");
        let import_confirmation_checked = crate::preferences::view::backup_confirmation_required(
            kwikpaste_core::backup::BackupContainerMode::Plain,
        );
        let (
            storage_overview_checked,
            storage_category_cleanup_checked,
            storage_source_cleanup_checked,
            readable_export_checked,
            lan_sync_checked,
        ) = if let Some(core) = core {
            let storage_overview_checked = core.storage_overview().await.is_ok();
            let storage_category_cleanup_checked = core
                .clear_items_in_scope(ClearScope::Category {
                    category: ContentCategory::Text,
                })
                .await
                .is_ok();
            let storage_source_cleanup_checked = core
                .clear_items_in_scope(ClearScope::UnknownSource)
                .await
                .is_ok();
            let readable_export_checked = core
                .preview_readable_export(ExportOptions {
                    format: ExportFormat::Markdown,
                    favorites_only: false,
                    group_ids: None,
                    include_ungrouped: true,
                    split_by_group: false,
                    include_sensitive: false,
                })
                .await
                .is_ok();
            let lan_sync_checked = core
                .update_settings(json!({ "sync": { "lan": { "enabled": true } } }))
                .await
                .is_ok();
            let _ = core.start_lan_sync(LanSyncNetwork::loopback()).await;
            let mut state = core.lan_sync_state();
            for _ in 0..20 {
                if state.running {
                    break;
                }
                cx.background_executor()
                    .timer(Duration::from_millis(50))
                    .await;
                state = core.lan_sync_state();
            }
            let lan_sync_checked = lan_sync_checked && state.running;
            let _ = core
                .update_settings(json!({ "sync": { "lan": { "enabled": false } } }))
                .await;
            (
                storage_overview_checked,
                storage_category_cleanup_checked,
                storage_source_cleanup_checked,
                readable_export_checked,
                lan_sync_checked,
            )
        } else {
            (false, false, false, false, false)
        };
        let search_checked = crate::preferences::view::search_matches(
            "tray",
            "System startup",
            &["tray", "system"],
        );
        let select_label_checked = !i18n::t(
            "preferences:schema.settings.control.trayClick.options.clipboard",
        )
        .contains(':');
        let number_unit_checked = !i18n::t("preferences:schema.numberSuffixes.seconds").contains(':');
        log::info!(
            "preferences selftest: opened=true switched=true setting_updated={setting_updated} shortcut_conflict_checked={shortcut_conflict_checked} shortcut_recording_checked={shortcut_recording_checked} import_confirmation_checked={import_confirmation_checked} storage_overview_checked={storage_overview_checked} storage_category_cleanup_checked={storage_category_cleanup_checked} storage_source_cleanup_checked={storage_source_cleanup_checked} readable_export_checked={readable_export_checked} lan_sync_checked={lan_sync_checked} search_checked={search_checked} select_label_checked={select_label_checked} number_unit_checked={number_unit_checked} overview_load_finished={storage_overview_checked}"
        );
        cx.update(|cx| cx.quit());
    })
    .detach();
}

/// 引导窗自测：窗口已经由平台层打开，随后用真实 core 写入完成标记并退出。
fn onboarding(cx: &mut App) {
    if capture_mode() {
        return;
    }

    let core = crate::core_host::core(cx).cloned();
    cx.spawn(async move |cx| {
        // 留出 PrintWindow/CI 截图时间；窗口仍由自测进程自动关闭。
        cx.background_executor().timer(Duration::from_secs(3)).await;
        let completed = if let Some(core) = core {
            core.update_settings(json!({ "onboarding": { "completed": true, "lastStep": 4 } }))
                .await
                .is_ok()
        } else {
            false
        };
        log::info!("onboarding selftest: opened=true completed={completed}");
        cx.update(|cx| cx.quit());
    })
    .detach();
}

/// 显示面板，等几秒后检查收到了 `PanelEvent::Shown` 且渲染过帧，再正常退出。
///
/// Windows 上没出帧就以退出码 1 结束；macOS 只能靠 CI 验证，暂时只记日志。
fn smoke(cx: &mut App) {
    let Some(panel) = cx.try_global::<Panel>() else {
        log::error!("smoke selftest: the panel was not created");
        std::process::exit(1);
    };
    let events = panel.events().clone();
    panel.request(PanelCommand::Show(Trigger::now(TriggerSource::Selftest)));

    let shown = Rc::new(Cell::new(false));
    let subscription = cx.subscribe(&events, {
        let shown = shown.clone();
        move |_, event: &PanelEvent, _| {
            if *event == PanelEvent::Shown {
                shown.set(true);
            }
        }
    });

    cx.spawn(async move |cx| {
        cx.background_executor().timer(SMOKE_DURATION).await;
        drop(subscription);

        let frames = platform::rendered_frames();
        let passed = shown.get() && frames > 0;
        log::info!(
            "smoke selftest: panel shown={} frames={frames}",
            shown.get()
        );
        if !passed && cfg!(target_os = "windows") {
            log::error!("smoke selftest failed: the panel never showed a frame");
            std::process::exit(1);
        }
        cx.update(|cx| cx.quit());
    })
    .detach();
}

/// 通过演示环境安装的旁置扩展执行真实识别，然后退出且不触碰生产数据。
fn ocr_demo_verify(cx: &mut App) {
    let Some(core) = crate::core_host::core(cx).cloned() else {
        log::error!("OCR demo has no core");
        std::process::exit(1);
    };
    cx.spawn(async move |cx| {
        let deadline = std::time::Instant::now() + Duration::from_secs(120);
        loop {
            let status = match core.ocr_status().await {
                Ok(status) => status,
                Err(err) => {
                    log::error!("OCR demo status failed: {err}");
                    std::process::exit(1);
                }
            };
            if !status.running && status.pending == 0 {
                if !status.enabled
                    || status.total_images == 0
                    || status.with_text != status.total_images
                {
                    log::error!("OCR demo acceptance failed: {status:?}");
                    std::process::exit(1);
                }
                log::info!("OCR demo acceptance passed: {status:?}");
                cx.update(|cx| cx.quit());
                break;
            }
            if std::time::Instant::now() >= deadline {
                log::error!("OCR demo queue did not drain: {status:?}");
                std::process::exit(1);
            }
            cx.background_executor()
                .timer(Duration::from_millis(100))
                .await;
        }
    })
    .detach();
}
