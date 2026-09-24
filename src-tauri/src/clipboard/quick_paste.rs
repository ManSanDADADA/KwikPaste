//! 全局快速粘贴：修饰键 + 数字把「全部」视图里的第 N 条历史直接粘贴到前台应用，不唤起剪贴板窗口。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tauri::{AppHandle, Emitter, Manager};

use super::guard::WritebackGuard;
use super::storage::ImageStore;
use super::watcher::CLIPBOARD_UPDATED_EVENT;
use super::write::write_to_clipboard;
use crate::core::Result;
use crate::db::items::{find_item_by_id, find_item_id_at, increment_item_use_count};
use crate::db::models::ClipboardKind;
use crate::db::DatabaseState;
use crate::settings::{Content, SettingsStore};
use crate::window::{self, CLIPBOARD_WINDOW_LABEL};

/// 等用户松开修饰键的上限；超时说明还按着键，这时注入粘贴会被目标应用读成别的组合键。
const MODIFIER_RELEASE_TIMEOUT: Duration = Duration::from_secs(2);

/// 剪贴板窗口隐藏 / 让出键盘焦点都是异步派发，注入粘贴前后各等一拍，与窗口内粘贴一致。
const WINDOW_SETTLE_DELAY: Duration = Duration::from_millis(50);

static RUNNING: AtomicBool = AtomicBool::new(false);

/// 一次快速粘贴的占用标记；上一次还没粘完时新的触发直接忽略，避免连按叠出多次粘贴。
struct RunningGuard;

impl RunningGuard {
    fn acquire() -> Option<Self> {
        (!RUNNING.swap(true, Ordering::AcqRel)).then_some(Self)
    }
}

impl Drop for RunningGuard {
    fn drop(&mut self) {
        RUNNING.store(false, Ordering::Release);
    }
}

/// 把第 `offset` 条（从 0 起）历史写回剪贴板，等用户松开修饰键后粘贴到前台应用；
/// 历史不足 `offset + 1` 条时什么也不做。
pub async fn quick_paste(app: &AppHandle, offset: i64) -> Result<()> {
    let Some(_running) = RunningGuard::acquire() else {
        return Ok(());
    };

    let pool = app.state::<DatabaseState>().pool().await;
    let content = app.state::<SettingsStore>().snapshot().clipboard.content;
    let Some(id) = find_item_id_at(&pool, content.sort, offset).await? else {
        return Ok(());
    };
    let Some(item) = find_item_by_id(&pool, &id).await? else {
        return Ok(());
    };

    let image_store = app.state::<ImageStore>();
    let guard = app.state::<Arc<WritebackGuard>>();
    write_to_clipboard(
        &image_store,
        &guard,
        &item,
        writes_plain(&content, item.kind),
    )?;

    if content.update_on_reuse {
        increment_item_use_count(&pool, &id).await?;
        if let Err(err) = app.emit(
            CLIPBOARD_UPDATED_EVENT,
            serde_json::json!({ "id": id, "kind": item.kind, "deduplicated": true }),
        ) {
            log::warn!("emit {CLIPBOARD_UPDATED_EVENT} after quick paste failed: {err}");
        }
    }

    if !crate::keystroke::wait_for_modifiers_released(MODIFIER_RELEASE_TIMEOUT).await {
        log::warn!("quick paste left item {id} on the clipboard: modifier keys are still held");
        return Ok(());
    }

    paste_into_foreground_app(app).await
}

/// 与窗口内默认粘贴一致：文本按「粘贴时去除格式」、文件按「粘贴为路径」决定是否写纯文本。
fn writes_plain(content: &Content, kind: ClipboardKind) -> bool {
    kind == ClipboardKind::Text && content.paste_plain
        || kind == ClipboardKind::Files && content.paste_files_as_path
}

/// 剪贴板窗口隐藏时直接注入粘贴；可见时先隐藏它（固定时改为让出键盘焦点），
/// 否则 macOS 上 ⌘V 会被仍是 key window 的面板吞掉。
async fn paste_into_foreground_app(app: &AppHandle) -> Result<()> {
    let visible = app
        .get_webview_window(CLIPBOARD_WINDOW_LABEL)
        .and_then(|window| window.is_visible().ok())
        .unwrap_or(false);
    if !visible {
        return crate::keystroke::simulate_paste();
    }

    let pinned = window::is_clipboard_window_pinned();
    if pinned {
        #[cfg(target_os = "macos")]
        if let Err(err) = window::macos::resign_clipboard_panel_key(app) {
            log::warn!("resign clipboard panel key before quick paste failed: {err:?}");
        }
    } else if let Err(err) = window::hide_window(app, CLIPBOARD_WINDOW_LABEL) {
        log::warn!("hide clipboard window before quick paste failed: {err:?}");
    }

    tokio::time::sleep(WINDOW_SETTLE_DELAY).await;
    crate::keystroke::simulate_paste()?;

    #[cfg(target_os = "macos")]
    if pinned {
        tokio::time::sleep(WINDOW_SETTLE_DELAY).await;
        if let Err(err) = window::macos::make_clipboard_panel_key(app) {
            log::warn!("restore clipboard panel key after quick paste failed: {err:?}");
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_paste_follows_default_paste_settings() {
        let mut content = Content::default();
        assert!(!writes_plain(&content, ClipboardKind::Text));
        assert!(!writes_plain(&content, ClipboardKind::Files));

        content.paste_plain = true;
        assert!(writes_plain(&content, ClipboardKind::Text));
        assert!(!writes_plain(&content, ClipboardKind::Image));

        content.paste_files_as_path = true;
        assert!(writes_plain(&content, ClipboardKind::Files));
    }

    #[test]
    fn running_guard_rejects_overlapping_runs() {
        let first = RunningGuard::acquire();
        assert!(first.is_some());
        assert!(RunningGuard::acquire().is_none());

        drop(first);
        assert!(RunningGuard::acquire().is_some());
    }
}
