//! 崩溃记录（`last-crash.json`）、运行标记、重启策略和拉起子进程。

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::Duration;

use async_channel::{Receiver, Sender};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::{Phase, logger};

pub const CRASH_FILE: &str = "last-crash.json";
pub const RUNNING_FILE: &str = "running.json";
pub const WATCHDOG_STOP_FILE: &str = "watchdog-stop";
/// 统计重启次数的时间窗。
pub const WINDOW: Duration = Duration::from_secs(10 * 60);
/// 时间窗内最多重启几次（最后一次是降级重启）。
const MAX_RESTARTS: u32 = 3;
/// 时间窗内第几次重启起降级。
const DEGRADED_FROM: u32 = 3;
/// 记录里最多保留几条。
const KEPT: usize = 10;
/// 其它线程 panic 后（`panic = "unwind"` 时进程还活着），主线程这么久还没完成有序重启，就由兜底
/// 线程拉起子进程并退出。
const FALLBACK_AFTER: Duration = Duration::from_secs(10);
/// 有序重启的退出码。
const RESTART_EXIT_CODE: i32 = 70;
/// GPUI 读的环境变量：降级模式关掉 DirectComposition。
const DISABLE_DIRECT_COMPOSITION: &str = "GPUI_DISABLE_DIRECT_COMPOSITION";
/// 无法持久化记录时，重启计数随这一时间窗一起传给子进程。
const FALLBACK_WINDOW_START: &str = "KWIKPASTE_CRASH_WINDOW_START";
/// 剪贴板监听线程的名字（core 的 `clipboard::watcher`）。
const WATCHER_THREAD: &str = "clipboard-watcher";

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CrashLog {
    #[serde(default)]
    crashes: Vec<CrashRecord>,
    /// 时间窗内崩溃次数超限、没有再重启。下次手动启动时读到并清掉。
    #[serde(default)]
    gave_up: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct CrashRecord {
    at: DateTime<Utc>,
    pid: u32,
    version: String,
    /// 崩溃时应用所处的阶段（[`Phase`]），用来认出「一启动就崩」之类的毒输入。
    #[serde(default)]
    phase: String,
    thread: String,
    location: String,
    message: String,
    /// `restart`、`restart-degraded`、`give-up`、自测不重启时的 `none`，或下次启动才发现的
    /// `unclean-exit`（原生 `__fastfail`、被杀、断电，没有经过崩溃处理）。
    action: String,
}

/// 运行标记：主实例启动后写，正常退出时删。下次启动还在，说明上次没有正常退出。
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Running {
    pid: u32,
    started: DateTime<Utc>,
}

/// 一次崩溃（panic、原生异常或看门狗判定）的描述。
pub struct CrashInfo {
    pub thread: String,
    pub location: String,
    pub message: String,
}

/// 一次重启：子进程带 `--relaunched-after-crash <relaunch>`，`degraded` 时另带 `--crash-degraded`。
#[derive(Debug, Clone)]
pub struct Restart {
    pub relaunch: u32,
    pub degraded: bool,
    pub reason: String,
}

enum Action {
    None,
    Restart(Restart),
    GiveUp,
}

static REQUESTS: LazyLock<(Sender<Restart>, Receiver<Restart>)> =
    LazyLock::new(async_channel::unbounded);
/// 只处理第一次崩溃：之后的 panic 只记日志。
static HANDLING: AtomicBool = AtomicBool::new(false);
static SPAWNED: AtomicBool = AtomicBool::new(false);
static EXIT_CODE: AtomicI32 = AtomicI32::new(0);
static WATCHDOG_CHILD: LazyLock<Mutex<Option<std::process::Child>>> =
    LazyLock::new(|| Mutex::new(None));

/// 主线程上要处理的有序重启请求。
pub fn restart_requests() -> Receiver<Restart> {
    REQUESTS.1.clone()
}

/// 有序重启拉起子进程之后要求的退出码（没有重启时为 0）。
pub fn exit_code() -> i32 {
    EXIT_CODE.load(Ordering::SeqCst)
}

/// panic hook 调用。主线程上的 panic、以及 `panic = "abort"` 构建里任何线程的 panic，hook 返回后
/// 进程就结束了，这里直接拉起子进程（它等单实例互斥量被遗弃后接管）；`panic = "unwind"` 时其它
/// 线程的 panic 交给主线程有序重启，并开兜底线程。
pub(super) fn on_panic(info: CrashInfo, on_main_thread: bool) {
    let Some(restart) = begin(info) else {
        return;
    };
    if on_main_thread || cfg!(panic = "abort") {
        spawn_now(&restart);
        return;
    }
    let _ = REQUESTS.0.try_send(restart.clone());
    start_fallback(restart);
}

/// 没人处理的原生异常（访问冲突等）：进程马上结束，直接拉起子进程。
#[cfg(target_os = "windows")]
pub(super) fn on_native_crash(info: CrashInfo) {
    if let Some(restart) = begin(info) {
        spawn_now(&restart);
    }
}

fn spawn_now(restart: &Restart) {
    if let Err(err) = spawn_relaunch(restart) {
        logger::write_crash(&format!("the restart could not be started: {err}"));
    }
}

/// 不是 panic、但应用已经不能正常工作（看门狗发现 vsync 线程死了）时要求有序重启。
pub fn request_restart(source: &str, reason: String) {
    log::error!("{source}: {reason}; restarting");
    let info = CrashInfo {
        thread: source.to_owned(),
        location: String::new(),
        message: reason,
    };
    let Some(restart) = begin(info) else {
        return;
    };
    let _ = REQUESTS.0.try_send(restart.clone());
    start_fallback(restart);
}

/// 记一次崩溃并按时间窗内的次数和阶段决定怎么办；需要重启时返回重启参数。
fn begin(info: CrashInfo) -> Option<Restart> {
    if HANDLING.swap(true, Ordering::SeqCst) {
        logger::write_crash("another crash is already being handled; not restarting again");
        return None;
    }
    let state = super::state();
    let file = state.and_then(|state| state.crash_file.as_deref());
    let mut log = file.map(load).unwrap_or_default();
    let now = Utc::now();
    let phase = super::phase();
    let recent: Vec<&CrashRecord> = recent(&log, now);
    let count = match file {
        Some(_) => u32::try_from(recent.len()).unwrap_or(u32::MAX) + 1,
        None => fallback_relaunch_count(super::relaunch_count(), fallback_window_start(now)),
    };
    // 毒输入：连续两次都在启动阶段就崩（例如某个启动时读到的数据），第二次重启直接降级，
    // 不再浪费一次原样重启。
    let startup_again = phase == Phase::Startup
        && recent
            .last()
            .is_some_and(|last| last.phase == Phase::Startup.name());
    let action = if !state.is_some_and(|state| state.restart_enabled) {
        Action::None
    } else if count <= MAX_RESTARTS {
        Action::Restart(Restart {
            relaunch: count,
            degraded: count >= DEGRADED_FROM || startup_again,
            reason: format!(
                "thread '{}' in phase {}: {}",
                info.thread,
                phase.name(),
                info.message
            ),
        })
    } else {
        Action::GiveUp
    };
    let action_name = match &action {
        Action::None => "none",
        Action::Restart(restart) if restart.degraded => "restart-degraded",
        Action::Restart(_) => "restart",
        Action::GiveUp => "give-up",
    };

    log.crashes.push(CrashRecord {
        at: now,
        pid: std::process::id(),
        version: env!("CARGO_PKG_VERSION").to_owned(),
        phase: phase.name().to_owned(),
        thread: info.thread,
        location: info.location,
        message: info.message,
        action: action_name.to_owned(),
    });
    trim(&mut log);
    log.gave_up = matches!(action, Action::GiveUp);
    if let Some(file) = file {
        save(file, &log);
    }
    logger::write_crash(&format!(
        "crash #{count} within {} minutes in phase {}: {action_name}",
        WINDOW.as_secs() / 60,
        phase.name()
    ));

    match action {
        Action::Restart(restart) => Some(restart),
        Action::None | Action::GiveUp => None,
    }
}

fn recent(log: &CrashLog, now: DateTime<Utc>) -> Vec<&CrashRecord> {
    let window = chrono::Duration::from_std(WINDOW).unwrap_or(chrono::Duration::MAX);
    log.crashes
        .iter()
        .filter(|crash| now.signed_duration_since(crash.at) < window)
        .collect()
}

/// 没有持久化记录时也让继承的计数过期，不把一次重启链变成进程终身额度。
fn fallback_window_start(now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    let started = std::env::var(FALLBACK_WINDOW_START).ok()?.parse().ok()?;
    active_fallback_window(started, now)
}

/// 仅保留尚未过期且不在未来的回退时间窗起点。
fn active_fallback_window(started: DateTime<Utc>, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    now.signed_duration_since(started)
        .to_std()
        .is_ok_and(|elapsed| elapsed < WINDOW)
        .then_some(started)
}

fn fallback_relaunch_count(previous: u32, started: Option<DateTime<Utc>>) -> u32 {
    started.map_or(1, |_| previous.saturating_add(1))
}

fn trim(log: &mut CrashLog) {
    let excess = log.crashes.len().saturating_sub(KEPT);
    log.crashes.drain(..excess);
}

fn load(path: &Path) -> CrashLog {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn save(path: &Path, log: &CrashLog) {
    write_json(path, log);
}

fn write_json(path: &Path, value: &impl Serialize) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    match serde_json::to_vec_pretty(value) {
        Ok(bytes) => {
            if let Err(err) = std::fs::write(path, bytes) {
                logger::write_crash(&format!("{} could not be written: {err}", path.display()));
            }
        }
        Err(err) => logger::write_crash(&format!("{} could not be encoded: {err}", path.display())),
    }
}

/// 拉起重启后的子进程（只拉一次）：同一个可执行文件、原来的参数加 `--relaunched-after-crash <n>`，
/// 降级时加 `--crash-degraded` 并带 `GPUI_DISABLE_DIRECT_COMPOSITION=1`。成功后退出码记为 70。
pub fn spawn_relaunch(restart: &Restart) -> std::io::Result<u32> {
    if SPAWNED.swap(true, Ordering::SeqCst) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "the restart was already started",
        ));
    }
    let spawned = std::env::current_exe().and_then(|exe| {
        let mut command = Command::new(exe);
        let now = Utc::now();
        command.env(
            FALLBACK_WINDOW_START,
            fallback_window_start(now).unwrap_or(now).to_rfc3339(),
        );
        command.args(super::relaunch_args(
            kwikpaste_os::args().skip(1),
            restart.relaunch,
            restart.degraded,
        ));
        if restart.degraded {
            command.env(DISABLE_DIRECT_COMPOSITION, "1");
        } else if super::degraded() {
            command.env_remove(DISABLE_DIRECT_COMPOSITION);
        }
        command.spawn()
    });

    match spawned {
        Ok(child) => {
            super::mark_watchdog_handoff();
            EXIT_CODE.store(RESTART_EXIT_CODE, Ordering::SeqCst);
            logger::write_crash(&format!(
                "restarted as pid {} (relaunch #{}{})",
                child.id(),
                restart.relaunch,
                if restart.degraded { ", degraded" } else { "" }
            ));
            Ok(child.id())
        }
        Err(err) => {
            SPAWNED.store(false, Ordering::SeqCst);
            Err(err)
        }
    }
}

