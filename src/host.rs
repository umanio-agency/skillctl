//! Parsing and host detection for skill-library repository URLs.
//!
//! skillctl shells out to plain `git` for clone/fetch/push, so any host that
//! `git` + the operator's credentials can reach works for read and
//! write-direct. We therefore accept GitHub, GitLab, and self-hosted
//! instances over HTTPS or SSH, and do NOT keep a host allowlist (it's the
//! operator's own config). The only hard rejections are:
//!   - cleartext `http://` (a network attacker could downgrade the clone),
//!   - a leading `-` (would be parsed as a flag by `git clone <url>`),
//!   - control characters,
//!   - any `::` (the `transport::address` syntax of git's alternate transports
//!     — `ext::` executes a command, `file::`/`fd::` read locally; the scp
//!     form `git@host:path` makes these easy to disguise),
//!   - anything that isn't a recognisable HTTPS / SSH / scp-style git URL.
//!
//! That parser-level rejection is backed by a second line of defense:
//! `git::git_cmd` pins git's transport allowlist to https + ssh only, so even
//! a URL that somehow slipped past validation can never make git speak an
//! alternate transport regardless of the operator's ambient git config.
//!
//! The cache directory name is derived from the parsed URL as
//! `<host>-<path>-<hash>`: the human prefix aids debugging while the hash of
//! the normalized URL guarantees two distinct libraries never collide on one
//! cache dir — even across hosts (`github.com` vs `gitlab.com`, same
//! `owner/repo`) or when the human prefix is sanitised/truncated the same way.

use crate::error::AppError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteUrl {
    /// Lowercased hostname, without userinfo or port (e.g. `github.com`).
    pub host: String,
    /// Repository path, no leading/trailing `/`, no trailing `.git`
    /// (e.g. `owner/repo` or `group/subgroup/project`).
    pub path: String,
    /// Canonical `host/path` form — the key hashed for the cache slug and
    /// (in a later phase) used to match `.skills.toml` provenance across URL
    /// spellings.
    pub normalized: String,
}

pub fn parse_remote_url(url: &str) -> Result<RemoteUrl, AppError> {
    let url = url.trim();
    if url.is_empty() {
        return Err(AppError::Config("empty repository URL".into()));
    }
    if url.starts_with('-') {
        return Err(AppError::Config(format!(
            "refusing repository URL starting with `-`: {url} (would be parsed as a git flag)"
        )));
    }
    if url.bytes().any(|b| b < 0x20 || b == 0x7f) {
        return Err(AppError::Config(
            "repository URL contains control characters".into(),
        ));
    }
    // Alternate git transports use `transport::address` (e.g. `ext::sh -c
    // <cmd>` executes a command, `file::`/`fd::` access locally). None of our
    // accepted forms (https://, ssh://, scp `git@host:path`) contain `::`, so
    // reject it outright — a scp-shaped string like `ext::sh -c x@h:p` would
    // otherwise be misparsed as a valid SSH URL and handed to `git clone`.
    // (IPv6-literal hosts, which also use `::`, are intentionally unsupported.)
    if url.contains("::") {
        return Err(AppError::Config(format!(
            "refusing repository URL containing `::` (looks like an alternate git transport): {url} — expected an HTTPS or SSH URL"
        )));
    }
    if url.starts_with("http://") {
        return Err(AppError::Config(format!(
            "refusing cleartext HTTP URL: {url} — use the HTTPS form instead"
        )));
    }

    let (authority, path_raw) = if let Some(rest) = url.strip_prefix("https://") {
        split_authority_path(rest)?
    } else if let Some(rest) = url.strip_prefix("ssh://") {
        split_authority_path(rest)?
    } else if let Some(parts) = parse_scp(url) {
        parts
    } else {
        return Err(AppError::Config(format!(
            "unsupported repository URL: {url} — expected an HTTPS (`https://host/owner/repo`) or SSH (`git@host:owner/repo`) URL"
        )));
    };

    let host = normalize_host(&authority)?;
    let path = normalize_path(&path_raw)?;
    let normalized = format!("{host}/{path}");
    Ok(RemoteUrl {
        host,
        path,
        normalized,
    })
}

/// Stable, collision-free cache directory name for a parsed remote.
pub fn cache_slug(remote: &RemoteUrl) -> String {
    let human = sanitize_component(&format!("{}-{}", remote.host, remote.path));
    let hash = fnv1a_64(remote.normalized.as_bytes());
    format!("{human}-{hash:016x}")
}

