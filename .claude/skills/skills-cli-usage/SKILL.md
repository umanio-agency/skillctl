---
name: skills-cli-usage
description: How to drive the `skills` CLI non-interactively. Load PROACTIVELY when the user asks to install, push, or contribute Claude skills, or mentions a "skills library" / "skills repo". Covers every command's flag surface, exit codes, and end-to-end recipes so an agent can run `skills` without a TTY.
tags: [meta, agent-tooling]
---

# skills-cli-usage

`skills` is a Rust CLI that manages a personal Claude skills library across projects. This skill is the agent-facing reference: it documents how to drive every command **without prompts**, so any agent (Claude Code or otherwise) can use it as a tool.

> If you're a human reading this, the same flags work in interactive mode — they pre-fill choices and skip the relevant prompts.

## How non-interactive mode is selected

`skills` auto-detects whether stdin and stdout are TTYs. When called from a script, agent, or pipe, it switches to non-interactive mode automatically. You can also force it explicitly with the global `--no-interaction` flag, or with `--json` (which implies it).

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
{"command":"push","results":[{"name":"…","status":"pushed|forked|skipped","operation":"update|fork","…":"…"}],"commit":{"sha":"…","message":"…"}|null,"summary":{"pushed":N,"forked":N,"skipped":N}}

// pull
{"command":"pull","results":[{"name":"…","status":"pulled|skipped","fork_local":"…|null","fork_local_path":"…","source_sha":"…"}],"summary":{"pulled":N,"forked_locally":N,"skipped":N}}

