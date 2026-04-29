---
name: skills-cli-project
description: Reference knowledge base for the skills-cli project. Vision, architecture, roadmap, current progress, and decisions log. Load PROACTIVELY at the start of any planning, research, or implementation work on skills-cli so you have full context. Update this skill whenever a meaningful decision is made, a milestone is reached, or the roadmap shifts.
---

# skills-cli — project reference

> **This document is the single source of truth for the project's state.** Update it as we go. New decisions go in the decisions log with a date. Roadmap status moves as work progresses. If a section becomes wrong, fix it — don't leave stale info.

---

## 1. Vision

A Rust CLI that lets a developer maintain **one personal repo of Claude skills** and reuse them across projects without manual copy/paste, while keeping both directions of the round-trip painless:

- **Library → Project:** install selected skills into a project.
- **Project → Library:** push edits made in a project back to the library, or fork them as a new skill.
- **Project → Library (new):** detect skills created locally and offer to add them to the library.

**Non-restrictive by design.** The CLI must work with any GitHub repo regardless of folder structure — skills are discovered by the presence of a `SKILL.md` file, never by a fixed layout. This is what makes it open-source-friendly: anyone can point it at their own skills repo.

## 2. User context

- **Who:** Fernando (umanio-agency), single dev, owns a personal skills repo on GitHub.
- **Pain point that triggered this:** manually duplicating skills into each project, then losing changes because syncing back to the source repo is tedious.
- **End state target:** make the tool open-source after v1.

## 3. Tech stack

- **Language:** Rust, edition 2024.
- **Crate name:** `skills-cli`. **Binary name:** `skills` (configured via `[[bin]]` in `Cargo.toml`).
- **License:** MIT.
- **Repo:** `umanio-agency/skills-cli` — **private until v1**, then public.
- **CLI language:** English. All user-facing strings (help text, prompts, error messages, log output) are in English regardless of the developer's working language.

### 3.1 Dependencies

| Concern | Choice | Notes |
|---|---|---|
| Argument parsing | `clap` v4 (`derive`) | Subcommand-based CLI. |
| Interactive prompts | `inquire` | Multi-select with built-in fuzzy filter. |
| Directory walking | `ignore` (engine behind `ripgrep`) | Honours `.gitignore`; skips `node_modules`/`target`/`.git` by default. |
| Config (de)serialization | `serde` (`derive`) + `toml` | TOML for both global and per-project config. |
| Platform paths | `directories` | Portable XDG paths for cache/config dirs. |
| Error handling | `anyhow` at the binary edge | Add `thiserror` later only if a caller needs to match variants. |
| Git access | **Shell out to `git`** via `std::process::Command`, encapsulated in a `git` module | Reuses the user's existing auth (gh helper, SSH agent). Swap to `gix` later if needed. |

Sync only — no `tokio`. No colour/logging crate yet; `inquire` styles its own prompts and we use plain `println!` until a real need arises.

## 4. Domain model

A **skill** = any folder containing a `SKILL.md` file. Nothing else qualifies as a skill, and the parent folder hierarchy is irrelevant.

Two repos are involved at runtime:
- **Library repo** — user's personal collection (source of truth).
- **Project repo** — where skills are installed and edited.

The CLI mediates four flows between them:

| Flow      | Direction         | Purpose                                                       |
|-----------|-------------------|---------------------------------------------------------------|
| `install` | library → project | Multi-select skills and copy them into the project.           |
| `push`    | project → library | Propagate local edits back to the original skill.             |
| `fork`    | project → library | Create a *new* skill in the library from edited local content.|
| `detect`  | project → library | Find skills created locally and offer to add them.            |

## 5. Architecture (working hypothesis)

These are *proposed* but not yet committed — see §7 for what's still open.

- **Library repo handling:** clone once into a local cache (`~/.cache/skills-cli/<owner>-<repo>`), `git fetch` on each command. Uses git for diff/push primitives instead of GitHub API calls. Works offline once cached.
- **Config storage:**
  - **Global:** `~/.config/skills-cli/config.toml` — default library repo URL, auth hint.
  - **Per-project:** `.skills.toml` at the project root — installed skills with the source commit SHA each was copied from (needed to detect drift on `push`).
- **CLI surface (sketch):**
  - `skills init <github-url>` — set the library repo for the current user (or this project).
  - `skills list` — list all skills detected in the library.
  - `skills add` — interactive multi-select; copies selected skills into the project and updates `.skills.toml`.
  - `skills push [<skill>...]` — diff installed skills against library, push changes; prompts for fork-vs-overwrite when edits diverge.
  - `skills detect` — scan project for `SKILL.md` files not in `.skills.toml`, offer to add them to the library.

### 5.1 Install path resolution

When `skills add` is run, the CLI must decide *where in the current project* to drop the selected skills. The destination is **never hardcoded** — it's chosen interactively each run (or persisted in `.skills.toml` after the first run; see §7).

**Step 1 — Discover existing destinations.** Recursively scan the current working directory for folders **literally named `skills`** at any depth (root *and* nested). Examples that qualify:
- `./skills/`
- `./.claude/skills/`
- `./.codex/skills/`
- `./packages/agent/skills/`

Scan should respect `.gitignore` and skip `node_modules`, `target`, `.git`, and other obvious noise. List each match as a choice the user can pick.

**Step 2a — If at least one `skills` folder is found:** present the list of paths as the install destinations. The user picks one. Always include **"Custom path…"** as an additional option so the user can type a destination outside the detected set.

