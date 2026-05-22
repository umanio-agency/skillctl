# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.4] - 2026-05-22

### Security & robustness

Close the seven HIGH atomicity / concurrency / DoS findings from the comprehensive audit's Phase 8.2. The headline items are not exploitable by an external attacker on a single-user box, but each represents a real data-loss or denial-of-service scenario under realistic conditions (Ctrl-C mid-operation, two concurrent `skillctl` runs, a malicious `.skills.toml` PR with an orphan `source_sha`).

- **Atomic `replace_folder_contents`.** The copy primitive used by `add` / `pull` / `push` now stages new content into a uniquely-named sibling of the destination, moves the old destination aside into a backup sibling, then atomically renames the staging dir over the destination. At any crash point, either the old or the new content is in place — never a half-written tree. Rolls the backup back if the final rename fails. Closes three HIGH findings (H5, H6, H7) with one primitive.
- **Atomic `.skills.toml` save.** `project_config::save` writes to a sibling temp file then `fs::rename`s it over the target — a crash mid-write only leaves the temp file on disk, never a truncated `.skills.toml`. Used by every command that mutates the tracked-skills index.
- **Process-level locking on the library cache and `.skills.toml`.** New `src/lock.rs` provides `acquire_exclusive(dir, what)` backed by `fs4`'s cross-platform `try_lock_exclusive`. Every command that touches the library cache (`list` / `add` / `push` / `pull` / `detect`) holds an exclusive lock on `<cache>/.skillctl.lock` for the full `git fetch → mutate → push` critical section; every command that mutates `.skills.toml` additionally locks `<cwd>/.skillctl.lock`. A second concurrent `skillctl` invocation fails fast with `AppError::Conflict` ("another skillctl is running") rather than racing on `.git/index.lock`. Closes H3 + H4.
- **`push` saves `.skills.toml` before any local rename.** Post-`git push`, the apply loop is now split into three phases: in-memory mutations, atomic save, then local renames (now non-fatal). A Ctrl-C between push and save used to leave `.skills.toml` referencing the old `source_sha`, which the next run would reclassify as `LibraryAhead` and offer to wipe local edits silently. The new ordering reduces the failure window to "disk full or EACCES at save time"; local rename failures degrade to a warning ("library updated but local rename failed — rename the local folder by hand") rather than dropping the SHA mapping. Closes H6.
- **`pull` fork-locally is now atomic.** The pre-v0.1.4 sequence (`fs::rename` original aside, then `copy_dir_all` library version) could lose the original on a mid-copy failure (rename succeeded, copy failed, original gone, library version not yet present). Rewritten with the same tempdir-swap pattern as `replace_folder_contents` via the new `fs_util::swap_with_bak` helper. Closes H7.
- **Orphan `source_sha` is per-skill, not a batch DoS.** A malicious `.skills.toml` entry with `source_sha = "0000…"` (a valid-hex but unknown commit) used to make `classify` return `Err` at the first such entry and abort the entire batch — weaponisable to DoS every other skill in the same `pull --all` / `push --all` run. `git::ls_tree_blobs` now returns `Result<Option<HashMap>>`, with `Ok(None)` for an unknown refspec; the classifier surfaces this as a new `SkillStatus::SourceShaOrphaned` variant, and `push` / `pull` log a per-skill warning ("source_sha doesn't resolve in the library; skipping") while continuing with the rest. Closes H9.
- **`pull --all` continues on per-skill failure.** The apply loop now wraps each skill in an IIFE that logs a warning on error and continues. `.skills.toml` is saved at the end regardless, so successful per-skill `source_sha` updates persist even when a sibling apply fails. Closes H8 (pull side). The push-side equivalent (one-commit-per-run cleanup-on-failure) is deferred to a follow-up release.

3 new unit tests cover the atomic-replace contract (failure preserves dst, failure cleans up staging, `swap_with_bak` round-trip); 2 new tests cover the lock primitive. `cargo test`: 100 pass; clippy clean; `cargo audit` clean. New runtime dependency: `fs4 = "0.13.1"` (advisory file locks).

