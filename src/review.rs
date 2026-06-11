//! Open a pull/merge request for a pushed branch via the host's own CLI.
//!
//! skillctl never stores a host API token: it shells out to `gh` (GitHub) or
//! `glab` (GitLab), which carry the operator's existing auth, exactly as the
//! git layer relies on the ambient git credentials. The branch must already be
//! pushed; this only opens the review request and returns its URL.

use std::path::Path;
use std::process::Command;

use crate::error::AppError;
use crate::sanitize::validate_message_safe;

/// Which review CLI to drive, detected from the library URL's host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Host {
    GitHub,
    GitLab,
    Other(String),
}

/// Map a parsed host (e.g. `github.com`, `gitlab.example.com`) to its review
/// CLI. Self-hosted GitLab instances conventionally carry `gitlab` in the
/// hostname; GitHub Enterprise hostnames are not reliably detectable, so only
/// `github.com` maps to GitHub here — anything else is `Other` and yields a
/// clear "open it manually" error rather than guessing wrong.
pub fn detect_host(host: &str) -> Host {
    let h = host.to_ascii_lowercase();
    if h == "github.com" {
        Host::GitHub
    } else if h == "gitlab.com" || h.contains("gitlab") {
        Host::GitLab
    } else {
        Host::Other(host.to_string())
    }
}

/// Open a PR/MR for `branch` targeting `base` in the repository checked out at
/// `repo_dir`, and return the created request's URL. `title`/`body` are passed
/// as separate argv (never a shell), and are validated to reject control
/// characters (CRLF/ESC/NUL) so neither can forge extra arguments or smuggle
/// terminal escapes into logs.
pub fn open_review_request(
    host: &Host,
    repo_dir: &Path,
    branch: &str,
    base: &str,
    title: &str,
    body: &str,
) -> Result<String, AppError> {
    validate_message_safe("PR/MR title", title)?;
    validate_message_safe("PR/MR body", body)?;

    let (bin, args) = match host {
        Host::GitHub => (
            "gh",
            vec![
                "pr", "create", "--head", branch, "--base", base, "--title", title, "--body", body,
            ],
        ),
        Host::GitLab => (
            "glab",
            vec![
                "mr",
                "create",
                "--source-branch",
                branch,
                "--target-branch",
                base,
                "--title",
                title,
                "--description",
                body,
                "--yes",
            ],
        ),
        Host::Other(h) => {
            return Err(AppError::Config(format!(
                "opening a PR/MR is not supported for host `{h}` — the branch `{branch}` has been pushed; open the request manually in your host's UI"
            )));
        }
    };

    let output = Command::new(bin)
        .current_dir(repo_dir)
        .args(&args)
        .output()
        .map_err(|e| {
            AppError::Config(format!(
                "could not run `{bin}` (is it installed and on PATH?): {e}"
            ))
        })?;

    if !output.status.success() {
        return Err(AppError::Git(format!(
            "`{bin}` failed to open the review request: {}",
            crate::git::scrub_stderr(&output.stderr)
        )));
    }

    // `gh pr create` / `glab mr create` print the request URL on stdout.
    // Scrub it the same way as stderr — strips control bytes and any credential
    // token pattern — so a tool variant that ever echoed a tokenised remote
    // can't leak it into the outro / JSON `pr_url` / logs.
    let stdout = String::from_utf8_lossy(&output.stdout);
    let url = stdout
        .lines()
        .map(str::trim)
        .find(|l| l.starts_with("https://") || l.starts_with("http://"))
        .map(|s| s.to_string())
        .unwrap_or_else(|| stdout.trim().to_string());
    Ok(crate::git::scrub_stderr(url.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_host_maps_known_hosts() {
        assert_eq!(detect_host("github.com"), Host::GitHub);
        assert_eq!(detect_host("GitHub.com"), Host::GitHub);
        assert_eq!(detect_host("gitlab.com"), Host::GitLab);
        assert_eq!(detect_host("gitlab.example.com"), Host::GitLab);
        assert_eq!(
            detect_host("git.company.com"),
            Host::Other("git.company.com".to_string())
        );
    }

    #[test]
    fn open_review_request_rejects_unsupported_host() {
        let err = open_review_request(
            &Host::Other("bitbucket.org".into()),
            Path::new("/tmp"),
            "skillctl/foo",
            "main",
            "t",
            "b",
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("not supported") && err.contains("bitbucket.org"));
    }

    #[test]
    fn open_review_request_rejects_control_chars_in_title() {
        assert!(
            open_review_request(
                &Host::GitHub,
                Path::new("/tmp"),
                "skillctl/foo",
                "main",
                "evil\r\nCo-Authored-By: x",
                "b",
            )
            .is_err()
        );
    }
}
