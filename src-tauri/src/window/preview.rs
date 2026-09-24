//! 系统级剪贴板预览窗口。
//!
//! 预览窗口就是预览面板本身：Rust 按内容度量算出面板矩形、把窗口定位定尺寸到该矩形，
//! 并套上原生材质（Mica / Acrylic），前端只负责在整个窗口里渲染内容。
//! 面板尺寸必须在显示前定好，所以内容度量由命令层从记录读出后传进来，不能等前端量完再回报。
//! 窗口接入生命周期管理（`DestroyWhenIdle`）：剪贴板窗口显示后预热建窗，首次悬停不必等
//! WebView 冷启动；隐藏空闲后销毁 WebView 释放内存，下次请求到达时再按需重建。

#![allow(clippy::unused_unit)]

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex, MutexGuard};
use std::thread;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::{
    AppHandle, Emitter, Manager, PhysicalPosition, PhysicalRect, PhysicalSize, WebviewUrl,
    WebviewWindow, WebviewWindowBuilder,
};

use crate::core::Result;
use crate::settings::SettingsStore;

use super::{get_window, lifecycle, CLIPBOARD_PREVIEW_WINDOW_LABEL, CLIPBOARD_WINDOW_LABEL};

#[cfg(target_os = "macos")]
use tauri_nspanel::{tauri_panel, ManagerExt, PanelLevel, StyleMask, WebviewWindowExt};

#[cfg(target_os = "windows")]
use windows::Win32::Foundation::HWND;
#[cfg(target_os = "windows")]
use windows::Win32::UI::WindowsAndMessaging::{
    SetWindowPos, HWND_TOPMOST, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_SHOWWINDOW,
};

const PREVIEW_UPDATED_EVENT: &str = "preview://updated";
const PREVIEW_PANEL_GAP: f64 = 40.0;
/// 剪贴板窗口显示后延迟这么久再预热，避开呼出时的主线程高峰。
const PREVIEW_PREWARM_DELAY_MS: u64 = 300;
/// 冷启动时等待预览页 ready 的上限。前端始终不回报（崩溃 / 极端卡顿）时照常显示，
/// 宁可先给一块空面板，也不能让预览彻底不出现。
const PREVIEW_READY_TIMEOUT_MS: u64 = 1_200;
const PREVIEW_PANEL_MARGIN: f64 = 32.0;

/// 面板尺寸区间与各区块高度，单位是逻辑像素，必须与预览页的渲染保持一致：
/// header 固定 `h-12`，文本行 `leading-5.5`，文件行 `min-h-10`，图片区上下左右各 16。
const PREVIEW_PANEL_MIN_WIDTH: f64 = 288.0;
const PREVIEW_PANEL_MAX_WIDTH: f64 = 480.0;
const PREVIEW_PANEL_MIN_HEIGHT: f64 = 96.0;
const PREVIEW_PANEL_MAX_HEIGHT: f64 = 480.0;
const PREVIEW_PANEL_HEADER_HEIGHT: f64 = 48.0;
const PREVIEW_PANEL_IMAGE_PADDING_X: f64 = 32.0;
const PREVIEW_PANEL_IMAGE_PADDING_Y: f64 = 32.0;
const PREVIEW_EMPTY_CONTENT_HEIGHT: f64 = 96.0;
const PREVIEW_TEXT_ROW_HEIGHT: f64 = 22.0;
const PREVIEW_TEXT_VERTICAL_PADDING: f64 = 32.0;
const PREVIEW_FILE_ROW_HEIGHT: f64 = 40.0;
const PREVIEW_FILE_VERTICAL_PADDING: f64 = 16.0;
const PREVIEW_FILE_MORE_FOOTER_HEIGHT: f64 = 40.0;
// 拆词视图的词块尺寸，与前端 `WordChipsViewer` 的样式一一对应：
// text-sm / leading-5 / px-1.5 / py-0.5 / min-w-6，词块间距 gap-1，四周留白 p-4。
const PREVIEW_WORD_FONT_SIZE: f64 = 14.0;
const PREVIEW_WORD_NARROW_CHAR_EM: f64 = 0.5;
const PREVIEW_WORD_CHIP_PADDING_X: f64 = 12.0;
const PREVIEW_WORD_CHIP_MIN_WIDTH: f64 = 24.0;
const PREVIEW_WORD_CHIP_HEIGHT: f64 = 24.0;
const PREVIEW_WORD_LINE_HEIGHT: f64 = 20.0;
const PREVIEW_WORD_GAP: f64 = 4.0;
const PREVIEW_WORDS_PADDING: f64 = 16.0;
/// 图片记录缺少原始宽高时的兜底面板尺寸。
const PREVIEW_PANEL_FALLBACK_SIZE: (f64, f64) = (320.0, 240.0);

static PREVIEW_REQUEST_ID: AtomicU64 = AtomicU64::new(0);
static PREVIEW_SESSION_ID: AtomicU64 = AtomicU64::new(0);
static PREVIEW_SUPPRESSED: AtomicBool = AtomicBool::new(false);
static PREVIEW_STATE: LazyLock<Mutex<Option<ClipboardPreviewState>>> =
    LazyLock::new(|| Mutex::new(None));
/// 串行化建窗：多个预览请求（如连续 hover）可能并发走到「检查不存在 → 建窗」，
/// 都过了存在性检查会触发重复 label 建窗报错。建窗都来自命令/后台线程、主线程从不持锁，
/// 不会与 builder 内部的主线程派发互锁。
static PREVIEW_BUILD_LOCK: Mutex<()> = Mutex::new(());
/// 预览窗口最近一次完成「定位 + 置顶 + 显示」时所用的 overlay 边界。
///
/// hover 跟随指针时前端每帧都会发一次 show，但同一块屏幕上 overlay 的位置和尺寸不变，
/// 逐帧重复 set_position / set_size / SetWindowPos / show（macOS 还要跨线程往返主线程）
/// 是纯浪费。边界一致且窗口确实可见时只广播新状态。隐藏与重建都会清空这里。
static PREVIEW_SHOWN_BOUNDS: Mutex<Option<PreviewBoundsKey>> = Mutex::new(None);

/// overlay 边界的可比较形式：`(x, y, width, height)`。
type PreviewBoundsKey = (i32, i32, u32, u32);

/// 等前端 ready 再显示的那次请求。
#[derive(Clone, Copy)]
struct PreviewPendingShow {
    request_id: u64,
    bounds: (PhysicalPosition<i32>, PhysicalSize<u32>),
}

/// 当前 WebView 实例的前端是否已完成初始化。建窗 / 重建时归零，收到 ready 后置位。
static PREVIEW_WEBVIEW_READY: AtomicBool = AtomicBool::new(false);
/// 前端尚未 ready 时挂起的显示请求。
static PREVIEW_PENDING_SHOW: Mutex<Option<PreviewPendingShow>> = Mutex::new(None);

/// 当前预览目标已经定下的左右方向：`(session_id, item_id, prefer_left)`。
///
/// hover 跟随指针时每帧都会重算布局。若每帧都按指针位置判方向，指针在卡片内横向划过
/// 窗口中线就会让整块面板飞到另一侧，所以方向只在换会话或换条目时决定一次。
static PREVIEW_PREFERRED_SIDE: Mutex<Option<(u64, String, bool)>> = Mutex::new(None);

