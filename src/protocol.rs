use serde::{Deserialize, Serialize};

#[cfg(unix)]
pub const IPC_SOCKET_PATH: &str = "/tmp/mitch.sock";
#[cfg(windows)]
pub const IPC_SOCKET_PATH: &str = "\\\\.\\pipe\\mitch";

/// Commands sent FROM the CLI client TO the daemon
#[derive(Debug, Serialize, Deserialize)]
pub enum ClientCommand {
    Scan { timeout_ms: u64 },
    Connect { name: String },
    Disconnect { name: String },
    Record { name: String },
    // etc.
}

/// Responses sent FROM the daemon TO the CLI client
#[derive(Debug, Serialize, Deserialize)]
pub enum DaemonResponse {
    Ok,
    Devices(Vec<String>), // Vec of device names/addresses
    Data(Vec<u8>),        // Data read from a characteristic
    Error(String),
}
