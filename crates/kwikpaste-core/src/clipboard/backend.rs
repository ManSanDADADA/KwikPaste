//! 系统剪贴板的读写接口。
//!
//! [`ClipboardReader`](super::ClipboardReader) 与写回函数只通过 [`ClipboardBackend`] 碰剪贴板：
//! 正式运行用 [`SystemClipboard`]（clipboard-rs，Windows 写图另走 clipboard-win），
//! 自动测试一律用 [`MemoryClipboard`]，不读写本机真实剪贴板。
//!
//! 实现不要求 `Send`：clipboard-rs 的上下文是 `!Send`，调用方在同一线程的同步段里创建、用完、丢弃。

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use clipboard_rs::common::{RustImage, RustImageData};
use clipboard_rs::{Clipboard, ClipboardContent, ClipboardContext, ContentFormat};

use super::read::png_dimensions;
use crate::error::{AppError, Result};

/// 读取前探测的内容类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipboardFormat {
    Text,
    Html,
    Rtf,
    Image,
    /// 原样存放的 PNG 字节（Windows 的 `PNG`、macOS 的 `public.png`），不含位图等其它图片格式。
    Png,
    Files,
}

/// 一次写入里的一种文本表示；同一次写入的几种表示并存，不互相清空。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClipboardWrite {
    Text(String),
    Html(String),
    Rtf(String),
}

/// 剪贴板上的图片解码后重新编码成的 PNG。宽或高为 0 时 `png` 为空。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedImage {
    pub width: u32,
    pub height: u32,
    pub png: Vec<u8>,
}

/// 读写剪贴板所需的最小操作集合，语义与 clipboard-rs 的同名操作一致：
/// 每次 `set*` 都先清空剪贴板，再写入这一次给出的全部表示。
pub trait ClipboardBackend {
    fn has(&self, format: ClipboardFormat) -> bool;
    fn get_text(&self) -> Result<String>;
    fn get_html(&self) -> Result<String>;
    fn get_rich_text(&self) -> Result<String>;
    fn get_files(&self) -> Result<Vec<String>>;
    /// 平台原生 PNG 表示的原始字节，不解码、不重新编码。
    fn get_png(&self) -> Result<Vec<u8>>;
    /// 把剪贴板上任意格式的图片（TIFF、DIB 等）解码后重新编码为 PNG。
    fn get_image_as_png(&self) -> Result<DecodedImage>;
    fn set_text(&self, text: String) -> Result<()>;
    fn set(&self, contents: Vec<ClipboardWrite>) -> Result<()>;
    fn set_files(&self, files: Vec<String>) -> Result<()>;
    /// 把 PNG 字节原样放上剪贴板；Windows 另附一份 `CF_DIB` 给只认位图的应用。
    fn set_png(&self, png: Vec<u8>) -> Result<()>;
    /// 系统临时目录（已规范化）：别的应用放上剪贴板的这里的单个图片文件按图片收录。
    /// 只有 Windows 的系统剪贴板给出；测试用的内存后端只在状态里指定时才有。
    fn temp_dir(&self) -> Option<PathBuf> {
        None
    }
}

/// 借用的后端照样能用，`ClipboardReader::with_backend(&backend)` 不必转移所有权。
impl<T: ClipboardBackend + ?Sized> ClipboardBackend for &T {
    fn has(&self, format: ClipboardFormat) -> bool {
        (**self).has(format)
    }

    fn get_text(&self) -> Result<String> {
        (**self).get_text()
    }

    fn get_html(&self) -> Result<String> {
        (**self).get_html()
    }

    fn get_rich_text(&self) -> Result<String> {
        (**self).get_rich_text()
    }

    fn get_files(&self) -> Result<Vec<String>> {
        (**self).get_files()
    }

    fn get_png(&self) -> Result<Vec<u8>> {
        (**self).get_png()
    }

    fn get_image_as_png(&self) -> Result<DecodedImage> {
        (**self).get_image_as_png()
    }

    fn set_text(&self, text: String) -> Result<()> {
        (**self).set_text(text)
    }

    fn set(&self, contents: Vec<ClipboardWrite>) -> Result<()> {
        (**self).set(contents)
    }

