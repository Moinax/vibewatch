mod config;
mod compositor;
mod ipc;
mod notify;
mod scanner;
mod session;
mod sound;
mod waybar;

#[cfg(feature = "panel")]
mod panel;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "vibewatch", about = "AI agent monitor for Wayland compositors")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Start the vibewatch daemon
    Daemon,
    /// Send a notification event from a hook
    Notify {
        /// The event payload (JSON string)
        event: String,
        /// Agent type
        #[arg(long, default_value = "claude-code")]
        agent: String,
    },
    /// Print current session status
    Status,
    /// Toggle the overlay panel visibility
    TogglePanel,
    /// Launch the standalone GTK4 panel
    #[cfg(feature = "panel")]
    Panel,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Daemon => todo!(),
        Commands::Notify { event: _, agent: _ } => todo!(),
        Commands::Status => todo!(),
        Commands::TogglePanel => todo!(),
        #[cfg(feature = "panel")]
        Commands::Panel => todo!(),
    }
}
