//! 文件记录的展示：文件条目、文件类型图标与路径判断。

use std::path::Path;

use serde::Serialize;
use sqlx::SqlitePool;

use crate::clipboard::{get_icon_cache_key, icon_png, FileIconStore, DIR_CACHE_KEY};
use crate::db::items::IMAGE_FILE_EXTENSIONS;
use crate::db::models::Platform;
use crate::error::{AppError, Result};

/// 单个文件路径的图标与存在状态。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileIconResult {
    /// icon 文件磁盘绝对路径；抽取失败 / 路径已删除且未缓存时为 `None`。
    pub icon_path: Option<String>,
    /// 当前路径是否仍存在于磁盘。
    pub exists: bool,
}

/// 统计 files payload 中的有效路径数量。
pub(crate) fn count_file_paths(content: &str) -> usize {
    content.split('\n').filter(|path| !path.is_empty()).count()
}

/// 解析文件是否为目录：存在时信任实时 metadata，缺失时回退到入库时保存的 file_types。
pub(crate) fn resolve_preview_file_is_dir(
    path: &Path,
    file_types: Option<&str>,
    index: usize,
) -> bool {
    if let Ok(metadata) = path.metadata() {
        return metadata.is_dir();
    }

    file_types
        .and_then(|types| types.split(',').nth(index))
        .map(|file_type| file_type == "d")
        .unwrap_or(false)
}

/// 解析普通文件大小；目录或缺失路径不显示 size。
pub(crate) fn resolve_preview_file_size(path: &Path, is_dir: bool) -> Option<i64> {
    if is_dir {
        return None;
    }

    path.metadata().ok().map(|metadata| metadata.len() as i64)
}

/// 将本地路径转为 UTF-8 字符串，失败时返回 clipboard 错误。
pub(crate) fn path_to_string(path: &Path, label: &str) -> Result<String> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| AppError::Clipboard(format!("{label} is not valid utf-8")))
}

/// 与 1.x 前端 `utils/is.ts` 的 `isImage` 同义：按扩展名（大小写不敏感）判断常见图片格式。
pub fn is_image_path(path: &str) -> bool {
    let Some(ext) = Path::new(path).extension().and_then(|e| e.to_str()) else {
        return false;
    };

    IMAGE_FILE_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str())
}

/// 解析文件 icon 路径：优先命中缓存，未命中时在路径存在的前提下抽取并落盘缓存。
/// 返回 `(icon_path, exists)`，其中 `icon_path` 可能为 `None`（抽取失败或已删除且无缓存）。
/// 抽取在阻塞线程池里做，必须在 core runtime 里调用。
pub(crate) async fn resolve_file_icon_path(
    pool: &SqlitePool,
    file_icon_store: &FileIconStore,
    path: &str,
    file_types: Option<&str>,
    index: usize,
) -> Result<(Option<String>, bool)> {
    let path_obj = Path::new(path);
    let exists = path_obj.exists();
    let platform = if cfg!(target_os = "macos") {
        Platform::Macos
    } else {
        Platform::Windows
    };

    let is_directory = file_types
        .and_then(|types| types.split(',').nth(index))
        .map(|t| t == "d");

    let cache_key = if is_directory == Some(true) {
        DIR_CACHE_KEY.to_string()
    } else {
        // 路径存在时实时判断（覆盖入库后类型变化的情况）；已删除时按扩展名推断。
        get_icon_cache_key(path_obj)
    };

    // DB 命中后还要确认 icon 文件仍在磁盘上：用户清缓存 / 手动删 file-icons 目录后，
    // 表里的 <hash>.png 映射就成了死引用。缺了就当 miss 重抽。
    if let Some(icon_file) = crate::db::file_icons::get_icon(pool, &cache_key, platform).await? {
        let icon_path = file_icon_store.icon_path(&icon_file);
        if icon_path.exists() {
            return Ok((icon_path.to_str().map(str::to_owned), exists));
        }
    }

    if !exists {
        return Ok((None, false));
    }

    let path_for_extract = path_obj.to_path_buf();
    let png_bytes = tokio::task::spawn_blocking(move || icon_png(&path_for_extract, None))
        .await
        .map_err(|err| AppError::Clipboard(format!("icon extract task join failed: {err}")))?;

    let Some(png) = png_bytes else {
        return Ok((None, exists));
    };

    let icon_file = file_icon_store.store(&png)?;
    crate::db::file_icons::upsert_icon(pool, &cache_key, platform, &icon_file).await?;

    let icon_path = file_icon_store.icon_path(&icon_file);
    Ok((icon_path.to_str().map(str::to_owned), exists))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_paths_are_recognized_case_insensitively() {
        assert!(is_image_path("C:/a/b.PNG"));
        assert!(is_image_path("/a/b.heic"));
        assert!(!is_image_path("/a/b.txt"));
        assert!(!is_image_path("/a/png"));
    }

    #[test]
    fn missing_paths_fall_back_to_stored_file_types() {
        let missing = Path::new("/kwikpaste/missing/dir");

        assert!(resolve_preview_file_is_dir(missing, Some("f,d"), 1));
        assert!(!resolve_preview_file_is_dir(missing, Some("f,d"), 0));
        assert!(!resolve_preview_file_is_dir(missing, None, 0));
        assert_eq!(resolve_preview_file_size(missing, false), None);
        assert_eq!(count_file_paths("a\n\nb\n"), 2);
    }
}
