# Contributing to skillctl

Thanks for your interest. `skillctl` is pre-v1, so the contribution surface is intentionally narrow: the goal is **fits the round-trip vision** (push, detect, fork, pull) rather than "any feature is welcome".

## Before you start

- For non-trivial changes, **open an issue first** so we can align on the approach. Bug fixes, doc improvements, and clear UX papercuts can go straight to a PR.
- Read the [README](README.md) — the "What this is" and "What it does" sections capture the round-trip vision and the scope `skillctl` is trying to hold.

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

## Dependency policy

`skillctl` uses Cargo's default caret semantics for direct dependencies in `Cargo.toml` (`anyhow = "1.0.102"` means `>=1.0.102, <2.0.0`). This is the standard Rust convention and lets us pick up patch + minor releases without manual intervention.

Defenses we rely on:

- **Direct deps are vetted at adoption time** — a new direct dep needs an issue + PR + reviewer sign-off, not a drive-by addition.
- **`Cargo.lock` is committed** — the binary built for a release pins the exact transitive tree; users `cargo install`-ing build a fresh tree, but our release binaries and Homebrew formula are reproducible.
- **`cargo audit` runs as a release gate** — any RustSec advisory against a pinned dep blocks the next tag.
- **Transitive deps are not auto-updated** — `cargo update -p <crate>` is a deliberate action, not a recurring chore. If a transitive dep needs to move, the PR explains why.

If you suspect a dep ships a semver-incorrect breaking change in a patch version, file an issue with a minimal reproduction. We have not had to pin around this so far, but the policy if we do is: explicit `=X.Y.Z` lock with a comment pointing to the upstream issue.

## PR process

- Keep PRs focused; split unrelated changes into separate PRs.
- Link the issue if there is one.
- CI must pass. Don't disable hooks (`--no-verify` is not OK) — fix the underlying issue instead.
- A maintainer reviews and merges; we squash on merge.

## Code of conduct

This project follows the [Contributor Covenant](CODE_OF_CONDUCT.md).
