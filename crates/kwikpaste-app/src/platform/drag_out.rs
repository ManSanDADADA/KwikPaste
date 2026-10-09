//! 拖出的 GPUI 一侧（附录 C §5）：把记录拖到别的应用。OLE 细节在 `kwikpaste_os::win::drag_out`。
//!
//! # UI 怎么接
//! 1. 卡片 `on_mouse_down(MouseButton::Left, …)` 里 [`DragTracker::press`]（记条目 id 和位置）。
//! 2. 列表根元素 `on_mouse_move` 里 [`DragTracker::moved`]：左键按住且移动超过系统拖拽阈值时返回
//!    条目 id，接着调用 [`start_item`]；`on_mouse_up` 里 [`DragTracker::release`]。不要用 GPUI 的
//!    `on_drag`（会留下 `cx.active_drag`）。
//! 3. 卡片的单击粘贴 / 复制放在点击（松开且没有拖出）时执行，不要放在按下时：否则一次拖出会先
//!    粘贴、松开时再投放一次。
//! 4. [`start_item`] 返回的任务在拖出结束后给出 [`DragReport`]（`Dropped` / `Refused` / `Cancelled`）；
//!    失败（文件已不存在等）是带用户文案的错误，可以直接做 toast。UI 不需要做清理：结束后这里已经
//!    给面板派发了一条窗口外坐标的左键 `MouseUp`，清掉所有元素残留的按下状态（否则右键抬起会凑成
//!    一次幽灵点击）。
//!
//! 拖出期间：GPUI 的前台任务全部冻结（Windows 的 ole32 模态循环；macOS 的 AppKit tracking
//! loop），渲染照常；Windows 的 Esc 由键盘钩子转成取消，macOS 由 AppKit 处理；
//! 窗外点击不隐藏面板；松开在面板自己上时过滤层回 NONE，GPUI 收不到 `MouseUp` / `FileDrop`。
//! 全程不激活面板（`WM_ACTIVATE` 为 0）。
//!
//! macOS 走 AppKit `beginDraggingSession`；调用必须保留在 `on_mouse_move` 同步路径的命名白名单
//! 函数里，结束回调负责清掉活动标志，下一轮 GPUI 任务再派发窗口外的 `MouseUp`。

use std::path::PathBuf;
use std::sync::Mutex;

use anyhow::{Context as _, anyhow};
use gpui::{
    AnyWindowHandle, App, AsyncApp, Modifiers, MouseButton, MouseMoveEvent, MouseUpEvent, Pixels,
    PlatformInput, Point, Task, Window, point, px,
};
use kwikpaste_core::Core;
use kwikpaste_core::db::models::{ClipboardItem, ClipboardKind, ClipboardSubKind};
use kwikpaste_core::i18n::commands::{Key, label};
#[allow(unused_imports, reason = "UI 接线用的接口，见本模块文档")]
pub use kwikpaste_os::drag_out::{DragData, DragReport, DragResult};

use super::probe;
use crate::core_host;

/// 检测「按住并拖动」：记下按下的位置，移动超过系统拖拽阈值时交出条目 id（每次按下只交一次）。
#[derive(Debug, Default)]
pub struct DragTracker {
    pressed: Option<(String, Point<Pixels>)>,
}

impl DragTracker {
    /// 在条目上按下左键。
    pub fn press(&mut self, item_id: impl Into<String>, position: Point<Pixels>) {
        self.pressed = Some((item_id.into(), position));
    }

    /// 鼠标移动：左键仍按着且超过阈值时返回要拖出的条目 id。
    pub fn moved(&mut self, event: &MouseMoveEvent, window: &Window) -> Option<String> {
        if event.pressed_button != Some(MouseButton::Left) {
            self.pressed = None;
            return None;
        }
        let (_, origin) = self.pressed.as_ref()?;
        let (threshold_x, threshold_y) = threshold(window);
        let delta = event.position - *origin;
        if delta.x.abs() < threshold_x && delta.y.abs() < threshold_y {
            return None;
        }

        self.pressed.take().map(|(id, _)| id)
    }

    /// 松开左键。
    pub fn release(&mut self) {
        self.pressed = None;
    }

