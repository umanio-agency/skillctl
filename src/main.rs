mod cli;
mod commands;
mod config;
mod context;
mod error;
mod fs_util;
mod git;
mod project_config;
mod prompt;
mod skill;
mod ui;

use clap::Parser;

use crate::cli::{Cli, Command};
use crate::context::Context;
use crate::error::{ExitCode, classify};

fn main() -> std::process::ExitCode {
    let cli = Cli::parse();
    let ctx = Context::from_flags(cli.no_interaction, cli.json);
    let result = match cli.command {
        Command::Init(args) => commands::init::run(args, &ctx),
        Command::List(args) => commands::list::run(args, &ctx),
        Command::Add(args) => commands::add::run(args, &ctx),
        Command::Push(args) => commands::push::run(args, &ctx),
        Command::Pull(args) => commands::pull::run(args, &ctx),
        Command::Detect(args) => commands::detect::run(args, &ctx),
    };
    match result {
        Ok(()) => ExitCode::Success.into(),
        Err(e) => {
            eprintln!("error: {e:#}");
            classify(&e).into()
        }
    }
}
