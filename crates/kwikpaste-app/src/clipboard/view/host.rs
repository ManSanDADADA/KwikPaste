//! 列表交给宿主的动作：粘贴、粘贴片段、复制、复制片段、拖出。面板固定时（[`super::pin`]）粘贴、复制之后
//! 面板留着。
//!
//! 数据来自平台层的 core 时走 [`crate::platform::paste`]（写回剪贴板、让出前台、注入粘贴键，复制后
//! 按设置隐藏面板）；夹具和自测 core 没有粘贴链路：粘贴什么也不做（列表照常发 `ListIntent::Paste`
//! 通知，自测据此核对），复制经数据源写回（夹具不碰系统剪贴板）。

use std::sync::Arc;

use futures::future::BoxFuture;
use gpui::{App, Task, Window};
use kwikpaste_core::clipboard::ClipboardFragment;
use kwikpaste_core::db::models::ClipboardItemQuery;

use super::{pin, request_panel};
use crate::{
    clipboard::source::ClipboardSource,
    platform::{self, PanelCommand, Trigger, TriggerSource},
};

/// 宿主动作。返回的任务在 UI 线程上 await，错误消息是给提示用的根因。
pub trait ItemHost {
    fn paste(
        &self,
        id: Arc<str>,
        plain: bool,
        selection: Option<(Vec<String>, ClipboardItemQuery)>,
        cx: &mut App,
    ) -> Task<anyhow::Result<()>>;

    fn paste_image_as_file(&self, id: Arc<str>, cx: &mut App) -> Task<anyhow::Result<()>>;

    fn paste_fragment(
        &self,
        id: Arc<str>,
        fragment: ClipboardFragment,
        cx: &mut App,
    ) -> Task<anyhow::Result<()>>;

    fn copy(
        &self,
        id: Arc<str>,
        plain: bool,
        selection: Option<(Vec<String>, ClipboardItemQuery)>,
        cx: &mut App,
    ) -> Task<anyhow::Result<()>>;

    /// 把图片识别出的文字写回剪贴板，隐藏规则同 [`ItemHost::copy`]。
    fn copy_image_text(&self, id: Arc<str>, cx: &mut App) -> Task<anyhow::Result<()>>;

    /// 把译文写回剪贴板，隐藏规则同 [`ItemHost::copy`]。
    fn copy_translation(&self, id: Arc<str>, cx: &mut App) -> Task<anyhow::Result<()>>;

    /// 粘贴译文，流程同 [`ItemHost::paste`]。
    fn paste_translation(&self, id: Arc<str>, cx: &mut App) -> Task<anyhow::Result<()>>;

    fn copy_fragment(
        &self,
        id: Arc<str>,
        fragment: ClipboardFragment,
        cx: &mut App,
    ) -> Task<anyhow::Result<()>>;

    /// 把记录拖到别的应用（`window` 是拖出源窗口），有 `selection` 时拖出合并后的多选。任务在拖放
    /// 结束后完成。
    fn drag_out(
        &self,
        id: Arc<str>,
        selection: Option<(Vec<String>, ClipboardItemQuery)>,
        window: &Window,
        cx: &mut App,
    ) -> Task<anyhow::Result<()>>;
}

/// 平台层的粘贴链路（数据来自宿主的 core）。
pub struct PlatformHost;

/// core 的错误（`AppError`）转成 anyhow，消息不变；`Other` 直接取出里面的 anyhow，保留可向下转型的根因。
fn ok_or_anyhow<T: 'static>(
    task: Task<kwikpaste_core::Result<T>>,
    cx: &mut App,
) -> Task<anyhow::Result<()>> {
    cx.foreground_executor().spawn(async move {
        task.await.map(|_| ()).map_err(|err| match err {
            kwikpaste_core::AppError::Other(err) => err,
            err => err.into(),
        })
    })
}

impl ItemHost for PlatformHost {
    fn paste(
        &self,
        id: Arc<str>,
        plain: bool,
        selection: Option<(Vec<String>, ClipboardItemQuery)>,
        cx: &mut App,
    ) -> Task<anyhow::Result<()>> {
        let task =
            platform::paste::paste_selection(cx, id.to_string(), plain, selection, pin::pinned(cx));
        ok_or_anyhow(task, cx)
    }

    fn paste_image_as_file(&self, id: Arc<str>, cx: &mut App) -> Task<anyhow::Result<()>> {
        let task = platform::paste::paste_image_as_file(cx, id.to_string(), pin::pinned(cx));
        ok_or_anyhow(task, cx)
    }

    fn paste_fragment(
        &self,
        id: Arc<str>,
        fragment: ClipboardFragment,
        cx: &mut App,
    ) -> Task<anyhow::Result<()>> {
        let keep_visible = pin::pinned(cx);
        let task = platform::paste::paste_fragment(cx, id.to_string(), fragment, keep_visible);
        ok_or_anyhow(task, cx)
    }

    fn copy(
        &self,
        id: Arc<str>,
        plain: bool,
        selection: Option<(Vec<String>, ClipboardItemQuery)>,
        cx: &mut App,
    ) -> Task<anyhow::Result<()>> {
        let task =
            platform::paste::copy_selection(cx, id.to_string(), plain, selection, pin::pinned(cx));
        ok_or_anyhow(task, cx)
    }

