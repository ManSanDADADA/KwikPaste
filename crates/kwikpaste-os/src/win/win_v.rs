//! 常驻键盘触发器：观察双击修饰键，并接管 `Win+V`（设置 `shortcuts.winV`，与 1.x `src-tauri/src/shortcut/win_v.rs` 相同）。
//!
//! `Win+V` 是系统保留热键，`RegisterHotKey` 拦不住，也挡不住系统剪贴板历史面板弹出。所以开启期间
//! 常驻一颗 `WH_KEYBOARD_LL` 钩子：V 按下时 Win 键按着就吞掉这次按下（自动重复也吞）和配对的松开，
//! 并通知宿主开合面板；同时注入一次占位键（未分配的 VK `0xE8`），打断「单击 Win」，Win 松开时不弹
//! 开始菜单。系统设置不动：关掉接管后 `Win+V` 立刻回到系统剪贴板历史；双击也关闭时线程退出。
//!
//! 别的程序注入的按键（`LLKHF_INJECTED`）原样放行（1.x 同样如此），自己注入的占位键也因此放行。
//! 自测可以打开 [`accept_probe_input`]：带 [`PROBE_INPUT_MARKER`] 的注入按键按真实按键处理。

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock, mpsc};
use std::time::Instant;

use kwikpaste_core::settings::{DoubleTap, DoubleTapKey, KeySide};
use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBD_EVENT_FLAGS, KEYBDINPUT,
    KEYEVENTF_KEYUP, SendInput, VIRTUAL_KEY, VK_CONTROL, VK_LCONTROL, VK_LMENU, VK_LSHIFT, VK_LWIN,
    VK_MENU, VK_RCONTROL, VK_RMENU, VK_RSHIFT, VK_RWIN, VK_SHIFT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, GetMessageW, KBDLLHOOKSTRUCT, LLKHF_INJECTED, MSG, PM_NOREMOVE, PeekMessageW,
    PostThreadMessageW, SetWindowsHookExW, UnhookWindowsHookEx, WH_KEYBOARD_LL, WM_KEYDOWN,
    WM_KEYUP, WM_QUIT, WM_SYSKEYDOWN, WM_SYSKEYUP,
};

use crate::double_tap::{Detector, PhysicalKey};

const VK_V: u32 = 0x56;
/// 未分配的 VK（AutoHotkey 的 menu-mask key 同款）：注入它没有副作用，却让系统认为 Win 按住期间
/// 有过别的输入，于是不弹开始菜单。
const VK_DUMMY: u16 = 0xE8;
/// 自测探针注入按键时写进 `dwExtraInfo` 的标记（ASCII "KPPR"）。
pub const PROBE_INPUT_MARKER: usize = 0x4B50_5052;

type Sink = Box<dyn Fn() + Send + Sync>;

static SINK: OnceLock<Sink> = OnceLock::new();
static ENABLED: AtomicBool = AtomicBool::new(false);
static ACCEPT_PROBE: AtomicBool = AtomicBool::new(false);
static V_CONSUMED: AtomicBool = AtomicBool::new(false);
static THREAD: Mutex<Option<HookThread>> = Mutex::new(None);
static DOUBLE_TAP_SINK: OnceLock<Sink> = OnceLock::new();
static DOUBLE_TAP_SUSPENDED: AtomicBool = AtomicBool::new(false);
static DOUBLE_TAP: Mutex<Detector> = Mutex::new(Detector::new(DoubleTap {
    key: DoubleTapKey::Disabled,
    side: KeySide::Any,
}));

struct HookThread {
    id: u32,
    join: std::thread::JoinHandle<()>,
}

/// 双击出口只通知宿主，不吞掉触发键；和 Win+V 共用一颗常驻钩子。
pub fn set_double_tap_sink(sink: impl Fn() + Send + Sync + 'static) -> io::Result<()> {
    DOUBLE_TAP_SINK
        .set(Box::new(sink))
        .map_err(|_| io::Error::other("the double-tap sink is already set"))
}