#[cfg(target_os = "macos")]
tauri_panel! {
    panel!(PreviewPanel {
        config: {
            is_floating_panel: true,
            can_become_key_window: false,
            can_become_main_window: false
        }
    })
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewAnchorRect {
    pub left: f64,
    pub top: f64,
    pub width: f64,
    pub height: f64,
    /// 指针在剪贴板 webview 里的水平位置，决定面板往左还是往右弹。键盘预览为 None。
    pub pointer_x: Option<f64>,
}

/// 预览面板尺寸所需的内容度量，由命令层从记录算出。
#[derive(Clone, Debug, PartialEq)]
pub enum PreviewContentMetrics {
    /// 预览文本软切后的行数。
    Text { rows: u32 },
    /// 拆词视图的每个词块；面板宽度定下来后再按宽度折行算高度。
    Words { chips: Vec<PreviewWordChip> },
    /// 图片原始尺寸；记录缺尺寸时为 `None`，退回兜底面板大小。
    Image {
        width: Option<f64>,
        height: Option<f64>,
    },
    /// 文件条目：实际渲染条数与总条数（总数更多时底部多一行提示）。
    Files { shown: u32, total: u32 },
}

/// 拆词视图里的一个词块：按字符估出的宽度（px），以及它前面是否换段。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PreviewWordChip {
    pub width: f64,
    pub line_break: bool,
}

impl PreviewWordChip {
    /// 按前端词块样式估宽度：中日韩、全角与 emoji 记 1em，其余记 0.5em（界面字体实测西文平均约 0.47em），
    /// 再加左右内边距。
    pub fn new(text: &str, line_break: bool) -> Self {
        let ems: f64 = text
            .chars()
            .map(|c| {
                if is_wide_char(c) {
                    1.0
                } else {
                    PREVIEW_WORD_NARROW_CHAR_EM
                }
            })
            .sum();
        let width = ems * PREVIEW_WORD_FONT_SIZE + PREVIEW_WORD_CHIP_PADDING_X;

        Self {
            width: width.max(PREVIEW_WORD_CHIP_MIN_WIDTH),
            line_break,
        }
    }
}

/// 按一个字宽排版的字符：中日韩文字、全角符号与 emoji。
fn is_wide_char(c: char) -> bool {
    matches!(
        u32::from(c),
        0x1100..=0x115F
            | 0x2E80..=0xA4CF
            | 0xAC00..=0xD7A3
            | 0xF900..=0xFAFF
            | 0xFE30..=0xFE4F
            | 0xFF00..=0xFF60
            | 0xFFE0..=0xFFE6
            | 0x1F300..=0x1FAFF
            | 0x20000..=0x3FFFD
    )
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewClipboardWindowRect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

/// 显示器局部逻辑像素中的矩形。窗口几何全在 Rust 侧算，不再序列化给前端。
#[derive(Clone, Copy, Debug, PartialEq)]
struct PreviewRect {
    left: f64,
    top: f64,
    width: f64,
    height: f64,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum PreviewPlacement {
    Right,
    Left,
    Bottom,
    Top,
}

/// 面板矩形与它相对卡片的方位。
#[derive(Clone, Copy, Debug)]
struct PreviewGeometry {
    panel: PreviewRect,
    placement: PreviewPlacement,
}

/// 广播给预览页的状态。窗口几何已由 Rust 定好，前端只需要知道渲染哪条记录，
/// 以及面板落在卡片的哪一侧（决定展开动画的方向）。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClipboardPreviewState {
    pub request_id: u64,
    pub session_id: u64,
    pub item_id: String,
    pub placement: PreviewPlacement,
}

/// 打开或重定向预览窗口：按内容度量定出面板矩形，把窗口摆到该矩形上并广播状态。
///
/// `metrics` 为 `None` 表示记录已不存在或尚未取到，此时按空内容的最小面板显示。
pub fn show_clipboard_preview(
    app: &AppHandle,
    item_id: String,
    anchor: PreviewAnchorRect,
    metrics: Option<PreviewContentMetrics>,
) -> Result<Option<ClipboardPreviewState>> {
    validate_anchor(&anchor)?;

    if PREVIEW_SUPPRESSED.load(Ordering::SeqCst) || !is_clipboard_window_visible(app) {
        close_clipboard_preview_now(app)?;
        return Ok(None);
    }

    let request_id = PREVIEW_REQUEST_ID.fetch_add(1, Ordering::SeqCst) + 1;
    let session_id = preview_session_id_for_show();
    let window = ensure_preview_window(app)?;
    let monitor = resolve_preview_monitor(app)?;
    let monitor_bounds = preview_monitor_bounds(&monitor);
    let scale_factor = monitor.scale_factor();
    let clipboard_window = clipboard_window_rect(app);
    let prefer_left = preferred_panel_side(
        session_id,
        &item_id,
        &anchor,
        scale_factor,
        clipboard_window.as_ref(),
    );
    let geometry = build_preview_geometry(
        &anchor,
        metrics,
        scale_factor,
        &monitor_bounds,
        clipboard_window.as_ref(),
        prefer_left,
    );
    let bounds = preview_window_bounds(geometry.panel, &monitor_bounds, scale_factor);
    // 面板矩形没变且窗口确实可见时只广播状态，跳过定位 / 置顶 / show 这些窗口级调用。
    let reuse_shown_window =
        preview_window_bounds_unchanged(bounds_key(bounds)) && window.is_visible().unwrap_or(false);

    if !reuse_shown_window {
        prepare_preview_window_for_show(app, &window, bounds)?;
    }

    let state = ClipboardPreviewState {
        request_id,
        session_id,
        item_id,
        placement: geometry.placement,
    };

    set_preview_state(Some(state.clone()));
    window
        .emit(PREVIEW_UPDATED_EVENT, &state)
        .map_err(|e| anyhow::anyhow!(e))?;

    if !reuse_shown_window {
        // 冷启动：WebView 刚建出来，前端还没挂载。此时 show 出去的是一块带原生背板的
        // 空窗口，内容要等几百毫秒才补上。挂起显示，等 ready 再放出来。
        if PREVIEW_WEBVIEW_READY.load(Ordering::SeqCst) {
            show_preview_window(app, &window)?;
            set_preview_shown_bounds(Some(bounds_key(bounds)));
        } else {
            set_pending_show(Some(PreviewPendingShow { request_id, bounds }));
            schedule_pending_show_fallback(app.clone(), request_id);
        }
    }

    Ok(Some(state))
}

/// 预览页完成初始化：把冷启动期间挂起的显示补上。
pub fn on_preview_window_ready(app: &AppHandle) {
    PREVIEW_WEBVIEW_READY.store(true, Ordering::SeqCst);
    flush_pending_show(app, None);
}

/// 显示挂起的请求；`expected_request_id` 非空时只在请求仍是它时才显示。
fn flush_pending_show(app: &AppHandle, expected_request_id: Option<u64>) {
    let pending = {
        let mut guard = lock_pending_show();
        match guard.as_ref() {
            Some(pending) if expected_request_id.is_none_or(|id| id == pending.request_id) => {
                guard.take()
            }
            _ => None,
        }
    };
    let Some(pending) = pending else {
        return;
    };

    // 期间已经关掉或被更新的请求取代就不必再显示。
    if PREVIEW_REQUEST_ID.load(Ordering::SeqCst) != pending.request_id {
        return;
    }
    if get_clipboard_preview_state().ok().flatten().is_none() {
        return;
    }

    let Ok(window) = get_window(app, CLIPBOARD_PREVIEW_WINDOW_LABEL) else {
        return;
    };

    if let Err(error) = show_preview_window(app, &window) {
        log::error!("show preview window after ready failed: {error}");
        return;
    }

    set_preview_shown_bounds(Some(bounds_key(pending.bounds)));
}

/// 前端始终不回报 ready 时的兜底显示。
fn schedule_pending_show_fallback(app: AppHandle, request_id: u64) {
    thread::spawn(move || {
        thread::sleep(Duration::from_millis(PREVIEW_READY_TIMEOUT_MS));
        flush_pending_show(&app, Some(request_id));
    });
}

fn set_pending_show(pending: Option<PreviewPendingShow>) {
    *lock_pending_show() = pending;
}

fn lock_pending_show() -> MutexGuard<'static, Option<PreviewPendingShow>> {
    PREVIEW_PENDING_SHOW.lock().unwrap_or_else(|poisoned| {
        log::error!("preview pending show mutex poisoned, recovering");
        poisoned.into_inner()
    })
}

