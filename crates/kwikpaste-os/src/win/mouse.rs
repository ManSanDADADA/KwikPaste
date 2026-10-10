//! 低级鼠标钩子（`WH_MOUSE_LL`），两项功能共用一个钩子线程（与 1.x `src-tauri/src/mouse` 相同）：
//!
//! - 窗外点击隐藏：面板可见期间，在本进程窗口以外按下鼠标就通知宿主隐藏面板。面板从不激活，
//!   系统不会给它失焦通知，只能在全局监听按下。点击本身不吞，照常落到目标窗口。光标下的顶层窗口
//!   属于本进程（面板、以后的预览和菜单、托盘菜单）时不算窗外；非激活的本进程窗口按下会通知宿主
//!   重新捕获导航键。
//! - 鼠标按键唤起（设置 `shortcuts.mouseTrigger`，开启期间钩子常驻）：选定按键（中键、后退、前进）
//!   的单击整个交给快贴，按下和松开都吞掉，松开时通知宿主开合面板；按键原有的单击功能随之停用。
//!   侧键按住时鼠标移动多远都算单击；中键按住拖出阈值（系统拖动阈值的两倍）就把吞掉的按下补发
//!   给应用（打上 [`OWN_INPUT_MARKER`]，钩子放行），自动滚动、平移画布照常。光标在本进程窗口上时
//!   中键照常交给窗口；侧键照样接管。松开按按下时记下的按键配对，按下后改了设置也不会漏掉。
//!   前台是全屏应用或设置列表里的应用时，按下连同松开整组放行给它（见 [`super::trigger_pause`]）。
//!
//! 钩子每 2 秒重装一次排到钩子链最前面：其它软件后装的钩子最多只能抢先这么久，钩子因回调超时被
//! 系统悄悄摘掉时也能借此找回。两项都关掉时钩子线程退出。
//!
//! 同一线程还挂着 `EVENT_SYSTEM_FOREGROUND`：Alt+Tab、Win 键、任务栏等没有鼠标按下的切换路径
//! 同样让面板不再盖着原来的应用，前台换到本进程和面板目标以外的窗口时与窗外点击一样通知宿主。

use std::cell::Cell;
use std::io;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Mutex, OnceLock, mpsc};

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{GetCurrentProcessId, GetCurrentThreadId};
use windows::Win32::UI::Accessibility::{HWINEVENTHOOK, SetWinEventHook, UnhookWinEvent};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_MOUSE, MOUSEEVENTF_MIDDLEDOWN, MOUSEINPUT, SendInput,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, EVENT_SYSTEM_FOREGROUND, GA_ROOT, GWL_EXSTYLE, GetAncestor,
    GetForegroundWindow, GetMessageW, GetSystemMetrics, GetWindowLongPtrW,
    GetWindowThreadProcessId, HHOOK, KillTimer, MSG, MSLLHOOKSTRUCT, PM_NOREMOVE, PeekMessageW,
    PostThreadMessageW, SM_CXDRAG, SM_CYDRAG, SetTimer, SetWindowsHookExW, UnhookWindowsHookEx,
    WH_MOUSE_LL, WINEVENT_OUTOFCONTEXT, WM_LBUTTONDOWN, WM_MBUTTONDOWN, WM_MBUTTONUP, WM_MOUSEMOVE,
    WM_QUIT, WM_RBUTTONDOWN, WM_TIMER, WM_XBUTTONDOWN, WM_XBUTTONUP, WS_EX_NOACTIVATE,
    WindowFromPoint,
};

use super::keyboard::{self, OWN_INPUT_MARKER};
use crate::geometry::Point;

/// 重装钩子的间隔。
const HOOK_REFRESH_MS: u32 = 2000;
const XBUTTON1: u16 = 1;
const XBUTTON2: u16 = 2;

/// 钩子交给宿主的事件。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseEvent {
    /// 在本进程窗口以外按下了鼠标（左、右、中键），坐标是物理像素。
    OutsideClick(Point),
    /// 在本进程的非激活窗口上按下鼠标；用于重新捕获面板导航键。
    InsideClick(Point),
    /// 前台窗口换成了本进程和面板目标以外的窗口（Alt+Tab 等）；宿主按窗外点击处理。
    ForegroundChanged,
    /// 唤起按键单击（松开）：开合面板。
    Trigger,
}