// detect
{"command":"detect","target":"…|null","results":[{"name":"…","status":"added|skipped","library_path":"…","local_path":"…","source_sha":"…"}],"commit":{"sha":"…","message":"…"}|null,"summary":{"added":N,"skipped":N}}
```

Stable rules:
- The `command` field always matches the subcommand name.
- `results[]` only contains skills that were *acted on* (installed, pushed, forked, pulled, added, or explicitly skipped). Skills that were silently no-ops (e.g. unchanged on `push`) are not in `results[]`. To enumerate everything, use `skills list --json`.
- `summary` totals exactly equal the count of corresponding `status` values in `results[]`.
- `commit` is `null` when no commit was made (nothing to apply, or library-only-read commands like `pull`/`list`).

## One-time setup: link a library

```sh
skills init https://github.com/<owner>/<repo>
```

- Clones the library repo into a platform-appropriate cache.
- Persists the URL in a global config file.
- Re-running `init` against the same URL refreshes the cache; against a different URL replaces the cached library.
- Only GitHub URLs (HTTPS or SSH) are supported in v1.

## Commands

### `skills list` — read-only

```sh
skills list
skills list --tag <tag> [--tag <tag> …] [--all-tags]
```

Refreshes the library cache (best-effort `git fetch`) and prints every skill with its name, any frontmatter tags in `[…]`, and a one-line description.

| Flag | Purpose |
|---|---|
| `--tag <tag>` | Filter to skills carrying this tag. Repeatable; default semantics is union (any of the given tags). |
| `--all-tags` | Switch to intersection (skill must carry every requested tag). Requires `--tag`. |

### `skills add` — install skills from the library into a project

```sh
skills add --skill <name> [--skill <name> …] --dest <path>
skills add --all --dest <path>
skills add --tag <tag> [--tag <tag> …] [--all-tags] --dest <path>
```

| Flag | Purpose | Required in non-interactive |
|---|---|---|
| `--skill <name>` | Install a specific skill (repeatable). Mutually exclusive with `--all` and `--tag`. | Yes, unless `--all` or `--tag` |
| `--all` | Install every skill in the library. Mutually exclusive with `--skill` and `--tag`. | Yes, unless `--skill` or `--tag` |
| `--tag <tag>` | Install every skill carrying this tag (repeatable). Default semantics is union (any of the given tags). Mutually exclusive with `--skill` and `--all`. | Yes, unless `--skill` or `--all` |
| `--all-tags` | Switch tag matching from union to intersection (skill must carry every requested tag). Requires `--tag`. | No |
| `--dest <path>` | Project-relative destination folder (e.g. `.claude/skills`). The folder is created if missing. | **Yes** |
| `--on-conflict <overwrite\|skip\|abort>` | Strategy when a destination skill folder already exists. | Yes if any conflict is encountered |

Each installed skill is recorded in `.skills.toml` at the project root with the source path inside the library, the library commit SHA at install time, the local destination, and an RFC3339 timestamp.

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

### `skills push` — propagate local edits back to the library

```sh
skills push --skill <name> [--skill <name> …]
skills push --all
skills push --tag <tag> [--tag <tag> …] [--all-tags]
```

| Flag | Purpose |
|---|---|
| `--skill <name>` | Push only specific skills by name (repeatable). Mutually exclusive with `--all`/`--tag`. |
| `--all` | Push every skill that has pushable changes. Mutually exclusive with `--skill`/`--tag`. |
| `--tag <tag>` | Push every pushable skill whose **local** SKILL.md carries this tag. Repeatable; default semantics is union (any of). Mutually exclusive with `--skill`/`--all`. |
| `--all-tags` | Switch tag matching to intersection (skill must carry every requested tag). Requires `--tag`. |
| `--on-divergence <overwrite\|skip\|fork>` | Strategy for divergent (and library-missing) skills. Default when omitted: skip with a warning. |
| `--fork-suffix <suffix>` | Required when `--on-divergence fork` is used non-interactively. New name = `<original>-<suffix>`. |
| `--message <text>` | Override the auto-generated commit message. |

For each pushable skill, `skills push` runs a content diff (via git blob hashes), applies the chosen strategy, then commits **once** for the whole run and pushes to the library remote. The `source_sha` of every successfully pushed entry in `.skills.toml` is rewritten to the new HEAD.

**Fork** (creating a new library skill from local edits) is supported non-interactively via `--on-divergence fork --fork-suffix <s>`: every divergent (or library-missing) skill is forked under the name `<original>-<suffix>`.

### `skills pull` — refresh installed skills from the library

```sh
skills pull --skill <name> [--skill <name> …]
skills pull --all
skills pull --tag <tag> [--tag <tag> …] [--all-tags]
```

| Flag | Purpose |
|---|---|
| `--skill <name>` | Pull only specific skills by name (repeatable). Mutually exclusive with `--all`/`--tag`. |
| `--all` | Pull every skill that has library updates available. Mutually exclusive with `--skill`/`--tag`. |
| `--tag <tag>` | Pull every pullable skill whose **local** SKILL.md carries this tag. Repeatable; default semantics is union. Mutually exclusive with `--skill`/`--all`. |
| `--all-tags` | Switch tag matching to intersection. Requires `--tag`. |
| `--on-divergence <overwrite\|skip\|fork>` | Strategy for divergent skills. `fork` here means **fork-locally** (rename the local copy under a new name, then pull the library version into the original destination). Default when omitted: skip. |
| `--fork-suffix <suffix>` | Required when `--on-divergence fork` is used non-interactively. New local name = `<original>-<suffix>`. |

For each pullable skill, `skills pull` runs the same blob-SHA classification as `push` (in reverse direction): pullable = `LibraryAhead` (library moved, local hasn't) or `BothDiverged`. Library content overwrites local; the project's `.skills.toml` `source_sha` is rewritten to the current library HEAD. **No git operations on the project side** — the project repo is untouched, and the user can review/commit the resulting file changes via their own workflow.

**Fork-locally** (preserving your local edits under a new name while pulling the library version into the original location) is supported non-interactively via `--on-divergence fork --fork-suffix <s>`: each divergent skill's local folder is renamed to `<original>-<suffix>`, then the library version drops into the original destination.

### `skills detect` — find new local skills and add them to the library

```sh
skills detect --skill <name> [--skill <name> …] --target <library-path>
skills detect --all --target <library-path>
skills detect --tag <tag> [--tag <tag> …] [--all-tags] --target <library-path>
```

| Flag | Purpose | Required in non-interactive |
|---|---|---|
| `--skill <name>` | Add a specific detected skill by name (repeatable). Mutually exclusive with `--all`/`--tag`. | Yes, unless `--all` or `--tag` |
| `--all` | Add every detected new skill. Mutually exclusive with `--skill`/`--tag`. | Yes, unless `--skill` or `--tag` |
| `--tag <tag>` | Add every newly detected skill carrying this tag (repeatable). Default semantics is union. Mutually exclusive with `--skill`/`--all`. | Yes, unless `--skill` or `--all` |
| `--all-tags` | Switch tag matching to intersection. Requires `--tag`. | No |
| `--target <path>` | Library-relative folder where the new skills should land (e.g. `skills` or `.claude/skills`). | **Yes** |

`skills detect` walks the current directory for `SKILL.md` files, drops anything already declared in `.skills.toml`, copies the leftovers into the library cache under `<target>/<skill-folder-name>`, single-commits with a `add skill(s): …` message, pushes, and appends the new entries to `.skills.toml`.

## Skill identity

A "skill" is any folder containing a file literally named `SKILL.md`. The skill's `name` comes from the YAML frontmatter `name:` field at the top of `SKILL.md`; if absent, the folder name is used. All `--skill <name>` flags match against this resolved name.

## Exit codes

- `0` — success, including "nothing to do" outcomes (no changes to push, no new skills detected, no skills to install, etc.).
- `1` — generic / unexpected error.
- `2` — **configuration error**: no library configured, library cache missing, malformed URL, missing required flag in non-interactive mode (e.g. `--dest`, `--skill`, `--target`), invalid skill name, malformed `.skills.toml`.
- `3` — **conflict**: a destination already exists with no `--on-conflict` policy in non-interactive mode, a fork target collides in the library, or a local fork target collides.
- `4` — **git error**: `git clone`/`fetch`/`commit`/`push`/`hash-object`/`ls-tree` failed (auth, network, missing user identity, etc.).

Agents should branch on exit code first, then optionally inspect stderr for context. Stdout in `--json` mode is always either a single JSON object (success or partial success) or empty (early failure before output is built).

## Output

`skills` prints a tree-style human log to stdout in interactive/default mode (intro line, per-skill `log::*` lines, outro summary). In `--json` mode, that human log is suppressed and a single structured JSON object lands on stdout instead — see "Structured output: `--json`" above for shapes.

Errors always go to stderr regardless of mode.

## Interactive prompt (multi-select with live filter)

When a multi-select prompt opens (in `add`, `push`, `pull`, `detect` without flags or `--all` and a TTY), the prompt has a live filter:

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
skills init git@github.com:me/skills.git
skills add --skill claude-api --skill review --dest .claude/skills
```

