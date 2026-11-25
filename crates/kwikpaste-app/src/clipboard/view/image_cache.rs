//! `KpImageCache`：列表里剪贴板图片的有界缓存（附录 D §3.3、L7）。
//!
//! - 键是（文件路径，目标物理尺寸）。解码和缩放都在后台线程完成，缓存里只留缩到显示尺寸的位图，
//!   大图（包括单图文件记录指向的原图，附录 D §8 第 7 条）也不会把原图留在内存或图集里。
//! - 默认 LRU 64 项；图标视图可用独立的 128 项实例；淘汰时
//!   `cx.drop_image(image, Some(window))` 把它从窗口的图集里删掉。
//! - 面板隐藏时 [`KpImageCache::clear`] 全部释放。
//! - 解码失败记成 [`ImageState::Failed`]，卡片换成静态的“坏图”图标，骨架不再脉动。
//!
//! 卡片直接问这里要 `Arc<RenderImage>`，再用 `img(ImageSource::Render(..))` 按显式尺寸画出来，
//! 不经过 GPUI 的全局资源缓存，也不在外层套 `image_cache()` 元素（那样同一张图会缓存两份）。

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
};

use gpui::{App, Context, RenderImage, Task, Window};

/// 缓存项上限（约 4 MB：64 张 64×300 级别的 BGRA 位图）。
pub const CAPACITY: usize = 64;

/// 缓存键：同一文件按修改时间、显示尺寸分别缓存。
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ImageKey {
    pub path: Arc<Path>,
    /// 可变原文件的版本；捕获图片和图标为空。
    pub modified: Option<i64>,
    /// 目标宽高（物理像素）。
    pub width: u32,
    pub height: u32,
    /// 缩放方式；图标用等比缩放，避免非方形文件图标变形。
    pub resize: ResizeMode,
}

/// 图片缩放方式。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ResizeMode {
    /// 缩放到目标宽高，供剪贴板缩略图使用。
    Exact,
    /// 等比缩放到目标方框内，供图标使用。
    Contain,
}

/// 一次查询的结果。
#[derive(Clone, Debug)]
pub enum ImageState {
    Loading,
    Ready(Arc<RenderImage>),
    Failed,
}

enum Slot {
    Loading(#[allow(dead_code)] Task<()>),
    Ready(Arc<RenderImage>),
    Failed,
}

struct Entry {
    slot: Slot,
    used: u64,
}

/// 计数，自测和跑分读它。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CacheStats {
    pub entries: usize,
    pub decoded: u64,
    pub failed: u64,
    pub evicted: u64,
}

pub struct KpImageCache {
    entries: HashMap<ImageKey, Entry>,
    clock: u64,
    stats: CacheStats,
    capacity: usize,
}

impl Default for KpImageCache {
    fn default() -> Self {
        Self {
            entries: HashMap::new(),
            clock: 0,
            stats: CacheStats::default(),
            capacity: CAPACITY,
        }
    }
}

impl KpImageCache {
    pub fn new() -> Self {
        Self::with_capacity(CAPACITY)
    }

