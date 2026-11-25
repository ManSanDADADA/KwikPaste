//! 组密钥的派生、条目 id、条目与设备信息的加解密，以及 HLC（REMOTE.md §5）。

use anyhow::{anyhow, Context};
use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{Key, XChaCha20Poly1305, XNonce};
use rand::rngs::OsRng;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::db::models::Platform;

const ENTRY_KEY_CONTEXT: &str = "KwikPaste remote entry key v1";
const ENTRY_ID_CONTEXT: &str = "KwikPaste remote entry id v1";
pub(super) const KEY_LEN: usize = 32;
const NONCE_LEN: usize = 24;
/// 条目 id 取带密钥哈希的前 32 个十六进制字符。
const ENTRY_ID_LEN: usize = 32;

pub(super) fn to_base64(bytes: &[u8]) -> String {
    STANDARD.encode(bytes)
}

pub(super) fn from_base64(text: &str) -> anyhow::Result<Vec<u8>> {
    STANDARD.decode(text.trim()).context("invalid base64")
}

pub(super) fn random_key() -> [u8; KEY_LEN] {
    let mut key = [0u8; KEY_LEN];
    OsRng.fill_bytes(&mut key);
    key
}

/// 组密钥和它派生出的条目加密密钥、条目 id 密钥。服务器只见过密文和条目 id。
pub(super) struct GroupKeys {
    group: Zeroizing<[u8; KEY_LEN]>,
    entry: Zeroizing<[u8; KEY_LEN]>,
    id: Zeroizing<[u8; KEY_LEN]>,
}

impl GroupKeys {
    pub fn new(group: [u8; KEY_LEN]) -> Self {
        Self {
            entry: Zeroizing::new(blake3::derive_key(ENTRY_KEY_CONTEXT, &group)),
            id: Zeroizing::new(blake3::derive_key(ENTRY_ID_CONTEXT, &group)),
            group: Zeroizing::new(group),
        }
    }

    pub fn from_base64(text: &str) -> anyhow::Result<Self> {
        let bytes = Zeroizing::new(from_base64(text)?);
        let group: [u8; KEY_LEN] = bytes
            .as_slice()
            .try_into()
            .map_err(|_| anyhow!("group key has wrong length"))?;
        Ok(Self::new(group))
    }

    pub fn group_base64(&self) -> String {
        to_base64(self.group.as_slice())
    }

    pub fn item_id(&self, item_key: &str) -> String {
        self.entry_id(&[b"item", item_key.as_bytes()])
    }

    pub fn meta_id(&self, field: &str, item_key: &str) -> String {
        self.entry_id(&[b"meta", field.as_bytes(), item_key.as_bytes()])
    }

    pub fn settings_id(&self, scope: &str, pointer: &str) -> String {
        self.entry_id(&[b"settings", scope.as_bytes(), pointer.as_bytes()])
    }

    /// 定义串各段之间用 0 分隔。
    fn entry_id(&self, parts: &[&[u8]]) -> String {
        let mut hasher = blake3::Hasher::new_keyed(&self.id);
        for (index, part) in parts.iter().enumerate() {
            if index > 0 {
                hasher.update(&[0]);
            }
            hasher.update(part);
        }
        hasher.finalize().to_hex()[..ENTRY_ID_LEN].to_owned()
    }

    pub fn seal_entry(
        &self,
        entry_id: &str,
        kind: &str,
        hlc: &str,
        plaintext: &[u8],
    ) -> anyhow::Result<Vec<u8>> {
        seal(&self.entry, &entry_aad(entry_id, kind, hlc), plaintext)
    }

    pub fn open_entry(
        &self,
        entry_id: &str,
        kind: &str,
        hlc: &str,
        sealed: &[u8],
    ) -> anyhow::Result<Vec<u8>> {
        open(&self.entry, &entry_aad(entry_id, kind, hlc), sealed)
    }

    pub fn seal_device(&self, device_id: &str, info: &DeviceInfo) -> anyhow::Result<String> {
        let sealed = seal(
            &self.entry,
            &device_aad(device_id),
            &serde_json::to_vec(info)?,
        )?;
        Ok(to_base64(&sealed))
    }

