//! OCR helper 的有界二进制帧；正文只含路径或文本，绝不携带图片字节。
use std::io::{self, Read, Write};

use serde::{Deserialize, Serialize, de::DeserializeOwned};

pub const OCR_PROTOCOL: u32 = 2;
pub const WETYPE_PROTOCOL: u32 = 1;
pub const TRANSLATE_PROTOCOL: u32 = 1;
pub const WETYPE_MAX_TEXT_BYTES: usize = 1024 * 1024;
pub const WETYPE_MAX_IMAGE_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WeTypeDevice {
    pub uin: u64,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "tag", deny_unknown_fields)]
pub enum WeTypeCommand {
    Pair {
        code: String,
        device_name: String,
    },
    Unpair,
    SendText {
        text: String,
    },
    SendImage {
        path: String,
    },
    /// 单条待确认接收；宿主提交或明确跳过后才允许 SDK 推进水位线。
    Acknowledge,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "tag", deny_unknown_fields)]
pub enum WeTypeContent {
    Text { text: String },
    Image { path: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "tag", deny_unknown_fields)]
pub enum WeTypeEvent {
    State {
        paired: bool,
        self_name: String,
        devices: Vec<WeTypeDevice>,
        error: Option<String>,
    },
    PairResult {
        ok: bool,
        error: Option<String>,
    },
    Received {
        from_uin: u64,
        from_name: String,
        content: WeTypeContent,
    },
    SendResult {
        ok: bool,
        error: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum OcrSupport {
    Available { languages: Vec<String> },
    MissingLanguage,
    Unsupported,
    NotInstalled,
    Disabled,
}

// JSON 的控制字符最多膨胀六倍，另留出路径、设备名和事件元数据空间。
pub const WETYPE_MAX_FRAME_BYTES: usize = WETYPE_MAX_TEXT_BYTES * 6 + 64 * 1024;
pub const MAX_TEXT_CHARS: usize = 64 * 1024;
// 最坏每行只有一个字：连同换行最多约 32K 行，八个 f32 坐标也必须放得下。
pub const MAX_FRAME_BYTES: usize = MAX_TEXT_CHARS * 96 + 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "tag", deny_unknown_fields)]
pub enum Request {
    Probe,
    Recognize { job_id: String, path: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "tag", deny_unknown_fields)]
pub enum Response {
    Probe { support: OcrSupport },
    Recognize { job_id: String, outcome: Outcome },
}

/// 原图像素网格中的归一化行框；依次为阅读起点上、终点上、终点下、起点下。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TextLine {
    pub text: String,
    pub quad: [[f32; 2]; 4],
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "tag", deny_unknown_fields)]
pub enum Outcome {
    Done {
        lines: Vec<TextLine>,
        language: String,
    },
    Skipped {
        reason: String,
    },
    Failed {
        reason: String,
    },
}

/// 翻译扩展的请求。语言用 BCP 47 写法（`zh-Hans`、`en`…）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "tag", deny_unknown_fields)]
pub enum TranslateRequest {
    Translate {
        job_id: String,
        text: String,
        /// 首选语言：原文不是它时译成它。
        first: String,
        /// 第二语言：原文是首选语言时译成它。
        second: String,
        /// 自动模式：原文是首选语言或语种拿不准时返回 `Skipped`，不联网。
        auto: bool,
        engine: TranslateEngine,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "tag", deny_unknown_fields)]
pub enum TranslateEngine {
    /// 免密钥的网页接口，具体用哪几家由扩展决定。
    Free,
    /// OpenAI Chat Completions 格式；`prompt` 为空时用扩展内置的提示词。
    OpenAi {
        base_url: String,
        api_key: String,
        model: String,
        prompt: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "tag", deny_unknown_fields)]
pub enum TranslateResponse {
    Translate {
        job_id: String,
        outcome: TranslateOutcome,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "tag", deny_unknown_fields)]
pub enum TranslateOutcome {
    Done {
        text: String,
        /// 服务报告的原文语种；AI 接口没有。
        source: Option<String>,
        target: String,
        /// 稳定的短 id：`volc`、`transmart`；AI 接口是模型名。
        provider: String,
    },
    /// 只在 `auto` 为真时返回。
    Skipped,
    Failed {
        kind: TranslateFailure,
        message: String,
    },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum TranslateFailure {
    Network,
    Auth,
    RateLimited,
    Unsupported,
    Other,
}

/// 复用调用方缓冲区；仅帧边界上的 EOF 正常，半帧或未知 tag 必须拒绝。
pub fn read_frame<T: DeserializeOwned>(
    reader: &mut impl Read,
    buffer: &mut Vec<u8>,
) -> io::Result<Option<T>> {
    read_frame_with_limit(reader, buffer, MAX_FRAME_BYTES)
}

/// 同一帧格式按扩展使用不同正文上限，不改变 OCR 已有边界。
pub fn read_frame_with_limit<T: DeserializeOwned>(
    reader: &mut impl Read,
    buffer: &mut Vec<u8>,
    limit: usize,
) -> io::Result<Option<T>> {
    let mut length = [0; 4];
    match reader.read_exact(&mut length[..1]) {
        Err(err) if err.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        other => other?,
    }
    reader.read_exact(&mut length[1..])?;
    let length = u32::from_le_bytes(length) as usize;
    if length == 0 || length > limit {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid extension frame length",
        ));
    }
    buffer.resize(length, 0);
    reader.read_exact(buffer)?;
    serde_json::from_slice(buffer)
        .map(Some)
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))
}

