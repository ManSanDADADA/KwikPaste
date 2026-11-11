//! 7×24 稳健性（附录 C §8）：日志文件、panic hook、原生崩溃处理、崩溃重启与重启上限。
//!
//! - [`install`] 是 `main` 的第一步：开日志（普通启动写 `<日志目录>/KwikPaste.log`，与 1.x 同一个
//!   文件、同样的大小上限和格式；自测写 stderr），装 panic hook 和原生崩溃处理。
//! - panic hook 把线程名、阶段、位置、payload 和调用栈写进日志并 flush（release 改成
//!   `panic = "abort"` 后 hook 返回进程即结束），把这次崩溃记进 `<bootstrap>/state/last-crash.json`，
//!   然后交给原来的 hook：崩溃照常发生，不会被吞掉。
//! - 原生崩溃（访问冲突等没人处理的 SEH 异常）：`SetUnhandledExceptionFilter` 里同样记录并重启。
//!   `__fastfail`（不经 panic 的 abort、栈溢出）进程内拦不到：主实例启动后写运行标记
//!   `state/running.json`、正常退出时删掉，下次启动发现标记还在又没有对应的崩溃记录，就补记一条
//!   `unclean-exit`（计入时间窗，但当时没有重启）。
//! - 重启：10 分钟内第 1、2 次原样重启，第 3 次降级重启（`GPUI_DISABLE_DIRECT_COMPOSITION=1`、
//!   强制减少动画；时间窗内有崩在剪贴板监听线程上的，降级时不启动监听），再崩就不重启、在记录里标
//!   `gaveUp`。连续两次都崩在启动阶段（毒输入）时第二次就降级。子进程带
//!   `--relaunched-after-crash <n>`（降级另带 `--crash-degraded`）和原来的参数，不弹面板。
//!   - 主线程 panic、`panic = "abort"` 构建里任何线程的 panic、原生崩溃：进程马上结束，直接拉起
//!     子进程；子进程等互斥量（≤5 s，父进程死时得到 `WAIT_ABANDONED`）接管单实例。
//!   - `panic = "unwind"` 构建里其它线程的 panic：进程和消息循环还活着，请求经 channel 送给主线程
//!     做有序重启（`platform::watchdog`：释放单实例、拉起子进程、退出码 70）；10 s 内主线程没接手
//!     就由兜底线程拉起子进程并退出。
//! - 看门狗（`platform::watchdog`）在显示面板前和每 30 s 检查 vsync 线程（补丁 0001 的
//!   `vsync_thread_alive`），死了就走同一条有序重启。
//! - 阶段（[`Phase`]）由平台层在关键处设置，记进崩溃记录和日志。
//!
//! 自测进程默认只记录不重启，`--selftest-crash-restart` 才打开（`tools/platform-probes/crash-restart.ps1`）。

mod crash;
mod logger;
mod panic;

use std::path::PathBuf;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Mutex, OnceLock};

pub use crash::{Restart, exit_code, request_restart, restart_requests, spawn_relaunch};

use crate::selftest;

/// 崩溃重启的子进程带的参数：`--relaunched-after-crash <n>`，`n` 是 10 分钟内的第几次。
pub const RELAUNCHED: &str = kwikpaste_os::single_instance::RELAUNCHED_AFTER_CRASH_ARG;
/// 降级重启另带的参数。
pub const DEGRADED: &str = kwikpaste_os::single_instance::CRASH_DEGRADED_ARG;
/// Internal helper process mode. It is handled before GPUI/logging is initialized.
pub const WATCHDOG: &str = "--watchdog";

/// 应用当前所处的阶段，崩溃时记下来。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Phase {
    /// 进程启动到平台层就绪（core、面板、热键、托盘都建好）之前。
    Startup = 0,
    /// 面板隐藏、后台待命（剪贴板监听在跑）。
    Idle = 1,
    /// 面板可见。
    Panel = 2,
    /// 粘贴链路（写回剪贴板、交还前台、注入按键）。
    Paste = 3,
    /// 拖出（OLE 模态循环）。
    DragOut = 4,
    /// 正在退出。
    Shutdown = 5,
}

impl Phase {
    pub fn name(self) -> &'static str {
        match self {
            Self::Startup => "startup",
            Self::Idle => "idle",
            Self::Panel => "panel",
            Self::Paste => "paste",
            Self::DragOut => "drag-out",
            Self::Shutdown => "shutdown",
        }
    }

    fn from_u8(value: u8) -> Self {
        match value {
            1 => Self::Idle,
            2 => Self::Panel,
            3 => Self::Paste,
            4 => Self::DragOut,
            5 => Self::Shutdown,
            _ => Self::Startup,
        }
    }
}

static PHASE: AtomicU8 = AtomicU8::new(Phase::Startup as u8);

/// 进入某个阶段。
pub fn set_phase(phase: Phase) {
    PHASE.store(phase as u8, Ordering::SeqCst);
}

