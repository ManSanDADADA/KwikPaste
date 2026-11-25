//! 本机的远程同步状态 `<bootstrap>/sync/remote.json`：服务器、凭据、组密钥与游标。
//!
//! 和局域网身份放在一起，不随数据目录搬走，不进 `settings.json`、备份和反馈上报。

use std::fs;
use std::path::Path;

use anyhow::Context;
use serde::{Deserialize, Serialize};

use super::client::official_url;
use super::RemoteServerView;

const FILENAME: &str = "remote.json";

/// 同步服务器。官方服务器的地址不落盘，调试构建可以用环境变量换掉。
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub(super) enum ServerConfig {
    Official,
    SelfHost { url: String, token: String },
}

impl ServerConfig {
    pub fn base_url(&self) -> String {
        match self {
            Self::Official => official_url(),
            Self::SelfHost { url, .. } => url.clone(),
        }
    }

    pub fn server_token(&self) -> Option<&str> {
        match self {
            Self::Official => None,
            Self::SelfHost { token, .. } => Some(token),
        }
    }

    pub fn view(&self) -> RemoteServerView {
        match self {
            Self::Official => RemoteServerView::Official,
            Self::SelfHost { url, .. } => RemoteServerView::SelfHost { url: url.clone() },
        }
    }
}

/// `remote.json` 的内容。含凭据与组密钥，不实现 `Debug`。
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RemoteFile {
    pub server: ServerConfig,
    pub group_id: String,
    /// 加入时的局域网身份设备 id；身份重新生成（便携版拷到别的电脑）后凭据作废。
    pub device_id: String,
    pub device_token: String,
    /// base64 的组密钥。
    pub group_key: String,
    /// 已经收齐的服务器序号。
    #[serde(default)]
    pub cursor: u64,
    /// 第一次补齐完成：之后才上传设置差异，连接也改用 `items=all`。
    #[serde(default)]
    pub caught_up: bool,
    /// 上次登记到服务器的设备名。
    #[serde(default)]
    pub device_name: Option<String>,
    /// 「同步全部历史」打开后整个历史已经上传完。
    #[serde(default)]
    pub history_uploaded: bool,
}

/// 读不到或读不懂时当作没有加入同步组（读不懂的记日志）。
pub(super) fn load(dir: &Path) -> Option<RemoteFile> {
    let path = dir.join(FILENAME);
    let content = match fs::read_to_string(&path) {
        Ok(content) => content,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return None,
        Err(err) => {
            log::warn!("remote sync state unreadable: {err}");
            return None;
        }
    };
    serde_json::from_str(&content)
        .map_err(|err| log::warn!("remote sync state is invalid: {err}"))
        .ok()
}

pub(super) fn save(dir: &Path, file: &RemoteFile) -> anyhow::Result<()> {
    fs::create_dir_all(dir).with_context(|| format!("failed to create sync dir at {dir:?}"))?;
    super::super::identity::write_atomic(&dir.join(FILENAME), &serde_json::to_vec_pretty(file)?)
}

pub(super) fn remove(dir: &Path) -> anyhow::Result<()> {
    match fs::remove_file(dir.join(FILENAME)) {
        Err(err) if err.kind() != std::io::ErrorKind::NotFound => {
            Err(err).context("failed to remove remote sync state")
        }
        _ => Ok(()),
    }
}
