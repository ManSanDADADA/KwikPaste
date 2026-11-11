//! Noise 加密通道。
//!
//! TCP 连上后先跑 `Noise_XX`（每条握手消息 `u16` 长度前缀），之后每条应用消息拆成若干块加密：
//! 第一块是 4 字节明文总长，其余块是内容，每块不超过 Noise 单条消息上限（65535 字节含 16 字节 tag）。
//! 读写两半各自维护 nonce，可以放在两个任务里并发收发。

use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context};
use snow::StatelessTransportState;
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader, BufWriter};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::TcpStream;

use super::identity::DeviceIdentity;
use super::protocol::{
    self, Message, HANDSHAKE_MESSAGE_BYTES, MAX_HEADER_BYTES, NOISE_PARAMS, NOISE_PROLOGUE,
};
use crate::settings::LAN_SYNC_MAX_IMAGE_MB_MAX;

const NOISE_MAX_MESSAGE: usize = 65535;
const NOISE_TAG: usize = 16;
const CHUNK_PLAINTEXT: usize = NOISE_MAX_MESSAGE - NOISE_TAG;
const KEY_LEN: usize = 32;
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
/// 认证后收发多久没有任何一块进展就算连接卡死。按块计时：慢链路上的大图只要还在传就不会超时；
/// 对方每 20 秒发一次 ping，空闲的连接也不会被误判。
const IDLE_TIMEOUT: Duration = Duration::from_secs(60);
/// 预分配上限：总长字段来自对端，先按小块分配，收到多少再长多少。
const INITIAL_RECV_CAPACITY: usize = 1024 * 1024;
/// 发出的单条消息的硬上限：JSON 头上限加设置里能选的最大附件。真正的附件上限按对方申报的来。
const MAX_SEND_BYTES: usize =
    4 + MAX_HEADER_BYTES + LAN_SYNC_MAX_IMAGE_MB_MAX as usize * 1024 * 1024;

/// 握手完成后的通道。
pub struct Handshaken {
    pub reader: SecureReader,
    pub writer: SecureWriter,
    pub remote_public_key: [u8; KEY_LEN],
    pub handshake_hash: Vec<u8>,
}

/// 在 `stream` 上完成 Noise XX 握手；`initiator` 为主动连接的一方。
pub async fn handshake(
    stream: TcpStream,
    identity: &DeviceIdentity,
    initiator: bool,
) -> anyhow::Result<Handshaken> {
    tokio::time::timeout(
        HANDSHAKE_TIMEOUT,
        handshake_inner(stream, identity, initiator),
    )
    .await
    .map_err(|_| anyhow!("noise handshake timed out"))?
}

async fn handshake_inner(
    stream: TcpStream,
    identity: &DeviceIdentity,
    initiator: bool,
) -> anyhow::Result<Handshaken> {
    stream.set_nodelay(true).ok();
    let (read_half, write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);
    let mut writer = BufWriter::new(write_half);

    let params = NOISE_PARAMS
        .parse()
        .map_err(|err| anyhow!("invalid noise params: {err}"))?;
    let builder = snow::Builder::new(params)
        .local_private_key(identity.private_key())
        .map_err(noise_err)?
        .prologue(NOISE_PROLOGUE)
        .map_err(noise_err)?;
    let mut state = if initiator {
        builder.build_initiator()
    } else {
        builder.build_responder()
    }
    .map_err(noise_err)?;

    let mut message = vec![0u8; NOISE_MAX_MESSAGE];
    let mut payload = vec![0u8; NOISE_MAX_MESSAGE];
    while !state.is_handshake_finished() {
        if state.is_my_turn() {
            let len = state.write_message(&[], &mut message).map_err(noise_err)?;
            write_frame(&mut writer, &message[..len]).await?;
            writer.flush().await.context("failed to flush handshake")?;
        } else {
            let frame = read_frame(&mut reader).await?;
            state
                .read_message(&frame, &mut payload)
                .map_err(noise_err)?;
        }
    }

    let remote_public_key: [u8; KEY_LEN] = state
        .get_remote_static()
        .ok_or_else(|| anyhow!("peer sent no static key"))?
        .try_into()
        .map_err(|_| anyhow!("peer static key has wrong length"))?;
    let handshake_hash = state.get_handshake_hash().to_vec();
    let transport = Arc::new(state.into_stateless_transport_mode().map_err(noise_err)?);

    Ok(Handshaken {
        reader: SecureReader {
            inner: reader,
            transport: transport.clone(),
            nonce: 0,
            buffer: vec![0u8; NOISE_MAX_MESSAGE],
            limit: HANDSHAKE_MESSAGE_BYTES,
        },
        writer: SecureWriter {
            inner: writer,
            transport,
            nonce: 0,
            buffer: vec![0u8; NOISE_MAX_MESSAGE],
        },
        remote_public_key,
        handshake_hash,
    })
}

pub struct SecureWriter {
    inner: BufWriter<OwnedWriteHalf>,
    transport: Arc<StatelessTransportState>,
    nonce: u64,
    buffer: Vec<u8>,
}

impl SecureWriter {
    pub async fn send_message(
        &mut self,
        message: &Message,
        attachment: &[u8],
    ) -> anyhow::Result<()> {
        let bytes = protocol::encode(message, attachment)?;
        self.send(&bytes).await
    }

