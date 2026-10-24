mod config;
mod executor;
mod server;
mod template;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Arg, Command};
use rmcp::ServiceExt;
use server::CliServer;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("cli2mcp: {error}");
            ExitCode::FAILURE
        }
    }
}

#[tokio::main(flavor = "current_thread")]
async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let matches = Command::new("cli2mcp")
        .arg(
            Arg::new("config")
                .required(true)
                .value_parser(clap::value_parser!(PathBuf)),
        )
        .get_matches();

    let path = matches
        .get_one::<PathBuf>("config")
        .expect("config is a required argument");
    let server = CliServer::new(config::load(path)?)?;
    let service = match server.serve(rmcp::transport::stdio_pipes()).await {
        Ok(service) => service,
        Err(rmcp::service::ServerInitializeError::ConnectionClosed(_)) => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    service.waiting().await?;
    Ok(())
}
