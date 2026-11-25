//! 同步服务器的 HTTP 接口与帧编解码（REMOTE.md §2–§4）。

use std::time::Duration;

use anyhow::{anyhow, Context};
use chrono::{DateTime, Utc};
use reqwest::header::{HeaderMap, RETRY_AFTER};
use reqwest::{Method, RequestBuilder, StatusCode};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::state::ServerConfig;

/// 官方服务器。
pub(super) const OFFICIAL_URL: &str = "https://sync.fastthree.com";
/// 本机支持的协议版本。
pub(super) const PROTOCOL: u32 = 1;
/// 帧头上限。
pub(super) const MAX_HEADER_BYTES: usize = 64 * 1024;
/// 一次上传请求的体积上限（官方服务器的限制，自部署服务器也按它分批）。
pub(super) const MAX_REQUEST_BYTES: usize = 8 * 1024 * 1024;
/// 事件流 300 秒没收到任何字节就断开重连（服务器每 240 秒发一次 ping）。
pub(super) const STREAM_READ_TIMEOUT: Duration = Duration::from_secs(300);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// 加入请求在服务器上最多等邀请方 60 秒。
const JOIN_TIMEOUT: Duration = Duration::from_secs(90);
const FRAMES_CONTENT_TYPE: &str = "application/x-kwikpaste-frames";

/// 官方服务器地址；只有调试构建认 `KWIKPASTE_SYNC_OFFICIAL_URL`，本机跑服务端做端到端测试用。
pub(super) fn official_url() -> String {
    #[cfg(debug_assertions)]
    if let Ok(url) = std::env::var("KWIKPASTE_SYNC_OFFICIAL_URL") {
        let url = url.trim().trim_end_matches('/');
        if !url.is_empty() {
            return url.to_owned();
        }
    }
    OFFICIAL_URL.to_owned()
}

/// 自部署服务器的地址：必须是 http(s) 地址，去掉末尾的 `/`。
pub(super) fn normalize_url(input: &str) -> Option<String> {
    let url = input.trim().trim_end_matches('/');
    let parsed = reqwest::Url::parse(url).ok()?;
    (matches!(parsed.scheme(), "http" | "https") && parsed.host_str().is_some())
        .then(|| url.to_owned())
}

