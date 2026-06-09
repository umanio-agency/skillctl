use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};

#[derive(Parser, Debug)]
#[command(
    name = "skillctl",
    version,
    about = "Manage your personal Claude skills library across projects."
)]
pub struct Cli {
    /// Force non-interactive mode. Required decisions must come from flags;
    /// the CLI will not fall back to a prompt. Auto-enabled when stdin or
    /// stdout isn't a TTY, and implied by --json.
    #[arg(long, global = true)]
    pub no_interaction: bool,

    /// Emit a structured JSON object to stdout (per-command schema documented
    /// in skillctl-usage). Implies --no-interaction; suppresses the
    /// human-readable cliclack output.
    #[arg(long, global = true)]
    pub json: bool,

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
    /// Remove skills from the current project (folder + any .skills.toml entry).
    Remove(RemoveArgs),
    /// Manage configured skill libraries (read sources + write targets).
    #[command(subcommand)]
    Library(LibraryCommand),
}

#[derive(Subcommand, Debug)]
pub enum LibraryCommand {
    /// Register a new library by name and URL (clones it into the cache).
    Add(LibraryAddArgs),
    /// List the configured libraries.
    List,
    /// Remove a configured library (drops the config entry; leaves the cache).
    Remove(LibraryRefArgs),
    /// Mark a configured library as the default.
    SetDefault(LibraryRefArgs),
}

#[derive(Args, Debug)]
pub struct LibraryAddArgs {
    /// Short name used to reference this library (e.g. with future --from/--to).
    pub name: String,

    /// Repository URL (GitHub HTTPS or SSH).
    pub url: String,

    /// Access level: `read` (default — consume only), `write`, or `pr`.
    #[arg(long, value_enum, default_value = "read")]
    pub access: AccessArg,

    /// Mark this library as the default. The first library added is always
    /// the default regardless of this flag.
    #[arg(long)]
    pub default: bool,
}

#[derive(Args, Debug)]
pub struct LibraryRefArgs {
    /// Name of the configured library.
    pub name: String,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum AccessArg {
    Read,
    Write,
    Pr,
}

#[derive(Args, Debug)]
pub struct InitArgs {
    /// GitHub URL of the skills library (e.g. https://github.com/owner/repo).
    pub url: String,
}

#[derive(Args, Debug)]
pub struct ListArgs {
    /// Show only skills carrying this tag. Repeatable; default semantics is
    /// union (any of the given tags).
    #[arg(long = "tag", value_name = "TAG")]
    pub tags: Vec<String>,

    /// Switch tag matching from union (any) to intersection (all) when
    /// multiple `--tag` flags are passed. Has no effect without `--tag`.
    #[arg(long, requires = "tags")]
    pub all_tags: bool,
}

#[derive(Args, Debug)]
pub struct AddArgs {
    /// Skill name to install. Repeatable. Mutually exclusive with --all and --tag.
    #[arg(long = "skill", value_name = "NAME", conflicts_with_all = ["all", "tags"])]
    pub skills: Vec<String>,

    /// Install every skill from the library.
    #[arg(long, conflicts_with_all = ["skills", "tags"])]
    pub all: bool,

    /// Install every skill carrying this tag. Repeatable; default semantics is
    /// union (any of the given tags). Mutually exclusive with --skill and --all.
    #[arg(long = "tag", value_name = "TAG", conflicts_with_all = ["skills", "all"])]
    pub tags: Vec<String>,

    /// Switch tag matching from union (any) to intersection (all) when
    /// multiple `--tag` flags are passed. Has no effect without `--tag`.
    #[arg(long, requires = "tags")]
    pub all_tags: bool,

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
    /// Skill name to push. Repeatable. Mutually exclusive with --all and --tag.
    #[arg(long = "skill", value_name = "NAME", conflicts_with_all = ["all", "tags"])]
    pub skills: Vec<String>,

    /// Push every skill that has pushable changes (LocalChangesOnly + diverged + library-missing).
    #[arg(long, conflicts_with_all = ["skills", "tags"])]
    pub all: bool,

    /// Push every pushable skill carrying this tag. Repeatable; default
    /// semantics is union (any of the given tags). Mutually exclusive with
    /// --skill and --all. Tags are read from each skill's local SKILL.md.
    #[arg(long = "tag", value_name = "TAG", conflicts_with_all = ["skills", "all"])]
    pub tags: Vec<String>,

    /// Switch tag matching from union (any) to intersection (all) when
    /// multiple `--tag` flags are passed. Has no effect without `--tag`.
    #[arg(long, requires = "tags")]
    pub all_tags: bool,

