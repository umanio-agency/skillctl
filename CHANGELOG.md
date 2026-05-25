# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.6] - 2026-05-25

### Robustness & hygiene

Close the audit's Phase 8.4 "low-impact polish" batch: 9 of the 17 remaining LOW findings, plus a new "Trust model" section in SECURITY.md that documents the boundaries underlying all the v0.1.2 → v0.1.6 hardening work. The 8 deferred LOW items either need a new runtime dependency (homograph detection, Unicode normalization), require a release-workflow change (SLSA provenance), or interact with pre-v1 design questions (slug-collision uniqueness, fork-destination UX).

- **Force HTTPS in library URLs** (L1). `skillctl init http://github.com/owner/repo` was previously accepted and silently downgraded to cleartext for the initial clone. A network attacker on the operator's link could MITM the response and serve modified content. Now `slug_for_url` rejects `http://` with a clear "use HTTPS instead" message. SSH (`git@host:`, `ssh://`) is unchanged.
- **UTF-8 BOM stripped before frontmatter parse** (L2). Some editors (Notepad on Windows, occasionally VS Code) prepend a `\u{feff}` BOM to UTF-8 files. The frontmatter parser saw `\u{feff}---` instead of `---` and treated the whole SKILL.md as "no frontmatter." Now the parser strips a leading BOM before checking the opening fence.
- **Balanced quotes enforced in `clean_value`** (L4). `clean_value` was using `trim_matches(|c| c == '"' || c == '\'')` which silently stripped mismatched quotes — `"foo'` became `foo`. Mismatched quotes now pass through unchanged so the operator sees the malformed value and can fix it.
- **`git push` failure rolls back the just-created commit** (L7). When `git commit` succeeds but `git push` fails (network blip, auth expiry), the local commit sat orphaned in the cache, ahead of upstream. The next `fetch_and_fast_forward` would silently `reset --hard @{upstream}` it away — or, post-M10, refuse to refresh because the working tree happened to get dirty in between. New `git::reset_hard_to_parent` helper, wired into both `push` and `detect`, restores the cache to a clean state on push failure.
- **SKILL.md read capped at 1 MiB** (L8). `std::fs::read_to_string` for SKILL.md was unbounded — a 5 GiB file would be slurped silently into RAM during `discover`. New `read_skill_md_bounded` helper refuses to load more than 1 MiB and surfaces a per-skill warning instead.
- **Submodule recursion disabled** (L12 + L13). `git clone` now passes `--no-recurse-submodules` explicitly so a malicious library with a `.gitmodules` pointing at attacker-controlled repos cannot pull-through during `skillctl init`. The cargo-dist release workflow's `actions/checkout` steps switched from `submodules: recursive` to `submodules: false` (we have no submodules; this is defense-in-depth that survives the next `cargo dist init` regeneration). Skills do not use submodules; if a legitimate use case appears, it can be opt-in via an explicit flag later.
- **`add` continues on per-skill failure** (L15). The apply loop in `add` used `?` for `fs::remove_dir_all`, `copy_dir_all`, and the `source_path` strip-prefix — a single per-skill failure aborted the whole batch, and `.skills.toml` was only saved at the end, so partial successes were untracked. Now each skill is wrapped in an IIFE that logs a warning + continues on failure, and `project_config::save` always runs (capturing partial state). Same pattern as `pull` (v0.1.4) and `push` (v0.1.5).
- **`$HOME` rendered as `~/` in displayed paths** (L17). Absolute paths in error messages and JSON output (`library cache not found at /Users/<operator>/Library/Caches/...`) leaked the operator's Unix username into CI logs and agent-mode JSON. New `fs_util::display_path(&path)` swaps a leading `$HOME` with `~/` and is applied at every "library cache not found" / cache-path-display site.
- **`list`'s `eprintln!` routed through `ui::log_warning`** (L18). A single bare `eprintln!("warning: could not refresh library cache (...)")` in `list` bypassed the `--json` gating, polluting JSON consumers' stderr with non-JSON text. Now routed through the shared `ui::log_warning` helper, which is JSON-aware.
- **SECURITY.md trust-model section**. New section that explicitly names the three trust boundaries — Trusted (operator's machine, interactive flags, the binary itself), Semi-trusted (library URL and cache), Adversarial (frontmatter, `.skills.toml`, git working tree, non-interactive flag values) — plus an explicit Out-of-scope list (compromised git binary, side-channel attacks). External auditors and contributors can now know where to look without reverse-engineering the code.

11 new unit tests (1 HTTPS-required, 1 BOM strip, 4 balanced-quote, 2 SKILL.md size cap, 3 `$HOME` rendering). `cargo test`: 147 pass; clippy clean; `cargo audit` clean.

**Deferred to a future release** (with reasons, since v0.1.6 explicitly chose to keep the scope minimal):

- **L3** (homograph warning, e.g. Cyrillic `а` vs Latin `a` in skill names). Needs a `unicode-confusables` (or similar) dep; warrants its own decision before adding a runtime crate.
- **L5** (NFC normalisation of paths/names). Needs `unicode-normalization`; same reasoning.
- **L6** (case-insensitive FS collision warning on APFS-CI). No new dep but ~30 lines of runtime logic; deferred to a UX-focused release.
- **L9** (Cargo.toml caret-semantics doc). Documentation-only; will land alongside a broader contributor-docs pass.
- **L10** (SLSA provenance / cosign attestations on release binaries). Release-workflow change; deserves its own PR + dry-run on a tag.
- **L11** (cache-slug collision via hash suffix). Pre-v1 with one-library-at-a-time, slug collisions are theoretical only; revisit if multi-library support lands.
- **L14** (prompt operator on fork destination instead of inheriting the source's parent). UX question best decided alongside a broader `fork` flow review.

## [0.1.5] - 2026-05-22

### Security & robustness

Close the comprehensive audit's Phase 8.3: 13 MEDIUM findings plus the deferred push-side half of H8. No item here is single-shot exploitable, but each closes a credibility-eroding leak (credentials in logs), DoS vector (unbounded parsers, recursive walkers), or footgun (silently-discarded state, hook execution via shared cache).

- **Credentials stripped from stored `library.url`** (M1). `skillctl init https://x-access-token:<PAT>@github.com/...` would store the full URL — token and all — in `config.toml`, then echo it back in JSON output, error chains, and CI logs. `init` now sanitises the URL (strips `user[:password]@` from `https://`/`http://` authority sections) before persisting; the one-time `git clone` still uses the original URL for authentication, but the token never lands on disk or in any later command's output. SSH forms (`git@host:...`, `ssh://git@host/...`) are unchanged.
- **Git stderr scrubbed in every error chain** (M3). Each `git`-shell-out site used `String::from_utf8_lossy(&stderr).trim()` — which would faithfully echo credential-helper banners, proxy URLs containing PATs, ANSI control sequences, and stack traces past the first line. The new `git::scrub_stderr` helper takes the first non-empty line, strips C0/C1/DEL/ESC control bytes, and redacts known token prefixes (`ghp_*`, `gho_*`, `ghs_*`, `ghu_*`, `github_pat_*`, `x-access-token:*`) to `<prefix>***`. Applied uniformly across every git invocation.
- **`core.hooksPath` neutralised on every git call** (M12). The library cache is a git repo whose `.git/config` is reachable from inside skill content. A malicious library that dropped a script at the operator's globally-configured `core.hooksPath` would have it executed by any `git commit` in the cache. Every `Command::new("git")` now goes through a `git_cmd()` helper that prepends `-c core.hooksPath=/dev/null`, so hook execution is impossible regardless of global or in-cache git config.
- **`git status --porcelain` check before `reset --hard @{upstream}`** (M10). `fetch_and_fast_forward` used to unconditionally `git reset --hard @{upstream}`, silently destroying any uncommitted state left over from a previous skillctl run that crashed mid-commit (e.g. `replace_folder_contents` succeeded but `git push` failed). Now refuses to refresh when the cache reports any porcelain output, surfacing a clear "uncommitted changes — inspect with `git -C <cache> status`" message so the operator can investigate before any destruction happens.
- **Frontmatter parser bounded at 200 lines** (M4). A SKILL.md with an opening `---` but no closing fence would force the parser to scan the entire (potentially multi-GiB) body — a cheap DoS reachable on every `skill::discover` call. Capped to `MAX_FRONTMATTER_LINES = 200`; unterminated frontmatter is now treated as "no frontmatter" (the skill is dropped from discovery rather than half-parsed).
- **`validate_fork_name` rejects control characters and caps length** (M5). The previous fork-name validator only rejected empty / `.` / `..` / path separators — a name like `foo\0bar` would panic inside `CString::new` when later passed to `Command`. Now rejects any control char (NUL, ESC, ANSI, DEL, newline, CR, tab) and caps at 64 bytes. Consolidated as `sanitize::validate_fork_name` (was duplicated between `push.rs` and `pull.rs`).
- **`.skills.toml` rejects unknown fields, duplicates, and overflow** (M6). Added `#[serde(deny_unknown_fields)]` on `ProjectConfig` and `InstalledSkill`, so a malicious PR can no longer smuggle unknown keys (which might later be load-bearing for an unreleased feature) into the deserialiser. Duplicate `name` or `destination` entries are rejected at load — silent dedup would make every command ambiguous about which entry wins. Capped at 256 entries to bound the diff-classifier work.
- **`copy_dir_all` is iterative and masks mode bits** (M7 + M8). Converted from recursion to an explicit `Vec<(PathBuf, PathBuf)>` work stack, so an adversarial skill with 10k-deep nesting can no longer blow Rust's default 8 MiB thread stack. On Unix, copied file modes are now masked to `0o644 | (src_mode & 0o100)` — only the user-execute bit propagates; setuid, setgid, sticky, group-write, world-write, group-execute and world-execute are stripped. A library that drop-ins a setuid binary cannot weaponise the round-trip into elevated privileges on the destination.
- **`detect` dedup unions canonical AND lexical comparison** (M9). The "already installed" set was built from `fs::canonicalize` only — silently dropping entries whose destination had been deleted from disk. An attacker who removed `.claude/skills/foo/` and dropped a replacement at the same path would have it re-detected as a new skill on the next `detect`. Now compares by canonical path (when both ends exist) AND lexical path (covers the deleted-destination case via the new `path_safety::normalize_lexical` helper).
- **`detect` walker respects `.gitignore` and skips vendor dirs by default** (M11). A malicious npm package shipping its own `SKILL.md` under `node_modules/...` could be picked up by `skillctl detect --all` running in CI and uploaded to the library. `skill::discover` now takes an `include_vendored` parameter; the default (false) leans on `ignore::WalkBuilder`'s `.gitignore`/`.ignore` respect plus a hard-skip on `node_modules`/`target`. New CLI flag `skillctl detect --include-vendored` for the explicit opt-in.
- **Homebrew tap typo-squat documented** (M13). Both README and SECURITY.md now prominently call out the canonical fully-qualified install (`brew install umanio-agency/homebrew-tap/skillctl`) and explain that anyone can ship a `skillctl.rb` formula under their own `homebrew-tap` repo. Pinning the owner avoids the typo-squat risk.
- **`push --all` continues on per-skill failure** (H8 push-side). The pre-v0.1.5 apply loop used `?` inside the per-skill body, so one failing skill aborted the entire batch and orphaned the cache's working tree for the successful early skills (commit + push never happened, cache stayed dirty until the next `fetch_and_fast_forward` reset it). Now each apply is wrapped in an IIFE: on per-skill failure, the change is rolled back with `git checkout HEAD -- <library_relative>`, a warning is logged, and the loop continues. If all skills fail, the command exits cleanly with "nothing pushed". This closes the half of H8 deferred from v0.1.4.

13 new unit tests added (3 path_safety lexical normalisation, 3 sanitize fork-name hardening, 4 `.skills.toml` deny/dedup/cap, 3 discover gitignore/node_modules/include-vendored, 2 frontmatter bound, 7 git stderr scrub, 3 fs_util mode-mask + deep nesting). `cargo test`: 136 pass; clippy clean; `cargo audit` clean.

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