pub fn phase() -> Phase {
    Phase::from_u8(PHASE.load(Ordering::SeqCst))
}

/// 本进程的启动状态，[`install`] 时确定。
struct State {
    /// 崩溃记录 `<bootstrap>/state/last-crash.json`；路径解析失败时为 `None`（只写日志）。
    crash_file: Option<PathBuf>,
    /// 原生提示中要打开的日志目录。
    log_dir: Option<PathBuf>,
    /// 运行标记 `<bootstrap>/state/running.json`。
    running_file: Option<PathBuf>,
    /// Graceful shutdown marker consumed by the external watchdog.
    watchdog_stop_file: Option<PathBuf>,
    /// 这是第几次崩溃重启（普通启动为 0）。
    relaunch: u32,
    /// 降级启动。
    degraded: bool,
    /// 崩溃时是否重启：普通启动是，自测只在 `--selftest-crash-restart` 时是。
    restart_enabled: bool,
}

static STATE: OnceLock<State> = OnceLock::new();
static GAVE_UP_NOTICE: OnceLock<Mutex<Option<PathBuf>>> = OnceLock::new();

/// `main` 的第一步：开日志、装 panic hook 和原生崩溃处理。
pub fn install() {
    let identity = crate::identity::current();
    let paths = kwikpaste_core::CorePaths::for_native(identity.identifier, identity.env)
        .inspect_err(|err| eprintln!("app paths could not be resolved: {err:#}"))
        .ok();

    if selftest::active() {
        selftest::init_logging();
    } else if let Some(paths) = &paths {
        logger::init(paths.log_dir());
    }

    let args: Vec<String> = std::env::args().collect();
    let relaunch = parse_relaunch(args.iter().cloned());
    let degraded = args.iter().any(|arg| arg == DEGRADED);
    let restart_enabled = !selftest::active() || selftest::enabled(selftest::CRASH_RESTART);
    let bootstrap = paths.as_ref().map(|paths| paths.bootstrap_dir());
    let _ = STATE.set(State {
        crash_file: bootstrap
            .as_deref()
            .map(|dir| crash::state_file(dir, crash::CRASH_FILE)),
        log_dir: paths.as_ref().map(|paths| paths.log_dir()),
        running_file: bootstrap
            .as_deref()
            .map(|dir| crash::state_file(dir, crash::RUNNING_FILE)),
        watchdog_stop_file: bootstrap
            .as_deref()
            .map(|dir| crash::state_file(dir, crash::WATCHDOG_STOP_FILE)),
        relaunch,
        degraded,
        restart_enabled,
    });
    panic::install();
    #[cfg(target_os = "windows")]
    kwikpaste_os::win::crash::install(on_native_crash);

    if relaunch > 0 {
        log::warn!(
            "relaunched after crash #{relaunch} within {} minutes{}",
            crash::WINDOW.as_secs() / 60,
            if degraded { " (degraded mode)" } else { "" }
        );
    }
}

/// Handle the helper process before any GPUI or renderer code is loaded.
pub fn run_watchdog_if_requested() -> bool {
    let mut args = std::env::args().skip(1);
    if args.next().as_deref() != Some(WATCHDOG) {
        return false;
    }
    let Some(pid) = args.next().and_then(|value| value.parse::<u32>().ok()) else {
        return true;
    };
    let Some(running_file) = args.next().map(PathBuf::from) else {
        return true;
    };
    let Some(crash_file) = args.next().map(PathBuf::from) else {
        return true;
    };
    crash::run_watchdog(pid, running_file, crash_file, args.collect());
    true
}

/// Start one tiny sibling process that waits for this process without initializing GPUI.
pub fn start_watchdog() {
    if selftest::active() && !selftest::enabled(selftest::WATCHDOG) {
        return;
    }
    let Some(state) = STATE.get() else {
        return;
    };
    let (Some(running), Some(crash)) = (&state.running_file, &state.crash_file) else {
        return;
    };
    crash::start_watchdog(running, crash);
}

/// Mark an intentional stop (quit/update/installer handoff) so a forced process termination is
/// never mistaken for a crash by the helper process.
pub fn suppress_watchdog_restart() {
    if let Some(path) = STATE
        .get()
        .and_then(|state| state.watchdog_stop_file.as_deref())
    {
        let _ = std::fs::write(path, std::process::id().to_string());
    }
}

pub(super) fn mark_watchdog_handoff() {
    if let Some(path) = STATE
        .get()
        .and_then(|state| state.watchdog_stop_file.as_deref())
    {
        let _ = std::fs::write(path, std::process::id().to_string());
    }
}