/// 隐藏预览窗口并清空当前预览状态。
///
/// 立即隐藏：窗口现在带原生背板，而背板不随网页内容一起淡出——留时间给退出动画的话，
/// 内容已经淡没了，那块 Mica 还会在屏幕上多停一瞬。
pub fn close_clipboard_preview(app: &AppHandle) -> Result<()> {
    close_clipboard_preview_now(app)
}

/// 立即隐藏预览窗口并清空状态；用于剪贴板窗口隐藏等不需要退出动画的路径。
pub fn close_clipboard_preview_now(app: &AppHandle) -> Result<()> {
    PREVIEW_REQUEST_ID.fetch_add(1, Ordering::SeqCst);
    set_preview_state(None);

    if let Some(window) = app.get_webview_window(CLIPBOARD_PREVIEW_WINDOW_LABEL) {
        window
            .emit(PREVIEW_UPDATED_EVENT, Option::<ClipboardPreviewState>::None)
            .map_err(|e| anyhow::anyhow!(e))?;
        hide_preview_window(app, &window)?;
    }

    Ok(())
}

/// 剪贴板窗口开始隐藏时压制后续过期 show 请求，并立即收起预览窗口。
pub fn suppress_for_clipboard_hide(app: &AppHandle) {
    PREVIEW_SUPPRESSED.store(true, Ordering::SeqCst);
    if let Err(error) = close_clipboard_preview_now(app) {
        log::error!("suppress preview on clipboard hide failed: {error}");
    }
}

/// 剪贴板窗口重新显示后允许新的预览请求进入，并预热预览 WebView。
pub fn resume_after_clipboard_show(app: &AppHandle) {
    PREVIEW_SUPPRESSED.store(false, Ordering::SeqCst);
    prewarm_preview_window(app);
}

/// 预热预览 WebView：冷启动要几百毫秒，等用户悬停才建窗那段延迟是看得见的。
///
/// 只在预览功能开着时预热，建完立刻按隐藏登记——空闲销毁计时照常跑，用不上的话
/// 60 秒后照样回收，不会因为预热就常驻一个 WebView。
fn prewarm_preview_window(app: &AppHandle) {
    let Some(store) = app.try_state::<SettingsStore>() else {
        return;
    };
    let preview = store.snapshot().clipboard.preview;

    if !preview.hover_enabled && !preview.space_enabled {
        return;
    }
    if app
        .get_webview_window(CLIPBOARD_PREVIEW_WINDOW_LABEL)
        .is_some()
    {
        return;
    }

    let app = app.clone();
    thread::spawn(move || {
        // 让剪贴板窗口先把自己显示完：建 WebView 要占主线程，抢在它前面会让呼出变慢。
        thread::sleep(Duration::from_millis(PREVIEW_PREWARM_DELAY_MS));

        if !is_clipboard_window_visible(&app) {
            return;
        }
        if app
            .get_webview_window(CLIPBOARD_PREVIEW_WINDOW_LABEL)
            .is_some()
        {
            return;
        }
        if let Err(error) = build_clipboard_preview_window(&app) {
            log::warn!("prewarm preview window failed: {error}");
            return;
        }

        // 窗口建出来就是隐藏态，登记一下让空闲销毁计时开始走。
        lifecycle::on_hidden(&app, CLIPBOARD_PREVIEW_WINDOW_LABEL, "prewarm");
    });
}

/// 返回预览窗口最近一次收到的状态，供预览页首屏补拉。
pub fn get_clipboard_preview_state() -> Result<Option<ClipboardPreviewState>> {
    let guard = PREVIEW_STATE.lock().unwrap_or_else(|poisoned| {
        log::error!("preview state mutex poisoned on get, recovering");
        poisoned.into_inner()
    });

    Ok(guard.clone())
}

/// 按需重建预览窗口。预览窗口不再由 Tauri 配置预创建（改为空闲销毁 + 按需重建），
/// 所有选项必须在此用 builder 完整复刻原 `tauri.conf.json` 声明，否则重建后行为漂移。
///
/// 建窗后保持 `visible: false`：定位与显示由预览 show 流程统一处理；
/// macOS 的 NSPanel 转换由 [`ensure_preview_window`] 在每次取窗时兜底执行。
pub fn build_clipboard_preview_window(app: &AppHandle) -> Result<()> {
    let _guard = PREVIEW_BUILD_LOCK.lock().unwrap_or_else(|poisoned| {
        log::error!("preview build mutex poisoned, recovering");
        poisoned.into_inner()
    });

    if app
        .get_webview_window(CLIPBOARD_PREVIEW_WINDOW_LABEL)
        .is_some()
    {
        return Ok(());
    }

    // 空闲销毁后重建出来的是一个全新的隐藏窗口：上一轮的显示记录和 ready 状态都不再成立。
    set_preview_shown_bounds(None);
    set_pending_show(None);
    PREVIEW_WEBVIEW_READY.store(false, Ordering::SeqCst);

    let window = WebviewWindowBuilder::new(
        app,
        CLIPBOARD_PREVIEW_WINDOW_LABEL,
        WebviewUrl::App("index.html/#/preview".into()),
    )
    .title("KwikPaste Preview")
    // 建窗尺寸只是首帧布局的初值（显示前一定会按面板矩形重设）。给 1x1 会让第一帧在
    // 一个像素的视口里排版，内容溢出后闪出一圈原生滚动条，所以直接用兜底面板尺寸。
    .inner_size(PREVIEW_PANEL_FALLBACK_SIZE.0, PREVIEW_PANEL_FALLBACK_SIZE.1)
    .resizable(false)
    .maximizable(false)
    .minimizable(false)
    .always_on_top(true)
    .decorations(false)
    // 不要系统阴影：无边框窗口靠保留 WS_THICKFRAME 才能拿到 DWM 阴影，而那圈
    // 调整边框（两侧与底部各 8px、顶部 1px）会在透明窗口上被画成一条灰带。
    // 浮起感交给原生材质和 1px 边框；圆角在建窗后向 DWM 单独申请。
    .shadow(false)
    .transparent(true)
    .skip_taskbar(true)
    .focused(false)
    .focusable(false)
    // 面板可以点选词语：不激活的面板上第一下点击也要直接交给网页。
    .accept_first_mouse(true)
    .disable_drag_drop_handler()
    .visible(false)
    .build()
    .map_err(|err| anyhow::anyhow!("build clipboard preview window: {err}"))?;

    super::round_popup_corners(&window);
    // 原生材质只在建窗时铺一次。它是先清掉再重设的，窗口可见时每次换条目都重铺，
    // DWM 会把背板拆了重建，整块面板跟着闪一下。外观设置变更另有 apply_existing 兜着。
    super::apply_window_material(app, CLIPBOARD_PREVIEW_WINDOW_LABEL);

    Ok(())
}

