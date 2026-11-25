//! 用配对码加入同步组（REMOTE.md §4.4）。
//!
//! 配对码只有约 20 bit，服务器又在中间转发，不能直接拿它加密：两边用 SPAKE2 把配对码变成一次性的
//! 共享密钥 K，邀请方用 K 密封组密钥和一个随机的 joinSecret，只把 joinSecret 的哈希交给服务器。
//! 只有配对码对的加入方解得开，拿着 joinSecret 才换得到设备令牌；服务器始终拿不到组密钥。

use anyhow::anyhow;
use rand::rngs::OsRng;
use serde::{Deserialize, Serialize};
use spake2::{Ed25519Group, Identity, Password, Spake2};

use super::crypto::{from_base64, open, seal, to_base64, KEY_LEN};

const SPAKE_ID_JOINER: &[u8] = b"kwikpaste-remote-joiner";
const SPAKE_ID_MEMBER: &[u8] = b"kwikpaste-remote-member";
const PAIRING_KEY_CONTEXT: &str = "KwikPaste remote pairing v1";

/// 邀请方用 K 密封交给加入方的内容。
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct JoinGrant {
    /// base64 的组密钥。
    pub group_key: String,
    /// 32 字节随机数的 hex。
    pub join_secret: String,
}

/// 输入配对码、要加入的一方（SPAKE2 的 A）。
pub(super) fn start_joiner(code: &str) -> (Spake2<Ed25519Group>, Vec<u8>) {
    Spake2::<Ed25519Group>::start_a_with_rng(
        &Password::new(code.as_bytes()),
        &Identity::new(SPAKE_ID_JOINER),
        &Identity::new(SPAKE_ID_MEMBER),
        OsRng,
    )
}

/// 显示配对码的成员（SPAKE2 的 B）。
pub(super) fn start_member(code: &str) -> (Spake2<Ed25519Group>, Vec<u8>) {
    Spake2::<Ed25519Group>::start_b_with_rng(
        &Password::new(code.as_bytes()),
        &Identity::new(SPAKE_ID_JOINER),
        &Identity::new(SPAKE_ID_MEMBER),
        OsRng,
    )
}

/// `K = BLAKE3.derive_key(上下文, SPAKE2 共享密钥 ‖ session)`。
pub(super) fn pairing_key(shared: &[u8], session: &str) -> [u8; KEY_LEN] {
    let mut hasher = blake3::Hasher::new_derive_key(PAIRING_KEY_CONTEXT);
    hasher.update(shared);
    hasher.update(session.as_bytes());
    *hasher.finalize().as_bytes()
}

pub(super) fn seal_grant(
    key: &[u8; KEY_LEN],
    session: &str,
    grant: &JoinGrant,
) -> anyhow::Result<String> {
    Ok(to_base64(&seal(
        key,
        session.as_bytes(),
        &serde_json::to_vec(grant)?,
    )?))
}

/// 解不开说明两边的配对码不同。
pub(super) fn open_grant(
    key: &[u8; KEY_LEN],
    session: &str,
    sealed: &str,
) -> anyhow::Result<JoinGrant> {
    let plain = open(key, session.as_bytes(), &from_base64(sealed)?)
        .map_err(|_| anyhow!("join grant does not open"))?;
    Ok(serde_json::from_slice(&plain)?)
}

/// `joinProof = hex(BLAKE3(joinSecret))`，按 joinSecret 的 32 个原始字节计算（线上传的是它的 hex）。
pub(super) fn join_proof(join_secret: &[u8; KEY_LEN]) -> String {
    blake3::hash(join_secret).to_hex().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exchange(joiner_code: &str, member_code: &str) -> ([u8; KEY_LEN], [u8; KEY_LEN]) {
        let (joiner, joiner_msg) = start_joiner(joiner_code);
        let (member, member_msg) = start_member(member_code);
        let joiner_shared = joiner.finish(&member_msg).unwrap();
        let member_shared = member.finish(&joiner_msg).unwrap();
        (
            pairing_key(&joiner_shared, "session-1"),
            pairing_key(&member_shared, "session-1"),
        )
    }

    #[test]
    fn same_code_opens_the_grant() {
        let (joiner, member) = exchange("123456", "123456");
        let grant = JoinGrant {
            group_key: to_base64(&[3; KEY_LEN]),
            join_secret: "ab".repeat(32),
        };
        let sealed = seal_grant(&member, "session-1", &grant).unwrap();

        let opened = open_grant(&joiner, "session-1", &sealed).unwrap();
        assert_eq!(opened.group_key, grant.group_key);
        assert_eq!(opened.join_secret, grant.join_secret);
        assert!(open_grant(&joiner, "session-2", &sealed).is_err());
        assert_eq!(
            join_proof(&[0xab; KEY_LEN]),
            blake3::hash(&[0xab; KEY_LEN]).to_hex().to_string()
        );
    }

    #[test]
    fn wrong_code_cannot_open_the_grant() {
        let (joiner, member) = exchange("123456", "654321");
        let grant = JoinGrant {
            group_key: to_base64(&[3; KEY_LEN]),
            join_secret: "ab".repeat(32),
        };
        let sealed = seal_grant(&member, "session-1", &grant).unwrap();

        assert!(open_grant(&joiner, "session-1", &sealed).is_err());
    }
}