    /// 判断是否越过系统拖拽阈值但不消费按下状态，供面板内排序手势先行判定。
    pub fn crossed_threshold(&self, event: &MouseMoveEvent, window: &Window) -> bool {
        if event.pressed_button != Some(MouseButton::Left) {
            return false;
        }
        let Some((_, origin)) = self.pressed.as_ref() else {
            return false;
        };
        let (threshold_x, threshold_y) = threshold(window);
        let delta = event.position - *origin;
        delta.x.abs() >= threshold_x || delta.y.abs() >= threshold_y
    }
}

/// 系统的拖拽阈值（逻辑像素）：Windows `SM_CXDRAG` / `SM_CYDRAG` 按窗口缩放换算，macOS 3 pt。
fn threshold(window: &Window) -> (Pixels, Pixels) {
    #[cfg(target_os = "windows")]
    {
        let (x, y) = kwikpaste_os::win::drag_out::threshold();
        let scale = window.scale_factor().max(1.0);
        (px(x as f32 / scale), px(y as f32 / scale))
    }
    #[cfg(target_os = "macos")]
    {
        let _ = window;
        (px(3.0), px(3.0))
    }
}

#[allow(dead_code, reason = "UI 接线用的接口，见本模块文档")]
/// 拖出一条记录：按记录类型取载荷（文本带 HTML / RTF，图片拖原图文件，文件拖文件）和预览图。
pub fn start_item(
    item_id: String,
    window: &Window,
    cx: &mut App,
) -> Task<anyhow::Result<DragReport>> {
    let target = DragTarget::of(window);
    let core = core_host::core(cx).cloned();
    cx.spawn(async move |cx: &mut AsyncApp| {
        let core = core.context("the core is not running")?;
        let item = core
            .find_item(&item_id)
            .await?
            .with_context(|| format!("item {item_id} no longer exists"))?;
        let data = payload(&item, &core)?;
        let preview = preview(&item, &data, &core).await;
        run(target?, data, preview, cx).await
    })
}

/// 拖出给定的内容（自测与以后的预览窗用）。`preview_png` 是跟随光标的预览图。
pub fn start(
    data: DragData,
    preview_png: Option<Vec<u8>>,
    window: &Window,
    cx: &mut App,
) -> Task<anyhow::Result<DragReport>> {
    let target = DragTarget::of(window);
    #[cfg(target_os = "macos")]
    {
        let target = match target {
            Ok(target) => target,
            Err(error) => return Task::ready(Err(error)),
        };
        let started = kwikpaste_os::clock::now_ticks();
        probe::drag_started(&data);
        let phase = super::enter_phase(crate::health::Phase::DragOut);
        let (sender, receiver) = async_channel::bounded(1);
        if let Err(error) = unsafe {
            kwikpaste_os::mac::drag_out::begin_session_sync(
                target.native,
                &data,
                preview_png.as_deref(),
                move |report| {
                    let _ = sender.try_send(report);
                },
            )
        } {
            drop(phase);
            log::error!("drag-out failed: {error:#}");
            return Task::ready(Err(error.into()));
        }
        cx.spawn(async move |cx| finish_mac_drag(target.handle, receiver, started, phase, cx).await)
    }
    #[cfg(not(target_os = "macos"))]
    {
        cx.spawn(async move |cx: &mut AsyncApp| run(target?, data, preview_png, cx).await)
    }
}

/// 拖出源窗口：GPUI 句柄（拖后清理用）和原生句柄。
struct DragTarget {
    handle: AnyWindowHandle,
    #[cfg_attr(target_os = "macos", allow(dead_code))]
    native: isize,
}

impl DragTarget {
    fn of(window: &Window) -> anyhow::Result<Self> {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};

        let handle = HasWindowHandle::window_handle(window)
            .map_err(|err| anyhow!("drag source window handle: {err:?}"))?;
        let native = match handle.as_raw() {
            #[cfg(target_os = "windows")]
            RawWindowHandle::Win32(handle) => handle.hwnd.get(),
            #[cfg(target_os = "macos")]
            RawWindowHandle::AppKit(handle) => handle.ns_view.as_ptr() as isize,
            _ => return Err(anyhow!("unsupported drag source window")),
        };