    fn set_files(&self, files: Vec<String>) -> Result<()> {
        (**self).set_files(files)
    }

    fn set_png(&self, png: Vec<u8>) -> Result<()> {
        (**self).set_png(png)
    }

    fn temp_dir(&self) -> Option<PathBuf> {
        (**self).temp_dir()
    }
}

/// 平台剪贴板里 PNG 的原始格式标识符：读取时 `get_buffer` 直取原始字节，写回时原样放回。
#[cfg(target_os = "macos")]
pub(super) const PNG_FORMAT: &str = "public.png";
#[cfg(target_os = "windows")]
pub(super) const PNG_FORMAT: &str = "PNG";

/// macOS 剪贴板里 TIFF 的格式标识符（截图与多数应用复制的图片）。
#[cfg(target_os = "macos")]
const TIFF_FORMAT: &str = "public.tiff";

/// 解码好的图片重新编码成 PNG。编码在 clipboard-rs 里做：image 的编码是泛型，在哪个 crate 里展开
/// 就跟哪个 crate 的优化级别，clipboard-rs 在根 Cargo.toml 的 opt-level 3 名单上，core 是 z，
/// 在 core 里编码一张 4K 图要慢好几倍。
fn encode_image(image: RustImageData) -> Result<DecodedImage> {
    let (width, height) = image.get_size();
    if width == 0 || height == 0 {
        return Ok(DecodedImage {
            width,
            height,
            png: Vec::new(),
        });
    }

    let png = image.to_png().map_err(clip_err)?.get_bytes().to_vec();
    Ok(DecodedImage { width, height, png })
}

/// 本机系统剪贴板。`!Send`，在使用它的线程上创建。
pub struct SystemClipboard {
    ctx: ClipboardContext,
}

impl SystemClipboard {
    pub fn new() -> Result<Self> {
        let ctx = ClipboardContext::new().map_err(clip_err)?;
        Ok(Self { ctx })
    }
}

impl ClipboardBackend for SystemClipboard {
    fn has(&self, format: ClipboardFormat) -> bool {
        self.ctx.has(match format {
            ClipboardFormat::Text => ContentFormat::Text,
            ClipboardFormat::Html => ContentFormat::Html,
            ClipboardFormat::Rtf => ContentFormat::Rtf,
            ClipboardFormat::Image => ContentFormat::Image,
            ClipboardFormat::Png => ContentFormat::Other(PNG_FORMAT.to_owned()),
            ClipboardFormat::Files => ContentFormat::Files,
        })
    }

    fn get_text(&self) -> Result<String> {
        self.ctx.get_text().map_err(clip_err)
    }

    #[cfg(target_os = "macos")]
    fn get_html(&self) -> Result<String> {
        self.ctx.get_html().map_err(clip_err)
    }

    /// clipboard-rs 按 CF_HTML 头里的字节偏移切字符串，偏移落在多字节字符中间（按字符或 UTF-16
    /// 计数的写法）就会 panic：这里取原始数据，自己按偏移安全地切。
    #[cfg(target_os = "windows")]
    fn get_html(&self) -> Result<String> {
        let data = self
            .ctx
            .get_buffer(windows_html::FORMAT)
            .map_err(clip_err)?;
        let data = String::from_utf8(data).map_err(clip_err)?;
        windows_html::extract(&data)
            .map(str::to_owned)
            .ok_or_else(|| AppError::Clipboard("invalid CF_HTML offsets".to_owned()))
    }

    fn get_rich_text(&self) -> Result<String> {
        self.ctx.get_rich_text().map_err(clip_err)
    }

    fn get_files(&self) -> Result<Vec<String>> {
        self.ctx.get_files().map_err(clip_err)
    }

    /// 规范化后才能和剪贴板里的长路径比较：临时目录常是 8.3 短路径，大小写也可能不同。
    #[cfg(target_os = "windows")]
    fn temp_dir(&self) -> Option<PathBuf> {
        std::fs::canonicalize(std::env::temp_dir()).ok()
    }

    fn get_png(&self) -> Result<Vec<u8>> {
        self.ctx.get_buffer(PNG_FORMAT).map_err(clip_err)
    }