/// 请求失败：网络不通、服务器回了错误，或者回的内容读不懂。
#[derive(Debug)]
pub(super) enum ApiError {
    Network(String),
    Server {
        status: StatusCode,
        code: String,
        message: String,
        retry_after: Option<Duration>,
    },
    Invalid(String),
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Network(message) => write!(f, "network error: {message}"),
            Self::Server {
                status,
                code,
                message,
                ..
            } => write!(f, "server error {status} {code}: {message}"),
            Self::Invalid(message) => write!(f, "invalid response: {message}"),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct VersionRange {
    pub min: u32,
    pub max: u32,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ServerInfo {
    pub protocol: VersionRange,
    #[serde(default)]
    pub policy: PolicyWire,
}

/// `/v1/info` 的 `policy`；缺字段按最保守的取值。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(super) struct PolicyWire {
    pub history: bool,
    pub meta: bool,
    pub settings: bool,
    pub text_retention_hours: Option<u32>,
    pub images: String,
    pub image_max_bytes: u64,
    pub image_compress: Option<CompressWire>,
    pub max_entry_bytes: u64,
    pub max_devices: u32,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct CompressWire {
    pub max_side: u32,
    pub quality: u8,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Credentials {
    pub group_id: String,
    pub device_token: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct PairingReply {
    pub code: String,
    pub expires_at: DateTime<Utc>,
}

#[derive(Deserialize)]
pub(super) struct JoinReply {
    pub session: String,
    pub spake: String,
    pub sealed: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct PairingAnswer {
    pub spake: String,
    pub sealed: String,
    pub join_proof: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct DeviceRow {
    pub device_id: String,
    pub info: String,
    #[serde(default)]
    pub last_seen_at: Option<String>,
    #[serde(default)]
    pub online: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct UploadResult {
    pub entry_id: String,
    pub status: String,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub retry_at: Option<DateTime<Utc>>,
}

#[derive(Deserialize)]
struct UploadReply {
    results: Vec<UploadResult>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ErrorBody {
    #[serde(default)]
    error: String,
    #[serde(default)]
    message: String,
}

/// 一台服务器的客户端；带上设备令牌后才能调成员接口。
#[derive(Clone)]
pub(super) struct Client {
    http: reqwest::Client,
    base: String,
    server_token: Option<String>,
    device_token: Option<String>,
    client_header: String,
}

impl Client {
    pub fn new(
        server: &ServerConfig,
        device_token: Option<String>,
        version: &semver::Version,
    ) -> anyhow::Result<Self> {
        let http = reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .build()
            .context("failed to build the sync client")?;
        Ok(Self {
            http,
            base: server.base_url(),
            server_token: server.server_token().map(str::to_owned),
            device_token,
            client_header: format!("KwikPaste/{version} ({})", platform_name()),
        })
    }

    fn request(&self, method: Method, path: &str) -> RequestBuilder {
        let mut builder = self
            .http
            .request(method, format!("{}{path}", self.base))
            .header("X-KwikPaste-Client", &self.client_header);
        if let Some(token) = &self.server_token {
            builder = builder.header("X-KwikPaste-Server-Token", token);
        }
        if let Some(token) = &self.device_token {
            builder = builder.bearer_auth(token);
        }
        builder
    }

    /// 发出请求；非 2xx 转成 [`ApiError::Server`]。
    async fn send(builder: RequestBuilder) -> Result<reqwest::Response, ApiError> {
        let response = builder
            .send()
            .await
            .map_err(|err| ApiError::Network(err.to_string()))?;
        let status = response.status();
        if status.is_success() {
            return Ok(response);
        }

        let retry_after = retry_after(response.headers());
        let body = response.bytes().await.unwrap_or_default();
        let parsed = serde_json::from_slice::<ErrorBody>(&body).ok();
        Err(ApiError::Server {
            status,
            code: parsed
                .as_ref()
                .map(|body| body.error.clone())
                .unwrap_or_default(),
            message: parsed.map(|body| body.message).unwrap_or_default(),
            retry_after,
        })
    }

    async fn json<T: DeserializeOwned>(builder: RequestBuilder) -> Result<T, ApiError> {
        let body = Self::send(builder)
            .await?
            .bytes()
            .await
            .map_err(|err| ApiError::Network(err.to_string()))?;
        serde_json::from_slice(&body).map_err(|err| ApiError::Invalid(err.to_string()))
    }

    async fn empty(builder: RequestBuilder) -> Result<(), ApiError> {
        Self::send(builder).await.map(drop)
    }

    pub async fn info(&self) -> Result<ServerInfo, ApiError> {
        Self::json(
            self.request(Method::GET, "/v1/info")
                .timeout(REQUEST_TIMEOUT),
        )
        .await
    }

    pub async fn create_group(&self, device_id: &str, info: &str) -> Result<Credentials, ApiError> {
        Self::json(
            self.request(Method::POST, "/v1/groups")
                .timeout(REQUEST_TIMEOUT)
                .with_json(&json!({ "deviceId": device_id, "info": info })),
        )
        .await
    }

    pub async fn request_pairing(&self) -> Result<PairingReply, ApiError> {
        Self::json(
            self.request(Method::POST, "/v1/pairing")
                .timeout(REQUEST_TIMEOUT),
        )
        .await
    }

    pub async fn cancel_pairing(&self) -> Result<(), ApiError> {
        Self::empty(
            self.request(Method::DELETE, "/v1/pairing")
                .timeout(REQUEST_TIMEOUT),
        )
        .await
    }

    pub async fn join(
        &self,
        code: &str,
        device_id: &str,
        spake: &str,
    ) -> Result<JoinReply, ApiError> {
        Self::json(
            self.request(Method::POST, "/v1/join")
                .timeout(JOIN_TIMEOUT)
                .with_json(&json!({ "code": code, "deviceId": device_id, "spake": spake })),
        )
        .await
    }

    pub async fn answer_pairing(
        &self,
        session: &str,
        answer: &PairingAnswer,
    ) -> Result<(), ApiError> {
        Self::empty(
            self.request(Method::POST, &format!("/v1/pairing/{session}/reply"))
                .timeout(REQUEST_TIMEOUT)
                .with_json(answer),
        )
        .await
    }

    pub async fn complete_join(
        &self,
        session: &str,
        join_secret: &str,
        info: &str,
    ) -> Result<Credentials, ApiError> {
        Self::json(
            self.request(Method::POST, &format!("/v1/join/{session}/complete"))
                .timeout(REQUEST_TIMEOUT)
                .with_json(&json!({ "joinSecret": join_secret, "info": info })),
        )
        .await
    }

    pub async fn devices(&self) -> Result<Vec<DeviceRow>, ApiError> {
        Self::json(
            self.request(Method::GET, "/v1/devices")
                .timeout(REQUEST_TIMEOUT),
        )
        .await
    }

    pub async fn update_self(&self, info: &str) -> Result<(), ApiError> {
        Self::empty(
            self.request(Method::PUT, "/v1/devices/self")
                .timeout(REQUEST_TIMEOUT)
                .with_json(&json!({ "info": info })),
        )
        .await
    }

    pub async fn remove_device(&self, device_id: &str) -> Result<(), ApiError> {
        Self::empty(
            self.request(Method::DELETE, &format!("/v1/devices/{device_id}"))
                .timeout(REQUEST_TIMEOUT),
        )
        .await
    }

    pub async fn upload(&self, body: Vec<u8>) -> Result<Vec<UploadResult>, ApiError> {
        let reply: UploadReply = Self::json(
            self.request(Method::POST, "/v1/entries")
                .header(reqwest::header::CONTENT_TYPE, FRAMES_CONTENT_TYPE)
                // 体积按 8 MiB 分批，慢网络上一批要多给点时间。
                .timeout(REQUEST_TIMEOUT * 4)
                .body(body),
        )
        .await?;
        Ok(reply.results)
    }

    /// 打开事件流；正文由调用方逐块读（每块之间最多等 [`STREAM_READ_TIMEOUT`]）。
    pub async fn events(&self, after: u64, all_items: bool) -> Result<reqwest::Response, ApiError> {
        let items = if all_items { "all" } else { "none" };
        Self::send(self.request(
            Method::GET,
            &format!("/v1/events?after={after}&items={items}"),
        ))
        .await
    }
}

/// reqwest 没开 `json` 特性：自己序列化请求体。
trait WithJson {
    fn with_json(self, value: &impl Serialize) -> Self;
}

impl WithJson for RequestBuilder {
    fn with_json(self, value: &impl Serialize) -> Self {
        self.header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(serde_json::to_vec(value).unwrap_or_default())
    }
}

fn platform_name() -> &'static str {
    #[cfg(target_os = "macos")]
    {
        "macos"
    }
    #[cfg(not(target_os = "macos"))]
    {
        "windows"
    }
}

fn retry_after(headers: &HeaderMap) -> Option<Duration> {
    headers
        .get(RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse()
        .ok()
        .map(Duration::from_secs)
}

/// 追加一帧：`u32 大端头长度 ‖ JSON 头 ‖ u32 大端体长度 ‖ 体`。
pub(super) fn encode_frame(out: &mut Vec<u8>, header: &[u8], body: &[u8]) -> anyhow::Result<()> {
    if header.len() > MAX_HEADER_BYTES {
        return Err(anyhow!("frame header is too large"));
    }
    let body_len = u32::try_from(body.len()).context("frame body is too large")?;
    out.extend_from_slice(&(header.len() as u32).to_be_bytes());
    out.extend_from_slice(header);
    out.extend_from_slice(&body_len.to_be_bytes());
    out.extend_from_slice(body);
    Ok(())
}

/// 从事件流里逐块拼出完整的帧。头超过 64 KiB 或体超过上限时报错，调用方断开重连。
pub(super) struct FrameReader {
    buf: Vec<u8>,
    pos: usize,
    max_body: usize,
}

impl FrameReader {
    pub fn new(max_body: usize) -> Self {
        Self {
            buf: Vec::new(),
            pos: 0,
            max_body,
        }
    }

    pub fn push(&mut self, chunk: &[u8]) {
        self.buf.extend_from_slice(chunk);
    }

    /// 下一个完整的帧 `(头, 体)`；还不完整时返回 `None`。
    pub fn next_frame(&mut self) -> anyhow::Result<Option<(Vec<u8>, Vec<u8>)>> {
        let data = &self.buf[self.pos..];
        let Some(header_len) = read_len(data, 0) else {
            return Ok(None);
        };
        if header_len > MAX_HEADER_BYTES {
            return Err(anyhow!("frame header of {header_len} bytes is too large"));
        }
        let Some(body_len) = read_len(data, 4 + header_len) else {
            return Ok(None);
        };
        if body_len > self.max_body {
            return Err(anyhow!("frame body of {body_len} bytes is too large"));
        }
        let end = 8 + header_len + body_len;
        if data.len() < end {
            return Ok(None);
        }

        let header = data[4..4 + header_len].to_vec();
        let body = data[8 + header_len..end].to_vec();
        self.pos += end;
        if self.pos == self.buf.len() {
            self.buf.clear();
            self.pos = 0;
        } else if self.pos >= 1024 * 1024 {
            self.buf.drain(..self.pos);
            self.pos = 0;
        }
        Ok(Some((header, body)))
    }
}

fn read_len(data: &[u8], at: usize) -> Option<usize> {
    let bytes: [u8; 4] = data.get(at..at.checked_add(4)?)?.try_into().ok()?;
    Some(u32::from_be_bytes(bytes) as usize)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_survive_arbitrary_chunking() {
        let mut stream = Vec::new();
        encode_frame(&mut stream, br#"{"type":"ping"}"#, b"").unwrap();
        encode_frame(&mut stream, br#"{"type":"entry"}"#, &[9; 300]).unwrap();

        for split in [1, 3, 7, 50, stream.len()] {
            let mut reader = FrameReader::new(1024);
            let mut frames = Vec::new();
            for chunk in stream.chunks(split) {
                reader.push(chunk);
                while let Some(frame) = reader.next_frame().unwrap() {
                    frames.push(frame);
                }
            }
            assert_eq!(
                frames,
                [
                    (br#"{"type":"ping"}"#.to_vec(), Vec::new()),
                    (br#"{"type":"entry"}"#.to_vec(), vec![9; 300]),
                ]
            );
        }
    }

    #[test]
    fn oversized_frames_are_rejected() {
        assert!(encode_frame(&mut Vec::new(), &vec![b' '; MAX_HEADER_BYTES + 1], b"").is_err());

        let mut reader = FrameReader::new(10);
        reader.push(&((MAX_HEADER_BYTES + 1) as u32).to_be_bytes());
        assert!(reader.next_frame().is_err());

        let mut stream = Vec::new();
        encode_frame(&mut stream, b"{}", &[0; 11]).unwrap();
        let mut reader = FrameReader::new(10);
        reader.push(&stream);
        assert!(reader.next_frame().is_err());
    }

    #[test]
    fn self_host_urls_are_normalized() {
        assert_eq!(
            normalize_url(" https://sync.example.com:8443/kp/ ").as_deref(),
            Some("https://sync.example.com:8443/kp")
        );
        assert_eq!(
            normalize_url("http://192.168.1.8:8080").as_deref(),
            Some("http://192.168.1.8:8080")
        );
        assert!(normalize_url("sync.example.com").is_none());
        assert!(normalize_url("ftp://example.com").is_none());
    }
}
