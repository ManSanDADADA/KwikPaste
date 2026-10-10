//! 宿主一侧的 kwikpaste-core：建 runtime、启动 core，把 core 的事件送回 GPUI 主线程。
//!
//! 数据目录由 [`crate::identity`] 决定：开发、自测构建用各自的 identifier 和 `AppEnv::Dev`
//! （`%LOCALAPPDATA%\com.fastthree.kwikpaste.native-dev\dev\…`），绝不会落到已安装 1.x 的
//! `com.fastthree.kwikpaste\prod`。只有打开 `production-identity` 的发布构建才用正式的目录。

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::Context as _;
use async_channel::Receiver;
use gpui::{App, Global};
use kwikpaste_core::clipboard::MemoryClipboard;
use kwikpaste_core::{
    AppInfo, Core, CoreEvent, CoreOptions, CorePaths, CoreRuntime, sync::LanSyncNetwork,
};
use kwikpaste_os::services::NativeServices;

/// 运行中的 core 与它的 runtime。作为 GPUI 全局保存，界面经 [`core`] 取用。
pub struct CoreHost {
    core: Core,
    // runtime 要活得比 core 久：core 的任务都跑在它上面。
    _runtime: CoreRuntime,
}

impl Global for CoreHost {}

/// 启动后交给平台层的 core 和它的事件流。
pub struct StartedCore {
    pub host: CoreHost,
    pub events: Receiver<CoreEvent>,
}

/// 建 runtime 并启动 core（读设置、打开并迁移数据库、启动自动清理），再接上平台能力
/// （前台应用、应用扫描、提示音），普通启动时开始监听系统剪贴板。在创建 GPUI 平台之前调用，
/// 期间阻塞当前线程。
pub fn start() -> anyhow::Result<StartedCore> {
    let identity = crate::identity::current();
    let version = semver::Version::parse(env!("CARGO_PKG_VERSION"))
        .context("the crate version is not semver")?;
    let info = AppInfo {
        name: kwikpaste_core::APP_NAME,
        identifier: identity.identifier,
        version,
        env: identity.env,
    };
    let paths = CorePaths::for_native(identity.identifier, identity.env)?;
    let options = CoreOptions {
        locale: kwikpaste_os::locale::system_locale(),
        fixture_apps: crate::selftest::active(),
        ..CoreOptions::default()
    };

    let runtime = CoreRuntime::new()?;
    let (sender, events) = async_channel::unbounded();
    let sink = move |event: CoreEvent| {
        let _ = sender.try_send(event);
    };
    let core = futures::executor::block_on(Core::start(
        info,
        paths,
        options,
        Arc::new(sink),
        runtime.handle(),
    ))
    .context("kwikpaste-core did not start")?;
    core.set_platform_services(Arc::new(NativeServices));
    // 自测进程不读写本机剪贴板，也不监听；只有真机剪贴板探针（`--selftest-real-clipboard`）例外。
    let real_clipboard =
        !crate::selftest::active() || crate::selftest::enabled(crate::selftest::REAL_CLIPBOARD);
    if real_clipboard && crate::health::capture_disabled() {
        log::warn!(
            "degraded mode after crashes on the clipboard watcher: clipboard capture stays off until the next normal start"
        );
    } else if real_clipboard {
        if let Err(err) = core.start_watcher() {
            log::error!("the clipboard watcher did not start: {err}");
        }
    } else {
        core.set_clipboard_provider(Arc::new(MemoryClipboard::new()));
    }
    if crate::selftest::enabled(crate::selftest::OCR_DEMO)
        && let Err(err) = seed_ocr_demo(&core)
    {
        return Err(err.context("the OCR demo images could not be stored"));
    }
    let lan_network = if crate::selftest::active() {
        LanSyncNetwork::loopback()
    } else {
        LanSyncNetwork::default()
    };
    let lan_state = futures::executor::block_on(core.start_lan_sync(lan_network));
    if core.settings().sync.lan.enabled && !lan_state.running {
        log::warn!(
            "LAN sync is enabled but did not start: {}",
            lan_state.error.as_deref().unwrap_or("unknown error")
        );
    }
    log::info!(
        "core started: {} {:?}, data in {}",
        identity.identifier,
        identity.env,
        core.paths().bootstrap_dir().display()
    );

    Ok(StartedCore {
        host: CoreHost {
            core,
            _runtime: runtime,
        },
        events,
    })
}

/// `--selftest-ocr-demo`：按采集流程存入 `KP_OCR_DEMO_DIR` 下的 PNG（重复内容会去重），再按
/// `KP_OCR_DEMO_MODE` 开关识别。
fn seed_ocr_demo(core: &Core) -> anyhow::Result<()> {
    use kwikpaste_core::clipboard::{ClipboardPayload, ImagePayload};

    let dir = std::env::var_os("KP_OCR_DEMO_DIR").context("KP_OCR_DEMO_DIR is not set")?;
    let mut paths: Vec<_> = std::fs::read_dir(dir)?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "png"))
        .collect();
    paths.sort();
    for path in paths {
        let bytes = std::fs::read(&path)?;
        let (width, height) = image::image_dimensions(&path)?;
        let payload = ClipboardPayload::Image(ImagePayload {
            bytes,
            width,
            height,
        });
        if let Some(item) = core.build_item(&payload)? {
            futures::executor::block_on(core.store_item(item, None))?;
        }
    }
    // `KP_OCR_DEMO_MODE=off` 关掉识别；`fresh` 先清掉识别结果再开启，内存对比时让每次都真的识别一遍。
    let mode = std::env::var("KP_OCR_DEMO_MODE").unwrap_or_default();
    if mode == "fresh" {
        futures::executor::block_on(core.clear_ocr_data())?;
    }
    let enabled = mode != "off";
    #[cfg(debug_assertions)]
    if enabled {
        futures::executor::block_on(core.install_extension_from_dev(
            "ocr",
            "1.0.0",
            kwikpaste_core::extensions::OCR_PROTOCOL,
        ))?;
    }
    if core.installed_extensions().contains_key("ocr") {
        futures::executor::block_on(core.set_extension_enabled("ocr", enabled))?;
    }
    Ok(())
}

impl CoreHost {
    pub fn core(&self) -> &Core {
        &self.core
    }
}

/// 更新交接已经关停了 core（交接在退出前自己调用 `Core::shutdown`），退出时不再关一次。
static SHUT_DOWN: AtomicBool = AtomicBool::new(false);

pub fn mark_shut_down() {
    SHUT_DOWN.store(true, Ordering::SeqCst);
}

pub fn is_shut_down() -> bool {
    SHUT_DOWN.load(Ordering::SeqCst)
}

/// 当前的 core。平台层启动后一直存在；展示窗之类不经平台层启动的模式下为 `None`。
pub fn core(cx: &App) -> Option<&Core> {
    cx.try_global::<CoreHost>().map(CoreHost::core)
}