    /// clipboard-rs 的 `get_image` 解 `CF_DIBV5` 时不设任何上限：这里自己取原始数据，按
    /// [`crate::imaging`] 的上限解码。
    #[cfg(target_os = "windows")]
    fn get_image_as_png(&self) -> Result<DecodedImage> {
        encode_image(windows_png::read_image()?)
    }

    /// 原始 PNG / TIFF 先只读图片头、按 [`crate::imaging`] 的上限核对宽高与缓冲，再交给 clipboard-rs
    /// 解（解码是泛型，在它那里展开才跑 opt-level 3；它用 image 的默认上限，分配不超过 512 MiB）。
    /// 都没有时才让 clipboard-rs 经 NSImage 转出 TIFF 再解，解出来超过宽高上限的照样不要。
    #[cfg(target_os = "macos")]
    fn get_image_as_png(&self) -> Result<DecodedImage> {
        for format in [PNG_FORMAT, TIFF_FORMAT] {
            let Ok(bytes) = self.ctx.get_buffer(format) else {
                continue;
            };
            if bytes.is_empty() {
                continue;
            }
            let (width, height) = crate::imaging::from_bytes(&bytes)
                .and_then(image::ImageReader::into_dimensions)
                .map_err(clip_err)?;
            crate::imaging::check_rgba_size(width, height).map_err(clip_err)?;
            return encode_image(RustImageData::from_bytes(&bytes).map_err(clip_err)?);
        }

        let image = self.ctx.get_image().map_err(clip_err)?;
        let (width, height) = image.get_size();
        if width > 0 && height > 0 {
            crate::imaging::check_rgba_size(width, height).map_err(clip_err)?;
        }
        encode_image(image)
    }

    fn set_text(&self, text: String) -> Result<()> {
        self.ctx.set_text(text).map_err(clip_err)
    }

    fn set(&self, contents: Vec<ClipboardWrite>) -> Result<()> {
        let contents = contents
            .into_iter()
            .map(|content| match content {
                ClipboardWrite::Text(text) => ClipboardContent::Text(text),
                ClipboardWrite::Html(html) => ClipboardContent::Html(html),
                ClipboardWrite::Rtf(rtf) => ClipboardContent::Rtf(rtf),
            })
            .collect();
        self.ctx.set(contents).map_err(clip_err)
    }

    fn set_files(&self, files: Vec<String>) -> Result<()> {
        self.ctx.set_files(files).map_err(clip_err)
    }

    #[cfg(target_os = "macos")]
    fn set_png(&self, png: Vec<u8>) -> Result<()> {
        self.ctx
            .set(vec![ClipboardContent::Other(PNG_FORMAT.to_owned(), png)])
            .map_err(clip_err)
    }

    /// 解码放在打开剪贴板之前，占用剪贴板只做两次拷贝；系统会按需从 `CF_DIB`
    /// 合成 `CF_BITMAP` / `CF_DIBV5`。
    #[cfg(target_os = "windows")]
    fn set_png(&self, png: Vec<u8>) -> Result<()> {
        let dib = windows_png::png_to_dib(&png)?;

        let _clipboard = windows_png::open_clipboard()?;
        clipboard_win::empty().map_err(clip_err)?;
        let png_format = clipboard_win::register_format(PNG_FORMAT)
            .ok_or_else(|| AppError::Clipboard("PNG clipboard format unavailable".to_owned()))?;
        clipboard_win::raw::set_without_clear(png_format.get(), &png).map_err(clip_err)?;
        clipboard_win::raw::set_without_clear(clipboard_win::formats::CF_DIB, &dib)
            .map_err(clip_err)?;
        Ok(())
    }
}

/// CF_HTML（`HTML Format`）：头部几行 `键:十进制字节偏移`，`StartHTML` / `EndHTML` 圈出 HTML 本体。
#[cfg(target_os = "windows")]
mod windows_html {
    pub(super) const FORMAT: &str = "HTML Format";

