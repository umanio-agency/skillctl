use clap::{Args, Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(
    name = "skills",
    version,
    about = "Manage your personal Claude skills library across projects."
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Configure the skills library repo to pull from.
    Init(InitArgs),
    /// List all skills available in the configured library.
    List(ListArgs),
    /// Interactively select skills to install in the current project.
    Add(AddArgs),
    /// Push local edits to installed skills back to the library.
    Push(PushArgs),
    /// Find skills created locally and offer to add them to the library.
    Detect(DetectArgs),
}

#[derive(Args, Debug)]
pub struct InitArgs {
    /// GitHub URL of the skills library (e.g. https://github.com/owner/repo).
    pub url: String,
}

#[derive(Args, Debug)]
pub struct ListArgs {}

#[derive(Args, Debug)]
pub struct AddArgs {}

#[derive(Args, Debug)]
pub struct PushArgs {
    /// Skill names to push. If omitted, all skills with local changes are considered.
    pub skills: Vec<String>,
}

#[derive(Args, Debug)]
pub struct DetectArgs {}
