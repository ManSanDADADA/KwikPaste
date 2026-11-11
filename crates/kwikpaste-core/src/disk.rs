//! 本地数据目录的体积统计，供偏好页存储占用与按存储上限清理共用。

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use anyhow::Context;

use crate::error::Result;

/// 文件不存在时按 0 处理，避免首次启动时显示错误状态。
pub fn file_size(path: &Path) -> Result<u64> {
    if !path.exists() {
        return Ok(0);
    }

    Ok(fs::metadata(path)
        .with_context(|| format!("failed to read metadata at {path:?}"))?
        .len())
}

/// 递归统计目录大小；目录不存在时按 0 处理。
pub fn dir_size(path: &Path) -> Result<u64> {
    dir_size_excluding(path, &[])
}

/// 递归统计目录大小，跳过 `excluded` 里列出的文件；目录不存在时按 0 处理。
/// 遍历期间被别处删掉或改名的条目（缩略图临时文件落盘、清理删文件）同样按 0 处理，不让整次统计失败。
pub fn dir_size_excluding(path: &Path, excluded: &[PathBuf]) -> Result<u64> {
    let Some(entries) = skip_missing(fs::read_dir(path), || {
        format!("failed to read directory at {path:?}")
    })?
    else {
        return Ok(0);
    };

    let mut total = 0;
    for entry in entries {
        let Some(entry) = skip_missing(entry, || format!("failed to read entry under {path:?}"))?
        else {
            continue;
        };
        let entry_path = entry.path();
        if excluded.contains(&entry_path) {
            continue;
        }
        let Some(metadata) = skip_missing(entry.metadata(), || {
            format!("failed to read metadata at {entry_path:?}")
        })?
        else {
            continue;
        };

        if metadata.is_dir() {
            total += dir_size_excluding(&entry_path, excluded)?;
            continue;
        }

        total += metadata.len();
    }

    Ok(total)
}

/// IO 结果里的 NotFound 变成 `None`，其余错误带上下文返回。
fn skip_missing<T>(
    result: std::io::Result<T>,
    context: impl FnOnce() -> String,
) -> Result<Option<T>> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(None),
        Err(err) => Err(anyhow::Error::new(err).context(context()).into()),
    }
}

/// 未创建的目标目录沿父级回溯，查询最近已存在目录所在卷的可用字节数。
pub fn available_space(path: &Path) -> Result<u64> {
    let ancestor = path
        .ancestors()
        .find(|ancestor| ancestor.is_dir())
        .context("no existing ancestor for storage target")?;
    available_space_at(ancestor)
}

#[cfg(target_os = "windows")]
fn available_space_at(path: &Path) -> Result<u64> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;

    let path: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut available = 0;
    // 缓冲区以 NUL 结尾，输出指针仅在调用期间有效；读取当前用户实际可用的空间。
    let ok = unsafe {
        GetDiskFreeSpaceExW(
            path.as_ptr(),
            &mut available,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        return Err(anyhow::anyhow!(std::io::Error::last_os_error()).into());
    }
    Ok(available)
}

#[cfg(target_os = "macos")]
fn available_space_at(path: &Path) -> Result<u64> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let path = CString::new(path.as_os_str().as_bytes()).context("target path contains NUL")?;
    let mut stat = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // statvfs 成功才读初始化后的结构体；f_bavail 不包含仅 root 可用的保留块。
    if unsafe { libc::statvfs(path.as_ptr(), stat.as_mut_ptr()) } != 0 {
        return Err(anyhow::anyhow!(std::io::Error::last_os_error()).into());
    }
    let stat = unsafe { stat.assume_init() };
    Ok((stat.f_bavail as u64).saturating_mul(stat.f_frsize as u64))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn excluded_files_are_skipped_while_walking() {
        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join("db");
        fs::create_dir_all(&db).unwrap();
        fs::write(db.join("clipboard.db"), vec![0u8; 4096]).unwrap();
        fs::write(db.join("clipboard.db-wal"), vec![0u8; 1000]).unwrap();
        fs::write(db.join("clipboard.db-shm"), vec![0u8; 300]).unwrap();
        fs::write(temp.path().join("settings.json"), vec![0u8; 20]).unwrap();
        // 只跳过点名的旁路文件，别处同名后缀的文件照常计入。
        fs::write(temp.path().join("notes-wal"), vec![0u8; 7]).unwrap();

        let excluded = [db.join("clipboard.db-wal"), db.join("clipboard.db-shm")];

        assert_eq!(dir_size(temp.path()).unwrap(), 4096 + 1000 + 300 + 20 + 7);
        assert_eq!(
            dir_size_excluding(temp.path(), &excluded).unwrap(),
            4096 + 20 + 7
        );
        assert_eq!(
            dir_size_excluding(&temp.path().join("missing"), &excluded).unwrap(),
            0
        );
    }
}