    /// 优先取 HTML 上下文；上下文偏移均为 -1 时取片段。已识别的传输头不能作为正文返回，
    /// 缺少偏移、越界、倒置或不在字符边界上时返回 `None`。
    pub(super) fn extract(data: &str) -> Option<&str> {
        let (mut start, mut end, mut fragment_start, mut fragment_end) = (None, None, None, None);
        let mut header_end = 0;
        for line in data.split_inclusive('\n') {
            let Some((key, value)) = line.trim_end().split_once(':') else {
                break;
            };
            let target = match key {
                "StartHTML" => &mut start,
                "EndHTML" => &mut end,
                "StartFragment" => &mut fragment_start,
                "EndFragment" => &mut fragment_end,
                "Version" | "SourceURL" | "StartSelection" | "EndSelection" => {
                    header_end += line.len();
                    continue;
                }
                _ => break,
            };
            *target = Some(value.trim().parse::<i64>().ok()?);
            header_end += line.len();
        }
        if header_end == 0 {
            return Some(data);
        }

        let (start, end) = match (start?, end?) {
            (-1, -1) => (fragment_start?, fragment_end?),
            offsets => offsets,
        };
        let start = usize::try_from(start).ok()?;
        let end = usize::try_from(end).ok()?;
        if start < header_end {
            return None;
        }

        data.get(start..end)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn extracts_the_html_between_the_offsets() {
            let body = "<html><body>你好</body></html>";
            let header_len = "Version:0.9\r\nStartHTML:0000000000\r\nEndHTML:0000000000\r\n".len();
            let data = format!(
                "Version:0.9\r\nStartHTML:{header_len:010}\r\nEndHTML:{:010}\r\n{body}",
                header_len + body.len()
            );
            assert_eq!(extract(&data), Some(body));
        }

        #[test]
        fn extracts_fragment_when_html_context_is_absent() {
            let body = "<b>你好 Hello</b>";
            let header = "Version:1.0\r\nStartHTML:-1\r\nEndHTML:-1\r\nStartFragment:0000000000\r\nEndFragment:0000000000\r\n";
            let data = format!(
                "Version:1.0\r\nStartHTML:-1\r\nEndHTML:-1\r\nStartFragment:{:010}\r\nEndFragment:{:010}\r\n{body}\0",
                header.len(),
                header.len() + body.len()
            );
            assert_eq!(extract(&data), Some(body));

            for (start, end) in [
                (0, header.len()),
                (header.len(), data.len() + 1),
                (header.len() + 1, header.len()),
                (header.len() + 4, header.len() + body.len()),
            ] {
                let invalid = format!(
                    "Version:1.0\r\nStartHTML:-1\r\nEndHTML:-1\r\nStartFragment:{start:010}\r\nEndFragment:{end:010}\r\n{body}"
                );
                assert_eq!(extract(&invalid), None);
            }
        }

        #[test]
        fn invalid_transport_headers_are_not_returned_as_html() {
            for data in [
                "Version:1.0\r\nStartHTML:-1\r\nEndHTML:-1\r\n<b>Hello</b>",
                "Version:1.0\r\nStartHTML:invalid\r\nEndHTML:999\r\n<b>Hello</b>",
                "Version:1.0\r\n<b>Hello</b>",
                "StartHTML:0\r\nEndHTML:0\r\n<b>Hello</b>",
                "StartHTML:-2\r\nEndHTML:-2\r\nStartFragment:0\r\nEndFragment:0\r\n",
            ] {
                assert_eq!(extract(data), None);
            }
        }

        #[test]
        fn offsets_inside_a_character_or_out_of_range_are_rejected() {
            // 头部 42 字节，「你」占 42..45：从 43 开始切在字符中间。
            let data = "StartHTML:0000000043\r\nEndHTML:0000000048\r\n你好";
            assert_eq!(extract(data), None);
            assert_eq!(extract("StartHTML:0000000099\r\n<b>x</b>"), None);
            assert_eq!(
                extract("StartHTML:0000000010\r\nEndHTML:0000000005\r\n"),
                None
            );
            assert_eq!(extract("<b>x</b>"), Some("<b>x</b>"));
        }
    }
}

#[cfg(target_os = "windows")]
mod windows_png {
    use clipboard_rs::common::{RustImage, RustImageData};

    use super::clip_err;
    use crate::error::{AppError, Result};

    /// 打开剪贴板失败后的退避间隔。别的剪贴板监听程序可能正读着上一份（大图）内容，
    /// `new_attempts` 自带的重试只让出时间片、几微秒就耗尽，与读取侧一样给一段有界的等待。
    const OPEN_RETRY_DELAYS: [std::time::Duration; 3] = [
        std::time::Duration::from_millis(15),
        std::time::Duration::from_millis(35),
        std::time::Duration::from_millis(75),
    ];

