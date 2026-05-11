# Contributing to skillctl

Thanks for your interest. `skillctl` is pre-v1, so the contribution surface is intentionally narrow: the goal is **fits the round-trip vision** (push, detect, fork, pull) rather than "any feature is welcome".

## Before you start

- For non-trivial changes, **open an issue first** so we can align on the approach. Bug fixes, doc improvements, and clear UX papercuts can go straight to a PR.
- Look at [`.claude/skills/skillctl-project/SKILL.md`](.claude/skills/skillctl-project/SKILL.md) — vision, architecture, and an append-only decisions log. Many design questions already have a recorded answer there.

## Dev setup

Requirements:

- Rust 1.85+ (edition 2024)
- `git` available on `PATH` (the CLI shells out to it)

```sh
git clone https://github.com/umanio-agency/skillctl.git
cd skillctl
cargo build
```

## Local checks before pushing

CI runs all four; please run them locally so PRs land green:

```sh
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo build --all-targets
cargo test --all
```

## Commit messages

- Subject in **imperative mood** ("add fork-locally", not "added fork-locally").
- One topic per commit when feasible.
- Body explains **why**, not what — the diff already shows the what.
- Wrap body lines at ~72 chars.
- **Do not** add `Co-Authored-By: Claude` (or any AI co-author) trailers, even if you used an AI assistant to draft the change. Stay consistent with the existing history.

## PR process

- Keep PRs focused; split unrelated changes into separate PRs.
- Link the issue if there is one.
- CI must pass. Don't disable hooks (`--no-verify` is not OK) — fix the underlying issue instead.
- A maintainer reviews and merges; we squash on merge.

## Code of conduct

This project follows the [Contributor Covenant](CODE_OF_CONDUCT.md).
