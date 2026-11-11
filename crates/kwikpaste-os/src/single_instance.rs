//! 单实例。
//!
//! 协议逐字节沿用 tauri-plugin-single-instance 2.4.3（未开 `semver` 特性），名字全部由 identifier 派生：
//! 正式版用 `com.fastthree.kwikpaste` 时能和 1.x 互相识别、互相转交参数；开发版和自测必须传别的
//! identifier，否则会把参数转交给本机正在运行的 1.x，或者被它当成第二实例。
//!
//! - Windows：命名互斥量判重，主实例开一个隐藏窗口收 `WM_COPYDATA`（`dwData = 1542`，
//!   数据是 UTF-8 的 `"<cwd>|<arg0>|<arg1>…\0"`）。
//! - macOS：`/tmp/<identifier>_si.sock`，数据是 `"<cwd>\0\0<arg0>\0<arg1>…"`。
//!
//! 参数里含 `|` 会被切坏，这是对方协议的既有行为，为了新旧互认保持不变。

use std::io;

#[cfg(target_os = "macos")]
use crate::mac::single_instance as platform;
#[cfg(target_os = "windows")]
use crate::win::single_instance as platform;

pub use platform::PrimaryInstance;

/// 崩溃重启的子进程带的参数：`--relaunched-after-crash <n>`，`n` 是 10 分钟内的第几次。
pub const RELAUNCHED_AFTER_CRASH_ARG: &str = "--relaunched-after-crash";
/// 降级重启另带的参数。
pub const CRASH_DEGRADED_ARG: &str = "--crash-degraded";

/// 后启动的实例转交过来的启动参数。`args[0]` 是对方的可执行文件路径。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invocation {
    pub cwd: String,
    pub args: Vec<String>,
}

/// [`claim`] 的结果。
pub enum Claim {
    /// 本进程是主实例。守卫存活期间，后启动的实例会把参数转交过来；退出前丢弃守卫以释放名字。
    Primary(PrimaryInstance),
    /// 已有主实例在运行，本进程的参数已经转交给它，应当直接退出。
    Forwarded,
}

/// 判重并在需要时转交参数。必须在创建 GPU 设备、窗口之前调用，Windows 上还必须在主线程调用：
/// 收参数的隐藏窗口属于调用线程，靠该线程的消息循环派发。
///
/// `on_invocation` 在 Windows 上于主线程的窗口过程里调用，在 macOS 上于监听线程调用；
/// 里面只应把参数转发出去（例如送进 channel），不要做耗时操作或碰 UI 框架。
pub fn claim(
    identifier: &str,
    on_invocation: impl Fn(Invocation) + Send + 'static,
) -> io::Result<Claim> {
    platform::claim(identifier, Box::new(on_invocation))
}

/// 崩溃重启的子进程（`--relaunched-after-crash`）用来代替 [`claim`]：不把参数转交给正在死去的
/// 前一个实例，而是等它退出后成为主实例，最多等 `timeout`。Windows 上超时返回 `TimedOut` 错误；
/// macOS 上超时后按普通流程判重，前一个实例还活着时返回 [`Claim::Forwarded`]。两种情况调用方都
/// 应直接退出。
pub fn take_over(
    identifier: &str,
    on_invocation: impl Fn(Invocation) + Send + 'static,
    timeout: std::time::Duration,
) -> io::Result<Claim> {
    platform::take_over(identifier, Box::new(on_invocation), timeout)
}

/// 当前进程的工作目录和参数，按对方协议的取法（取不到工作目录时为空串）。
pub(crate) fn current_invocation() -> Invocation {
    let cwd = std::env::current_dir()
        .ok()
        .and_then(|dir| dir.to_str().map(str::to_owned))
        .unwrap_or_default();

    // std::env::args 遇到不是合法 Unicode 的参数会 panic（例如带孤立代理项的 NTFS 路径）；
    // 两边的协议本来就按 UTF-8 有损传递。
    Invocation {
        cwd,
        args: std::env::args_os()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect(),
    }
}

/// Windows 协议：`"<cwd>|<arg0>|…\0"`。
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
pub(crate) fn encode_pipe(invocation: &Invocation) -> Vec<u8> {
    let mut data = invocation.cwd.clone();
    for arg in &invocation.args {
        data.push('|');
        data.push_str(arg);
    }
    data.push('\0');
    data.into_bytes()
}

/// 解析 Windows 协议的数据；到第一个 NUL 为止，非法 UTF-8 按替换字符处理。
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
pub(crate) fn decode_pipe(bytes: &[u8]) -> Invocation {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    let text = String::from_utf8_lossy(&bytes[..end]);
    let mut parts = text.split('|');
    let cwd = parts.next().unwrap_or_default().to_owned();

    Invocation {
        cwd,
        args: parts.map(str::to_owned).collect(),
    }
}

/// macOS 协议：`"<cwd>\0\0<arg0>\0<arg1>…"`。
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn encode_nul(invocation: &Invocation) -> Vec<u8> {
    let mut data = invocation.cwd.clone();
    data.push_str("\0\0");
    data.push_str(&invocation.args.join("\0"));
    data.into_bytes()
}

/// 解析 macOS 协议的数据。
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn decode_nul(text: &str) -> Invocation {
    let (cwd, args) = text.split_once("\0\0").unwrap_or_default();

    Invocation {
        cwd: cwd.to_owned(),
        args: args.split('\0').map(str::to_owned).collect(),
    }
}

/// macOS 的 socket 路径：identifier 里的 `.`、`-` 换成 `_`，放在 `/tmp`（路径要短于 100 字节）。
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn socket_path(identifier: &str) -> std::path::PathBuf {
    let name = identifier.replace(['.', '-'], "_");
    std::path::PathBuf::from(format!("/tmp/{name}_si.sock"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Invocation {
        Invocation {
            cwd: r"C:\Users\me".to_owned(),
            args: vec![
                r"C:\Program Files\KwikPaste\KwikPaste.exe".to_owned(),
                "--auto-launch".to_owned(),
            ],
        }
    }

    #[test]
    fn pipe_format_matches_the_tauri_plugin() {
        let bytes = encode_pipe(&sample());

        assert_eq!(
            bytes,
            b"C:\\Users\\me|C:\\Program Files\\KwikPaste\\KwikPaste.exe|--auto-launch\0".to_vec()
        );
        assert_eq!(decode_pipe(&bytes), sample());
    }

    #[test]
    fn pipe_decode_stops_at_nul_and_tolerates_missing_terminator() {
        assert_eq!(decode_pipe(b"cwd|a\0garbage"), decode_pipe(b"cwd|a"));
        assert_eq!(
            decode_pipe(b""),
            Invocation {
                cwd: String::new(),
                args: Vec::new()
            }
        );
    }

    #[test]
    fn nul_format_matches_the_tauri_plugin() {
        let bytes = encode_nul(&sample());

        assert_eq!(
            bytes,
            b"C:\\Users\\me\0\0C:\\Program Files\\KwikPaste\\KwikPaste.exe\0--auto-launch".to_vec()
        );
        let text = String::from_utf8(bytes).expect("utf-8");
        assert_eq!(decode_nul(&text), sample());
    }

    #[test]
    fn socket_path_matches_the_tauri_plugin() {
        assert_eq!(
            socket_path("com.fastthree.kwikpaste"),
            std::path::PathBuf::from("/tmp/com_fastthree_kwikpaste_si.sock")
        );
        assert_eq!(
            socket_path("com.fastthree.kwikpaste.native-dev"),
            std::path::PathBuf::from("/tmp/com_fastthree_kwikpaste_native_dev_si.sock")
        );
    }
}
