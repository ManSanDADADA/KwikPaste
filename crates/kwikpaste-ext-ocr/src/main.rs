//! OCR 框架入口，仅由独立的 OCR extension 子进程调用。
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

use std::io::{Read, Write};
use std::sync::mpsc;
use std::time::Duration;

use kwikpaste_ext_protocol::{self as protocol, OcrSupport, Outcome, Request, Response};

#[cfg(target_os = "macos")]
use macos::Engine;
#[cfg(target_os = "windows")]
use windows::Engine;

/// EOF、10 秒空闲或父进程消失后退出；整个 helper 串行识别，不把框架带到主进程。
fn run_helper<R: Read + Send + 'static, W: Write>(
    mut reader: R,
    mut writer: W,
) -> anyhow::Result<()> {
    background();
    #[cfg(target_os = "macos")]
    watch_parent()?;
    let (sender, receiver) = mpsc::sync_channel(1);
    std::thread::Builder::new()
        .name("ocr-input".into())
        .spawn(move || {
            let mut buffer = Vec::new();
            loop {
                let request = protocol::read_frame::<Request>(&mut reader, &mut buffer);
                let done = !matches!(request, Ok(Some(_)));
                if sender.send(request).is_err() || done {
                    break;
                }
            }
        })?;
    let mut engine = Engine::new();
    let mut buffer = Vec::new();
    loop {
        let request = match receiver.recv_timeout(Duration::from_secs(10)) {
            Ok(Ok(Some(request))) => request,
            Ok(Err(err)) => return Err(err.into()),
            _ => break,
        };
        let response = match request {
            Request::Probe => Response::Probe {
                support: engine
                    .as_ref()
                    .map_or_else(|_| OcrSupport::Unsupported, Engine::support),
            },
            Request::Recognize { job_id, path } => {
                let outcome = match &mut engine {
                    Ok(engine) => engine
                        .recognize(std::path::Path::new(&path))
                        .unwrap_or_else(|err| Outcome::Failed {
                            reason: err.to_string(),
                        }),
                    Err(err) => Outcome::Failed {
                        reason: err.to_string(),
                    },
                };
                Response::Recognize { job_id, outcome }
            }
        };
        protocol::write_frame(&mut writer, &response, &mut buffer)?;
    }
    Ok(())
}

/// 将耗时识别降为后台任务；不影响主进程的优先级。
fn background() {
    #[cfg(target_os = "windows")]
    unsafe {
        use ::windows::Win32::System::Threading::{
            BELOW_NORMAL_PRIORITY_CLASS, GetCurrentProcess, PROCESS_MODE_BACKGROUND_BEGIN,
            SetPriorityClass,
        };
        let _ = SetPriorityClass(GetCurrentProcess(), BELOW_NORMAL_PRIORITY_CLASS);
        let _ = SetPriorityClass(GetCurrentProcess(), PROCESS_MODE_BACKGROUND_BEGIN);
    }
    #[cfg(target_os = "macos")]
    unsafe {
        libc::setpriority(libc::PRIO_DARWIN_PROCESS, 0, libc::PRIO_DARWIN_BG);
    }
}

/// macOS 没有 Job Object；父 PID 变化时直接结束 helper，即使框架正在识别。
#[cfg(target_os = "macos")]
fn watch_parent() -> anyhow::Result<()> {
    let parent = unsafe { libc::getppid() };
    if parent <= 1 {
        anyhow::bail!("OCR parent has already exited");
    }
    std::thread::Builder::new()
        .name("ocr-parent".into())
        .spawn(move || {
            loop {
                std::thread::sleep(Duration::from_millis(250));
                if unsafe { libc::getppid() } != parent {
                    std::process::exit(0);
                }
            }
        })?;
    Ok(())
}

/// 去掉两个 CJK 字符之间的空白，保留拉丁单词的分隔和换行。
#[cfg(any(target_os = "windows", test))]
fn normalize_line(line: &str) -> String {
    fn cjk(ch: char) -> bool {
        matches!(ch as u32, 0x3400..=0x4dbf | 0x4e00..=0x9fff | 0xf900..=0xfaff | 0x20000..=0x3134f | 0x3040..=0x30ff | 0xac00..=0xd7af)
    }
    let mut result = String::with_capacity(line.len());
    let mut space = String::new();
    let mut previous = None;
    for ch in line.chars() {
        if ch.is_whitespace() {
            space.push(ch);
            continue;
        }
        if !(previous.is_some_and(cjk) && cjk(ch)) {
            result.push_str(&space);
        }
        space.clear();
        result.push(ch);
        previous = Some(ch);
    }
    result.trim().to_owned()
}

fn main() -> anyhow::Result<()> {
    run_helper(std::io::stdin(), std::io::stdout().lock())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cjk_spacing_preserves_latin_words() {
        assert_eq!(
            normalize_line("中 文 识 别 English words"),
            "中文识别 English words"
        );
    }
}
