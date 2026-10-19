use crate::protocol::{ClientCommand, DaemonResponse, read_msg, socket_path, write_msg};
use anyhow::{Result, anyhow};
use tokio::io::AsyncWriteExt;

#[cfg(unix)]
use tokio::net::UnixStream;

pub async fn run_client(command: ClientCommand) -> Result<()> {
    #[cfg(unix)]
    let mut stream = match UnixStream::connect(socket_path()).await {
        Ok(stream) => stream,
        Err(e) => {
            eprintln!("Error: Could not connect to daemon. Is it running?");
            eprintln!("Try running: `mitch_cli daemon-start`");
            return Err(e.into());
        }
    };

    write_msg(&mut stream, &command).await?;
    stream.shutdown().await?;

    let response: DaemonResponse = read_msg(&mut stream).await?;

    match response {
        DaemonResponse::Ok => println!("Success."),
        DaemonResponse::Error(err) => return Err(anyhow!("Daemon error: {err}")),
        DaemonResponse::Devices(items) => {
            for device in items {
                println!("{device}");
            }
        }
        DaemonResponse::Status(device_status) => {
            if device_status.is_empty() {
                println!("No devices connected.");
            }
            for status in device_status {
                match status.battery_charge {
                    Some(charge) => println!("{}\tbattery: {charge}%", status.name),
                    None => println!("{}\tbattery: unknown", status.name),
                }
            }
        }
    }

    Ok(())
}
