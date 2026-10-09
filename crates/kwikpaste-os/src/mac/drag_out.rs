//! AppKit 拖出：同步开始会话，`endedAtPoint` 释放源和活动状态。Esc 由 AppKit tracking loop 取消。

use std::cell::RefCell;
use std::io;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use objc2::rc::Retained;
use objc2::runtime::{NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{AnyThread, DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSApplication, NSDragOperation, NSDraggingContext, NSDraggingItem, NSDraggingSession,
    NSDraggingSource, NSEvent, NSEventModifierFlags, NSEventType, NSImage, NSPasteboardItem,
    NSView,
};
use objc2_foundation::{NSData, NSMutableArray, NSPoint, NSRect, NSSize, NSString, NSURL};

use crate::drag_out::{DragData, DragReport, DragResult, set_active};

const PLAIN: &str = "public.utf8-plain-text";
const HTML: &str = "public.html";
const RTF: &str = "public.rtf";
const PREVIEW_SIZE: f64 = 128.;

static CREATED: AtomicU32 = AtomicU32::new(0);
static ENDED: AtomicU32 = AtomicU32::new(0);
static UNREGISTERED: AtomicU32 = AtomicU32::new(0);
static SELFTEST_RELEASE: AtomicBool = AtomicBool::new(false);

thread_local! {
    static SOURCE: RefCell<Option<Retained<DragSource>>> = const { RefCell::new(None) };
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "KwikPasteNativeDragSource"]
    #[ivars = SourceState]
    struct DragSource;

    unsafe impl NSObjectProtocol for DragSource {}

    unsafe impl NSDraggingSource for DragSource {
        #[unsafe(method(draggingSession:sourceOperationMaskForDraggingContext:))]
        unsafe fn operation(&self, _: &NSDraggingSession, _: NSDraggingContext) -> NSDragOperation {
            NSDragOperation::Copy
        }

        #[unsafe(method(draggingSession:endedAtPoint:operation:))]
        unsafe fn ended(&self, _: &NSDraggingSession, point: NSPoint, operation: NSDragOperation) {
            let state = self.ivars();
            let self_drop = state.view.window().is_some_and(|window| {
                let frame = window.frame();
                point.x >= frame.origin.x
                    && point.x < frame.origin.x + frame.size.width
                    && point.y >= frame.origin.y
                    && point.y < frame.origin.y + frame.size.height
            });
            set_active(false);
            ENDED.fetch_add(1, Ordering::SeqCst);
            (state.finished)(DragReport {
                result: if self_drop {
                    DragResult::Refused
                } else if operation == NSDragOperation::None {
                    DragResult::Cancelled
                } else {
                    DragResult::Dropped
                },
                effect: if self_drop {
                    0
                } else {
                    operation.bits() as u32
                },
                hresult: 0,
                cancel_requested: None,
            });
            SOURCE.with(|source| {
                source.borrow_mut().take();
            });
        }
    }
);

struct SourceState {
    view: Retained<NSView>,
    finished: Box<dyn Fn(DragReport)>,
}

/// 主线程同步白名单：只由 GPUI `on_mouse_move` 路径调用，不派发到异步任务。
///
/// # Safety
/// `native` 必须是仍存活的 GPUI NSView；调用时 NSApp.currentEvent 必须是本窗口的拖动事件。
pub unsafe fn begin_session_sync(
    native: isize,
    data: &DragData,
    preview: Option<&[u8]>,
    finished: impl Fn(DragReport) + 'static,
) -> io::Result<()> {
    let marker = MainThreadMarker::new()
        .ok_or_else(|| io::Error::other("beginDraggingSession requires the main thread"))?;
    if crate::drag_out::is_active() {
        return Err(io::Error::other("another drag session is active"));
    }
    let view = unsafe { Retained::retain(native as *mut NSView) }
        .ok_or_else(|| io::Error::other("drag source NSView is null"))?;
    let window = view
        .window()
        .ok_or_else(|| io::Error::other("drag source has no window"))?;
    let event = NSApplication::sharedApplication(marker)
        .currentEvent()
        .filter(|event| {
            event.r#type() == NSEventType::LeftMouseDragged
                && event.windowNumber() == window.windowNumber()
        })
        .ok_or_else(|| {
            io::Error::other("drag-out must begin during this window's mouseDragged event")
        })?;
    let items = NSMutableArray::<NSDraggingItem>::new();
    match data {
        DragData::Files(paths) => {
            if paths.is_empty()
                || paths
                    .iter()
                    .any(|path| !path.is_absolute() || !path.exists())
            {
                return Err(io::Error::other("drag source files are missing"));
            }
            for path in paths {
                let url = NSURL::fileURLWithPath_isDirectory(
                    &NSString::from_str(&path.to_string_lossy()),
                    path.is_dir(),
                );
                let item = NSDraggingItem::initWithPasteboardWriter(
                    NSDraggingItem::alloc(),
                    &ProtocolObject::from_retained(url),
                );
                items.addObject(&item);
            }
        }
        DragData::Text { plain, html, rtf } => {
            if plain.is_empty() {
                return Err(io::Error::other("drag text is empty"));
            }
            let pasteboard = NSPasteboardItem::new();
            for (kind, value) in [
                (PLAIN, Some(plain)),
                (HTML, html.as_ref()),
                (RTF, rtf.as_ref()),
            ] {
                if let Some(value) = value {
                    let bytes = NSData::from_vec(value.as_bytes().to_vec());
                    if !pasteboard.setData_forType(&bytes, &NSString::from_str(kind)) {
                        return Err(io::Error::other("drag pasteboard data could not be set"));
                    }
                }
            }
            let item = NSDraggingItem::initWithPasteboardWriter(
                NSDraggingItem::alloc(),
                &ProtocolObject::from_retained(pasteboard),
            );
            items.addObject(&item);
        }
    }
    let image = preview
        .and_then(|bytes| {
            NSImage::initWithData(NSImage::alloc(), &NSData::from_vec(bytes.to_vec()))
        })
        .unwrap_or_else(|| NSImage::initWithSize(NSImage::alloc(), NSSize::new(32., 32.)));
    let raw_size = image.size();
    let scale = PREVIEW_SIZE / raw_size.width.max(raw_size.height).max(1.);
    let size = NSSize::new(raw_size.width * scale, raw_size.height * scale);
    image.setSize(size);
    let cursor = event.locationInWindow();
    let frame = NSRect::new(
        NSPoint::new(cursor.x - size.width / 2., cursor.y - size.height / 2.),
        size,
    );
    for item in items.iter() {
        unsafe { item.setDraggingFrame_contents(frame, Some(&image)) };
    }
    let source = DragSource::alloc(marker).set_ivars(SourceState {
        view: view.clone(),
        finished: Box::new(finished),
    });
    let source: Retained<DragSource> = unsafe { msg_send![super(source), init] };
    SOURCE.with(|slot| *slot.borrow_mut() = Some(source.clone()));

    // `beginDraggingSession` enters AppKit's tracking loop synchronously. The selftest arms this
    // before posting its synthetic drag event; enqueueing the release at the front here means it
    // is observed after the current drag event starts the session, without CGEvent permissions.
    let selftest_release = if SELFTEST_RELEASE.swap(false, Ordering::SeqCst) {
        Some(
            NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
                NSEventType::LeftMouseUp,
                event.locationInWindow(),
                NSEventModifierFlags::empty(),
                0.,
                window.windowNumber(),
                None,
                0,
                1,
                1.,
            )
            .ok_or_else(|| io::Error::other("selftest mouse-up event could not be created"))?,
        )
    } else {
        None
    };
    set_active(true);
    if let Some(event) = selftest_release {
        NSApplication::sharedApplication(marker).postEvent_atStart(&event, true);
    }
    let session = view.beginDraggingSessionWithItems_event_source(
        &items,
        &event,
        &ProtocolObject::<dyn NSDraggingSource>::from_retained(source),
    );
    session.setAnimatesToStartingPositionsOnCancelOrFail(false);
    CREATED.fetch_add(1, Ordering::SeqCst);
    Ok(())
}

