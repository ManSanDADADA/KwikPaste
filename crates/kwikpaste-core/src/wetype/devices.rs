//! 宿主持有的设备名缓存，保留已离线或被手机移除的设备名称。
use std::collections::BTreeMap;
use std::io::Write;
use std::path::PathBuf;
use std::sync::RwLock;

use kwikpaste_ext_protocol::WeTypeDevice;

pub(crate) struct DeviceNames {
    path: PathBuf,
    names: RwLock<BTreeMap<u64, String>>,
}

impl DeviceNames {
    pub fn load(path: PathBuf) -> Self {
        let names = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|err| {
                log::warn!("WeType device cache invalid: {err}");
                BTreeMap::new()
            }),
            Err(err) => {
                if err.kind() != std::io::ErrorKind::NotFound {
                    log::warn!("WeType device cache read failed: {err}");
                }
                BTreeMap::new()
            }
        };
        Self {
            path,
            names: RwLock::new(names),
        }
    }

    /// 每次 State/Received 合并，不移除旧名字；原子保存到宿主目录。返回名字是否有变化。
    pub fn update(&self, devices: &[WeTypeDevice]) -> anyhow::Result<bool> {
        let mut names = self.names.write().unwrap_or_else(|p| p.into_inner());
        let mut next = names.clone();
        for device in devices {
            if !device.name.trim().is_empty() {
                next.insert(device.uin, device.name.clone());
            }
        }
        if *names == next {
            return Ok(false);
        }
        let dir = self
            .path
            .parent()
            .ok_or_else(|| anyhow::anyhow!("device cache parent missing"))?;
        std::fs::create_dir_all(dir)?;
        let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
        tmp.write_all(&serde_json::to_vec_pretty(&next)?)?;
        tmp.as_file().sync_all()?;
        tmp.persist(&self.path)?;
        *names = next;
        Ok(true)
    }

    pub fn display_name(&self, uin: &str) -> Option<String> {
        self.names
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .get(&uin.parse().ok()?)
            .cloned()
    }

    pub fn clear(&self) {
        self.names
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .clear();
    }
}
