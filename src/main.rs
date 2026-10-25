mod config;
mod executor;
mod server;
mod template;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Arg, ArgAction, Command};
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
        .version(env!("CARGO_PKG_VERSION"))
        .arg(
            Arg::new("config")
                .required(true)
                .value_parser(clap::value_parser!(PathBuf)),
        )
        .arg(
            Arg::new("effect")
                .long("effect")
                .value_name("EFFECTS")
                .help(
                    "Expose only tools with these comma-separated effects; omit to expose all tools",
                )
                .action(ArgAction::Set)
                .value_delimiter(',')
                .value_parser(clap::value_parser!(config::ToolEffect)),
        )
        .get_matches();

    let path = matches
        .get_one::<PathBuf>("config")
        .expect("config is a required argument");
    let mut config = config::load(path)?;
    if let Some(effects) = matches.get_many::<config::ToolEffect>("effect") {
        config
            .tools
            .retain(|tool| effects.clone().any(|effect| *effect == tool.effect));
    }
    let server = CliServer::new(config)?;
    let service = match server.serve(rmcp::transport::stdio_pipes()).await {
        Ok(service) => service,
        Err(rmcp::service::ServerInitializeError::ConnectionClosed(_)) => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    service.waiting().await?;
    Ok(())
}
