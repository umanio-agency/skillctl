# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0] - 2026-05-09

### Added

- `init` — clone a GitHub-hosted skills library into a per-user cache.
- `list` — print every skill in the library with name, description, and tags.
- `add` — multi-select install with live-filter prompt; records `source_sha` in `.skills.toml` to enable round-trip diffing.
- `push` — diff installed skills against the library (git-blob-based), with fork-as-new and overwrite/skip on divergence; one commit per run.
- `pull` — refresh installed skills from the library; fork-locally on divergence preserves your edits under a new name.
- `detect` — find new local `SKILL.md` files not in `.skills.toml` and contribute them to the library in a single commit.
- Tag filtering (`--tag`, `--all-tags`) on every multi-skill flow. Tags live in `SKILL.md` frontmatter (inline or block YAML).
- Non-interactive (agent) mode: auto-detected via `IsTerminal`, forceable via `--no-interaction`. Every interactive decision has a flag-driven equivalent.
- `--json` output mode with stable per-command schemas (init / list / add / push / pull / detect).
- Granular exit codes: `0` success, `1` generic, `2` config, `3` conflict, `4` git.
- Live-filter multi-select prompt: type to narrow, ↑/↓/space/enter, Esc to cancel.
- Companion skills under `.claude/skills/`: `skills-cli-project` (vision, architecture, decisions log) and `skills-cli-usage` (agent-facing CLI contract).
- CI on GitHub Actions (`fmt --check`, `clippy -D warnings`, `build`, `test`).

### Changed

- Binary renamed from `skills` to `skillctl` to avoid shadowing `vercel-labs/skills` (the `npx skills` CLI) on `$PATH`. Crate name remains `skills-cli`.
- README repositioned as the contributor-side companion to `npx skills`, with explicit comparison and pain-point-to-command mapping.
