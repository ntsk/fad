mod api;
mod apps;
mod auth;
mod commands;
mod config;

use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "fad", version, about = "Firebase App Distribution CLI")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    #[command(about = "Log in to Firebase with your Google account")]
    Login,
    #[command(about = "Log out and revoke the stored credentials")]
    Logout,
    #[command(about = "List accessible Firebase projects")]
    Projects,
    #[command(about = "Set the target Firebase project and app")]
    Use {
        #[arg(
            value_name = "PROJECT_ID",
            help = "Project ID to pick an app from (interactive when omitted)"
        )]
        project_id: Option<String>,
    },
    #[command(about = "List releases of the target app")]
    Releases,
    #[command(about = "Download and install a release")]
    Install {
        #[arg(value_name = "ID", help = "Release ID to install")]
        id: String,
    },
    #[command(about = "Download a release binary without installing")]
    Download {
        #[arg(value_name = "ID", help = "Release ID to download")]
        id: String,
        #[arg(
            short,
            long,
            value_name = "DIR",
            help = "Directory to save the binary into (defaults to the current directory)"
        )]
        output: Option<std::path::PathBuf>,
    },
}

fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Login => auth::login(),
        Command::Logout => auth::logout(),
        Command::Projects => commands::projects(),
        Command::Use { project_id } => commands::use_target(project_id.as_deref()),
        Command::Releases => commands::list(),
        Command::Install { id } => commands::install(&id),
        Command::Download { id, output } => commands::download(&id, output),
    }
}