    /// Resolution strategy for divergent (and library-missing) skills:
    /// `skip` / `overwrite` / `fork`. `fork` requires `--fork-suffix` in
    /// non-interactive mode.
    #[arg(long, value_enum, value_name = "POLICY")]
    pub on_divergence: Option<OnDivergence>,

    /// Suffix appended to the original skill name when forking
    /// non-interactively (e.g. `--fork-suffix custom` → `<name>-custom`).
    /// Required when `--on-divergence fork` is used without a TTY.
    #[arg(long, value_name = "SUFFIX")]
    pub fork_suffix: Option<String>,

    /// Override the auto-generated commit message.
    #[arg(long, value_name = "MESSAGE")]
    pub message: Option<String>,
}

#[derive(Args, Debug)]
pub struct PullArgs {
    /// Skill name to pull. Repeatable. Mutually exclusive with --all and --tag.
    #[arg(long = "skill", value_name = "NAME", conflicts_with_all = ["all", "tags"])]
    pub skills: Vec<String>,

    /// Pull every skill that has library updates available (LibraryAhead + diverged).
    #[arg(long, conflicts_with_all = ["skills", "tags"])]
    pub all: bool,

    /// Pull every pullable skill carrying this tag. Repeatable; default
    /// semantics is union (any of the given tags). Mutually exclusive with
    /// --skill and --all. Tags are read from each skill's local SKILL.md.
    #[arg(long = "tag", value_name = "TAG", conflicts_with_all = ["skills", "all"])]
    pub tags: Vec<String>,

    /// Switch tag matching from union (any) to intersection (all) when
    /// multiple `--tag` flags are passed. Has no effect without `--tag`.
    #[arg(long, requires = "tags")]
    pub all_tags: bool,

    /// Resolution strategy for divergent skills: `skip` / `overwrite` /
    /// `fork`. `fork` here means **fork-locally**: rename the existing local
    /// folder under a new name, then pull the library version into the
    /// original destination. Requires `--fork-suffix` in non-interactive mode.
    #[arg(long, value_enum, value_name = "POLICY")]
    pub on_divergence: Option<OnDivergence>,

    /// Suffix appended to the original skill name when fork-locally is used
    /// non-interactively (e.g. `--fork-suffix local` → `<name>-local`).
    /// Required when `--on-divergence fork` is used without a TTY.
    #[arg(long, value_name = "SUFFIX")]
    pub fork_suffix: Option<String>,
}

#[derive(Args, Debug)]
pub struct DetectArgs {
    /// Name of a new local skill to add. Repeatable. Mutually exclusive with --all and --tag.
    #[arg(long = "skill", value_name = "NAME", conflicts_with_all = ["all", "tags"])]
    pub skills: Vec<String>,

    /// Add every detected new skill.
    #[arg(long, conflicts_with_all = ["skills", "tags"])]
    pub all: bool,

    /// Add every newly detected skill carrying this tag. Repeatable; default
    /// semantics is union (any of the given tags). Mutually exclusive with
    /// --skill and --all.
    #[arg(long = "tag", value_name = "TAG", conflicts_with_all = ["skills", "all"])]
    pub tags: Vec<String>,

    /// Switch tag matching from union (any) to intersection (all) when
    /// multiple `--tag` flags are passed. Has no effect without `--tag`.
    #[arg(long, requires = "tags")]
    pub all_tags: bool,

    /// Target path inside the library (e.g. `.` for the library root,
    /// `skills`, or `.claude/skills`). Required in non-interactive mode.
    #[arg(long, value_name = "PATH")]
    pub target: Option<PathBuf>,

    /// Also walk paths normally ignored by `.gitignore` (e.g.
    /// `node_modules/`, `vendor/`, `Pods/`). By default `detect` respects
    /// the project's `.gitignore` so a `SKILL.md` smuggled inside a
    /// third-party dependency cannot be silently shipped to the library.
    #[arg(long)]
    pub include_vendored: bool,
}

#[derive(Args, Debug)]
pub struct RemoveArgs {
    /// Skill to remove, by name. Repeatable. Mutually exclusive with --all.
    #[arg(long = "skill", value_name = "NAME", conflicts_with = "all")]
    pub skills: Vec<String>,

    /// Remove every removable skill found in the project (installed via
    /// skillctl, created locally, or orphaned .skills.toml entries).
    #[arg(long, conflicts_with = "skills")]
    pub all: bool,
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
    /// Force the local version onto the library (push), or pull the library
    /// version into the local destination (pull) — discarding the other side.
    Overwrite,
    /// Leave the divergent skill untouched on both sides.
    Skip,
    /// Fork the divergent skill. On `push`, create a new library skill from
    /// the local content. On `pull`, rename the local copy under a new name
    /// and pull the library version into the original destination. Requires
    /// `--fork-suffix` in non-interactive mode.
    Fork,
}
