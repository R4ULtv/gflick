mod args;
mod command;
mod output;
mod selector;

use std::{
    io::{self, Write},
    process::ExitCode,
};

use anyhow::Result;
use clap::{CommandFactory, Parser};

use crate::{
    args::{Cli, Command},
    command::{IpcBackend, execute, ipc_context},
    output::write_event,
};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("Error: {error:#}");
            ExitCode::from(1)
        }
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Completions { shell } => {
            clap_complete::generate(shell, &mut Cli::command(), "open-hub", &mut io::stdout());
            Ok(())
        }
        Command::Events { count } => events(cli.format, count),
        command => {
            let mut backend = IpcBackend;
            execute(&mut backend, command, cli.format, io::stdout()).map_err(ipc_context)
        }
    }
}

fn events(format: args::OutputFormat, count: Option<u64>) -> Result<()> {
    let mut subscription = open_hub_client::EventSubscription::connect().map_err(ipc_context)?;
    let mut output = io::stdout().lock();
    let mut received = 0u64;
    while count.is_none_or(|count| received < count) {
        let Some(event) = subscription.recv().map_err(ipc_context)? else {
            break;
        };
        write_event(&mut output, format, &event)?;
        received += 1;
    }
    output.flush()?;
    Ok(())
}
