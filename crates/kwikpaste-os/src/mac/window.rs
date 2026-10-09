//! 普通窗口的前台交接；剪贴板的非激活面板不走此路径。

use std::{ffi::c_void, io, ptr::NonNull};

use objc2::{MainThreadMarker, rc::Retained};
use objc2_app_kit::{
    NSApplicationActivationOptions, NSRunningApplication, NSView, NSWindow, NSWindowStyleMask,
};

/// 持有窗口，避免延迟任务使用已经释放的原生指针。
pub struct OrdinaryWindow(Retained<NSWindow>);

impl OrdinaryWindow {
    /// 从 GPUI 的有效 NSView 获取普通窗口；仅允许主线程调用。
    ///
    /// # Safety
    /// `view` 必须是仍存活的 NSView 指针。
    pub unsafe fn from_raw(view: NonNull<c_void>) -> io::Result<Self> {
        MainThreadMarker::new()
            .ok_or_else(|| io::Error::other("window capture requires the main thread"))?;
        // SAFETY: 调用者保证有效 NSView，窗口被 Retained 持有。
        let view = unsafe { view.cast::<NSView>().as_ref() };
        let window = view
            .window()
            .ok_or_else(|| io::Error::other("the view has no window"))?;
        if window
            .styleMask()
            .contains(NSWindowStyleMask::NonactivatingPanel)
        {
            return Err(io::Error::other("cannot activate a nonactivating panel"));
        }
        Ok(Self(window))
    }

    /// 必须在主线程且 GPUI 窗口借用之外执行，AppKit 激活回调可能重入。
    pub fn bring_to_front(&self) -> io::Result<()> {
        MainThreadMarker::new()
            .ok_or_else(|| io::Error::other("window activation requires the main thread"))?;
        if self.0.isMiniaturized() {
            self.0.deminiaturize(None);
        }
        #[allow(deprecated)]
        if !NSRunningApplication::currentApplication()
            .activateWithOptions(NSApplicationActivationOptions::empty())
        {
            return Err(io::Error::other("preferences activation was refused"));
        }
        self.0.makeKeyAndOrderFront(None);
        Ok(())
    }

    /// 真实 AppKit 窗口状态，供双门控自测读取。
    pub fn is_key_and_visible(&self) -> bool {
        self.0.isVisible()
            && self.0.isKeyWindow()
            && NSRunningApplication::currentApplication().isActive()
    }
}
