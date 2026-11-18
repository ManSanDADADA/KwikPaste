//! AI 接口的配置文件 `extensions/translate/state/ai.json`。只在本机：不进 `settings.json`
//! （它会进备份，反馈上报还会带上改过的设置项），卸载扩展时随 `state/` 一起删除。

use std::io::Write as _;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::paths::CorePaths;

/// 故意不实现 `Debug`：里面有 API Key，不能被 `{:?}` 带进日志。
#[derive(Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct TranslateAiConfig {
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    /// 空串表示用扩展内置的提示词。
    pub prompt: String,
}

impl TranslateAiConfig {
    /// 地址和模型都填了才能发请求；本机接口（如 Ollama）不需要 Key。
    pub fn complete(&self) -> bool {
        !self.base_url.trim().is_empty() && !self.model.trim().is_empty()
    }
}

fn path(paths: &CorePaths) -> PathBuf {
    paths
        .extensions_dir()
        .join("translate")
        .join("state")
        .join("ai.json")
}

/// 文件不存在或读不懂时是空配置。
pub(super) fn load(paths: &CorePaths) -> TranslateAiConfig {
    let Ok(bytes) = std::fs::read(path(paths)) else {
        return TranslateAiConfig::default();
    };
    serde_json::from_slice(&bytes).unwrap_or_else(|err| {
        log::warn!("translate AI config is unreadable: {err}");
        TranslateAiConfig::default()
    })
}

/// 先写同目录的临时文件再原子替换。
pub(super) fn save(paths: &CorePaths, config: &TranslateAiConfig) -> anyhow::Result<()> {
    let path = path(paths);
    let dir = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("translate state directory is missing"))?;
    std::fs::create_dir_all(dir)?;
    let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
    tmp.write_all(&serde_json::to_vec_pretty(config)?)?;
    tmp.as_file().sync_all()?;
    tmp.persist(&path)?;
    Ok(())
}