/// 唤起面板的鼠标按键（设置 `shortcuts.mouseTrigger`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TriggerButton {
    Middle = 1,
    /// XBUTTON1，多数鼠标上的「后退」侧键。
    Back = 2,
    /// XBUTTON2，多数鼠标上的「前进」侧键。
    Forward = 3,
}

type Sink = Box<dyn Fn(MouseEvent) + Send + Sync>;

static SINK: OnceLock<Sink> = OnceLock::new();
static OUTSIDE_CLICK: AtomicBool = AtomicBool::new(false);
/// 选中的唤起按键（[`TriggerButton`] 的编码），0 表示关闭。
static TRIGGER: AtomicU8 = AtomicU8::new(0);
static THREAD: Mutex<Option<u32>> = Mutex::new(None);

thread_local! {
    /// 唤起按键按下后的状态；钩子回调总在钩子线程执行，线程退出即重置。
    static PRESS: Cell<Press> = const { Cell::new(Press::Idle) };
}

#[derive(Debug, Clone, Copy)]
enum Press {
    Idle,
    /// 按下已吞掉，等它松开；记下按下位置。
    Held(TriggerButton, POINT),
    /// 中键已拖出阈值，按下已补发给应用，松开原样放行。
    MiddleDragging,
}

/// 钩子对唤起按键事件的处理结果。
#[derive(Debug, PartialEq, Eq)]
enum Verdict {
    Pass,
    Swallow,
    /// 吞掉，并开合面板。
    Toggle,
    /// 放行这次移动，并把吞掉的中键按下补发给应用。
    ReplayMiddleDown,
}

/// 设置事件出口，进程内只设一次。出口在钩子线程上调用，不得阻塞。
pub fn set_sink(sink: impl Fn(MouseEvent) + Send + Sync + 'static) -> io::Result<()> {
    SINK.set(Box::new(sink))
        .map_err(|_| io::Error::other("the mouse hook sink is already set"))
}

/// 面板显示时调用：开始监听窗外点击，钩子就绪后才返回。
pub fn start_outside_click() -> io::Result<()> {
    update_hook(|| OUTSIDE_CLICK.store(true, Ordering::SeqCst))
}

/// 面板隐藏时调用：停止监听窗外点击（唤起按键开着时钩子保留）。
pub fn stop_outside_click() {
    let _ = update_hook(|| OUTSIDE_CLICK.store(false, Ordering::SeqCst));
}

/// 按设置切换唤起按键，`None` 关闭；幂等。
pub fn set_trigger(button: Option<TriggerButton>) -> io::Result<()> {
    update_hook(|| TRIGGER.store(button.map_or(0, |button| button as u8), Ordering::SeqCst))
}

/// 在锁内改一项开关，再按是否还有功能开着起停钩子线程。
fn update_hook(change: impl FnOnce()) -> io::Result<()> {
    let mut thread = THREAD
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    change();
    let needed = OUTSIDE_CLICK.load(Ordering::SeqCst) || TRIGGER.load(Ordering::SeqCst) != 0;
    match (*thread, needed) {
        (None, true) => match spawn_hook_thread() {
            Ok(id) => *thread = Some(id),
            Err(err) => {
                OUTSIDE_CLICK.store(false, Ordering::SeqCst);
                TRIGGER.store(0, Ordering::SeqCst);
                return Err(err);
            }
        },
        (Some(id), false) => {
            let _ = unsafe { PostThreadMessageW(id, WM_QUIT, WPARAM(0), LPARAM(0)) };
            *thread = None;
        }
        _ => {}
    }

    Ok(())
}

fn spawn_hook_thread() -> io::Result<u32> {
    let (ready, started) = mpsc::sync_channel(1);
    std::thread::Builder::new()
        .name("mouse-hook".to_owned())
        .spawn(move || run_hook_thread(ready))?;

    started
        .recv()
        .map_err(|_| io::Error::other("the mouse hook thread stopped while starting"))?
}

fn install() -> windows::core::Result<HHOOK> {
    let module = unsafe { GetModuleHandleW(None) }.ok().map(Into::into);
    unsafe { SetWindowsHookExW(WH_MOUSE_LL, Some(hook_proc), module, 0) }
}