        Ok(Self {
            handle: window.window_handle(),
            native,
        })
    }
}

/// 在 `cx.spawn` 的任务体里、不持有任何 `update` 借用时调用 `DoDragDrop`，结束后清理。
async fn run(
    target: DragTarget,
    data: DragData,
    preview_png: Option<Vec<u8>>,
    cx: &mut AsyncApp,
) -> anyhow::Result<DragReport> {
    let started = kwikpaste_os::clock::now_ticks();
    probe::drag_started(&data);
    let phase = super::enter_phase(crate::health::Phase::DragOut);

    #[cfg(target_os = "windows")]
    let report = kwikpaste_os::win::drag_out::run(target.native, &data, preview_png.as_deref())
        .context("drag-out failed");
    #[cfg(target_os = "macos")]
    let report: anyhow::Result<DragReport> = {
        let (sender, receiver) = async_channel::bounded(1);
        let begin = cx.update(|_| unsafe {
            kwikpaste_os::mac::drag_out::begin_session_sync(
                target.native,
                &data,
                preview_png.as_deref(),
                move |report| {
                    let _ = sender.try_send(report);
                },
            )
        });
        begin?;
        Ok(receiver
            .recv()
            .await
            .context("macOS drag session ended without a result")?)
    };

    let returned = kwikpaste_os::clock::now_ticks();
    drop(phase);
    clear_pending_mouse_down(target.handle, cx);
    match &report {
        Ok(report) => {
            log::debug!("drag-out finished: {report:?}");
            probe::drag_finished(report, started, returned);
        }
        Err(err) => log::error!("{err:#}"),
    }

    report
}

#[cfg(target_os = "macos")]
async fn finish_mac_drag(
    handle: AnyWindowHandle,
    receiver: async_channel::Receiver<DragReport>,
    started: i64,
    phase: super::PhaseGuard,
    cx: &mut AsyncApp,
) -> anyhow::Result<DragReport> {
    let report = receiver
        .recv()
        .await
        .context("macOS drag session ended without a result")?;
    let returned = kwikpaste_os::clock::now_ticks();
    drop(phase);
    clear_pending_mouse_down(handle, cx);
    probe::drag_finished(&report, started, returned);
    log::debug!("drag-out finished: {report:?}");
    Ok(report)
}

/// 拖拽期间真实的左键抬起被 ole32 吃掉，GPUI 元素里还留着按下状态；派发一条窗口外坐标的抬起，
/// 它们只清掉状态、不触发点击。
fn clear_pending_mouse_down(handle: AnyWindowHandle, cx: &mut AsyncApp) {
    let cleared = handle.update(cx, |_, window, cx| {
        window.dispatch_event(
            PlatformInput::MouseUp(MouseUpEvent {
                button: MouseButton::Left,
                position: point(px(-10000.0), px(-10000.0)),
                modifiers: Modifiers::default(),
                click_count: 1,
            }),
            cx,
        );
    });
    if let Err(err) = cleared {
        log::warn!("drag-out cleanup could not reach the window: {err:#}");
    }
}

/// 记录 → 拖出载荷（与 1.x `resolve_drag_payload` 相同）。HTML / RTF 记录的 `content` 是富格式源，
/// `search_text` 才是系统给的纯文本；其余文本记录 `content` 就是纯文本。
fn payload(item: &ClipboardItem, core: &Core) -> anyhow::Result<DragData> {
    let language = core.language();
    match item.kind {
        ClipboardKind::Files => {
            let paths: Vec<PathBuf> = item
                .content
                .split('\n')
                .filter(|line| !line.is_empty())
                .map(PathBuf::from)
                .filter(|path| path.exists())
                .collect();
            if paths.is_empty() {
                return Err(anyhow!(label(language, Key::DragSourceFilesMissing)));
            }
            Ok(DragData::Files(paths))
        }
        ClipboardKind::Image => {
            let path = core.image_store().origin_path(&item.content);
            if !path.exists() {
                return Err(anyhow!(label(language, Key::DragImageMissing)));
            }
            Ok(DragData::Files(vec![path]))
        }
        ClipboardKind::Text => {
            if item.content.is_empty() {
                return Err(anyhow!(label(language, Key::DragTextEmpty)));
            }
            let plain = || {
                item.search_text
                    .clone()
                    .unwrap_or_else(|| item.content.clone())
            };
            Ok(match item.sub_kind {
                Some(ClipboardSubKind::Html) => DragData::Text {
                    plain: plain(),
                    html: Some(item.content.clone()),
                    rtf: None,
                },
                Some(ClipboardSubKind::Rtf) => DragData::Text {
                    plain: plain(),
                    html: None,
                    rtf: Some(item.content.clone()),
                },
                _ => DragData::Text {
                    plain: item.content.clone(),
                    html: None,
                    rtf: None,
                },
            })
        }
    }
}

