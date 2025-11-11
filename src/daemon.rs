use crate::mitch::{Commands, MyInfo, MyOutlet};
use crate::protocol::{ClientCommand, DaemonResponse, IPC_SOCKET_PATH};
use anyhow::Result;
use btleplug::api::{Central, Manager as _, Peripheral, WriteType};
use btleplug::platform::Manager;
use futures::StreamExt;
use lsl::{Pushable, StreamInfo, StreamOutlet};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
#[cfg(windows)]
use tokio::net::windows::named_pipe::ServerOptions;
#[cfg(unix)]
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{Mutex, mpsc};
use uuid::{Uuid, uuid};

pub const COMMAND_CHAR: Uuid = uuid!("d5913036-2d8a-41ee-85b9-4e361aa5c8a7");
pub const DATA_CHAR: Uuid = uuid!("09bf2c52-d1d9-c0b7-4145-475964544307");

// This will hold all our active device connections
// We'd wrap this in Arc<Mutex<...>> to share between tasks
type DeviceMap = Arc<Mutex<HashMap<String, mpsc::Sender<DeviceCommand>>>>;

enum DeviceCommand {
    StartRecording { lsl_stream_name: String },
    Shutdown,
}

pub async fn run_daemon() -> Result<()> {
    // 1. Setup Bluetooth (this is cross-platform)
    let manager = Manager::new().await?;
    let adapter = manager.adapters().await?.into_iter().next().unwrap();
    let device_map = DeviceMap::new(Mutex::new(HashMap::new()));

    println!("Daemon listening on {}", IPC_SOCKET_PATH);

    #[cfg(unix)]
    {
        // UNIX: Clean up old socket and bind
        let _ = tokio::fs::remove_file(IPC_SOCKET_PATH).await;
        let listener = UnixListener::bind(IPC_SOCKET_PATH)?;

        loop {
            match listener.accept().await {
                Ok((stream, _addr)) => {
                    println!("Client connected.");
                    let device_map_clone = device_map.clone();
                    let adapter_clone = adapter.clone();

                    // Spawn a task to handle this client
                    tokio::spawn(async move {
                        if let Err(e) = handle_client(stream, adapter_clone, device_map_clone).await
                        {
                            eprintln!("Client error: {}", e);
                        }
                        println!("Client disconnected.");
                    });
                }
                Err(e) => eprintln!("Failed to accept client: {}", e),
            }
        }
    }

    #[cfg(windows)]
    {
        // WINDOWS: Loop creating new pipe instances
        loop {
            // Create a new pipe instance for the next client.
            let server = ServerOptions::new().create(IPC_SOCKET_PATH)?;

            // Wait for a client to connect to this specific instance
            server.connect().await?;
            println!("Client connected.");

            let device_map_clone = device_map.clone();
            let adapter_clone = adapter.clone();

            // Spawn a task to handle this client
            // The `server` object itself is the stream
            tokio::spawn(async move {
                if let Err(e) = handle_client(server, adapter_clone, device_map_clone).await {
                    eprintln!("Client error: {}", e);
                }
                println!("Client disconnected.");
            });
        }
    }
}

async fn handle_client<S>(
    mut stream: S,
    adapter: btleplug::platform::Adapter,
    device_map: DeviceMap,
) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut command_json = Vec::new();
    stream.read_to_end(&mut command_json).await?;
    let command: ClientCommand = serde_json::from_slice(&command_json)?;

    let response = match command {
        ClientCommand::Scan { timeout_ms } => {
            // ... (scan logic is unchanged)
            DaemonResponse::Ok // Placeholder
        }

        ClientCommand::Connect { name } => {
            println!("Daemon: Connecting to {}...", name);
            // 1. Find the peripheral (this is a simplified search)
            let peripheral = adapter
                .peripherals()
                .await?
                .into_iter()
                .find(|p| p.address().to_string() == name)
                .ok_or_else(|| anyhow::anyhow!("Device not found"))?;

            // 2. Connect
            peripheral.connect().await?;
            println!("Daemon: Connected.");

            // 3. Create the actor's command channel
            let (tx, rx) = mpsc::channel(32); // 32 is a typical buffer size
            let map_clone = device_map.clone();
            let addr_clone = name.clone();

            // 4. SPAWN THE ACTOR TASK!
            // This is the thread you asked about.
            tokio::task::spawn_local(device_actor_task(peripheral, rx, map_clone, addr_clone));

            // 5. Store the sender in the map
            let mut map = device_map.lock().await;
            map.insert(name, tx);

            DaemonResponse::Ok
        }

        ClientCommand::Disconnect { name } => {
            println!("Daemon: Disconnecting from {}...", name);
            let mut map = device_map.lock().await;

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
            println!("Daemon: Telling {} to record...", name);
            let map = device_map.lock().await;

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
    };

    let response_json = serde_json::to_vec(&response)?;
    stream.write_all(&response_json).await?;
    Ok(())
}

