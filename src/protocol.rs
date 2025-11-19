use serde::{Deserialize, Serialize};

#[cfg(unix)]
pub const IPC_SOCKET_PATH: &str = "/tmp/mitch.sock";

#[derive(Debug, Serialize, Deserialize)]
pub enum ClientCommand {
    Scan { timeout_ms: u64 },
    Status,
    Connect { name: String },
    Disconnect { name: String },
    Record { name: String },
}

#[derive(Debug, Serialize, Deserialize)]
pub enum DaemonResponse {
    Ok,
    Devices(Vec<String>),
    Error(String),
}
