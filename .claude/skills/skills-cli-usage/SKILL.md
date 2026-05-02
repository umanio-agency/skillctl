---
name: skills-cli-usage
description: How to drive the `skills` CLI non-interactively. Load PROACTIVELY when the user asks to install, push, or contribute Claude skills, or mentions a "skills library" / "skills repo". Covers every command's flag surface, exit codes, and end-to-end recipes so an agent can run `skills` without a TTY.
tags: [meta, agent-tooling]
---

# skills-cli-usage

`skills` is a Rust CLI that manages a personal Claude skills library across projects. This skill is the agent-facing reference: it documents how to drive every command **without prompts**, so any agent (Claude Code or otherwise) can use it as a tool.

> If you're a human reading this, the same flags work in interactive mode — they pre-fill choices and skip the relevant prompts.

## How non-interactive mode is selected

`skills` auto-detects whether stdin and stdout are TTYs. When called from a script, agent, or pipe, it switches to non-interactive mode automatically. You can also force it explicitly with the global `--no-interaction` flag.

In non-interactive mode, **every decision must come from a flag.** If a required input is missing, the command exits with a clear error rather than silently falling back to a prompt.

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

Tags are read from the `SKILL.md` frontmatter as an inline YAML array:

```yaml
---
name: claude-api
description: Build and tune Claude API apps with prompt caching.
tags: [api, claude, caching]
---
```

Block-style YAML (multi-line lists) is not supported in v1; use inline arrays. A bare scalar `tags: foo` is accepted and treated as a single-tag list.

### `skills push` — propagate local edits back to the library

```sh
skills push --skill <name> [--skill <name> …]
skills push --all
```

| Flag | Purpose |
|---|---|
| `--skill <name>` | Push only specific skills by name (repeatable). |
| `--all` | Push every skill that has pushable changes. |
| `--on-divergence <overwrite\|skip>` | Strategy for skills that changed both locally **and** in the library since install. Default behaviour when omitted: skip with a warning. |
| `--message <text>` | Override the auto-generated commit message. |

For each pushable skill, `skills push` runs a content diff (via git blob hashes), applies the chosen strategy, then commits **once** for the whole run and pushes to the library remote. The `source_sha` of every successfully pushed entry in `.skills.toml` is rewritten to the new HEAD.

**Fork** (creating a new library skill from local edits) is interactive-only in v1. In non-interactive mode, divergent skills are skipped if `--on-divergence` isn't `overwrite`.

### `skills pull` — refresh installed skills from the library

```sh
skills pull --skill <name> [--skill <name> …]
skills pull --all
```

| Flag | Purpose |
|---|---|
| `--skill <name>` | Pull only specific skills by name (repeatable). |
| `--all` | Pull every skill that has library updates available. |
| `--on-divergence <overwrite\|skip>` | Strategy for skills that changed both locally **and** in the library since install. Default behaviour when omitted: skip with a warning. |

For each pullable skill, `skills pull` runs the same blob-SHA classification as `push` (in reverse direction): pullable = `LibraryAhead` (library moved, local hasn't) or `BothDiverged`. Library content overwrites local; the project's `.skills.toml` `source_sha` is rewritten to the current library HEAD. **No git operations on the project side** — the project repo is untouched, and the user can review/commit the resulting file changes via their own workflow.

**Fork-locally** (preserving your local edits under a new name while pulling the library version into the original location) is interactive-only in v1. In non-interactive mode, divergent skills are skipped if `--on-divergence` isn't `overwrite`.

### `skills detect` — find new local skills and add them to the library

```sh
skills detect --skill <name> [--skill <name> …] --target <library-path>
skills detect --all --target <library-path>
```

| Flag | Purpose | Required in non-interactive |
|---|---|---|
| `--skill <name>` | Add a specific detected skill by name (repeatable). | Yes, unless `--all` |
| `--all` | Add every detected new skill. | Yes, unless `--skill` |
| `--target <path>` | Library-relative folder where the new skills should land (e.g. `skills` or `.claude/skills`). | **Yes** |

`skills detect` walks the current directory for `SKILL.md` files, drops anything already declared in `.skills.toml`, copies the leftovers into the library cache under `<target>/<skill-folder-name>`, single-commits with a `add skill(s): …` message, pushes, and appends the new entries to `.skills.toml`.

## Skill identity

A "skill" is any folder containing a file literally named `SKILL.md`. The skill's `name` comes from the YAML frontmatter `name:` field at the top of `SKILL.md`; if absent, the folder name is used. All `--skill <name>` flags match against this resolved name.

## Exit codes (v1)

- `0` — success, including "nothing to do" outcomes (no changes to push, no new skills detected, etc.).
- `1` — any error (missing config, missing flag in non-interactive mode, git failure, network failure, malformed args, etc.). The human-readable error is printed on stderr.

Agents should treat anything non-zero as failure and read stderr for context. Finer-grained exit codes are tracked in the project's backlog.

## Output

`skills` always prints a structured tree-style log to stdout (intro line, per-skill `log::*` lines, outro summary) regardless of mode. There is **no `--json` mode yet** — that's a planned future addition. For now, agents should prefer asserting on success/failure (exit code) and rely on side effects (`.skills.toml` contents, files in the destination, library remote state) for verification.

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

### Pull every available library update

```sh
skills pull --all
```

### Force-pull everything, even where local has unrelated edits

```sh
skills pull --all --on-divergence overwrite
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
- Forking a divergent skill into a new library skill is currently **interactive-only**. To do it from an agent, ask the user.
- Forking *locally* during `pull` (preserving local edits under a new name) is also interactive-only.