/// 跟随双击设置；Windows 的 Command 值等同于关闭，不改变持久化值。
pub fn set_double_tap(mut config: DoubleTap) -> io::Result<()> {
    if config.key == DoubleTapKey::Command {
        config.key = DoubleTapKey::Disabled;
    }
    {
        let mut detector = DOUBLE_TAP.lock().unwrap_or_else(|p| p.into_inner());
        if config.key == DoubleTapKey::Disabled {
            *detector = Detector::new(config);
        } else {
            detector.configure(config);
        }
    }
    refresh_hook()
}

/// 录制快捷键期间停用双击，并丢弃未完成手势。
pub fn suspend_double_tap(suspended: bool) {
    DOUBLE_TAP_SUSPENDED.store(suspended, Ordering::SeqCst);
    DOUBLE_TAP.lock().unwrap_or_else(|p| p.into_inner()).reset();
}

/// 设置出口（在钩子线程上调用，不得阻塞），进程内只设一次。
pub fn set_sink(sink: impl Fn() + Send + Sync + 'static) -> io::Result<()> {
    SINK.set(Box::new(sink))
        .map_err(|_| io::Error::other("the Win+V sink is already set"))
}

/// 自测：把带 [`PROBE_INPUT_MARKER`] 的注入按键当成真实按键。
pub fn accept_probe_input(accept: bool) {
    ACCEPT_PROBE.store(accept, Ordering::SeqCst);
}

/// 按设置开关接管；幂等。
pub fn set_enabled(enabled: bool) -> io::Result<()> {
    ENABLED.store(enabled, Ordering::SeqCst);
    if !enabled {
        V_CONSUMED.store(false, Ordering::SeqCst);
    }
    if let Err(err) = refresh_hook() {
        ENABLED.store(false, Ordering::SeqCst);
        return Err(err);
    }
    Ok(())
}

/// Win+V 或双击任一开启时驻留，全部关闭时等待旧钩子退出再允许重装。
fn refresh_hook() -> io::Result<()> {
    let mut thread = THREAD.lock().unwrap_or_else(|p| p.into_inner());
    let enabled = ENABLED.load(Ordering::SeqCst)
        || DOUBLE_TAP
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .enabled();
    if enabled && thread.is_none() {
        *thread = Some(spawn_hook_thread()?);
    } else if !enabled && let Some(running) = thread.take() {
        let _ = unsafe { PostThreadMessageW(running.id, WM_QUIT, WPARAM(0), LPARAM(0)) };
        let _ = running.join.join();
        V_CONSUMED.store(false, Ordering::SeqCst);
    }
    Ok(())
}

/// 钩子是否在运行（自测核对开关生效用）。
pub fn is_active() -> bool {
    THREAD
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .as_ref()
        .is_some_and(|thread| !thread.join.is_finished())
}

fn spawn_hook_thread() -> io::Result<HookThread> {
    let (ready, started) = mpsc::sync_channel(1);
    let join = std::thread::Builder::new()
        .name("keyboard-trigger-hook".to_owned())
        .spawn(move || {
            let module = unsafe { GetModuleHandleW(None) }.ok().map(Into::into);
            let hook =
                match unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook_proc), module, 0) } {
                    Ok(hook) => hook,
                    Err(err) => {
                        let _ = ready.send(Err(io::Error::other(err)));
                        return;
                    }
                };
            let mut msg = MSG::default();
            let _ = unsafe { PeekMessageW(&mut msg, None, 0, 0, PM_NOREMOVE) };
            let _ = ready.send(Ok(unsafe { GetCurrentThreadId() }));
            while unsafe { GetMessageW(&mut msg, None, 0, 0) }.0 > 0 {}
            let _ = unsafe { UnhookWindowsHookEx(hook) };
        })?;

    let id = started
        .recv()
        .map_err(|_| io::Error::other("the keyboard trigger hook stopped while starting"))??;
    Ok(HookThread { id, join })
}

unsafe extern "system" fn hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 {
        let event = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
        observe_double_tap(wparam.0 as u32, event);
        if ENABLED.load(Ordering::SeqCst) && handle(wparam.0 as u32, event) {
            return LRESULT(1);
        }
    }

    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

