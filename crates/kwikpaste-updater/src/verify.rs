//! minisign 验签，与 Tauri 的 updater 插件相同：公钥和签名都是「minisign 文本文件」再 base64 一层，
//! 允许旧式（不预哈希）签名。
//!
//! 公钥编进二进制，值与 `src-tauri/tauri.conf.json` 的 `plugins.updater.pubkey` 相同（1.x 与 2.x
//! 用同一把 minisign 私钥签名）；测试断言两边一致。

use anyhow::{Context, anyhow};
use base64::Engine;
use minisign_verify::{PublicKey, Signature};

/// 生产公钥（base64 的 minisign 公钥文件）。
const PRODUCTION_PUBLIC_KEY: &str = include_str!("../pubkey.txt");

/// 本次运行用的公钥：默认是生产公钥；只有编进 `e2e-overrides` 时才能用环境变量换成测试公钥。
pub(crate) fn public_key() -> String {
    crate::overrides::var(crate::overrides::PUBLIC_KEY)
        .unwrap_or_else(|| PRODUCTION_PUBLIC_KEY.trim().to_owned())
}

/// 用 `public_key`（base64 的公钥文件）校验 `data` 的 `signature`（base64 的签名文件），返回签名时的文件名。
///
/// `tauri signer sign` 和 `tauri bundle` 写的 trusted comment 是 `timestamp:<秒>\tfile:<文件名>`，受全局签名
/// 保护。同一把密钥签 1.x、2.x 和扩展，调用方必须拿文件名确认签的就是期望的那个发布产物。
pub(crate) fn verify(data: &[u8], signature: &str, public_key: &str) -> anyhow::Result<String> {
    let public_key = PublicKey::decode(&decode_text(public_key).context("invalid public key")?)
        .map_err(|err| anyhow!("invalid public key: {err}"))?;
    let signature = Signature::decode(&decode_text(signature).context("invalid signature")?)
        .map_err(|err| anyhow!("invalid signature: {err}"))?;
    public_key
        .verify(data, &signature, true)
        .map_err(|err| anyhow!("signature verification failed: {err}"))?;
    signature
        .trusted_comment()
        .split('\t')
        .find_map(|field| field.strip_prefix("file:"))
        .map(str::to_owned)
        .ok_or_else(|| anyhow!("signature does not name the signed file"))
}

fn decode_text(value: &str) -> anyhow::Result<String> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(value.trim())
        .context("not base64")?;
    String::from_utf8(bytes).context("not utf-8")
}

/// 测试用的一次性 minisign 密钥：每次在内存里新生成，绝不落盘，与生产私钥无关。
#[cfg(test)]
pub(crate) mod testing {
    use base64::Engine;
    use blake2::{Blake2b512, Digest};
    use ed25519_dalek::{Signer, SigningKey};
    use rand::RngCore;

    pub(crate) struct TestKey {
        signing: SigningKey,
        key_id: [u8; 8],
    }

    impl TestKey {
        pub(crate) fn generate() -> Self {
            let mut secret = [0u8; 32];
            let mut key_id = [0u8; 8];
            rand::rngs::OsRng.fill_bytes(&mut secret);
            rand::rngs::OsRng.fill_bytes(&mut key_id);
            Self {
                signing: SigningKey::from_bytes(&secret),
                key_id,
            }
        }

        /// 与 `tauri.conf.json` 同格式的公钥。
        pub(crate) fn public_key(&self) -> String {
            let mut bin = b"Ed".to_vec();
            bin.extend_from_slice(&self.key_id);
            bin.extend_from_slice(self.signing.verifying_key().as_bytes());
            let text = format!(
                "untrusted comment: minisign public key: {}\n{}\n",
                hex(&self.key_id),
                b64(&bin)
            );
            b64(text.as_bytes())
        }

        /// 与清单 `signature` 字段同格式的签名；`prehashed` 对应 minisign 的新格式。签名记的文件名无关紧要时用。
        pub(crate) fn sign(&self, data: &[u8], prehashed: bool) -> String {
            self.sign_with(data, prehashed, "KwikPaste-test")
        }

        /// 像 `tauri signer sign <file>` 一样把文件名写进 trusted comment。
        pub(crate) fn sign_as(&self, data: &[u8], file: &str) -> String {
            self.sign_with(data, true, file)
        }

        fn sign_with(&self, data: &[u8], prehashed: bool, file: &str) -> String {
            let (algorithm, message) = if prehashed {
                (b"ED", Blake2b512::digest(data).to_vec())
            } else {
                (b"Ed", data.to_vec())
            };
            let signature = self.signing.sign(&message).to_bytes();
            let mut bin = algorithm.to_vec();
            bin.extend_from_slice(&self.key_id);
            bin.extend_from_slice(&signature);

            let trusted = format!("timestamp:1790000000\tfile:{file}");
            let mut global = signature.to_vec();
            global.extend_from_slice(trusted.as_bytes());
            let global = self.signing.sign(&global).to_bytes();

            let text = format!(
                "untrusted comment: signature from a throwaway test key\n{}\ntrusted comment: {trusted}\n{}\n",
                b64(&bin),
                b64(&global)
            );
            b64(text.as_bytes())
        }
    }

    fn b64(bytes: &[u8]) -> String {
        base64::engine::general_purpose::STANDARD.encode(bytes)
    }

    fn hex(bytes: &[u8]) -> String {
        bytes
            .iter()
            .rev()
            .map(|byte| format!("{byte:02X}"))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::testing::TestKey;
    use super::*;

    #[test]
    fn prehashed_and_legacy_signatures_verify() {
        let key = TestKey::generate();
        let data = b"MZ installer bytes";

        for prehashed in [true, false] {
            let signature = key.sign(data, prehashed);
            assert_eq!(
                verify(data, &signature, &key.public_key()).unwrap(),
                "KwikPaste-test"
            );
            assert!(verify(b"MZ tampered bytes", &signature, &key.public_key()).is_err());
        }
        let signature = key.sign_as(data, "KwikPaste_2.0.1_x64-setup.exe");
        assert_eq!(
            verify(data, &signature, &key.public_key()).unwrap(),
            "KwikPaste_2.0.1_x64-setup.exe"
        );
    }

    #[test]
    fn signatures_from_another_key_are_rejected() {
        let key = TestKey::generate();
        let other = TestKey::generate();
        let signature = other.sign(b"data", true);

        assert!(verify(b"data", &signature, &key.public_key()).is_err());
        assert!(verify(b"data", "not base64!", &key.public_key()).is_err());
        assert!(verify(b"data", &signature, "bm90IGEga2V5").is_err());
    }

    /// 编进二进制的公钥必须与 1.x 的 `tauri.conf.json` 相同：两边用同一把私钥签名。
    #[test]
    fn production_key_matches_tauri_conf() {
        let conf = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../src-tauri/tauri.conf.json");
        let conf: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(conf).unwrap()).unwrap();
        let expected = conf["plugins"]["updater"]["pubkey"].as_str().unwrap();

        assert_eq!(PRODUCTION_PUBLIC_KEY.trim(), expected);
        PublicKey::decode(&decode_text(PRODUCTION_PUBLIC_KEY).unwrap()).unwrap();
        if !cfg!(feature = "e2e-overrides") {
            assert_eq!(public_key(), expected);
        }
    }
}