    pub fn open_device(&self, device_id: &str, sealed: &str) -> anyhow::Result<DeviceInfo> {
        let plain = open(&self.entry, &device_aad(device_id), &from_base64(sealed)?)?;
        Ok(serde_json::from_slice(&plain)?)
    }
}

/// 密封前的设备信息（REMOTE.md §4.5）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct DeviceInfo {
    pub name: String,
    pub platform: Platform,
    #[serde(default)]
    pub app_version: String,
}

fn entry_aad(entry_id: &str, kind: &str, hlc: &str) -> Vec<u8> {
    [
        entry_id.as_bytes(),
        &[0],
        kind.as_bytes(),
        &[0],
        hlc.as_bytes(),
    ]
    .concat()
}

fn device_aad(device_id: &str) -> Vec<u8> {
    [b"device".as_slice(), &[0], device_id.as_bytes()].concat()
}

/// `nonce(24) ‖ XChaCha20-Poly1305(key, nonce, aad, plaintext)`。
pub(super) fn seal(key: &[u8; KEY_LEN], aad: &[u8], plaintext: &[u8]) -> anyhow::Result<Vec<u8>> {
    let mut nonce = [0u8; NONCE_LEN];
    OsRng.fill_bytes(&mut nonce);
    let sealed = XChaCha20Poly1305::new(Key::from_slice(key))
        .encrypt(
            XNonce::from_slice(&nonce),
            Payload {
                msg: plaintext,
                aad,
            },
        )
        .map_err(|_| anyhow!("encryption failed"))?;
    Ok([nonce.as_slice(), &sealed].concat())
}

pub(super) fn open(key: &[u8; KEY_LEN], aad: &[u8], sealed: &[u8]) -> anyhow::Result<Vec<u8>> {
    if sealed.len() < NONCE_LEN {
        return Err(anyhow!("ciphertext is too short"));
    }
    let (nonce, body) = sealed.split_at(NONCE_LEN);
    XChaCha20Poly1305::new(Key::from_slice(key))
        .decrypt(XNonce::from_slice(nonce), Payload { msg: body, aad })
        .map_err(|_| anyhow!("decryption failed"))
}

/// 条目明文：`u32 大端 JSON 长度 ‖ JSON ‖ 二进制`。
pub(super) fn pack(json: &[u8], binary: &[u8]) -> anyhow::Result<Vec<u8>> {
    let length = u32::try_from(json.len()).context("entry json is too large")?;
    Ok([&length.to_be_bytes(), json, binary].concat())
}

pub(super) fn unpack(plain: &[u8]) -> anyhow::Result<(&[u8], &[u8])> {
    let Some((length, rest)) = plain.split_first_chunk::<4>() else {
        return Err(anyhow!("entry plaintext is too short"));
    };
    let length = u32::from_be_bytes(*length) as usize;
    if rest.len() < length {
        return Err(anyhow!("entry json is truncated"));
    }
    Ok(rest.split_at(length))
}

/// 混合逻辑时钟：64 位 = 毫秒 Unix 时间 << 16 | 计数。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct Hlc(pub u64);

impl Hlc {
    /// 线上格式 `<16 位小写十六进制>-<设备 id>`。
    pub fn wire(self, device_id: &str) -> String {
        format!("{:016x}-{device_id}", self.0)
    }
}

/// 解析线上格式，返回时钟值与设备 id。
pub(super) fn parse_hlc(text: &str) -> Option<(Hlc, &str)> {
    let (clock, device) = text.split_once('-')?;
    if clock.len() != 16 || device.is_empty() {
        return None;
    }
    u64::from_str_radix(clock, 16)
        .ok()
        .map(|value| (Hlc(value), device))
}

/// `a` 是否比 `b` 新：先比时钟值，再比设备 id。解析不了的当作最旧。
pub(super) fn hlc_newer(a: &str, b: &str) -> bool {
    match (parse_hlc(a), parse_hlc(b)) {
        (Some(a), Some(b)) => a > b,
        (Some(_), None) => true,
        _ => false,
    }
}

/// 本机的 HLC：取 `max(本机时钟, 见过的最大值 + 1)`，收到更大的值时追上去。
#[derive(Debug, Default)]
pub(super) struct HlcClock {
    last: u64,
}