    /// 创建一个有界图片缓存；图标缓存用 128 项，缩略图仍用默认 64 项。
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            ..Self::default()
        }
    }

    pub fn stats(&self) -> CacheStats {
        CacheStats {
            entries: self.entries.len(),
            ..self.stats
        }
    }

    /// 取一张图。没有时登记后台解码并返回 `Loading`，解码完成后本实体 `notify`。
    pub fn request(
        &mut self,
        key: ImageKey,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> ImageState {
        self.clock += 1;
        if let Some(entry) = self.entries.get_mut(&key) {
            entry.used = self.clock;
            return match &entry.slot {
                Slot::Loading(_) => ImageState::Loading,
                Slot::Ready(image) => ImageState::Ready(image.clone()),
                Slot::Failed => ImageState::Failed,
            };
        }

        if key.width == 0 || key.height == 0 {
            return ImageState::Failed;
        }

        self.evict_for_one(window, cx);
        let task = self.spawn_decode(key.clone(), cx);
        self.entries.insert(
            key,
            Entry {
                slot: Slot::Loading(task),
                used: self.clock,
            },
        );

        ImageState::Loading
    }

    /// 释放全部位图（面板隐藏时）。正在解码的任务随之取消。
    pub fn clear(&mut self, window: Option<&mut Window>, cx: &mut App) {
        let mut window = window;
        for (_, entry) in self.entries.drain() {
            if let Slot::Ready(image) = entry.slot {
                cx.drop_image(image, window.as_deref_mut());
            }
        }
    }

    /// 只留下 `keep` 这一个尺寸：同一文件别的尺寸（预览缩放时换下来的）连同还在解码的一起丢掉，
    /// 解码好的从图集里删掉。
    pub fn drop_other_sizes(&mut self, keep: &ImageKey, window: &mut Window, cx: &mut App) {
        let stale: Vec<ImageKey> = self
            .entries
            .keys()
            .filter(|key| key.path == keep.path && *key != keep)
            .cloned()
            .collect();
        for key in stale {
            if let Some(Entry {
                slot: Slot::Ready(image),
                ..
            }) = self.entries.remove(&key)
            {
                cx.drop_image(image, Some(window));
            }
        }
    }

    fn evict_for_one(&mut self, window: &mut Window, cx: &mut App) {
        if self.entries.len() < self.capacity {
            return;
        }

        let Some(oldest) = self
            .entries
            .iter()
            .min_by_key(|(_, entry)| entry.used)
            .map(|(key, _)| key.clone())
        else {
            return;
        };
        if let Some(entry) = self.entries.remove(&oldest) {
            self.stats.evicted += 1;
            if let Slot::Ready(image) = entry.slot {
                cx.drop_image(image, Some(window));
            }
        }
    }

    fn spawn_decode(&mut self, key: ImageKey, cx: &mut Context<Self>) -> Task<()> {
        let path = key.path.to_path_buf();
        let (width, height) = (key.width, key.height);
        let resize = key.resize;
        let decode = cx.background_executor().spawn(async move {
            match resize {
                ResizeMode::Exact => decode(&path, width, height),
                ResizeMode::Contain => decode_contained(&path, width, height),
            }
        });

        cx.spawn(async move |this, cx| {
            let result = decode.await;
            this.update(cx, |cache, cx| cache.finish(&key, result, cx))
                .ok();
        })
    }

    fn finish(
        &mut self,
        key: &ImageKey,
        result: anyhow::Result<RenderImage>,
        cx: &mut Context<Self>,
    ) {
        let Some(entry) = self.entries.get_mut(key) else {
            return;
        };

        entry.slot = match result {
            Ok(image) => {
                self.stats.decoded += 1;
                Slot::Ready(Arc::new(image))
            }
            Err(err) => {
                self.stats.failed += 1;
                log::warn!("image {} could not be decoded: {err:#}", key.path.display());
                Slot::Failed
            }
        };
        cx.notify();
    }
}

/// 解码并缩放到 `width`×`height`（物理像素），转成 GPUI 要的 BGRA。在后台线程上运行。
/// 解码按 core 的上限（图片可能来自同步或备份导入），超限返回错误。
pub fn decode(path: &Path, width: u32, height: u32) -> anyhow::Result<RenderImage> {
    let pixels = decode_pixels(path, width, height, ResizeMode::Exact)?;
    Ok(RenderImage::new(vec![image::Frame::new(pixels)]))
}

/// 解码、缩放到 `width`×`height`（转之前的方向），再顺时针转 `turns` 个直角；大图预览用。
/// 同时返回有没有不透明度不满的像素（要不要垫棋盘格）。
pub fn decode_rotated(
    path: &Path,
    width: u32,
    height: u32,
    turns: u8,
) -> anyhow::Result<(RenderImage, bool)> {
    let pixels = decode_pixels(path, width, height, ResizeMode::Exact)?;
    let transparent = pixels.pixels().any(|pixel| pixel.0[3] < u8::MAX);
    let pixels = match turns % 4 {
        1 => image::imageops::rotate90(&pixels),
        2 => image::imageops::rotate180(&pixels),
        3 => image::imageops::rotate270(&pixels),
        _ => pixels,
    };
    Ok((
        RenderImage::new(vec![image::Frame::new(pixels)]),
        transparent,
    ))
}

/// 解码并等比缩放到目标方框内，供图标使用。
pub fn decode_contained(path: &Path, width: u32, height: u32) -> anyhow::Result<RenderImage> {
    let pixels = decode_pixels(path, width, height, ResizeMode::Contain)?;
    Ok(RenderImage::new(vec![image::Frame::new(pixels)]))
}

