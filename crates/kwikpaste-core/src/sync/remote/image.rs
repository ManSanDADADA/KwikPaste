//! 远程同步的图片压缩（REMOTE.md §6.3）：缩到最长边、libwebp 有损编码，再和无损 WebP 比，取小的。

use anyhow::{anyhow, Context};
use image::codecs::webp::WebPEncoder;
use image::imageops::FilterType;
use image::{ExtendedColorType, ImageEncoder as _};

/// 压缩参数：最长边（不放大）与有损质量。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Compress {
    pub max_side: u32,
    pub quality: u8,
}

/// 自部署服务器的「高质量」与「省流」档；官方服务器的参数由 `policy.imageCompress` 下发。
pub(super) const HIGH: Compress = Compress {
    max_side: 2560,
    quality: 85,
};
pub(super) const SAVER: Compress = Compress {
    max_side: 1920,
    quality: 70,
};

/// 压缩好的 WebP 与它的宽高。
pub(super) struct Compressed {
    pub bytes: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

/// 解码本机原图（受统一解码上限保护），按参数压成 WebP。耗 CPU，调用方放到阻塞线程池里。
pub(super) fn compress(source: &[u8], params: Compress) -> anyhow::Result<Compressed> {
    let image = crate::imaging::from_bytes(source)?
        .decode()
        .context("failed to decode image for sync")?;
    let (width, height) = (image.width(), image.height());
    let longest = width.max(height);
    let image = if longest > params.max_side && params.max_side > 0 {
        let scale = f64::from(params.max_side) / f64::from(longest);
        let target = |side: u32| ((f64::from(side) * scale).round() as u32).max(1);
        image.resize_exact(target(width), target(height), FilterType::CatmullRom)
    } else {
        image
    };
    let rgba = image.to_rgba8();
    let (width, height) = rgba.dimensions();

    let lossy = encode_lossy(rgba.as_raw(), width, height, params.quality)?;
    let mut lossless = Vec::new();
    WebPEncoder::new_lossless(&mut lossless)
        .write_image(rgba.as_raw(), width, height, ExtendedColorType::Rgba8)
        .context("failed to encode lossless webp")?;

    Ok(Compressed {
        bytes: if lossless.len() < lossy.len() {
            lossless
        } else {
            lossy
        },
        width,
        height,
    })
}

/// libwebp 有损编码。`webp` crate 的 `encode` 失败时会 panic，这里自己建配置走返回错误的接口。
fn encode_lossy(rgba: &[u8], width: u32, height: u32, quality: u8) -> anyhow::Result<Vec<u8>> {
    let expected = (width as usize)
        .checked_mul(height as usize)
        .and_then(|pixels| pixels.checked_mul(4));
    if width == 0 || height == 0 || expected != Some(rgba.len()) {
        return Err(anyhow!("invalid image buffer for webp"));
    }
    let mut config =
        webp::WebPConfig::new().map_err(|_| anyhow!("libwebp configuration unavailable"))?;
    config.lossless = 0;
    config.alpha_compression = 1;
    config.quality = f32::from(quality.min(100));
    let encoded = webp::Encoder::from_rgba(rgba, width, height)
        .encode_advanced(&config)
        .map_err(|err| anyhow!("failed to encode webp: {err:?}"))?;
    Ok(encoded.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(width: u32, height: u32, noisy: bool) -> Vec<u8> {
        let image = image::RgbaImage::from_fn(width, height, |x, y| {
            if noisy {
                let v = (x.wrapping_mul(2_654_435_761) ^ y.wrapping_mul(40_503)) as u8;
                image::Rgba([v, v.wrapping_mul(7), v.wrapping_mul(13), 255])
            } else {
                image::Rgba([30, 60, 90, 255])
            }
        });
        let mut out = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(image)
            .write_to(&mut out, image::ImageFormat::Png)
            .unwrap();
        out.into_inner()
    }

    fn lossless_len(rgba: &image::RgbaImage) -> usize {
        let mut out = Vec::new();
        WebPEncoder::new_lossless(&mut out)
            .write_image(
                rgba.as_raw(),
                rgba.width(),
                rgba.height(),
                ExtendedColorType::Rgba8,
            )
            .unwrap();
        out.len()
    }

    #[test]
    fn shrinks_to_max_side_without_upscaling() {
        let params = Compress {
            max_side: 64,
            quality: 70,
        };
        let wide = compress(&png(256, 128, false), params).unwrap();
        assert_eq!((wide.width, wide.height), (64, 32));
        assert!(image::load_from_memory(&wide.bytes).is_ok());

        let small = compress(&png(40, 20, false), params).unwrap();
        assert_eq!((small.width, small.height), (40, 20));
    }

    #[test]
    fn keeps_the_smaller_encoding() {
        let params = Compress {
            max_side: 512,
            quality: 70,
        };
        for noisy in [false, true] {
            let source = png(96, 96, noisy);
            let rgba = image::load_from_memory(&source).unwrap().to_rgba8();
            let lossy = encode_lossy(rgba.as_raw(), 96, 96, 70).unwrap().len();
            let lossless = lossless_len(&rgba);

            let out = compress(&source, params).unwrap();
            assert_eq!(out.bytes.len(), lossy.min(lossless), "noisy = {noisy}");
        }
    }
}