    pub(super) fn open_clipboard() -> Result<clipboard_win::Clipboard> {
        let mut result = clipboard_win::Clipboard::new_attempts(10);
        for delay in OPEN_RETRY_DELAYS {
            if result.is_ok() {
                break;
            }
            std::thread::sleep(delay);
            result = clipboard_win::Clipboard::new_attempts(10);
        }
        result.map_err(clip_err)
    }

    /// `BITMAPINFOHEADER` 的字节长度。
    pub(super) const DIB_HEADER_LEN: usize = 40;

    /// `BITMAPINFOHEADER` 的 `biCompression`：未压缩、按位掩码。
    const BI_RGB: u32 = 0;
    const BI_BITFIELDS: u32 = 3;

    /// 读剪贴板上的图片并按 [`crate::imaging`] 的上限解码。格式的先后与 clipboard-rs 相同：
    /// 注册格式 `PNG`，再 `CF_DIBV5`、`CF_DIB`（系统会在几种位图格式之间互相合成）。
    pub(super) fn read_image() -> Result<RustImageData> {
        use clipboard_win::formats::{RawData, CF_DIB, CF_DIBV5};

        if let Some(format) = clipboard_win::register_format(super::PNG_FORMAT) {
            if clipboard_win::is_format_avail(format.get()) {
                let png: Vec<u8> =
                    clipboard_win::get_clipboard(RawData(format.get())).map_err(clip_err)?;
                return crate::imaging::from_bytes(&png)
                    .and_then(image::ImageReader::decode)
                    .map(RustImageData::from_dynamic_image)
                    .map_err(clip_err);
            }
        }
        for format in [CF_DIBV5, CF_DIB] {
            if clipboard_win::is_format_avail(format) {
                let dib: Vec<u8> =
                    clipboard_win::get_clipboard(RawData(format)).map_err(clip_err)?;
                return decode_dib(&dib);
            }
        }
        Err(AppError::Clipboard("no image data in clipboard".to_owned()))
    }

    /// 截图等常见的 DIB（24/32 位，未压缩或位掩码，40/108/124 字节的头）：返回宽、高和像素数据
    /// 在 DIB 里的起点。起点的算法与 image 解无文件头 DIB 时一致：头之后，`BI_BITFIELDS` 再跳过
    /// 3 个掩码。其余格式返回 `None`。
    pub(super) fn common_dib_layout(dib: &[u8]) -> Option<(u32, u32, u32)> {
        let u32_at = |offset: usize| -> Option<u32> {
            Some(u32::from_le_bytes(
                dib.get(offset..offset + 4)?.try_into().ok()?,
            ))
        };
        let header_len = u32_at(0)?;
        if !matches!(header_len, 40 | 108 | 124) || dib.len() < header_len as usize {
            return None;
        }
        let width = u32_at(4)? as i32;
        let height = u32_at(8)? as i32;
        let bit_count = u16::from_le_bytes(dib.get(14..16)?.try_into().ok()?);
        let compression = u32_at(16)?;
        if width <= 0 || height == 0 || !matches!(bit_count, 24 | 32) {
            return None;
        }
        let masks = match compression {
            BI_RGB => 0,
            BI_BITFIELDS => 12,
            _ => return None,
        };
        Some((
            width.unsigned_abs(),
            height.unsigned_abs(),
            header_len + masks,
        ))
    }

