use tauri::{Monitor, PhysicalPosition, PhysicalSize, WebviewWindow};

use crate::core::Result;
use crate::settings::WindowPosition;

/// 窗口与工作区边缘至少留出的逻辑像素。
const WORK_AREA_MARGIN: f64 = 16.0;

struct MonitorInfo {
    position: PhysicalPosition<i32>,
    size: PhysicalSize<u32>,
}

fn cursor_monitor(window: &WebviewWindow) -> Result<Option<(Monitor, PhysicalPosition<f64>)>> {
    let cursor = window.cursor_position().map_err(|e| anyhow::anyhow!(e))?;
    let scale = window.scale_factor().map_err(|e| anyhow::anyhow!(e))?;

    let logical = cursor.to_logical::<f64>(scale);

    let monitor = window
        .monitor_from_point(logical.x, logical.y)
        .map_err(|e| anyhow::anyhow!(e))?;

    Ok(monitor.map(|monitor| (monitor, cursor)))
}

fn monitor_from_cursor(
    window: &WebviewWindow,
) -> Result<Option<(MonitorInfo, PhysicalPosition<f64>)>> {
    let Some((monitor, cursor)) = cursor_monitor(window)? else {
        return Ok(None);
    };

    Ok(Some((
        MonitorInfo {
            position: *monitor.position(),
            size: *monitor.size(),
        },
        cursor,
    )))
}

pub fn position_window(window: &WebviewWindow, position: WindowPosition) -> Result<()> {
    let Some((monitor, cursor)) = monitor_from_cursor(window)? else {
        return Ok(());
    };

    match position {
        WindowPosition::Remember => {}
        WindowPosition::FollowCursor => apply_follow(window, &monitor, &cursor)?,
        WindowPosition::Center => apply_center(window, &monitor)?,
    }

    Ok(())
}

fn apply_follow(
    window: &WebviewWindow,
    monitor: &MonitorInfo,
    cursor: &PhysicalPosition<f64>,
) -> Result<()> {
    let win_size = window.inner_size().map_err(|e| anyhow::anyhow!(e))?;
    let mon_x = monitor.position.x as f64;
    let mon_y = monitor.position.y as f64;
    let mon_w = monitor.size.width as f64;
    let mon_h = monitor.size.height as f64;

    let x = cursor.x.min(mon_x + mon_w - win_size.width as f64);
    let y = cursor.y.min(mon_y + mon_h - win_size.height as f64);

    window
        .set_position(PhysicalPosition::new(x.round() as i32, y.round() as i32))
        .map_err(|e| anyhow::anyhow!(e))?;
    Ok(())
}

/// 将窗口居中到当前光标所在显示器。
/// 用于存档位置已失效（显示器被拔出）时的 fallback。
pub(super) fn center_on_cursor_monitor(window: &WebviewWindow) -> Result<()> {
    let Some((monitor, _)) = monitor_from_cursor(window)? else {
        return Ok(());
    };
    apply_center(window, &monitor)
}

fn apply_center(window: &WebviewWindow, monitor: &MonitorInfo) -> Result<()> {
    let win_size = window.inner_size().map_err(|e| anyhow::anyhow!(e))?;
    let mon_x = monitor.position.x as f64;
    let mon_y = monitor.position.y as f64;
    let mon_w = monitor.size.width as f64;
    let mon_h = monitor.size.height as f64;

    let x = mon_x + (mon_w - win_size.width as f64) / 2.0;
    let y = mon_y + (mon_h - win_size.height as f64) / 2.0;

    window
        .set_position(PhysicalPosition::new(x.round() as i32, y.round() as i32))
        .map_err(|e| anyhow::anyhow!(e))?;
    Ok(())
}

/// 按页面设计尺寸（CSS px）给窗口定尺寸，并居中到光标所在显示器的工作区。
///
/// WebView2 把系统「文本大小」当整页缩放叠加在 DPI 上，窗口只按 DPI 放大时页面拿到的
/// CSS 视口会被等比压缩，所以这里把文本缩放一并乘进去。工作区放不下时收窄到工作区内，
/// 剩下的交给页面自己滚动。
pub fn fit_to_cursor_monitor(window: &WebviewWindow, design: (f64, f64)) -> Result<()> {
    let Some((monitor, _)) = cursor_monitor(window)? else {
        return Ok(());
    };
    let area = monitor.work_area();
    let dpi_scale = monitor.scale_factor();
    let size = fit_size(
        design,
        dpi_scale * super::text_scale_factor(),
        area.size,
        WORK_AREA_MARGIN * dpi_scale,
    );
    let position = PhysicalPosition::new(
        area.position.x + (area.size.width as i32 - size.width as i32) / 2,
        area.position.y + (area.size.height as i32 - size.height as i32) / 2,
    );

    // 跨 DPI 显示器移动会触发 WM_DPICHANGED 按旧逻辑尺寸重排窗口，所以先移过去再定尺寸。
    window
        .set_position(position)
        .map_err(|e| anyhow::anyhow!(e))?;
    window.set_size(size).map_err(|e| anyhow::anyhow!(e))?;

    // Windows 无边框窗口的外框仍带一圈不可见的缩放边，`set_position` 落的是外框，
    // 按内容区相对外框的偏移校正后，可见部分才真正居中。
    let outer = window.outer_position().map_err(|e| anyhow::anyhow!(e))?;
    let inner = window.inner_position().map_err(|e| anyhow::anyhow!(e))?;
    window
        .set_position(PhysicalPosition::new(
            position.x - (inner.x - outer.x),
            position.y - (inner.y - outer.y),
        ))
        .map_err(|e| anyhow::anyhow!(e))?;
    Ok(())
}

/// 设计尺寸乘缩放后，收进四周各留 `margin` 物理像素的工作区。
fn fit_size(
    design: (f64, f64),
    scale: f64,
    area: PhysicalSize<u32>,
    margin: f64,
) -> PhysicalSize<u32> {
    let max_width = (area.width as f64 - margin * 2.0).max(1.0);
    let max_height = (area.height as f64 - margin * 2.0).max(1.0);

    PhysicalSize::new(
        (design.0 * scale).min(max_width).round() as u32,
        (design.1 * scale).min(max_height).round() as u32,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_size_keeps_design_size_when_it_fits() {
        let size = fit_size((900.0, 600.0), 1.5, PhysicalSize::new(2560, 1392), 24.0);

        assert_eq!(size, PhysicalSize::new(1350, 900));
    }

    #[test]
    fn fit_size_applies_text_scale_on_top_of_dpi() {
        let size = fit_size(
            (900.0, 600.0),
            1.25 * 1.5,
            PhysicalSize::new(3840, 2100),
            20.0,
        );

        assert_eq!(size, PhysicalSize::new(1688, 1125));
    }

    #[test]
    fn fit_size_clamps_to_work_area_minus_margin() {
        let size = fit_size(
            (900.0, 600.0),
            1.5 * 1.5,
            PhysicalSize::new(1920, 1032),
            24.0,
        );

        assert_eq!(size, PhysicalSize::new(1872, 984));
    }

    #[test]
    fn fit_size_never_collapses_to_zero() {
        let size = fit_size((900.0, 600.0), 1.0, PhysicalSize::new(10, 10), 16.0);

        assert_eq!(size, PhysicalSize::new(1, 1));
    }
}
