# skillctl

[![CI](https://github.com/umanio-agency/skillctl/actions/workflows/ci.yml/badge.svg)](https://github.com/umanio-agency/skillctl/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

> CLI to manage your personal library of agent skills across projects. Binary: **`skillctl`**.

> Status: pre-v1, in active development.

## What this is

`skillctl` is a CLI for maintaining a **personal library of agent skills** and keeping it in sync with the projects where you actually use them.

A *skill* is any folder containing a `SKILL.md` file — read as instructions and context by agent tools in the [open agent skills ecosystem](https://skills.sh) (Claude Code, Codex, Cursor, OpenCode, and many more). `skillctl` treats skills as first-class artifacts you author, share across projects, and refine over time:

- **Libraries, many projects.** Keep your skills in git repos. Install any subset into a project with `skillctl add` — from your personal library, a team library, or ad-hoc from any repo URL.
- **Edits where they happen.** Tweak a skill in the heat of a project, then `skillctl push` to send the improvements back to the library it came from.
- **Update everywhere at once.** Fixed a shared skill? `skillctl propagate` (or `push --propagate`) refreshes it in every other project on disk that installed it — no per-project `pull`.
- **Local skills surfaced.** Wrote something new locally? `skillctl detect` finds it and contributes it upstream — or scaffold a fresh one with `skillctl create`.
- **Many libraries, with access levels.** Configure several libraries — `read` (consume only), `write` (commit directly), or `pr` (push a branch and open a PR/MR) — across GitHub, GitLab, and self-hosted git. `pull`/`push` follow each skill back to the library it came from.
- **Safety built in.** Skills installed from a non-default source are content-audited (`skillctl audit`); the git transport is locked to HTTPS/SSH; untrusted manifest fields are sanitised.
- **Conflicts handled.** On divergence, choose overwrite, fork-as-new, fork-locally, or skip — per-skill, per-flow.

The library stays the source of truth; your skills evolve where you use them.

> Already using [`vercel-labs/skills`](https://github.com/vercel-labs/skills) (`npx skills`)? The two compose — same `SKILL.md` format, interoperable libraries. See [Using skillctl alongside `npx skills`](#using-skillctl-alongside-npx-skills) below.

## What it does

| Flow                          | Direction         | Purpose                                                          |
|-------------------------------|-------------------|------------------------------------------------------------------|
| `add`                         | library → project | Multi-select install with live filter; records provenance + `source_sha`. `--from <name\|url>` picks a library or installs ad-hoc from a repo. |
| `list`                        | library → ø       | Inventory with tags + descriptions; `--from all` spans every library. |
| `push`                        | project → library | Diff local edits and commit them back to each skill's own library. `--to` promotes into another writable library. |
| `pull`                        | library → project | Refresh installed skills from their own library; fork-locally on divergence. |
| `propagate`                   | library → projects | Refresh a skill in every project on disk that installed it (`push --propagate` does it in one step). |
| `detect`                      | project → library | Walk the project for new `SKILL.md` files and contribute them (`--to <lib>`). |
| `create`                      | ø → project       | Scaffold a new skill folder with a template `SKILL.md`.          |
| `library`                     | config            | Add/list/remove configured libraries and set the default.        |
| `audit`                       | content           | Scan skill content for dangerous patterns and report a verdict.  |
| `tag`                         | project           | Add/remove tags on a project skill's `SKILL.md` frontmatter.     |

Plus `init` (link the first library). Every multi-skill flow supports `--tag` filtering and `--json` for agents. Pushing to a `pr`-access library opens a PR (`gh`) or MR (`glab`) instead of committing directly.

## Install

### Homebrew (macOS, Linux)

```sh
brew install umanio-agency/homebrew-tap/skillctl
```

Always use the **fully-qualified** form above — `<tap-owner>/<tap-repo>/skillctl`. The unqualified `brew install skillctl` would resolve to whichever tap is currently active in your Homebrew installation, and anyone can create a `homebrew-tap` repo under their own GitHub user and ship a `skillctl.rb` formula. Pinning the tap owner (`umanio-agency`) avoids that typo-squat risk.

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

- **`skillctl init <url>`** — clone your first library into a local cache and mark it the default. Accepts GitHub, GitLab, and self-hosted URLs (HTTPS or SSH).
- **`skillctl list`** — print every skill in the library, with its tags and description. `--from <name>` lists another library; `--from all` spans every configured one.
- **`skillctl add`** — multi-select skills and copy them into the current project (recorded in `.skills.toml`). `--from <name>` installs from a named library; `--from <url>` (or `github:owner/repo`) installs ad-hoc from any repo, with `--save-as` to keep it as a library.
- **`skillctl push`** — push local edits back to each skill's own library. On a diverged skill, choose overwrite, fork-as-new, or skip. `--to <lib>` promotes a skill into another writable library. Pushing to a `pr` library opens a PR/MR. `--propagate` also refreshes each pushed skill in every other project on disk that installed it.
- **`skillctl pull`** — refresh installed skills with updates from their own library. On a diverged skill, choose overwrite, fork-locally, or skip.
- **`skillctl propagate <skill>…`** — refresh a library's current version of a skill in every *other* project that installed it, discovered by scanning `--root <path>` (or the configured `[propagate] roots`) for `.skills.toml`. Sites with local edits are skipped, never clobbered; `--dry-run` previews.
- **`skillctl detect`** — find local skills not yet in `.skills.toml` and add them to a chosen writable library (`--to <lib>`).
- **`skillctl create <name>`** — scaffold a new skill folder with a template `SKILL.md` (frontmatter + body skeleton) in the current project, ready for `detect` to contribute later.
- **`skillctl library add|list|remove|set-default`** — manage configured libraries (each `read` / `write` / `pr`).
- **`skillctl audit`** — scan skill content for dangerous patterns (credentials, obfuscation, risky shell, prompt-injection) and report a verdict.
- **`skillctl tag add|remove <tag>… --skill <name>`** — edit a project skill's frontmatter tags from the CLI.
- **`skillctl remove`** — remove installed/local skills from the current project (never touches the library or git).

## Multiple libraries

Beyond the primary library, configure as many as you like — a team library, a read-only upstream you follow, etc. — each with an access level:

```sh
skillctl library add team https://github.com/acme/skills --access write
skillctl library add upstream https://gitlab.com/vendor/skills   # defaults to --access read
skillctl library list

skillctl add --from team --skill deploy        # install from a named library
skillctl add --from all                        # browse every library (interactive tabs)
skillctl add --from github:acme/playbook        # ad-hoc from any repo, no config needed
```

- **`read`** — consume only. **`write`** — `push` commits directly. **`pr`** — `push` opens a PR (`gh`) / MR (`glab`) for review.
- Each installed skill records which library it came from; `pull` and `push` follow that provenance automatically (a single run can touch several libraries).
- Installing from any non-default source runs the content audit by default (`--no-audit` is refused for third-party content).
- `push --to <writable-lib>` promotes a skill installed from a read-only source into your own or a team library.

A single configured library behaves exactly as before — none of this needs new flags until you add a second.

## Tags

Each `SKILL.md` can carry tags in its frontmatter:

```yaml
---
name: claude-api
description: Build and tune Claude API apps with prompt caching.
tags: [api, claude, caching]
---
```

Use `--tag <name>` (repeatable) on `add` / `list` / `push` / `pull` / `detect` to filter, or `--tag <name> --all-tags` for intersection. `skillctl add --tag images-gen --dest .claude/skills` bulk-installs every skill carrying `images-gen`. Edit a skill's tags without hand-writing YAML with `skillctl tag add <tag> --skill <name>` / `skillctl tag remove …`.

## Non-interactive / agent mode

Every interactive flow has flag-driven equivalents so an LLM agent can drive the CLI end-to-end:

- Selection: `--skill <name>` (repeatable), `--all`, or `--tag <name>`.
- Source / target: `--from <name|url>` / `--from all` (add, list), `--to <lib>` (push promotion, detect), `--save-as <name>` (ad-hoc remote add).
- Destination: `--dest <path>` (add, create), `--target <library-path>` (detect).
- Conflict resolution: `--on-conflict overwrite|skip|abort` (add), `--on-divergence overwrite|skip|fork` (push, pull) with `--fork-suffix <s>` for non-interactive forks.
- Audit: `--no-audit` / `--fail-on <severity>` (add, pull, detect, audit).
- Propagation: `--propagate` + `--root <path>` (push); standalone `skillctl propagate <skill>… --root <path>` with `--dry-run`. Scan roots fall back to `[propagate] roots` in `config.toml` when `--root` is omitted.
- PR/MR: `--pr-title <title>` and `--yes` (push to a `pr` library).
- Output: `--json` emits a structured object on stdout (cliclack output suppressed).
- `--no-interaction` forces non-interactive mode on a TTY.

Stable exit codes: `0` success (incl. nothing-to-do), `1` generic, `2` config (missing flag, no library, etc.), `3` conflict, `4` git error, `5` content-audit threshold exceeded.

The full agent contract — flag matrix per command, JSON shapes, recipes, failure modes — lives in [`.claude/skills/skillctl-usage/SKILL.md`](.claude/skills/skillctl-usage/SKILL.md). It's installable into any project via `skillctl add` so the project's agent picks it up.

## Using skillctl alongside `npx skills`

[`vercel-labs/skills`](https://github.com/vercel-labs/skills) (invoked as `npx skills`) is a popular tool for installing skills from public GitHub repos into a project. The two tools are designed to compose: they use the same `SKILL.md` definition and the same arbitrary-folder discovery, so libraries are interoperable. Use `npx skills` for one-way installs from public registries, and `skillctl` for round-trip management of your own library.

| Capability                                       | `skillctl` | `npx skills` |
|--------------------------------------------------|:----------:|:------------:|
| Install from a GitHub repo                       | ✓          | ✓            |
| Install from GitLab / self-hosted git            | ✓          | ✗            |
| Multiple libraries with read/write/pr access     | ✓          | ✗            |
| Push local edits back to the library             | ✓          | ✗            |
| Open a PR/MR for review (`pr` libraries)         | ✓          | ✗            |
| Detect new local skills and contribute upstream  | ✓          | ✗            |
| Fork-as-new on push divergence                   | ✓          | ✗            |
| Fork-locally on pull divergence                  | ✓          | ✗            |
| Content-audit untrusted skills before install    | ✓          | ✗            |
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