    async fn send(&mut self, plaintext: &[u8]) -> anyhow::Result<()> {
        if plaintext.len() > MAX_SEND_BYTES {
            return Err(anyhow!("sync message too large: {} bytes", plaintext.len()));
        }

        let total = u32::try_from(plaintext.len()).context("sync message too large")?;
        self.write_chunk(&total.to_be_bytes()).await?;
        for chunk in plaintext.chunks(CHUNK_PLAINTEXT) {
            self.write_chunk(chunk).await?;
        }
        tokio::time::timeout(IDLE_TIMEOUT, self.inner.flush())
            .await
            .map_err(|_| anyhow!("sync write timed out"))?
            .context("failed to flush sync message")?;
        Ok(())
    }

    async fn write_chunk(&mut self, chunk: &[u8]) -> anyhow::Result<()> {
        let len = self
            .transport
            .write_message(self.nonce, chunk, &mut self.buffer)
            .map_err(noise_err)?;
        self.nonce += 1;
        tokio::time::timeout(
            IDLE_TIMEOUT,
            write_frame(&mut self.inner, &self.buffer[..len]),
        )
        .await
        .map_err(|_| anyhow!("sync write timed out"))?
    }
}

pub struct SecureReader {
    inner: BufReader<OwnedReadHalf>,
    transport: Arc<StatelessTransportState>,
    nonce: u64,
    buffer: Vec<u8>,
    /// 单条消息上限：认证完成前很小，之后放宽到 JSON 头上限加本机的附件上限。
    limit: usize,
}

impl SecureReader {
    /// 认证完成后放宽单条消息上限。
    pub fn set_limit(&mut self, limit: usize) {
        self.limit = limit;
    }

    pub async fn recv_message(&mut self) -> anyhow::Result<(Message, Vec<u8>)> {
        let bytes = self.recv().await?;
        protocol::decode(bytes)
    }

    async fn recv(&mut self) -> anyhow::Result<Vec<u8>> {
        let header = self.read_chunk().await?;
        let header: [u8; 4] = header
            .as_slice()
            .try_into()
            .map_err(|_| anyhow!("sync message length chunk malformed"))?;
        let total = u32::from_be_bytes(header) as usize;
        if total > self.limit {
            return Err(anyhow!("sync message too large: {total} bytes"));
        }

        let mut out = Vec::with_capacity(total.min(INITIAL_RECV_CAPACITY));
        while out.len() < total {
            let chunk = self.read_chunk().await?;
            if out.len() + chunk.len() > total {
                return Err(anyhow!("sync message longer than announced"));
            }
            out.extend_from_slice(&chunk);
        }
        Ok(out)
    }

    async fn read_chunk(&mut self) -> anyhow::Result<Vec<u8>> {
        let frame = tokio::time::timeout(IDLE_TIMEOUT, read_frame(&mut self.inner))
            .await
            .map_err(|_| anyhow!("connection went quiet"))??;
        let len = self
            .transport
            .read_message(self.nonce, &frame, &mut self.buffer)
            .map_err(noise_err)?;
        self.nonce += 1;
        Ok(self.buffer[..len].to_vec())
    }
}

async fn write_frame(writer: &mut BufWriter<OwnedWriteHalf>, bytes: &[u8]) -> anyhow::Result<()> {
    let len = u16::try_from(bytes.len()).context("noise frame too large")?;
    writer
        .write_all(&len.to_be_bytes())
        .await
        .context("failed to write frame length")?;
    writer
        .write_all(bytes)
        .await
        .context("failed to write frame")?;
    Ok(())
}

async fn read_frame(reader: &mut BufReader<OwnedReadHalf>) -> anyhow::Result<Vec<u8>> {
    let mut len = [0u8; 2];
    reader
        .read_exact(&mut len)
        .await
        .context("connection closed")?;
    let len = u16::from_be_bytes(len) as usize;
    let mut frame = vec![0u8; len];
    reader
        .read_exact(&mut frame)
        .await
        .context("connection closed mid-frame")?;
    Ok(frame)
}

fn noise_err(err: snow::Error) -> anyhow::Error {
    anyhow!("noise: {err}")
}

#[cfg(test)]
mod tests {
    use tokio::net::TcpListener;

    use super::super::identity::{device_id_for, load_or_create};
    use super::*;

    #[tokio::test]
    async fn handshake_then_large_message_round_trip() {
        let dir_a = tempfile::tempdir().unwrap();
        let dir_b = tempfile::tempdir().unwrap();
        let a = load_or_create(dir_a.path()).unwrap().identity;
        let b = load_or_create(dir_b.path()).unwrap().identity;
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let big = vec![0x5au8; 300_000];
        let b_id = b.device_id().to_owned();

        let server = {
            let big = big.clone();
            tokio::spawn(async move {
                let (stream, _) = listener.accept().await.unwrap();
                let mut channel = handshake(stream, &b, false).await.unwrap();
                // 认证前的上限挡住大消息，放宽之后才收得下。
                channel.reader.set_limit(1024 * 1024);
                let (message, attachment) = channel.reader.recv_message().await.unwrap();
                assert!(matches!(message, Message::Ping));
                assert_eq!(attachment, big);
                channel
                    .writer
                    .send_message(&Message::Unpair, &[])
                    .await
                    .unwrap();
                (
                    device_id_for(&channel.remote_public_key),
                    channel.handshake_hash,
                )
            })
        };

        let stream = TcpStream::connect(addr).await.unwrap();
        let mut channel = handshake(stream, &a, true).await.unwrap();
        channel
            .writer
            .send_message(&Message::Ping, &big)
            .await
            .unwrap();
        let (reply, _) = channel.reader.recv_message().await.unwrap();
        let (seen_by_server, server_hash) = server.await.unwrap();

        assert!(matches!(reply, Message::Unpair));
        assert_eq!(seen_by_server, a.device_id());
        assert_eq!(device_id_for(&channel.remote_public_key), b_id);
        assert_eq!(channel.handshake_hash, server_hash);
    }
}
