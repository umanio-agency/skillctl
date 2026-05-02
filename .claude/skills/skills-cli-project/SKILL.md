---
name: skills-cli-project
description: Reference knowledge base for the skills-cli project. Vision, architecture, roadmap, current progress, and decisions log. Load PROACTIVELY at the start of any planning, research, or implementation work on skills-cli so you have full context. Update this skill whenever a meaningful decision is made, a milestone is reached, or the roadmap shifts.
tags: [meta, project-context]
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
| Interactive prompts | `cliclack` | Clack.js-style polished prompts (intro/outro framing, styled log lines, label + dim hint per item). Swapped from `inquire` because long descriptions wrapped awkwardly in `inquire`'s `MultiSelect`. |
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
- `[x]` **Phase 1 — Foundations.** Crates picked (see §3.1). `clap` skeleton wired with five subcommand stubs. Module layout: `src/main.rs` → `src/cli.rs` (clap defs) + `src/commands/{init,list,add,push,detect}.rs`. `cargo build` and `cargo clippy --all-targets -- -D warnings` clean.
- `[x]` **Phase 2 — Library link + listing.** `skills init <github-url>` clones the library into a platform-appropriate cache and persists the URL in `config.toml`. `skills list` reads config, best-effort `git fetch && reset --hard @{upstream}` to refresh, walks the cache via `ignore::WalkBuilder` (hidden dirs included so `.claude/skills/` is found), and prints each `SKILL.md`'s `name` + `description` (frontmatter parsed by a tolerant hand-rolled parser — no YAML crate added). Modules introduced: `src/config.rs`, `src/git.rs`, `src/skill.rs`. Eight unit tests covering URL slugging and frontmatter parsing. End-to-end smoke test against `umanio-agency/skills-cli` itself succeeds.
- `[x]` **Phase 3 — Install.** `skills add` is a fully interactive flow built on `cliclack`: `intro("skills add")`, refresh the library cache, multi-select with **label + truncated hint** per item (descriptions are normalized and cut at the first sentence or 100 chars to keep rows scannable), resolve the destination per §5.1 (recursive scan for folders named `skills`, ignoring `node_modules`/`target`; falls back to the four-preset Select + Custom path), copy each chosen skill folder via a hand-rolled recursive copy, and emit styled `log::success`/`log::info`/`log::warning` lines as it goes. On a destination that already exists, prompt **Overwrite / Skip / Abort** per skill. Abort calls `outro_cancel("aborted")` and persists whatever was already installed; the happy path ends with a summary `outro`. Records each install in `.skills.toml` with the schema agreed in §7. Modules introduced: `src/project_config.rs`. Crates added: `time` (`formatting`) for the RFC3339 `installed_at` timestamp; `cliclack` for the prompt UX. The interactive prompts cannot be exercised from the CLI test harness (they need a real TTY), so manual smoke-testing in a terminal is the validation of last resort.
- `[x]` **Phase 4 — Push back.** `skills push` classifies each entry in `.skills.toml` into one of five states (Unchanged / LocalChangesOnly / BothDiverged / LocalMissing / LibraryMissing) by computing a manifest of git blob SHAs for the local skill folder (via `git hash-object`) and the library at both `source_sha` and `HEAD` (via `git ls-tree -r -z`). Pushable candidates (LocalChangesOnly + BothDiverged) are surfaced in a `cliclack::multiselect` with status hints; divergent ones trigger a per-skill **Overwrite library / Skip** prompt (Fork lands in Phase 5). Selected skills are written into the library cache, staged with `git add -A`, committed once for the whole run with a `update skill(s): …` message, and `git push`-ed. After a successful push, `.skills.toml` is rewritten with the new HEAD SHA on every pushed entry. New module: `src/fs_util.rs` (shared `copy_dir_all` + `replace_folder_contents`). New git helpers: `ls_tree_blobs`, `hash_object`, `add_all`, `has_staged_changes`, `commit`, `push`. Five new unit tests on the manifest diff helper. The interactive flow once again can't be exercised from the test harness.
- `[x]` **Phase 5.5 — Non-interactive agent mode (shipped early from §10.3 backlog).** Every interactive command now has a flag-driven non-interactive twin. Activation is automatic when stdin/stdout aren't a TTY (via `std::io::IsTerminal`); the global `--no-interaction` flag forces it on a TTY. Per-command flags: `add` gets `--skill`/`--all`/`--dest`/`--on-conflict`; `push` gets `--skill`/`--all`/`--on-divergence`/`--message`; `detect` gets `--skill`/`--all`/`--target`. When a required decision isn't supplied via flags in non-interactive mode, the command **fails fast** with a clear error rather than falling back to a prompt. Fork remains interactive-only — non-interactive divergent skills are skipped. New module: `src/context.rs` carries the resolved `Context { interactive }` from `main.rs` to every command. The companion `skills-cli-usage` skill ships at `.claude/skills/skills-cli-usage/SKILL.md`: it's the agent-facing reference (flag matrix, recipes, exit codes) and is itself a usage example of the tool eating its own dog food.
- `[x]` **Phase 5 — Fork + detect.** `skills push` now offers a third option in the divergence prompt — **Fork as new skill** — alongside Overwrite and Skip; the same option is offered when a skill is `LibraryMissing` (its source path no longer exists upstream). The fork prompts for a new name, validates against `/`/`\` and against collisions in the library, places the fork at the original `source_path`'s parent directory, copies the local content there, stages it, commits with a `add skill: …` message (or a 2-line `sync skills\n\nUpdate: a, b\nAdd: x` body when updates and forks land in the same run), pushes, then **renames the local folder** to match the new name and **replaces** the corresponding `.skills.toml` entry (new name, new source_path, new destination, new source_sha, new installed_at). `skills detect` walks cwd for `SKILL.md` files via `skill::discover`, filters out anything whose canonical path matches an existing `.skills.toml` `destination`, multi-selects the leftovers, prompts for a library destination (existing `skills/` folders + custom path; falls back to `skills/` at root if none exist), copies + commits + pushes with a `add skill(s): …` message, and appends new entries to `.skills.toml`. Refactors: `find_skills_folders` moved to `src/skill.rs` (now public), path helpers `relative_to_or_self` / `strip_dot_prefix` moved to `src/fs_util.rs`, the `short_hint` description compactor moved to `src/commands/shared.rs`. New tests on `build_commit_message` (5 cases) bring the suite to 22 passing.
- `[x]` **Phase 5.6 — Pull (shipped from §10.4 backlog, 2026-05-02).** New `skills pull` command closes the round-trip in the inbound direction. Reuses the diff classification machinery from `push` (now extracted to `src/commands/diff.rs`); the `SkillStatus` enum gained a `LibraryAhead { library_changed }` variant so `pull` can distinguish "library moved, local hasn't" (clean pull) from "both moved" (divergent). Apply path: `replace_folder_contents` from library cache to local destination; rewrite `source_sha` to current library HEAD in `.skills.toml`. **No git operations on the project side.** Divergence resolution offers Pull-and-discard / Fork-locally / Skip; fork-locally renames the existing local folder, copies the library version into the original location, and leaves the renamed copy as an untracked SKILL.md folder (which `skills detect` can later pick up). Non-interactive: `--skill`/`--all` + `--on-divergence overwrite|skip` (fork-locally interactive-only, like push fork). Cross-promotion: when `push` finds a `LibraryAhead` skill it now suggests `skills pull`; when `pull` finds a `LocalChangesOnly` skill it suggests `skills push`. End-to-end smoke test against the live library succeeds (`source_sha` rewritten from 7431706 to 6ea8ce7 on `/tmp/test-skills-cli-agent`).
- `[x]` **Phase 5.7 — Skill tags (shipped from §10.1 backlog, 2026-05-02).** Tags live in the `SKILL.md` frontmatter as an inline YAML array (`tags: [a, b, c]`). The hand-rolled parser was extended: arrays with quoted/unquoted elements, empty `[]`, and a forgiving fallback for bare scalars (`tags: foo` → `["foo"]`) all work; block-style YAML deferred. `Skill` gained a `tags: Vec<String>` field. `skills add` got `--tag <name>` (repeatable, mutex with `--skill`/`--all`) plus `--all-tags` (requires `--tag`) to switch matching from union (default, any-of) to intersection (all-of). Non-interactive `--tag X` installs every matching skill; interactive `--tag X` pre-filters the multi-select. `skills list` mirrors the same flags and appends `[tag, tag]` after the skill name when tags are present. `push`/`pull`/`detect` intentionally don't get tag flags in this iteration. Helper `commands::shared::matches_tags(skill_tags, filter, all_tags)` lives in the shared module with four unit tests; frontmatter parsing has five new tests (array, quoted, empty, missing, scalar fallback). The two skills in this repo (`skills-cli-project` and `skills-cli-usage`) gained `tags: [meta, project-context]` and `tags: [meta, agent-tooling]` respectively as dogfood.
- `[ ]` **Phase 6 — Polish & open source.** Help text, error messages, README usage section, CI (lint + test). The **public flip is gated**: per 2026-05-02 user directive, repo stays private until the tool has been used for real on multiple projects and the remaining backlog items the user wants land first. Don't propose flipping public until the user explicitly says they're ready.

> **Backlog (post-v1, see §10):** progressive-filter multi-select prompt.
>
> *Already shipped from the backlog (early):* §10.3 non-interactive agent mode (2026-05-01, see Phase 5.5). §10.4 pull library updates (2026-05-02, see Phase 5.6). §10.1 skill tags (2026-05-02, see Phase 5.7).

## 7. Open questions / decisions still needed

- **Auth for private library repos:** assume the user has `gh` or SSH keys set up, or do we wrap something? Initial answer: assume the host's git credentials work (don't reinvent auth).
- **Should the install destination be remembered?** After the first `skills add`, do later runs default to the same destination silently, or always re-prompt? Likely: remember and re-prompt only if `--reselect` is passed. **Currently:** always re-prompts.
- **Recursive scan depth limit?** No hard limit yet; rely on ignore rules (`.gitignore`, `node_modules`, etc.) to keep it cheap. Revisit if perf is an issue on large monorepos.
- **Multi-line frontmatter values?** Hand-rolled parser is single-line only. Revisit if real skills need multi-line `description:` blocks.
- **Pushing back unshallows the cache?** Phase 4 will need full history. Either re-clone without `--depth=1` (we already do this) or `git fetch --unshallow` if we change the clone strategy. Today the cache is a full clone, so this is a non-issue unless we add shallow clones for speed.

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
- **2026-04-29** — **Storage paths use `directories::ProjectDirs::from("dev", "umanio-agency", "skills-cli")`** — i.e. platform conventions, not forced XDG. On macOS: config under `~/Library/Application Support/dev.umanio-agency.skills-cli/` and cache under `~/Library/Caches/dev.umanio-agency.skills-cli/`. On Linux: `~/.config/skills-cli/` and `~/.cache/skills-cli/`. Each library repo is cached in a subfolder named `<owner>-<repo>`.
- **2026-04-29** — **GitHub-only library URLs in v1.** Accept `https://github.com/owner/repo[.git]` and `git@github.com:owner/repo.git`; reject anything else with a clear error. Other hosts (GitLab, self-hosted) can come post-v1.
- **2026-04-29** — **Frontmatter parser is hand-rolled and tolerant.** It only extracts `name:` and `description:` from a leading `---`-delimited block, supports single-line values with optional quotes, and ignores anything else. No YAML crate added. Multi-line values are not yet supported — revisit if real-world skills need it.
- **2026-04-29** — **Cache refresh on `list` is best-effort.** `git fetch --quiet --prune && git reset --quiet --hard @{upstream}` runs before discovery; if it fails (e.g. offline), `list` prints a warning to stderr and falls back to the cached snapshot.
- **2026-04-29** — **`.skills.toml` schema confirmed:** `[[installed]]` array, each entry has `name`, `source_path` (relative to the library root), `source_sha` (library commit at install time — the anchor used by `push` to detect drift), `destination` (relative to the project root), `installed_at` (RFC3339 UTC).
- **2026-04-29** — **Conflict policy on `add` when destination exists:** prompt the user with **Overwrite / Skip / Abort** per skill. Abort saves whatever was already installed in this run before exiting cleanly. No flag-based override yet.
- **2026-04-29** — **`time` crate added** (`formatting` feature) solely for the RFC3339 `installed_at` timestamp. Cheap dependency, well-maintained; preferred over `chrono` for being modern and feature-scoped.
- **2026-04-29** — **Swapped `inquire` for `cliclack`.** Reason: a long `description` made the multi-select unreadable in `inquire` (the line wrapped over the prompt). `cliclack` shows label + dim hint per item with proper truncation, plus `intro`/`outro`/`log::*` framing that makes the flow feel like a single coherent wizard instead of a sequence of standalone prompts. Hint text is also pre-trimmed (whitespace normalized, cut at the first sentence or 100 chars) so even very long descriptions stay on one line.
- **2026-04-30** — **Diff strategy for `push` is git-blob-based.** For each installed skill we build three blob-SHA manifests — local (via `git hash-object` on each file), library at `source_sha` (via `git ls-tree -r -z`), and library at `HEAD` (same). Equality of manifests determines status: `local == source` → `Unchanged`; `local != source && head == source` → safe push; both differ → divergence. This approach reuses git's own content addressing (no custom hashing), survives line-ending and mode noise, and avoids materializing historical files on disk.
- **2026-04-30** — **`push` uses one commit per run.** All selected skills are staged together and recorded in a single `update skill(s): …` commit, then pushed in one `git push`. Rationale: matches the user's mental model of "I made edits, sync them"; keeps git history readable; reduces the chance of partial-failure states (one push success vs many). Future flag may switch to per-skill commits if needed.
- **2026-04-30** — **Divergence resolution in v1 = Overwrite or Skip.** When local and library both moved past `source_sha`, the user is prompted **Overwrite library** (force-push our version, library-side changes are lost) or **Skip** (do nothing). The third natural option, **Fork as new skill**, intentionally lands in Phase 5 with the rest of the fork flow. There's no automatic merging — that's well outside scope and what `git` itself is for.
- **2026-04-30** — **`installed_at` in `.skills.toml` is not updated on push.** It remains the original install timestamp; only `source_sha` is rewritten when a skill is pushed. A separate `last_pushed_at` field can be added later if the use case appears.
- **2026-04-30** — **Author identity for push commits is the user's git globals.** We do not configure `user.name`/`user.email` on the cache repo; if globals are missing, `git commit` fails with a message we surface verbatim. Reasoning: every dev who is going to push has already configured this, and reinventing identity management in the CLI is out of scope.
- **2026-05-01** — **Fork places the new skill at the original's parent directory, and renames the local folder.** When a divergent skill is forked, the new library path is `parent_of(source_path)/<new-name>` (e.g. fork of `.claude/skills/foo` → `.claude/skills/foo-custom`). Local UX: the project's folder is `fs::rename`-d post-push to match the new name, and the `.skills.toml` entry is **replaced** (not duplicated) — the local folder now belongs to the fork, not the original. Rename + entry replacement happen *after* the push succeeds, so a network failure leaves the project in its pre-push state and the user can retry.
- **2026-05-01** — **Commit message format on `push`:** `update skill(s): …` for updates only, `add skill(s): …` for forks only, and a 2-line body (`sync skills\n\nUpdate: a, b\nAdd: x`) when the same run mixes both. `detect` always uses `add skill(s): …`. One commit per command run, never a commit per skill.
- **2026-05-01** — **`skills detect` filtering uses canonical paths.** Each `.skills.toml` entry's `destination` is canonicalized via `std::fs::canonicalize` and compared against the canonical path of every discovered `SKILL.md`'s parent. Symlinks, redundant `.` segments, and trailing slashes are normalized away by canonicalization, so cosmetic path differences never produce false-positive "new" skills. If canonicalization fails (e.g. broken symlink), the entry is treated as new — the user can still pick whether to add it.
- **2026-05-01** — **`skills detect` library destination falls back to `skills/` when the library has no `skills/` folders yet.** Rationale: a fresh library shouldn't force the user to type a custom path on first use; `skills/` at the root is the most common convention. Custom path remains available.
- **2026-05-01** — **Non-interactive activation is auto-detected via `std::io::IsTerminal` on stdin AND stdout, and overridable with the global `--no-interaction` flag.** When non-interactive, the strict rule applies: a missing required input *fails the command* with a clear "pass --xxx" error — never silently prompts. Reasoning: avoid silent hangs in CI/agent contexts where a forgotten prompt would block forever.
- **2026-05-01** — **Per-command flag surface for agent mode:** `add` → `--skill <name>` (repeatable) or `--all`, `--dest <path>`, `--on-conflict overwrite|skip|abort`. `push` → `--skill` (repeatable) or `--all`, `--on-divergence overwrite|skip`, `--message`. `detect` → `--skill` (repeatable) or `--all`, `--target <library-path>`. The `--skill`/`--all` pair is `conflicts_with` in clap — passing both is rejected up front. `push`'s positional `[SKILLS]…` argument was replaced by `--skill` for cross-command consistency.
- **2026-05-01** — **Fork stays interactive-only in v1.** Non-interactive `push` on a divergent skill: applies `--on-divergence` if present (skip or overwrite), else skips with a warning. Reason: forks need a *new name*, and inventing a name policy (suffix? timestamp?) is a UX decision worth making with intent rather than baking in a default. Backlog item: `--fork-suffix` or `--fork-name` flag if a real use case appears.
- **2026-05-01** — **Exit codes are `0` (success, including "nothing to do") and `1` (any failure) for v1.** Granular codes (config-missing, conflict-unresolved, etc.) intentionally deferred: the human-readable message on stderr already conveys cause, and agents that need finer logic can parse it. Will revisit if a real consumer asks. Documented in `skills-cli-usage`.
- **2026-05-01** — **No `--json` output mode yet.** Reason: defining stable schemas per command is a non-trivial design decision and would require gating *all* `cliclack` output (intro/outro/log lines clash with JSON-on-stdout). Tree-style human output is stable enough for v1 agents that mainly care about exit code + side effects. Deferred to a focused future pass; tracked in §10.3.
- **2026-05-01** — **The `skills-cli-usage` companion skill is the agent-facing source of truth for CLI usage.** Lives at `.claude/skills/skills-cli-usage/SKILL.md` so it travels with the repo and can itself be installed via `skills add` into any project that wants its agent to know how to use the CLI. Update it whenever flag surface, exit codes, or output format changes — it is *the* contract with downstream agents.
- **2026-05-02** — **Repo stays private until the user explicitly OKs the public flip.** The user wants to use the tool on real projects and ship the remaining wanted features (notably tags) before going public. Phase 6 is split implicitly: polish (CI, README, error messages, help text) is fair game whenever; the "make it public" step is gated. Don't volunteer it.
- **2026-05-02** — **`pull` reuses `push`'s blob-SHA classification.** Both are extracted into `src/commands/diff.rs` (`SkillStatus`, `classify`, `local_blob_manifest`, `count_diff`). `SkillStatus::Unchanged` was split into `Unchanged` (both sides at `source_sha`) and `LibraryAhead { library_changed }` (library moved, local didn't), letting `pull` distinguish "clean pullable" from "no-op". `push` and `pull` describe statuses differently and cross-promote each other in their info lines.
- **2026-05-02** — **`pull` does NOT touch project-side git.** It modifies files (via `replace_folder_contents`) and `.skills.toml`, then exits. The user reviews and commits with their own workflow. Reasoning: the project may be a git repo, may not be, may have its own conventions — opinionated commits there would be intrusive. Library-side is read-only too: no commit, no push.
- **2026-05-02** — **`pull` fork-locally semantics:** rename the existing local folder under a user-chosen name (the renamed copy has *no* `.skills.toml` entry — it becomes an "orphan" SKILL.md folder), then copy the library version into the original destination and rewrite the original entry's `source_sha`. The user can later contribute the orphan via `skills detect`. Like push fork, this is interactive-only in v1.
- **2026-05-02** — **Tag storage is in-frontmatter, inline-YAML-only.** Tags live as `tags: [a, b, c]` in the `SKILL.md` frontmatter — discoverable, travels with the skill, no separate registry. Block-style YAML (`tags:\n  - a\n  - b`) is deferred; the parser stays hand-rolled. A bare scalar (`tags: foo`) is accepted as a single-tag list to be forgiving.
- **2026-05-02** — **Tag composition: union by default, intersection on demand.** `--tag X --tag Y` matches skills with either tag (most common need: "everything image-related"); `--all-tags` switches to intersection (`X AND Y`) for narrower picks. `--all-tags` requires `--tag`.
- **2026-05-02** — **`--skill`, `--all`, `--tag` are mutually exclusive in `add`.** Three orthogonal selection modes, one at a time. Combinations like `--skill a --tag b` were considered but add UX surface for low marginal value in v1.
- **2026-05-02** — **Tags only land on `add` and `list` in v1.** `push`/`pull`/`detect` keep their existing flag surface. Reasoning: these commands operate on already-installed or already-classified skills; tag filtering there is a nice-to-have but not blocking. Add later if a real flow asks for it.
- **2026-05-02** — **No `skills tag add/remove` management commands.** Users edit the frontmatter directly. Adding a CLI for this is cheap to defer; the editing UX is fine for now.

