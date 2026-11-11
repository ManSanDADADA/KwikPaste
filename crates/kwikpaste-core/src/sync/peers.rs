//! 已配对设备。
//!
//! 和身份文件放在同一目录（不进备份包）。移除的设备只留名称，已收到的记录仍能显示「来自 xxx」。

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::Context;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::db::models::Platform;

const PEERS_FILENAME: &str = "peers.json";
const PEERS_VERSION: u16 = 1;
/// 水位线落盘的最短间隔。实时检查点每次复制都会推进它；没落盘的推进最多让下次补齐
/// 多发几条，对方按内容去重。
const WATERMARK_SAVE_INTERVAL: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrustedPeer {
    pub device_id: String,
    /// 配对时固定下来的 X25519 公钥（hex）；之后每次连接都必须是这把钥匙。
    pub public_key: String,
    pub name: String,
    pub platform: Platform,
    /// 最近一次连上时对方的 `ip:监听端口`，mDNS 不通时直接用它重连。
    pub address: Option<String>,
    pub paired_at: DateTime<Utc>,
    pub last_seen_at: Option<DateTime<Utc>>,
    /// 补齐水位线：已经收齐的对方本机序号（对方的 `sync_seq`），重连时请求这之后的记录。
    /// 只在一页补齐全部入库后推进，不用任何一边的时钟。
    #[serde(default)]
    pub watermark: u64,
    /// 水位线属于对方哪一代序号。对方的数据库重建过（重装、清空数据）序号会从头编，
    /// 代次跟着变，这时水位线作废、从头补齐。
    #[serde(default)]
    pub epoch: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FormerPeer {
    device_id: String,
    name: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct PeersFile {
    version: u16,
    peers: Vec<TrustedPeer>,
    former: Vec<FormerPeer>,
}

pub struct PeerStore {
    path: PathBuf,
    state: Mutex<PeersFile>,
    /// 上次因为水位线落盘的时间；间隔内的推进只改内存，`watermark_pending` 记着还没写下去。
    watermark_saved_at: Mutex<Option<Instant>>,
    watermark_pending: AtomicBool,
}

impl PeerStore {
    /// 读取 `dir` 下的已配对设备；文件不存在或损坏时从空列表开始。
    pub fn load(dir: &Path) -> Self {
        let path = dir.join(PEERS_FILENAME);
        let state = match fs::read_to_string(&path) {
            Ok(content) => serde_json::from_str(&content).unwrap_or_else(|err| {
                log::warn!("sync peers file unreadable, starting empty: {err}");
                PeersFile::default()
            }),
            Err(_) => PeersFile::default(),
        };

        Self {
            path,
            state: Mutex::new(state),
            watermark_saved_at: Mutex::new(None),
            watermark_pending: AtomicBool::new(false),
        }
    }

    pub fn list(&self) -> Vec<TrustedPeer> {
        self.lock().peers.clone()
    }

    pub fn get(&self, device_id: &str) -> Option<TrustedPeer> {
        self.lock()
            .peers
            .iter()
            .find(|peer| peer.device_id == device_id)
            .cloned()
    }

    /// 是否是已配对设备，且公钥与配对时一致。
    pub fn is_trusted(&self, device_id: &str, public_key_hex: &str) -> bool {
        self.lock()
            .peers
            .iter()
            .any(|peer| peer.device_id == device_id && peer.public_key == public_key_hex)
    }

    /// 记录一台新配对（或重新配对）的设备。
    pub fn add(&self, peer: TrustedPeer) {
        let mut state = self.lock();
        state
            .former
            .retain(|former| former.device_id != peer.device_id);
        state
            .peers
            .retain(|existing| existing.device_id != peer.device_id);
        state.peers.push(peer);
        self.save(&state);
    }

    /// 连上后更新对方的名称、平台和地址。
    pub fn touch(&self, device_id: &str, name: &str, platform: Platform, address: Option<String>) {
        let mut state = self.lock();
        let Some(peer) = state
            .peers
            .iter_mut()
            .find(|peer| peer.device_id == device_id)
        else {
            return;
        };

        peer.name = name.to_owned();
        peer.platform = platform;
        if address.is_some() {
            peer.address = address;
        }
        peer.last_seen_at = Some(Utc::now());
        self.save(&state);
    }

    /// 补齐水位线：同一代序号里只往前走；对方换了一代时整个替换。
    pub fn advance_watermark(&self, device_id: &str, epoch: &str, until: u64) {
        let mut state = self.lock();
        let Some(peer) = state
            .peers
            .iter_mut()
            .find(|peer| peer.device_id == device_id)
        else {
            return;
        };

        if peer.epoch.as_deref() == Some(epoch) {
            if peer.watermark >= until {
                return;
            }
        } else {
            peer.epoch = Some(epoch.to_owned());
        }

        peer.watermark = until;
        let mut saved_at = self
            .watermark_saved_at
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        if saved_at.is_some_and(|at| at.elapsed() < WATERMARK_SAVE_INTERVAL) {
            self.watermark_pending.store(true, Ordering::Relaxed);
            return;
        }
        *saved_at = Some(Instant::now());
        drop(saved_at);
        self.save(&state);
    }

    /// 把间隔内只改了内存的水位线写下去；同步运行时停止时调用。
    pub fn flush(&self) {
        let state = self.lock();
        if self.watermark_pending.load(Ordering::Relaxed) {
            self.save(&state);
        }
    }

    /// 移除配对，名称留作历史记录的来源显示。
    pub fn remove(&self, device_id: &str) -> Option<TrustedPeer> {
        let mut state = self.lock();
        let index = state
            .peers
            .iter()
            .position(|peer| peer.device_id == device_id)?;
        let peer = state.peers.remove(index);
        state.former.retain(|former| former.device_id != device_id);
        state.former.push(FormerPeer {
            device_id: peer.device_id.clone(),
            name: peer.name.clone(),
        });
        self.save(&state);
        Some(peer)
    }

    /// 本机身份换了，旧的配对全部失效。
    pub fn forget_all(&self) {
        let mut state = self.lock();
        let peers = std::mem::take(&mut state.peers);
        for peer in peers {
            state
                .former
                .retain(|former| former.device_id != peer.device_id);
            state.former.push(FormerPeer {
                device_id: peer.device_id,
                name: peer.name,
            });
        }
        self.save(&state);
    }

    /// 记录来源设备的显示名称，包括已移除的设备。
    pub fn display_name(&self, device_id: &str) -> Option<String> {
        let state = self.lock();
        state
            .peers
            .iter()
            .find(|peer| peer.device_id == device_id)
            .map(|peer| peer.name.clone())
            .or_else(|| {
                state
                    .former
                    .iter()
                    .find(|former| former.device_id == device_id)
                    .map(|former| former.name.clone())
            })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, PeersFile> {
        self.state.lock().expect("sync peers poisoned")
    }

    fn save(&self, state: &PeersFile) {
        self.watermark_pending.store(false, Ordering::Relaxed);
        if let Err(err) = self.write(state) {
            log::warn!("save sync peers failed: {err:#}");
        }
    }

    fn write(&self, state: &PeersFile) -> anyhow::Result<()> {
        if let Some(dir) = self.path.parent() {
            fs::create_dir_all(dir).with_context(|| format!("failed to create {dir:?}"))?;
        }

        let file = PeersFile {
            version: PEERS_VERSION,
            peers: state.peers.clone(),
            former: state.former.clone(),
        };
        let tmp = self.path.with_extension("json.tmp");
        fs::write(&tmp, serde_json::to_vec_pretty(&file)?)
            .with_context(|| format!("failed to write {tmp:?}"))?;
        fs::rename(&tmp, &self.path)
            .with_context(|| format!("failed to replace {:?}", self.path))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peer(id: &str) -> TrustedPeer {
        TrustedPeer {
            device_id: id.to_owned(),
            public_key: format!("key-{id}"),
            name: format!("name-{id}"),
            platform: Platform::Windows,
            address: None,
            paired_at: Utc::now(),
            last_seen_at: None,
            watermark: 0,
            epoch: None,
        }
    }

    #[test]
    fn persists_and_checks_public_key() {
        let dir = tempfile::tempdir().unwrap();
        PeerStore::load(dir.path()).add(peer("a"));

        let store = PeerStore::load(dir.path());

        assert!(store.is_trusted("a", "key-a"));
        assert!(!store.is_trusted("a", "other-key"));
        assert!(!store.is_trusted("b", "key-a"));
    }

    #[test]
    fn removed_peer_keeps_display_name() {
        let dir = tempfile::tempdir().unwrap();
        let store = PeerStore::load(dir.path());
        store.add(peer("a"));

        store.remove("a");

        assert!(store.get("a").is_none());
        assert_eq!(store.display_name("a").as_deref(), Some("name-a"));
    }

    #[test]
    fn watermark_only_moves_forward_and_persists() {
        let dir = tempfile::tempdir().unwrap();
        let store = PeerStore::load(dir.path());
        store.add(peer("a"));

        store.advance_watermark("a", "e1", 40);
        store.advance_watermark("a", "e1", 12);

        assert_eq!(store.get("a").unwrap().watermark, 40);
        let reloaded = PeerStore::load(dir.path()).get("a").unwrap();
        assert_eq!(
            (reloaded.epoch.as_deref(), reloaded.watermark),
            (Some("e1"), 40)
        );

        // 对方序号换了一代：水位线可以回到更小的值。
        store.advance_watermark("a", "e2", 3);
        let peer = store.get("a").unwrap();
        assert_eq!((peer.epoch.as_deref(), peer.watermark), (Some("e2"), 3));
        // 间隔内的推进先只改内存，停止时再写下去。
        let on_disk = PeerStore::load(dir.path()).get("a").unwrap();
        assert_eq!(on_disk.watermark, 40);
        store.flush();
        let on_disk = PeerStore::load(dir.path()).get("a").unwrap();
        assert_eq!(
            (on_disk.epoch.as_deref(), on_disk.watermark),
            (Some("e2"), 3)
        );
    }
}