/// Split `host[:port]/path` (or `user@host[:port]/path`) into authority and
/// path, dropping any `?query`/`#fragment`. Both parts must be non-empty.
fn split_authority_path(rest: &str) -> Result<(String, String), AppError> {
    let rest = rest.split(['?', '#']).next().unwrap_or(rest);
    match rest.split_once('/') {
        Some((authority, path)) if !authority.is_empty() => {
            Ok((authority.to_string(), path.to_string()))
        }
        _ => Err(AppError::Config(
            "malformed repository URL: expected host/owner/repo after the scheme".into(),
        )),
    }
}

/// Recognise the scp-style SSH form `user@host:path` (no scheme). Returns the
/// `user@host` authority and the path, or `None` if it isn't that shape.
fn parse_scp(url: &str) -> Option<(String, String)> {
    if url.contains("://") {
        return None;
    }
    let at = url.find('@')?;
    let after_at = &url[at + 1..];
    let colon = after_at.find(':')?;
    let authority = &url[..at + 1 + colon]; // user@host
    let path = &after_at[colon + 1..];
    if path.is_empty() {
        return None;
    }
    Some((authority.to_string(), path.to_string()))
}

/// Strip userinfo (`user[:pass]@`) and a trailing `:port`, lowercase the host.
fn normalize_host(authority: &str) -> Result<String, AppError> {
    let host_port = match authority.rsplit_once('@') {
        Some((_userinfo, hp)) => hp,
        None => authority,
    };
    let host = match host_port.rsplit_once(':') {
        Some((h, p)) if !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()) => h,
        _ => host_port,
    };
    let host = host.trim().to_ascii_lowercase();
    if host.is_empty() {
        return Err(AppError::Config(
            "repository URL has no host (expected host/owner/repo)".into(),
        ));
    }
    // A host starting with `-` could be read as a flag by ssh/git, and a host
    // with shell/option metacharacters is never legitimate. Restrict to a
    // conservative hostname charset (defense-in-depth; git rejects most of
    // these too, but we don't want to rely on that).
    if host.starts_with('-')
        || !host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-' || b == b'_')
    {
        return Err(AppError::Config(format!(
            "invalid host in repository URL: `{host}`"
        )));
    }
    Ok(host)
}

/// Trim slashes and a trailing `.git`; reject an empty path.
fn normalize_path(path: &str) -> Result<String, AppError> {
    let path = path.split(['?', '#']).next().unwrap_or(path);
    let p = path.trim().trim_matches('/');
    let p = p.strip_suffix(".git").unwrap_or(p);
    let p = p.trim_end_matches('/');
    if p.is_empty() {
        return Err(AppError::Config(
            "repository URL has no path (expected host/owner/repo)".into(),
        ));
    }
    Ok(p.to_string())
}

/// Map a string to a filesystem-safe, ASCII slug component. Non-`[A-Za-z0-9._-]`
/// becomes `-`; the result is bounded so the human prefix stays short (the
/// hash, not this prefix, is what guarantees uniqueness).
fn sanitize_component(s: &str) -> String {
    let mut out: String = s
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    out.truncate(80);
    out
}