    /// 解码 `CF_DIB` / `CF_DIBV5`（没有 `BITMAPFILEHEADER` 的 BMP），按 [`crate::imaging`] 的上限。
    ///
    /// 常见布局先从头里读宽高、核对上限，再补上文件头交给 clipboard-rs 解：image 的解码是泛型
    /// （`load_from_memory` 也是 `inline(always)`），在 clipboard-rs 里展开才跑 opt-level 3，
    /// 在 core 里解一张 4K 截图要慢 5 倍以上。clipboard-rs 用 image 的默认上限（分配不超过 512 MiB）。
    /// 少见的格式（调色板、RLE、旧式头）在 core 里按上限解。
    pub(super) fn decode_dib(dib: &[u8]) -> Result<RustImageData> {
        let Some((width, height, data_offset)) = common_dib_layout(dib) else {
            let decoder =
                image::codecs::bmp::BmpDecoder::new_without_file_header(std::io::Cursor::new(dib))
                    .map_err(clip_err)?;
            return crate::imaging::decode(decoder)
                .map(RustImageData::from_dynamic_image)
                .map_err(clip_err);
        };

        crate::imaging::check_rgba_size(width, height).map_err(clip_err)?;
        let file_len = u32::try_from(14 + dib.len())
            .map_err(|_| AppError::Clipboard("the bitmap is too large".to_owned()))?;
        let mut file = Vec::with_capacity(14 + dib.len());
        file.extend_from_slice(b"BM");
        file.extend_from_slice(&file_len.to_le_bytes());
        file.extend_from_slice(&[0; 4]);
        file.extend_from_slice(&(14 + data_offset).to_le_bytes());
        file.extend_from_slice(dib);
        RustImageData::from_bytes(&file).map_err(clip_err)
    }

    /// 解码 PNG 为 `CF_DIB` 数据：`BITMAPINFOHEADER` + 自下而上的 32 位 BGRA 行（`BI_RGB`）。
    /// PNG 可能来自同步或备份导入，解码按 [`crate::imaging`] 的上限。
    pub(super) fn png_to_dib(png: &[u8]) -> Result<Vec<u8>> {
        let rgba = crate::imaging::from_bytes(png)
            .and_then(image::ImageReader::decode)
            .map_err(clip_err)?
            .into_rgba8();
        let (width, height) = rgba.dimensions();
        if width == 0 || height == 0 {
            return Err(AppError::Clipboard("the image is empty".to_owned()));
        }
        let row_len = width as usize * 4;
        let pixels_len = row_len * height as usize;

        let mut dib = Vec::with_capacity(DIB_HEADER_LEN + pixels_len);
        dib.extend_from_slice(&(DIB_HEADER_LEN as u32).to_le_bytes());
        dib.extend_from_slice(&(width as i32).to_le_bytes());
        // 正高度表示行自下而上存放。
        dib.extend_from_slice(&(height as i32).to_le_bytes());
        dib.extend_from_slice(&1u16.to_le_bytes());
        dib.extend_from_slice(&32u16.to_le_bytes());
        dib.extend_from_slice(&0u32.to_le_bytes());
        dib.extend_from_slice(&(pixels_len as u32).to_le_bytes());
        // 分辨率、调色板计数：全 0。
        dib.extend_from_slice(&[0; 16]);

        for row in rgba.as_raw().chunks_exact(row_len).rev() {
            for pixel in row.chunks_exact(4) {
                dib.extend_from_slice(&[pixel[2], pixel[1], pixel[0], pixel[3]]);
            }
        }
        Ok(dib)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn png_to_dib_writes_bottom_up_bgra() {
            // 2×2：上排红、绿，下排蓝、半透明白。
            let image = image::RgbaImage::from_raw(
                2,
                2,
                vec![
                    255, 0, 0, 255, 0, 255, 0, 255, //
                    0, 0, 255, 255, 255, 255, 255, 128,
                ],
            )
            .unwrap();
            let mut png = Vec::new();
            image::DynamicImage::ImageRgba8(image)
                .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
                .unwrap();

            let dib = png_to_dib(&png).unwrap();

            assert_eq!(dib.len(), DIB_HEADER_LEN + 16);
            assert_eq!(u32::from_le_bytes(dib[0..4].try_into().unwrap()), 40);
            assert_eq!(i32::from_le_bytes(dib[4..8].try_into().unwrap()), 2);
            assert_eq!(i32::from_le_bytes(dib[8..12].try_into().unwrap()), 2);
            assert_eq!(u16::from_le_bytes(dib[14..16].try_into().unwrap()), 32);
            assert_eq!(
                &dib[DIB_HEADER_LEN..],
                &[
                    255, 0, 0, 255, 255, 255, 255, 128, // 下排：蓝、半透明白
                    0, 0, 255, 255, 0, 255, 0, 255, // 上排：红、绿
                ]
            );
        }

        fn sample_png(width: u32, height: u32) -> Vec<u8> {
            let image = image::RgbaImage::from_fn(width, height, |x, y| {
                image::Rgba([(x * 40) as u8, (y * 60) as u8, 7, 255])
            });
            let mut png = Vec::new();
            image::DynamicImage::ImageRgba8(image)
                .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
                .unwrap();
            png
        }

        /// 40 字节头的 DIB 改写成 124 字节头（V5）+ `BI_BITFIELDS` + 头后 3 个掩码，像素不动。
        fn as_v5_bitfields(dib: &[u8]) -> Vec<u8> {
            let mut header = dib[..DIB_HEADER_LEN].to_vec();
            header[0..4].copy_from_slice(&124u32.to_le_bytes());
            header[16..20].copy_from_slice(&BI_BITFIELDS.to_le_bytes());
            let masks: [u32; 4] = [0x00ff_0000, 0x0000_ff00, 0x0000_00ff, 0xff00_0000];
            for mask in masks {
                header.extend_from_slice(&mask.to_le_bytes());
            }
            header.resize(124, 0);
            for mask in &masks[..3] {
                header.extend_from_slice(&mask.to_le_bytes());
            }
            header.extend_from_slice(&dib[DIB_HEADER_LEN..]);
            header
        }

        #[test]
        fn dibs_decode_through_both_paths_alike() {
            let png = sample_png(5, 3);
            let expected = image::load_from_memory(&png).unwrap().into_rgba8();
            let dib = png_to_dib(&png).unwrap();
            let v5 = as_v5_bitfields(&dib);

            assert_eq!(common_dib_layout(&dib), Some((5, 3, 40)));
            assert_eq!(common_dib_layout(&v5), Some((5, 3, 136)));
            for dib in [&dib, &v5] {
                let fast = decode_dib(dib)
                    .unwrap()
                    .get_dynamic_image()
                    .unwrap()
                    .into_rgba8();
                assert_eq!(fast, expected);
                let generic = image::codecs::bmp::BmpDecoder::new_without_file_header(
                    std::io::Cursor::new(dib.as_slice()),
                )
                .unwrap();
                let generic = image::DynamicImage::from_decoder(generic)
                    .unwrap()
                    .into_rgba8();
                assert_eq!(fast, generic);
            }
        }

        #[test]
        fn dibs_claiming_huge_sizes_fail_without_allocating() {
            let mut dib = png_to_dib(&sample_png(2, 2)).unwrap();
            dib[4..8].copy_from_slice(&100_000i32.to_le_bytes());
            dib[8..12].copy_from_slice(&(-100_000i32).to_le_bytes());
            assert!(decode_dib(&dib).is_err());

            // 少见格式（8 位调色板）走 image 的无文件头解码，同样按上限。
            dib[14..16].copy_from_slice(&8u16.to_le_bytes());
            assert_eq!(common_dib_layout(&dib), None);
            assert!(decode_dib(&dib).is_err());

            assert!(decode_dib(&[]).is_err());
            assert!(decode_dib(&[40, 0, 0]).is_err());
        }
    }
}

