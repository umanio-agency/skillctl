# Setting up a dev machine

How to get a working `skillctl` checkout on a fresh machine. For contribution rules (commit style, PR process, dependency policy), see [CONTRIBUTING.md](CONTRIBUTING.md).

## Prerequisites

- Rust 1.85+ (edition 2024), installed via [rustup](https://rustup.rs). No `rust-toolchain` file is pinned; latest stable works.
- `git` on `PATH` — the CLI shells out to it.
- Optional: `gh` (GitHub CLI) or `glab` (GitLab CLI), only to exercise PR/MR-mode libraries.
- Optional: Homebrew, to install the released binary alongside the dev build (`brew install umanio-agency/homebrew-tap/skillctl`).

## Clone

```sh
git clone https://github.com/umanio-agency/skillctl.git
cd skillctl
```

Work happens on `main`. There are no submodules or nested repos.

## Build

```sh
cargo build
```

No environment variables, `.env` files, or secrets are needed to build or test.

## Local-only files (not versioned)

These are gitignored on purpose. They are not required to build or test, but an agent working on the repo uses them:

| Path | Role |
| --- | --- |
| `.claude/skills/*/` (except `skillctl-usage/`) | Locally installed Claude skills (e.g. a maintainer-only project knowledge base). |
| `.claude/skills/.skills.toml`, `.skills.toml` | `skillctl` install state for those skills. |

Restore them from your own backup, or reinstall from your library with `skillctl add`.

`skillctl`'s user config lives outside the repo (`~/Library/Application Support/dev.umanio-agency.skills-cli/config.toml` on macOS). Recreate it with `skillctl init` or copy it from another machine.

## Verify

```sh
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test --all
cargo run -- --help
```

All four must pass before you push.
