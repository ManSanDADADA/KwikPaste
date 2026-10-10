//! OCR helper 的有界二进制帧；正文只含路径或文本，绝不携带图片字节。
use std::io::{self, Read, Write};

use serde::{Deserialize, Serialize, de::DeserializeOwned};

pub const OCR_PROTOCOL: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum OcrSupport {
    Available { languages: Vec<String> },
    MissingLanguage,
    Unsupported,
    NotInstalled,
    Disabled,
}

pub const MAX_FRAME_BYTES: usize = 512 * 1024;
pub const MAX_TEXT_CHARS: usize = 64 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "tag", deny_unknown_fields)]
pub enum Request {
    Probe,
    Recognize { job_id: String, path: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "tag", deny_unknown_fields)]
pub enum Response {
    Probe { support: OcrSupport },
    Recognize { job_id: String, outcome: Outcome },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "tag", deny_unknown_fields)]
pub enum Outcome {
    Done { text: String, language: String },
    Skipped { reason: String },
    Failed { reason: String },
}

/// 复用调用方缓冲区；仅帧边界上的 EOF 正常，半帧或未知 tag 必须拒绝。
pub fn read_frame<T: DeserializeOwned>(
    reader: &mut impl Read,
    buffer: &mut Vec<u8>,
) -> io::Result<Option<T>> {
    let mut length = [0; 4];
    match reader.read_exact(&mut length[..1]) {
        Err(err) if err.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        other => other?,
    }
    reader.read_exact(&mut length[1..])?;
    let length = u32::from_le_bytes(length) as usize;
    if length == 0 || length > MAX_FRAME_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid OCR frame length",
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
    buffer.clear();
    struct Limited<'a>(&'a mut Vec<u8>);
    impl Write for Limited<'_> {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if self.0.len().saturating_add(bytes.len()) > MAX_FRAME_BYTES {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "OCR frame too large",
                ));
            }
            self.0.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    serde_json::to_writer(Limited(buffer), value)?;
    if buffer.len() > MAX_FRAME_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "OCR frame too large",
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
                    text: "中文\nEnglish".into(),
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
