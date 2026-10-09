//! 写回回环抑制。
//!
//! 应用自身写回剪贴板会触发 OS 监听，进而被当成一次新的
//! 复制再次入库，形成回环。写回前调用 [`WritebackGuard::suppress`] 登记将写入内容的
//! `content_hash`；监听回调读到内容后调用 [`WritebackGuard::should_skip`]，命中则跳过本次入库。
//!
//! 用 `content_hash` 比对而非简单布尔标记：避免「写回事件尚未到达就来了一次真实复制」
//! 误伤真实复制；同时带 TTL 兜底——若写回的内容与剪贴板现状完全相同（OS 可能不发变更事件），
//! 登记的指纹不会永久滞留导致后续同内容复制被吞。HTML/RTF 写回会同时写入纯文本回退，
//! 因此 guard 支持短期登记多个指纹。
//! 图片登记将原始哈希与有界解码的尺寸/RGBA 指纹放在同一条记录，任一命中便消费整条。

use std::io::Cursor;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// 登记的写回指纹在多久内有效。写回后监听事件通常在毫秒级到达，
/// 给足冗余但不至于长到误吞后续的真实复制。
const SUPPRESS_TTL: Duration = Duration::from_secs(2);

// 仅限制回环识别的辅助解码；大图仍按原始哈希写回和采集。
const IMAGE_FINGERPRINT_INPUT_MAX: usize = 20 * 1024 * 1024;
const IMAGE_FINGERPRINT_PIXELS_MAX: u64 = 40_000_000;
const IMAGE_FINGERPRINT_RGBA_MAX: u64 = 160 * 1024 * 1024;
const IMAGE_FINGERPRINT_DECODE_MAX: u64 = 200 * 1024 * 1024;

pub struct WritebackGuard {
    pending: Mutex<Vec<Pending>>,
}

struct Pending {
    content_hash: String,
    image_fingerprint: Option<blake3::Hash>,
    at: Instant,
}

impl Default for WritebackGuard {
    fn default() -> Self {
        Self {
            pending: Mutex::new(Vec::new()),
        }
    }
}

impl WritebackGuard {
    pub fn new() -> Self {
        Self::default()
    }

    /// 写回剪贴板前登记将写入内容的 `content_hash`。
    pub fn suppress(&self, content_hash: String) {
        self.register(content_hash, None);
    }

    /// 图片解码失败或超限时只登记原始哈希，不阻止原样写回。
    pub fn suppress_image(&self, content_hash: String, bytes: &[u8]) {
        self.register(content_hash, image_writeback_fingerprint(bytes));
    }

    fn register(&self, content_hash: String, image_fingerprint: Option<blake3::Hash>) {
        let mut pending = self.pending.lock().expect("writeback guard poisoned");
        pending.retain(|p| p.at.elapsed() <= SUPPRESS_TTL);
        pending.push(Pending {
            content_hash,
            image_fingerprint,
            at: Instant::now(),
        });
    }

    /// 仅在图片写回待消费时比较像素；失败视为未命中，保留原始哈希登记。
    pub fn should_skip_image(&self, bytes: &[u8]) -> bool {
        self.should_skip_image_with(|| image_writeback_fingerprint(bytes))
    }

    /// 解码期间不占用登记锁，消费前重新清理 TTL，避免慢解码延长抑制窗口。
    fn should_skip_image_with(&self, fingerprint: impl FnOnce() -> Option<blake3::Hash>) -> bool {
        {
            let mut pending = self.pending.lock().expect("writeback guard poisoned");
            pending.retain(|p| p.at.elapsed() <= SUPPRESS_TTL);
            if !pending.iter().any(|p| p.image_fingerprint.is_some()) {
                return false;
            }
        }

        let Some(fingerprint) = fingerprint() else {
            return false;
        };
        let mut pending = self.pending.lock().expect("writeback guard poisoned");
        pending.retain(|p| p.at.elapsed() <= SUPPRESS_TTL);
        let Some(index) = pending
            .iter()
            .position(|p| p.image_fingerprint == Some(fingerprint))
        else {
            return false;
        };
        pending.remove(index);
        true
    }

    /// 监听回调判断本次变更是否为自身写回所致：命中登记指纹（且未过期）则返回 `true`
    /// 并消费掉登记；否则返回 `false`。过期的登记顺带清理。
    pub fn should_skip(&self, content_hash: &str) -> bool {
        let mut pending = self.pending.lock().expect("writeback guard poisoned");
        pending.retain(|p| p.at.elapsed() <= SUPPRESS_TTL);

        let Some(index) = pending.iter().position(|p| p.content_hash == content_hash) else {
            return false;
        };
        pending.remove(index);

        true
    }

