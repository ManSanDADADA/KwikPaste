//! 原生粘贴交接票据：窗口/进程身份和会话代号只用于短期验证，不持久化。

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PasteTarget {
    pub generation: u64,
    /// Windows 为 HWND；macOS 以保留的 NSRunningApplication 身份验证，窗口值为 0。
    pub window: isize,
    pub process_id: u32,
}