/// Spawn the minimal external helper. Its command line contains only the state paths and the
/// original app arguments, so a relaunch is an exact replay of the candidate that crashed.
pub(super) fn start_watchdog(running_file: &Path, crash_file: &Path) {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let mut command = Command::new(exe);
    command
        .arg(super::WATCHDOG)
        .arg(std::process::id().to_string())
        .arg(running_file)
        .arg(crash_file)
        .args(kwikpaste_os::args().skip(1));
    match command.spawn() {
        Ok(child) => {
            if let Ok(mut slot) = WATCHDOG_CHILD.lock() {
                *slot = Some(child);
            }
            log::debug!("external crash watchdog started");
        }
        Err(err) => log::warn!("external crash watchdog could not start: {err}"),
    }
}

pub(super) fn stop_watchdog() {
    if let Ok(mut slot) = WATCHDOG_CHILD.lock()
        && let Some(mut child) = slot.take()
    {
        let _ = child.kill();
        let _ = child.wait();
    }
}

/// Watchdog entry point. This process never calls `health::install`, creates a GPUI platform, or
/// opens a window. It restarts only when the running marker still belongs to the dead parent and
/// no intentional-stop marker was written.
pub(super) fn run_watchdog(
    pid: u32,
    running_file: PathBuf,
    crash_file: PathBuf,
    original_args: Vec<String>,
) {
    wait_for_process(pid);
    std::thread::sleep(Duration::from_millis(150));
    let stop_file = running_file.with_file_name(WATCHDOG_STOP_FILE);
    if stop_file.exists() || !running_marker_matches(&running_file, pid) {
        return;
    }
    let mut log = load(&crash_file);
    let relaunch = record_watchdog_exit(&mut log, pid, Utc::now());
    save(&crash_file, &log);
    if relaunch > MAX_RESTARTS {
        log::warn!("external watchdog restart limit reached within ten minutes");
        return;
    }
    let degraded = relaunch >= DEGRADED_FROM;
    let was_degraded = original_args.iter().any(|arg| arg == super::DEGRADED);
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let mut command = Command::new(exe);
    command.args(super::relaunch_args(original_args, relaunch, degraded));
    if degraded {
        command.env(DISABLE_DIRECT_COMPOSITION, "1");
    } else if was_degraded {
        command.env_remove(DISABLE_DIRECT_COMPOSITION);
    }
    match command.spawn() {
        Ok(child) => {
            super::mark_watchdog_handoff();
            log::warn!(
                "external watchdog relaunched pid {} (#{relaunch})",
                child.id()
            );
        }
        Err(err) => log::error!("external watchdog could not relaunch the app: {err}"),
    }
}

