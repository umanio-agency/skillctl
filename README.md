# skills-cli

A CLI tool to manage your personal Claude skills library across projects.

> Status: pre-v1, in active development. Repo is private until v1.

## Why

You probably have a personal repo where you collect Claude skills. Reusing them in a new project means manually duplicating folders, and any improvement you make in a project is hard to push back to the source repo. `skills-cli` mediates that round-trip without manual copy/paste.

A "skill" is any folder containing a `SKILL.md` file. Where it lives inside the library repo doesn't matter — `skills` discovers them all by file presence.

## What it does

| Flow      | Direction         | Purpose                                                       |
|-----------|-------------------|---------------------------------------------------------------|
| `add`     | library → project | Multi-select skills (with live filter) and copy them in.      |
| `push`    | project → library | Propagate local edits back, with fork support on divergence.  |
| `pull`    | library → project | Refresh installed skills with library updates.                |
| `detect`  | project → library | Find local skills not yet in the library and add them.        |

Plus `init` (link a library) and `list` (read-only inventory).

## Install

Build from source:

```sh
git clone https://github.com/umanio-agency/skills-cli.git
cd skills-cli
cargo install --path .
```

Requires Rust 1.85+ (edition 2024) and a working `git` on `PATH`.

## Quick start

```sh
# Point skills at your personal library repo
skills init https://github.com/your-user/your-skills.git

# See what's available
skills list

# Install some skills into the current project
cd ~/some-project
skills add
```

The interactive `add` shows a multi-select with a live filter — type to narrow the list, ↑/↓ to navigate, space to toggle, enter to confirm.

## Commands

- **`skills init <github-url>`** — clone your library into a local cache.
- **`skills list`** — print every skill in the library, with its tags and description.
- **`skills add`** — multi-select skills and copy them into the current project. Recorded in `.skills.toml`.
- **`skills push`** — push local edits back to the library. On a diverged skill, choose between overwrite, fork-as-new, and skip.
- **`skills pull`** — refresh installed skills with library updates. On a diverged skill, choose between overwrite, fork-locally, and skip.
- **`skills detect`** — find local skills not yet declared in `.skills.toml` and add them to the library.

## Tags

Each `SKILL.md` can carry tags in its frontmatter:

```yaml
---
name: claude-api
description: Build and tune Claude API apps with prompt caching.
tags: [api, claude, caching]
---
```

Use `--tag <name>` (repeatable) on `add` / `list` / `push` / `pull` / `detect` to filter, or `--tag <name> --all-tags` for intersection. `skills add --tag images-gen --dest .claude/skills` bulk-installs every skill carrying `images-gen`.

## Non-interactive / agent mode

Every interactive flow has flag-driven equivalents so an LLM agent can drive the CLI end-to-end:

- Selection: `--skill <name>` (repeatable), `--all`, or `--tag <name>`.
- Destination: `--dest <path>` (add), `--target <library-path>` (detect).
- Conflict resolution: `--on-conflict overwrite|skip|abort` (add), `--on-divergence overwrite|skip|fork` (push, pull) with `--fork-suffix <s>` for non-interactive forks.
- Output: `--json` emits a structured object on stdout (cliclack output suppressed).
- `--no-interaction` forces non-interactive mode on a TTY.

Stable exit codes: `0` success (incl. nothing-to-do), `1` generic, `2` config (missing flag, no library, etc.), `3` conflict, `4` git error.

The full agent contract — flag matrix per command, JSON shapes, recipes, failure modes — lives in [`.claude/skills/skills-cli-usage/SKILL.md`](.claude/skills/skills-cli-usage/SKILL.md). It's installable into any project via `skills add` so the project's agent picks it up.

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
