use super::{DeviceCommand, DeviceEntry, DeviceMap};
use crate::{daemon::client::Client, mitch::Commands, protocol::DeviceStatus};
use anyhow::{Result, anyhow};
use bluez_async::{
    AdapterId, BluetoothError, BluetoothEvent, BluetoothSession, CharacteristicEvent,
    CharacteristicId, DeviceEvent, DeviceInfo, WriteOptions, WriteType,
};
use futures::StreamExt as _;
use lsl::{Pushable as _, StreamInfo, StreamOutlet};
use std::time::Duration;
use tokio::sync::mpsc::{Receiver, WeakSender};
use tracing::{error, info, warn};
use uuid::{Uuid, uuid};

pub const COMMAND_CHAR: Uuid = uuid!("d5913036-2d8a-41ee-85b9-4e361aa5c8a7");
pub const DATA_CHAR: Uuid = uuid!("09bf2c52-d1d9-c0b7-4145-475964544307");

pub const SERVICE: Uuid = uuid!("c8c0a708-e361-4b5e-a365-98fa6b0a836f");

/// Number of pressure channels in a data notification.
const CHANNELS: usize = 16;
/// Bytes preceding the samples in a data notification.
const HEADER_LEN: usize = 4;

/// Seconds to wait between reconnect attempts.
const RECONNECT_BACKOFF: [u64; 4] = [2, 4, 8, 16];

pub struct DeviceActor {
    name: String,
    device: DeviceInfo,
    adapter: AdapterId,
    session: BluetoothSession,
    rx: Receiver<DeviceCommand>,
    /// Our own sender, used to only remove our entry from the map during cleanup.
    self_tx: WeakSender<DeviceCommand>,
    device_map: DeviceMap,
    connected: bool,
    data_char: Option<CharacteristicId>,
    lsl_outlet: Option<StreamOutlet>,
}

/// Outcome of a reconnect attempt.
enum Reconnect {
    Reconnected,
    GaveUp,
    Shutdown,
}

impl DeviceActor {
    #[must_use = "Creating a DeviceActor without spawning it does nothing"]
    pub fn new(
        name: &str,
        device: DeviceInfo,
        adapter: AdapterId,
        session: BluetoothSession,
        rx: Receiver<DeviceCommand>,
        self_tx: WeakSender<DeviceCommand>,
        device_map: DeviceMap,
    ) -> Self {
        Self {
            name: name.to_string(),
            device,
            adapter,
            session,
            rx,
            self_tx,
            device_map,
            connected: true,
            data_char: None,
            lsl_outlet: None,
        }
    }

    pub fn spawn(self) {
        tokio::task::spawn_local(self.task());
    }

    async fn task(mut self) {
        info!("Actor for {}: Spawned.", self.name);

        if let Err(e) = self.run().await {
            error!("Actor for {}: {:#}", self.name, e);
        }

        info!("Actor for {}: Cleaning up resources...", self.name);
        if let Some(data_char) = &self.data_char {
            self.session.stop_notify(data_char).await.ok();
        }
        if self.connected {
            self.session.disconnect(&self.device.id).await.ok();
        }

        let mut map = self.device_map.lock().await;
        // Only remove the entry if it is still ours, the device may have been reconnected
        // by a new actor in the meantime.
        let is_ours = match (map.get(&self.name), self.self_tx.upgrade()) {
            (Some(DeviceEntry::Connected(tx)), Some(own)) => tx.same_channel(&own),
            _ => false,
        };
        if is_ours {
            map.remove(&self.name);
        }
        info!("Actor for {}: Shutdown complete.", self.name);
    }

