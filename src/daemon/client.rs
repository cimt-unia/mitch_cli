use super::{DeviceEntry, DeviceMap};
use crate::{
    daemon::{DeviceCommand, device_actor::DeviceActor},
    protocol::{ClientCommand, DaemonResponse, DeviceStatus, read_msg, write_msg},
};
use ::futures::future::join_all;
use anyhow::{Result, anyhow};
use bluez_async::{AdapterId, AdapterInfo, BluetoothSession, DeviceInfo, MacAddress};
use std::{process::Stdio, str::FromStr, time::Duration};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::{
    sync::oneshot::{self},
    time,
};
use tracing::{info, warn};

/// Maximum time to wait for a single device to answer a status request.
const STATUS_TIMEOUT: Duration = Duration::from_secs(5);

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
        let command: ClientCommand = read_msg(&mut stream).await?;
        info!("Received new command: {command:?}");

        let response = match command {
            ClientCommand::Scan { timeout_ms } => {
                self.session
                    .start_discovery_on_adapter(&self.adapter.id)
                    .await?;
                time::sleep(Duration::from_millis(timeout_ms)).await;
                self.session
                    .stop_discovery_on_adapter(&self.adapter.id)
                    .await?;
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

                match map.remove(&name) {
                    Some(DeviceEntry::Connected(tx)) => {
                        // We don't care if the send fails (task might already be dead)
                        let _ = tx.send(DeviceCommand::Shutdown).await;
                        DaemonResponse::Ok
                    }
                    Some(DeviceEntry::Connecting) => {
                        map.insert(name, DeviceEntry::Connecting);
                        DaemonResponse::Error("Device is still connecting".to_string())
                    }
                    None => DaemonResponse::Error("Device not connected".to_string()),
                }
            }
            ClientCommand::Record { name } => {
                info!("Telling {} to record...", name);
                let tx = self
                    .device_map
                    .lock()
                    .await
                    .get(&name)
                    .and_then(|e| e.sender().cloned());

                match tx {
                    Some(tx) => match tx.send(DeviceCommand::StartRecording).await {
                        Ok(()) => DaemonResponse::Ok,
                        Err(_) => DaemonResponse::Error(format!("{name} is shutting down")),
                    },
                    None => DaemonResponse::Error("Device not connected".to_string()),
                }
            }
            ClientCommand::Status => {
                // Collect the senders first so the lock is not held while waiting on actors,
                // actors need the lock themselves during cleanup.
                let senders: Vec<_> = self
                    .device_map
                    .lock()
                    .await
                    .values()
                    .filter_map(|e| e.sender().cloned())
                    .collect();
                let status_fut = senders.into_iter().map(|c| async move {
                    let (tx, rx) = oneshot::channel::<DeviceStatus>();
                    c.send(DeviceCommand::Status { tx }).await.ok()?;
                    time::timeout(STATUS_TIMEOUT, rx).await.ok()?.ok()
                });
                let res = join_all(status_fut).await.into_iter().flatten().collect();
                DaemonResponse::Status(res)
            }
        };

        write_msg(&mut stream, &response).await?;
        Ok(response)
    }

    async fn connect(&self, name: &str) -> Result<DaemonResponse> {
        {
            let mut map = self.device_map.lock().await;
            if map.contains_key(name) {
                warn!("Already connected to {}", name);
                return Ok(DaemonResponse::Error(format!(
                    "Already connected to {name}"
                )));
            }
            // Reserve the name so concurrent connects to the same device are rejected.
            map.insert(name.to_owned(), DeviceEntry::Connecting);
        }

        let device = match self.find_and_connect(name).await {
            Ok(Some(device)) => device,
            Ok(None) => {
                self.device_map.lock().await.remove(name);
                return Ok(DaemonResponse::Error(format!("{name} not found")));
            }
            Err(e) => {
                self.device_map.lock().await.remove(name);
                return Err(e);
            }
        };

        let (tx, rx) = tokio::sync::mpsc::channel(32);
        let weak_tx = tx.downgrade();
        self.device_map
            .lock()
            .await
            .insert(name.to_owned(), DeviceEntry::Connected(tx));

        DeviceActor::new(
            name,
            device,
            self.adapter.id.clone(),
            self.session.clone(),
            rx,
            weak_tx,
            self.device_map.clone(),
        )
        .spawn();

        Ok(DaemonResponse::Ok)
    }

    /// Discovers the device by name and connects to it, `None` if it was not found.
    async fn find_and_connect(&self, name: &str) -> Result<Option<DeviceInfo>> {
        info!("Connecting to {}...", name);
        self.session
            .start_discovery_on_adapter(&self.adapter.id)
            .await?;
        time::sleep(Duration::from_secs(5)).await;
        self.session
            .stop_discovery_on_adapter(&self.adapter.id)
            .await?;
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
            return Ok(None);
        };

        self.session.connect(&device.id).await?;
        info!("Daemon: Connected.");

        if let Err(e) = Self::update_connection(&device, &self.adapter.id).await {
            warn!("Failed to upgrade connection with error: {}", e);
            warn!("Continuing with default config");
        }

        Ok(Some(device))
    }

    pub(crate) async fn update_connection(device: &DeviceInfo, adapter: &AdapterId) -> Result<()> {
        let name = device.name.as_ref().expect("mitch to have name");
        info!("Attempting to update the connection to {}", name);
        let hci = adapter.to_string();
        let handle = Self::get_handle_from_mac(device.mac_address, &hci)
            .await
            .ok_or(anyhow!("Unable to retrieve btle handle from mac_address"))?;

        let mut hci_handle = tokio::process::Command::new("sudo")
            .arg("-n")
            .arg("hcitool")
            .arg("-i")
            .arg(&hci)
            .arg("lecup")
            .arg(handle) // Handle
            .arg("40") // min
            .arg("56") // max
            .arg("0") // latency
            .arg("200") // timeout
            .spawn()?;

        let exit_status = hci_handle.wait().await?;

        if exit_status.success() {
            info!("Upgraded btle connection to {}, successfully", name);
            Ok(())
        } else {
            Err(anyhow!("Failed to upgrade btle connection for {}", name))
        }
    }

    async fn get_handle_from_mac(mac: MacAddress, hci: &str) -> Option<String> {
        let hci_handle = tokio::process::Command::new("sudo")
            .arg("-n")
            .arg("hcitool")
            .arg("-i")
            .arg(hci)
            .arg("con")
            .stdout(Stdio::piped())
            .spawn()
            .ok()?;
        let out = hci_handle.wait_with_output().await.ok()?;
        let out_s = String::from_utf8(out.stdout).ok()?;
        let mut out_lines = out_s.lines();
        out_lines.next();
        out_lines.find_map(|l| {
            let mut spl = l.split_ascii_whitespace();
            let m = spl.nth(2)?;
            let handle = spl.nth(1)?;
            if mac == MacAddress::from_str(m).ok()? {
                Some(handle.to_string())
            } else {
                None
            }
        })
    }
}
