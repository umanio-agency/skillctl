# Security policy

`skillctl` (binary `skillctl`) is a local CLI. Its surface is small:

- It reads and writes inside the working directory and a per-user cache directory (`~/Library/Caches/dev.umanio-agency.skills-cli/` on macOS, the XDG-equivalent on Linux).
- It shells out to your local `git` binary for clone/fetch/commit/push, using whatever credentials your `git` is already configured with (gh helper, SSH agent, etc.). It does **not** handle credentials itself.
- It reads YAML-like frontmatter from `SKILL.md` files and writes TOML to `.skills.toml` and the global config file.
- There is no network surface beyond what `git` does, no privileged operations, and no telemetry.

## Trust model

`skillctl`'s internal validation and sanitisation is shaped by the following trust boundaries. External auditors and contributors should focus probes on the **adversarial** category — anything in the **trusted** category is taken at face value.

**Trusted (taken at face value):**

- The operator's machine — filesystem, `$PATH`, environment variables, git binary, git's configured credentials.
- Flags typed by the operator on an interactive TTY. When skillctl runs interactively, `--dest <absolute-path>` and other path-accepting flags are accepted as-is.
- The skillctl binary itself, its dependencies (audited via `cargo audit` on every release), and the configuration written to the per-user config file (which only skillctl writes).

**Semi-trusted (the operator chose the source but its content is treated as adversarial):**

- The library repository URL passed to `skillctl init`. The host (GitHub) is trusted; the *content* it serves is not.
- The library cache (`~/Library/Caches/dev.umanio-agency.skills-cli/<slug>/`). Skillctl owns the directory but treats every file under it as adversarial after the initial clone.

**Adversarial (treated as untrusted in every code path that touches them):**

- `SKILL.md` frontmatter (`name`, `description`, `tags`) from any source — library cache, project tree, fork-locally targets. Sanitised at the discovery boundary; control bytes / ANSI / NUL / CRLF are rejected; oversize files (> 1 MiB) are refused.
- `.skills.toml` entries, especially those that arrive via PR. `name`, `source_path`, `source_sha`, `destination` are validated at load: identifier-class for `name`, hex regex for `source_sha`, lexical subpath check (no `..`, no absolute, no Windows prefix) for the two `PathBuf` fields. Duplicates and unknown fields are rejected.
- The library cache's git working tree and submodules. `git clone --no-recurse-submodules` blocks submodule pull-through; every git invocation runs with `-c core.hooksPath=/dev/null` so a malicious library cannot ship hook scripts.
- Skill folder contents: symlinks, hardlinks (Unix), FIFOs, devices, and sockets are refused at copy time. File modes are masked to `0o644 | (src_mode & 0o100)` on Unix — only the user-execute bit propagates.
- Non-interactive flag values (`--dest <path>` in `--json` / `--no-interaction` mode, where the "operator" may be an LLM running on attacker-supplied input). Absolute paths are rejected; `..` is always rejected.

**Out of scope (not defended against):**

- A compromised git binary or local credential helper. Skillctl uses whatever git you point it at; if `which git` returns a trojan, all bets are off.
- A compromised library repository owned by a third party that the operator explicitly trusts (e.g. corporate skills repo). Skillctl reduces blast radius via the controls above, but cannot make a malicious-but-trusted library safe.
- Side-channel attacks via filesystem timing, memory analysis, or OS-level surveillance. The threat model assumes a normal single-user developer machine.

## Reporting a vulnerability

If you find a security issue (e.g. a way to make `skillctl` write outside the destinations it's supposed to, or to leak credentials in error messages), please report it privately:

1. **Preferred:** use [GitHub's "Report a vulnerability"](https://github.com/umanio-agency/skillctl/security/advisories/new) button on this repo.
2. **Or email:** pinho.dcj@gmail.com.

Please do not open a public issue for security reports. We aim to acknowledge within 7 days.

## Supported versions

The project is pre-v1; only the `main` branch is supported and security fixes land there. Once v1 ships, this section will document the supported release range.

## Install channel hygiene

- **Homebrew:** always use the fully-qualified tap name — `brew install umanio-agency/homebrew-tap/skillctl`. Homebrew taps are namespaced by their GitHub owner, and anyone can create a `homebrew-tap` repo and publish a `skillctl.rb` formula. Pinning the owner (`umanio-agency`) prevents typo-squat attacks where a malicious tap is added before the official one.
- **crates.io:** the published crate is `skillctl` (owner: `pinho.dcj@gmail.com`). The historical name `skills-cli` is owned by an unrelated third party and is **not** affiliated with this project.
- **Direct binaries:** the `curl | sh` and PowerShell installers serve assets from `github.com/umanio-agency/skillctl/releases/latest/download/…` and verify SHA-256 sums published alongside each release.

## Verifying release binaries

Starting with v0.1.8, every binary in a GitHub Release ships with an [SLSA build-provenance attestation](https://slsa.dev/spec/v1.0/levels) signed by GitHub Actions and recorded in Sigstore's transparency log. The attestation proves the binary was produced by `umanio-agency/skillctl`'s release workflow on the corresponding tag — anyone who downloads the binary from somewhere else (a mirror, a cached fork, a tampered installer) will fail verification.

To verify a binary you downloaded:

```sh
# Install the GitHub CLI if you don't have it: https://cli.github.com/
gh auth login                                                # one-time
gh attestation verify skillctl-x86_64-apple-darwin.tar.xz \
  --repo umanio-agency/skillctl
```

A successful verification prints `Loaded digest sha256:... matches verified attestation`. A mismatch or missing attestation means the artifact was not produced by our release workflow — do not run it.

If you install via `cargo install skillctl`, the binary is built locally from the crates.io source and SLSA verification does not apply (cargo registry tampering would need a separate trust path).
