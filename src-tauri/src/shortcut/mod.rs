//! 全局快捷键：注册由 Rust 主导，前端在偏好设置里改完通过 `update_settings` 触发重注册。
//!
//! 配置来自 `settings::Shortcuts`：打开窗口的两个快捷键，以及开启后的快速粘贴
//! 「修饰键 + 数字」。本模块只负责 OS 级注册——`paste_plain` 是窗口内交互
//! （前端 `useKeyPress`），不在这里处理。

use std::sync::Mutex;
use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutEvent, ShortcutState};

use crate::core::{AppError, Result};
use crate::settings::{SettingsStore, Shortcuts};
use crate::window::{self, CLIPBOARD_WINDOW_LABEL, PREFERENCE_WINDOW_LABEL};

#[cfg(target_os = "windows")]
mod win_v;

pub const CONFLICT_EVENT: &str = "shortcut://conflict";
const RESUME_DEBOUNCE: Duration = Duration::from_millis(160);
const QUICK_PASTE_ACTION: &str = "quick_paste";

/// 快速粘贴的数字键与历史序号（从 0 起）：1–9 对应前 9 条，0 对应第 10 条，
/// 与剪贴板窗口里 ⌘ / Ctrl + 数字的提示一致。
const QUICK_PASTE_KEYS: [(&str, i64); 10] = [
    ("1", 0),
    ("2", 1),
    ("3", 2),
    ("4", 3),
    ("5", 4),
    ("6", 5),
    ("7", 6),
    ("8", 7),
    ("9", 8),
    ("0", 9),
];

#[derive(Debug, Clone, Serialize)]
pub struct ShortcutConflict {
    pub action: &'static str,
    pub binding: String,
    pub reason: String,
}

#[derive(Default)]
pub struct ShortcutManager {
    active: Mutex<Vec<(&'static str, Shortcut)>>,
    pause: Mutex<ShortcutPause>,
}

#[derive(Default)]
struct ShortcutPause {
    resume_epoch: u64,
    suspend_count: usize,
}

impl ShortcutPause {
    /// 记录一次暂停请求，并返回是否需要实际注销已注册快捷键。
    fn suspend(&mut self) -> bool {
        self.resume_epoch += 1;
        self.suspend_count += 1;

        self.suspend_count == 1
    }

    /// 释放一次暂停请求；返回 `None` 表示没有对应的暂停可释放。
    fn resume(&mut self) -> Option<bool> {
        if self.suspend_count == 0 {
            return None;
        }

        self.suspend_count -= 1;

        Some(self.suspend_count == 0)
    }

    /// 开启新的恢复世代，用于让更早的延迟恢复任务失效。
    fn next_resume_epoch(&mut self) -> u64 {
        self.resume_epoch += 1;
        self.resume_epoch
    }

    /// 判断延迟恢复任务是否仍对当前暂停状态有效。
    fn allows_resume(&self, epoch: u64) -> bool {
        self.resume_epoch == epoch && self.suspend_count == 0
    }

    /// 判断全局快捷键是否处于暂停态。
    fn suspended(&self) -> bool {
        self.suspend_count > 0
    }
}

pub fn init(app: &AppHandle, shortcuts: &Shortcuts) -> Result<()> {
    app.manage(ShortcutManager::default());
    apply(app, shortcuts)
}

/// 暂停所有已注册的全局快捷键；用于前端录入快捷键期间避免旧绑定被触发。
pub fn suspend(app: &AppHandle) -> Result<()> {
    let manager = app.state::<ShortcutManager>();
    let should_unregister = {
        let mut pause = manager.pause.lock().expect("shortcut state poisoned");
        pause.suspend()
    };

    if should_unregister {
        unregister_active(app)?;
    }

    Ok(())
}

/// 释放一次全局快捷键暂停；只有所有录入器都结束后才按最新设置恢复注册。
pub fn resume(app: &AppHandle) -> Result<()> {
    let manager = app.state::<ShortcutManager>();
    let should_schedule = {
        let mut pause = manager.pause.lock().expect("shortcut state poisoned");

        match pause.resume() {
            Some(should_schedule) => should_schedule,
            None => {
                log::warn!("resume global shortcuts called without active suspend");

                return Ok(());
            }
        }
    };

    if should_schedule {
        schedule_resume(app);
    }

    Ok(())
}

/// 全量替换：先取消上一轮注册，再逐个注册新项。注册失败仅 emit 不中断其它项。
pub fn apply(app: &AppHandle, shortcuts: &Shortcuts) -> Result<()> {
    unregister_active(app)?;

    if is_suspended(app) {
        return Ok(());
    }

    let desired: [(&'static str, &str); 2] = [
        ("open_clipboard", &shortcuts.open_clipboard),
        ("open_preference", &shortcuts.open_preference),
    ];

    #[cfg(target_os = "windows")]
    win_v::set_enabled(app, shortcuts.win_v);

    let mut active = Vec::new();
    for (action, binding) in desired {
        if binding.trim().is_empty() {
            continue;
        }
        register_action(app, &mut active, action, binding, move |app, event| {
            handle_window_event(app, action, event);
        });
    }

    if shortcuts.quick_paste.enabled {
        let modifiers = shortcuts.quick_paste.modifiers.accelerator();
        for (key, offset) in QUICK_PASTE_KEYS {
            let binding = format!("{modifiers}+{key}");
            register_action(
                app,
                &mut active,
                QUICK_PASTE_ACTION,
                &binding,
                move |app, event| {
                    handle_quick_paste_event(app, offset, event);
                },
            );
        }
    }

    *app.state::<ShortcutManager>()
        .active
        .lock()
        .expect("shortcut state poisoned") = active;
    Ok(())
}

/// 判断是否仍有录入器持有全局快捷键暂停。
fn is_suspended(app: &AppHandle) -> bool {
    app.state::<ShortcutManager>()
        .pause
        .lock()
        .expect("shortcut state poisoned")
        .suspended()
}

/// 延迟恢复全局快捷键；若用户立即切到另一个录入器，新 suspend 会让本次恢复失效。
fn schedule_resume(app: &AppHandle) {
    let app = app.clone();
    let epoch = {
        let manager = app.state::<ShortcutManager>();
        let mut pause = manager.pause.lock().expect("shortcut state poisoned");
        pause.next_resume_epoch()
    };

    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(RESUME_DEBOUNCE).await;

        if !should_run_scheduled_resume(&app, epoch) {
            return;
        }

        let settings = app.state::<SettingsStore>().snapshot();
        if let Err(err) = apply(&app, &settings.shortcuts) {
            log::warn!("resume delayed global shortcuts failed: {err}");
        }
    });
}

