# skills-cli

CLI tool to manage personal Claude skills libraries across projects. Pre-v1.

## Stack
- Rust, edition 2024
- Binary crate `skills-cli`, binary name `skillctl` (see `[[bin]]` in `Cargo.toml`)

## Commands
- `cargo build` / `cargo run -- <args>` / `cargo test`
- `cargo clippy --all-targets -- -D warnings` before pushing
- `cargo fmt` for formatting

## Domain model
A **skill** is any folder containing a `SKILL.md` file. Detection must be folder-structure-agnostic — the user's library repo can place skills anywhere.

Two repos are involved at runtime:
- **Library repo** — user's personal collection of skills (source of truth).
- **Project repo** — where skills are installed and may be edited.

Core flows the CLI must support:
- `install`  — library → project (multi-select)
- `push`     — project → library (propagate local edits)
- `fork`     — project → library as a *new* skill (when local edits should not overwrite the original)
- `detect`   — project → library (find new skills created locally and offer to add them)

## Conventions
- Prefer typed errors (`thiserror` / `anyhow` at the binary edge) over `unwrap`/`expect` outside tests.
- Keep modules small and single-purpose.
- Default to no comments; only add one when the *why* is non-obvious.
- Don't add features, abstractions, or fallbacks beyond what the current task requires.

## Git / commits
- **Never** add `Co-Authored-By: Claude ...` (or any Claude/Anthropic co-author trailer) to commit messages.
- Subject in imperative mood; body explains *why*, not *what*.
- Default branch: `main`. Remote: `origin` → `umanio-agency/skills-cli`.
- Never force-push without explicit user approval.

## Out of scope until requested
- Publishing to crates.io
- Release automation / CI workflows
- Cross-platform installers
