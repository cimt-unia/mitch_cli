use super::{DeviceCommand, DeviceMap};
use crate::{daemon::client::Client, mitch::Commands, protocol::DeviceStatus};
use anyhow::{Result, anyhow};
use bluez_async::{
    BluetoothError, BluetoothSession, CharacteristicEvent, DeviceEvent, DeviceInfo, WriteOptions,
    WriteType,
};
use core::panic;
use futures::StreamExt as _;
use lsl::{Pushable as _, StreamInfo, StreamOutlet};
use std::time::Duration;
use tokio::sync::mpsc::Receiver;
use tracing::{info, warn};
use uuid::{Uuid, uuid};

pub const COMMAND_CHAR: Uuid = uuid!("d5913036-2d8a-41ee-85b9-4e361aa5c8a7");
pub const DATA_CHAR: Uuid = uuid!("09bf2c52-d1d9-c0b7-4145-475964544307");

pub const SERVICE: Uuid = uuid!("c8c0a708-e361-4b5e-a365-98fa6b0a836f");

pub struct DeviceActor {
    name: String,
    device: DeviceInfo,
    session: BluetoothSession,
    rx: Receiver<DeviceCommand>,
    device_map: DeviceMap,
}

impl DeviceActor {
    #[must_use = "Creating a DeviceActor without spawning it does nothing"]
    pub fn new(
        name: &str,
        device: DeviceInfo,
        session: BluetoothSession,
        rx: Receiver<DeviceCommand>,
        device_map: DeviceMap,
    ) -> Self {
        Self {
            name: name.to_string(),
            device,
            session,
            rx,
            device_map,
        }
    }

    pub fn spawn(self) {
        tokio::task::spawn_local(self.task());
    }

