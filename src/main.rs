use crate::protocol::ClientCommand;
use clap::{Parser, Subcommand};
use daemon::Daemon;
use tokio::task::LocalSet;
use tracing::{Level, info};
use tracing_subscriber::FmtSubscriber;
mod client;
mod daemon;
pub mod mitch;
mod protocol;

#[derive(Debug, Parser)]
#[clap(name = "mitch_cli", version = "0.1.0")]
struct Cli {
    #[clap(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    DaemonStart {
        #[clap(long, default_value = None, help = "The HCI to use, if none supplied uses the first it finds (this is non deterministic), if you only have one BTLE chip feel free to leave it empty")]
        hci: Option<u32>,
    },
    Scan {
        #[clap(short, long, default_value_t = 2000)]
        timeout: u64,
    },

    Status,

    Connect {
        name: String,
    },
    Disconnect {
        name: String,
    },
    Stream {
        name: String,
    },
    StopStream {
        name: String,
    },
    Shutdown,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let subscriber = FmtSubscriber::builder()
        .with_max_level(Level::INFO)
        .with_writer(std::io::stderr)
        .finish();
    tracing::subscriber::set_global_default(subscriber).expect("setting default subscriber failed");
    let args = Cli::parse();

    match args.command {
        Command::DaemonStart { hci } => {
            info!("Starting daemon...");
            let localset = LocalSet::new();
            localset.run_until(Daemon::new(hci).await?.run()).await?;
        }
        Command::Scan { timeout } => {
            client::run_client(ClientCommand::Scan {
                timeout_ms: timeout,
            })
            .await?;
        }
        Command::Connect { name } => {
            client::run_client(ClientCommand::Connect { name }).await?;
        }
        Command::Disconnect { name } => {
            client::run_client(ClientCommand::Disconnect { name }).await?;
        }
        Command::Stream { name } => client::run_client(ClientCommand::Stream { name }).await?,
        Command::Status => client::run_client(ClientCommand::Status).await?,
        Command::StopStream { name } => {
            client::run_client(ClientCommand::StopStream { name }).await?
        }
        Command::Shutdown => client::run_client(ClientCommand::Shutdown).await?,
    }

    Ok(())
}
