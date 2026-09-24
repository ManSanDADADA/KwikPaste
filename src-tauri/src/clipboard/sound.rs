//! 复制成功提示音。音频字节直接 `include_bytes!` 打入二进制——
//! 10KB 的单声道 16-bit PCM WAV 比额外维护 resource 路径 + 运行时文件 IO 简单。
//!
//! - Windows：交给系统 `PlaySoundW` 异步播放，不必打包音频输出与解码栈。
//! - macOS：rodio 的 `OutputStream` 是 `!Send` 且必须存活到播放结束，所以每次播放都
//!   新开一条短命线程：建流 → 解码 → 播放 → sink 空了即释放。剪贴板事件频率
//!   远低于音频开销（建流 ~ms 级），不必维护常驻 worker。

use tauri::{AppHandle, Manager};

use crate::settings::SettingsStore;

const COPY_SOUND_BYTES: &[u8] = include_bytes!("../../assets/sounds/copy.wav");

/// 若设置启用了 `feedback.copy_sound`，异步播放一次提示音。
/// 失败仅 warn——提示音不应阻断剪贴板入库主流程。
pub fn maybe_play_copy(app: &AppHandle) {
    let enabled = app
        .try_state::<SettingsStore>()
        .map(|s| s.snapshot().clipboard.feedback.copy_sound)
        .unwrap_or(false);
    if !enabled {
        return;
    }
    play();
}

/// 异步播放一次复制成功提示音，供偏好设置试听使用。
pub fn play_copy_sound() {
    play();
}

#[cfg(target_os = "windows")]
fn play() {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::HMODULE;
    use windows::Win32::Media::Audio::{PlaySoundW, SND_ASYNC, SND_MEMORY, SND_NODEFAULT};

    // SND_MEMORY 让第一个参数指向内存里的整段 WAV；SND_ASYNC 要求它在播放期间一直有效，静态字节满足。
    let played = unsafe {
        PlaySoundW(
            PCWSTR(COPY_SOUND_BYTES.as_ptr().cast()),
            HMODULE::default(),
            SND_MEMORY | SND_ASYNC | SND_NODEFAULT,
        )
    };
    if !played.as_bool() {
        log::warn!("play copy sound failed");
    }
}

#[cfg(target_os = "macos")]
fn play() {
    std::thread::Builder::new()
        .name("copy-sound".into())
        .spawn(|| {
            if let Err(err) = play_blocking() {
                log::warn!("play copy sound failed: {err}");
            }
        })
        .ok();
}

#[cfg(target_os = "macos")]
fn play_blocking() -> Result<(), String> {
    use std::io::Cursor;

    use rodio::{Decoder, OutputStream, Sink};

    let (_stream, handle) = OutputStream::try_default().map_err(|e| e.to_string())?;
    let sink = Sink::try_new(&handle).map_err(|e| e.to_string())?;
    let source = Decoder::new(Cursor::new(COPY_SOUND_BYTES)).map_err(|e| e.to_string())?;
    sink.append(source);
    sink.sleep_until_end();
    Ok(())
}