## 9. How to use this skill

When starting a session on skills-cli:
1. Read this file first to load full context.
2. Cross-check **§6 Roadmap** for what's done vs next.
3. Cross-check **§7 Open questions** before proposing any design that touches an undecided area.
4. Cross-check **§10 Backlog** before pitching a "new" feature — it may already be parked there with prior framing.
5. After a meaningful change (new decision, milestone reached, roadmap revision), **update this file** in the same change set — ideally in the same commit as the work that prompted the update.

## 10. Backlog (future ideas, not yet scheduled)

Ideas the user wants to explore but that aren't on a phase yet. Each entry lists the idea, the value, and the open design questions / known blockers — so when we *do* schedule them we don't restart from a blank page.

### 10.1 Skill tags

- **Status:** shipped 2026-05-02 as Phase 5.7. MVP covers `add` and `list`; `push`/`pull`/`detect` tag flags and `skills tag add/remove` management commands deliberately not included — open as follow-ups if real use surfaces them.
- **Raised:** 2026-04-30.
- **Idea:** Allow tagging skills (e.g. `images-gen`, `code-review`) to categorize them, and let the user install every skill carrying a given tag in one shot (`skills add --tag images-gen` → bulk install).
- **Value:** Bootstrap a project for a specific workflow without clicking through a multi-select for ten related skills.
- **Open design questions:**
  - **Where do tags live?** Strong default: in the `SKILL.md` frontmatter (`tags: [a, b, c]`) — discoverable, lives with the skill content, replicates naturally on clone/copy. Alternative: a `tags.toml` at the library root, useful if we want centralised tag definitions/aliases.
  - **CLI shape:** likely both — `skills add --tag <name>` for a non-interactive bulk install, *and* a tag-aware filter in the interactive multi-select (chips or a tag-prefixed query like `tag:images-gen`).
  - **Management:** edit-the-frontmatter-manually, or expose `skills tag add/remove`?
  - **Composition:** does an `--tag a --tag b` mean union or intersection? Probably union (more useful default), with an opt-in `--all-tags` for intersection.