    /// 单测用：直接塞一条已过期登记。
    #[cfg(test)]
    fn suppress_expired_for_test(&self, content_hash: String) {
        let mut pending = self.pending.lock().expect("writeback guard poisoned");
        pending.push(Pending {
            content_hash,
            image_fingerprint: None,
            at: Instant::now() - SUPPRESS_TTL - Duration::from_millis(1),
        });
    }

    /// 单测用：返回当前登记数量。
    #[cfg(test)]
    fn pending_len_for_test(&self) -> usize {
        let pending = self.pending.lock().expect("writeback guard poisoned");

        pending.len()
    }
}

/// PNG 字节只作辅助识别：读取、解码和 RGBA 转换都有上限，不修改存储哈希。
/// decoder 最多 200 MiB，RGBA 最多 160 MiB；转换时两份缓冲可短期共存。
fn image_writeback_fingerprint(bytes: &[u8]) -> Option<blake3::Hash> {
    if bytes.len() > IMAGE_FINGERPRINT_INPUT_MAX {
        return None;
    }

    let reader = || {
        let mut reader =
            image::ImageReader::with_format(Cursor::new(bytes), image::ImageFormat::Png);
        let mut limits = image::Limits::default();
        limits.max_alloc = Some(IMAGE_FINGERPRINT_DECODE_MAX);
        reader.limits(limits);
        reader
    };
    let (width, height) = reader().into_dimensions().ok()?;
    let pixel_count = u64::from(width).checked_mul(u64::from(height))?;
    if pixel_count > IMAGE_FINGERPRINT_PIXELS_MAX
        || pixel_count.checked_mul(4)? > IMAGE_FINGERPRINT_RGBA_MAX
    {
        return None;
    }

    let pixels = reader().decode().ok()?.into_rgba8();
    let mut hasher = blake3::Hasher::new();
    hasher.update(&width.to_le_bytes());
    hasher.update(&height.to_le_bytes());
    hasher.update(pixels.as_raw());
    Some(hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encoded_image(
        compression: image::codecs::png::CompressionType,
        width: u32,
        height: u32,
        red: u8,
    ) -> Vec<u8> {
        use image::ImageEncoder;
        let mut bytes = Vec::new();
        let pixels = image::RgbaImage::from_pixel(width, height, image::Rgba([red, 10, 20, 255]));
        image::codecs::png::PngEncoder::new_with_quality(
            &mut bytes,
            compression,
            image::codecs::png::FilterType::NoFilter,
        )
        .write_image(
            pixels.as_raw(),
            width,
            height,
            image::ExtendedColorType::Rgba8,
        )
        .unwrap();
        bytes
    }

    #[test]
    fn image_writeback_matches_pixels_after_png_reencoding() {
        use image::codecs::png::CompressionType;
        let original = encoded_image(CompressionType::Fast, 64, 48, 50);
        let rewritten = encoded_image(CompressionType::Best, 64, 48, 50);
        assert_ne!(original, rewritten);
        let guard = WritebackGuard::new();
        guard.suppress_image("original-hash".to_owned(), &original);

        assert!(!guard.should_skip("reencoded-hash"));
        assert!(guard.should_skip_image(&rewritten));
        assert!(!guard.should_skip("original-hash"));
        assert!(!guard.should_skip_image(&rewritten));
    }

    #[test]
    fn raw_match_consumes_the_image_fallback_too() {
        let png = encoded_image(image::codecs::png::CompressionType::Fast, 64, 48, 50);
        let guard = WritebackGuard::new();
        guard.suppress_image("original-hash".to_owned(), &png);

        assert!(guard.should_skip("original-hash"));
        assert!(!guard.should_skip_image_with(|| panic!("consumed fallback must not decode")));
    }

    #[test]
    fn image_match_preserves_other_pending_registrations() {
        use image::codecs::png::CompressionType;
        let first = encoded_image(CompressionType::Fast, 64, 48, 50);
        let second = encoded_image(CompressionType::Fast, 64, 48, 90);
        let guard = WritebackGuard::new();
        guard.suppress_image("first-hash".to_owned(), &first);
        guard.suppress_image("second-hash".to_owned(), &second);

        assert!(guard.should_skip_image(&first));
        assert!(!guard.should_skip("first-hash"));
        assert!(guard.should_skip_image(&second));
        assert!(!guard.should_skip("second-hash"));
    }

    #[test]
    fn image_fallback_distinguishes_dimensions_and_pixels() {
        use image::codecs::png::CompressionType;
        let original = encoded_image(CompressionType::Fast, 64, 48, 50);
        let different_pixels = encoded_image(CompressionType::Fast, 64, 48, 90);
        let different_dimensions = encoded_image(CompressionType::Fast, 48, 64, 50);
        let guard = WritebackGuard::new();
        guard.suppress_image("original-hash".to_owned(), &original);

        assert!(!guard.should_skip_image(&different_pixels));
        assert!(!guard.should_skip_image(&different_dimensions));
        assert!(guard.should_skip("original-hash"));
    }

    #[test]
    fn image_compare_does_not_decode_without_an_active_image_registration() {
        let guard = WritebackGuard::new();
        assert!(!guard.should_skip_image_with(|| panic!("no pending writeback")));
        guard.suppress("text-hash".to_owned());
        assert!(!guard.should_skip_image_with(|| panic!("text is not an image")));

        let png = encoded_image(image::codecs::png::CompressionType::Fast, 64, 48, 50);
        guard.suppress_image("image-hash".to_owned(), &png);
        for pending in guard.pending.lock().unwrap().iter_mut() {
            pending.at = Instant::now() - SUPPRESS_TTL - Duration::from_millis(1);
        }
        assert!(!guard.should_skip_image_with(|| panic!("expired image must not decode")));
        assert!(!guard.should_skip("image-hash"));
    }

    #[test]
    fn image_decode_failure_retains_the_raw_guard() {
        let png = encoded_image(image::codecs::png::CompressionType::Fast, 64, 48, 50);
        let guard = WritebackGuard::new();
        guard.suppress_image("image-hash".to_owned(), &png);
        assert!(!guard.should_skip_image(b"not a png"));
        assert!(guard.should_skip("image-hash"));

        guard.suppress_image("invalid-hash".to_owned(), b"not a png");
        assert!(!guard.should_skip_image_with(|| panic!("failed registration must not decode")));
        assert!(guard.should_skip("invalid-hash"));
    }

    #[test]
    fn oversized_fallback_is_best_effort_and_keeps_raw_suppression() {
        let oversized = vec![0; IMAGE_FINGERPRINT_INPUT_MAX + 1];
        assert!(image_writeback_fingerprint(&oversized).is_none());
        let guard = WritebackGuard::new();
        guard.suppress_image("large-hash".to_owned(), &oversized);
        assert!(!guard.should_skip_image_with(|| panic!("oversized registration must not decode")));
        assert!(guard.should_skip("large-hash"));

        let png = png_header_with_dimensions(8001, 5000);
        assert_eq!(
            image::ImageReader::with_format(Cursor::new(&png), image::ImageFormat::Png)
                .into_dimensions()
                .unwrap(),
            (8001, 5000)
        );
        assert!(image_writeback_fingerprint(&png).is_none());
    }

    /// 合法的 PNG 头声明超限尺寸；无需为测试分配巨大像素缓冲。
    fn png_header_with_dimensions(width: u32, height: u32) -> Vec<u8> {
        let mut png = encoded_image(image::codecs::png::CompressionType::Fast, 1, 1, 50);
        png[16..20].copy_from_slice(&width.to_be_bytes());
        png[20..24].copy_from_slice(&height.to_be_bytes());
        let mut crc = !0_u32;
        for byte in &png[12..29] {
            crc ^= u32::from(*byte);
            for _ in 0..8 {
                crc = (crc >> 1) ^ (0xedb8_8320 & 0_u32.wrapping_sub(crc & 1));
            }
        }
        png[29..33].copy_from_slice(&(!crc).to_be_bytes());
        png
    }

    #[test]
    fn skips_once_then_resets() {
        let guard = WritebackGuard::new();
        guard.suppress("hash-a".to_owned());

        assert!(guard.should_skip("hash-a"));
        // 登记已消费，同内容的下一次（真实复制）不再被吞。
        assert!(!guard.should_skip("hash-a"));
    }

    #[test]
    fn supports_multiple_pending_hashes() {
        let guard = WritebackGuard::new();
        guard.suppress("hash-a".to_owned());
        guard.suppress("hash-b".to_owned());

        assert!(guard.should_skip("hash-a"));
        assert!(guard.should_skip("hash-b"));
        assert_eq!(guard.pending_len_for_test(), 0);
    }

    #[test]
    fn does_not_skip_unrelated_content() {
        let guard = WritebackGuard::new();
        guard.suppress("hash-a".to_owned());

        // 写回事件未到，先来了一次别的真实复制 → 不该被吞，登记仍在。
        assert!(!guard.should_skip("hash-b"));
        assert!(guard.should_skip("hash-a"));
    }

    #[test]
    fn expired_suppression_is_ignored() {
        let guard = WritebackGuard::new();
        guard.suppress_expired_for_test("hash-a".to_owned());

        assert!(!guard.should_skip("hash-a"));
    }
}
