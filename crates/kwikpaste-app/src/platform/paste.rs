//! 粘贴链路的宿主一侧：UI 的列表发出意图，这里调 core 写回剪贴板、让出前台、注入粘贴键。
//!
//! # UI 怎么接
//! - **粘贴**（列表 `ListIntent::Paste`、Enter；Mod+Enter 是纯文本粘贴；窗口内 Ctrl/⌘+数字粘贴第 N 张
//!   可见卡片）：[`paste`]`(cx, id, plain, keep_visible)`。流程：`Core::prepare_paste` 按「粘贴时去除格式」
//!   「粘贴文件为路径」写回剪贴板并记一次使用 → 让出前台（编辑态先把前台还给进入前的窗口；
//!   `keep_visible` 为假时隐藏面板，固定面板时传真）→ 面板原先可见就等 50 ms → 注入粘贴键
//!   （Windows Ctrl+V，WindTerm 等少数终端 Shift+Insert；macOS ⌘V）。
//!   列表经 `clipboard::view::host::PlatformHost` 调到这里（`keep_visible` 先传假）；粘贴的就是列表给的 id，「当前项是不是刚复制的那条」由列表负责。
//! - **粘贴片段**（快捷信息、拆词选区）：[`paste_fragment`]，流程同上；只复制不粘贴用 [`copy_fragment`]。
//! - **复制**（不粘贴）：[`copy`]。`Core::copy_item` 写回剪贴板；固定面板（`keep_visible`）完成复制后
//!   释放导航键，设置「复制后隐藏窗口」打开且 `keep_visible` 为假时隐藏面板。
//! - **全局快速粘贴**（设置 `shortcuts.quickPaste` 的修饰键 + 1…9、0）由热键模块直接调用
//!   [`quick_paste`]，UI 不用管：`Core::prepare_quick_paste` → 等修饰键全部松开（最多 2 s；超时就把内容
//!   留在剪贴板上、不粘贴）→ 面板可见时让出前台并等 50 ms → 注入 → 丢弃凭据。
//! - **全局纯文本粘贴**（设置 `shortcuts.pastePlain`）读取当前系统剪贴板，必要时通过回环抑制写成纯文本，再等待修饰键释放后注入粘贴键。
//!
//! 返回的 `Task` 可以 await 拿到 core 的错误（`AppError` 的消息是用户可读的根因，给 toast 用），
//! 不关心就 `detach_and_log_err`（`gpui::TaskExt`）。粘贴键落到当前前台窗口：面板非编辑态从不激活，
//! 前台一直是目标应用。

use std::time::{Duration, Instant};

use gpui::{App, AsyncApp, Task};
use kwikpaste_core::clipboard::ClipboardFragment;
use kwikpaste_core::db::models::ClipboardItemQuery;
use kwikpaste_core::ops::CopyOutcome;
use kwikpaste_core::{AppError, Core, Result};
use kwikpaste_os::keystroke;

use super::panel::{PanelCommand, Trigger, TriggerSource};
use super::probe;
use crate::core_host;

/// 面板隐藏、还前台都要等系统处理完；不等这一拍，粘贴键可能赶在前台切回去之前到达。
const SETTLE_DELAY: Duration = Duration::from_millis(50);
/// 快速粘贴等用户松开修饰键的上限；超时说明还按着键，这时注入会被目标应用读成别的组合键。
const MODIFIER_RELEASE_TIMEOUT: Duration = Duration::from_secs(2);
const MODIFIER_POLL_INTERVAL: Duration = Duration::from_millis(15);

/// 粘贴一条记录：写回剪贴板、让出前台、注入粘贴键。
pub fn paste(cx: &mut App, id: String, plain: bool, keep_visible: bool) -> Task<Result<()>> {
    paste_selection(cx, id, plain, None, keep_visible)
}

/// 多选两条以上时合并成一段文本或一份文件列表再粘贴，否则粘贴 `id`；整个操作只注入一次粘贴键。
pub fn paste_selection(
    cx: &mut App,
    id: String,
    plain: bool,
    selection: Option<(Vec<String>, ClipboardItemQuery)>,
    keep_visible: bool,
) -> Task<Result<()>> {
    let Some(core) = core_host::core(cx).cloned() else {
        return Task::ready(Err(core_missing()));
    };

    cx.spawn(async move |cx: &mut AsyncApp| {
        let _phase = super::enter_phase(crate::health::Phase::Paste);
        let started = Instant::now();
        let merged = if let Some((ids, query)) = selection {
            let files_as_paths = plain || core.settings().clipboard.content.paste_files_as_path;
            core.copy_selection(ids, query, files_as_paths)
                .await?
                .is_some()
        } else {
            false
        };
        if !merged {
            core.prepare_paste(&id, plain).await?;
        }
        let report = yield_and_inject(cx, keep_visible).await?;
        probe::pasted("item", &id, plain, report, started.elapsed());
        Ok(())
    })
}