### 10.2 Progressive-filter multi-select prompt

- **Raised:** 2026-04-30.
- **Idea:** Replace the current `cliclack::multiselect` with a prompt that has a live search bar:
  1. Type → list filters in real time.
  2. ↑/↓ navigates the filtered list.
  3. Enter while typing in the search → adds the focused skill to the running selection **without** ending the prompt.
  4. The user can then clear the filter, type a new query, navigate, hit Enter again, etc., building up the selection iteratively.
  5. A separate key (Esc/Tab/`>`/explicit "Done" item) finalises and triggers the install.
- **Value:** Much faster bulk selection in large libraries — no scrolling through unrelated skills.
- **Known blockers / options:**
  - `cliclack`'s default `multiselect` uses Space to toggle and Enter to confirm; no built-in live-filter input.
  - `inquire`'s multi-select had a `fuzzy` feature (we left it behind in the cliclack swap), but Enter still meant "confirm everything" — the *additive* Enter semantics is non-standard and AFAIK no off-the-shelf prompt provides it.
  - Likely paths when we get there: (a) build a custom prompt with `ratatui` + `crossterm` (full control, larger investment), (b) revisit `inquire`/`dialoguer` for filter + custom key bindings, (c) layer a small TUI on top of cliclack just for this prompt while keeping the rest of the cliclack flow.
  - **Cross-cutting with §10.1:** filter input should match on name *and* tags, so the two features land naturally in one prompt.