fn bounds_key(bounds: (PhysicalPosition<i32>, PhysicalSize<u32>)) -> PreviewBoundsKey {
    let (position, size) = bounds;

    (position.x, position.y, size.width, size.height)
}

/// 判断预览窗口最近一次显示用的就是这块面板边界。
fn preview_window_bounds_unchanged(bounds: PreviewBoundsKey) -> bool {
    *lock_preview_shown_bounds() == Some(bounds)
}

fn set_preview_shown_bounds(bounds: Option<PreviewBoundsKey>) {
    *lock_preview_shown_bounds() = bounds;
}

fn lock_preview_shown_bounds() -> MutexGuard<'static, Option<PreviewBoundsKey>> {
    PREVIEW_SHOWN_BOUNDS.lock().unwrap_or_else(|poisoned| {
        log::error!("preview shown bounds mutex poisoned, recovering");
        poisoned.into_inner()
    })
}

fn set_preview_state(state: Option<ClipboardPreviewState>) {
    let mut guard = PREVIEW_STATE.lock().unwrap_or_else(|poisoned| {
        log::error!("preview state mutex poisoned on set, recovering");
        poisoned.into_inner()
    });
    *guard = state;
}

/// 判断剪贴板窗口是否仍处于可见状态，防止过期 hover 请求在剪贴板窗口隐藏后唤起预览。
fn is_clipboard_window_visible(app: &AppHandle) -> bool {
    app.get_webview_window(CLIPBOARD_WINDOW_LABEL)
        .and_then(|window| window.is_visible().ok())
        .unwrap_or(false)
}

/// 返回本次 show 所属的可见会话 id；隐藏后再次 show 会开启新会话。
fn preview_session_id_for_show() -> u64 {
    let guard = PREVIEW_STATE.lock().unwrap_or_else(|poisoned| {
        log::error!("preview state mutex poisoned on session, recovering");
        poisoned.into_inner()
    });

    if guard.is_some() {
        return PREVIEW_SESSION_ID.load(Ordering::SeqCst);
    }

    PREVIEW_SESSION_ID.fetch_add(1, Ordering::SeqCst) + 1
}

fn validate_anchor(anchor: &PreviewAnchorRect) -> Result<()> {
    let values = [anchor.left, anchor.top, anchor.width, anchor.height];
    if !values.iter().all(|value| value.is_finite()) || anchor.width <= 0.0 || anchor.height <= 0.0
    {
        return Err(anyhow::anyhow!("preview anchor is invalid").into());
    }
    if anchor.pointer_x.is_some_and(|pointer| !pointer.is_finite()) {
        return Err(anyhow::anyhow!("preview pointer is invalid").into());
    }

    Ok(())
}

/// 取预览窗口；已被空闲销毁（或尚未创建）时按需重建。
/// macOS 下每次都兜底确保 NSPanel 转换完成，覆盖重建后的全新窗口。
fn ensure_preview_window(app: &AppHandle) -> Result<WebviewWindow> {
    if app
        .get_webview_window(CLIPBOARD_PREVIEW_WINDOW_LABEL)
        .is_none()
    {
        build_clipboard_preview_window(app)?;
    }

    let window = get_window(app, CLIPBOARD_PREVIEW_WINDOW_LABEL)?;

    #[cfg(target_os = "macos")]
    ensure_macos_preview_panel(app, &window)?;

    Ok(window)
}

fn raise_preview_window(app: &AppHandle, window: &WebviewWindow) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        let _ = window;
        set_macos_preview_panel_level(app)
    }

    #[cfg(target_os = "windows")]
    {
        let _ = app;
        window
            .set_always_on_top(true)
            .map_err(|e| anyhow::anyhow!(e))?;
        raise_windows_preview_window(window, false)?;

        Ok(())
    }
}

fn prepare_preview_window_for_show(
    app: &AppHandle,
    window: &WebviewWindow,
    bounds: (PhysicalPosition<i32>, PhysicalSize<u32>),
) -> Result<()> {
    apply_preview_window_bounds(window, bounds)?;
    // 面板接收鼠标：指针停在面板上时它不会自动收起，可以直接点选、拖选文本里的词。
    window
        .set_ignore_cursor_events(false)
        .map_err(|e| anyhow::anyhow!(e))?;
    raise_preview_window(app, window)
}

/// 平台 show 收口点；成功后推进生命周期到 `Visible`，使未触发的空闲销毁计时器过期。
fn show_preview_window(app: &AppHandle, window: &WebviewWindow) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        let _ = window;
        show_macos_preview_panel(app)?;
    }

    #[cfg(target_os = "windows")]
    {
        window.show().map_err(|e| anyhow::anyhow!(e))?;
        raise_windows_preview_window(window, true)?;
    }

    lifecycle::on_shown(app, CLIPBOARD_PREVIEW_WINDOW_LABEL);

    Ok(())
}

/// 平台 hide 收口点；成功后推进生命周期到 `HiddenWarm`，启动空闲销毁计时。
/// 对已隐藏窗口的重复 hide（如剪贴板窗口隐藏时的压制路径）也会走到这里，
/// 由生命周期管理器对重复进入 `HiddenWarm` 去重计时。
fn hide_preview_window(app: &AppHandle, window: &WebviewWindow) -> Result<()> {
    set_preview_shown_bounds(None);
    set_pending_show(None);

    #[cfg(target_os = "macos")]
    {
        let _ = window;
        hide_macos_preview_panel(app)?;
    }

    #[cfg(target_os = "windows")]
    window.hide().map_err(|e| anyhow::anyhow!(e))?;

    lifecycle::on_hidden(app, CLIPBOARD_PREVIEW_WINDOW_LABEL, "preview-hide");

    Ok(())
}

#[cfg(target_os = "macos")]
fn ensure_macos_preview_panel(app: &AppHandle, window: &WebviewWindow) -> Result<()> {
    let handle = app.clone();
    let preview_window = window.clone();
    let (tx, rx) = std::sync::mpsc::channel();

    app.run_on_main_thread(move || {
        let result = setup_macos_preview_panel(&handle, &preview_window);
        let _ = tx.send(result);
    })
    .map_err(|e| anyhow::anyhow!(e))?;

    rx.recv()
        .map_err(|e| anyhow::anyhow!("preview panel setup channel closed: {e}"))?
}

#[cfg(target_os = "macos")]
fn setup_macos_preview_panel(app: &AppHandle, window: &WebviewWindow) -> Result<()> {
    let panel = match app.get_webview_panel(CLIPBOARD_PREVIEW_WINDOW_LABEL) {
        Ok(panel) => panel,
        Err(_) => window
            .to_panel::<PreviewPanel>()
            .map_err(|e| anyhow::anyhow!("to_panel failed: {e:?}"))?,
    };

    panel.set_level(PanelLevel::Status.value());
    // 不激活 App：在面板上点选词语时，前台仍是要粘贴的目标应用。
    panel.set_style_mask(StyleMask::empty().nonactivating_panel().into());
    panel.set_ignores_mouse_events(false);

    Ok(())
}

