# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