### 10.3 Non-interactive agent mode

- **Status:** shipped 2026-05-01 as Phase 5.5. The MVP covers everything in this entry except the structured `--json` output and granular exit codes — both deferred (see decisions log for rationale). Companion skill lives at `.claude/skills/skills-cli-usage/SKILL.md`.
- **Raised:** 2026-04-30.
- **Idea:** Every interactive command (`init`, `add`, `push`, future `detect`) gets a non-interactive twin that accepts every decision the interactive flow would prompt for as flags/args. No TUI, no TTY required. The goal is that an LLM agent (Claude Code or any other) can drive `skills` end-to-end without a human in the loop.
- **Value:** Unlocks a companion "skills-cli usage" skill that any agent can load to discover and use this CLI as a tool — install/update/push skills as part of larger automations, batch operations across many projects, CI scripting. Agents can't handle a cliclack/inquire TUI (no real TTY), so the current interactive-only shape blocks that whole class of caller.
- **Open design questions:**
  - **Activation:** explicit `--non-interactive` global flag, or auto-detect when stdout/stderr aren't a TTY (`isatty`)? Likely: auto-detect by default + a `--no-interaction` override, with the strict rule *"if a required decision isn't supplied via flags, fail fast with a clear error — never silently fall back to a prompt"*.
  - **Per-command flag surface (sketch):**
    - `init <url>` already non-interactive; just confirm.
    - `add`: `--skill <name>` (repeatable) or `--all`; `--dest <path>` (required when not auto-detectable); `--on-conflict overwrite|skip|abort`.
    - `push`: `--skill <name>` (repeatable) or `--all`; `--on-divergence overwrite|skip|fork`; optional `--message <msg>` to override the auto commit message.
    - `detect` (Phase 5): `--skill <path>` (repeatable) or `--all`; `--target <library-path>`.
  - **Structured output:** a `--json` mode (per-command or global) that emits machine-readable status/results, alongside the human-readable default. Important for agents that want to react to outcomes (which skills installed, which were skipped, new HEAD sha, etc.).
  - **Exit codes:** stable and documented (e.g. `0` success, `2` nothing to do, `3` unresolved conflict, `4` config missing) so agents can branch on result without parsing text.
  - **Companion artifact:** a Claude-skill named e.g. `skills-cli-usage` shipped *with this repo* that documents the non-interactive surface and gives agents recipes ("to install the foo skill into a project, run …"). Probably lives at `.claude/skills/skills-cli-usage/SKILL.md` and is itself a usage example of the tool eating its own dog food.
