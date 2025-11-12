use super::{DeviceCommand, DeviceMap};
use crate::mitch::Commands;
use btleplug::api::{Peripheral as _, WriteType};
use btleplug::platform::Peripheral;
use futures::StreamExt as _;
use lsl::{Pushable as _, StreamInfo, StreamOutlet};
use tokio::sync::mpsc::Receiver;
use uuid::{Uuid, uuid};

pub const COMMAND_CHAR: Uuid = uuid!("d5913036-2d8a-41ee-85b9-4e361aa5c8a7");
pub const DATA_CHAR: Uuid = uuid!("09bf2c52-d1d9-c0b7-4145-475964544307");

pub struct DeviceActor {
    name: String,
    peripheral: Peripheral,
    rx: Receiver<DeviceCommand>,
    device_map: DeviceMap,
}

impl DeviceActor {
    #[must_use = "Creating a DeviceActor without spawning it does nothing"]
    pub fn new(
        name: &str,
        peripheral: btleplug::platform::Peripheral,
        rx: Receiver<DeviceCommand>,
        device_map: DeviceMap,
    ) -> Self {
        Self {
            name: name.to_string(),
            peripheral,
            rx,
            device_map,
        }
    }

    pub fn spawn(self) {
        tokio::task::spawn_local(self.task());
    }

    async fn task(mut self) {
        let addr = self.peripheral.address();
        println!("Actor for {}: Spawned.", addr);

        let mut notifications_stream = match self.peripheral.notifications().await {
            Ok(stream) => stream.fuse(),
            Err(e) => {
                eprintln!("Actor for {}: Failed to get notifications: {}", addr, e);
                return;
            }
        };

        // We'll use a placeholder for the LSL outlet
        let mut lsl_outlet: Option<StreamOutlet> = None; // Replace () with lsl_rs::Outlet<...>

        // --- Main actor loop ---
        loop {
            tokio::select! {
                // --- BRANCH 1: Listen for commands ---
                maybe_command = self.rx.recv() => {
                    match maybe_command {
                        Some(DeviceCommand::StartRecording { lsl_stream_name }) => {
                            println!("Actor {}: Received StartRecording ({})", addr, lsl_stream_name);

                            let info =
                                StreamInfo::new(self.name.as_str(), "Pressure", 16, 50.0, lsl::ChannelFormat::Int16, self.name.as_str())
                                    .unwrap();
                            let outlet = StreamOutlet::new(&info, 1, 360).unwrap();
                            // 1. Create LSL outlet
                            // lsl_outlet = Some(lsl_rs::Outlet::new(...));
                            lsl_outlet = Some(outlet); // Placeholder
                            println!("Actor {}: LSL Outlet created.", addr);

                            // 2. Subscribe to notifications & tell device to start TX
                            // (You'll need to find your specific characteristic UUID)
                            // peripheral.subscribe(characteristic).await;
                            // peripheral.write(..., b"START_TX_COMMAND", ...).await;
                            let mut c = self.peripheral.characteristics();
                            if c.is_empty() {
                                self.peripheral.discover_services().await.unwrap();
                                c = self.peripheral.characteristics();
                            }
                            let data_char = c.iter().find(|c| c.uuid == DATA_CHAR).unwrap();
                            self.peripheral.subscribe(data_char).await.unwrap();
                            let cmd_char = c.iter().find(|c| c.uuid == COMMAND_CHAR).unwrap();
                            self.peripheral
                                .write(
                                    cmd_char,
                                    Commands::StartPressureStream.as_ref(),
                                    WriteType::WithResponse,
                                )
                                .await.unwrap();
                            self.peripheral.read(cmd_char).await.unwrap();
                        }
                        Some(DeviceCommand::Shutdown) => {
                            println!("Actor {}: Received Shutdown command.", addr);
                            break; // Break the loop to enter cleanup
                        }
                        None => {
                            println!("Actor {}: Command channel closed. Shutting down.", addr);
                            break; // Break the loop
                        }
                    }
                },

                maybe_data = notifications_stream.next() => {
                    match maybe_data {
                        Some(data) => {
                            if data.uuid != DATA_CHAR {
                                continue;
                            }
                            if let Some(outlet) = lsl_outlet.as_ref() {
                                outlet
                                    .push_sample(
                                        &data.value[4..].iter().map(|b| *b as i16).collect::<Vec<i16>>(),
                                    )
                                    .unwrap();                            }
                        }
                        None => {
                            eprintln!("Actor {}: BLE connection dropped! Shutting down.", addr);
                            break; // Break the loop
                        }
                    }
                },
            }
        }

        // --- UNIFIED CLEANUP ---
        println!("Actor for {}: Cleaning up resources...", addr);
        self.peripheral.disconnect().await.ok();

        let mut map = self.device_map.lock().await;
        map.remove(&self.name);
        println!("Actor for {}: Shutdown complete.", addr);
    }
}
