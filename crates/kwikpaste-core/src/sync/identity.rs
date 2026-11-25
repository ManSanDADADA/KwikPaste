//! 本机的同步身份：一对 X25519 密钥，设备 id 由公钥派生，别人没法只改 id 冒充。
//!
//! 身份文件记着生成它的那台电脑（机器指纹的哈希）。便携版文件夹被拷到别的电脑时指纹对不上，
//! 就重新生成身份，旧的配对关系一并作废，避免两台电脑用同一个身份。

use std::fs;
use std::path::Path;

use anyhow::{anyhow, Context};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use super::protocol::{from_hex, to_hex, NOISE_PARAMS};

const IDENTITY_FILENAME: &str = "identity.json";
const IDENTITY_VERSION: u16 = 1;
const KEY_LEN: usize = 32;
/// 设备 id 取公钥哈希的前 20 个十六进制字符（80 bit），界面上也够短。
const DEVICE_ID_LEN: usize = 20;

pub struct DeviceIdentity {
    device_id: String,
    public_key: [u8; KEY_LEN],
    private_key: Zeroizing<[u8; KEY_LEN]>,
}

impl DeviceIdentity {
    pub fn device_id(&self) -> &str {
        &self.device_id
    }

    pub fn public_key(&self) -> &[u8; KEY_LEN] {
        &self.public_key
    }

    pub fn private_key(&self) -> &[u8] {
        self.private_key.as_slice()
    }
}

/// 读取结果；`regenerated` 表示已有身份不属于这台电脑（或已损坏）而重新生成，
/// 调用方要清掉旧身份下的配对关系。
pub struct LoadedIdentity {
    pub identity: DeviceIdentity,
    pub regenerated: bool,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct IdentityFile {
    version: u16,
    public_key: String,
    private_key: String,
    machine: Option<String>,
    created_at: DateTime<Utc>,
}

/// 由公钥派生设备 id。
pub fn device_id_for(public_key: &[u8]) -> String {
    blake3::hash(public_key).to_hex()[..DEVICE_ID_LEN].to_owned()
}

/// 读取 `dir` 下的身份；不存在、损坏或属于别的电脑时生成新的并落盘。
pub fn load_or_create(dir: &Path) -> anyhow::Result<LoadedIdentity> {
    fs::create_dir_all(dir).with_context(|| format!("failed to create sync dir at {dir:?}"))?;
    let path = dir.join(IDENTITY_FILENAME);
    let machine = machine_tag();

    let regenerated = match fs::read_to_string(&path) {
        Ok(content) => match parse_identity(&content) {
            Ok((identity, file_machine)) => {
                let same_machine = match (&file_machine, &machine) {
                    (Some(file), Some(current)) => file == current,
                    _ => true,
                };
                if same_machine {
                    return Ok(LoadedIdentity {
                        identity,
                        regenerated: false,
                    });
                }

                log::warn!("sync identity belongs to another machine, generating a new one");
                true
            }
            Err(err) => {
                log::warn!("sync identity unreadable, generating a new one: {err:#}");
                true
            }
        },
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => false,
        Err(err) => return Err(err).with_context(|| format!("failed to read {path:?}")),
    };

    let identity = generate()?;
    let file = IdentityFile {
        version: IDENTITY_VERSION,
        public_key: to_hex(identity.public_key()),
        private_key: to_hex(identity.private_key()),
        machine,
        created_at: Utc::now(),
    };
    write_atomic(&path, &serde_json::to_vec_pretty(&file)?)?;
    log::info!("sync identity ready: {}", identity.device_id());

    Ok(LoadedIdentity {
        identity,
        regenerated,
    })
}

fn parse_identity(content: &str) -> anyhow::Result<(DeviceIdentity, Option<String>)> {
    let file: IdentityFile = serde_json::from_str(content)?;
    let public_key: [u8; KEY_LEN] = from_hex(&file.public_key)?
        .try_into()
        .map_err(|_| anyhow!("public key has wrong length"))?;
    let private_bytes = Zeroizing::new(from_hex(&file.private_key)?);
    let private_key: [u8; KEY_LEN] = private_bytes
        .as_slice()
        .try_into()
        .map_err(|_| anyhow!("private key has wrong length"))?;

    Ok((
        DeviceIdentity {
            device_id: device_id_for(&public_key),
            public_key,
            private_key: Zeroizing::new(private_key),
        },
        file.machine,
    ))
}

fn generate() -> anyhow::Result<DeviceIdentity> {
    let params = NOISE_PARAMS
        .parse()
        .map_err(|err| anyhow!("invalid noise params: {err}"))?;
    let keypair = snow::Builder::new(params)
        .generate_keypair()
        .map_err(|err| anyhow!("failed to generate sync keypair: {err}"))?;
    let public_key: [u8; KEY_LEN] = keypair
        .public
        .as_slice()
        .try_into()
        .map_err(|_| anyhow!("generated public key has wrong length"))?;
    let private_bytes = Zeroizing::new(keypair.private);
    let private_key: [u8; KEY_LEN] = private_bytes
        .as_slice()
        .try_into()
        .map_err(|_| anyhow!("generated private key has wrong length"))?;

    Ok(DeviceIdentity {
        device_id: device_id_for(&public_key),
        public_key,
        private_key: Zeroizing::new(private_key),
    })
}

pub(super) fn write_atomic(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, bytes).with_context(|| format!("failed to write {tmp:?}"))?;
    fs::rename(&tmp, path).with_context(|| format!("failed to replace {path:?}"))?;
    Ok(())
}

