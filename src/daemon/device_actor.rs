use super::{DeviceCommand, DeviceMap};
use crate::mitch::Commands;
use anyhow::{Result, anyhow};
use bluez_async::{
    BluetoothSession, CharacteristicEvent, DeviceEvent, DeviceInfo, WriteOptions, WriteType,
};
use core::panic;
use futures::StreamExt as _;
use lsl::{Pushable as _, StreamInfo, StreamOutlet};
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

        let mut notifications_stream = match self.session.device_event_stream(&self.device.id).await
        {
            Ok(stream) => stream.fuse(),
            Err(e) => {
                return Err(anyhow!(
                    "Actor for {}: Failed to get notifications: {}",
                    self.name,
                    e
                ));
            }
        };

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

        loop {
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
                            println!("{res:?}");
                            tx.send(res[4]).ok();
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
                                    let CharacteristicEvent::Value { value: data } = event else {
                                        panic!()
                                    };
                                    outlet
                                        .push_sample(
                                            &data[4..].iter().map(|b| *b as i16).collect::<Vec<i16>>(),
                                        )
                                        .unwrap();
                            }
                        }
                        Some(bluez_async::BluetoothEvent::Device { id: _, event: DeviceEvent::Connected { connected } }) => {
                            if !connected {
                                info!("Actor {}: lost connection attempting reconnect", self.name);
                                if self.session.connect(&self.device.id).await.is_err() {
                                    warn!("Failed to reconnect to {} cleaning up", self.name);
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
                                    self.session.read_characteristic_value(&cmd_char.id).await?;
                                    self.session.start_notify(&data_char.id).await?;
                                }
                                info!("Actor {}: sucessfully reconnected", self.name);
                            }
                        }
                        None => {
                            warn!("Actor {}: something strange happened cleaning up", self.name);
                            break; // Break the loop
                        }
                        _ => {}
                    }
                },
            }
        }

        info!("Actor for {}: Cleaning up resources...", self.name);
        self.session.disconnect(&self.device.id).await.ok();

        let mut map = self.device_map.lock().await;
        map.remove(&self.name);
        info!("Actor for {}: Shutdown complete.", self.name);
        Ok(())
    }
}