fn run_hook_thread(ready: mpsc::SyncSender<io::Result<u32>>) {
    let mut hook = match install() {
        Ok(hook) => hook,
        Err(err) => {
            let _ = ready.send(Err(io::Error::other(err)));
            return;
        }
    };

    let mut msg = MSG::default();
    let _ = unsafe { PeekMessageW(&mut msg, None, 0, 0, PM_NOREMOVE) };
    let foreground = unsafe {
        SetWinEventHook(
            EVENT_SYSTEM_FOREGROUND,
            EVENT_SYSTEM_FOREGROUND,
            None,
            Some(foreground_changed),
            0,
            0,
            WINEVENT_OUTOFCONTEXT,
        )
    };
    if foreground.is_invalid() {
        let _ = unsafe { UnhookWindowsHookEx(hook) };
        let _ = ready.send(Err(io::Error::other(
            "the foreground event hook could not be installed",
        )));
        return;
    }
    let _ = ready.send(Ok(unsafe { GetCurrentThreadId() }));
    let timer = unsafe { SetTimer(None, 0, HOOK_REFRESH_MS, None) };

    while unsafe { GetMessageW(&mut msg, None, 0, 0) }.0 > 0 {
        if msg.message == WM_TIMER {
            // 先装新的再卸旧的，中间不留空档。
            if let Ok(fresh) = install() {
                let _ = unsafe { UnhookWindowsHookEx(hook) };
                hook = fresh;
            }
        }
    }

    let _ = unsafe { KillTimer(None, timer) };
    let _ = unsafe { UnhookWinEvent(foreground) };
    let _ = unsafe { UnhookWindowsHookEx(hook) };
}

/// 进程外 WinEvent 回调在装钩子的线程的消息循环里执行。以回调时的前台为准，不看事件带的窗口：
/// 面板刚显示时，显示之前的前台切换事件可能才送到，那时前台已经就是面板的目标。
unsafe extern "system" fn foreground_changed(
    _: HWINEVENTHOOK,
    _: u32,
    _: HWND,
    _: i32,
    _: i32,
    _: u32,
    _: u32,
) {
    if !OUTSIDE_CLICK.load(Ordering::SeqCst) {
        return;
    }

    let window = unsafe { GetForegroundWindow() };
    if window.is_invalid() || window.0 as isize == keyboard::target() || own_root(window).is_some()
    {
        return;
    }
    emit(MouseEvent::ForegroundChanged);
}

unsafe extern "system" fn hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code < 0 {
        return unsafe { CallNextHookEx(None, code, wparam, lparam) };
    }
    let message = wparam.0 as u32;
    let event = unsafe { &*(lparam.0 as *const MSLLHOOKSTRUCT) };

    if let Some(trigger) = current_trigger() {
        match judge(
            trigger,
            message,
            event,
            super::trigger_pause::recheck_for_input,
        ) {
            Verdict::Pass => {}
            Verdict::Swallow => return LRESULT(1),
            Verdict::Toggle => {
                emit(MouseEvent::Trigger);
                return LRESULT(1);
            }
            Verdict::ReplayMiddleDown => replay_middle_down(),
        }
    }

    if OUTSIDE_CLICK.load(Ordering::SeqCst)
        && matches!(message, WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN)
    {
        let point = Point {
            x: event.pt.x,
            y: event.pt.y,
        };
        if let Some(window) = own_window_at(event.pt) {
            // 只有不激活的窗口（面板、预览）算面板内按下：按下不改前台，目标仍是原应用。偏好设置这类
            // 会被激活的窗口、已在前台的系统文件对话框不算，下一次按键按前台变化释放。
            if is_non_activating(window) && !is_own_foreground() {
                emit(MouseEvent::InsideClick(point));
            }
        } else if !crate::drag_out::is_active() {
            emit(MouseEvent::OutsideClick(point));
        }
    }

    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

fn emit(event: MouseEvent) {
    if let Some(sink) = SINK.get() {
        sink(event);
    }
}

fn current_trigger() -> Option<TriggerButton> {
    match TRIGGER.load(Ordering::SeqCst) {
        1 => Some(TriggerButton::Middle),
        2 => Some(TriggerButton::Back),
        3 => Some(TriggerButton::Forward),
        _ => None,
    }
}

