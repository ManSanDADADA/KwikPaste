//! 快贴原生版（GPUI）入口。
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod clipboard;
mod core_host;
mod gallery;
mod health;
mod i18n;
mod identity;
#[cfg(target_os = "windows")]
mod ocr_memory;
mod platform;
mod preferences;
mod selftest;

use gpui::{App, Application};

fn main() -> anyhow::Result<()> {
    platform::feedback::mark_started();
    #[cfg(target_os = "windows")]
    if ocr_memory::run_if_requested()? {
        return Ok(());
    }
    if health::run_watchdog_if_requested() {
        return Ok(());
    }
    #[cfg(target_os = "windows")]
    if kwikpaste_os::args().nth(1).as_deref() == Some("--icon-helper") {
        let result =
            kwikpaste_core::clipboard::icon::run_helper(std::io::stdin(), std::io::stdout().lock());
        if let Err(err) = result {
            eprintln!("icon helper stopped: {err}");
        }
        return Ok(());
    }
    // 第一步：日志与 panic hook（崩溃记录、崩溃重启），见 `health`。
    health::install();
    log::info!(
        "launched with {:?}",
        std::env::args_os().skip(1).collect::<Vec<_>>()
    );
    #[cfg(target_os = "windows")]
    match std::env::current_exe() {
        Ok(path) => kwikpaste_core::clipboard::set_helper_exe(path),
        Err(err) => log::warn!("icon helper executable path is unavailable: {err}"),
    }
    // 这里返回的错误只到 stderr（Windows 上没有控制台），先写进日志。
    let Some(launch) =
        platform::launch().inspect_err(|err| log::error!("startup failed: {err:#}"))?
    else {
        return Ok(());
    };
    let platform =
        platform::create().inspect_err(|err| log::error!("platform init failed: {err:#}"))?;
    health::start_watchdog();

    let application = Application::with_platform(platform).with_assets(kwikpaste_ui::Assets);
    #[cfg(target_os = "macos")]
    {
        application.on_reopen(platform::reopen_from_dock);
        let invocations = launch.invocation_sender();
        application.on_open_urls(move |urls| {
            for url in urls {
                if let Some(path) = platform::host::backup_path_from_url(&url) {
                    // 复用第二实例队列，启动完成前收到的 Finder 请求也不会丢失。
                    let _ = invocations.try_send(kwikpaste_os::single_instance::Invocation {
                        cwd: String::new(),
                        args: vec![String::new(), path.to_string_lossy().into_owned()],
                    });
                }
            }
        });
    }
    application.run(move |cx: &mut App| {
        // macOS 的退出走 `[NSApp terminate:]` 直接结束进程，`run` 不会返回，下面那次 `clean_exit`
        // 到不了；不在这里清掉运行标记，外部看门狗会把每次正常退出（含 Homebrew 升级前的退出）当成崩溃重启。
        cx.on_app_quit(|_| {
            health::clean_exit();
            async {}
        })
        .detach();
        kwikpaste_ui::init(cx);
        i18n::init(cx);
        clipboard::init(cx);

        // 组件展示窗是自测模式：不建面板、托盘和热键，关掉窗口就退出。
        if gallery::open_if_requested(cx) {
            cx.on_window_closed(|cx, _| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();
            return;
        }

        // 面板在 `platform::start` 里建好，列表视图由 `build_panel` 构造；普通启动的数据来自 core。
        let source = clipboard::prepare_source();
        let mut list = None;
        if let Err(err) = platform::start(cx, launch, |window, cx| {
            let view = clipboard::build_panel(&source, window, cx);
            list = Some(view.clone());
            view
        }) {
            log::error!("the panel could not be created: {err:#}");
            std::process::exit(1);
        }
        if selftest::active() {
            // 截图用的主题、语言、文本缩放覆盖，放在平台层按设置和系统应用之后。
            gallery::apply_env_overrides(cx);
        }
        if let Some(list) = list {
            clipboard::attach(&list, source, cx);
        }

        selftest::schedule(cx);
    });

    health::clean_exit();
    // 有序重启（70）或更新交接要求的退出码（例如安装包没能启动、已重启当前版本时）。
    let code = platform::exit_code();
    if code != 0 {
        std::process::exit(code);
    }
    Ok(())
}
