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
#[clap(name = "my-ble-tool", version = "0.1.0")]
struct Cli {
    #[clap(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    DaemonStart,

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

    Record {
        name: String,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let subscriber = FmtSubscriber::builder()
        .with_max_level(Level::TRACE)
        .finish();
    tracing::subscriber::set_global_default(subscriber).expect("setting default subscriber failed");
    let args = Cli::parse();

    match args.command {
        Command::DaemonStart => {
            info!("Starting daemon...");
            let localset = LocalSet::new();
            localset.run_until(Daemon::new().await?.run()).await?;
        }
        Command::Scan { timeout } => {
            client::run_client(protocol::ClientCommand::Scan {
                timeout_ms: timeout,
            })
            .await?;
        }
        Command::Connect { name } => {
            client::run_client(protocol::ClientCommand::Connect { name }).await?;
        }
        Command::Disconnect { name } => {
            client::run_client(protocol::ClientCommand::Disconnect { name }).await?;
        }
        Command::Record { name } => {
            client::run_client(protocol::ClientCommand::Record { name }).await?
        }
        Command::Status => client::run_client(protocol::ClientCommand::Status).await?,
    }

    Ok(())
}
