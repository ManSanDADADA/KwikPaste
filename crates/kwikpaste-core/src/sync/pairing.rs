//! 配对码与 SPAKE2 握手。
//!
//! 6 位配对码只有约 20 bit，不能直接当密钥：抓到包就能离线穷举。这里用 SPAKE2 把配对码变成
//! 一次性的共享密钥，旁观者无法穷举，主动冒充的人每次连接只能猜一次。双方再用这把密钥对
//! Noise 握手哈希和两边的公钥各算一个确认值，确认值对上才说明公钥没被中间人换掉。

use rand::rngs::OsRng;
use rand::Rng;
use spake2::{Ed25519Group, Identity, Password, Spake2};

use super::protocol::from_hex;

/// 同一个配对码最多尝试的次数；用完要在显示方刷新。
pub const PAIRING_ATTEMPTS: u8 = 5;
pub(crate) const CODE_DIGITS: usize = 6;
const SPAKE_ID_DIALER: &[u8] = b"kwikpaste-lan-dialer";
const SPAKE_ID_LISTENER: &[u8] = b"kwikpaste-lan-listener";
const CONFIRM_CONTEXT: &str = "KwikPaste LAN pairing confirmation v1";

/// 本机当前显示的配对码。
pub struct PairingCode {
    code: String,
    attempts_left: u8,
}

impl PairingCode {
    pub fn new() -> Self {
        Self {
            code: random_code(),
            attempts_left: PAIRING_ATTEMPTS,
        }
    }

    /// 次数用完后不再展示旧码，界面提示刷新。
    pub fn code(&self) -> Option<&str> {
        (self.attempts_left > 0).then_some(self.code.as_str())
    }

    pub fn attempts_left(&self) -> u8 {
        self.attempts_left
    }

    /// 开始一次配对尝试并返回配对码。先扣次数再握手：对方中途断开也算一次，
    /// 否则攻击者拿到确认值后断开就能白拿一次猜测。
    pub fn begin_attempt(&mut self) -> Option<String> {
        if self.attempts_left == 0 {
            return None;
        }

        self.attempts_left -= 1;
        Some(self.code.clone())
    }
}

fn random_code() -> String {
    let value: u32 = OsRng.gen_range(0..1_000_000);
    format!("{value:0width$}", width = CODE_DIGITS)
}

/// 规范化用户输入的配对码：去掉空格和连字符，必须是 `digits` 位数字（局域网 6 位，远程同步 8 位）。
pub fn normalize_code(input: &str, digits: usize) -> Option<String> {
    let code: String = input
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '-')
        .collect();

    (code.len() == digits && code.chars().all(|c| c.is_ascii_digit())).then_some(code)
}

/// 输入配对码、主动连接的一方。
pub fn start_dialer(code: &str) -> (Spake2<Ed25519Group>, Vec<u8>) {
    Spake2::<Ed25519Group>::start_a_with_rng(
        &Password::new(code.as_bytes()),
        &Identity::new(SPAKE_ID_DIALER),
        &Identity::new(SPAKE_ID_LISTENER),
        OsRng,
    )
}

/// 显示配对码、被连接的一方。
pub fn start_listener(code: &str) -> (Spake2<Ed25519Group>, Vec<u8>) {
    Spake2::<Ed25519Group>::start_b_with_rng(
        &Password::new(code.as_bytes()),
        &Identity::new(SPAKE_ID_DIALER),
        &Identity::new(SPAKE_ID_LISTENER),
        OsRng,
    )
}

#[derive(Debug, Clone, Copy)]
pub enum Role {
    Dialer,
    Listener,
}

impl Role {
    fn label(self) -> &'static [u8] {
        match self {
            Role::Dialer => b"dialer",
            Role::Listener => b"listener",
        }
    }
}

/// 确认值绑定的握手内容：Noise 握手哈希 + 发起方公钥 + 被连接方公钥。
pub struct Transcript<'a> {
    pub handshake_hash: &'a [u8],
    pub dialer_key: &'a [u8],
    pub listener_key: &'a [u8],
}

/// 用 SPAKE2 共享密钥为 `role` 计算确认值。
pub fn confirmation(shared_key: &[u8], role: Role, transcript: &Transcript<'_>) -> blake3::Hash {
    let key = blake3::derive_key(CONFIRM_CONTEXT, shared_key);
    let mut hasher = blake3::Hasher::new_keyed(&key);
    hasher.update(role.label());
    hasher.update(transcript.handshake_hash);
    hasher.update(transcript.dialer_key);
    hasher.update(transcript.listener_key);
    hasher.finalize()
}

/// 比较对方发来的确认值（hex）；`blake3::Hash` 的相等比较是常数时间的。
pub fn verify_confirmation(expected: &blake3::Hash, received_hex: &str) -> bool {
    let Ok(bytes) = from_hex(received_hex) else {
        return false;
    };
    let Ok(bytes) = <[u8; 32]>::try_from(bytes.as_slice()) else {
        return false;
    };

    *expected == blake3::Hash::from(bytes)
}

#[cfg(test)]
mod tests {
    use super::super::protocol::to_hex;
    use super::*;

    fn transcript() -> Transcript<'static> {
        Transcript {
            handshake_hash: b"hash",
            dialer_key: b"dialer-public-key",
            listener_key: b"listener-public-key",
        }
    }

    #[test]
    fn same_code_confirms_both_ways() {
        let (dialer, dialer_msg) = start_dialer("123456");
        let (listener, listener_msg) = start_listener("123456");
        let dialer_key = dialer.finish(&listener_msg).unwrap();
        let listener_key = listener.finish(&dialer_msg).unwrap();

        let dialer_mac = confirmation(&dialer_key, Role::Dialer, &transcript());
        let expected = confirmation(&listener_key, Role::Dialer, &transcript());

        assert!(verify_confirmation(
            &expected,
            &to_hex(dialer_mac.as_bytes())
        ));
    }

    #[test]
    fn wrong_code_fails_confirmation() {
        let (dialer, dialer_msg) = start_dialer("123456");
        let (listener, listener_msg) = start_listener("654321");
        let dialer_key = dialer.finish(&listener_msg).unwrap();
        let listener_key = listener.finish(&dialer_msg).unwrap();

        let dialer_mac = confirmation(&dialer_key, Role::Dialer, &transcript());
        let expected = confirmation(&listener_key, Role::Dialer, &transcript());

        assert!(!verify_confirmation(
            &expected,
            &to_hex(dialer_mac.as_bytes())
        ));
    }

    #[test]
    fn roles_produce_different_confirmations() {
        let key = [7u8; 32];

        assert_ne!(
            confirmation(&key, Role::Dialer, &transcript()),
            confirmation(&key, Role::Listener, &transcript())
        );
    }

    #[test]
    fn attempts_run_out() {
        let mut code = PairingCode::new();

        for _ in 0..PAIRING_ATTEMPTS {
            assert!(code.begin_attempt().is_some());
        }

        assert!(code.begin_attempt().is_none());
        assert!(code.code().is_none());
    }

    #[test]
    fn normalizes_user_input() {
        assert_eq!(normalize_code(" 123 456 ", 6), Some("123456".to_owned()));
        assert_eq!(normalize_code("123-456", 6), Some("123456".to_owned()));
        assert_eq!(normalize_code("12345", 6), None);
        assert_eq!(normalize_code("1234 5678", 8), Some("12345678".to_owned()));
        assert_eq!(normalize_code("12345a", 6), None);
    }
}