/// 把图片原文件写成文件列表，复用普通粘贴的前台交接与按键注入。
pub fn paste_image_as_file(cx: &mut App, id: String, keep_visible: bool) -> Task<Result<()>> {
    let Some(core) = core_host::core(cx).cloned() else {
        return Task::ready(Err(core_missing()));
    };

    cx.spawn(async move |cx: &mut AsyncApp| {
        let _phase = super::enter_phase(crate::health::Phase::Paste);
        let started = Instant::now();
        core.prepare_paste_image_as_file(&id).await?;
        let report = yield_and_inject(cx, keep_visible).await?;
        probe::pasted("image-file", &id, false, report, started.elapsed());
        Ok(())
    })
}

/// 粘贴一条记录里的片段，流程同 [`paste`]。
pub fn paste_fragment(
    cx: &mut App,
    id: String,
    fragment: ClipboardFragment,
    keep_visible: bool,
) -> Task<Result<()>> {
    let Some(core) = core_host::core(cx).cloned() else {
        return Task::ready(Err(core_missing()));
    };

    cx.spawn(async move |cx: &mut AsyncApp| {
        let _phase = super::enter_phase(crate::health::Phase::Paste);
        let started = Instant::now();
        core.prepare_paste_fragment(&id, fragment).await?;
        let report = yield_and_inject(cx, keep_visible).await?;
        probe::pasted("fragment", &id, false, report, started.elapsed());
        Ok(())
    })
}

/// 把记录写回剪贴板（不粘贴）；设置要求复制后隐藏且面板没固定时隐藏面板。
pub fn copy(
    cx: &mut App,
    id: String,
    plain: bool,
    keep_visible: bool,
) -> Task<Result<CopyOutcome>> {
    copy_selection(cx, id, plain, None, keep_visible)
}

/// 多选两条以上时合并后复制（同 [`paste_selection`]），否则复制 `id`；隐藏规则同 [`copy`]。
pub fn copy_selection(
    cx: &mut App,
    id: String,
    plain: bool,
    selection: Option<(Vec<String>, ClipboardItemQuery)>,
    keep_visible: bool,
) -> Task<Result<CopyOutcome>> {
    let Some(core) = core_host::core(cx).cloned() else {
        return Task::ready(Err(core_missing()));
    };

    cx.spawn(async move |cx: &mut AsyncApp| {
        let merged = if let Some((ids, query)) = selection {
            core.copy_selection(ids, query, plain).await?
        } else {
            None
        };
        let outcome = match merged {
            Some(outcome) => outcome,
            None => core.copy_item(&id, plain).await?,
        };
        if keep_visible {
            cx.update(|cx| {
                super::request(cx, PanelCommand::SetInputCapture(false));
            });
        } else if outcome.hide_window {
            cx.update(|cx| {
                super::request(cx, PanelCommand::Hide(Trigger::now(TriggerSource::Copy)))
            });
        }
        probe::copied(&id, plain, outcome.hide_window);
        Ok(outcome)
    })
}

/// 把图片识别出的文字写回剪贴板（不粘贴），隐藏规则同 [`copy`]。
pub fn copy_image_text(cx: &mut App, id: String, keep_visible: bool) -> Task<Result<CopyOutcome>> {
    let Some(core) = core_host::core(cx).cloned() else {
        return Task::ready(Err(core_missing()));
    };
    cx.spawn(async move |cx: &mut AsyncApp| {
        let outcome = core.copy_image_text(&id).await?;
        if keep_visible {
            cx.update(|cx| {
                super::request(cx, PanelCommand::SetInputCapture(false));
            });
        } else if outcome.hide_window {
            cx.update(|cx| {
                super::request(cx, PanelCommand::Hide(Trigger::now(TriggerSource::Copy)));
            });
        }
        probe::copied(&id, false, outcome.hide_window);
        Ok(outcome)
    })
}

/// 把记录里的片段（快捷信息、拆词选区）写回剪贴板（不粘贴），隐藏规则同 [`copy`]。
pub fn copy_fragment(
    cx: &mut App,
    id: String,
    fragment: ClipboardFragment,
    keep_visible: bool,
) -> Task<Result<CopyOutcome>> {
    let Some(core) = core_host::core(cx).cloned() else {
        return Task::ready(Err(core_missing()));
    };

    cx.spawn(async move |cx: &mut AsyncApp| {
        let outcome = core.copy_fragment(&id, fragment).await?;
        if keep_visible {
            cx.update(|cx| {
                super::request(cx, PanelCommand::SetInputCapture(false));
            });
        } else if outcome.hide_window {
            cx.update(|cx| {
                super::request(cx, PanelCommand::Hide(Trigger::now(TriggerSource::Copy)))
            });
        }
        probe::copied(&id, false, outcome.hide_window);
        Ok(outcome)
    })
}

