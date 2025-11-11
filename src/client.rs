use crate::protocol::{ClientCommand, DaemonResponse, IPC_SOCKET_PATH};
use anyhow::Result;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[cfg(unix)]
use tokio::net::UnixStream;
#[cfg(windows)]
use tokio::net::windows::named_pipe::ClientOptions;

pub async fn run_client(command: ClientCommand) -> Result<()> {
    #[cfg(unix)]
    let mut stream = match UnixStream::connect(IPC_SOCKET_PATH).await {
        Ok(stream) => stream,
        Err(e) => {
            eprintln!(
                "Error: Could not connect to daemon ({}). Is it running?",
                IPC_SOCKET_PATH
            );
            eprintln!("Try running: `my-ble-tool daemon-start`");
            return Err(e.into());
        }
    };

    #[cfg(windows)]
    let mut stream = match ClientOptions::new().open(IPC_SOCKET_PATH) {
        Ok(stream) => stream,
        Err(e) => {
            eprintln!(
                "Error: Could not connect to daemon ({}). Is it running?",
                IPC_SOCKET_PATH
            );
            eprintln!("Try running: `my-ble-tool daemon-start`");
            return Err(e.into());
        }
    };

    let command_json = serde_json::to_vec(&command)?;
    stream.write_all(&command_json).await?;
    stream.shutdown().await?; // Close write-half

    let mut response_json = Vec::new();
    stream.read_to_end(&mut response_json).await?;

    let response: DaemonResponse = serde_json::from_slice(&response_json)?;

    match response {
        DaemonResponse::Ok => println!("Success."),
        DaemonResponse::Error(err) => eprintln!("Daemon error: {}", err),
        _ => println!("{:?}", response),
    }

    Ok(())
}