/// 每次需要读写剪贴板时打开一个后端。`Core` 的高层读写都经它拿后端，测试换成 [`MemoryClipboard`]。
///
/// 返回的后端只在打开它的线程上用完即弃（系统剪贴板句柄是 `!Send`）。
pub trait ClipboardProvider: Send + Sync + 'static {
    fn open(&self) -> Result<Box<dyn ClipboardBackend>>;
}

/// 本机系统剪贴板的提供者。
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClipboardProvider;

impl ClipboardProvider for SystemClipboardProvider {
    fn open(&self) -> Result<Box<dyn ClipboardBackend>> {
        Ok(Box::new(SystemClipboard::new()?))
    }
}

/// 内存里的假剪贴板：自动测试和自测入口用它代替本机剪贴板。
///
/// 行为按真实剪贴板建模：每次写入先清空，再放入这次给出的表示；图片只保存 PNG 字节，
/// `get_png` 原样返回，`get_image_as_png` 从 PNG 头读出宽高。克隆出的实例共享同一份内容，
/// 所以它本身也是一个 [`ClipboardProvider`]。
#[derive(Debug, Default, Clone)]
pub struct MemoryClipboard {
    state: Arc<Mutex<MemoryState>>,
}

impl ClipboardProvider for MemoryClipboard {
    fn open(&self) -> Result<Box<dyn ClipboardBackend>> {
        Ok(Box::new(self.clone()))
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MemoryState {
    pub text: Option<String>,
    pub html: Option<String>,
    pub rtf: Option<String>,
    pub files: Option<Vec<String>>,
    pub png: Option<Vec<u8>>,
    /// 剪贴板声明有文件格式，但读取时报错（模拟被占用或格式损坏）。
    pub files_unreadable: bool,
    /// 模拟的系统临时目录，见 [`ClipboardBackend::temp_dir`]。
    pub temp_dir: Option<PathBuf>,
}

impl MemoryClipboard {
    pub fn new() -> Self {
        Self::default()
    }

    /// 用给定内容初始化，模拟别的应用刚复制了这些表示。
    pub fn with_state(state: MemoryState) -> Self {
        Self {
            state: Arc::new(Mutex::new(state)),
        }
    }

    /// 当前内容的快照，测试据此断言写回了什么。
    pub fn snapshot(&self) -> MemoryState {
        self.lock().clone()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, MemoryState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn replace(&self, next: MemoryState) {
        *self.lock() = next;
    }
}

impl ClipboardBackend for MemoryClipboard {
    fn has(&self, format: ClipboardFormat) -> bool {
        let state = self.lock();
        match format {
            ClipboardFormat::Text => state.text.is_some(),
            ClipboardFormat::Html => state.html.is_some(),
            ClipboardFormat::Rtf => state.rtf.is_some(),
            ClipboardFormat::Image | ClipboardFormat::Png => state.png.is_some(),
            ClipboardFormat::Files => state.files.is_some() || state.files_unreadable,
        }
    }

    fn get_text(&self) -> Result<String> {
        self.lock().text.clone().ok_or_else(|| missing("text"))
    }

    fn get_html(&self) -> Result<String> {
        self.lock().html.clone().ok_or_else(|| missing("html"))
    }

    fn get_rich_text(&self) -> Result<String> {
        self.lock().rtf.clone().ok_or_else(|| missing("rtf"))
    }

    fn get_files(&self) -> Result<Vec<String>> {
        let state = self.lock();
        if state.files_unreadable {
            return Err(AppError::Clipboard("get files failed".to_owned()));
        }
        state.files.clone().ok_or_else(|| missing("files"))
    }

    fn temp_dir(&self) -> Option<PathBuf> {
        self.lock().temp_dir.clone()
    }

    fn get_png(&self) -> Result<Vec<u8>> {
        self.lock().png.clone().ok_or_else(|| missing("png"))
    }

    fn get_image_as_png(&self) -> Result<DecodedImage> {
        let png = self.get_png()?;
        let (width, height) = png_dimensions(&png).ok_or_else(|| missing("image"))?;
        Ok(DecodedImage { width, height, png })
    }

    fn set_text(&self, text: String) -> Result<()> {
        self.replace(MemoryState {
            text: Some(text),
            ..MemoryState::default()
        });
        Ok(())
    }

    fn set(&self, contents: Vec<ClipboardWrite>) -> Result<()> {
        let mut next = MemoryState::default();
        for content in contents {
            match content {
                ClipboardWrite::Text(text) => next.text = Some(text),
                ClipboardWrite::Html(html) => next.html = Some(html),
                ClipboardWrite::Rtf(rtf) => next.rtf = Some(rtf),
            }
        }
        self.replace(next);
        Ok(())
    }

    fn set_files(&self, files: Vec<String>) -> Result<()> {
        self.replace(MemoryState {
            files: Some(files),
            ..MemoryState::default()
        });
        Ok(())
    }

    fn set_png(&self, png: Vec<u8>) -> Result<()> {
        self.replace(MemoryState {
            png: Some(png),
            ..MemoryState::default()
        });
        Ok(())
    }
}

fn missing(format: &str) -> AppError {
    AppError::Clipboard(format!("clipboard has no {format} content"))
}

pub(super) fn clip_err<E: std::fmt::Display>(err: E) -> AppError {
    AppError::Clipboard(err.to_string())
}