### Bulk-install everything

```sh
skills add --all --dest .claude/skills --on-conflict skip
```

### Bulk-install every skill carrying a tag

```sh
skills add --tag api --dest .claude/skills
```

### Install only skills tagged with both `code-review` AND `gitlab`

```sh
skills add --tag code-review --tag gitlab --all-tags --dest .claude/skills
```

### List every skill tagged `meta`

```sh
skills list --tag meta
```

### Push every local edit, defaulting to skip on conflicts

```sh
skills push --all
```

### Force-push everything, including diverged skills

```sh
skills push --all --on-divergence overwrite
```

### Fork every diverged skill under a `<name>-custom` library entry

```sh
skills push --all --on-divergence fork --fork-suffix custom
```

### Pull every available library update

```sh
skills pull --all
```

### Force-pull everything, even where local has unrelated edits

```sh
skills pull --all --on-divergence overwrite
```

### Pull updates while keeping your local edits as `<name>-local`

```sh
skills pull --all --on-divergence fork --fork-suffix local
```

### Contribute every new local skill back to the library

```sh
skills detect --all --target .claude/skills
```

### Push specific skills with a custom message

```sh
skills push --skill review --skill security-review --message "polish: tighter reviewer prompts"
```

## Failure modes worth checking before invoking

1. **No library configured.** Calls fail with `no library configured — run skills init <github-url> first`. Run `skills init` (or check for an existing library URL via inspecting `~/.config/skills-cli/config.toml` on Linux or the equivalent under `~/Library/Application Support/dev.umanio-agency.skills-cli/` on macOS).
2. **Library cache deleted.** Same fix: re-run `skills init` with the URL.
3. **Push without `user.name`/`user.email` configured globally.** Git itself errors out — the message is forwarded verbatim. Fix: `git config --global user.name …` / `user.email …`.
4. **`--target` for `detect` must be relative to the library root.** Absolute paths are rejected with a clear message.
5. **`--skill <name>` with a name not in the library / not detected.** Fails fast.

## Constraints to remember

- `skills` does not handle merges. When local and library both moved past the recorded `source_sha`, the operator chooses one side; there is no automatic three-way merge.
- `skills push` always produces **one commit per run**, regardless of how many skills are touched.
- Forking is now supported non-interactively via `--on-divergence fork --fork-suffix <s>` — the suffix is appended to each forked skill's name. Without `--fork-suffix`, fork stays interactive (each fork prompts for a name).
