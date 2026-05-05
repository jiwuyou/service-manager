use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "service-manager",
    version,
    about = "Local-only service manager (CLI + REST API)."
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Start the REST API server (and embedded Web UI).
    Serve {
        /// Path to config JSON. Defaults to ${UserConfigDir}/service-manager/config.json.
        #[arg(long)]
        config: Option<PathBuf>,
        /// Bind address (host:port). Defaults to 127.0.0.1:8787.
        #[arg(long)]
        bind: Option<String>,
    },

    /// Run local diagnostics (config/store accessibility).
    Doctor {
        /// Path to config JSON. Defaults to ${UserConfigDir}/service-manager/config.json.
        #[arg(long)]
        config: Option<PathBuf>,
    },

    /// Token utilities.
    Token {
        #[command(subcommand)]
        command: TokenCommand,
    },

    /// Install service-manager as a local user service (no sudo).
    InstallService {
        /// Path to config JSON. Defaults to ${UserConfigDir}/service-manager/config.json.
        #[arg(long)]
        config: Option<PathBuf>,
        /// Bind address (host:port). Defaults to 127.0.0.1:8787.
        #[arg(long)]
        bind: Option<String>,
    },
    /// Uninstall the local user service (no sudo).
    UninstallService {
        /// Path to config JSON. Defaults to ${UserConfigDir}/service-manager/config.json.
        #[arg(long)]
        config: Option<PathBuf>,
    },
}

#[derive(Debug, Subcommand)]
pub enum TokenCommand {
    /// Print the configured bearer token (env override respected when config token is empty).
    Show {
        /// Path to config JSON. Defaults to ${UserConfigDir}/service-manager/config.json.
        #[arg(long)]
        config: Option<PathBuf>,
    },
    /// Generate a new token and persist it to config.
    Rotate {
        /// Path to config JSON. Defaults to ${UserConfigDir}/service-manager/config.json.
        #[arg(long)]
        config: Option<PathBuf>,
    },
}
