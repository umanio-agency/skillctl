mod cli;
mod commands;
mod config;
mod git;
mod skill;

use anyhow::Result;
use clap::Parser;

use crate::cli::{Cli, Command};

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Init(args) => commands::init::run(args),
        Command::List(args) => commands::list::run(args),
        Command::Add(args) => commands::add::run(args),
        Command::Push(args) => commands::push::run(args),
        Command::Detect(args) => commands::detect::run(args),
    }
}
