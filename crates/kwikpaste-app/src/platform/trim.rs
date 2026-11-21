//! 常驻闲置时收缩工作集（Windows）。
//!
//! 面板和其它窗口都关着时进程什么也不画，专用工作集里多半是启动、渲染时碰过之后就不再用的页
//! （GPU 驱动、字体、堆）。面板隐藏、窗口关闭、启动完成后各等 [`IDLE_DELAY`]，期间没有
//! 窗口重新出现就把工作集交还给系统。下次呼出缺的都是软缺页（页还在内存里），实测首帧多约 1.5 ms。

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use gpui::{App, AsyncApp};

use super::panel::{Panel, PanelEvent};

/// 最后一个窗口消失后再等这么久：连续呼出（看一眼、关掉、马上再开）不受影响。
const IDLE_DELAY: Duration = Duration::from_secs(30);

/// 每次排期加一；到点时不是最新的一次就作废，等于顺延。
static GENERATION: AtomicU64 = AtomicU64::new(0);

/// 启动完成后调用一次：先为启动本身排一次，之后跟着面板隐藏和窗口关闭排期。
pub fn serve(cx: &mut App) {
    let events = cx.global::<Panel>().events().clone();
    cx.subscribe(&events, |_, event: &PanelEvent, cx| {
        if matches!(event, PanelEvent::Hidden) {
            schedule(cx);
        }
    })
    .detach();
    cx.on_window_closed(|cx, _| schedule(cx)).detach();
    schedule(cx);
}

fn schedule(cx: &mut App) {
    let generation = GENERATION.fetch_add(1, Ordering::Relaxed) + 1;
    cx.spawn(async move |cx: &mut AsyncApp| {
        cx.background_executor().timer(IDLE_DELAY).await;
        if GENERATION.load(Ordering::Relaxed) != generation
            || super::panel::is_shown()
            || super::updater::any_app_window_visible()
        {
            return;
        }
        if let Err(err) = kwikpaste_os::win::trim_working_set() {
            log::warn!("could not trim the working set: {err}");
        }
    })
    .detach();
}