**Step 2b — If none is found:** offer the four preset destinations plus a custom path. Selecting any of them creates the folder if missing:

| Choice    | Path              |
|-----------|-------------------|
| `claude`  | `.claude/skills`  |
| `codex`   | `.codex/skills`   |
| `cursor`  | `.cursor/skills`  |
| `agents`  | `.agents/skills`  |
| Custom…   | user-typed path   |

Each selected skill is copied into `<destination>/<skill-name>/` (preserving the skill folder name from the library).

## 6. Roadmap

Status legend: `[ ]` not started · `[~]` in progress · `[x]` done

- `[x]` **Phase 0 — Bootstrap.** Repo created (private), Rust binary crate scaffolded, README/LICENSE/`.gitignore`, `.claude/CLAUDE.md`, this reference skill.
- `[~]` **Phase 1 — Foundations.** Crates picked (see §3.1). `clap` skeleton wired with five subcommand stubs (`init`, `list`, `add`, `push`, `detect`) — each returns `Ok` after a placeholder `println!`. Module layout: `src/main.rs` → `src/cli.rs` (clap defs) + `src/commands/{init,list,add,push,detect}.rs`. `cargo build` and `cargo clippy --all-targets -- -D warnings` are clean. Error type formalisation deferred — `anyhow` is enough until a real need surfaces.
- `[ ]` **Phase 2 — Library link + listing.** `skills init`, `skills list`. Cache + clone/fetch logic. `SKILL.md` discovery via `walkdir`.
- `[ ]` **Phase 3 — Install.** `skills add` with interactive multi-select. Copy skill folder, write `.skills.toml` with source commit SHA.
- `[ ]` **Phase 4 — Push back.** `skills push` — diff installed skill vs library, commit + push. Detect divergence (when both sides changed) and surface conflict resolution.
- `[ ]` **Phase 5 — Fork + detect.** `skills push --as-new` (or interactive prompt) for forking. `skills detect` for new local skills.
- `[ ]` **Phase 6 — Polish & open source.** Help text, error messages, README usage section, CI (lint + test), publish public, optional crates.io release.

## 7. Open questions / decisions still needed

- **Cache location:** `~/.cache/skills-cli/` (resolved via `directories`) vs `~/.claude/skills-cli/cache/`. Leaning XDG `.cache`. Confirm before Phase 2.
- **`.skills.toml` schema:** what to record per installed skill — source commit SHA, install timestamp, source path within the library, **destination path within the project** (so subsequent `add` runs can default to the same root)?
- **Auth for private library repos:** assume the user has `gh` or SSH keys set up, or do we wrap something? Initial answer: assume the host's git credentials work (don't reinvent auth).
- **Should the install destination be remembered?** After the first `skills add`, do later runs default to the same destination silently, or always re-prompt? Likely: remember and re-prompt only if `--reselect` is passed.
- **Recursive scan depth limit?** No hard limit yet; rely on ignore rules (`.gitignore`, `node_modules`, etc.) to keep it cheap. Revisit if perf is an issue on large monorepos.

## 8. Decisions log

Append-only. Date each entry. When a decision is later reversed, add a new entry referencing the old one — don't edit history.

- **2026-04-29** — Project bootstrapped. Language: **Rust 2024**. Crate `skills-cli`, binary `skills`. License: **MIT**. Repo `umanio-agency/skills-cli`, **private** until v1.
- **2026-04-29** — A skill is identified by the presence of a `SKILL.md` file; folder structure of the library repo is irrelevant. This is a hard requirement (open-source friendliness).
- **2026-04-29** — Default branch `main`. No `Co-Authored-By: Claude` trailer in commit messages (user preference, also recorded in `.claude/CLAUDE.md`).
- **2026-04-29** — This reference skill (`skills-cli-project`) is the canonical source for project state. Update it as work progresses.
- **2026-04-29** — **CLI language is English** (help, prompts, errors, logs). Documentation may be bilingual later, but the binary itself ships English-only.
- **2026-04-29** — **Install destination is interactive, never hardcoded.** On `skills add`, recursively scan cwd for folders named `skills` (any depth, respecting common ignore rules) and let the user pick. If none are found, offer four presets — `.claude/skills`, `.codex/skills`, `.cursor/skills`, `.agents/skills` — plus a custom-path option. The custom-path option is also offered when matches *are* found, so the user can override.
- **2026-04-29** — **Phase 1 dependency stack confirmed:** `clap` (derive), `inquire`, `serde` + `toml`, `ignore`, `directories`, `anyhow`. Git operations go through a thin internal `git` module that **shells out to `git`** (rationale: reuses the user's existing auth — gh credential helper, SSH agent — without reimplementing it). Sync only, no async runtime. No logging or colour crate yet.
- **2026-04-29** — **Module layout:** `src/main.rs` (entry + dispatch), `src/cli.rs` (clap definitions), `src/commands/{init,list,add,push,detect}.rs` (one file per subcommand, each exposing `pub fn run(args) -> Result<()>`). Domain modules (`config`, `git`, `skill`) will be added in the phase that needs them — not pre-created.

## 9. How to use this skill

When starting a session on skills-cli:
1. Read this file first to load full context.
2. Cross-check **§6 Roadmap** for what's done vs next.
3. Cross-check **§7 Open questions** before proposing any design that touches an undecided area.
4. After a meaningful change (new decision, milestone reached, roadmap revision), **update this file** in the same change set — ideally in the same commit as the work that prompted the update.
