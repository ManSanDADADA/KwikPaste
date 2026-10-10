//! WinRT OCR 的解码、选语言与分条识别都留在 helper 内。
use std::path::Path;

use kwikpaste_ext_protocol::{MAX_TEXT_CHARS, OcrSupport, Outcome};
use windows::{
    Globalization::{ApplicationLanguages, Language},
    Graphics::Imaging::{
        BitmapAlphaMode, BitmapBounds, BitmapDecoder, BitmapPixelFormat, BitmapTransform,
        ColorManagementMode, ExifOrientationMode,
    },
    Media::Ocr::OcrEngine,
    Storage::StorageFile,
    Win32::System::WinRT::{RO_INIT_MULTITHREADED, RoInitialize, RoUninitialize},
    core::HSTRING,
};

pub(super) struct Engine {
    engine: Option<OcrEngine>,
}

impl Engine {
    /// 优先用户配置中的可用 CJK，其次任意已安装 CJK，最后系统语言回退。
    pub fn new() -> anyhow::Result<Self> {
        unsafe {
            RoInitialize(RO_INIT_MULTITHREADED)?;
        }
        let result = Self::select();
        if result.is_err() {
            unsafe {
                RoUninitialize();
            }
        }
        result.map(|engine| Self { engine })
    }

    fn select() -> anyhow::Result<Option<OcrEngine>> {
        let available = OcrEngine::AvailableRecognizerLanguages()?;
        if available.Size()? == 0 {
            return Ok(None);
        }
        for tag in ApplicationLanguages::Languages()? {
            let tag = tag.to_string();
            if is_cjk(&tag) {
                let language = Language::CreateLanguage(&HSTRING::from(tag))?;
                if OcrEngine::IsLanguageSupported(&language)?
                    && let Ok(engine) = OcrEngine::TryCreateFromLanguage(&language)
                {
                    return Ok(Some(engine));
                }
            }
        }
        for language in available {
            if is_cjk(&language.LanguageTag()?.to_string())
                && let Ok(engine) = OcrEngine::TryCreateFromLanguage(&language)
            {
                return Ok(Some(engine));
            }
        }
        Ok(OcrEngine::TryCreateFromUserProfileLanguages().ok())
    }

    pub fn support(&self) -> OcrSupport {
        match self
            .engine
            .as_ref()
            .and_then(|engine| engine.RecognizerLanguage().ok())
            .and_then(|language| language.LanguageTag().ok())
        {
            Some(tag) => OcrSupport::Available {
                languages: vec![tag.to_string()],
            },
            None => OcrSupport::MissingLanguage,
        }
    }

    /// WinRT 从路径直接解码；超过 40 MP 直接跳过，其余逐条处理并立即释放 bitmap。
    pub fn recognize(&mut self, path: &Path) -> anyhow::Result<Outcome> {
        let Some(engine) = &self.engine else {
            return Ok(Outcome::Failed {
                reason: "no OCR language installed".into(),
            });
        };
        let path = std::fs::canonicalize(path)?;
        let path = path
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("invalid image path"))?
            .trim_start_matches(r"\\?\");
        let file = StorageFile::GetFileFromPathAsync(&HSTRING::from(path))?.join()?;
        let stream = file.OpenReadAsync()?.join()?;
        let decoder = BitmapDecoder::CreateAsync(&stream)?.join()?;
        let width = decoder.PixelWidth()?;
        let height = decoder.PixelHeight()?;
        if u64::from(width) * u64::from(height) > 40_000_000 {
            return Ok(Outcome::Skipped {
                reason: "image exceeds 40 MP".into(),
            });
        }
        if width == 0 || height == 0 {
            anyhow::bail!("empty image");
        }
        let max = OcrEngine::MaxImageDimension()?.max(1);
        let scaled_width = width.min(max);
        let scaled_height =
            ((u64::from(height) * u64::from(scaled_width)) / u64::from(width)).max(1) as u32;
        let strips =
            width > max || height > max || (height > width.saturating_mul(2) && height > 2000);
        let target_height = if strips {
            max.min(1600)
        } else {
            scaled_height.min(max)
        };
        let mut y = 0;
        let mut lines: Vec<String> = Vec::new();
        let mut chars = 0;
        let mut overlapped = false;
        while y < scaled_height && chars < MAX_TEXT_CHARS {
            let target_end = (y + target_height).min(scaled_height);
            let cut = if target_end < scaled_height {
                blank_cut(&decoder, scaled_width, scaled_height, y, target_end)?
            } else {
                Some(target_end)
            };
            let end = cut.unwrap_or(target_end);
            let transform = transform(scaled_width, scaled_height, y, end - y)?;
            let bitmap = decoder
                .GetSoftwareBitmapTransformedAsync(
                    BitmapPixelFormat::Bgra8,
                    BitmapAlphaMode::Ignore,
                    &transform,
                    ExifOrientationMode::IgnoreExifOrientation,
                    ColorManagementMode::DoNotColorManage,
                )?
                .join()?;
            let recognized = engine.RecognizeAsync(&bitmap)?.join()?;
            let mut chunk = Vec::new();
            for line in recognized.Lines()? {
                let text = super::normalize_line(&line.Text()?.to_string());
                if !text.is_empty() {
                    chunk.push(text);
                }
            }
            bitmap.Close()?;
            // 无空行可切时重叠 64 px，消掉边界上的完整重复行。
            let overlap = if overlapped {
                (1..=chunk.len().min(lines.len()))
                    .rev()
                    .find(|&count| lines[lines.len() - count..] == chunk[..count])
                    .unwrap_or(0)
            } else {
                0
            };
            for line in chunk.into_iter().skip(overlap) {
                chars += line.chars().count() + 1;
                lines.push(line);
                if chars >= MAX_TEXT_CHARS {
                    break;
                }
            }
            overlapped = cut.is_none() && end < scaled_height && end > y + 64;
            y = if overlapped { end - 64 } else { end };
        }
        stream.Close()?;
        Ok(Outcome::Done {
            text: lines.join("\n").chars().take(MAX_TEXT_CHARS).collect(),
            language: engine.RecognizerLanguage()?.LanguageTag()?.to_string(),
        })
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.engine.take();
        unsafe {
            RoUninitialize();
        }
    }
}