pub fn record_unregistered() {
    UNREGISTERED.fetch_add(1, Ordering::SeqCst);
}

pub fn selftest_counts() -> (u32, u32, u32) {
    (
        CREATED.load(Ordering::SeqCst),
        ENDED.load(Ordering::SeqCst),
        UNREGISTERED.load(Ordering::SeqCst),
    )
}

/// 自测只向本进程 NSApp 的事件队列派发鼠标事件，不访问其它应用或系统剪贴板。
///
/// # Safety
/// `native` 必须是仍存活的本进程 NSView。
pub unsafe fn selftest_mouse(native: isize, kind: NSEventType, x: f64, y: f64) -> io::Result<()> {
    let marker = MainThreadMarker::new()
        .ok_or_else(|| io::Error::other("selftest event requires main thread"))?;
    let view = unsafe { &*(native as *const NSView) };
    let window = view
        .window()
        .ok_or_else(|| io::Error::other("selftest window is gone"))?;
    let event = NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
        kind, NSPoint::new(x, y), NSEventModifierFlags::empty(), 0., window.windowNumber(), None, 0, 1, 1.)
        .ok_or_else(|| io::Error::other("selftest mouse event could not be created"))?;
    NSApplication::sharedApplication(marker).postEvent_atStart(&event, false);
    Ok(())
}

/// 自测的 Esc 只进入本进程的 AppKit tracking loop。
pub fn selftest_escape() -> io::Result<()> {
    let marker = MainThreadMarker::new()
        .ok_or_else(|| io::Error::other("selftest event requires main thread"))?;
    let escape = NSString::from_str("\u{1b}");
    let event = NSEvent::keyEventWithType_location_modifierFlags_timestamp_windowNumber_context_characters_charactersIgnoringModifiers_isARepeat_keyCode(
        NSEventType::KeyDown, NSPoint::ZERO, NSEventModifierFlags::empty(), 0., 0, None, &escape, &escape, false, 53)
        .ok_or_else(|| io::Error::other("selftest Escape event could not be created"))?;
    NSApplication::sharedApplication(marker).postEvent_atStart(&event, false);
    Ok(())
}

/// 给自测视图发送一组本进程鼠标事件；实际拖出仍由视图的 `on_mouse_move` 同步发起。
///
/// AppKit 的拖动 tracking loop 在 CI 上没有真实鼠标松开事件，因此先为本次会话 armed，
/// 再由 `begin_session_sync` 把合成的 `LeftMouseUp` 放进本进程事件队列，让 `endedAtPoint`
/// 走正常的清理路径。
///
/// # Safety
/// `native` 必须是主线程上仍然存活的 NSView 指针。
pub unsafe fn selftest_drag(native: isize) -> io::Result<()> {
    let view = unsafe { &*(native as *const NSView) };
    let bounds = view.bounds();
    let point = NSPoint::new(
        bounds.origin.x + bounds.size.width / 2.,
        bounds.origin.y + bounds.size.height * 0.55,
    );
    SELFTEST_RELEASE.store(true, Ordering::SeqCst);
    unsafe {
        selftest_mouse(native, NSEventType::LeftMouseDown, point.x, point.y)?;
        selftest_mouse(
            native,
            NSEventType::LeftMouseDragged,
            point.x + 12.,
            point.y + 12.,
        )?;
    }
    Ok(())
}
