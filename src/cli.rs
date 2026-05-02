use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};

#[derive(Parser, Debug)]
#[command(
    name = "skills",
    version,
    about = "Manage your personal Claude skills library across projects."
)]
pub struct Cli {
    /// Force non-interactive mode. Required decisions must come from flags;
    /// the CLI will not fall back to a prompt. Auto-enabled when stdin or
    /// stdout isn't a TTY.
    #[arg(long, global = true)]
    pub no_interaction: bool,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Configure the skills library repo to pull from.
    Init(InitArgs),
    /// List all skills available in the configured library.
    List(ListArgs),
    /// Select skills to install in the current project.
    Add(AddArgs),
    /// Push local edits to installed skills back to the library.
    Push(PushArgs),
    /// Pull library updates into installed skills.
    Pull(PullArgs),
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
pub struct AddArgs {
    /// Skill name to install. Repeatable. Mutually exclusive with --all.
    #[arg(long = "skill", value_name = "NAME", conflicts_with = "all")]
    pub skills: Vec<String>,

    /// Install every skill from the library.
    #[arg(long, conflicts_with = "skills")]
    pub all: bool,

    /// Install destination relative to the project root (e.g. `.claude/skills`).
    /// Required in non-interactive mode unless an existing destination is
    /// implicitly chosen by the auto-detection (currently never).
    #[arg(long, value_name = "PATH")]
    pub dest: Option<PathBuf>,

    /// Resolution strategy when an install destination already exists.
    /// Required in non-interactive mode if any conflict is encountered.
    #[arg(long, value_enum, value_name = "POLICY")]
    pub on_conflict: Option<OnConflict>,
}

#[derive(Args, Debug)]
pub struct PushArgs {
    /// Skill name to push. Repeatable. Mutually exclusive with --all.
    #[arg(long = "skill", value_name = "NAME", conflicts_with = "all")]
    pub skills: Vec<String>,

    /// Push every skill that has pushable changes (LocalChangesOnly + diverged + library-missing).
    #[arg(long, conflicts_with = "skills")]
    pub all: bool,

    /// Resolution strategy for divergent skills (skip|overwrite). Fork is
    /// interactive-only in v1 — non-interactive runs fall back to skip when
    /// this flag is omitted.
    #[arg(long, value_enum, value_name = "POLICY")]
    pub on_divergence: Option<OnDivergence>,

    /// Override the auto-generated commit message.
    #[arg(long, value_name = "MESSAGE")]
    pub message: Option<String>,
}

#[derive(Args, Debug)]
pub struct PullArgs {
    /// Skill name to pull. Repeatable. Mutually exclusive with --all.
    #[arg(long = "skill", value_name = "NAME", conflicts_with = "all")]
    pub skills: Vec<String>,

    /// Pull every skill that has library updates available (LibraryAhead + diverged).
    #[arg(long, conflicts_with = "skills")]
    pub all: bool,

    /// Resolution strategy for divergent skills (skip|overwrite). Fork-locally
    /// is interactive-only in v1 — non-interactive runs fall back to skip when
    /// this flag is omitted.
    #[arg(long, value_enum, value_name = "POLICY")]
    pub on_divergence: Option<OnDivergence>,
}

#[derive(Args, Debug)]
pub struct DetectArgs {
    /// Name of a new local skill to add. Repeatable. Mutually exclusive with --all.
    #[arg(long = "skill", value_name = "NAME", conflicts_with = "all")]
    pub skills: Vec<String>,

    /// Add every detected new skill.
    #[arg(long, conflicts_with = "skills")]
    pub all: bool,

    /// Target path inside the library (e.g. `skills` or `.claude/skills`).
    /// Required in non-interactive mode.
    #[arg(long, value_name = "PATH")]
    pub target: Option<PathBuf>,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum OnConflict {
    /// Replace the existing destination folder with the library version.
    Overwrite,
    /// Leave the existing folder untouched and skip recording the install.
    Skip,
    /// Stop on the first conflict, persisting whatever was already installed.
    Abort,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum OnDivergence {
    /// Force the local version onto the library, discarding upstream changes.
    Overwrite,
    /// Leave the divergent skill untouched on both sides.
    Skip,
}
