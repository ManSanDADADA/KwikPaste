//! 看门狗与有序重启的主线程一侧（附录 C §8 第 4、6 条，策略见 `crate::health`）。
//!
//! - 有序重启：其它线程 panic 或看门狗判定后，请求经 channel 送到这里：释放单实例、拉起带
//!   `--relaunched-after-crash <n>` 的子进程、退出（退出码 70）。
//! - 看门狗：显示面板前、以及每 10 s 检查渲染：vsync 线程（补丁 0001 的 `vsync_thread_alive`）死了
//!   窗口就再也不会重绘；GPU 设备丢失后按计划重试都失败（补丁 0003 的 `device_loss_status().failing`，
//!   约 8 s）说明进程内恢复不了。两者都按崩溃处理、有序重启（10 分钟内第 3 次降级重启时关掉
//!   DirectComposition）。macOS 同样监听 Metal 设备移除通知并走同一条重启门槛。
//! - 设备丢失恢复：系统唤醒后主动请求一次重建（`request_device_recheck`），不等驱动报错。

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use gpui::{App, AsyncApp};

use super::{instance, probe};
use crate::health::{self, Restart};

const CHECK_INTERVAL: Duration = Duration::from_secs(10);

/// 自测模拟 vsync 线程已死（`--selftest-vsync-dead`）。
static SIMULATED_DEAD: AtomicBool = AtomicBool::new(false);

/// 在 `Application::run` 回调里调用：开始处理有序重启请求并定时检查渲染。
pub fn serve(cx: &mut App) {
    probe::health(
        health::relaunch_count(),
        health::degraded(),
        std::env::var_os("GPUI_DISABLE_DIRECT_COMPOSITION").is_some(),
    );

    let requests = health::restart_requests();
    cx.spawn(async move |cx: &mut AsyncApp| {
        if let Ok(restart) = requests.recv().await {
            cx.update(|cx| restart_now(cx, &restart));
        }
    })
    .detach();

    cx.spawn(async move |cx: &mut AsyncApp| {
        loop {
            cx.background_executor().timer(CHECK_INTERVAL).await;
            if !render_ok("periodic check") {
                break;
            }
        }
    })
    .detach();

    // 睡眠、唤醒两个都要注册：只注册一个时 GPUI 在 Windows 上收不到电源通知。
    cx.on_system_sleep(|_| log::info!("system is going to sleep"))
        .detach();
    cx.on_system_wake(|_| {
        log::info!("system woke up; rechecking the GPU device");
        recheck_device();
    })
    .detach();

    #[cfg(target_os = "macos")]
    if let Err(err) = kwikpaste_os::mac::metal::watch_device_removed(|| {
        log::error!("Metal reported that the rendering device was removed");
        recheck_device();
    }) {
        log::warn!("could not watch Metal device removal: {err}");
    }
}

/// 渲染是否正常；不正常时要求有序重启并返回 `false`（调用方不要再显示面板）。
pub fn render_ok(source: &str) -> bool {
    let problem = if !render_thread_alive() {
        "the vsync thread is not running"
    } else if device_recovery_failing() {
        "the GPU device was lost and could not be recovered"
    } else {
        return true;
    };
    health::request_restart("watchdog", format!("{source}: {problem}"));

    false
}

#[cfg(target_os = "windows")]
fn render_thread_alive() -> bool {
    gpui_windows::vsync_thread_alive() && !SIMULATED_DEAD.load(Ordering::SeqCst)
}

#[cfg(target_os = "macos")]
fn render_thread_alive() -> bool {
    !SIMULATED_DEAD.load(Ordering::SeqCst)
}

#[cfg(target_os = "windows")]
fn device_recovery_failing() -> bool {
    gpui_windows::device_loss_status().failing
}

#[cfg(target_os = "macos")]
fn device_recovery_failing() -> bool {
    kwikpaste_os::mac::metal::device_removed()
}

#[cfg(target_os = "windows")]
fn recheck_device() {
    gpui_windows::request_device_recheck();
}

/// macOS 睡眠唤醒不会丢 Metal 设备，真正的移除由 Metal 通知置位；这里不能主动置位，否则每次唤醒都重启。
#[cfg(target_os = "macos")]
fn recheck_device() {}

/// 自测：让看门狗认为渲染线程已死。
pub fn simulate_dead_render_thread() {
    log::warn!("selftest: the watchdog now treats the vsync thread as dead");
    SIMULATED_DEAD.store(true, Ordering::SeqCst);
}

/// 自测：用补丁 0006 的同一状态位模拟显示器关闭再恢复，不触碰用户的真实显示器。
pub fn simulate_display_sleep() {
    #[cfg(target_os = "windows")]
    {
        gpui_windows::simulate_display_sleeping(true);
        probe::display_power("off");
        gpui_windows::simulate_display_sleeping(false);
        probe::display_power("on");
    }
    #[cfg(not(target_os = "windows"))]
    {
        probe::display_power("unsupported");
    }
}

/// 自测（`--selftest-device-lost=<n>`）：模拟一次 GPU 设备丢失，前 `n` 次重建全局设备失败；
/// 恢复（或判定恢复不了）后写探针事件 `device_recovered` / `device_failing`。
pub fn simulate_device_lost(failed_attempts: u32, cx: &mut App) {
    #[cfg(target_os = "windows")]
    {
        let before = gpui_windows::device_loss_status();
        let frames = super::rendered_frames();
        log::warn!("selftest: simulating a GPU device loss ({failed_attempts} failed attempts)");
        gpui_windows::simulate_device_lost(failed_attempts);
        cx.spawn(async move |cx: &mut AsyncApp| {
            for _ in 0..300 {
                cx.background_executor()
                    .timer(Duration::from_millis(100))
                    .await;
                let status = gpui_windows::device_loss_status();
                if status.recoveries > before.recoveries {
                    // 恢复后的强制重绘在 200 ms 之后。
                    cx.background_executor()
                        .timer(Duration::from_millis(500))
                        .await;
                    let drawn = super::rendered_frames().saturating_sub(frames);
                    probe::device(
                        "device_recovered",
                        status.losses,
                        status.recoveries,
                        status.last_recovery_ms,
                        drawn,
                    );
                    return;
                }
                if status.failing {
                    probe::device("device_failing", status.losses, status.recoveries, 0, 0);
                    return;
                }
            }
        })
        .detach();
    }
    #[cfg(target_os = "macos")]
    {
        let _ = (failed_attempts, cx);
        log::warn!("selftest: simulating a Metal device removal");
        kwikpaste_os::mac::metal::simulate_device_removed();
    }
}

fn restart_now(cx: &mut App, restart: &Restart) {
    log::error!(
        "orderly restart #{}{}: {}",
        restart.relaunch,
        if restart.degraded { " (degraded)" } else { "" },
        restart.reason
    );
    probe::quitting();
    // 先放掉单实例，子进程启动时直接成为主实例。
    super::hotkey::unregister_all(cx);
    instance::release(cx);
    if let Err(err) = health::spawn_relaunch(restart) {
        log::error!("the restarted instance could not be started: {err}");
    }
    cx.quit();
}