/// Windows 走 WIC 按目标尺寸解码；WebP（WIC 要可选系统扩展）和 ICO（多帧选取规则不同）与 macOS 一样走 crate。
fn decode_pixels(
    path: &Path,
    width: u32,
    height: u32,
    resize: ResizeMode,
) -> anyhow::Result<image::RgbaImage> {
    // 目标尺寸由显示设置推出来，设置文件里的离谱数值不能变成一次超大的缩放分配。
    anyhow::ensure!(
        width > 0 && height > 0,
        "empty target size {width}x{height}"
    );
    kwikpaste_core::imaging::check_rgba_size(width, height)?;
    let reader = kwikpaste_core::imaging::open(path)?;

    #[cfg(target_os = "windows")]
    if !matches!(
        reader.format(),
        Some(image::ImageFormat::WebP | image::ImageFormat::Ico)
    ) {
        let (source_width, source_height) = reader.into_dimensions()?;
        kwikpaste_core::imaging::check_rgba_size(source_width, source_height)?;
        anyhow::ensure!(source_width > 0 && source_height > 0, "empty source image");
        let (width, height) = match resize {
            ResizeMode::Exact => (width, height),
            ResizeMode::Contain => {
                // 与 image::resize 一样四舍五入，并允许把小图放大。
                let ratio = (f64::from(width) / f64::from(source_width))
                    .min(f64::from(height) / f64::from(source_height));
                (
                    (f64::from(source_width) * ratio).round().max(1.) as u32,
                    (f64::from(source_height) * ratio).round().max(1.) as u32,
                )
            }
        };
        let pixels = kwikpaste_os::win::imaging::decode_bgra(path, width, height)?;
        return image::RgbaImage::from_raw(width, height, pixels)
            .ok_or_else(|| anyhow::anyhow!("invalid WIC pixel buffer"));
    }

    let image = reader.decode()?;
    let image = match resize {
        ResizeMode::Exact if image.width() == width && image.height() == height => image,
        ResizeMode::Exact => {
            image.resize_exact(width, height, image::imageops::FilterType::Triangle)
        }
        ResizeMode::Contain => image.resize(width, height, image::imageops::FilterType::Triangle),
    };

    let mut pixels = image.into_rgba8();
    for pixel in pixels.pixels_mut() {
        pixel.0.swap(0, 2);
    }

    Ok(pixels)
}

/// 文件路径转缓存键里的 `Arc<Path>`。
pub fn path_of(text: &str) -> Arc<Path> {
    Arc::from(PathBuf::from(text).as_path())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_image_cache_hits_only_when_the_modification_time_matches() {
        let key = |modified| ImageKey {
            path: path_of("mutable.png"),
            modified,
            width: 8,
            height: 8,
            resize: ResizeMode::Exact,
        };
        let image = Arc::new(RenderImage::new(vec![image::Frame::new(
            image::RgbaImage::new(8, 8),
        )]));
        let mut cache = KpImageCache::new();
        cache.entries.insert(
            key(Some(1000)),
            Entry {
                slot: Slot::Ready(image.clone()),
                used: 0,
            },
        );
        assert!(
            matches!(cache.entries.get(&key(Some(1000))), Some(Entry { slot: Slot::Ready(cached), .. }) if Arc::ptr_eq(cached, &image))
        );
        assert!(!cache.entries.contains_key(&key(Some(2000))));
    }

    #[test]
    fn decode_resizes_and_swaps_to_bgra() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("red.png");
        image::RgbaImage::from_pixel(40, 20, image::Rgba([255, 0, 0, 255]))
            .save(&path)
            .expect("write png");

        let image = decode(&path, 10, 5).expect("decodes");
        assert_eq!(image.size(0).width.0, 10);
        assert_eq!(image.size(0).height.0, 5);
        let bytes = image.as_bytes(0).expect("frame");
        assert_eq!(bytes.get(..4), Some(&[0, 0, 255, 255][..]), "BGRA order");

        assert!(decode(&dir.path().join("missing.png"), 10, 5).is_err());
    }

    #[test]
    fn decode_contained_preserves_aspect_rounding_alpha_and_upscaling() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("transparent.png");
        image::RgbaImage::from_pixel(40, 20, image::Rgba([255, 0, 0, 128]))
            .save(&path)
            .expect("write png");

        for (width, height, expected) in [(10, 10, (10, 5)), (7, 7, (7, 4)), (80, 80, (80, 40))] {
            let image = decode_contained(&path, width, height).expect("contained");
            assert_eq!((image.size(0).width.0, image.size(0).height.0), expected);
            assert!(
                image
                    .as_bytes(0)
                    .expect("frame")
                    .chunks_exact(4)
                    .all(|pixel| pixel == [0, 0, 255, 128]),
                "straight BGRA, not premultiplied"
            );
        }
        let image = decode(&path, 10, 10).expect("exact");
        assert_eq!((image.size(0).width.0, image.size(0).height.0), (10, 10));
    }

    #[test]
    fn decode_supported_formats_by_content() {
        let dir = tempfile::tempdir().expect("temp dir");
        let image = image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            40,
            20,
            image::Rgba([255, 0, 0, 255]),
        ));
        for format in [
            image::ImageFormat::Bmp,
            image::ImageFormat::Gif,
            image::ImageFormat::Ico,
            image::ImageFormat::Jpeg,
            image::ImageFormat::Png,
            image::ImageFormat::Tiff,
            image::ImageFormat::WebP,
        ] {
            let path = dir.path().join(format!("{format:?}.data"));
            image.save_with_format(&path, format).expect("write image");
            let output = decode_contained(&path, 10, 10).expect("decode by content");
            assert_eq!(
                (output.size(0).width.0, output.size(0).height.0),
                (10, 5),
                "{format:?}"
            );
            let pixel = &output.as_bytes(0).expect("frame")[..4];
            assert!(
                pixel[0] < 3 && pixel[1] < 3 && pixel[2] > 252 && pixel[3] == 255,
                "{format:?}: {pixel:?}"
            );
        }
    }
}