    async fn run(&mut self) -> Result<()> {
        let mut notifications_stream = self
            .session
            .event_stream()
            .await
            .map_err(|e| anyhow!("Failed to get notifications: {e}"))?
            .fuse();

        let service = self
            .session
            .get_service_by_uuid(&self.device.id, SERVICE)
            .await?;
        let cmd_char = self
            .session
            .get_characteristic_by_uuid(&service.id, COMMAND_CHAR)
            .await?
            .id;
        let data_char = self
            .session
            .get_characteristic_by_uuid(&service.id, DATA_CHAR)
            .await?
            .id;
        self.data_char = Some(data_char.clone());

        loop {
            tokio::select! {
                maybe_command = self.rx.recv() => {
                    match maybe_command {
                        Some(DeviceCommand::StartStreaming) => {
                            info!("Actor {}: Received StartStreaming", self.name);
                            self.lsl_outlet = Some(self.create_outlet()?);
                            info!("Actor {}: LSL Outlet created.", self.name);
                            self.start_stream(&cmd_char, &data_char).await?;
                        }
                        Some(DeviceCommand::StopStreaming) => {
                            info!("Actor {}: Received StopStreaming", self.name);
                            self.stop_stream(&cmd_char, &data_char).await?;
                            self.lsl_outlet = None;
                        }
                        Some(DeviceCommand::Shutdown) => {
                            info!("Actor {}: Received Shutdown command.", self.name);
                            return Ok(());
                        }
                        Some(DeviceCommand::Status { tx }) => {
                            let charge = self.battery_charge(&cmd_char).await?;
                            tx.send(DeviceStatus { name: self.name.clone(), battery_charge: charge }).ok();
                        }
                        None => {
                            info!("Actor {}: Command channel closed. Shutting down.", self.name);
                            return Ok(());
                        }
                    }
                },

                maybe_data = notifications_stream.next() => {
                    match maybe_data {
                        Some(BluetoothEvent::Characteristic { id, event }) => {
                            if data_char == id && let Some(outlet) = self.lsl_outlet.as_ref() {
                                match event {
                                    CharacteristicEvent::Value { value: data } => {
                                        if data.len() != HEADER_LEN + CHANNELS {
                                            warn!("Actor {}: Dropping packet with unexpected length {}", self.name, data.len());
                                            continue;
                                        }
                                        let sample: Vec<i16> = data[HEADER_LEN..].iter().map(|b| *b as i16).collect();
                                        if let Err(e) = outlet.push_sample(&sample) {
                                            warn!("Actor {}: Failed to push sample: {:?}", self.name, e);
                                        }
                                    }
                                    _ => {
                                        warn!("Actor {}: Received non-value characteristic event.", self.name);
                                    }
                                }
                            }
                        }
                        Some(BluetoothEvent::Device { id, event: DeviceEvent::Connected { connected: false } }) => {
                            if id != self.device.id {
                                continue;
                            }
                            info!("Actor {}: lost connection attempting reconnect", self.name);
                            match self.reconnect().await? {
                                Reconnect::Reconnected => {}
                                Reconnect::GaveUp => {
                                    warn!("Failed to reconnect to {} cleaning up", self.name);
                                    self.connected = false;
                                    return Ok(());
                                }
                                Reconnect::Shutdown => {
                                    info!("Actor {}: Shutdown requested during reconnect.", self.name);
                                    return Ok(());
                                }
                            }
                            info!("Actor {}: successfully reconnected", self.name);
                            if let Err(e) = Client::update_connection(&self.device, &self.adapter).await {
                                warn!("Failed to upgrade connection with error: {}", e);
                                warn!("Continuing with default config");
                            }
                            if self.lsl_outlet.is_some() {
                                self.start_stream(&cmd_char, &data_char).await?;
                            }
                        }
                        None => {
                            warn!("Actor {}: something strange happened cleaning up", self.name);
                            return Ok(());
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    /// Tries to reconnect with exponential backoff while still answering commands.
    async fn reconnect(&mut self) -> Result<Reconnect> {
        for backoff in RECONNECT_BACKOFF {
            self.session.disconnect(&self.device.id).await.ok();
            let Err(e) = self.session.connect(&self.device.id).await else {
                return Ok(Reconnect::Reconnected);
            };
            error!("{:#?}", e);
            warn!(
                "Failed to reconnect to {}, attempting again in {}s",
                self.name, backoff
            );
            if !self.wait_for(Duration::from_secs(backoff)).await {
                return Ok(Reconnect::Shutdown);
            }
            if let BluetoothError::DbusError(err) = e
                && err
                    .name()
                    .map(|n| n.contains("UnknownObject"))
                    .unwrap_or(false)
            {
                warn!("BlueZ destroyed the device object trying to rediscover");
                self.rediscover().await?;
            }
        }
        // Final attempt after the last backoff.
        self.session.disconnect(&self.device.id).await.ok();
        match self.session.connect(&self.device.id).await {
            Ok(()) => Ok(Reconnect::Reconnected),
            Err(_) => Ok(Reconnect::GaveUp),
        }
    }

    /// Sleeps for `duration` while answering commands, returns false if shutdown was requested.
    async fn wait_for(&mut self, duration: Duration) -> bool {
        let sleep = tokio::time::sleep(duration);
        tokio::pin!(sleep);
        loop {
            tokio::select! {
                _ = &mut sleep => return true,
                cmd = self.rx.recv() => match cmd {
                    Some(DeviceCommand::Shutdown) | None => {
                        self.connected = false;
                        return false;
                    }
                    Some(DeviceCommand::Status { tx }) => {
                        tx.send(DeviceStatus { name: self.name.clone(), battery_charge: None }).ok();
                    }
                    Some(DeviceCommand::StartStreaming) => {
                        // Stream gets started once reconnected.
                        match self.create_outlet() {
                            Ok(outlet) => self.lsl_outlet = Some(outlet),
                            Err(e) => warn!("Actor {}: {:#}", self.name, e),
                        }
                    }
                    Some(DeviceCommand::StopStreaming) => {
                        // Dropping the outlet prevents the stream from restarting on reconnect.
                        self.lsl_outlet = None;
                    }
                },
            }
        }
    }

    async fn rediscover(&mut self) -> Result<()> {
        self.session
            .start_discovery_on_adapter(&self.adapter)
            .await?;
        tokio::time::sleep(Duration::from_secs(5)).await;
        self.session
            .stop_discovery_on_adapter(&self.adapter)
            .await?;
        let device = self
            .session
            .get_devices_on_adapter(&self.adapter)
            .await?
            .into_iter()
            .find(|per| per.name.as_deref() == Some(self.name.as_str()));
        if let Some(device) = device {
            self.device = device;
        }
        Ok(())
    }

    fn create_outlet(&self) -> Result<StreamOutlet> {
        let info = StreamInfo::new(
            self.name.as_str(),
            "Pressure",
            CHANNELS as u32,
            50.0,
            lsl::ChannelFormat::Int16,
            self.name.as_str(),
        )
        .map_err(|e| anyhow!("Failed to create LSL stream info: {e:?}"))?;
        StreamOutlet::new(&info, 1, 360).map_err(|e| anyhow!("Failed to create LSL outlet: {e:?}"))
    }

    async fn start_stream(
        &self,
        cmd_char: &CharacteristicId,
        data_char: &CharacteristicId,
    ) -> Result<()> {
        self.write_command(cmd_char, Commands::StartPressureStream)
            .await?;
        self.session.read_characteristic_value(cmd_char).await?;
        self.session.start_notify(data_char).await?;
        Ok(())
    }

    async fn stop_stream(
        &self,
        cmd_char: &CharacteristicId,
        data_char: &CharacteristicId,
    ) -> Result<()> {
        self.session.stop_notify(data_char).await?;
        self.write_command(cmd_char, Commands::StopStream).await?;
        self.session.read_characteristic_value(cmd_char).await?;
        Ok(())
    }

    async fn battery_charge(&self, cmd_char: &CharacteristicId) -> Result<Option<u8>> {
        self.write_command(cmd_char, Commands::GetPower).await?;
        let res = self.session.read_characteristic_value(cmd_char).await?;
        Ok(match (res.get(3), res.get(4)) {
            (Some(0), Some(charge)) => Some(*charge),
            _ => None,
        })
    }

    async fn write_command(&self, cmd_char: &CharacteristicId, cmd: Commands) -> Result<()> {
        self.session
            .write_characteristic_value_with_options(
                cmd_char,
                cmd.as_ref(),
                WriteOptions {
                    write_type: Some(WriteType::WithResponse),
                    ..Default::default()
                },
            )
            .await?;
        Ok(())
    }
}
