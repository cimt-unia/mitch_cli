use super::DeviceMap;
use crate::{
    daemon::{DeviceCommand, device_actor::DeviceActor},
    protocol::{ClientCommand, DaemonResponse, DeviceStatus},
};
use ::futures::future::join_all;
use anyhow::Result;
use bluez_async::{AdapterInfo, BluetoothSession};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::{
    sync::oneshot::{self},
    time,
};
use tracing::info;

pub struct Client {
    session: BluetoothSession,
    adapter: AdapterInfo,
    device_map: DeviceMap,
}

impl Client {
    pub fn new(session: BluetoothSession, adapter: AdapterInfo, device_map: DeviceMap) -> Self {
        Self {
            session,
            adapter,
            device_map,
        }
    }

    pub async fn handle<S>(&self, mut stream: S) -> Result<DaemonResponse>
    where
        S: AsyncRead + AsyncWrite + Unpin,
    {
        let mut len_buf = [0u8; 8];
        stream.read_exact(&mut len_buf).await?;
        let len = u64::from_le_bytes(len_buf) as usize;
        let mut command_json = vec![0; len];
        stream.read_exact(&mut command_json).await?;
        let command: ClientCommand = serde_json::from_slice(&command_json)?;
        info!("Received new command: {command:?}");

        let response = match command {
            ClientCommand::Scan { timeout_ms } => {
                self.session
                    .start_discovery_on_adapter(&self.adapter.id)
                    .await?;
                time::sleep(Duration::from_millis(timeout_ms)).await;
                self.session.stop_discovery().await?;
                let mut devices = Vec::new();
                for per in self
                    .session
                    .get_devices_on_adapter(&self.adapter.id)
                    .await?
                {
                    let n = per.name.unwrap_or_default();
                    if n.starts_with("mitch") {
                        devices.push(n);
                    }
                }
                DaemonResponse::Devices(devices)
            }
            ClientCommand::Connect { name } => self.connect(name.as_str()).await?,
            ClientCommand::Disconnect { name } => {
                info!("Disconnecting from {}...", name);
                let mut map = self.device_map.lock().await;

                // Find the actor's channel and send Shutdown
                if let Some(tx) = map.remove(&name) {
                    // We don't care if the send fails (task might already be dead)
                    let _ = tx.send(DeviceCommand::Shutdown).await;
                    DaemonResponse::Ok
                } else {
                    DaemonResponse::Error("Device not connected".to_string())
                }
            }
            ClientCommand::Record { name } => {
                info!("Telling {} to record...", name);
                let map = self.device_map.lock().await;

                if let Some(tx) = map.get(&name) {
                    tx.send(DeviceCommand::StartRecording {
                        lsl_stream_name: name,
                    })
                    .await?;
                    DaemonResponse::Ok
                } else {
                    DaemonResponse::Error("Device not connected".to_string())
                }
            }
            ClientCommand::Status => {
                let map = self.device_map.lock().await;
                let mut status_fut = Vec::with_capacity(map.len());
                for (_, c) in map.iter() {
                    let (tx, rx) = oneshot::channel::<DeviceStatus>();
                    c.send(DeviceCommand::Status { tx }).await?;
                    status_fut.push(rx);
                }
                let res = join_all(status_fut.into_iter())
                    .await
                    .into_iter()
                    .filter_map(|s| s.ok())
                    .collect();
                DaemonResponse::Status(res)
            }
        };

        let response_json = serde_json::to_vec(&response)?;
        let len = response_json.len() as u64;
        stream.write_all(&len.to_le_bytes()).await?;
        stream.write_all(&response_json).await?;
        Ok(response)
    }

    async fn connect(&self, name: &str) -> Result<DaemonResponse> {
        info!("Connecting to {}...", name);
        // 1. Find the peripheral (this is a simplified search)
        self.session
            .start_discovery_on_adapter(&self.adapter.id)
            .await?;
        time::sleep(Duration::from_secs(5)).await;
        self.session.stop_discovery().await?;
        let mut device = None;
        for per in self
            .session
            .get_devices_on_adapter(&self.adapter.id)
            .await?
        {
            if let Some(ref n) = per.name
                && n == name
            {
                device = Some(per);
            }
        }

        let Some(device) = device else {
            return Ok(DaemonResponse::Error(format!("{name} not found")));
        };

        // 2. Connect
        self.session.connect(&device.id).await?;
        info!("Daemon: Connected.");

        // 3. Create the actor's command channel
        let (tx, rx) = tokio::sync::mpsc::channel(32); // 32 is a typical buffer size
        let map_clone = self.device_map.clone();

        DeviceActor::new(name, device, self.session.clone(), rx, map_clone).spawn();

        // 5. Store the sender in the map
        let mut map = self.device_map.lock().await;
        map.insert(name.to_owned(), tx);

        Ok(DaemonResponse::Ok)
    }
}
