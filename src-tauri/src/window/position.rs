use tauri::{Monitor, PhysicalPosition, PhysicalRect, PhysicalSize, WebviewWindow};

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

/// 让按 CSS px 设计的页面在系统「文本大小」放大后仍然装得下。
///
/// 最小内容区按 DPI × 文本缩放放大并收进工作区；可调整大小的窗口不够大时撑到最小尺寸，
/// 不可调整大小的窗口直接定到该尺寸。尺寸变了就保持窗口中心不动，再推回工作区内。
/// 文本缩放为 100% 时可调整大小的窗口沿用建窗尺寸和用户拉出的尺寸；固定尺寸的窗口仍校正一次，
/// 免得文本缩放调回 100% 后一直沿用放大时存下的尺寸。macOS 没有这项设置，窗口保持原样。
pub fn fit_text_scale(window: &WebviewWindow, design: (f64, f64)) -> Result<()> {
    if cfg!(target_os = "macos") {
        return Ok(());
    }

    let text_scale = super::text_scale_factor();
    let resizable = window.is_resizable().map_err(|e| anyhow::anyhow!(e))?;
    if text_scale <= 1.0 && resizable {
        return Ok(());
    }

    let Some(monitor) = window.current_monitor().map_err(|e| anyhow::anyhow!(e))? else {
        return Ok(());
    };
    let area = monitor.work_area();
    let dpi_scale = monitor.scale_factor();
    let min_size = fit_size(
        design,
        dpi_scale * text_scale,
        area.size,
        WORK_AREA_MARGIN * dpi_scale,
    );
    // 设最小尺寸时系统会立刻从左上角把窗口撑大，所以先记下原来的位置和尺寸。
    let current = window.inner_size().map_err(|e| anyhow::anyhow!(e))?;
    let position = window.outer_position().map_err(|e| anyhow::anyhow!(e))?;
    let size = text_scaled_inner_size(current, min_size, resizable);

    window
        .set_min_size(Some(min_size.to_logical::<f64>(dpi_scale)))
        .map_err(|e| anyhow::anyhow!(e))?;
    if size == current {
        return Ok(());
    }

    window.set_size(size).map_err(|e| anyhow::anyhow!(e))?;
    window
        .set_position(recentered_position(position, current, size, area))
        .map_err(|e| anyhow::anyhow!(e))?;
    Ok(())
}

/// 可调整大小的窗口只在不够大时撑到最小尺寸，保留用户拉大的部分；不可调整的窗口定到最小尺寸，
/// 文本缩放调小后也跟着缩回。
fn text_scaled_inner_size(
    current: PhysicalSize<u32>,
    min_size: PhysicalSize<u32>,
    resizable: bool,
) -> PhysicalSize<u32> {
    if !resizable {
        return min_size;
    }

    PhysicalSize::new(
        current.width.max(min_size.width),
        current.height.max(min_size.height),
    )
}

/// 尺寸从 `current` 变到 `size` 后，让窗口中心保持不动，再整体推回工作区内。
fn recentered_position(
    position: PhysicalPosition<i32>,
    current: PhysicalSize<u32>,
    size: PhysicalSize<u32>,
    area: &PhysicalRect<i32, u32>,
) -> PhysicalPosition<i32> {
    let x = position.x - (size.width as i32 - current.width as i32) / 2;
    let y = position.y - (size.height as i32 - current.height as i32) / 2;
    let max_x = area.position.x + area.size.width as i32 - size.width as i32;
    let max_y = area.position.y + area.size.height as i32 - size.height as i32;

    PhysicalPosition::new(
        x.min(max_x).max(area.position.x),
        y.min(max_y).max(area.position.y),
    )
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

    fn work_area() -> PhysicalRect<i32, u32> {
        PhysicalRect {
            position: PhysicalPosition::new(0, 0),
            size: PhysicalSize::new(3840, 2100),
        }
    }

    // 偏好窗口 960×600 在 175% DPI、150% 文本缩放下至少要 2520×1575。
    #[test]
    fn resizable_window_grows_only_the_sides_that_are_too_small() {
        let min_size = fit_size((960.0, 600.0), 1.75 * 1.5, work_area().size, 28.0);

        assert_eq!(min_size, PhysicalSize::new(2520, 1575));
        assert_eq!(
            text_scaled_inner_size(PhysicalSize::new(1680, 1050), min_size, true),
            min_size
        );
        assert_eq!(
            text_scaled_inner_size(PhysicalSize::new(3000, 1000), min_size, true),
            PhysicalSize::new(3000, 1575)
        );
    }

    // 更新窗口不能调整大小：文本缩放调大调小都直接跟到设计尺寸。
    #[test]
    fn fixed_window_takes_the_scaled_size_exactly() {
        let min_size = fit_size((520.0, 230.0), 1.75, work_area().size, 28.0);

        assert_eq!(
            text_scaled_inner_size(PhysicalSize::new(1365, 604), min_size, false),
            PhysicalSize::new(910, 403)
        );
    }

    #[test]
    fn recentered_position_keeps_the_window_center() {
        let position = recentered_position(
            PhysicalPosition::new(1000, 600),
            PhysicalSize::new(910, 403),
            PhysicalSize::new(1365, 604),
            &work_area(),
        );

        assert_eq!(position, PhysicalPosition::new(773, 500));
    }

    #[test]
    fn recentered_position_pushes_the_window_back_into_the_work_area() {
        let near_bottom_right = recentered_position(
            PhysicalPosition::new(3200, 1800),
            PhysicalSize::new(630, 1050),
            PhysicalSize::new(945, 1575),
            &work_area(),
        );
        let larger_than_area = recentered_position(
            PhysicalPosition::new(100, 100),
            PhysicalSize::new(630, 1050),
            PhysicalSize::new(4000, 2200),
            &work_area(),
        );

        assert_eq!(near_bottom_right, PhysicalPosition::new(2895, 525));
        assert_eq!(larger_than_area, PhysicalPosition::new(0, 0));
    }
}
