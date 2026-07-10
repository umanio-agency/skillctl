# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.5.0] - 2026-07-10

A feature release on top of v0.4.0's multi-library model. Everything below is backward-compatible new surface — every existing workflow is unchanged — so a minor bump.

### Added

- **`skillctl propagate <skill>…` and `push --propagate`** — fan a library's current version of a skill out to every *other* project on disk that installed it ("fix once, updated everywhere"). Install sites are discovered by **scanning for `.skills.toml`** (crate `ignore`: prunes `node_modules`/`target`/`.git`, honours `.gitignore`) — no central registry to maintain, so it self-heals when a project is moved, cloned, or freshly added. For each matching site it replays `pull`'s classification: a site cleanly behind the library is **updated** (folder replaced + `source_sha` rewritten), a site with **local edits is skipped and reported** (never clobbered), an up-to-date site is a noop, and a site from another library is untouched. `--dry-run` previews. **`push --propagate`** runs the same fan-out in one step right after a successful push — only round-trip *updates* propagate (forks and `--to` promotions don't), and the project you pushed from is skipped (already at HEAD). Scan roots come from `--root <path>` (repeatable) or the new **`[propagate] roots`** section in `config.toml` (omitted from the file while empty).
- **`skillctl create <name>`** — scaffold a new skill folder with a template `SKILL.md` (frontmatter `name`/`description`/optional inline `tags` + a body skeleton) that round-trips through skillctl's own parser, so the skill is immediately visible to `detect`/`add`/`audit`. Project-local (no library, git, or network); refuses to overwrite an existing folder (exit 3). Interactive location picker, or `--dest` non-interactively; `--description` / `--tag` optional.
- **Content audit now gates `pull` and `detect`.** The Phase-10C audit engine — previously run only on `add` / remote install — now also scans the **incoming** library version on `pull` (before it overwrites local content) and **local** content on `detect` (before it's published to a possibly shared library). Warn-only by default; `--fail-on <severity>` refuses the whole batch atomically (exit 5, nothing written); `--no-audit` skips. `audit_verdict` per skill in `--json`.
- **Interactive batch-triage for flagged content.** When an interactive warn-only run flags one or more skills (verdict ≥ `warning`), `add` / `pull` / `detect` present a menu before applying: *Decide for each* (include / skip / view findings per skill), *Proceed with all*, or *Cancel everything* (nothing applied, exit 0). Non-flagged skills always proceed; `--fail-on` / `--no-audit` / non-interactive suppress the menu, so scripted and `--json` output is byte-identical.
- **Tag meta-actions in the interactive picker.** Typing a query that matches a tag surfaces two actionable rows above the skill matches — `▸ tag:<name> — filter to N` (enter a tag-filter mode) and `▸ tag:<name> — select all N` (check every carrier) — the interactive equivalent of `--tag` / `--all-tags`, in `add` / `pull` / `detect` / `push`.

### Security

- Inbound library content is now audited **before it lands**: `pull` scans the exact bytes it's about to write (the same `safe_join` runs in the audit pre-pass and the write loop, so no skill can bypass the scan), and `--no-audit` is refused on `pull` for non-default (third-party) provenance. `propagate` reuses `pull`'s hardened apply path — provenance matching **fail-closes** on an unparseable `library_url`, `safe_join` guards both the library read and the project write, and the site walker never follows symlinks.
- Dependency remediation ahead of the release: `crossbeam-epoch` 0.9.18 → 0.9.20 (RUSTSEC-2026-0204, transitive via `ignore`), `anyhow` 1.0.102 → 1.0.103 (RUSTSEC-2026-0190 unsoundness advisory).

Each addition went through the project's per-feature Phase-9 scoped security self-review (0 defects), plus end-to-end sandbox drivers exercising the real binary. `cargo test`: 264 pass; clippy clean; `cargo audit` clean.

## [0.4.0] - 2026-06-16

Two additions on top of the v0.3.0 multi-remote model. Both are backward-compatible (new surface only).

### Added

- **Ad-hoc install from a remote source** — `skillctl add --from <url>` installs skills directly from a git repo that isn't a configured library, where `<url>` is a full URL or a `github:owner/repo` / `gitlab:owner/repo` shorthand. It's the one-shot "try a skill from that repo" path, complementing the persistent `library add` + `--from <name>` flow. A configured library name always wins (so this never shadows one), and a URL that matches an existing library is treated as an alias for it. The source is cloned into the cache and its content is **always audited** (third-party by definition — `--no-audit` is refused). By default the install is **ephemeral** — recorded in `.skills.toml` by URL but not added to `config.toml`, so `pull`/`push` skip it. Pass `--save-as <name>`, or accept the interactive "keep as a library?" offer, to register it as a `read`-access library that `pull` can track afterward. (A curated public registry and `#ref` pinning are not included.)
- **`skillctl tag add <tag>… --skill <name>`** / **`skillctl tag remove …`** — add or remove tags on a project skill's `SKILL.md` frontmatter from the CLI, instead of hand-editing YAML. Project-local (no git or network, like `remove`): it rewrites the `tags:` field in place — an existing inline (`tags: [a, b]`) or block form becomes a canonical `tags: [a, b, c]`, one is inserted if absent, and removing the last tag drops the field — while preserving every other byte of the file (other frontmatter keys, the body, BOM, and line endings). The write is atomic. Because tags are read from the local `SKILL.md`, a retag takes effect immediately and propagates to the library on the next `push`.

### Security

- A remote `add --from <url>` install can never skip the content audit (`--no-audit` is refused, and the audit also runs unconditionally on the install path). Inline credentials in a `--from` URL are kept out of the stored provenance, logs, and JSON (only a sanitised display URL is recorded).
- `skillctl tag` values are restricted to simple tokens (no `,` `[` `]` `"` `'` or non-space whitespace, including Unicode line/paragraph separators) so they can't break the frontmatter or smuggle terminal escapes. The frontmatter rewrite only ever touches lines inside the `---` fences, replaces a symlinked `SKILL.md` rather than following it, and is written atomically.

Both additions went through the project's standard pre-release security-audit pass; findings (all LOW) were fixed or accepted. `cargo test`: 246 pass; clippy clean.

## [0.3.0] - 2026-06-12

The multi-remote model (Phase 10): manage **several** skill libraries, each with an access level, across GitHub, GitLab, and self-hosted git, with the full round-trip — install, pull, push, promote, and open a PR/MR. A single configured library keeps working with **zero new flags**; everything below is opt-in once you add a second library. Shipped as six independently audited steps (10A–10F).

### Added

- **Multiple libraries.** `config.toml` now holds an array of libraries, each with a `name`, `url`, and an **access level** — `read` (consume only), `write` (commit directly), or `pr` (push a branch and open a PR/MR). New `skillctl library add <name> <url> [--access …] [--default]`, `library list`, `library remove`, and `library set-default`. `skillctl init <url>` is now sugar for "add the first library and mark it default". Exactly one library is the default; added libraries default to `--access read` so you can't push to a third-party source by accident.
- **Any git host, over HTTPS or SSH.** Library URLs accept GitHub, GitLab, and self-hosted instances in `https://host/owner/repo`, `git@host:owner/repo`, and `ssh://git@host[:port]/owner/repo` forms (no host allowlist — it's your own config). The cache directory name is now host-aware and collision-free.
- **Consume from many** (`add` / `list`). `--from <name>` reads from a named library; `--from all` spans every configured library (source shown per skill, with a nested `--json` shape). The interactive `add` picker grows a **tab per library** (←/→ to switch) when more than one is configured; selections accumulate across tabs into one install. On a name/destination clash across libraries, the later skill is suffixed `-<library>` so both land distinctly.
- **Content security audit.** New `skillctl audit [--skill|--all] [--fail-on <severity>]` scans a skill's `SKILL.md` and bundled files for dangerous patterns (credentials, obfuscation, risky shell, dynamic code, prompt-injection) and reports a verdict. `add` runs it automatically (warn-only by default; `--fail-on <severity>` blocks; `--no-audit` skips). New exit code `5` for an audit threshold breach.
- **Provenance-following `pull` and `push`.** Each installed skill records which library it came from; `pull` refreshes it from **that** library and `push` writes it back **there** (a run can touch several libraries — one commit per library). A skill whose source library is no longer configured is listed and skipped.
- **`detect --to <name>`.** Choose which writable library new local skills are added to — the sole writable library by default, a `--to` name, or an interactive pick when several are configured.
- **`push --to <name>` — promotion.** Publish the selected skills into a writable library regardless of where they came from (and rewrite their provenance to it) — the way to contribute a skill installed from a read-only source into your own or a team library. On a path collision in the target, the existing `--on-divergence` vocabulary applies: `overwrite`, `fork` (add as a new skill), or `skip` (the default — never clobbers without consent).
- **`push` to a `pr`-access library opens a PR/MR.** Instead of committing to the default branch, push a `skillctl/<slug>` branch and open a pull request (`gh`) or merge request (`glab`); the URL surfaces in the outro and JSON (`pr_url`). New `--pr-title` and `--yes`; interactive runs show an editable title and a confirm before anything is pushed. No host token is stored — it reuses your existing `gh`/`glab` auth; unsupported hosts get a clear "branch pushed, open it manually" message.

### Changed

- **`config.toml` migrates automatically** from the legacy single-`[library]` form to the new `[[library]]` array, in memory on read and persisted on the next config-writing command — no surprise rewrites. Each `.skills.toml` entry gains optional `library` + `library_url` provenance fields (absent ⇒ the default library, so old manifests keep working).
- **One-time cache re-clone.** Because the cache directory name became host-aware, existing single-library users get a single re-clone on the first command after upgrading (the cache is disposable). `init` / `library add` recreate it.

### Security

- **Cross-library installs are untrusted by default.** Installing from any non-default (third-party) library makes the content audit **mandatory** — `--no-audit` is refused there (still warn-only unless you add `--fail-on`). Your own default/primary library is unaffected.
- **Provenance routing is fail-closed.** Push/pull match a skill to its library by normalized URL; a `.skills.toml` whose `library_url` is present but unparseable resolves to *no* library (and is rejected at load) rather than falling back to the rename-able name alias — so a crafted manifest can't route a foreign skill onto the wrong cache.
- **One repository per library.** Two configured libraries pointing at the same repo (e.g. a `read` and a `write` spelling of one URL) are rejected at load and at `library add`, closing an access-gate bypass where a skill's write target would otherwise depend on config order.
- **Hardened git/PR surface.** The git transport allowlist is pinned to https+ssh (alternate transports like `ext::`/`file://` can never run); `--message`/`--pr-title` reject control characters (no forged commit trailers) before any commit; `gh`/`glab` are invoked with argv (no shell), and their output is scrubbed of credential tokens.

Every step ran the project's standard pre-release security-audit pass; findings (1 HIGH access-gate bypass, several MEDIUM/LOW) were fixed before each commit. `cargo test`: 237 pass; clippy clean.

## [0.2.0] - 2026-05-27

### Added

- **`skillctl remove`** — remove skills from the current project. Lists every removable skill (installed via skillctl, created locally, or an orphaned `.skills.toml` entry whose folder is already gone), with each kind distinguished in the selection list, and lets you pick by interactive multi-select, by name (`--skill <name>`, repeatable), or all at once (`--all`). Deletes the selected skill folders and drops their `.skills.toml` entries; `.skills.toml` is only rewritten when a tracked entry actually changes. The command is project-only — it never touches the library or git.

### Security

- The removal path refuses to follow a symlink: the destination's type is re-checked immediately before deletion, so a folder swapped for a symlink cannot redirect the recursive delete outside the project (closes a TOCTOU window surfaced by the pre-release audit). A symlinked destination is treated as "no folder on disk" — only its manifest entry can be dropped, never deleted through.
- `--skill <name>` fails closed: an unknown name errors, and a name shared by two skills errors as ambiguous rather than silently deleting the wrong folder. The project root, `.git`, and `.skills.toml` can never be selected for deletion.

`skillctl remove` was reviewed before release with the project's standard multi-agent security audit pass (FS-safety / untrusted-input / logic dimensions). 5 new unit tests; `cargo test`: 158 pass; clippy clean; `cargo audit` clean.

## [0.1.8] - 2026-05-25

### Security & robustness

Close Phase 9.1 — five of the eight LOW findings that were explicitly deferred at v0.1.6. The remaining three (L11 cache-slug uniqueness, L14 fork destination prompt, plus the not-yet-numbered "deferred-with-reason" items from §10 of the internal audit) are gated on broader UX or migration decisions that warrant their own designs.

- **APFS case-insensitive collision warning** (L6). Two skills named `Foo` and `foo` are distinct under skillctl's identifier-class validation (case-significant) but collapse to the same path on case-insensitive filesystems (APFS-CI on macOS external drives, HFS+, NTFS), so a subsequent `add` would silently clobber one with the other. `skill::discover` now groups skills by their lowercased name and surfaces a warning per collision group. Doesn't reject — case-insensitivity is host-dependent and operators on case-sensitive ext4/APFS-CS are fine — but lets the operator notice before something disappears.
- **Homograph / mixed-script name warning** (L3). New `unicode-script` dependency (~20 KB). `skill::discover` checks each accepted skill name for characters spanning two or more distinct Unicode scripts (ignoring `Common` / `Inherited` / `Unknown` — digits, punctuation, emoji are exempt). A name like `clаude` (where the `а` is U+0430 Cyrillic, not U+0061 Latin) raises a warning so the operator can spot a homograph attack from a malicious library that publishes a skill visually indistinguishable from a legitimate one.
- **NFC normalisation of paths in dedup** (L5). New `unicode-normalization` dependency (~100 KB). `path_safety::normalize_lexical` now Unicode-NFC-normalises every UTF-8 path component before returning. macOS HFS+ stores filenames in NFD (decomposed: `é` = `e` + combining acute) while Linux stores NFC (one codepoint); without this, the lexical dedup in `detect` and the `safe_join` comparisons treat the same logical filename as two distinct paths when the project crosses platforms. Non-UTF-8 components pass through unchanged.
- **`skill::discover` now returns warnings instead of `eprintln!`** (infrastructure fix). The oversize-SKILL.md warning added in v0.1.6 used `eprintln!`, which bypasses `--json` gating. `discover` now returns a `DiscoverOutput { skills, warnings }`; callers in `add` / `list` / `detect` route each warning through `ui::log_warning`, which silently no-ops in `--json` mode. Closes a latent v0.1.6 footgun and makes L3 + L6 warnings JSON-safe at the same time.
- **`actions/attest-build-provenance@v3` on every release** (L10). New `.github/workflows/attest.yml` triggers on the GitHub Release `published` event, downloads every binary asset, and generates a [SLSA build-provenance attestation](https://slsa.dev/spec/v1.0/levels) signed by GitHub Actions and recorded in Sigstore's transparency log. Users can verify a binary they downloaded with `gh attestation verify <file> --repo umanio-agency/skillctl` — a mismatch or missing attestation means the artifact wasn't produced by this repo's release workflow. Standalone workflow (not threaded into cargo-dist's generated `release.yml`) so it survives `cargo dist init` regenerations.
- **Dependency policy documented** (L9). `CONTRIBUTING.md` gains a "Dependency policy" section spelling out the caret-semantics convention, the `Cargo.lock`-as-pinning trust path, `cargo audit` as a release gate, the no-auto-update transitive policy, and the response plan if a dep ships a semver-incorrect break.

12 new unit tests (4 case-collision/homograph in `skill::tests`, 5 mixed-script unit cases, 1 NFC equality in `path_safety::tests`, 2 collision-suppression). `cargo test`: 153 pass; clippy clean; `cargo audit` clean.

**Deferred to a future release** (with reasons):

- **L11** (cache-slug uniqueness via hash suffix). Pre-v1 with one-library-at-a-time the slug collision is theoretical; revisit if/when multi-library support lands or two upstream `<owner>` namespaces produce a real conflict on the same dev machine.
- **L14** (prompt operator on fork destination instead of inheriting source parent). UX question worth a dedicated `fork` flow review rather than a one-line nudge.

## [0.1.7] - 2026-05-25

### Release engineering

Re-ship v0.1.6's content with the release pipeline unblocked. v0.1.6 was tagged but its cargo-dist `plan` job failed because the manual `submodules: recursive → false` edit (Phase 8.4 L12) in `.github/workflows/release.yml` made `dist host --steps=create` refuse to run ("out of date contents and needs to be regenerated"). v0.1.6 is on crates.io but has no GitHub Release artifacts and no Homebrew tap update; v0.1.7 fixes that.

- **`allow-dirty = ["ci"]` in `dist-workspace.toml`.** cargo-dist exposes an explicit allow-list to tolerate manual edits in the workflow it generates. Adding `ci` to it preserves the L12 defense-in-depth (no recursive submodule pull from the release runner) while letting `dist host` proceed.

No code or test changes from v0.1.6.

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