/// 面板钩子已吞的事件也从这里观察；未吞的事件只在常驻钩子观察，避免重复送入。
pub(super) fn observe_double_tap(message: u32, event: &KBDLLHOOKSTRUCT) {
    if !observed_input(event, ACCEPT_PROBE.load(Ordering::SeqCst)) {
        return;
    }
    let down = matches!(message, WM_KEYDOWN | WM_SYSKEYDOWN);
    if !down && !matches!(message, WM_KEYUP | WM_SYSKEYUP) {
        return;
    }
    static EPOCH: OnceLock<Instant> = OnceLock::new();
    let timestamp = EPOCH.get_or_init(Instant::now).elapsed().as_millis() as u64;
    let fire = {
        let mut detector = DOUBLE_TAP.lock().unwrap_or_else(|p| p.into_inner());
        if down {
            detector.clear_released(|physical| key_down(virtual_key(physical)));
        }
        let fire = if let Some(key) = physical_key(event.vkCode) {
            detector.modifier(key, down, timestamp)
        } else {
            if down {
                detector.reset();
            }
            false
        };
        if DOUBLE_TAP_SUSPENDED.load(Ordering::SeqCst) || super::trigger_pause::is_paused() {
            detector.reset();
            false
        } else {
            fire
        }
    };
    if fire
        && !super::trigger_pause::recheck_for_input()
        && !DOUBLE_TAP_SUSPENDED.load(Ordering::SeqCst)
        && let Some(sink) = DOUBLE_TAP_SINK.get()
    {
        sink();
    }
}

/// AltGr 伪 LCtrl 和注入输入不参与手势；仅显式开启的探针标记例外。
fn observed_input(event: &KBDLLHOOKSTRUCT, accept_probe: bool) -> bool {
    let probe = accept_probe && event.dwExtraInfo == PROBE_INPUT_MARKER;
    (event.flags.0 & LLKHF_INJECTED.0 == 0 || probe)
        && !(event.vkCode == u32::from(VK_LCONTROL.0) && event.scanCode == 0x21D)
}

fn physical_key(vk: u32) -> Option<PhysicalKey> {
    let (key, side) = match VIRTUAL_KEY(vk as u16) {
        VK_LCONTROL => (DoubleTapKey::Control, KeySide::Left),
        VK_RCONTROL => (DoubleTapKey::Control, KeySide::Right),
        VK_LSHIFT => (DoubleTapKey::Shift, KeySide::Left),
        VK_RSHIFT => (DoubleTapKey::Shift, KeySide::Right),
        VK_LMENU => (DoubleTapKey::Alt, KeySide::Left),
        VK_RMENU => (DoubleTapKey::Alt, KeySide::Right),
        _ => return None,
    };
    Some(PhysicalKey { key, side })
}

fn virtual_key(physical: PhysicalKey) -> VIRTUAL_KEY {
    match (physical.key, physical.side) {
        (DoubleTapKey::Control, KeySide::Left) => VK_LCONTROL,
        (DoubleTapKey::Control, _) => VK_RCONTROL,
        (DoubleTapKey::Shift, KeySide::Left) => VK_LSHIFT,
        (DoubleTapKey::Shift, _) => VK_RSHIFT,
        (_, KeySide::Left) => VK_LMENU,
        (_, _) => VK_RMENU,
    }
}

