//! Vision 仅在 helper 中使用；保持 macOS 10.15 可用的 selector 集合。
use kwikpaste_ext_protocol::{MAX_TEXT_CHARS, OcrSupport, Outcome};
use objc2::{AnyThread, rc::autoreleasepool, runtime::AnyClass};
use objc2_core_foundation::CFURL;
use objc2_core_graphics::CGImage;
use objc2_foundation::{NSArray, NSDictionary, NSString};
use objc2_image_io::CGImageSource;
use objc2_vision::{
    VNImageRequestHandler, VNRecognizeTextRequest, VNRequest, VNRequestTextRecognitionLevel,
};
use std::path::Path;

pub(super) struct Engine {
    languages: Vec<String>,
}

impl Engine {
    /// 先检查类和旧版语言查询 selector；10.15 上不调用新增的实例方法。
    pub fn new() -> anyhow::Result<Self> {
        // 只在 helper 打开 Vision；不依赖主程序启动时的框架装载。
        let vision = unsafe {
            libc::dlopen(
                c"/System/Library/Frameworks/Vision.framework/Vision".as_ptr(),
                libc::RTLD_LAZY | libc::RTLD_LOCAL,
            )
        };
        if vision.is_null() {
            anyhow::bail!("Vision framework unavailable");
        }
        let class = AnyClass::get(c"VNRecognizeTextRequest")
            .ok_or_else(|| anyhow::anyhow!("Vision text recognition unavailable"))?;
        let supported: bool = unsafe {
            objc2::msg_send![class, respondsToSelector: objc2::sel!(supportedRecognitionLanguagesForTextRecognitionLevel:revision:error:)]
        };
        if !supported {
            anyhow::bail!("Vision language query unavailable");
        }
        autoreleasepool(|_| {
            let request = VNRecognizeTextRequest::new();
            #[allow(deprecated)]
            let available = unsafe { VNRecognizeTextRequest::supportedRecognitionLanguagesForTextRecognitionLevel_revision_error(VNRequestTextRecognitionLevel::Accurate, request.revision()) }
                .map_err(|err| anyhow::anyhow!("{err}"))?;
            let available: Vec<String> = available.iter().map(|tag| tag.to_string()).collect();
            let languages = ["zh-Hans", "zh-Hant", "en-US"]
                .into_iter()
                .filter(|tag| available.iter().any(|available| available == tag))
                .map(str::to_owned)
                .collect();
            Ok(Self { languages })
        })
    }

    pub fn support(&self) -> OcrSupport {
        if self.languages.is_empty() {
            OcrSupport::Unsupported
        } else {
            OcrSupport::Available {
                languages: self.languages.clone(),
            }
        }
    }

    /// ImageIO 持有惰性 CGImage，Vision 自行解码；每张图的 autoreleasepool 在返回时释放。
    pub fn recognize(&mut self, path: &Path) -> anyhow::Result<Outcome> {
        autoreleasepool(|_| {
            if self.languages.is_empty() {
                anyhow::bail!("no supported Vision recognition language");
            }
            let url =
                CFURL::from_file_path(path).ok_or_else(|| anyhow::anyhow!("invalid image URL"))?;
            let source = unsafe { CGImageSource::with_url(&url, None) }
                .ok_or_else(|| anyhow::anyhow!("cannot open image"))?;
            let image = unsafe { source.image_at_index(0, None) }
                .ok_or_else(|| anyhow::anyhow!("cannot load image"))?;
            if CGImage::width(Some(&image)).saturating_mul(CGImage::height(Some(&image)))
                > 40_000_000
            {
                return Ok(Outcome::Skipped {
                    reason: "image exceeds 40 MP".into(),
                });
            }
            let request = VNRecognizeTextRequest::new();
            request.setRecognitionLevel(VNRequestTextRecognitionLevel::Accurate);
            request.setUsesLanguageCorrection(true);
            let languages: Vec<_> = self
                .languages
                .iter()
                .map(|tag| NSString::from_str(tag))
                .collect();
            request.setRecognitionLanguages(&NSArray::from_retained_slice(&languages));
            let handler = unsafe {
                VNImageRequestHandler::initWithCGImage_options(
                    VNImageRequestHandler::alloc(),
                    &image,
                    &NSDictionary::new(),
                )
            };
            let request_ref: &VNRequest = request.as_ref();
            let requests = NSArray::from_slice(&[request_ref]);
            handler
                .performRequests_error(&requests)
                .map_err(|err| anyhow::anyhow!("{err}"))?;
            let mut lines = Vec::new();
            if let Some(results) = request.results() {
                for observation in results.iter() {
                    if let Some(text) = observation.topCandidates(1).firstObject() {
                        lines.push(text.string().to_string());
                    }
                }
            }
            Ok(Outcome::Done {
                text: lines.join("\n").chars().take(MAX_TEXT_CHARS).collect(),
                language: self.languages.join(","),
            })
        })
    }
}
