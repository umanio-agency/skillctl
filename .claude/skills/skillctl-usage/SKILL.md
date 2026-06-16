---
name: skillctl-usage
description: How to drive the `skillctl` CLI non-interactively. Load PROACTIVELY when the user asks to install, push, or contribute Claude skills, or mentions a "skills library" / "skills repo". Covers every command's flag surface, exit codes, and end-to-end recipes so an agent can run `skillctl` without a TTY.
tags: [meta, agent-tooling]
---

# skillctl-usage

`skillctl` is a Rust CLI that manages a personal Claude skills library across projects. This skill is the agent-facing reference: it documents how to drive every command **without prompts**, so any agent (Claude Code or otherwise) can use it as a tool.

> If you're a human reading this, the same flags work in interactive mode — they pre-fill choices and skip the relevant prompts.

## How non-interactive mode is selected

`skillctl` auto-detects whether stdin and stdout are TTYs. When called from a script, agent, or pipe, it switches to non-interactive mode automatically. You can also force it explicitly with the global `--no-interaction` flag, or with `--json` (which implies it).

In non-interactive mode, **every decision must come from a flag.** If a required input is missing, the command exits with a clear error rather than silently falling back to a prompt.

## Structured output: `--json`

The global `--json` flag suppresses the human-readable cliclack output and emits a single JSON object on stdout at the end of the command. `--json` implies `--no-interaction`. Errors continue to go to stderr.

Per-command top-level shape:

```jsonc
// init
{"command":"init","library":{"url":"…","cache_path":"…"}}

// list
{"command":"list","library":"…","skills":[{"name":"…","path":"…","description":"…|null","tags":["…"]}]}

// add
{"command":"add","destination":"…|null","results":[{"name":"…","status":"installed|skipped|aborted","…":"…"}],"summary":{"installed":N,"skipped":N,"aborted":N}}

// push
{"command":"push","results":[{"name":"…","status":"pushed|forked|promoted|pr_opened|skipped","operation":"update|fork|pr","pr_url":"…","branch":"…","new_name":"…","library":"…","…":"…"}],"commit":{"sha":"…","message":"…"}|null,"summary":{"pushed":N,"forked":N,"promoted":N,"pr_opened":N,"skipped":N}}

// pull
{"command":"pull","results":[{"name":"…","status":"pulled|skipped","fork_local":"…|null","fork_local_path":"…","source_sha":"…"}],"summary":{"pulled":N,"forked_locally":N,"skipped":N}}

// detect
{"command":"detect","target":"…|null","results":[{"name":"…","status":"added|skipped","library_path":"…","local_path":"…","source_sha":"…"}],"commit":{"sha":"…","message":"…"}|null,"summary":{"added":N,"skipped":N}}

// remove
{"command":"remove","results":[{"name":"…","status":"removed|failed","path":"…","removed_folder":true|false,"removed_entry":true|false,"reason":"…"}],"summary":{"removed":N,"failed":N}}
```

Stable rules:
- The `command` field always matches the subcommand name.
- `results[]` only contains skills that were *acted on* (installed, pushed, forked, pulled, added, or explicitly skipped). Skills that were silently no-ops (e.g. unchanged on `push`) are not in `results[]`. To enumerate everything, use `skillctl list --json`.
- `summary` totals exactly equal the count of corresponding `status` values in `results[]`.
- `commit` is `null` when no commit was made (nothing to apply, or library-only-read commands like `pull`/`list`).

## One-time setup: link a library

```sh
skillctl init https://github.com/<owner>/<repo>
```

- Clones the library repo into a platform-appropriate cache.
- Persists the URL in a global config file as the **default library** (named `personal`, access `write`).
- Re-running `init` against the same URL refreshes the cache; against a different URL re-points the default library.
- Any git host works — GitHub, GitLab, or self-hosted — over HTTPS or SSH (e.g. `git@host:owner/repo.git`). Cleartext `http://` is refused.

## Multiple libraries

