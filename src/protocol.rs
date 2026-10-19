use anyhow::{Result, bail};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::path::PathBuf;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub const MAX_MSG_LEN: u64 = 64 * 1024;

/// Socket in the per-user runtime dir if available, `/tmp` otherwise.
#[cfg(unix)]
pub fn socket_path() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("mitch.sock")
}

#[derive(Debug, Serialize, Deserialize)]
pub enum ClientCommand {
    Scan { timeout_ms: u64 },
    Status,
    Connect { name: String },
    Disconnect { name: String },
    Stream { name: String },
    StopStream { name: String },
    Shutdown,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum DaemonResponse {
    Ok,
    Devices(Vec<String>),
    Status(Vec<DeviceStatus>),
    Error(String),
    Shutdown,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DeviceStatus {
    pub name: String,
    pub battery_charge: Option<u8>,
}

/// Writes `msg` as JSON prefixed with its length as little endian u64.
pub async fn write_msg<W, T>(stream: &mut W, msg: &T) -> Result<()>
where
    W: AsyncWrite + Unpin,
    T: Serialize,
{
    let json = serde_json::to_vec(msg)?;
    stream.write_all(&(json.len() as u64).to_le_bytes()).await?;
    stream.write_all(&json).await?;
    Ok(())
}

/// Reads a message written by [`write_msg`].
pub async fn read_msg<R, T>(stream: &mut R) -> Result<T>
where
    R: AsyncRead + Unpin,
    T: DeserializeOwned,
{
    let mut len_buf = [0u8; 8];
    stream.read_exact(&mut len_buf).await?;
    let len = u64::from_le_bytes(len_buf);
    if len > MAX_MSG_LEN {
        bail!("message too large ({len} bytes, max {MAX_MSG_LEN})");
    }
    let mut json = vec![0; len as usize];
    stream.read_exact(&mut json).await?;
    Ok(serde_json::from_slice(&json)?)
}