    async fn task(mut self) -> Result<()> {
        info!("Actor for {}: Spawned.", self.name);

        let mut notifications_stream = match self.session.event_stream().await {
            Ok(stream) => stream.fuse(),
            Err(e) => {
                return Err(anyhow!(
                    "Actor for {}: Failed to get notifications: {}",
                    self.name,
                    e
                ));
            }
        };

        let mut connected = true;
        let mut lsl_outlet: Option<StreamOutlet> = None;
        let service = self
            .session
            .get_service_by_uuid(&self.device.id, SERVICE)
            .await?;
        let cmd_char = self
            .session
            .get_characteristic_by_uuid(&service.id, COMMAND_CHAR)
            .await?;
        let data_char = self
            .session
            .get_characteristic_by_uuid(&service.id, DATA_CHAR)
            .await?;
        let data_id = data_char.id.clone();

        'main: loop {
            tokio::select! {
                maybe_command = self.rx.recv() => {
                    match maybe_command {
                        Some(DeviceCommand::StartRecording { lsl_stream_name }) => {
                            info!("Actor {}: Received StartRecording ({})", self.name, lsl_stream_name);

                            let info =
                                StreamInfo::new(self.name.as_str(), "Pressure", 16, 50.0, lsl::ChannelFormat::Int16, self.name.as_str())
                                .unwrap();
                            let outlet = StreamOutlet::new(&info, 1, 360).unwrap();
                            lsl_outlet = Some(outlet);
                            info!("Actor {}: LSL Outlet created.", self.name);

                            self.session
                                .write_characteristic_value_with_options(
                                    &cmd_char.id,
                                    Commands::StartPressureStream.as_ref(),
                                    WriteOptions {
                                        write_type: Some(WriteType::WithResponse),
                                        ..Default::default()
                                    },
                                )
                                .await?;
                            self.session.read_characteristic_value(&cmd_char.id).await?;
                            self.session.start_notify(&data_char.id).await?;
                        }
                        Some(DeviceCommand::Shutdown) => {
                            info!("Actor {}: Received Shutdown command.", self.name);
                            break; // Break the loop to enter cleanup
                        }
                        Some(DeviceCommand::Status { tx }) => {
                            self.session
                                .write_characteristic_value_with_options(
                                    &cmd_char.id,
                                    Commands::GetPower.as_ref(),
                                    WriteOptions {
                                        write_type: Some(WriteType::WithResponse),
                                        ..Default::default()
                                    },
                                )
                                .await?;
                            let res = self.session.read_characteristic_value(&cmd_char.id).await?;
                            let charge = if res[3] == 0 {
                                Some(res[4])
                            } else {
                                None
                            };
                            tx.send(DeviceStatus{ name: self.name.clone(), battery_charge: charge }).ok();
                        }
                        None => {
                            info!("Actor {}: Command channel closed. Shutting down.", self.name);
                            break; // Break the loop
                        }
                    }
                },

                maybe_data = notifications_stream.next() => {
                    match maybe_data {
                        Some(bluez_async::BluetoothEvent::Characteristic { id, event }) => {
                            if data_id == id  &&
                                let Some(outlet) = lsl_outlet.as_ref() {
                                    match event {
                                        CharacteristicEvent::Value { value: data } => {
                                            if data.len() >= 4 {
                                                outlet.push_sample(&data[4..].iter().map(|b| *b as i16).collect::<Vec<i16>>()).unwrap();
                                            }
                                        }
                                        _ => {
                                            warn!("Actor {}: Received non-value characteristic event.", self.name);
                                        }
                                    }}
                        }
                        Some(bluez_async::BluetoothEvent::Device { id, event: DeviceEvent::Connected { connected: is_connected } }) => {
                            if id != self.device.id {
                                continue;
                            }
                            if !is_connected {
                                info!("Actor {}: lost connection attempting reconnect", self.name);
                                let exp_backoff = [2, 4, 8, 16, u64::MAX];
                                let max_attempts = exp_backoff.len() - 1;
                                for (i, backoff) in exp_backoff.iter().enumerate() {
                                    self.session.disconnect(&self.device.id).await.ok();
                                    if let Err(e) = self.session.connect(&self.device.id).await {
                                        if i == max_attempts {
                                            warn!("Failed to reconnect to {} cleaning up", self.name);
                                            connected = false;
                                            break 'main;
                                        }
                                        warn!("Failed to reconnect to {}, attempting again in {}s", self.name, backoff);
                                        tokio::time::sleep(Duration::from_secs(*backoff)).await;
                                        if let BluetoothError::DbusError(err) = e &&
                                            err.name().map(|n| n.contains("UnknownObject")).unwrap_or(false) {
                                            warn!("BlueZ destroyed the device object trying to rediscover");
                                            let adapter = self.session.get_adapters().await?[0].clone();
                                            self.session
                                                .start_discovery_on_adapter(&adapter.id)
                                                .await?;
                                            tokio::time::sleep(Duration::from_secs(5)).await;
                                            self.session.stop_discovery().await?;
                                            let mut device = None;
                                            for per in self
                                                .session
                                                    .get_devices_on_adapter(&adapter.id)
                                                    .await?
                                                    {
                                                        if let Some(ref n) = per.name
                                                            && n == &self.name
                                                            {
                                                                device = Some(per);
                                                            }
                                                    }
                                            if let Some(device) = device {
                                                self.device = device;
                                            }
                                        }
                                        continue
                                    }
                                    if lsl_outlet.is_some() {
                                        self.session
                                            .write_characteristic_value_with_options(
                                                &cmd_char.id,
                                                Commands::StartPressureStream.as_ref(),
                                                WriteOptions {
                                                    write_type: Some(WriteType::WithResponse),
                                                    ..Default::default()
                                                },
                                            )
                                            .await?;
                                        self.session.read_characteristic_value(&cmd_char.id).await.ok();
                                        self.session.start_notify(&data_char.id).await.ok();
                                    }
                                    break;
                                }
                                info!("Actor {}: sucessfully reconnected", self.name);
                                if let Err(e) = Client::update_connection(&self.device).await {
                                    warn!("Failed to upgrade connection with error: {}", e);
                                    warn!("Continuing with default config");
                                }
                            }
                        }
                        None => {
                            warn!("Actor {}: something strange happened cleaning up", self.name);
                            break; // Break the loop
                        }
                        _ => {}
                    }
                }
            }
        }

        info!("Actor for {}: Cleaning up resources...", self.name);
        self.session.stop_notify(&data_char.id).await.ok();
        if connected {
            self.session.disconnect(&self.device.id).await.ok();
        }

        let mut map = self.device_map.lock().await;
        map.remove(&self.name);
        info!("Actor for {}: Shutdown complete.", self.name);
        Ok(())
    }
}