    fn copy_image_text(&self, id: Arc<str>, cx: &mut App) -> Task<anyhow::Result<()>> {
        let task = platform::paste::copy_image_text(cx, id.to_string(), pin::pinned(cx));
        ok_or_anyhow(task, cx)
    }

    fn copy_translation(&self, id: Arc<str>, cx: &mut App) -> Task<anyhow::Result<()>> {
        let task = platform::paste::copy_translation(cx, id.to_string(), pin::pinned(cx));
        ok_or_anyhow(task, cx)
    }

    fn paste_translation(&self, id: Arc<str>, cx: &mut App) -> Task<anyhow::Result<()>> {
        let task = platform::paste::paste_translation(cx, id.to_string(), pin::pinned(cx));
        ok_or_anyhow(task, cx)
    }

    fn copy_fragment(
        &self,
        id: Arc<str>,
        fragment: ClipboardFragment,
        cx: &mut App,
    ) -> Task<anyhow::Result<()>> {
        let keep_visible = pin::pinned(cx);
        let task = platform::paste::copy_fragment(cx, id.to_string(), fragment, keep_visible);
        ok_or_anyhow(task, cx)
    }

    fn drag_out(
        &self,
        id: Arc<str>,
        selection: Option<(Vec<String>, ClipboardItemQuery)>,
        window: &Window,
        cx: &mut App,
    ) -> Task<anyhow::Result<()>> {
        let task = match selection {
            Some((ids, query)) => {
                platform::drag_out::start_selection(id.to_string(), ids, query, window, cx)
            }
            None => platform::drag_out::start_item(id.to_string(), window, cx),
        };
        cx.foreground_executor().spawn(async move {
            let report = task.await?;
            log::debug!("drag-out of {id}: {report:?}");
            Ok(())
        })
    }
}

/// 没有粘贴链路的数据源（夹具、自测 core）。
pub struct SourceHost {
    source: Arc<dyn ClipboardSource>,
}

impl SourceHost {
    pub fn new(source: Arc<dyn ClipboardSource>) -> Self {
        Self { source }
    }
}

impl ItemHost for SourceHost {
    fn paste(
        &self,
        id: Arc<str>,
        plain: bool,
        _: Option<(Vec<String>, ClipboardItemQuery)>,
        _: &mut App,
    ) -> Task<anyhow::Result<()>> {
        log::info!("no paste chain for {id} (plain: {plain}): the data is not the host core's");
        Task::ready(Ok(()))
    }

    fn paste_image_as_file(&self, id: Arc<str>, _: &mut App) -> Task<anyhow::Result<()>> {
        log::info!("no image-file paste chain for {id}: the data is not the host core's");
        Task::ready(Ok(()))
    }

    fn paste_fragment(
        &self,
        id: Arc<str>,
        _: ClipboardFragment,
        _: &mut App,
    ) -> Task<anyhow::Result<()>> {
        log::info!("no paste chain for a fragment of {id}: the data is not the host core's");
        Task::ready(Ok(()))
    }

    fn copy(
        &self,
        id: Arc<str>,
        plain: bool,
        _: Option<(Vec<String>, ClipboardItemQuery)>,
        cx: &mut App,
    ) -> Task<anyhow::Result<()>> {
        self.copy_with(self.source.copy(id, plain), cx)
    }

    fn copy_image_text(&self, id: Arc<str>, cx: &mut App) -> Task<anyhow::Result<()>> {
        self.copy_with(self.source.copy_image_text(id), cx)
    }

    fn copy_translation(&self, id: Arc<str>, cx: &mut App) -> Task<anyhow::Result<()>> {
        self.copy_with(self.source.copy_translation(id), cx)
    }

    fn paste_translation(&self, id: Arc<str>, _: &mut App) -> Task<anyhow::Result<()>> {
        log::info!("no paste chain for the translation of {id}: the data is not the host core's");
        Task::ready(Ok(()))
    }

    fn copy_fragment(
        &self,
        id: Arc<str>,
        _: ClipboardFragment,
        cx: &mut App,
    ) -> Task<anyhow::Result<()>> {
        log::info!("fragment of {id} not copied: the data is not the host core's");
        if pin::pinned(cx) {
            request_panel(cx, PanelCommand::SetInputCapture(false));
        }
        Task::ready(Ok(()))
    }

    fn drag_out(
        &self,
        id: Arc<str>,
        _: Option<(Vec<String>, ClipboardItemQuery)>,
        _: &Window,
        _: &mut App,
    ) -> Task<anyhow::Result<()>> {
        log::info!("no drag-out for {id}: the data is not the host core's");
        Task::ready(Ok(()))
    }
}

impl SourceHost {
    /// 数据源写完剪贴板后按设置隐藏面板；钉住时只交还输入焦点。
    fn copy_with(
        &self,
        future: BoxFuture<'static, anyhow::Result<bool>>,
        cx: &mut App,
    ) -> Task<anyhow::Result<()>> {
        cx.spawn(async move |cx| {
            let hide = future.await?;
            let keep_visible = cx.update(|cx| pin::pinned(cx));
            if hide && !keep_visible {
                cx.update(|cx| {
                    request_panel(cx, PanelCommand::Hide(Trigger::now(TriggerSource::Copy)))
                });
            } else if keep_visible {
                cx.update(|cx| request_panel(cx, PanelCommand::SetInputCapture(false)));
            }
            Ok(())
        })
    }
}
