use crate::protocol::{DaemonResponse, DeviceStatus, socket_path, write_msg};
use anyhow::{Result, anyhow};
use bluez_async::{AdapterInfo, BluetoothSession};
use client::Client;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
#[cfg(unix)]
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{Mutex, Notify, mpsc, oneshot::Sender};
use tracing::{error, info, warn};

mod client;
mod device_actor;

type DeviceMap = Arc<Mutex<HashMap<String, DeviceEntry>>>;

/// Maximum time the shutdown handler waits for actors to clean up.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

enum DeviceEntry {
    /// A client is currently discovering/connecting this device, reserves the name.
    Connecting,
    Connected(mpsc::Sender<DeviceCommand>),
}

impl DeviceEntry {
    fn sender(&self) -> Option<&mpsc::Sender<DeviceCommand>> {
        match self {
            DeviceEntry::Connecting => None,
            DeviceEntry::Connected(tx) => Some(tx),
        }
    }
}

enum DeviceCommand {
    StartStreaming,
    StopStreaming,
    Status { tx: Sender<DeviceStatus> },
    Shutdown,
}

pub struct Daemon {
    session: BluetoothSession,
    adapter: AdapterInfo,
    device_map: DeviceMap,
}

impl Daemon {
    pub async fn new(hci: Option<u32>) -> Result<Self> {
        let session = BluetoothSession::new().await?.1;
        let adapters = session.get_adapters().await?;
        let adapter = if let Some(hci) = hci {
            adapters
                .into_iter()
                .find(|a| a.id.to_string() == format!("hci{hci}"))
                .ok_or(anyhow!("hci{hci} not found"))?
        } else {
            adapters
                .into_iter()
                .next()
                .ok_or(anyhow!("No bluetooth adapter found"))?
        };
        info!("Daemon using {adapter:#?}");
        let device_map = DeviceMap::new(Mutex::new(HashMap::new()));
        Ok(Self {
            session,
            adapter,
            device_map,
        })
    }

    /// Tells all actors to shut down and waits (bounded) for them to clean up.
    async fn shutdown(&self) {
        info!("Shutting down daemon...");
        let _ = std::fs::remove_file(socket_path());
        let m = &self.device_map;
        let senders: Vec<_> = m
            .lock()
            .await
            .values()
            .filter_map(|e| e.sender().cloned())
            .collect();
        for tx in senders {
            tx.send(DeviceCommand::Shutdown).await.ok();
        }
        let wait_empty = async {
            while !m.lock().await.is_empty() {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        };
        if tokio::time::timeout(SHUTDOWN_TIMEOUT, wait_empty)
            .await
            .is_err()
        {
            warn!("Not all devices shut down in time, exiting anyway");
        }
    }

    pub async fn run(&self) -> Result<()> {
        #[cfg(unix)]
        {
            let path = socket_path();
            let listener = bind_listener(&path).await?;
            info!("Daemon listening on {}", path.display());

            // Both Ctrl-C and the `shutdown` client command only signal the accept loop,
            // the actual shutdown runs here so it cannot race with new clients.
            let shutdown = Arc::new(Notify::new());
            let ctrlc_shutdown = shutdown.clone();
            ctrlc_async::set_async_handler(async move { ctrlc_shutdown.notify_one() })?;

            loop {
                let (mut stream, _addr) = tokio::select! {
                    _ = shutdown.notified() => break,
                    res = listener.accept() => match res {
                        Ok(conn) => conn,
                        Err(e) => {
                            error!("Failed to accept client: {}", e);
                            continue;
                        }
                    },
                };
                let session_clone = self.session.clone();
                let device_map_clone = self.device_map.clone();
                let adapter_clone = self.adapter.clone();
                let shutdown = shutdown.clone();

                // Spawn a task to handle this client
                tokio::task::spawn_local(async move {
                    match Client::new(session_clone, adapter_clone, device_map_clone)
                        .handle(&mut stream)
                        .await
                    {
                        Err(e) => {
                            let err = e.to_string();
                            error!("Client error: {}", err);
                            let _ = write_msg(&mut stream, &DaemonResponse::Error(err)).await;
                        }
                        Ok(DaemonResponse::Shutdown) => shutdown.notify_one(),
                        Ok(_) => {}
                    }
                });
            }

            self.shutdown().await;
            Ok(())
        }
    }
}

/// Binds the IPC socket, removing a stale socket file left behind by a crashed daemon.
#[cfg(unix)]
async fn bind_listener(path: &std::path::Path) -> Result<UnixListener> {
    match UnixListener::bind(path) {
        Ok(listener) => Ok(listener),
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
            if UnixStream::connect(path).await.is_ok() {
                return Err(anyhow!(
                    "There seems to be a daemon running already ({})",
                    path.display()
                ));
            }
            warn!("Removing stale socket {}", path.display());
            std::fs::remove_file(path)?;
            Ok(UnixListener::bind(path)?)
        }
        Err(e) => Err(e.into()),
    }
}
