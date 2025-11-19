use crate::protocol::{ClientCommand, DaemonResponse, IPC_SOCKET_PATH};
use anyhow::Result;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[cfg(unix)]
use tokio::net::UnixStream;

pub async fn run_client(command: ClientCommand) -> Result<()> {
    #[cfg(unix)]
    let mut stream = match UnixStream::connect(IPC_SOCKET_PATH).await {
        Ok(stream) => stream,
        Err(e) => {
            eprintln!("Error: Could not connect to daemon. Is it running?");
            eprintln!("Try running: `mitch_cli daemon-start`");
            return Err(e.into());
        }
    };

    let command_json = serde_json::to_vec(&command)?;
    let command_len = command_json.len() as u64;
    stream.write_all(&command_len.to_le_bytes()).await?;
    stream.write_all(&command_json).await?;
    stream.shutdown().await?;

    let mut len_buf = [0u8; 8];
    stream.read_exact(&mut len_buf).await?;
    let len = u64::from_le_bytes(len_buf) as usize;

    let mut response_json = vec![0; len];
    stream.read_exact(&mut response_json).await?;

    let response: DaemonResponse = serde_json::from_slice(&response_json)?;

    match response {
        DaemonResponse::Ok => println!("Success."),
        DaemonResponse::Error(err) => eprintln!("Daemon error: {}", err),
        DaemonResponse::Devices(items) => {
            for device in items {
                println!("{device}");
            }
        }
        DaemonResponse::Status(device_status) => {
            println!("{:?}", device_status)
        }
    }

    Ok(())
}
