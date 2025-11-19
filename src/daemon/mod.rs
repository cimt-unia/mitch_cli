use crate::protocol::IPC_SOCKET_PATH;
use anyhow::Result;
use bluez_async::{AdapterInfo, BluetoothSession};
use client::Client;
use std::collections::HashMap;
use std::sync::Arc;
#[cfg(unix)]
use tokio::net::UnixListener;
use tokio::sync::{Mutex, mpsc, oneshot::Sender};
use tracing::{error, info};

mod client;
mod device_actor;

type DeviceMap = Arc<Mutex<HashMap<String, mpsc::Sender<DeviceCommand>>>>;

enum DeviceCommand {
    StartRecording {
        lsl_stream_name: String,
    },
    Status {
        tx: Sender<u8>,
    },
    Shutdown,
}

pub struct Daemon {
    session: BluetoothSession,
    adapter: AdapterInfo,
    device_map: DeviceMap,
}

impl Daemon {
    pub async fn new() -> Result<Self> {
        let session = BluetoothSession::new().await?.1;
        let adapter = session.get_adapters().await?[0].clone();
        let device_map = DeviceMap::new(Mutex::new(HashMap::new()));
        Ok(Self {
            session,
            adapter,
            device_map,
        })
    }
    pub async fn run(&self) -> Result<()> {
        info!("Daemon listening on {}", IPC_SOCKET_PATH);

        #[cfg(unix)]
        {
            let _ = tokio::fs::remove_file(IPC_SOCKET_PATH).await;
            let listener = UnixListener::bind(IPC_SOCKET_PATH)?;

            loop {
                match listener.accept().await {
                    Ok((mut stream, _addr)) => {
                        let session_clone = self.session.clone();
                        let device_map_clone = self.device_map.clone();
                        let adapter_clone = self.adapter.clone();

                        // Spawn a task to handle this client
                        tokio::task::spawn_local(async move {
                            if let Err(e) =
                                Client::new(session_clone, adapter_clone, device_map_clone)
                                    .handle(&mut stream)
                                    .await
                            {
                                error!("Client error: {}", e);
                            }
                        });
                    }
                    Err(e) => error!("Failed to accept client: {}", e),
                }
            }
        }
    }
}