#[cfg(target_os = "macos")]
fn set_macos_preview_panel_level(app: &AppHandle) -> Result<()> {
    let handle = app.clone();

    app.run_on_main_thread(move || {
        if let Ok(panel) = handle.get_webview_panel(CLIPBOARD_PREVIEW_WINDOW_LABEL) {
            panel.set_level(PanelLevel::Status.value());
            panel.set_ignores_mouse_events(false);
        }
    })
    .map_err(|e| anyhow::anyhow!(e))?;

    Ok(())
}

#[cfg(target_os = "macos")]
fn show_macos_preview_panel(app: &AppHandle) -> Result<()> {
    let handle = app.clone();
    let (tx, rx) = std::sync::mpsc::channel();

    app.run_on_main_thread(move || {
        let result = (|| -> Result<()> {
            let panel = handle
                .get_webview_panel(CLIPBOARD_PREVIEW_WINDOW_LABEL)
                .map_err(|e| anyhow::anyhow!("preview panel not found: {e:?}"))?;
            panel.set_ignores_mouse_events(false);
            panel.set_level(PanelLevel::Status.value());
            panel.show();
            Ok(())
        })();
        let _ = tx.send(result);
    })
    .map_err(|e| anyhow::anyhow!(e))?;

    rx.recv()
        .map_err(|e| anyhow::anyhow!("preview panel show channel closed: {e}"))?
}

#[cfg(target_os = "macos")]
fn hide_macos_preview_panel(app: &AppHandle) -> Result<()> {
    let handle = app.clone();

    app.run_on_main_thread(move || {
        if let Ok(panel) = handle.get_webview_panel(CLIPBOARD_PREVIEW_WINDOW_LABEL) {
            panel.hide();
        }
    })
    .map_err(|e| anyhow::anyhow!(e))?;

    Ok(())
}

/// 预览面板可见且包含该 physical 坐标；剪贴板窗口的外部点击隐藏据此把面板当作窗内。
#[cfg(target_os = "windows")]
pub fn contains_physical_point(app: &AppHandle, x: i32, y: i32) -> bool {
    let Some(window) = app.get_webview_window(CLIPBOARD_PREVIEW_WINDOW_LABEL) else {
        return false;
    };
    if !window.is_visible().unwrap_or(false) {
        return false;
    }
    let (Ok(position), Ok(size)) = (window.outer_position(), window.outer_size()) else {
        return false;
    };

    x >= position.x
        && x < position.x + size.width as i32
        && y >= position.y
        && y < position.y + size.height as i32
}

/// 将预览窗口重新压到 Windows topmost 栈顶，避免被同为 always-on-top 的剪贴板窗口盖住。
#[cfg(target_os = "windows")]
fn raise_windows_preview_window(window: &WebviewWindow, show: bool) -> Result<()> {
    let raw_hwnd = window.hwnd().map_err(|e| anyhow::anyhow!(e))?;
    let hwnd = HWND(raw_hwnd.0 as isize);
    let mut flags = SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE;

    if show {
        flags |= SWP_SHOWWINDOW;
    }

    unsafe {
        SetWindowPos(hwnd, HWND_TOPMOST, 0, 0, 0, 0, flags).map_err(|e| anyhow::anyhow!(e))?;
    }

    Ok(())
}

fn resolve_preview_monitor(app: &AppHandle) -> Result<tauri::Monitor> {
    let clipboard_window = get_window(app, CLIPBOARD_WINDOW_LABEL)?;
    if let Some(monitor) = clipboard_window
        .current_monitor()
        .map_err(|e| anyhow::anyhow!(e))?
    {
        return Ok(monitor);
    }

    clipboard_window
        .primary_monitor()
        .map_err(|e| anyhow::anyhow!(e))?
        .ok_or_else(|| anyhow::anyhow!("primary monitor not found").into())
}

/// 用完整显示器区域作为面板可落位的范围；`work_area` 会被 Dock / 任务栏压缩，
/// 面板本身已经留了 `PREVIEW_PANEL_MARGIN` 的安全边距。
fn preview_monitor_bounds(monitor: &tauri::Monitor) -> PhysicalRect<i32, u32> {
    PhysicalRect {
        position: *monitor.position(),
        size: *monitor.size(),
    }
}

fn apply_preview_window_bounds(
    window: &WebviewWindow,
    bounds: (PhysicalPosition<i32>, PhysicalSize<u32>),
) -> Result<()> {
    let (position, size) = bounds;

    window.set_size(size).map_err(|e| anyhow::anyhow!(e))?;
    window
        .set_position(position)
        .map_err(|e| anyhow::anyhow!(e))?;

    Ok(())
}

/// 把显示器局部逻辑矩形换算成窗口的物理位置与尺寸。
fn preview_window_bounds(
    panel: PreviewRect,
    monitor: &PhysicalRect<i32, u32>,
    scale_factor: f64,
) -> (PhysicalPosition<i32>, PhysicalSize<u32>) {
    let x = monitor.position.x + (panel.left * scale_factor).round() as i32;
    let y = monitor.position.y + (panel.top * scale_factor).round() as i32;
    let width = (panel.width * scale_factor).round().max(1.0) as u32;
    let height = (panel.height * scale_factor).round().max(1.0) as u32;

    (
        PhysicalPosition::new(x, y),
        PhysicalSize::new(width, height),
    )
}

/// 按内容度量与卡片位置算出面板矩形和方位。
fn build_preview_geometry(
    anchor: &PreviewAnchorRect,
    metrics: Option<PreviewContentMetrics>,
    scale_factor: f64,
    monitor: &PhysicalRect<i32, u32>,
    clipboard_window: Option<&PreviewClipboardWindowRect>,
    prefer_left: bool,
) -> PreviewGeometry {
    let monitor_rect = PreviewRect {
        left: 0.0,
        top: 0.0,
        width: monitor.size.width as f64 / scale_factor,
        height: monitor.size.height as f64 / scale_factor,
    };
    let available = inset_rect(monitor_rect, PREVIEW_PANEL_MARGIN);
    let source_rect = resolve_source_rect(
        anchor,
        scale_factor,
        monitor,
        clipboard_window,
        monitor_rect,
    );
    let size = resolve_panel_size(metrics, available);
    let placement = resolve_placement(source_rect, available, size, prefer_left);
    let panel = clamp_rect(raw_panel_rect(source_rect, placement, size), available);

    PreviewGeometry { panel, placement }
}

/// 按内容度量算出面板尺寸，规则与预览页的渲染一一对应。
fn resolve_panel_size(
    metrics: Option<PreviewContentMetrics>,
    available: PreviewRect,
) -> (f64, f64) {
    let max_width = PREVIEW_PANEL_MAX_WIDTH.min(available.width);
    let max_height = PREVIEW_PANEL_MAX_HEIGHT.min(available.height);
    let min_width = PREVIEW_PANEL_MIN_WIDTH.min(max_width);
    let min_height = PREVIEW_PANEL_MIN_HEIGHT.min(max_height);

    let (width, height) = match metrics {
        // 记录已消失时只剩空状态，按最小面板显示。
        None => (
            max_width,
            PREVIEW_PANEL_HEADER_HEIGHT + PREVIEW_EMPTY_CONTENT_HEIGHT,
        ),
        Some(PreviewContentMetrics::Text { rows }) => (
            max_width,
            PREVIEW_PANEL_HEADER_HEIGHT + text_content_height(rows),
        ),
        Some(PreviewContentMetrics::Words { chips }) => (
            max_width,
            PREVIEW_PANEL_HEADER_HEIGHT
                + words_content_height(&chips, max_width - PREVIEW_WORDS_PADDING * 2.0),
        ),
        Some(PreviewContentMetrics::Files { shown, total }) => (
            max_width,
            PREVIEW_PANEL_HEADER_HEIGHT + files_content_height(shown, total),
        ),
        Some(PreviewContentMetrics::Image { width, height }) => {
            image_panel_size(width, height, max_width, max_height)
        }
    };

    (
        width.clamp(min_width, max_width),
        height.clamp(min_height, max_height),
    )
}

