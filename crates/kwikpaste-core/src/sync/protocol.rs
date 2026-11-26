//! 局域网同步的线上消息（协议 v2，完整约定见同目录的 `PROTOCOL.md`）。
//!
//! 一条应用消息 = `u32` 头长度 + JSON 头 + 二进制附件（图片原始 PNG），整体再按
//! [`super::transport`] 分块加密。消息是带 `type` 标签的枚举：不认识的类型、读不懂的消息都跳过，
//! 以后加新消息（比如同步收藏、备注）不用换大版本。

use anyhow::{anyhow, Context};
use serde::{Deserialize, Serialize};

use crate::db::models::{ClipboardKind, ClipboardSubKind, Platform};

/// mDNS / DNS-SD 服务类型。
pub const SERVICE_TYPE: &str = "_kwikpaste._tcp.local.";
/// 本机支持的协议版本范围。握手时双方取共同的最高版本。
pub const SUPPORTED: VersionRange = VersionRange { min: 2, max: 2 };
/// Noise 握手模式：XX 双方交换静态公钥，身份由配对时固定的公钥确认。
pub const NOISE_PARAMS: &str = "Noise_XX_25519_ChaChaPoly_BLAKE2s";
/// 不带版本号：prologue 不同 Noise 握手就过不去，版本要在握手之后的 `hello` 里协商。
pub const NOISE_PROLOGUE: &[u8] = b"KwikPaste LAN sync";
/// JSON 头（含文本内容）的上限；超过的文本不发。
pub const MAX_HEADER_BYTES: usize = 16 * 1024 * 1024;
/// 认证完成之前（还没交换附件上限）单条消息的上限。
pub const HANDSHAKE_MESSAGE_BYTES: usize = 64 * 1024;

/// 一台设备支持的协议版本范围（含两端）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct VersionRange {
    pub min: u16,
    pub max: u16,
}

impl VersionRange {
    /// 两边都支持的最高版本；没有交集时返回 `None`。
    pub fn negotiate(self, other: VersionRange) -> Option<u16> {
        let low = self.min.max(other.min);
        let high = self.max.min(other.max);
        (low <= high).then_some(high)
    }

    pub fn contains(self, version: u16) -> bool {
        (self.min..=self.max).contains(&version)
    }
}

/// 版本不兼容时是哪一边旧了，界面据此提示升级哪台设备。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Incompatibility {
    /// 对方支持的最高版本也比本机最低的低。
    PeerOutdated,
    /// 本机支持的最高版本也比对方最低的低。
    SelfOutdated,
}

/// 两个版本范围没有交集时，判断是哪一边旧了。
pub fn incompatibility(ours: VersionRange, theirs: VersionRange) -> Option<Incompatibility> {
    if theirs.max < ours.min {
        Some(Incompatibility::PeerOutdated)
    } else if theirs.min > ours.max {
        Some(Incompatibility::SelfOutdated)
    } else {
        None
    }
}

/// 连接双方在握手后互报的设备信息。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceInfo {
    pub name: String,
    pub platform: Platform,
    pub app_version: String,
    pub protocol: VersionRange,
    /// 本机监听端口；对端据此记下「下次怎么连回来」。
    pub listen_port: u16,
    /// 这台设备愿意接收的单条附件上限（字节），跟随它设置里的图片大小上限。
    pub max_attachment_bytes: u64,
    /// 这台设备局域网收不收文本；旧版本没有这个字段，按收处理。
    #[serde(default = "accepts_text_default")]
    pub accepts_text: bool,
}

fn accepts_text_default() -> bool {
    true
}

/// 发起方的连接目的。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Intent {
    /// 已配对设备之间的同步会话。
    Sync,
    /// 用对方显示的配对码配对；`spake` 是 SPAKE2 首条消息（hex）。
    Pair { spake: String },
    /// 更新的版本才有的目的。
    #[serde(other)]
    Unknown,
}

/// 拒绝原因，发起方据此给用户提示。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RejectReason {
    NotPaired,
    PairingUnavailable,
    WrongCode,
    /// 没有共同的协议版本；`reject.supported` 带着拒绝方支持的范围。
    Incompatible,
    /// 更新的版本才有的原因。
    #[serde(other)]
    Unknown,
}

/// 同步的一条剪贴板记录。只描述内容本身，不带本机的记录 id：两边按内容指纹去重。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WireItem {
    pub kind: ClipboardKind,
    pub sub_kind: Option<ClipboardSubKind>,
    /// 文本的源表示（纯文本 / HTML / RTF）；图片为空，内容在附件里。
    pub content: String,
    pub search_text: Option<String>,
    pub summary: Option<String>,
    pub width: Option<i64>,
    pub height: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Message {
    Hello {
        device: DeviceInfo,
        intent: Intent,
    },
    /// 同步会话被接受；`protocol` 是协商出的版本。
    Welcome {
        device: DeviceInfo,
        protocol: u16,
    },
    PairChallenge {
        device: DeviceInfo,
        protocol: u16,
        spake: String,
    },
    PairConfirm {
        mac: String,
    },
    PairDone {
        mac: String,
    },
    Reject {
        reason: RejectReason,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        supported: Option<VersionRange>,
    },
    Ping,
    /// 一条记录；`live` 为真表示对方刚刚复制（可写入系统剪贴板），为假是补齐的历史。
    Push {
        item: WireItem,
        live: bool,
    },
    /// 请求对方补发它本机序号大于 `after` 的记录。`after` 是上次补齐到的水位线，`epoch` 是那条
    /// 水位线所属的序号代次；与对方当前的代次不同（或第一次补齐没有）时对方从头发。
    CatchUp {
        after: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        epoch: Option<String>,
    },
    /// 一页补齐发完：收到它之前的记录都已入库，水位线推进到 `epoch` 代的 `until`；
    /// `more` 为真时接着请求下一页。
    CatchUpPage {
        until: u64,
        more: bool,
        epoch: String,
    },
    /// 发送方已把接收方从已配对设备里移除。
    Unpair,
    /// 不认识的消息类型：跳过。
    #[serde(other)]
    Unknown,
}

