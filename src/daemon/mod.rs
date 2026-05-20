use crate::protocol::{DeviceStatus, IPC_SOCKET_PATH};
use anyhow::Result;
use bluez_async::{AdapterInfo, BluetoothSession};
use client::Client;
use std::collections::HashMap;
use std::sync::Arc;
#[cfg(unix)]
use tokio::net::UnixListener;
use tokio::sync::{Mutex, mpsc, oneshot::Sender};
use tracing::{error, info, warn};

mod client;
mod device_actor;

type DeviceMap = Arc<Mutex<HashMap<String, mpsc::Sender<DeviceCommand>>>>;

enum DeviceCommand {
    StartRecording { lsl_stream_name: String },
    Status { tx: Sender<DeviceStatus> },
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
            use anyhow::Context;

            let m = self.device_map.clone();

            ctrlc_async::set_async_handler(async move {
                use std::process::exit;

                let _ = std::fs::remove_file(IPC_SOCKET_PATH);
                for (_, tx) in m.lock().await.iter() {
                    tx.send(DeviceCommand::Shutdown).await.ok();
                }
                while !m.lock().await.is_empty() {
                    use std::time::Duration;

                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                exit(0);
            })?;
            let listener = UnixListener::bind(IPC_SOCKET_PATH)
                .context("There seems to be a daemon running allready")?;

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
impl Drop for Daemon {
    fn drop(&mut self) {
        warn!("droppi");
        print!("droppi");
    }
}