/// 文本 viewer 走虚拟行，高度按软切后的行数估。
fn text_content_height(rows: u32) -> f64 {
    if rows == 0 {
        return PREVIEW_EMPTY_CONTENT_HEIGHT;
    }

    rows as f64 * PREVIEW_TEXT_ROW_HEIGHT + PREVIEW_TEXT_VERTICAL_PADDING
}

/// 拆词视图按面板内容宽度模拟 flex 折行：换段的空行多占一道间距，
/// 比整行还宽的词块在块内折行，按多出的文字行加高。
fn words_content_height(chips: &[PreviewWordChip], content_width: f64) -> f64 {
    if chips.is_empty() || content_width <= 0.0 {
        return PREVIEW_EMPTY_CONTENT_HEIGHT;
    }

    let mut rows = 0_u32;
    let mut breaks = 0_u32;
    let mut wrapped_lines = 0.0;
    let mut line_width = 0.0;

    for chip in chips {
        let width = chip.width.min(content_width);
        wrapped_lines += (chip.width / content_width).ceil().max(1.0) - 1.0;

        if rows == 0 {
            rows = 1;
            line_width = width;
            continue;
        }

        if chip.line_break {
            breaks += 1;
            rows += 1;
            line_width = width;
            continue;
        }

        if line_width + PREVIEW_WORD_GAP + width > content_width {
            rows += 1;
            line_width = width;
        } else {
            line_width += PREVIEW_WORD_GAP + width;
        }
    }

    f64::from(rows) * PREVIEW_WORD_CHIP_HEIGHT
        + f64::from(rows - 1 + breaks) * PREVIEW_WORD_GAP
        + wrapped_lines * PREVIEW_WORD_LINE_HEIGHT
        + PREVIEW_WORDS_PADDING * 2.0
}

/// 文件 viewer 的高度按已返回条数估，被截断时多留一行提示。
fn files_content_height(shown: u32, total: u32) -> f64 {
    if shown == 0 {
        return PREVIEW_EMPTY_CONTENT_HEIGHT;
    }

    let footer = if total > shown {
        PREVIEW_FILE_MORE_FOOTER_HEIGHT
    } else {
        0.0
    };

    shown as f64 * PREVIEW_FILE_ROW_HEIGHT + PREVIEW_FILE_VERTICAL_PADDING + footer
}

/// 图片按原始宽高等比缩放到面板上限内，面板正好裹住缩放后的图加内边距。
fn image_panel_size(
    width: Option<f64>,
    height: Option<f64>,
    max_width: f64,
    max_height: f64,
) -> (f64, f64) {
    let (Some(width), Some(height)) = (width, height) else {
        return PREVIEW_PANEL_FALLBACK_SIZE;
    };
    if width <= 0.0 || height <= 0.0 {
        return PREVIEW_PANEL_FALLBACK_SIZE;
    }

    let max_image_width = (max_width - PREVIEW_PANEL_IMAGE_PADDING_X).max(1.0);
    let max_image_height =
        (max_height - PREVIEW_PANEL_HEADER_HEIGHT - PREVIEW_PANEL_IMAGE_PADDING_Y).max(1.0);
    let scale = 1.0_f64
        .min(max_image_width / width)
        .min(max_image_height / height);

    (
        (width * scale).ceil() + PREVIEW_PANEL_IMAGE_PADDING_X,
        (height * scale).ceil() + PREVIEW_PANEL_HEADER_HEIGHT + PREVIEW_PANEL_IMAGE_PADDING_Y,
    )
}

/// 把卡片矩形从剪贴板 webview 坐标映射到显示器局部逻辑坐标。
fn resolve_source_rect(
    anchor: &PreviewAnchorRect,
    scale_factor: f64,
    monitor: &PhysicalRect<i32, u32>,
    clipboard_window: Option<&PreviewClipboardWindowRect>,
    monitor_rect: PreviewRect,
) -> PreviewRect {
    let source = if let Some(clipboard_window) = clipboard_window {
        let main_rect = PreviewRect {
            left: (clipboard_window.x - monitor.position.x) as f64 / scale_factor,
            top: (clipboard_window.y - monitor.position.y) as f64 / scale_factor,
            width: clipboard_window.width as f64 / scale_factor,
            height: clipboard_window.height as f64 / scale_factor,
        };

        PreviewRect {
            left: main_rect.left + anchor.left,
            top: main_rect.top + anchor.top,
            width: anchor.width,
            height: anchor.height,
        }
    } else {
        PreviewRect {
            left: anchor.left,
            top: anchor.top,
            width: anchor.width,
            height: anchor.height,
        }
    };

    intersect_rect(source, monitor_rect).unwrap_or_else(|| clamp_rect(source, monitor_rect))
}

/// 返回本次预览目标的左右方向，同一会话内的同一条目复用第一次的判定。
fn preferred_panel_side(
    session_id: u64,
    item_id: &str,
    anchor: &PreviewAnchorRect,
    scale_factor: f64,
    clipboard_window: Option<&PreviewClipboardWindowRect>,
) -> bool {
    let mut guard = PREVIEW_PREFERRED_SIDE.lock().unwrap_or_else(|poisoned| {
        log::error!("preview preferred side mutex poisoned, recovering");
        poisoned.into_inner()
    });

    if let Some((session, item, prefer_left)) = guard.as_ref() {
        if *session == session_id && item == item_id {
            return *prefer_left;
        }
    }

    let prefer_left = prefers_left_panel(anchor, scale_factor, clipboard_window);
    *guard = Some((session_id, item_id.to_owned(), prefer_left));

    prefer_left
}

/// hover 预览的左右方向跟随指针在剪贴板窗口里的水平位置：指针偏左就往左弹，偏右就往右弹。
/// 键盘预览没有指针、拿不到窗口几何时保持默认的先右后左。
fn prefers_left_panel(
    anchor: &PreviewAnchorRect,
    scale_factor: f64,
    clipboard_window: Option<&PreviewClipboardWindowRect>,
) -> bool {
    let (Some(pointer_x), Some(window)) = (anchor.pointer_x, clipboard_window) else {
        return false;
    };

    pointer_x < window.width as f64 / scale_factor / 2.0
}

/// 按偏好侧依次找放得下的方位；首选一侧放不下就回落到另一侧，再回落到上下。
fn resolve_placement(
    source_rect: PreviewRect,
    available: PreviewRect,
    size: (f64, f64),
    prefer_left: bool,
) -> PreviewPlacement {
    let (preferred, opposite) = if prefer_left {
        (PreviewPlacement::Left, PreviewPlacement::Right)
    } else {
        (PreviewPlacement::Right, PreviewPlacement::Left)
    };
    let candidates = [
        preferred,
        opposite,
        PreviewPlacement::Bottom,
        PreviewPlacement::Top,
    ];

    candidates
        .into_iter()
        .find(|placement| placement_fits(*placement, source_rect, available, size))
        .unwrap_or(preferred)
}