/// 预览图只读已有的缓存文件，不现场解码：图片用缩略图，文件用第一个路径的类型图标；文本没有
/// （与 1.x 的 Windows 版相同）。
async fn preview(item: &ClipboardItem, data: &DragData, core: &Core) -> Option<Vec<u8>> {
    let DragData::Files(paths) = data else {
        return None;
    };
    if item.kind == ClipboardKind::Image
        && let Ok(bytes) = std::fs::read(core.image_store().thumbnail_path(&item.content))
    {
        return Some(bytes);
    }
    let first = paths.first()?.to_str()?;
    let icon = core
        .file_icon(first, item.file_types.as_deref(), 0)
        .await
        .ok()?;

    std::fs::read(icon.icon_path?).ok()
}

/// 平台自测的保护：只许投放到本进程和 `KWIKPASTE_DRAG_ALLOW`（逗号分隔的进程 id，探针脚本自己）
/// 的窗口上，拖拽超过 15 s 自动取消，测试拖拽绝不会落进用户的应用。
pub fn guard_selftest_drops() {
    #[cfg(target_os = "windows")]
    {
        let mut allowed: Vec<u32> = std::env::var("KWIKPASTE_DRAG_ALLOW")
            .unwrap_or_default()
            .split(',')
            .filter_map(|pid| pid.trim().parse().ok())
            .collect();
        allowed.push(std::process::id());
        kwikpaste_os::win::drag_out::set_drop_guard(
            move |pid| allowed.contains(&pid),
            std::time::Duration::from_secs(15),
        );
    }
}

/// 自测的拖出内容（`--selftest-drag-payload=<JSON>`）：平台自测视图在按住拖动时拖它。
static SELFTEST_PAYLOAD: Mutex<Option<DragData>> = Mutex::new(None);

/// 解析 `{"plain": …, "html": …, "rtf": …}` 或 `{"files": […]}`。
pub fn set_selftest_payload(json: &str) -> anyhow::Result<()> {
    #[derive(serde::Deserialize)]
    struct Payload {
        plain: Option<String>,
        html: Option<String>,
        rtf: Option<String>,
        files: Option<Vec<PathBuf>>,
    }

    let payload: Payload = serde_json::from_str(json).context("drag payload is not JSON")?;
    let data = match (payload.files, payload.plain) {
        (Some(files), _) => DragData::Files(files),
        (None, Some(plain)) => DragData::Text {
            plain,
            html: payload.html,
            rtf: payload.rtf,
        },
        (None, None) => return Err(anyhow!("drag payload needs `plain` or `files`")),
    };
    *SELFTEST_PAYLOAD
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(data);

    Ok(())
}

pub fn selftest_payload() -> Option<DragData> {
    SELFTEST_PAYLOAD
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selftest_payload_parses_text_and_files() {
        set_selftest_payload(r#"{"plain":"a","html":"<b>a</b>"}"#).expect("text");
        assert_eq!(
            selftest_payload(),
            Some(DragData::Text {
                plain: "a".to_owned(),
                html: Some("<b>a</b>".to_owned()),
                rtf: None
            })
        );
        set_selftest_payload(r#"{"files":["C:\\x.png"]}"#).expect("files");
        assert_eq!(
            selftest_payload(),
            Some(DragData::Files(vec![PathBuf::from(r"C:\x.png")]))
        );
        assert!(set_selftest_payload("{}").is_err());
    }
}