/// 在同一个有界缓冲区编码并发送长度前缀帧。
pub fn write_frame<T: Serialize>(
    writer: &mut impl Write,
    value: &T,
    buffer: &mut Vec<u8>,
) -> io::Result<()> {
    write_frame_with_limit(writer, value, buffer, MAX_FRAME_BYTES)
}

/// 共用编码器按指定上限编码，包含 JSON 转义后的长度。
pub fn write_frame_with_limit<T: Serialize>(
    writer: &mut impl Write,
    value: &T,
    buffer: &mut Vec<u8>,
    limit: usize,
) -> io::Result<()> {
    buffer.clear();
    struct Limited<'a>(&'a mut Vec<u8>, usize);
    impl Write for Limited<'_> {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if self.0.len().saturating_add(bytes.len()) > self.1 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "extension frame too large",
                ));
            }
            self.0.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    serde_json::to_writer(Limited(buffer, limit), value)?;
    if buffer.len() > limit || buffer.len() > u32::MAX as usize {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "extension frame too large",
        ));
    }
    writer.write_all(&(buffer.len() as u32).to_le_bytes())?;
    writer.write_all(buffer)?;
    writer.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn wetype_roundtrip<T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug>(
        value: T,
    ) -> io::Result<()> {
        let mut wire = Vec::new();
        let mut buffer = Vec::new();
        write_frame_with_limit(&mut wire, &value, &mut buffer, WETYPE_MAX_FRAME_BYTES)?;
        assert_eq!(
            read_frame_with_limit::<T>(
                &mut Cursor::new(wire),
                &mut buffer,
                WETYPE_MAX_FRAME_BYTES
            )?,
            Some(value)
        );
        Ok(())
    }

    #[test]
    fn wetype_commands_and_events_roundtrip() -> io::Result<()> {
        for command in [
            WeTypeCommand::Pair {
                code: "123456".into(),
                device_name: "KwikPaste · 电脑".into(),
            },
            WeTypeCommand::Unpair,
            WeTypeCommand::SendText {
                text: "中文\ntext".into(),
            },
            WeTypeCommand::SendImage {
                path: "C:\\图片.png".into(),
            },
            WeTypeCommand::Acknowledge,
        ] {
            wetype_roundtrip(command)?;
        }
        for event in [
            WeTypeEvent::State {
                paired: true,
                self_name: "KwikPaste".into(),
                devices: vec![WeTypeDevice {
                    uin: u64::MAX,
                    name: "Phone".into(),
                }],
                error: None,
            },
            WeTypeEvent::PairResult {
                ok: false,
                error: Some("invalid code".into()),
            },
            WeTypeEvent::Received {
                from_uin: 1,
                from_name: "Phone".into(),
                content: WeTypeContent::Text {
                    text: "text".into(),
                },
            },
            WeTypeEvent::Received {
                from_uin: 1,
                from_name: "Phone".into(),
                content: WeTypeContent::Image {
                    path: "/state/inbox/image".into(),
                },
            },
            WeTypeEvent::SendResult {
                ok: true,
                error: None,
            },
        ] {
            wetype_roundtrip(event)?;
        }
        Ok(())
    }

    #[test]
    fn wetype_full_megabyte_including_json_escapes_and_ocr_limit() -> io::Result<()> {
        wetype_roundtrip(WeTypeCommand::SendText {
            text: "x".repeat(WETYPE_MAX_TEXT_BYTES),
        })?;
        wetype_roundtrip(WeTypeCommand::SendText {
            text: "\0".repeat(WETYPE_MAX_TEXT_BYTES),
        })?;
        let command = WeTypeCommand::SendText {
            text: "x".repeat(MAX_FRAME_BYTES),
        };
        assert!(write_frame(&mut Vec::new(), &command, &mut Vec::new()).is_err());
        assert!(
            read_frame_with_limit::<WeTypeCommand>(
                &mut Cursor::new((WETYPE_MAX_FRAME_BYTES as u32 + 1).to_le_bytes()),
                &mut Vec::new(),
                WETYPE_MAX_FRAME_BYTES
            )
            .is_err()
        );
        Ok(())
    }

    #[test]
    fn translate_frames_roundtrip() -> io::Result<()> {
        let request = TranslateRequest::Translate {
            job_id: "id".into(),
            text: "第一行\nsecond line".into(),
            first: "zh-Hans".into(),
            second: "en".into(),
            auto: false,
            engine: TranslateEngine::OpenAi {
                base_url: "http://localhost:11434/v1".into(),
                api_key: String::new(),
                model: "translategemma".into(),
                prompt: None,
            },
        };
        let response = TranslateResponse::Translate {
            job_id: "id".into(),
            outcome: TranslateOutcome::Done {
                text: "first line\n第二行".into(),
                source: Some("zh".into()),
                target: "en".into(),
                provider: "volc".into(),
            },
        };
        let mut wire = Vec::new();
        let mut buffer = Vec::new();
        write_frame(&mut wire, &request, &mut buffer)?;
        write_frame(&mut wire, &response, &mut buffer)?;
        let mut reader = Cursor::new(wire);
        assert_eq!(
            read_frame::<TranslateRequest>(&mut reader, &mut buffer)?,
            Some(request)
        );
        assert_eq!(
            read_frame::<TranslateResponse>(&mut reader, &mut buffer)?,
            Some(response)
        );
        Ok(())
    }

    #[test]
    fn roundtrip_and_eof() {
        let value = Request::Recognize {
            job_id: "id".into(),
            path: "C:\\图片.png".into(),
        };
        let mut wire = Vec::new();
        let mut buffer = Vec::new();
        write_frame(&mut wire, &value, &mut buffer).unwrap();
        let mut reader = Cursor::new(wire);
        assert_eq!(
            read_frame::<Request>(&mut reader, &mut buffer).unwrap(),
            Some(value)
        );
        assert!(
            read_frame::<Request>(&mut reader, &mut buffer)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn full_cjk_text_with_short_lines_fits_frame() {
        let value = Response::Recognize {
            job_id: "full".into(),
            outcome: Outcome::Done {
                lines: vec![
                    TextLine {
                        text: "字".into(),
                        quad: [[0.0000012345679, 0.0000012345679]; 4],
                    };
                    MAX_TEXT_CHARS / 2
                ],
                language: "zh-Hans".into(),
            },
        };
        let mut wire = Vec::new();
        let mut buffer = Vec::new();
        write_frame(&mut wire, &value, &mut buffer).unwrap();
        assert_eq!(
            read_frame::<Response>(&mut Cursor::new(wire), &mut buffer).unwrap(),
            Some(value)
        );
    }

    #[test]
    fn rejects_truncated_oversized_and_unknown_frames() {
        let mut buffer = Vec::new();
        for wire in [
            vec![1],
            vec![8, 0, 0, 0, b'{'],
            (MAX_FRAME_BYTES as u32 + 1).to_le_bytes().to_vec(),
            vec![0, 0, 0, 0],
        ] {
            assert!(read_frame::<Request>(&mut Cursor::new(wire), &mut buffer).is_err());
        }
        let json = br#"{"tag":"Unknown"}"#;
        let mut wire = (json.len() as u32).to_le_bytes().to_vec();
        wire.extend_from_slice(json);
        assert!(read_frame::<Request>(&mut Cursor::new(wire), &mut buffer).is_err());
    }
    #[test]
    fn response_variants_roundtrip_and_writer_is_bounded() {
        let mut buffer = Vec::new();
        for value in [
            Response::Probe {
                support: OcrSupport::Available {
                    languages: vec!["zh-Hans-CN".into()],
                },
            },
            Response::Recognize {
                job_id: "a".into(),
                outcome: Outcome::Done {
                    lines: vec![TextLine {
                        text: "中文\nEnglish".into(),
                        quad: [[0.1, 0.2], [0.8, 0.2], [0.8, 0.3], [0.1, 0.3]],
                    }],
                    language: "zh-Hans-CN".into(),
                },
            },
            Response::Recognize {
                job_id: "a".into(),
                outcome: Outcome::Skipped {
                    reason: "40 MP".into(),
                },
            },
            Response::Recognize {
                job_id: "a".into(),
                outcome: Outcome::Failed {
                    reason: "timeout".into(),
                },
            },
        ] {
            let mut wire = Vec::new();
            write_frame(&mut wire, &value, &mut buffer).unwrap();
            assert_eq!(
                read_frame::<Response>(&mut Cursor::new(wire), &mut buffer).unwrap(),
                Some(value)
            );
        }
        let oversized = Request::Recognize {
            job_id: "a".into(),
            path: "a".repeat(MAX_FRAME_BYTES + 1),
        };
        assert!(write_frame(&mut Vec::new(), &oversized, &mut buffer).is_err());
        assert!(buffer.len() <= MAX_FRAME_BYTES);
    }
}