/// 处理一个按键事件，返回是否吞掉。
fn handle(message: u32, event: &KBDLLHOOKSTRUCT) -> bool {
    let injected = event.flags.0 & LLKHF_INJECTED.0 != 0;
    let probe = ACCEPT_PROBE.load(Ordering::SeqCst) && event.dwExtraInfo == PROBE_INPUT_MARKER;
    if (injected && !probe) || event.vkCode != VK_V {
        return false;
    }

    if matches!(message, WM_KEYDOWN | WM_SYSKEYDOWN) {
        // 已吞掉的 V 持续到松开，期间释放 Win 或按下其它修饰键都不能泄漏重复按下。
        if V_CONSUMED.load(Ordering::SeqCst) {
            return true;
        }
        if !key_down(VK_LWIN) && !key_down(VK_RWIN) {
            return false;
        }
        // 只接管单纯的 Win+V：Win+Shift+V 等组合属于系统或其它应用。
        if key_down(VK_SHIFT) || key_down(VK_CONTROL) || key_down(VK_MENU) {
            return false;
        }
        // 让给前台应用时按下不吞，松开随之放行，系统照常处理 Win+V。
        if super::trigger_pause::recheck_for_input() {
            return false;
        }
        V_CONSUMED.store(true, Ordering::SeqCst);
        suppress_start_menu();
        if let Some(sink) = SINK.get() {
            sink();
        }
        return true;
    }

    // 配对吞掉 V 的松开：按下已被拦截，放行松开会让前台应用收到孤立的 KEYUP。
    matches!(message, WM_KEYUP | WM_SYSKEYUP) && V_CONSUMED.swap(false, Ordering::SeqCst)
}

fn key_down(vk: VIRTUAL_KEY) -> bool {
    unsafe { GetAsyncKeyState(i32::from(vk.0)) < 0 }
}

/// 注入一次占位键的按下、松开，阻止系统在 Win 松开时弹开始菜单。
fn suppress_start_menu() {
    let key = |flags: KEYBD_EVENT_FLAGS| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(VK_DUMMY),
                dwFlags: flags,
                ..Default::default()
            },
        },
    };
    let inputs = [key(KEYBD_EVENT_FLAGS(0)), key(KEYEVENTF_KEYUP)];
    let sent = unsafe { SendInput(&inputs, size_of::<INPUT>() as i32) };
    if sent as usize != inputs.len() {
        log::warn!("start menu suppress key sent {sent}/{}", inputs.len());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn consumed_v_repeats_stay_consumed_until_release() {
        let event = KBDLLHOOKSTRUCT {
            vkCode: VK_V,
            ..Default::default()
        };
        V_CONSUMED.store(true, Ordering::SeqCst);

        assert!(handle(WM_KEYDOWN, &event));
        assert!(handle(WM_SYSKEYDOWN, &event));
        assert!(handle(WM_KEYUP, &event));
        assert!(!handle(WM_KEYUP, &event));
    }

    #[test]
    fn sided_virtual_keys_map_to_physical_modifiers() {
        for (left, right, key) in [
            (VK_LCONTROL, VK_RCONTROL, DoubleTapKey::Control),
            (VK_LSHIFT, VK_RSHIFT, DoubleTapKey::Shift),
            (VK_LMENU, VK_RMENU, DoubleTapKey::Alt),
        ] {
            assert_eq!(
                physical_key(u32::from(left.0)),
                Some(PhysicalKey {
                    key,
                    side: KeySide::Left
                })
            );
            assert_eq!(
                physical_key(u32::from(right.0)),
                Some(PhysicalKey {
                    key,
                    side: KeySide::Right
                })
            );
        }
        assert!(physical_key(VK_V).is_none());
        assert!(physical_key(u32::from(VK_LWIN.0)).is_none());
    }

    #[test]
    fn injected_input_needs_opted_in_probe_and_altgr_fake_control_is_ignored() {
        let mut event = KBDLLHOOKSTRUCT {
            vkCode: u32::from(VK_LCONTROL.0),
            scanCode: 0x1D,
            ..Default::default()
        };
        assert!(observed_input(&event, false));
        event.scanCode = 0x21D;
        assert!(!observed_input(&event, true));
        event.scanCode = 0x1D;
        event.flags = LLKHF_INJECTED;
        for marker in [
            0,
            super::super::keyboard::OWN_INPUT_MARKER,
            PROBE_INPUT_MARKER,
        ] {
            event.dwExtraInfo = marker;
            assert!(!observed_input(&event, false));
            assert_eq!(observed_input(&event, true), marker == PROBE_INPUT_MARKER);
        }
    }
}