/// 从鼠标消息里认出中键 / 侧键，返回按键和是否为按下；侧键看 `mouseData` 的高位字。
fn button_of(message: u32, mouse_data: u32) -> Option<(TriggerButton, bool)> {
    match message {
        WM_MBUTTONDOWN => Some((TriggerButton::Middle, true)),
        WM_MBUTTONUP => Some((TriggerButton::Middle, false)),
        WM_XBUTTONDOWN | WM_XBUTTONUP => {
            let button = match (mouse_data >> 16) as u16 {
                XBUTTON1 => TriggerButton::Back,
                XBUTTON2 => TriggerButton::Forward,
                _ => return None,
            };
            Some((button, message == WM_XBUTTONDOWN))
        }
        _ => None,
    }
}

/// 判定唤起按键怎么处理这个事件；只更新按下状态，开合面板与补发按下留给调用方（与 1.x 相同）。
/// `yield_press` 在唤起按键按下时调用，返回真表示这次让给前台应用（全屏或列表里的应用）：按下放行，
/// 松开随之放行。
fn judge(
    trigger: TriggerButton,
    message: u32,
    event: &MSLLHOOKSTRUCT,
    yield_press: impl FnOnce() -> bool,
) -> Verdict {
    if event.dwExtraInfo == OWN_INPUT_MARKER {
        return Verdict::Pass;
    }

    if message == WM_MOUSEMOVE {
        if let Press::Held(TriggerButton::Middle, origin) = PRESS.get()
            && beyond_drag_threshold(origin, event.pt)
        {
            PRESS.set(Press::MiddleDragging);
            return Verdict::ReplayMiddleDown;
        }
        return Verdict::Pass;
    }

    let Some((button, down)) = button_of(message, event.mouseData) else {
        return Verdict::Pass;
    };

    if down {
        if button != trigger {
            return Verdict::Pass;
        }
        if yield_press() || (button == TriggerButton::Middle && own_window_at(event.pt).is_some()) {
            PRESS.set(Press::Idle);
            return Verdict::Pass;
        }
        PRESS.set(Press::Held(button, event.pt));
        return Verdict::Swallow;
    }

    match PRESS.get() {
        Press::Held(held, _) if held == button => {
            PRESS.set(Press::Idle);
            Verdict::Toggle
        }
        Press::MiddleDragging if button == TriggerButton::Middle => {
            PRESS.set(Press::Idle);
            Verdict::Pass
        }
        _ => Verdict::Pass,
    }
}

/// 偏移超过拖动阈值的两倍才算拖动：`SM_CXDRAG` 不随显示缩放变化，高缩放屏上按下滚轮带出的抖动
/// 就可能超过它（与 1.x 相同）。
fn beyond_drag_threshold(origin: POINT, point: POINT) -> bool {
    let (width, height) = unsafe { (GetSystemMetrics(SM_CXDRAG), GetSystemMetrics(SM_CYDRAG)) };

    (point.x - origin.x).abs() > width * 2 || (point.y - origin.y).abs() > height * 2
}

/// 把吞掉的中键按下补发给光标下的应用，打上标记让钩子放行。
fn replay_middle_down() {
    let input = INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dwFlags: MOUSEEVENTF_MIDDLEDOWN,
                dwExtraInfo: OWN_INPUT_MARKER,
                ..Default::default()
            },
        },
    };
    if unsafe { SendInput(&[input], size_of::<INPUT>() as i32) } != 1 {
        log::warn!("the middle button press could not be replayed");
    }
}

/// 光标下属于本进程的顶层窗口。`WindowFromPoint` 只给调用线程自己的窗口发 `WM_NCHITTEST`，
/// 钩子线程没有窗口，不会被别的应用卡住。
fn own_window_at(point: POINT) -> Option<HWND> {
    own_root(unsafe { WindowFromPoint(point) })
}

fn is_own_foreground() -> bool {
    own_root(unsafe { GetForegroundWindow() }).is_some()
}

fn own_root(window: HWND) -> Option<HWND> {
    if window.is_invalid() {
        return None;
    }

    let root = unsafe { GetAncestor(window, GA_ROOT) };
    let mut process = 0;
    unsafe { GetWindowThreadProcessId(root, Some(&mut process)) };
    (process == unsafe { GetCurrentProcessId() }).then_some(root)
}

