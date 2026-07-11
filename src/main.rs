mod api;
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
    #[command(about = "List releases or download and install one")]
    Install {
        #[arg(
            value_name = "ID",
            required_unless_present = "list",
            conflicts_with = "list",
            help = "Release ID to install"
        )]
        id: Option<String>,
        #[arg(long, help = "List installable releases")]
        list: bool,
    },
}

fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Login => auth::login(),
        Command::Install { id: Some(id), .. } => commands::install(&id),
        Command::Install { .. } => commands::list(),
    }
}