/// 64-bit FNV-1a. Not cryptographic — used only to disambiguate cache dir
/// names — but stable across runs/versions (unlike `DefaultHasher`), so a
/// library's cache path never changes from under it.
fn fnv1a_64(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(url: &str) -> RemoteUrl {
        parse_remote_url(url).unwrap_or_else(|e| panic!("expected `{url}` to parse, got: {e}"))
    }

    #[test]
    fn https_github() {
        let r = parse("https://github.com/owner/repo");
        assert_eq!(r.host, "github.com");
        assert_eq!(r.path, "owner/repo");
        assert_eq!(r.normalized, "github.com/owner/repo");
    }

    #[test]
    fn https_strips_dot_git_and_trailing_slash() {
        assert_eq!(
            parse("https://github.com/owner/repo.git").path,
            "owner/repo"
        );
        assert_eq!(parse("https://github.com/owner/repo/").path, "owner/repo");
        assert_eq!(
            parse("https://github.com/owner/repo.git/").path,
            "owner/repo"
        );
    }

    #[test]
    fn https_strips_query_and_fragment() {
        assert_eq!(parse("https://github.com/o/r?ref=main").path, "o/r");
        assert_eq!(parse("https://github.com/o/r#frag").path, "o/r");
    }

    #[test]
    fn gitlab_is_now_accepted() {
        let r = parse("https://gitlab.com/group/sub/project");
        assert_eq!(r.host, "gitlab.com");
        assert_eq!(r.path, "group/sub/project");
    }

    #[test]
    fn self_hosted_with_port() {
        let r = parse("https://git.company.com:8443/team/repo.git");
        assert_eq!(r.host, "git.company.com");
        assert_eq!(r.path, "team/repo");
    }

    #[test]
    fn scp_ssh_form() {
        let r = parse("git@github.com:owner/repo.git");
        assert_eq!(r.host, "github.com");
        assert_eq!(r.path, "owner/repo");
        let g = parse("git@gitlab.com:group/sub/project.git");
        assert_eq!(g.host, "gitlab.com");
        assert_eq!(g.path, "group/sub/project");
    }

    #[test]
    fn ssh_url_form_with_user_and_port() {
        let r = parse("ssh://git@git.example.com:2222/owner/repo.git");
        assert_eq!(r.host, "git.example.com");
        assert_eq!(r.path, "owner/repo");
    }

    #[test]
    fn https_strips_userinfo_from_host() {
        let r = parse("https://x-access-token:tok@github.com/owner/repo");
        assert_eq!(r.host, "github.com");
        assert_eq!(r.path, "owner/repo");
    }

    #[test]
    fn rejects_cleartext_http() {
        assert!(parse_remote_url("http://github.com/o/r").is_err());
    }

    #[test]
    fn rejects_leading_dash() {
        assert!(parse_remote_url("-oProxyCommand=evil").is_err());
        assert!(parse_remote_url("--upload-pack=evil git@h:o/r").is_err());
    }

    #[test]
    fn rejects_control_chars() {
        assert!(parse_remote_url("https://github.com/o/r\nevil").is_err());
    }

    #[test]
    fn rejects_unsupported_transport_and_empty() {
        assert!(parse_remote_url("file:///etc/passwd").is_err());
        assert!(parse_remote_url("ext::sh -c whoami").is_err());
        assert!(parse_remote_url("").is_err());
        assert!(parse_remote_url("   ").is_err());
    }

    #[test]
    fn rejects_scp_shaped_alternate_transport() {
        // `ext::sh -c <cmd>@host:path` has the scp `@...:...` shape but is an
        // alternate-transport string; the `::` reject must catch it before it
        // can be handed to `git clone`.
        assert!(parse_remote_url("ext::sh -c evil@host:path").is_err());
        assert!(parse_remote_url("fd::17@host:path").is_err());
        assert!(parse_remote_url("file::/etc@host:path").is_err());
        // A legitimate scp URL (single colon, no `::`) still parses.
        assert!(parse_remote_url("git@github.com:owner/repo.git").is_ok());
    }

    #[test]
    fn rejects_dash_leading_and_metacharacter_hosts() {
        assert!(parse_remote_url("ssh://-oProxyCommand=x/owner/repo").is_err());
        assert!(parse_remote_url("git@-host:owner/repo").is_err());
        assert!(parse_remote_url("https://ho st/owner/repo").is_err());
    }

    #[test]
    fn rejects_missing_path() {
        assert!(parse_remote_url("https://github.com").is_err());
        assert!(parse_remote_url("https://github.com/").is_err());
        assert!(parse_remote_url("git@github.com:").is_err());
    }

    #[test]
    fn slug_is_deterministic_and_human_prefixed() {
        let r = parse("https://github.com/umanio-agency/skillctl");
        let s = cache_slug(&r);
        assert!(
            s.starts_with("github.com-umanio-agency-skillctl-"),
            "got: {s}"
        );
        assert_eq!(
            s,
            cache_slug(&parse("https://github.com/umanio-agency/skillctl"))
        );
    }

    #[test]
    fn slug_differs_across_hosts_for_same_path() {
        let gh = cache_slug(&parse("https://github.com/o/r"));
        let gl = cache_slug(&parse("https://gitlab.com/o/r"));
        assert_ne!(gh, gl);
    }

    #[test]
    fn slug_differs_for_dash_ambiguous_owners() {
        // Closes L11: `a-b/c` and `a/b-c` must not collide on one cache dir.
        let one = cache_slug(&parse("https://github.com/a-b/c"));
        let two = cache_slug(&parse("https://github.com/a/b-c"));
        assert_ne!(one, two);
    }

    #[test]
    fn slug_https_and_scp_same_repo_share_cache() {
        // Same logical repo via https vs scp normalizes to the same key.
        let a = cache_slug(&parse("https://github.com/o/r.git"));
        let b = cache_slug(&parse("git@github.com:o/r.git"));
        assert_eq!(a, b);
    }
}