- **Design implication for v1:** every interactive prompt we add from now on should be *designed* with a flag-based equivalent in mind, even if we ship the interactive version first. Cheap to design in up front; expensive to retrofit later.

### 10.4 Pull library updates into installed skills

- **Status:** shipped 2026-05-02 as Phase 5.6. The MVP covers everything in this entry; `--fork-suffix` for non-interactive local fork was deliberately not added (same reason as push fork — naming is a UX decision worth keeping intentional).
- **Raised:** 2026-05-01 (surfaced while testing Phase 4).
- **Idea:** A new command (working title `skills update` / `skills pull`) that detects when the library has moved past a `.skills.toml` entry's `source_sha` and offers to refresh the local copy with the upstream version. Today, `push` correctly reports "no local changes" but says nothing about *upstream* changes the user could benefit from.
- **Value:** Closes the round-trip the other way. Without this, the only way to pick up library updates is to delete the local skill and run `skills add` again — losing local edits and re-doing destination selection.
- **Open design questions:**
  - **Status surface:** classification logic from `push` already knows when `head_eq_source` is false. We can reuse it: a skill with `local_eq_source && !head_eq_source` is a clean **pullable**.
  - **Conflict policy:** if local has also changed (BothDiverged), pulling would clobber local edits. Same prompt shape as `push` divergence: **Pull (overwrite local) / Skip / Fork-locally** (stash the local version under a new name without touching the library).
  - **Atomicity:** copy library content into local destination via `replace_folder_contents`, update `source_sha` to current HEAD in `.skills.toml`. No git operations on the project side.
  - **CLI:** dedicated command, or a `--pull` flag on a hypothetical `skills sync` aggregator? Probably dedicated for v1; avoid an "everything everywhere" command.
