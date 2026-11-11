//! macOS 单实例：`/tmp/<identifier>_si.sock`，与 tauri-plugin-single-instance 相同。

use std::fs::File;
use std::io::{self, ErrorKind, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use crate::single_instance::{
    Claim, Invocation, current_invocation, decode_nul, encode_nul, socket_path,
};

type Sink = Box<dyn Fn(Invocation) + Send>;

/// 主实例守卫：丢弃时删掉 socket 文件。监听线程随进程退出。
pub struct PrimaryInstance {
    socket: PathBuf,
}

impl Drop for PrimaryInstance {
    fn drop(&mut self) {
        let Ok(_lock) = lock_socket(&self.socket) else {
            return;
        };
        let _ = std::fs::remove_file(&self.socket);
    }
}

pub(crate) fn claim(identifier: &str, sink: Sink) -> io::Result<Claim> {
    let socket = socket_path(identifier);
    let _lock = lock_socket(&socket)?;

    match forward(&socket, &current_invocation()) {
        Ok(()) => Ok(Claim::Forwarded),
        Err(err)
            if matches!(
                err.kind(),
                ErrorKind::NotFound | ErrorKind::ConnectionRefused
            ) =>
        {
            // 没有主实例在听：上一个实例崩溃时可能留下了陈旧的 socket 文件。
            let _ = std::fs::remove_file(&socket);
            let listener = UnixListener::bind(&socket)?;
            thread::Builder::new()
                .name("single-instance".to_owned())
                .spawn(move || listen(listener, sink))?;
            Ok(Claim::Primary(PrimaryInstance { socket }))
        }
        Err(err) => Err(err),
    }
}

/// 锁文件放 `$TMPDIR`（每用户一份）：放共享的 `/tmp` 会归先运行的用户所有，其他用户打不开。
fn lock_path(socket: &Path) -> PathBuf {
    let name = socket.with_extension("lock");
    std::env::temp_dir().join(name.file_name().unwrap_or_default())
}

/// 串行化 socket 的检查、替换与清理；锁文件不删除，避免新文件绕过仍存活的旧锁。
fn lock_socket(socket: &Path) -> io::Result<File> {
    let file = File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(lock_path(socket))?;
    file.lock()?;
    Ok(file)
}

/// 崩溃重启的子进程用：先等前一个实例（父进程）退出——它退出后本进程被 launchd 收养，父 PID
/// 随之改变——最多等 `timeout`，再按普通流程判重（陈旧的 socket 会被清掉）。
pub(crate) fn take_over(identifier: &str, sink: Sink, timeout: Duration) -> io::Result<Claim> {
    let parent = std::os::unix::process::parent_id();
    let deadline = Instant::now() + timeout;
    while parent != 1 && std::os::unix::process::parent_id() == parent && Instant::now() < deadline
    {
        thread::sleep(Duration::from_millis(50));
    }

    claim(identifier, sink)
}

fn forward(socket: &Path, invocation: &Invocation) -> io::Result<()> {
    let mut stream = UnixStream::connect(socket)?;
    stream.write_all(&encode_nul(invocation))?;
    stream.flush()
}

fn listen(listener: UnixListener, sink: Sink) {
    for stream in listener.incoming() {
        let mut stream = match stream {
            Ok(stream) => stream,
            Err(err) => {
                log::debug!("single instance accept failed: {err}");
                continue;
            }
        };
        let mut text = String::new();
        if let Err(err) = stream.read_to_string(&mut text) {
            log::debug!("single instance read failed: {err}");
            continue;
        }
        sink(decode_nul(&text));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn socket_claim_lock_serializes_independent_openers() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("instance.sock");
        let lock = lock_socket(&socket).unwrap();
        let contender = File::options()
            .read(true)
            .write(true)
            .open(lock_path(&socket))
            .unwrap();

        assert!(contender.try_lock().is_err());
        drop(lock);
        assert!(contender.try_lock().is_ok());
    }
}
