mod api;
mod assets;
mod auth;
mod cli;
mod doctor;
mod error;
mod model;
mod providers;
mod server;
mod store;

use clap::Parser;

use crate::{
    cli::{Cli, Command, TokenCommand},
    error::Result,
};

#[tokio::main]
async fn main() {
    if let Err(e) = run().await {
        eprintln!("{e}");
        std::process::exit(1);
    }
}

async fn run() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Serve { config, bind } => server::serve(config, bind).await,
        Command::Doctor { config } => doctor::run(config).await,
        Command::Token { command } => match command {
            TokenCommand::Show { config } => {
                let (tok, path) = server::show_token(config)?;
                println!("{tok}");
                eprintln!("(from {})", path.display());
                Ok(())
            }
            TokenCommand::Rotate { config } => {
                let (tok, path) = server::rotate_token(config)?;
                println!("{tok}");
                eprintln!("(written to {})", path.display());
                Ok(())
            }
        },
        Command::InstallService { config, bind } => {
            server::install_service(config, bind)?;
            Ok(())
        }
        Command::UninstallService { config } => {
            server::uninstall_service(config)?;
            Ok(())
        }
    }
}