fn is_non_activating(window: HWND) -> bool {
    let ex_style = unsafe { GetWindowLongPtrW(window, GWL_EXSTYLE) };
    ex_style & WS_EX_NOACTIVATE.0 as isize != 0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(side: u16, x: i32, y: i32) -> MSLLHOOKSTRUCT {
        MSLLHOOKSTRUCT {
            pt: POINT { x, y },
            mouseData: u32::from(side) << 16,
            ..Default::default()
        }
    }

    #[test]
    fn side_buttons_are_told_apart_by_the_high_word() {
        assert_eq!(
            button_of(WM_XBUTTONDOWN, u32::from(XBUTTON1) << 16),
            Some((TriggerButton::Back, true))
        );
        assert_eq!(
            button_of(WM_XBUTTONUP, u32::from(XBUTTON2) << 16),
            Some((TriggerButton::Forward, false))
        );
        assert_eq!(
            button_of(WM_MBUTTONUP, 0),
            Some((TriggerButton::Middle, false))
        );
        assert_eq!(button_of(WM_XBUTTONDOWN, 0), None);
        assert_eq!(button_of(WM_LBUTTONDOWN, 0), None);
    }

    #[test]
    fn a_bound_side_button_is_taken_over_even_while_the_mouse_moves() {
        let back = event(XBUTTON1, 100, 100);
        let forward = event(XBUTTON2, 100, 100);
        let judge =
            |message, event: &MSLLHOOKSTRUCT| judge(TriggerButton::Back, message, event, || false);

        assert_eq!(judge(WM_XBUTTONDOWN, &back), Verdict::Swallow);
        assert_eq!(judge(WM_MOUSEMOVE, &event(0, 400, 100)), Verdict::Pass);
        assert_eq!(judge(WM_XBUTTONDOWN, &forward), Verdict::Pass);
        assert_eq!(judge(WM_XBUTTONUP, &forward), Verdict::Pass);
        assert_eq!(judge(WM_XBUTTONUP, &back), Verdict::Toggle);
        assert_eq!(judge(WM_XBUTTONUP, &back), Verdict::Pass);
    }

    #[test]
    fn a_middle_drag_goes_back_to_the_app_while_a_click_toggles() {
        let judge = |message, event: &MSLLHOOKSTRUCT| {
            judge(TriggerButton::Middle, message, event, || false)
        };
        let at = |x, y| event(0, x, y);
        let mut replayed = at(400, 100);
        replayed.dwExtraInfo = OWN_INPUT_MARKER;

        assert_eq!(judge(WM_MBUTTONDOWN, &at(100, 100)), Verdict::Swallow);
        assert_eq!(judge(WM_MOUSEMOVE, &at(101, 101)), Verdict::Pass);
        assert_eq!(
            judge(WM_MOUSEMOVE, &at(400, 100)),
            Verdict::ReplayMiddleDown
        );
        assert_eq!(judge(WM_MBUTTONDOWN, &replayed), Verdict::Pass);
        assert_eq!(judge(WM_MBUTTONUP, &at(500, 100)), Verdict::Pass);

        assert_eq!(judge(WM_MBUTTONDOWN, &at(100, 100)), Verdict::Swallow);
        assert_eq!(judge(WM_MBUTTONUP, &at(101, 101)), Verdict::Toggle);
    }

    #[test]
    fn a_yielded_press_passes_with_its_release() {
        let back = event(XBUTTON1, 100, 100);
        let yielded =
            |message, event: &MSLLHOOKSTRUCT| judge(TriggerButton::Back, message, event, || true);
        let normal =
            |message, event: &MSLLHOOKSTRUCT| judge(TriggerButton::Back, message, event, || false);

        assert_eq!(yielded(WM_XBUTTONDOWN, &back), Verdict::Pass);
        assert_eq!(normal(WM_XBUTTONUP, &back), Verdict::Pass);

        // 按下时没有让出，松开就照常开合，不会漏出孤立的松开。
        assert_eq!(normal(WM_XBUTTONDOWN, &back), Verdict::Swallow);
        assert_eq!(yielded(WM_XBUTTONUP, &back), Verdict::Toggle);
    }
}
