# Security policy

`skillctl` (binary `skillctl`) is a local CLI. Its surface is small:

- It reads and writes inside the working directory and a per-user cache directory (`~/Library/Caches/dev.umanio-agency.skills-cli/` on macOS, the XDG-equivalent on Linux).
- It shells out to your local `git` binary for clone/fetch/commit/push, using whatever credentials your `git` is already configured with (gh helper, SSH agent, etc.). It does **not** handle credentials itself.
- It reads YAML-like frontmatter from `SKILL.md` files and writes TOML to `.skills.toml` and the global config file.
- There is no network surface beyond what `git` does, no privileged operations, and no telemetry.

## Reporting a vulnerability

If you find a security issue (e.g. a way to make `skillctl` write outside the destinations it's supposed to, or to leak credentials in error messages), please report it privately:

1. **Preferred:** use [GitHub's "Report a vulnerability"](https://github.com/umanio-agency/skillctl/security/advisories/new) button on this repo.
2. **Or email:** pinho.dcj@gmail.com.

Please do not open a public issue for security reports. We aim to acknowledge within 7 days.

## Supported versions

The project is pre-v1; only the `main` branch is supported and security fixes land there. Once v1 ships, this section will document the supported release range.