/// 判断该方位能否在可用区域里完整放下面板（含与卡片之间的间距）。
fn placement_fits(
    placement: PreviewPlacement,
    source_rect: PreviewRect,
    available: PreviewRect,
    (width, height): (f64, f64),
) -> bool {
    match placement {
        PreviewPlacement::Right => {
            source_rect.right() + PREVIEW_PANEL_GAP + width <= available.right()
        }
        PreviewPlacement::Left => source_rect.left - PREVIEW_PANEL_GAP - width >= available.left,
        PreviewPlacement::Bottom => {
            source_rect.bottom() + PREVIEW_PANEL_GAP + height <= available.bottom()
        }
        PreviewPlacement::Top => source_rect.top - PREVIEW_PANEL_GAP - height >= available.top,
    }
}

fn raw_panel_rect(
    source_rect: PreviewRect,
    placement: PreviewPlacement,
    (width, height): (f64, f64),
) -> PreviewRect {
    let centered_top = source_rect.center_y() - height / 2.0;
    let centered_left = source_rect.center_x() - width / 2.0;

    match placement {
        PreviewPlacement::Right => PreviewRect {
            left: source_rect.right() + PREVIEW_PANEL_GAP,
            top: centered_top,
            width,
            height,
        },
        PreviewPlacement::Left => PreviewRect {
            left: source_rect.left - PREVIEW_PANEL_GAP - width,
            top: centered_top,
            width,
            height,
        },
        PreviewPlacement::Bottom => PreviewRect {
            left: centered_left,
            top: source_rect.bottom() + PREVIEW_PANEL_GAP,
            width,
            height,
        },
        PreviewPlacement::Top => PreviewRect {
            left: centered_left,
            top: source_rect.top - PREVIEW_PANEL_GAP - height,
            width,
            height,
        },
    }
}

fn clamp_rect(rect: PreviewRect, bounds: PreviewRect) -> PreviewRect {
    let max_left = (bounds.right() - rect.width).max(bounds.left);
    let max_top = (bounds.bottom() - rect.height).max(bounds.top);

    PreviewRect {
        left: rect.left.clamp(bounds.left, max_left),
        top: rect.top.clamp(bounds.top, max_top),
        width: rect.width,
        height: rect.height,
    }
}

fn intersect_rect(a: PreviewRect, b: PreviewRect) -> Option<PreviewRect> {
    let left = a.left.max(b.left);
    let top = a.top.max(b.top);
    let right = a.right().min(b.right());
    let bottom = a.bottom().min(b.bottom());

    if right <= left || bottom <= top {
        return None;
    }

    Some(PreviewRect {
        left,
        top,
        width: right - left,
        height: bottom - top,
    })
}

fn inset_rect(rect: PreviewRect, amount: f64) -> PreviewRect {
    PreviewRect {
        left: rect.left + amount,
        top: rect.top + amount,
        width: (rect.width - amount * 2.0).max(1.0),
        height: (rect.height - amount * 2.0).max(1.0),
    }
}

/// 返回剪贴板窗口内容区的屏幕几何，用于映射 WebView DOM rect 到预览 overlay 坐标。
fn clipboard_window_rect(app: &AppHandle) -> Option<PreviewClipboardWindowRect> {
    let window = app.get_webview_window(CLIPBOARD_WINDOW_LABEL)?;
    let pos = window.inner_position().ok()?;
    let size = window.inner_size().ok()?;

    Some(PreviewClipboardWindowRect {
        x: pos.x,
        y: pos.y,
        width: size.width,
        height: size.height,
    })
}

impl PreviewRect {
    fn right(self) -> f64 {
        self.left + self.width
    }

    fn bottom(self) -> f64 {
        self.top + self.height
    }

    fn center_x(self) -> f64 {
        self.left + self.width / 2.0
    }