impl HlcClock {
    pub fn tick(&mut self) -> Hlc {
        self.tick_at(chrono::Utc::now().timestamp_millis().max(0) as u64)
    }

    pub fn tick_at(&mut self, now_ms: u64) -> Hlc {
        self.last = (now_ms << 16).max(self.last.saturating_add(1));
        Hlc(self.last)
    }

    pub fn observe(&mut self, seen: Hlc) {
        self.last = self.last.max(seen.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys() -> GroupKeys {
        GroupKeys::new([7; KEY_LEN])
    }

    #[test]
    fn entries_round_trip_and_reject_another_aad() {
        let keys = keys();
        let plain = pack(br#"{"a":1}"#, b"\x89PNG").unwrap();
        let sealed = keys
            .seal_entry("e1", "item", "0000000000000001-dev", &plain)
            .unwrap();

        let opened = keys
            .open_entry("e1", "item", "0000000000000001-dev", &sealed)
            .unwrap();
        assert_eq!(
            unpack(&opened).unwrap(),
            (&br#"{"a":1}"#[..], &b"\x89PNG"[..])
        );
        for (id, kind, hlc) in [
            ("e2", "item", "0000000000000001-dev"),
            ("e1", "meta", "0000000000000001-dev"),
            ("e1", "item", "0000000000000002-dev"),
        ] {
            assert!(keys.open_entry(id, kind, hlc, &sealed).is_err());
        }
        assert!(GroupKeys::new([8; KEY_LEN])
            .open_entry("e1", "item", "0000000000000001-dev", &sealed)
            .is_err());
        assert!(unpack(&[0, 0, 0, 9, 1]).is_err());
    }

    #[test]
    fn device_info_is_bound_to_the_device_id() {
        let keys = keys();
        let info = DeviceInfo {
            name: "书房".into(),
            platform: Platform::Macos,
            app_version: "2.0.11".into(),
        };
        let sealed = keys.seal_device("aaaa", &info).unwrap();

        assert_eq!(keys.open_device("aaaa", &sealed).unwrap(), info);
        assert!(keys.open_device("bbbb", &sealed).is_err());
    }

    #[test]
    fn entry_ids_are_keyed_and_kind_separated() {
        let keys = keys();
        let item = keys.item_id("text:abc");

        assert_eq!(item.len(), 32);
        assert!(item.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(item, keys.item_id("text:abc"));
        assert_ne!(item, GroupKeys::new([8; KEY_LEN]).item_id("text:abc"));
        let expected =
            blake3::Hasher::new_keyed(&blake3::derive_key(ENTRY_ID_CONTEXT, &[7; KEY_LEN]))
                .update(b"meta\0favorite\0text:abc")
                .finalize()
                .to_hex()[..32]
                .to_owned();
        assert_eq!(keys.meta_id("favorite", "text:abc"), expected);
        assert_ne!(keys.meta_id("pinned", "text:abc"), expected);
        assert_ne!(
            keys.settings_id("all", "/appearance/theme"),
            keys.settings_id("windows", "/appearance/theme")
        );
    }

    #[test]
    fn hlc_is_monotonic_and_orders_by_device_last() {
        let mut clock = HlcClock::default();
        let first = clock.tick_at(1_000);
        let second = clock.tick_at(1_000);
        let earlier_clock = clock.tick_at(900);
        assert!(first < second && second < earlier_clock);

        clock.observe(Hlc(5_000 << 16));
        assert!(clock.tick_at(1_000) > Hlc(5_000 << 16));

        let wire = first.wire("dev");
        assert_eq!(wire, format!("{:016x}-dev", 1_000u64 << 16));
        assert_eq!(parse_hlc(&wire), Some((first, "dev")));
        assert!(hlc_newer(&second.wire("a"), &first.wire("z")));
        assert!(hlc_newer(&first.wire("b"), &first.wire("a")));
        assert!(!hlc_newer(&first.wire("a"), &first.wire("a")));
        assert!(hlc_newer(&first.wire("a"), "garbage"));
        assert!(parse_hlc("123-dev").is_none());
    }
}