async fn device_actor_task(
    peripheral: btleplug::platform::Peripheral,
    mut command_rx: mpsc::Receiver<DeviceCommand>,
    device_map: DeviceMap,
    name: String,
) {
    let addr = peripheral.address();
    println!("Actor for {}: Spawned.", addr);

    let mut notifications_stream = match peripheral.notifications().await {
        Ok(stream) => stream.fuse(),
        Err(e) => {
            eprintln!("Actor for {}: Failed to get notifications: {}", addr, e);
            return;
        }
    };

    // We'll use a placeholder for the LSL outlet
    let mut lsl_outlet: Option<MyOutlet> = None; // Replace () with lsl_rs::Outlet<...>

    // --- Main actor loop ---
    loop {
        tokio::select! {
            // --- BRANCH 1: Listen for commands ---
            maybe_command = command_rx.recv() => {
                match maybe_command {
                    Some(DeviceCommand::StartRecording { lsl_stream_name }) => {
                        println!("Actor {}: Received StartRecording ({})", addr, lsl_stream_name);

                        let info =
                            StreamInfo::new(name.as_str(), "Pressure", 16, 50.0, lsl::ChannelFormat::Int16, name.as_str())
                                .unwrap();
                        let outlet = MyOutlet(StreamOutlet::new(&info, 1, 360).unwrap());
                        // 1. Create LSL outlet
                        // lsl_outlet = Some(lsl_rs::Outlet::new(...));
                        lsl_outlet = Some(outlet); // Placeholder
                        println!("Actor {}: LSL Outlet created.", addr);

                        // 2. Subscribe to notifications & tell device to start TX
                        // (You'll need to find your specific characteristic UUID)
                        // peripheral.subscribe(characteristic).await;
                        // peripheral.write(..., b"START_TX_COMMAND", ...).await;
                        let c = peripheral.characteristics();
                        let data_char = c.iter().find(|c| c.uuid == DATA_CHAR).unwrap();
                        peripheral.subscribe(data_char).await.unwrap();
                        let cmd_char = c.iter().find(|c| c.uuid == COMMAND_CHAR).unwrap();
                        peripheral
                            .write(
                                cmd_char,
                                Commands::StartPressureStream.as_ref(),
                                WriteType::WithResponse,
                            )
                            .await.unwrap();
                        peripheral.read(cmd_char).await.unwrap();
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

            // --- BRANCH 2: Listen for BLE data ---
            maybe_data = notifications_stream.next() => {
                match maybe_data {
                    Some(data) => {
                        if let Some(outlet) = lsl_outlet.as_ref() {
                            // --- HOT PATH ---
                            // println!("Actor {}: Got data {:?}", addr, data.value);
                            // 1. Parse data.value
                            // 2. lsl_outlet.as_ref().unwrap().push_sample(parsed_data);
                            outlet.0.push_sample(&[
                                i16::from_le_bytes([data.value[4], data.value[5]]),
                                i16::from_le_bytes([data.value[6], data.value[7]]),
                                i16::from_le_bytes([data.value[8], data.value[9]]),
                            ]).unwrap()
                        }
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
    peripheral.disconnect().await.ok();

    let mut map = device_map.lock().await;
    map.remove(&name);
    println!("Actor for {}: Shutdown complete.", addr);
}