/// 全局快速粘贴「全部」视图里第 `offset` 条（从 0 起）。热键线程在按下时已屏蔽 Alt / Win 的单独松开。
/// 历史不足、上一次快速粘贴还没结束时什么都不做；失败只记日志。
pub fn quick_paste(cx: &mut App, offset: i64) -> Task<()> {
    let Some(core) = core_host::core(cx).cloned() else {
        return Task::ready(());
    };

    cx.spawn(async move |cx: &mut AsyncApp| {
        if let Err(err) = run_quick_paste(&core, offset, cx).await {
            log::warn!("quick paste of item {} failed: {err}", offset + 1);
        }
    })
}

/// 全局粘贴当前剪贴板的纯文本表示；没有可用文本时不注入按键。
pub fn paste_plain(cx: &mut App) -> Task<()> {
    let Some(core) = core_host::core(cx).cloned() else {
        return Task::ready(());
    };

    cx.spawn(async move |cx: &mut AsyncApp| {
        if let Err(err) = run_plain_paste(&core, cx).await {
            log::warn!("plain paste failed: {err}");
        }
    })
}

async fn run_quick_paste(core: &Core, offset: i64, cx: &mut AsyncApp) -> Result<()> {
    let _phase = super::enter_phase(crate::health::Phase::Paste);
    let started = Instant::now();
    let Some(ticket) = core.prepare_quick_paste(offset).await? else {
        return Ok(());
    };
    if !wait_for_modifiers_released(cx).await {
        log::warn!(
            "quick paste left item {} on the clipboard: modifier keys are still held",
            ticket.item_id
        );
        return Ok(());
    }

    let report = yield_and_inject(cx, false).await?;
    probe::pasted("quick", &ticket.item_id, false, report, started.elapsed());
    drop(ticket);
    Ok(())
}

async fn run_plain_paste(core: &Core, cx: &mut AsyncApp) -> Result<()> {
    let _phase = super::enter_phase(crate::health::Phase::Paste);
    let started = Instant::now();
    let Some(ticket) = core.prepare_plain_paste_from_clipboard().await? else {
        log::debug!("plain paste skipped: current clipboard has no text or files");
        return Ok(());
    };
    if !wait_for_modifiers_released(cx).await {
        log::warn!("plain paste left plain text on the clipboard: modifier keys are still held");
        return Ok(());
    }

    let report = yield_and_inject(cx, false).await?;
    probe::pasted("plain", &ticket.item_id, true, report, started.elapsed());
    drop(ticket);
    Ok(())
}

/// 一次注入的经过，供探针记录。
#[derive(Debug, Clone, Copy)]
pub struct InjectReport {
    /// 让出前台之前面板是否可见。
    pub panel_was_visible: bool,
    /// 注入时的前台窗口（Windows 句柄；macOS 为 0）。
    pub foreground: isize,
}

/// 让出前台（面板可见时），等一拍，注入粘贴键。
async fn yield_and_inject(cx: &mut AsyncApp, keep_visible: bool) -> Result<InjectReport> {
    let (done, yielded) = async_channel::bounded(1);
    cx.update(|cx| {
        super::request(cx, PanelCommand::YieldForPaste { keep_visible, done });
    });
    // 面板命令循环没在运行时 sender 已被丢弃，按「不可见」处理。
    let panel_was_visible = yielded.recv().await.unwrap_or(false);
    if panel_was_visible {
        cx.background_executor().timer(SETTLE_DELAY).await;
    }

    kwikpaste_os::keystroke::ensure_accessibility_trusted()
        .map_err(|err| AppError::Other(err.into()))?;
    let foreground = foreground_window();
    keystroke::simulate_paste().map_err(|err| AppError::Other(err.into()))?;
    Ok(InjectReport {
        panel_was_visible,
        foreground,
    })
}

/// 等用户松开全部修饰键，最多等 [`MODIFIER_RELEASE_TIMEOUT`]；返回是否已全部松开。
async fn wait_for_modifiers_released(cx: &mut AsyncApp) -> bool {
    let deadline = Instant::now() + MODIFIER_RELEASE_TIMEOUT;
    while keystroke::modifiers_pressed() {
        if Instant::now() >= deadline {
            return false;
        }
        cx.background_executor().timer(MODIFIER_POLL_INTERVAL).await;
    }

    true
}

/// 粘贴键被系统拒收（`SendInput` 返回拒绝访问）。前台权限并不更高时也会出现，例如游戏反作弊驱动
/// 把本进程拉黑；之后本进程的注入一直失败，换一个进程（重启）就恢复。
pub fn injection_blocked(err: &anyhow::Error) -> bool {
    cfg!(target_os = "windows")
        && err
            .downcast_ref::<std::io::Error>()
            .is_some_and(|err| err.kind() == std::io::ErrorKind::PermissionDenied)
}

fn foreground_window() -> isize {
    #[cfg(target_os = "windows")]
    {
        kwikpaste_os::win::foreground_window()
    }
    #[cfg(target_os = "macos")]
    {
        0
    }
}

fn core_missing() -> AppError {
    AppError::Other(anyhow::anyhow!("kwikpaste-core is not running"))
}