fn is_cjk(tag: &str) -> bool {
    let tag = tag.to_ascii_lowercase();
    tag.starts_with("zh") || tag.starts_with("ja") || tag.starts_with("ko")
}

fn transform(width: u32, height: u32, y: u32, rows: u32) -> anyhow::Result<BitmapTransform> {
    let transform = BitmapTransform::new()?;
    transform.SetScaledWidth(width)?;
    transform.SetScaledHeight(height)?;
    transform.SetBounds(BitmapBounds {
        X: 0,
        Y: y,
        Width: width,
        Height: rows,
    })?;
    Ok(transform)
}

/// 只解码目标边界前的窄带，在近乎纯色的连续空行中切开（支持白底和深色截图）。
fn blank_cut(
    decoder: &BitmapDecoder,
    width: u32,
    height: u32,
    start: u32,
    target: u32,
) -> anyhow::Result<Option<u32>> {
    let top = target.saturating_sub(96).max(start + 1);
    let rows = target - top;
    if rows < 4 {
        return Ok(None);
    }
    let transform = transform(width, height, top, rows)?;
    let pixels = decoder
        .GetPixelDataTransformedAsync(
            BitmapPixelFormat::Bgra8,
            BitmapAlphaMode::Ignore,
            &transform,
            ExifOrientationMode::IgnoreExifOrientation,
            ColorManagementMode::DoNotColorManage,
        )?
        .join()?
        .DetachPixelData()?;
    let mut consecutive = 0;
    let mut cut = None;
    for (row, values) in pixels.chunks_exact(width as usize * 4).enumerate() {
        let values = values.chunks_exact(4).map(|pixel| {
            ((u16::from(pixel[0]) + u16::from(pixel[1]) + u16::from(pixel[2])) / 3) as u8
        });
        let min = values.clone().min().unwrap_or(0);
        let max = values.max().unwrap_or(255);
        if max.saturating_sub(min) <= 12 {
            consecutive += 1;
        } else {
            consecutive = 0;
        }
        if consecutive >= 4 {
            cut = Some(top + row as u32 - 1);
        }
    }
    Ok(cut)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tall_page_preserves_repeated_lines_across_blank_strip_boundaries() {
        let text = image::load_from_memory(include_bytes!("../fixtures/ocr/chinese-english.png"))
            .unwrap()
            .to_rgba8();
        let mut page = image::RgbaImage::from_pixel(1080, 8000, image::Rgba([255, 255, 255, 255]));
        for y in (32..7500).step_by(1000) {
            image::imageops::overlay(&mut page, &text, 32, y);
        }
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("long.png");
        page.save(&path).unwrap();
        let mut engine = Engine::new().unwrap();
        let Outcome::Done { text, .. } = engine.recognize(&path).unwrap() else {
            panic!("OCR did not complete");
        };
        assert_eq!(
            text.to_ascii_lowercase().matches("kwikpaste").count(),
            8,
            "{text}"
        );
    }

    #[test]
    fn recognizes_real_chinese_and_english_fixture() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/ocr/chinese-english.png");
        let mut engine = Engine::new().unwrap();
        let OcrSupport::Available { languages } = engine.support() else {
            panic!("install an OCR language to run this acceptance test");
        };
        let Outcome::Done { text, .. } = engine.recognize(&path).unwrap() else {
            panic!("OCR did not complete");
        };
        assert!(text.to_ascii_lowercase().contains("kwikpaste"), "{text}");
        // 只有装了中文识别时才认得出中文；CI 的 Windows 镜像只带英文。
        if languages.iter().any(|tag| tag.starts_with("zh")) {
            assert!(text.contains("中文"), "{text}");
        }
    }
}