/// 序列化一条消息：`u32` 头长度 + JSON 头 + 附件。
pub fn encode(message: &Message, attachment: &[u8]) -> anyhow::Result<Vec<u8>> {
    let header = serde_json::to_vec(message).context("failed to encode sync message")?;
    if header.len() > MAX_HEADER_BYTES {
        return Err(anyhow!("sync message header too large"));
    }
    let header_len = u32::try_from(header.len()).context("sync message header too large")?;

    let mut out = Vec::with_capacity(4 + header.len() + attachment.len());
    out.extend_from_slice(&header_len.to_be_bytes());
    out.extend_from_slice(&header);
    out.extend_from_slice(attachment);
    Ok(out)
}

/// 解析 [`encode`] 的结果，返回消息与附件。头读不懂（未来版本改了某个字段的取值）时返回
/// [`Message::Unknown`]，由会话跳过；长度对不上才算错误。
pub fn decode(mut bytes: Vec<u8>) -> anyhow::Result<(Message, Vec<u8>)> {
    if bytes.len() < 4 {
        return Err(anyhow!("sync message too short"));
    }

    let header_len = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize;
    let header_end = 4usize
        .checked_add(header_len)
        .filter(|end| *end <= bytes.len() && header_len <= MAX_HEADER_BYTES)
        .ok_or_else(|| anyhow!("sync message header length out of range"))?;

    let message = serde_json::from_slice(&bytes[4..header_end]).unwrap_or_else(|err| {
        log::debug!("skip unreadable sync message: {err}");
        Message::Unknown
    });
    let attachment = bytes.split_off(header_end);
    Ok((message, attachment))
}

pub fn to_hex(bytes: &[u8]) -> String {
    use std::fmt::Write;

    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut out, b| {
            let _ = write!(out, "{b:02x}");
            out
        })
}

pub fn from_hex(text: &str) -> anyhow::Result<Vec<u8>> {
    if !text.is_ascii() || !text.len().is_multiple_of(2) {
        return Err(anyhow!("invalid hex string"));
    }

    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).map_err(|_| anyhow!("invalid hex string")))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(message: &str) -> Vec<u8> {
        let mut bytes = (message.len() as u32).to_be_bytes().to_vec();
        bytes.extend_from_slice(message.as_bytes());
        bytes
    }

    #[test]
    fn message_round_trips_with_attachment() {
        let message = Message::CatchUp {
            after: 42,
            epoch: Some("e".to_owned()),
        };
        let bytes = encode(&message, b"png-bytes").unwrap();
        let (decoded, attachment) = decode(bytes).unwrap();

        assert!(matches!(
            decoded,
            Message::CatchUp { after: 42, epoch: Some(epoch) } if epoch == "e"
        ));
        assert_eq!(attachment, b"png-bytes");
    }

    #[test]
    fn decode_rejects_header_longer_than_message() {
        let mut bytes = 100u32.to_be_bytes().to_vec();
        bytes.extend_from_slice(b"{}");

        assert!(decode(bytes).is_err());
    }

    /// 以后加的消息类型、字段和枚举取值，老版本都跳过而不是断开连接。
    #[test]
    fn unknown_messages_fields_and_values_are_skipped() {
        let (message, _) = decode(header(r#"{"type":"syncFavorite","id":"x"}"#)).unwrap();
        assert!(matches!(message, Message::Unknown));

        let (message, _) =
            decode(header(r#"{"type":"catchUp","after":7,"window":"30d"}"#)).unwrap();
        assert!(matches!(
            message,
            Message::CatchUp {
                after: 7,
                epoch: None
            }
        ));

        let (message, _) = decode(header(
            r#"{"type":"push","live":true,"item":{"kind":"video","content":""}}"#,
        ))
        .unwrap();
        assert!(matches!(message, Message::Unknown));

        let (message, _) = decode(header(r#"{"type":"reject","reason":"rateLimited"}"#)).unwrap();
        assert!(matches!(
            message,
            Message::Reject {
                reason: RejectReason::Unknown,
                supported: None
            }
        ));
        let intent: Intent = serde_json::from_str(r#"{"kind":"mirror"}"#).unwrap();
        assert!(matches!(intent, Intent::Unknown));
    }

    #[test]
    fn versions_negotiate_to_the_highest_common_one() {
        let ours = VersionRange { min: 2, max: 4 };

        assert_eq!(ours.negotiate(VersionRange { min: 3, max: 6 }), Some(4));
        assert_eq!(ours.negotiate(VersionRange { min: 2, max: 2 }), Some(2));
        assert_eq!(ours.negotiate(VersionRange { min: 5, max: 6 }), None);
        assert_eq!(
            incompatibility(ours, VersionRange { min: 1, max: 1 }),
            Some(Incompatibility::PeerOutdated)
        );
        assert_eq!(
            incompatibility(ours, VersionRange { min: 5, max: 9 }),
            Some(Incompatibility::SelfOutdated)
        );
        assert_eq!(incompatibility(ours, VersionRange { min: 3, max: 9 }), None);
        assert!(SUPPORTED.contains(2));
    }

    #[test]
    fn hex_round_trips() {
        let bytes = [0u8, 1, 0xab, 0xff];

        assert_eq!(to_hex(&bytes), "0001abff");
        assert_eq!(from_hex("0001abff").unwrap(), bytes);
        assert!(from_hex("abc").is_err());
        assert!(from_hex("zz").is_err());
    }
}