skillctl can track more than one library. A **library** carries an access level — `read` (consume only), `write` (direct commit), or `pr` (branch + PR/MR) — and exactly one is the **default**. The personal library from `init` is just the default.

```sh
skillctl library add <name> <url> [--access read|write|pr] [--default]
skillctl library list
skillctl library remove <name>
skillctl library set-default <name>
```

- `library add` clones the repo immediately (fail-fast on a bad URL/credentials). Added libraries default to `--access read` so you can't push to them by accident. The name `all` is reserved.
- The default library is what every command acts on when you don't say otherwise. `--from <name>` (on `list`/`add`) reads from another library; non-default reads are treated as **untrusted third-party content** (see the audit notes below).
- `pull` and `push` both **follow each skill's provenance** — a skill installed from any configured library is refreshed from / written back to that library (a run may touch several). A skill whose recorded provenance is no longer configured is listed and skipped (run `skillctl library add` to restore it).
- `push` to a `write`-access library commits directly; to a `pr`-access library it pushes a `skillctl/<slug>` branch and opens a PR (`gh`) or MR (`glab`), returning the URL. A skill from a `read`-access source can't be written back — **promote it** into a writable library with `push --to <lib>` (see the push section). Opening a PR/MR uses your existing `gh`/`glab` auth — no token is stored; unsupported hosts get a "push done, open it manually" message.
- Each repository may be configured at most once: `skillctl library add` refuses a URL that resolves to an already-configured repo (same repo under two access levels would make a skill's write target depend on config order).
- **Interactive only:** running `skillctl add` in a terminal with more than one library configured (or `skillctl add --from all`) opens a picker with a **tab per library** (←/→ to switch, opens on the default); selections accumulate across tabs into one install. Agents/non-interactive runs use the flags above instead.

## Commands

### `skillctl list` — read-only

```sh
skillctl list
skillctl list --from <name>          # list a specific library
skillctl list --from all             # list every configured library
skillctl list --tag <tag> [--tag <tag> …] [--all-tags]
```

Refreshes the library cache (best-effort `git fetch`) and prints every skill with its name, any frontmatter tags in `[…]`, and a one-line description.

| Flag | Purpose |
|---|---|
| `--from <name>` | List a named library instead of the default. `--from all` spans every configured library, grouped by library (each section shows the library name, access, and URL). |
| `--tag <tag>` | Filter to skills carrying this tag. Repeatable; default semantics is union (any of the given tags). |
| `--all-tags` | Switch to intersection (skill must carry every requested tag). Requires `--tag`. |

Under `--json`, a single-library list emits `{ "command": "list", "library": <url>, "skills": [...] }`; `--from all` emits `{ "command": "list", "from": "all", "libraries": [ { "name", "url", "access", "default", "skills": [...] } ] }`.

### `skillctl add` — install skills from the library into a project

```sh
skillctl add --skill <name> [--skill <name> …] --dest <path>
skillctl add --all --dest <path>
skillctl add --tag <tag> [--tag <tag> …] [--all-tags] --dest <path>
skillctl add --from <name> --skill <name> --dest <path>   # install from one named library
skillctl add --from all --tag <tag> --dest <path>         # install matching skills from every library
```

| Flag | Purpose | Required in non-interactive |
|---|---|---|
| `--from <name\|url>` | Install from a named library instead of the default. **A git URL or `github:owner/repo` / `gitlab:owner/repo` shorthand** installs ad-hoc from a remote source that isn't a configured library (see below). `--from all` installs matching skills from **every** configured library in one run (non-interactive: requires a selection — `--all`/`--skill`/`--tag`). Installing from any non-default source forces the content audit on. | No |
| `--save-as <name>` | Only with an ad-hoc `--from <url>`: also register the source as a `read`-access library under this name, so `skillctl pull` can track it later. Ignored when `--from` names an already-configured library. | No |
| `--skill <name>` | Install a specific skill (repeatable). Mutually exclusive with `--all` and `--tag`. | Yes, unless `--all` or `--tag` |
| `--all` | Install every skill in the library. Mutually exclusive with `--skill` and `--tag`. | Yes, unless `--skill` or `--tag` |
| `--tag <tag>` | Install every skill carrying this tag (repeatable). Default semantics is union (any of the given tags). Mutually exclusive with `--skill` and `--all`. | Yes, unless `--skill` or `--all` |
| `--all-tags` | Switch tag matching from union to intersection (skill must carry every requested tag). Requires `--tag`. | No |
| `--dest <path>` | Project-relative destination folder (e.g. `.claude/skills`). The folder is created if missing. | **Yes** |
| `--on-conflict <overwrite\|skip\|abort>` | Strategy when a destination skill folder already exists. | Yes if any conflict is encountered |
| `--no-audit` | Skip the content security audit of skills before installing. | No |
| `--fail-on <info\|warning\|critical>` | Refuse the **whole batch** (install nothing, exit 5) if any selected skill's content audit reaches this severity. Without it the audit is warn-only. | No |

Before anything is copied, `add` runs a content security audit (see `skillctl audit`) on each selected skill. By default it is **warn-only** (findings are logged, the install proceeds); under `--json` each installed skill's result carries an `"audit_verdict"` field (`safe`/`caution`/`warning`/`dangerous`) so a non-interactive caller still sees the signal. Pass `--fail-on <severity>` to block, or `--no-audit` to skip the scan entirely.

When installing from a **non-default library** (`--from <name>` where `<name>` isn't the default, or `--from all` while any non-default library is configured), the content is untrusted third-party material, so the audit is **mandatory**: `--no-audit` is refused (exit 2). It is still warn-only unless you add `--fail-on`. Installs from the default library are unaffected.

**Ad-hoc remote install** (`--from <url>`): `skillctl add --from github:owner/repo --skill foo --dest .claude/skills` clones the repo into the cache, audits its content (**mandatory** — `--no-audit` refused), and installs the selected skills with their remote-URL provenance. By default the source stays **ephemeral** — recorded in `.skills.toml` by URL but not added to `config.toml`, so `pull`/`push` skip it (it's a one-shot install). To keep tracking it, pass `--save-as <name>` (or accept the interactive "keep as a library?" offer) and it's registered as a `read` library. If the URL already matches a configured library, `--from <url>` simply installs from that library. Accepts the `github:`/`gitlab:` shorthand and full `https://`/`git@`/`ssh://` URLs; installs from the default branch HEAD.

With `--from all`, the default library is installed first and keeps the skill's bare name; if another library offers a skill whose name (or destination folder) is already taken, that install is suffixed `-<library>` (e.g. `deploy` from `personal` + `deploy-team` from `team`), so both land with distinct names, folders, and provenance. The JSON `results[]` entries carry a `library` field naming the source.

Each installed skill is recorded in `.skills.toml` at the project root with the source path inside the library, the library commit SHA at install time, the local destination, an RFC3339 timestamp, and the provenance (`library` name + `library_url`) it was installed from.

Tags are read from the `SKILL.md` frontmatter. Both inline and block forms work:

```yaml
---
name: claude-api
description: Build and tune Claude API apps with prompt caching.
tags: [api, claude, caching]
---
```

```yaml
---
name: claude-api
tags:
  - api
  - claude
  - caching
---
```

A bare scalar `tags: foo` is accepted and treated as a single-tag list. Tag flags on `push` and `pull` read tags from each skill's **local** SKILL.md (the user's current view), so retagging locally takes effect on the next run without needing to push or pull first.

`description:` accepts both a single-line value and YAML block scalars:

```yaml
description: |
  Multi-line literal description.
  Newlines are preserved.

description: >
  Multi-line folded description that
  joins lines with spaces, useful for
  one long sentence wrapped in source.
```

### `skillctl push` — propagate local edits back to the library

```sh
skillctl push --skill <name> [--skill <name> …]
skillctl push --all
skillctl push --tag <tag> [--tag <tag> …] [--all-tags]
```

| Flag | Purpose |
|---|---|
| `--to <name>` | **Promotion mode.** Publish the selected skills into this writable library (rewriting their provenance) instead of pushing each back to its own. Use it to contribute a skill installed from a `read`-only source into your own/team library. On a path collision in the target, `--on-divergence` applies. |
| `--skill <name>` | Push only specific skills by name (repeatable). Mutually exclusive with `--all`/`--tag`. |
| `--all` | Push every skill that has pushable changes. Mutually exclusive with `--skill`/`--tag`. |
| `--tag <tag>` | Push every pushable skill whose **local** SKILL.md carries this tag. Repeatable; default semantics is union (any of). Mutually exclusive with `--skill`/`--all`. |
| `--all-tags` | Switch tag matching to intersection (skill must carry every requested tag). Requires `--tag`. |
| `--on-divergence <overwrite\|skip\|fork>` | Strategy for divergent (and library-missing) skills. Default when omitted: skip with a warning. |
| `--fork-suffix <suffix>` | Required when `--on-divergence fork` is used non-interactively. New name = `<original>-<suffix>`. |
| `--message <text>` | Override the auto-generated commit message. For a `pr`-access library it is also the PR/MR description. |
| `--pr-title <title>` | Title for the PR/MR opened against a `pr`-access library (default: auto-generated). Ignored for `write` libraries. |
| `--yes` | Skip the interactive PR/MR confirmation (open it without prompting). Always implied in non-interactive mode. |

For each pushable skill, `skillctl push` runs a content diff (via git blob hashes) and applies the chosen strategy. `push` **follows provenance**: each skill is written back to the library it was installed from.
- For a `write` library: commits and pushes to its default branch, **one commit per library** (a run touching several makes several commits); each pushed `.skills.toml` entry's `source_sha` is rewritten to that library's new HEAD. In `--json`, `commit` is the single commit when exactly one `write` library was pushed, otherwise `null` (each result carries its `source_sha`).
- For a `pr` library: pushes a `skillctl/<slug>` branch and opens a PR (`gh`) / MR (`glab`); the result carries `"status":"pr_opened"`, `"pr_url"`, and `"branch"`, and the URL is shown in the outro. `.skills.toml` is **not** changed (the skill isn't merged yet). Interactive runs show an editable title + confirm; `--yes`/non-interactive open it directly.

Skills from `read` libraries, and skills whose provenance is no longer configured, are listed and skipped (see the access notes above).

**Promotion** (`push --to <writable-library>`): publishes the selected skills' local content into `<library>` (regardless of where they came from) and rewrites their `.skills.toml` provenance to it — the way to contribute a skill installed from a read-only source. Each skill lands at its current `source_path` in the target; if that path is already taken there, the `--on-divergence` policy decides — `overwrite` (replace the target's version), `fork` (add as a new skill under `<name>-<--fork-suffix>`, renaming the local folder), or `skip` (default non-interactively; interactive shows a three-way prompt). The target must be `write`-access (`read` is refused; `pr` promotion isn't supported yet). JSON results carry `"status":"promoted"` (+ `"new_name"` when forked).

**Fork** (creating a new library skill from local edits) is supported non-interactively via `--on-divergence fork --fork-suffix <s>`: every divergent (or library-missing) skill is forked under the name `<original>-<suffix>`.

### `skillctl pull` — refresh installed skills from the library

```sh
skillctl pull --skill <name> [--skill <name> …]
skillctl pull --all
skillctl pull --tag <tag> [--tag <tag> …] [--all-tags]
```

| Flag | Purpose |
|---|---|
| `--skill <name>` | Pull only specific skills by name (repeatable). Mutually exclusive with `--all`/`--tag`. |
| `--all` | Pull every skill that has library updates available. Mutually exclusive with `--skill`/`--tag`. |
| `--tag <tag>` | Pull every pullable skill whose **local** SKILL.md carries this tag. Repeatable; default semantics is union. Mutually exclusive with `--skill`/`--all`. |
| `--all-tags` | Switch tag matching to intersection. Requires `--tag`. |
| `--on-divergence <overwrite\|skip\|fork>` | Strategy for divergent skills. `fork` here means **fork-locally** (rename the local copy under a new name, then pull the library version into the original destination). Default when omitted: skip. |
| `--fork-suffix <suffix>` | Required when `--on-divergence fork` is used non-interactively. New local name = `<original>-<suffix>`. |

For each pullable skill, `skillctl pull` runs the same blob-SHA classification as `push` (in reverse direction): pullable = `LibraryAhead` (library moved, local hasn't) or `BothDiverged`. Library content overwrites local; the project's `.skills.toml` `source_sha` is rewritten to the current library HEAD. **No git operations on the project side** — the project repo is untouched, and the user can review/commit the resulting file changes via their own workflow. `pull` **follows provenance**: each skill refreshes from the library it was installed from (a run may touch several library caches), and its `source_sha` is rewritten to *that* library's HEAD. A skill whose recorded provenance is no longer a configured library is listed and skipped.

**Fork-locally** (preserving your local edits under a new name while pulling the library version into the original location) is supported non-interactively via `--on-divergence fork --fork-suffix <s>`: each divergent skill's local folder is renamed to `<original>-<suffix>`, then the library version drops into the original destination.

### `skillctl detect` — find new local skills and add them to the library

```sh
skillctl detect --skill <name> [--skill <name> …] --target <library-path>
skillctl detect --all --target <library-path>
skillctl detect --tag <tag> [--tag <tag> …] [--all-tags] --target <library-path>
```

| Flag | Purpose | Required in non-interactive |
|---|---|---|
| `--skill <name>` | Add a specific detected skill by name (repeatable). Mutually exclusive with `--all`/`--tag`. | Yes, unless `--all` or `--tag` |
| `--all` | Add every detected new skill. Mutually exclusive with `--skill`/`--tag`. | Yes, unless `--skill` or `--tag` |
| `--tag <tag>` | Add every newly detected skill carrying this tag (repeatable). Default semantics is union. Mutually exclusive with `--skill`/`--all`. | Yes, unless `--skill` or `--all` |
| `--all-tags` | Switch tag matching to intersection. Requires `--tag`. | No |
| `--to <name>` | Writable library to add the skills to. Defaults to the sole `write`-access library; **required when several are configured**. Refused for `read`/`pr` libraries. | Yes, when more than one writable library is configured |
| `--target <path>` | Library-relative folder where the new skills should land. Use `.` for the library root (flat-layout libraries), or e.g. `skills` / `.claude/skills` for a subfolder. | **Yes** |

`skillctl detect` walks the current directory for `SKILL.md` files, drops anything already declared in `.skills.toml`, copies the leftovers into the chosen library's cache under `<target>/<skill-folder-name>`, single-commits with a `add skill(s): …` message, pushes, and appends the new entries to `.skills.toml` with that library's provenance. The target library is the sole writable library by default; with several, pass `--to <name>` (non-interactive) or pick from the Select (interactive). `read`/`pr` libraries cannot be detect targets (the latter pending the PR/MR flow).

### `skillctl remove` — remove skills from the current project

```sh
skillctl remove --skill <name> [--skill <name> …]
skillctl remove --all
```

| Flag | Purpose | Required in non-interactive |
|---|---|---|
| `--skill <name>` | Remove a specific skill by name (repeatable). Mutually exclusive with `--all`. Errors if the name is unknown or ambiguous (two skills share it). | Yes, unless `--all` |
| `--all` | Remove every removable skill found in the project. Mutually exclusive with `--skill`. | Yes, unless `--skill` |

`skillctl remove` is **project-only** — it never touches the library or git. It walks the current directory for skill folders (respecting `.gitignore`, skipping `node_modules`/`target`) and cross-references `.skills.toml`, presenting three kinds of removable skill:

- **installed via skillctl** — folder present *and* tracked in `.skills.toml`. Removing it deletes the folder and drops the entry.
- **created locally, not tracked** — folder present but absent from `.skills.toml`. Removing it deletes the folder only.
- **orphan** — a `.skills.toml` entry whose folder is already gone. Removing it drops the stale entry only (nothing to delete on disk).

In each `results[]` item, `removed_folder` and `removed_entry` report which of the two actions actually happened. `.skills.toml` is only rewritten when at least one tracked entry is dropped. In an interactive TTY, a confirmation prompt is shown before anything is deleted; in non-interactive/`--json` mode the explicit `--skill`/`--all` flags are the authorisation. A symlinked destination is never followed — it is treated as "no folder on disk" so removal can only ever drop its manifest entry, never delete through the link.

### `skillctl audit` — scan skill content for dangerous patterns

```sh
skillctl audit                       # scan every skill in the project
skillctl audit --skill <name>        # scan only this skill (repeatable)
skillctl audit --fail-on warning     # exit 5 if any finding reaches the threshold
skillctl --json audit
```

| Flag | Purpose | Required in non-interactive |
|---|---|---|
| `--skill <name>` | Audit only this skill by name (repeatable). Mutually exclusive with `--all`. Errors if the name is unknown. | No |
| `--all` | Audit every skill found in the project (the default behaviour). Mutually exclusive with `--skill`. | No |
| `--fail-on <info\|warning\|critical>` | Exit with code 5 if any finding reaches this severity. Without it, `audit` always exits 0. | No |

`audit` is **read-only** — it scans the `SKILL.md` and any bundled files of each skill discovered in the current project and reports a per-skill verdict (`safe` / `caution` / `warning` / `dangerous`). Categories: `credentials` (embedded keys/tokens — critical), `obfuscation` (long base64 / hex-escape blobs — warning), `shell` (`rm -rf`, `curl|sh` — warning/info), `dynamic-code` (`eval(` — info), and `prompt-injection` (instruction-override / conceal-from-user / exfiltration phrasings — warning). It is a heuristic advisory aid, not a guarantee. The same scan gates `skillctl add` (see above). The `--json` shape is `{ "command": "audit", "skills": [ { "name", "verdict", "findings": [ { "severity", "category", "label", "file", "line", "snippet" } ] } ], "summary": { "scanned", "worst_severity" } }`.

## Skill identity

A "skill" is any folder containing a file literally named `SKILL.md`. The skill's `name` comes from the YAML frontmatter `name:` field at the top of `SKILL.md`; if absent, the folder name is used. All `--skill <name>` flags match against this resolved name.

## Exit codes

- `0` — success, including "nothing to do" outcomes (no changes to push, no new skills detected, no skills to install, etc.).
- `1` — generic / unexpected error.
- `2` — **configuration error**: no library configured, library cache missing, malformed URL, missing required flag in non-interactive mode (e.g. `--dest`, `--skill`, `--target`), invalid skill name, malformed `.skills.toml`.
- `3` — **conflict**: a destination already exists with no `--on-conflict` policy in non-interactive mode, a fork target collides in the library, or a local fork target collides.
- `4` — **git error**: `git clone`/`fetch`/`commit`/`push`/`hash-object`/`ls-tree` failed (auth, network, missing user identity, etc.).
- `5` — **content-audit threshold exceeded**: `add --fail-on <severity>` refused to install (nothing was installed), or `audit --fail-on <severity>` found a finding at or above the threshold.

Agents should branch on exit code first, then optionally inspect stderr for context. Stdout in `--json` mode is always either a single JSON object (success or partial success) or empty (early failure before output is built).

## Output

`skillctl` prints a tree-style human log to stdout in interactive/default mode (intro line, per-skill `log::*` lines, outro summary). In `--json` mode, that human log is suppressed and a single structured JSON object lands on stdout instead — see "Structured output: `--json`" above for shapes.

Errors always go to stderr regardless of mode.

## Interactive prompt (multi-select with live filter)

When a multi-select prompt opens (in `add`, `push`, `pull`, `detect`, `remove` without flags or `--all` and a TTY), the prompt has a live filter:

- **Type any character** — appends to the filter; the list filters in real time on the skill name (substring, case-insensitive).
- **Backspace** — edits the filter.
- **↑ / ↓** — navigates the filtered list.
- **Space** or **Tab** — toggles selection on the focused item.
- **Enter** — confirms the prompt with all currently-selected items.
- **Esc** or **Ctrl+C** — cancels.

The filter searches skill names only (so Space stays available as a toggle). The hint/description column is shown next to each row but not searched. A windowed view shows up to ~12 items at a time with `↑ N more above` / `↓ N more below` indicators when needed.

Agents driving the CLI never see this prompt — `--json` and non-TTY contexts suppress all interactive UI.

## Recipes

### Install a fresh project's skills

```sh
skillctl init git@github.com:me/skills.git
skillctl add --skill claude-api --skill review --dest .claude/skills
```

### Bulk-install everything

```sh
skillctl add --all --dest .claude/skills --on-conflict skip
```

### Add a team library and install from it

```sh
skillctl library add team https://gitlab.com/acme/ai-config --access read
skillctl list --from all                                  # browse what's where
skillctl add --from team --skill deploy --dest .claude/skills   # audit is mandatory here
```

### Bulk-install every skill carrying a tag

```sh
skillctl add --tag api --dest .claude/skills
```

### Install only skills tagged with both `code-review` AND `gitlab`

```sh
skillctl add --tag code-review --tag gitlab --all-tags --dest .claude/skills
```

### List every skill tagged `meta`

```sh
skillctl list --tag meta
```

### Push every local edit, defaulting to skip on conflicts

```sh
skillctl push --all
```

### Force-push everything, including diverged skills

```sh
skillctl push --all --on-divergence overwrite
```

### Fork every diverged skill under a `<name>-custom` library entry

```sh
skillctl push --all --on-divergence fork --fork-suffix custom
```

### Pull every available library update

```sh
skillctl pull --all
```

### Force-pull everything, even where local has unrelated edits

```sh
skillctl pull --all --on-divergence overwrite
```

### Pull updates while keeping your local edits as `<name>-local`

```sh
skillctl pull --all --on-divergence fork --fork-suffix local
```

### Contribute every new local skill back to the library (library root)

```sh
skillctl detect --all --target .
```

### Contribute new local skills to a `skills/` subfolder of the library

```sh
skillctl detect --all --target skills
```

### Push specific skills with a custom message

```sh
skillctl push --skill review --skill security-review --message "polish: tighter reviewer prompts"
```

### Remove specific skills from a project

```sh
skillctl remove --skill claude-api --skill review
```

### Remove every skill from a project (folders + manifest entries)

```sh
skillctl remove --all
```

## Failure modes worth checking before invoking

1. **No library configured.** Calls fail with `no library configured — run skillctl init <github-url> first`. Run `skillctl init` (or check for an existing library URL via inspecting `~/.config/skills-cli/config.toml` on Linux or the equivalent under `~/Library/Application Support/dev.umanio-agency.skills-cli/` on macOS).
2. **Library cache deleted.** Same fix: re-run `skillctl init` with the URL.
3. **Push without `user.name`/`user.email` configured globally.** Git itself errors out — the message is forwarded verbatim. Fix: `git config --global user.name …` / `user.email …`.
4. **`--target` for `detect` must be relative to the library root.** Absolute paths are rejected with a clear message.
5. **`--skill <name>` with a name not in the library / not detected.** Fails fast.

## Constraints to remember

- `skillctl` does not handle merges. When local and library both moved past the recorded `source_sha`, the operator chooses one side; there is no automatic three-way merge.
- `skillctl push` always produces **one commit per run**, regardless of how many skills are touched.
- Forking is now supported non-interactively via `--on-divergence fork --fork-suffix <s>` — the suffix is appended to each forked skill's name. Without `--fork-suffix`, fork stays interactive (each fork prompts for a name).