    fn center_y(self) -> f64 {
        self.top + self.height / 2.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn monitor() -> PhysicalRect<i32, u32> {
        PhysicalRect {
            position: PhysicalPosition::new(0, 0),
            size: PhysicalSize::new(1920, 1080),
        }
    }

    fn available() -> PreviewRect {
        inset_rect(
            PreviewRect {
                left: 0.0,
                top: 0.0,
                width: 1920.0,
                height: 1080.0,
            },
            PREVIEW_PANEL_MARGIN,
        )
    }

    fn card(left: f64, top: f64) -> PreviewRect {
        PreviewRect {
            left,
            top,
            width: 320.0,
            height: 80.0,
        }
    }

    fn anchor_with_pointer(pointer_x: Option<f64>) -> PreviewAnchorRect {
        PreviewAnchorRect {
            left: 20.0,
            top: 30.0,
            width: 320.0,
            height: 80.0,
            pointer_x,
        }
    }

    fn clipboard_rect() -> PreviewClipboardWindowRect {
        PreviewClipboardWindowRect {
            x: 300,
            y: 250,
            width: 800,
            height: 600,
        }
    }

    // 面板高度跟着文本行数走，超过上限后停在 max。
    #[test]
    fn text_panel_height_follows_row_count() {
        let (width, short) =
            resolve_panel_size(Some(PreviewContentMetrics::Text { rows: 3 }), available());
        assert_eq!(width, PREVIEW_PANEL_MAX_WIDTH);
        assert_eq!(
            short,
            PREVIEW_PANEL_HEADER_HEIGHT
                + 3.0 * PREVIEW_TEXT_ROW_HEIGHT
                + PREVIEW_TEXT_VERTICAL_PADDING
        );

        let (_, tall) =
            resolve_panel_size(Some(PreviewContentMetrics::Text { rows: 400 }), available());
        assert_eq!(tall, PREVIEW_PANEL_MAX_HEIGHT);

        let (_, empty) =
            resolve_panel_size(Some(PreviewContentMetrics::Text { rows: 0 }), available());
        assert_eq!(
            empty,
            PREVIEW_PANEL_HEADER_HEIGHT + PREVIEW_EMPTY_CONTENT_HEIGHT
        );
    }

    fn chips(text: &str) -> Vec<PreviewWordChip> {
        text.chars()
            .map(|c| PreviewWordChip::new(&c.to_string(), false))
            .collect()
    }

    // 汉字词块 14px 字宽加 12px 内边距，448px 的内容宽度一行放 15 块。
    #[test]
    fn word_panel_height_wraps_chips_by_panel_width() {
        let one_row = PREVIEW_WORD_CHIP_HEIGHT + PREVIEW_WORDS_PADDING * 2.0;

        assert_eq!(
            words_content_height(&chips(&"字".repeat(15)), 448.0),
            one_row
        );
        assert_eq!(
            words_content_height(&chips(&"字".repeat(16)), 448.0),
            one_row + PREVIEW_WORD_CHIP_HEIGHT + PREVIEW_WORD_GAP
        );
    }

    // 换段另起一行，并且空行本身再多占一道间距。
    #[test]
    fn word_panel_height_counts_paragraph_breaks() {
        let mut words = chips("一二");
        words.push(PreviewWordChip::new("三", true));

        assert_eq!(
            words_content_height(&words, 448.0),
            PREVIEW_WORD_CHIP_HEIGHT * 2.0 + PREVIEW_WORD_GAP * 2.0 + PREVIEW_WORDS_PADDING * 2.0
        );
    }

    #[test]
    fn word_chip_width_follows_script_and_minimum() {
        assert_eq!(PreviewWordChip::new("字", false).width, 26.0);
        assert_eq!(
            PreviewWordChip::new("*", false).width,
            PREVIEW_WORD_CHIP_MIN_WIDTH
        );
        assert_eq!(PreviewWordChip::new("W2000", false).width, 47.0);
    }

    // 被截断的文件列表底部多一行“还有多少条”的提示。
    #[test]
    fn files_panel_height_accounts_for_the_truncation_footer() {
        let (_, exact) = resolve_panel_size(
            Some(PreviewContentMetrics::Files { shown: 4, total: 4 }),
            available(),
        );
        let (_, truncated) = resolve_panel_size(
            Some(PreviewContentMetrics::Files {
                shown: 4,
                total: 90,
            }),
            available(),
        );

        assert_eq!(truncated - exact, PREVIEW_FILE_MORE_FOOTER_HEIGHT);
    }

    // 图片按原始比例裹紧；缺尺寸时退回兜底大小。
    #[test]
    fn image_panel_size_keeps_the_source_aspect_ratio() {
        let (width, height) = resolve_panel_size(
            Some(PreviewContentMetrics::Image {
                width: Some(200.0),
                height: Some(100.0),
            }),
            available(),
        );

        // 小图撑不满最小宽度，面板取 min-width；高度仍然只裹住图片加内边距。
        assert_eq!(width, PREVIEW_PANEL_MIN_WIDTH);
        assert_eq!(
            height,
            100.0 + PREVIEW_PANEL_HEADER_HEIGHT + PREVIEW_PANEL_IMAGE_PADDING_Y
        );

        let (fallback_width, fallback_height) = resolve_panel_size(
            Some(PreviewContentMetrics::Image {
                width: None,
                height: None,
            }),
            available(),
        );
        assert_eq!(
            (fallback_width, fallback_height),
            PREVIEW_PANEL_FALLBACK_SIZE
        );
    }

    // 超大图缩放后仍然落在面板上限内。
    #[test]
    fn oversized_image_is_scaled_into_the_panel() {
        let (width, height) = resolve_panel_size(
            Some(PreviewContentMetrics::Image {
                width: Some(4000.0),
                height: Some(3000.0),
            }),
            available(),
        );

        assert!(width <= PREVIEW_PANEL_MAX_WIDTH);
        assert!(height <= PREVIEW_PANEL_MAX_HEIGHT);
    }

    // 右边放得下就放右边，放不下回落到左边。
    #[test]
    fn placement_prefers_the_requested_side_and_falls_back() {
        let size = (PREVIEW_PANEL_MAX_WIDTH, 240.0);

        assert!(matches!(
            resolve_placement(card(700.0, 400.0), available(), size, false),
            PreviewPlacement::Right
        ));
        assert!(matches!(
            resolve_placement(card(700.0, 400.0), available(), size, true),
            PreviewPlacement::Left
        ));
        // 贴着左边缘时，即使首选左边也只能回落到右边。
        assert!(matches!(
            resolve_placement(card(40.0, 400.0), available(), size, true),
            PreviewPlacement::Right
        ));
    }

    // 面板始终被夹在留了安全边距的可用区域内。
    #[test]
    fn panel_stays_inside_the_available_bounds() {
        let geometry = build_preview_geometry(
            &PreviewAnchorRect {
                left: 20.0,
                top: 1000.0,
                width: 320.0,
                height: 80.0,
                pointer_x: None,
            },
            Some(PreviewContentMetrics::Text { rows: 40 }),
            1.0,
            &monitor(),
            None,
            false,
        );
        let bounds = available();

        assert!(geometry.panel.left >= bounds.left);
        assert!(geometry.panel.top >= bounds.top);
        assert!(geometry.panel.right() <= bounds.right());
        assert!(geometry.panel.bottom() <= bounds.bottom());
    }

    // 卡片矩形按剪贴板窗口的位置平移到显示器局部坐标。
    #[test]
    fn maps_anchor_from_clipboard_window_to_monitor_local_rect() {
        let source = resolve_source_rect(
            &anchor_with_pointer(None),
            2.0,
            &PhysicalRect {
                position: PhysicalPosition::new(100, 50),
                size: PhysicalSize::new(2400, 1600),
            },
            Some(&clipboard_rect()),
            PreviewRect {
                left: 0.0,
                top: 0.0,
                width: 1200.0,
                height: 800.0,
            },
        );

        assert_eq!(source.left, 120.0);
        assert_eq!(source.top, 130.0);
        assert_eq!(source.width, 320.0);
        assert_eq!(source.height, 80.0);
    }

    // 指针在剪贴板窗口左半边就往左弹，右半边往右弹；键盘预览没有指针，保持先右后左。
    #[test]
    fn pointer_side_inside_the_clipboard_window_picks_the_panel_side() {
        let clipboard = clipboard_rect();

        assert!(prefers_left_panel(
            &anchor_with_pointer(Some(60.0)),
            2.0,
            Some(&clipboard)
        ));
        assert!(!prefers_left_panel(
            &anchor_with_pointer(Some(340.0)),
            2.0,
            Some(&clipboard)
        ));
        assert!(!prefers_left_panel(
            &anchor_with_pointer(None),
            2.0,
            Some(&clipboard)
        ));
        assert!(!prefers_left_panel(
            &anchor_with_pointer(Some(60.0)),
            2.0,
            None
        ));
    }

    // 同一会话同一条目复用第一次的方向：指针在卡片里横向划过中线不会让面板来回飞。
    #[test]
    fn preferred_side_is_latched_per_preview_target() {
        let clipboard = PreviewClipboardWindowRect {
            x: 0,
            y: 0,
            width: 400,
            height: 600,
        };
        let session = 9_001;

        assert!(preferred_panel_side(
            session,
            "item-a",
            &anchor_with_pointer(Some(40.0)),
            1.0,
            Some(&clipboard)
        ));
        assert!(preferred_panel_side(
            session,
            "item-a",
            &anchor_with_pointer(Some(360.0)),
            1.0,
            Some(&clipboard)
        ));
        assert!(!preferred_panel_side(
            session,
            "item-b",
            &anchor_with_pointer(Some(360.0)),
            1.0,
            Some(&clipboard)
        ));
    }

    // 逻辑面板矩形换算成窗口的物理边界时带上显示器原点与缩放。
    #[test]
    fn window_bounds_convert_logical_panel_to_physical_pixels() {
        let (position, size) = preview_window_bounds(
            PreviewRect {
                left: 100.0,
                top: 50.0,
                width: 400.0,
                height: 300.0,
            },
            &PhysicalRect {
                position: PhysicalPosition::new(1920, 0),
                size: PhysicalSize::new(2560, 1440),
            },
            2.0,
        );

        assert_eq!((position.x, position.y), (2120, 100));
        assert_eq!((size.width, size.height), (800, 600));
    }

    // 同一块面板边界上的重定向复用已显示的窗口；边界一变就要重新定位。
    #[test]
    fn reuses_the_shown_window_only_for_the_same_bounds() {
        let first = (PhysicalPosition::new(0, 0), PhysicalSize::new(480, 320));
        let second = (PhysicalPosition::new(600, 0), PhysicalSize::new(480, 320));

        set_preview_shown_bounds(Some(bounds_key(first)));

        assert!(preview_window_bounds_unchanged(bounds_key(first)));
        assert!(!preview_window_bounds_unchanged(bounds_key(second)));

        set_preview_shown_bounds(None);

        assert!(!preview_window_bounds_unchanged(bounds_key(first)));
    }
}
