# skillctl

[![CI](https://github.com/umanio-agency/skillctl/actions/workflows/ci.yml/badge.svg)](https://github.com/umanio-agency/skillctl/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

> CLI to manage your personal library of agent skills across projects. Binary: **`skillctl`**.

> Status: pre-v1, in active development.

## What this is

`skillctl` is a CLI for maintaining a **personal library of agent skills** and keeping it in sync with the projects where you actually use them.

A *skill* is any folder containing a `SKILL.md` file — read as instructions and context by agent tools in the [open agent skills ecosystem](https://skills.sh) (Claude Code, Codex, Cursor, OpenCode, and many more). `skillctl` treats skills as first-class artifacts you author, share across projects, and refine over time:

- **One library, many projects.** Keep your skills in a single git repo. Install any subset into a project with `skillctl add`.
- **Edits where they happen.** Tweak a skill in the heat of a project, then `skillctl push` to send the improvements back to the library.
- **Local skills surfaced.** Wrote something new locally? `skillctl detect` finds it and contributes it upstream.
- **Conflicts handled.** On divergence, choose overwrite, fork-as-new, fork-locally, or skip — per-skill, per-flow.

The library stays the source of truth; your skills evolve where you use them.

> Already using [`vercel-labs/skills`](https://github.com/vercel-labs/skills) (`npx skills`)? The two compose — same `SKILL.md` format, interoperable libraries. See [Using skillctl alongside `npx skills`](#using-skillctl-alongside-npx-skills) below.

## What it does

| Flow                          | Direction         | Purpose                                                          |
|-------------------------------|-------------------|------------------------------------------------------------------|
| `add`                         | library → project | Multi-select install with live filter; records `source_sha`.     |
| `list`                        | library → ø       | Inventory with tags + descriptions.                              |
| `push`                        | project → library | Diff local edits and commit them back.                           |
| `pull`                        | library → project | Refresh installed skills; fork-locally on divergence.            |
| `detect`                      | project → library | Walk the project for new `SKILL.md` files and contribute them.   |
| `push --on-divergence fork`   | project → library | Fork as a *new* library skill when local has diverged.           |

Plus `init` (link a library). Every multi-skill flow supports `--tag` filtering and `--json` for agents.

## Install

### Homebrew (macOS, Linux)

```sh
brew install umanio-agency/homebrew-tap/skillctl
```

### Cargo (any platform with Rust 1.85+)

```sh
cargo install skillctl
```

### `curl | sh` (macOS, Linux)

```sh
curl -LsSf https://github.com/umanio-agency/skillctl/releases/latest/download/skillctl-installer.sh | sh
```

### PowerShell (Windows)

```powershell
irm https://github.com/umanio-agency/skillctl/releases/latest/download/skillctl-installer.ps1 | iex
```

### From source

```sh
git clone https://github.com/umanio-agency/skillctl.git
cd skillctl
cargo install --path .
```

Any method gives you a `skillctl` binary on `PATH`. The `git` CLI is a runtime dependency (skillctl shells out to it).

## Quick start

```sh
# One-time: point skillctl at your library
skillctl init https://github.com/your-user/your-skills.git

# Install some skills into a project (records source_sha for the round-trip)
cd ~/some-project
skillctl add

# Edit a skill in the project, then push the edits back to the library
skillctl push --all

# Or detect a brand-new local skill and contribute it upstream
skillctl detect --target .
```

The interactive `add` / `push` / `pull` / `detect` show a multi-select with a live filter — type to narrow the list, ↑/↓ to navigate, space to toggle, enter to confirm.

## Commands

- **`skillctl init <github-url>`** — clone your library into a local cache.
- **`skillctl list`** — print every skill in the library, with its tags and description.
- **`skillctl add`** — multi-select skills and copy them into the current project. Recorded in `.skills.toml`.
- **`skillctl push`** — push local edits back to the library. On a diverged skill, choose between overwrite, fork-as-new, and skip.
- **`skillctl pull`** — refresh installed skills with library updates. On a diverged skill, choose between overwrite, fork-locally, and skip.
- **`skillctl detect`** — find local skills not yet declared in `.skills.toml` and add them to the library.

## Tags

Each `SKILL.md` can carry tags in its frontmatter:

```yaml
---
name: claude-api
description: Build and tune Claude API apps with prompt caching.
tags: [api, claude, caching]
---
```

Use `--tag <name>` (repeatable) on `add` / `list` / `push` / `pull` / `detect` to filter, or `--tag <name> --all-tags` for intersection. `skillctl add --tag images-gen --dest .claude/skills` bulk-installs every skill carrying `images-gen`.

## Non-interactive / agent mode

Every interactive flow has flag-driven equivalents so an LLM agent can drive the CLI end-to-end:

- Selection: `--skill <name>` (repeatable), `--all`, or `--tag <name>`.
- Destination: `--dest <path>` (add), `--target <library-path>` (detect).
- Conflict resolution: `--on-conflict overwrite|skip|abort` (add), `--on-divergence overwrite|skip|fork` (push, pull) with `--fork-suffix <s>` for non-interactive forks.
- Output: `--json` emits a structured object on stdout (cliclack output suppressed).
- `--no-interaction` forces non-interactive mode on a TTY.

Stable exit codes: `0` success (incl. nothing-to-do), `1` generic, `2` config (missing flag, no library, etc.), `3` conflict, `4` git error.

The full agent contract — flag matrix per command, JSON shapes, recipes, failure modes — lives in [`.claude/skills/skillctl-usage/SKILL.md`](.claude/skills/skillctl-usage/SKILL.md). It's installable into any project via `skillctl add` so the project's agent picks it up.

## Using skillctl alongside `npx skills`

[`vercel-labs/skills`](https://github.com/vercel-labs/skills) (invoked as `npx skills`) is a popular tool for installing skills from public GitHub repos into a project. The two tools are designed to compose: they use the same `SKILL.md` definition and the same arbitrary-folder discovery, so libraries are interoperable. Use `npx skills` for one-way installs from public registries, and `skillctl` for round-trip management of your own library.

| Capability                                       | `skillctl` | `npx skills` |
|--------------------------------------------------|:----------:|:------------:|
| Install from a GitHub repo                       | ✓          | ✓            |
| Push local edits back to the library             | ✓          | ✗            |
| Detect new local skills and contribute upstream  | ✓          | ✗            |
| Fork-as-new on push divergence                   | ✓          | ✗            |
| Fork-locally on pull divergence                  | ✓          | ✗            |
| Tag-based filtering across all flows             | ✓          | ✗            |
| Stable `--json` output + granular exit codes     | ✓          | ✗            |

### Pain points it addresses

If you already use `npx skills`, you may have hit these:

- **Local edits wiped on `npx skills update`** ([vercel-labs/skills#455](https://github.com/vercel-labs/skills/issues/455)) — `skillctl pull --on-divergence fork --fork-suffix local` renames your local copy as `<name>-local` and pulls the library version into the original destination. No choice between "lose edits" and "skip update".
- **Hand-written local skills mixed into the install set** ([vercel-labs/skills#268](https://github.com/vercel-labs/skills/issues/268)) — `skillctl detect` walks the project for `SKILL.md` files not in `.skills.toml` and offers to contribute them to your library in one commit.
- **No path to push library improvements you made in a project** — `skillctl push` diffs each installed skill against the library at its `source_sha`, classifies the change, and commits + pushes the selected ones in a single library-side commit.

## Development

```sh
cargo build
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --all -- --check
```

CI runs all of the above on each push and pull request.

## License

MIT
