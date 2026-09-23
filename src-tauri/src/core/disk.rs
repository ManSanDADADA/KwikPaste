//! 本地数据目录的体积统计，供偏好页存储占用与按存储上限清理共用。

use std::fs;
use std::path::Path;

use anyhow::Context;

use super::Result;

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
    if !path.exists() {
        return Ok(0);
    }

    let mut total = 0;
    for entry in
        fs::read_dir(path).with_context(|| format!("failed to read directory at {path:?}"))?
    {
        let entry = entry.with_context(|| format!("failed to read entry under {path:?}"))?;
        let entry_path = entry.path();
        let metadata = entry
            .metadata()
            .with_context(|| format!("failed to read metadata at {entry_path:?}"))?;

        if metadata.is_dir() {
            total += dir_size(&entry_path)?;
            continue;
        }

        total += metadata.len();
    }

    Ok(total)
}