## [0.1.3] - 2026-05-21

### Security

Fix five additional vulnerabilities surfaced by a comprehensive multi-angle audit (six parallel sub-agents, each covering one threat-model dimension: command injection, input parsing, FS safety 2nd pass + concurrency, output safety + agent-mode JSON, supply chain, logic / state-machine). These were independent of the firebaguette audit that motivated v0.1.2; together they close every CRITICAL and offensive HIGH finding identified by the audit.

- **`source_sha` argument injection in `git ls-tree`** (CRITICAL, four agents converged on this). `InstalledSkill.source_sha` deserialized from `.skills.toml` (committed, PR-mergeable) flowed unvalidated into `git ls-tree -r -z <refspec> -- <path>`. Because the refspec sits before `--`, an attacker who slipped a malicious `.skills.toml` into a PR could set `source_sha = "--name-only"` / `--abbrev=0` / `--output=…` and corrupt the diff classifier — which drives `pull`/`push` destructive decisions — or forge divergence state to trick `push --on-divergence overwrite` into clobbering the wrong content. `InstalledSkill::validate` now rejects any `source_sha` that isn't 40–64 hex characters (sha1 / sha256).
- **FIFO / device / socket DoS in `copy_dir_all`** (CRITICAL). The file-type branch only checked `is_dir()` / `is_symlink()`; a FIFO inside a skill folder fell through to `fs::copy`, which blocks indefinitely waiting for a writer. A character device like `/dev/zero` would read until OOM. Now `copy_dir_all` only allows regular files and directories; anything else (FIFO, socket, device) is rejected with `AppError::Config`.
- **`add --dest` arbitrary-directory wipe in agent mode** (HIGH). `--dest` accepted absolute paths and `..` traversal without validation, so `skillctl add --dest /Users/victim/.ssh --on-conflict overwrite --skill <maliciously-named>` would wipe arbitrary directories in one shot from any agent-driven invocation. Now `--dest` rejects `..` unconditionally, and rejects absolute paths when running in non-interactive / `--json` mode (where the operator may be an LLM running on attacker-supplied input). Interactive use is unchanged.
- **Commit-message trailer forgery via skill names** (HIGH). Skill names were spliced verbatim into `git commit -m "update skill: <name>"` and into the `commit.message` field of `--json` output. A library skill with a `\n` in its name (e.g. `foo\nCo-Authored-By: evil@x`) produced a forged trailer that downstream tooling (Linear, GitHub commit-bot, release-notes scrapers) would treat as real authorship metadata. The new `sanitize` module strict-validates every `name` / `tag` (identifier-class: no control bytes, no newlines, no ESC) and lenient-validates `description` / `--message` (allows `\n`/`\t`, rejects `\r` / DEL / C0+C1 controls). Skills with poisoned names are dropped silently from `discover` (a poisoned name can't be safely displayed either); poisoned tags or descriptions are stripped from otherwise-valid skills.
- **Hardlink exfiltration via the round-trip** (HIGH). `fs::symlink_metadata` reports a regular file for hardlinks (shared inode), and `fs::copy` reads the target content. An untrusted agent writing `<project>/my-skill/data` as a hardlink to `~/.ssh/id_rsa` would have shipped the SSH key content to the library on the next `skillctl push` or `detect`. `copy_dir_all` now checks `nlink() > 1` on regular files (Unix) and refuses to copy hardlinked content with the same fail-closed philosophy as symlinks.

Audit methodology and the full remaining backlog (10 MEDIUM + 18 LOW spread across atomicity, concurrency, output hardening, supply chain documentation) are tracked privately and will be addressed in 0.1.4 / 0.1.5. 23 new unit + integration tests cover each rejection class; `cargo test`: 95 pass; clippy clean; `cargo audit` clean.

## [0.1.2] - 2026-05-20

### Security

Fix four path-safety vulnerabilities that, in combination, allowed a malicious skills library or a crafted `.skills.toml` (e.g. mergeable via PR) to **exfiltrate** arbitrary files through the round-trip (read on `skillctl add`, leak on `skillctl push`) and to **delete arbitrary directories** outside the project or library root on `skillctl pull` / `push` / `detect`. Reported privately on 2026-05-19 by **firebaguette** via the Umanio Discord; all four issues are addressed in this release.

- **Symlink follow in `fs_util::copy_dir_all`.** A symlink inside a skill folder (e.g. `niania → /home/user/.aws/credentials`) bypassed `entry.file_type().is_dir()`, fell into the file branch, and was dereferenced by `fs::copy` — copying the symlink target into the project. A subsequent `skillctl push` would have published the secret to the (possibly public) library. Symlinks are now hard-rejected by `copy_dir_all` at both the top-level source and any descendant entry, and `replace_folder_contents` refuses a symlinked destination so `remove_dir_all` cannot be tricked.
- **Path traversal via `destination` and `source_path` in `.skills.toml`.** Both fields were deserialized as `PathBuf` with zero validation. Because `Path::join` lets an absolute right-hand side replace the base, a `.skills.toml` entry like `destination = "/home/seb/.ssh"` made `cwd.join(...)` resolve outside the project and `replace_folder_contents` → `remove_dir_all` wipe arbitrary directories. `..` traversal was equally unguarded. New `InstalledSkill::validate` runs at `project_config::load` time and rejects absolute paths, `..`, and Windows-prefix components for both fields; the same check is wired (defense-in-depth) at every destructive call site in `push.rs` / `pull.rs` via the new `path_safety::safe_join` helper.
- **`detect --target` accepted `..` even though it rejected absolute paths.** Validation in `commands::detect::resolve_target` now goes through the same `validate_relative_subpath` helper, rejecting any non-`Normal`/`CurDir` component. The interactive "custom path" prompt was tightened to match.
- **Fork-name validation accepted `.` and `..` literally.** `validate_fork_name` in both `push.rs` and `pull.rs` only rejected `/` and `\`, so a fork named `..` would have produced a `Path::join` resolving to the parent directory, then `fs::rename` could have clobbered it. `.` and `..` are now explicit rejections.

Threat-model note: the fix is purely lexical (component-level) plus an explicit symlink check at copy time. No filesystem `canonicalize` calls were added, avoiding TOCTOU windows and keeping the validation pure-functional (`AppError::Config`, exit code 2). 34 new unit tests cover each rejection class and each attack scenario end-to-end.

### Changed

- README and crate description reframed around "agent skills" terminology to reflect the multi-tool nature of the `SKILL.md` convention (Claude Code, Codex, Cursor, OpenCode, and others in the [open agent skills ecosystem](https://skills.sh)) — no behavior change.

## [0.1.1] - 2026-05-11

### Added

- Published on crates.io: `cargo install skillctl` now works.
- Pre-built binaries on GitHub Releases for macOS (x86_64, aarch64), Linux (x86_64, aarch64), and Windows (x86_64), built via [`cargo-dist`](https://github.com/astral-sh/dist).
- Homebrew tap at [`umanio-agency/homebrew-tap`](https://github.com/umanio-agency/homebrew-tap): `brew install umanio-agency/homebrew-tap/skillctl`.
- Shell + PowerShell `curl | sh`-style installers wired into the release workflow.

### Changed

- Crate renamed from `skills-cli` to `skillctl` to publish on crates.io (the `skills-cli` crate name was already taken by an unrelated package).
- GitHub repository renamed from `umanio-agency/skills-cli` to `umanio-agency/skillctl`. GitHub redirects from the old URL still work for inbound links.
- Companion skill folders moved: `.claude/skills/skills-cli-{project,usage}/` → `.claude/skills/skillctl-{project,usage}/`.
- Config and cache paths (`dev.umanio-agency.skills-cli`, `~/.config/skills-cli/`, `~/.cache/skills-cli/`) intentionally **kept** to avoid breaking existing local state for no user-facing gain.

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
