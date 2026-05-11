# skillctl

[![CI](https://github.com/umanio-agency/skillctl/actions/workflows/ci.yml/badge.svg)](https://github.com/umanio-agency/skillctl/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

> The contributor-side CLI for personal Claude skills libraries. Binary: **`skillctl`**.

> Status: pre-v1, in active development.

## What this is

[`vercel-labs/skills`](https://github.com/vercel-labs/skills) (invoked as `npx skills`) is the canonical tool for *installing* skills from GitHub repos into a project. If that's all you need, use it — it has broader agent support and a larger ecosystem.

`skillctl` covers what `npx skills` doesn't: the **round-trip back to your library**. Push local edits back, detect skills you wrote locally and contribute them upstream, fork as a new skill on divergence, fork-locally on pull. A "skill" is any folder containing a `SKILL.md` file — the same definition `npx skills` uses, so libraries are interchangeable between the two tools.

## What it does

| Flow                          | Direction         | Purpose                                                          |
|-------------------------------|-------------------|------------------------------------------------------------------|
| `push`                        | project → library | Diff local edits and commit them back.                           |
| `detect`                      | project → library | Walk the project for new `SKILL.md` files and contribute them.   |
| `push --on-divergence fork`   | project → library | Fork as a *new* library skill when local has diverged.           |
| `pull`                        | library → project | Refresh installed skills; fork-locally on divergence.            |
| `add`                         | library → project | Multi-select install with live filter; records `source_sha`.     |
| `list`                        | library → ø       | Inventory with tags + descriptions.                              |

Plus `init` (link a library). Every multi-skill flow supports `--tag` filtering and `--json` for agents.

## Install

Build from source:

```sh
git clone https://github.com/umanio-agency/skillctl.git
cd skillctl
cargo install --path .
```

Requires Rust 1.85+ (edition 2024) and a working `git` on `PATH`. The binary lands as `skillctl`.

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

## Comparison with `npx skills`

If you only consume skills, `npx skills` is the right tool — broader agent support, larger ecosystem. `skillctl` is the contributor-side companion. What it adds on top:

| Feature                                        | `skillctl` | `npx skills` |
|------------------------------------------------|:----------:|:------------:|
| Install from a GitHub repo                     | ✓          | ✓            |
| Push local edits back to the library           | ✓          | ✗            |
| Detect new local skills and contribute upstream | ✓         | ✗            |
| Fork-as-new on push divergence                 | ✓          | ✗            |
| Fork-locally on pull divergence                | ✓          | ✗            |
| Tag-based filtering across all flows           | ✓          | ✗            |
| Stable `--json` + granular exit codes          | ✓          | ✗            |

### Pain points it addresses

If you already use `npx skills`, you may have hit these:

- **Local edits wiped on `npx skills update`** ([vercel-labs/skills#455](https://github.com/vercel-labs/skills/issues/455)) — `skillctl pull --on-divergence fork --fork-suffix local` renames your local copy as `<name>-local` and pulls the library version into the original destination. No choice between "lose edits" and "skip update".
- **Hand-written local skills mixed into the install set** ([vercel-labs/skills#268](https://github.com/vercel-labs/skills/issues/268)) — `skillctl detect` walks the project for `SKILL.md` files not in `.skills.toml` and offers to contribute them to your library in one commit.
- **No path to push library improvements you made in a project** — not in their tracker explicitly, but implicit in the issues above. `skillctl push` diffs each installed skill against the library at its `source_sha`, classifies the change, and commits + pushes the selected ones in a single library-side commit.

The two tools are layout-compatible — same `SKILL.md` definition, same arbitrary-folder discovery — so you can use `npx skills` for installs and `skillctl` for round-trips on the same library.

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