/// 只有仍处于同一恢复世代且暂停计数为 0 时，延迟恢复任务才允许执行。
fn should_run_scheduled_resume(app: &AppHandle, epoch: u64) -> bool {
    let manager = app.state::<ShortcutManager>();
    let pause = manager.pause.lock().expect("shortcut state poisoned");

    pause.allows_resume(epoch)
}

/// 取消当前轮所有已注册快捷键，并清空内部 active 状态。
fn unregister_active(app: &AppHandle) -> Result<()> {
    let plugin = app.global_shortcut();
    let manager = app.state::<ShortcutManager>();

    let previous = {
        let mut guard = manager.active.lock().expect("shortcut state poisoned");
        std::mem::take(&mut *guard)
    };
    for (_, shortcut) in &previous {
        if let Err(err) = plugin.unregister(*shortcut) {
            log::warn!("unregister previous shortcut failed: {err:?}");
        }
    }

    Ok(())
}

/// 注册一项快捷键并记入 `active`；失败只记日志并 emit 冲突，不影响其它项。
fn register_action<F>(
    app: &AppHandle,
    active: &mut Vec<(&'static str, Shortcut)>,
    action: &'static str,
    binding: &str,
    handler: F,
) where
    F: Fn(&AppHandle, ShortcutEvent) + Send + Sync + 'static,
{
    match register_one(app, active, binding, handler) {
        Ok(shortcut) => active.push((action, shortcut)),
        Err(err) => {
            log::warn!("register shortcut {action}={binding} failed: {err}");
            let _ = app.emit(
                CONFLICT_EVENT,
                ShortcutConflict {
                    action,
                    binding: binding.into(),
                    reason: err.to_string(),
                },
            );
        }
    }
}

fn register_one<F>(
    app: &AppHandle,
    active: &[(&'static str, Shortcut)],
    binding: &str,
    handler: F,
) -> Result<Shortcut>
where
    F: Fn(&AppHandle, ShortcutEvent) + Send + Sync + 'static,
{
    let plugin = app.global_shortcut();
    let shortcut: Shortcut = binding
        .parse()
        .map_err(|err| AppError::Other(anyhow::anyhow!("parse shortcut {binding}: {err}")))?;

    // 本轮已由前面的动作注册时不能再注册：下面的 unregister 会把它顶掉。
    if active.iter().any(|(_, registered)| *registered == shortcut) {
        return Err(AppError::Other(anyhow::anyhow!(
            "{binding} is already used by another KwikPaste shortcut"
        )));
    }

    if plugin.is_registered(shortcut) {
        plugin
            .unregister(shortcut)
            .map_err(|err| AppError::Other(anyhow::anyhow!(err)))?;
    }

    plugin
        .on_shortcut(shortcut, move |app, _scut, event| {
            handler(app, event);
        })
        .map_err(|err| AppError::Other(anyhow::anyhow!(err)))?;
    Ok(shortcut)
}

fn handle_window_event(app: &AppHandle, action: &'static str, event: ShortcutEvent) {
    // Pressed 触发一次即可（Released 是按键松开），避免 toggle 在按下/松开各执行一次回弹。
    if !matches!(event.state(), ShortcutState::Pressed) {
        return;
    }
    let label = match action {
        "open_clipboard" => CLIPBOARD_WINDOW_LABEL,
        "open_preference" => PREFERENCE_WINDOW_LABEL,
        _ => return,
    };
    if let Err(err) = window::toggle_window(app, label) {
        log::warn!("toggle window via shortcut {action} failed: {err}");
    }
}

/// 快速粘贴在按下时触发：趁修饰键还按着先处理 Alt / Win 单独松开的副作用，再异步完成粘贴。
fn handle_quick_paste_event(app: &AppHandle, offset: i64, event: ShortcutEvent) {
    if !matches!(event.state(), ShortcutState::Pressed) {
        return;
    }

    if let Err(err) = crate::keystroke::mask_modifier_release() {
        log::warn!("mask modifier release before quick paste failed: {err}");
    }

    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        if let Err(err) = crate::clipboard::quick_paste(&app, offset).await {
            log::warn!("quick paste item {} failed: {err}", offset + 1);
        }
    });
}