/// 没人处理的原生异常（在崩溃的线程上调用）。
#[cfg(target_os = "windows")]
fn on_native_crash(code: u32, address: usize) {
    let thread = std::thread::current();
    let thread = thread.name().unwrap_or("<unnamed>").to_owned();
    logger::write_crash(&format!(
        "native exception 0x{code:08X} at 0x{address:X} on thread '{thread}' in phase {}",
        phase().name()
    ));
    crash::on_native_crash(crash::CrashInfo {
        thread,
        location: format!("0x{address:X}"),
        message: format!("native exception 0x{code:08X}"),
    });
}

/// 主实例确认之后调用：补记上次没有正常退出的运行、处理超限标记、写本次的运行标记。
pub fn after_claim() {
    if let Some(state) = STATE.get()
        && let (Some(crash_file), Some(running_file)) = (&state.crash_file, &state.running_file)
        && crash::after_claim(crash_file, running_file, state.relaunch)
        && let Some(log_dir) = &state.log_dir
    {
        let notice = GAVE_UP_NOTICE.get_or_init(|| Mutex::new(None));
        if let Ok(mut notice) = notice.lock() {
            *notice = Some(log_dir.clone());
        }
    }
    if let Some(path) = STATE
        .get()
        .and_then(|state| state.watchdog_stop_file.as_deref())
    {
        let _ = std::fs::remove_file(path);
    }
}

/// 取出本次启动需要提示用户的崩溃放弃重启通知。
pub fn take_gave_up_notice() -> Option<PathBuf> {
    GAVE_UP_NOTICE
        .get()
        .and_then(|notice| notice.lock().ok()?.take())
}

/// 正常退出（应用退出回调里，Windows 上 `Application::run` 返回后再调一次）：删掉运行标记。
pub fn clean_exit() {
    set_phase(Phase::Shutdown);
    if let Some(running_file) = STATE.get().and_then(|state| state.running_file.as_deref()) {
        crash::clean_exit(running_file);
    }
    crash::stop_watchdog();
    log::logger().flush();
}

/// 这是第几次崩溃重启（普通启动为 0）。
pub fn relaunch_count() -> u32 {
    STATE.get().map_or(0, |state| state.relaunch)
}

/// 降级模式：DirectComposition 关闭（子进程的环境变量），动画关闭。
pub fn degraded() -> bool {
    STATE.get().is_some_and(|state| state.degraded)
}

/// 降级模式下不启动剪贴板监听：时间窗内有崩溃发生在监听线程上。
pub fn capture_disabled() -> bool {
    degraded()
        && STATE
            .get()
            .and_then(|state| state.crash_file.as_deref())
            .is_some_and(crash::watcher_crashed_recently)
}

fn state() -> Option<&'static State> {
    STATE.get()
}

/// 从命令行取 `--relaunched-after-crash <n>`。
fn parse_relaunch(args: impl IntoIterator<Item = String>) -> u32 {
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        if arg == RELAUNCHED {
            return args.next().and_then(|n| n.parse().ok()).unwrap_or(1);
        }
    }

    0
}

/// 重启子进程的参数：原来的参数去掉旧的重启参数，再带上新的。
fn relaunch_args(
    args: impl IntoIterator<Item = String>,
    relaunch: u32,
    degraded: bool,
) -> Vec<String> {
    let mut kept = Vec::new();
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        if arg == RELAUNCHED {
            args.next();
            continue;
        }
        if arg == DEGRADED {
            continue;
        }
        kept.push(arg);
    }
    kept.push(RELAUNCHED.to_owned());
    kept.push(relaunch.to_string());
    if degraded {
        kept.push(DEGRADED.to_owned());
    }

    kept
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|arg| (*arg).to_owned()).collect()
    }

    #[test]
    fn relaunch_count_comes_from_the_flag() {
        assert_eq!(parse_relaunch(args(&["--auto-launch"])), 0);
        assert_eq!(parse_relaunch(args(&[RELAUNCHED, "2"])), 2);
        assert_eq!(parse_relaunch(args(&["--auto-launch", RELAUNCHED])), 1);
    }

    #[test]
    fn relaunch_keeps_the_arguments_and_replaces_the_flags() {
        assert_eq!(
            relaunch_args(args(&["--auto-launch", RELAUNCHED, "1"]), 2, false),
            args(&["--auto-launch", RELAUNCHED, "2"])
        );
        assert_eq!(
            relaunch_args(args(&[RELAUNCHED, "2", DEGRADED]), 3, true),
            args(&[RELAUNCHED, "3", DEGRADED])
        );
        assert_eq!(
            relaunch_args(args(&[RELAUNCHED, "3", DEGRADED]), 1, false),
            args(&[RELAUNCHED, "1"])
        );
    }

    #[test]
    fn phases_round_trip() {
        for phase in [
            Phase::Startup,
            Phase::Idle,
            Phase::Panel,
            Phase::Paste,
            Phase::DragOut,
            Phase::Shutdown,
        ] {
            assert_eq!(Phase::from_u8(phase as u8), phase);
        }
    }
}