/// 机器指纹的哈希；只用来判断身份文件是不是在这台电脑上生成的，取不到时不做判断。
fn machine_tag() -> Option<String> {
    let fingerprint = machine_fingerprint()?;
    let mut hasher = blake3::Hasher::new_derive_key("KwikPaste LAN sync machine tag v1");
    hasher.update(fingerprint.trim().as_bytes());
    Some(hasher.finalize().to_hex()[..32].to_owned())
}

#[cfg(target_os = "windows")]
fn machine_fingerprint() -> Option<String> {
    windows_registry::LOCAL_MACHINE
        .open(r"SOFTWARE\Microsoft\Cryptography")
        .and_then(|key| key.get_string("MachineGuid"))
        .map_err(|err| log::warn!("read MachineGuid failed: {err}"))
        .ok()
        .filter(|guid| !guid.trim().is_empty())
}

#[cfg(target_os = "macos")]
fn machine_fingerprint() -> Option<String> {
    #[repr(C)]
    struct Timespec {
        tv_sec: i64,
        tv_nsec: i64,
    }

    extern "C" {
        fn gethostuuid(id: *mut u8, wait: *const Timespec) -> i32;
    }

    let mut id = [0u8; 16];
    let wait = Timespec {
        tv_sec: 1,
        tv_nsec: 0,
    };
    // SAFETY: `id` 是 gethostuuid 要求的 16 字节 uuid_t 缓冲区，`wait` 在调用期间有效。
    let result = unsafe { gethostuuid(id.as_mut_ptr(), &wait) };
    if result != 0 {
        log::warn!("gethostuuid failed: {result}");
        return None;
    }

    Some(to_hex(&id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_then_reloads_same_identity() {
        let dir = tempfile::tempdir().unwrap();

        let first = load_or_create(dir.path()).unwrap();
        let second = load_or_create(dir.path()).unwrap();

        assert!(!first.regenerated);
        assert!(!second.regenerated);
        assert_eq!(first.identity.device_id(), second.identity.device_id());
        assert_eq!(
            first.identity.device_id(),
            device_id_for(first.identity.public_key())
        );
    }

    #[test]
    fn regenerates_identity_from_another_machine() {
        let dir = tempfile::tempdir().unwrap();
        let first = load_or_create(dir.path()).unwrap();
        let path = dir.path().join(IDENTITY_FILENAME);
        let mut file: IdentityFile =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        file.machine = Some("another-machine".to_owned());
        fs::write(&path, serde_json::to_vec(&file).unwrap()).unwrap();

        let second = load_or_create(dir.path()).unwrap();

        if machine_tag().is_some() {
            assert!(second.regenerated);
            assert_ne!(first.identity.device_id(), second.identity.device_id());
        }
    }

    #[test]
    fn regenerates_corrupted_identity() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(IDENTITY_FILENAME), "not json").unwrap();

        let loaded = load_or_create(dir.path()).unwrap();

        assert!(loaded.regenerated);
    }
}