/// 外部终止补记在失败当时；已有进程内记录时不重复计数，后续启动也不会再次补记。
fn record_watchdog_exit(log: &mut CrashLog, pid: u32, now: DateTime<Utc>) -> u32 {
    if !log.crashes.iter().any(|crash| crash.pid == pid) {
        log.crashes.push(CrashRecord {
            at: now,
            pid,
            version: String::new(),
            phase: "unknown".to_owned(),
            thread: "unknown".to_owned(),
            location: String::new(),
            message: "the process ended without shutting down".to_owned(),
            action: "unclean-exit".to_owned(),
        });
    }
    let count = u32::try_from(recent(log, now).len()).unwrap_or(u32::MAX);
    log.gave_up = count > MAX_RESTARTS;
    if log.gave_up
        && let Some(crash) = log.crashes.iter_mut().find(|crash| crash.pid == pid)
    {
        crash.action = "give-up".to_owned();
    }
    trim(log);
    count
}

fn running_marker_matches(path: &Path, pid: u32) -> bool {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .and_then(|value| value.get("pid").and_then(serde_json::Value::as_u64))
        .is_some_and(|value| value == u64::from(pid))
}

fn wait_for_process(pid: u32) {
    #[cfg(target_os = "windows")]
    {
        use windows_sys::Win32::{
            Foundation::CloseHandle,
            System::Threading::{INFINITE, OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject},
        };
        // SAFETY: The helper receives a PID from its parent and requests only synchronization.
        let handle = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
        if !handle.is_null() {
            unsafe {
                WaitForSingleObject(handle, INFINITE);
                CloseHandle(handle);
            }
        }
    }
    #[cfg(target_os = "macos")]
    {
        while unsafe { libc::kill(pid as i32, 0) } == 0 {
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

/// 主线程迟迟不接手时的兜底：拉起子进程（还没拉起的话）并直接退出。
fn start_fallback(restart: Restart) {
    let spawned = std::thread::Builder::new()
        .name("crash-restart".to_owned())
        .spawn(move || {
            std::thread::sleep(FALLBACK_AFTER);
            logger::write_crash(
                "the orderly restart did not finish in time; restarting from the fallback thread",
            );
            if !SPAWNED.load(Ordering::SeqCst) {
                spawn_now(&restart);
            }
            std::process::exit(RESTART_EXIT_CODE);
        });
    if let Err(err) = spawned {
        logger::write_crash(&format!("the fallback restart thread did not start: {err}"));
    }
}

/// 主实例确认之后：上次没有正常退出、又没留下崩溃记录（原生 `__fastfail`、被杀、断电）的补记一条；
/// 普通启动时，上一轮崩溃次数超限、没再重启的记一条日志并清掉标记。最后写本次的运行标记。
pub(super) fn after_claim(crash_file: &Path, running_file: &Path, relaunch: u32) -> bool {
    let mut log = load(crash_file);
    let mut changed = false;

    if let Some(previous) = std::fs::read(running_file)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Running>(&bytes).ok())
        && previous.pid != std::process::id()
        && !log.crashes.iter().any(|crash| crash.pid == previous.pid)
    {
        log::warn!(
            "the previous run (pid {}, started {}) ended without shutting down: a native crash, a kill or a power loss",
            previous.pid,
            previous.started
        );
        log.crashes.push(CrashRecord {
            at: Utc::now(),
            pid: previous.pid,
            version: String::new(),
            phase: "unknown".to_owned(),
            thread: "unknown".to_owned(),
            location: String::new(),
            message: "the process ended without shutting down".to_owned(),
            action: "unclean-exit".to_owned(),
        });
        trim(&mut log);
        changed = true;
    }

    let gave_up = relaunch == 0 && log.gave_up;
    if gave_up {
        if let Some(last) = log
            .crashes
            .iter()
            .rev()
            .find(|crash| crash.action == "give-up")
        {
            log::warn!(
                "the previous run crashed more than {MAX_RESTARTS} times within {} minutes and was not restarted; last crash at {} in phase {} on thread '{}' at {}: {}",
                WINDOW.as_secs() / 60,
                last.at,
                last.phase,
                last.thread,
                last.location,
                last.message
            );
        }
        log.gave_up = false;
        changed = true;
    }
    if changed {
        save(crash_file, &log);
    }

    write_json(
        running_file,
        &Running {
            pid: std::process::id(),
            started: Utc::now(),
        },
    );
    gave_up
}

/// 正常退出：删掉运行标记。
pub(super) fn clean_exit(running_file: &Path) {
    let _ = std::fs::remove_file(running_file);
}

/// 降级启动时：时间窗内的崩溃里有发生在剪贴板监听线程上的，就不启动监听（它很可能就是崩溃的
/// 来源，例如某种剪贴板内容让解析崩溃）。
pub(super) fn watcher_crashed_recently(crash_file: &Path) -> bool {
    let log = load(crash_file);
    recent(&log, Utc::now())
        .iter()
        .any(|crash| crash.thread == WATCHER_THREAD)
}

/// 崩溃记录的路径：`<bootstrap>/state/<name>`。
pub(super) fn state_file(bootstrap: &Path, name: &str) -> PathBuf {
    bootstrap.join("state").join(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(minutes_ago: i64, now: DateTime<Utc>, phase: &str) -> CrashRecord {
        CrashRecord {
            at: now - chrono::Duration::minutes(minutes_ago),
            pid: 1,
            version: String::new(),
            phase: phase.to_owned(),
            thread: "main".to_owned(),
            location: String::new(),
            message: String::new(),
            action: "restart".to_owned(),
        }
    }

    #[test]
    fn watchdog_counts_each_exit_once_and_expires_healthy_gaps() {
        let now = Utc::now();
        let mut log = CrashLog::default();
        for pid in 1..=5 {
            let at = now + chrono::Duration::minutes(i64::from(pid) * 11);
            assert_eq!(record_watchdog_exit(&mut log, pid, at), 1);
            assert_eq!(record_watchdog_exit(&mut log, pid, at), 1);
            assert!(!log.gave_up);
        }
        assert_eq!(log.crashes.len(), 5);

        let at = now + chrono::Duration::minutes(55);
        assert_eq!(record_watchdog_exit(&mut log, 6, at), 2);
        assert_eq!(record_watchdog_exit(&mut log, 7, at), 3);
        assert_eq!(record_watchdog_exit(&mut log, 8, at), 4);
        assert!(log.gave_up);
        assert_eq!(log.crashes.last().unwrap().action, "give-up");
        assert_eq!(
            record_watchdog_exit(&mut log, 9, at + chrono::Duration::minutes(10)),
            1
        );
        assert!(!log.gave_up);
    }

    #[test]
    fn fallback_relaunch_counter_expires_with_its_window() {
        let now = Utc::now();
        assert_eq!(
            fallback_relaunch_count(3, active_fallback_window(now, now)),
            4
        );
        let later = now + chrono::Duration::minutes(10);
        assert_eq!(
            fallback_relaunch_count(3, active_fallback_window(now, later)),
            1
        );
        assert_eq!(fallback_relaunch_count(3, None), 1);
    }

    #[test]
    fn only_crashes_inside_the_window_count() {
        let now = Utc::now();
        let log = CrashLog {
            crashes: vec![
                record(30, now, "idle"),
                record(9, now, "idle"),
                record(1, now, "panel"),
            ],
            gave_up: false,
        };

        assert_eq!(recent(&log, now).len(), 2);
    }

    #[test]
    fn crash_log_round_trips_and_tolerates_missing_fields() {
        let now = Utc::now();
        let log = CrashLog {
            crashes: vec![record(1, now, "startup")],
            gave_up: true,
        };
        let text = serde_json::to_string(&log).expect("encode");
        assert!(text.contains("\"gaveUp\":true"));
        assert!(text.contains("\"phase\":\"startup\""));
        let back: CrashLog = serde_json::from_str(&text).expect("decode");
        assert_eq!(back.crashes.len(), 1);

        let empty: CrashLog = serde_json::from_str("{}").expect("decode");
        assert!(empty.crashes.is_empty() && !empty.gave_up);
        // 旧记录没有 phase 字段。
        let old: CrashRecord = serde_json::from_str(
            r#"{"at":"2026-10-03T00:00:00Z","pid":1,"version":"","thread":"main","location":"","message":"","action":"restart"}"#,
        )
        .expect("decode");
        assert_eq!(old.phase, "");
    }

    #[test]
    fn an_unclean_previous_run_is_recorded_once() {
        let dir = std::env::temp_dir().join(format!("kwikpaste-crash-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let crash_file = state_file(&dir, CRASH_FILE);
        let running_file = state_file(&dir, RUNNING_FILE);
        write_json(
            &running_file,
            &Running {
                pid: u32::MAX,
                started: Utc::now(),
            },
        );

        after_claim(&crash_file, &running_file, 0);
        let log = load(&crash_file);
        assert_eq!(log.crashes.len(), 1);
        assert_eq!(log.crashes[0].action, "unclean-exit");
        let running: Running =
            serde_json::from_slice(&std::fs::read(&running_file).expect("marker")).expect("json");
        assert_eq!(running.pid, std::process::id());

        // 这次正常退出：下次启动不再补记。
        clean_exit(&running_file);
        after_claim(&crash_file, &running_file, 0);
        assert_eq!(load(&crash_file).crashes.len(), 1);
        let _ = std::fs::remove_dir_all(dir);
    }
}
