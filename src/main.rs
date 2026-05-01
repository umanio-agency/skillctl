mod cli;
mod commands;
mod config;
mod context;
mod fs_util;
mod git;
mod project_config;
mod skill;

use anyhow::Result;
use clap::Parser;

use crate::cli::{Cli, Command};
use crate::context::Context;

fn main() -> Result<()> {
    let cli = Cli::parse();
    let ctx = Context::from_flag(cli.no_interaction);
    match cli.command {
        Command::Init(args) => commands::init::run(args),
        Command::List(args) => commands::list::run(args),
        Command::Add(args) => commands::add::run(args, &ctx),
        Command::Push(args) => commands::push::run(args, &ctx),
        Command::Detect(args) => commands::detect::run(args, &ctx),
    }
}
