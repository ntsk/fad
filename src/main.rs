mod api;
mod apps;
mod auth;
mod commands;
mod config;
mod http;

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
    Releases {
        #[arg(long, value_name = "APP_ID", help = APP_HELP)]
        app: Option<String>,
    },
    #[command(about = "Upload an APK/AAB as a new release")]
    Upload {
        #[arg(value_name = "FILE", help = "Path to the APK or AAB to upload")]
        file: std::path::PathBuf,
        #[arg(
            short,
            long,
            value_name = "NOTES",
            help = "Release notes to attach to the uploaded build"
        )]
        notes: Option<String>,
        #[arg(long, value_name = "APP_ID", help = APP_HELP)]
        app: Option<String>,
    },
    #[command(about = "Download and install a release")]
    Install {
        #[arg(value_name = "ID", help = "Release ID to install")]
        id: String,
        #[arg(
            long,
            value_name = "PATH",
            help = "Keystore used to sign AAB installs (bundletool --ks); defaults to the debug keystore"
        )]
        ks: Option<std::path::PathBuf>,
        #[arg(
            long = "ks-pass",
            value_name = "VALUE",
            help = "Keystore password, e.g. pass:secret or file:/path (bundletool --ks-pass)"
        )]
        ks_pass: Option<String>,
        #[arg(
            long = "ks-key-alias",
            value_name = "ALIAS",
            help = "Key alias within the keystore (bundletool --ks-key-alias)"
        )]
        ks_key_alias: Option<String>,
        #[arg(
            long = "key-pass",
            value_name = "VALUE",
            help = "Key password, e.g. pass:secret or file:/path (bundletool --key-pass)"
        )]
        key_pass: Option<String>,
        #[arg(long, value_name = "APP_ID", help = APP_HELP)]
        app: Option<String>,
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
        #[arg(long, value_name = "APP_ID", help = APP_HELP)]
        app: Option<String>,
    },
}

const APP_HELP: &str = "Target Firebase App ID (overrides the app_id in config.toml)";

fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Login => auth::login(),
        Command::Logout => auth::logout(),
        Command::Projects => commands::projects(),
        Command::Use { project_id } => commands::use_target(project_id.as_deref()),
        Command::Releases { app } => commands::list(app.as_deref()),
        Command::Upload { file, notes, app } => {
            commands::upload(&file, notes.as_deref(), app.as_deref())
        }
        Command::Install {
            id,
            ks,
            ks_pass,
            ks_key_alias,
            key_pass,
            app,
        } => commands::install(
            &id,
            &commands::SigningOptions {
                ks,
                ks_pass,
                ks_key_alias,
                key_pass,
            },
            app.as_deref(),
        ),
        Command::Download { id, output, app } => commands::download(&id, output, app.as_deref()),
    }
}
