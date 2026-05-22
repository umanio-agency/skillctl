//! String sanitisation for values sourced from attacker-controlled inputs
//! that flow into terminal output, commit messages, or JSON.
//!
//! Two policies:
//! - [`validate_identifier`] — strict. Single-line, no control bytes, no
//!   escape sequences. Used for skill `name`, individual `tags`, and any
//!   other field where the value must be embeddable in a terminal log line,
//!   a commit subject, or a JSON identifier without risk of ANSI/OSC-8
//!   hijacking or CRLF trailer injection.
//! - [`validate_message_safe`] — lenient. Allows `\n` and `\t` (legitimate
//!   in multi-line descriptions and user-supplied commit message bodies)
//!   but rejects `\r` (CRLF injection), DEL, ESC, and other C0/C1 control
//!   bytes.
//!
//! Both return `AppError::Config` on rejection so the failure surfaces as
//! exit code 2 with a clear stderr message.

use crate::error::AppError;

/// Reject any control character. Used for short identifiers (skill names,
/// tags) that must stay single-line and free of terminal escape sequences.
pub fn validate_identifier(label: &str, s: &str) -> Result<(), AppError> {
    for (i, ch) in s.char_indices() {
        if ch == '\n' || ch == '\r' || ch == '\t' || is_control_or_escape(ch) {
            return Err(AppError::Config(format!(
                "{label} contains a control character at byte {i} (U+{:04X}); refusing to use it (would risk ANSI/OSC-8 injection or commit-message trailer forgery)",
                ch as u32
            )));
        }
    }
    Ok(())
}

/// Lenient variant: allows newline and tab (legitimate in multi-line
/// descriptions or user-supplied commit messages) but rejects CR, DEL,
/// ESC, and other C0/C1 controls.
pub fn validate_message_safe(label: &str, s: &str) -> Result<(), AppError> {
    for (i, ch) in s.char_indices() {
        if ch == '\n' || ch == '\t' {
            continue;
        }
        if ch == '\r' || is_control_or_escape(ch) {
            return Err(AppError::Config(format!(
                "{label} contains a control character at byte {i} (U+{:04X}); refusing to use it",
                ch as u32
            )));
        }
    }
    Ok(())
}

fn is_control_or_escape(ch: char) -> bool {
    let cp = ch as u32;
    // C0 controls (U+0000..U+001F, except \n and \t handled by callers).
    // DEL (U+007F). C1 controls (U+0080..U+009F, includes CSI U+009B).
    cp < 0x20 || cp == 0x7F || (0x80..=0x9F).contains(&cp)
}

/// Maximum length (in bytes) of a fork name. Beyond this it stops being a
/// sensible folder name and starts being a vector to bury arguments inside
/// path-display strings.
pub const FORK_NAME_MAX_LEN: usize = 64;

/// Validate a fork name (used as a new skill folder name in the library and
/// in the project). Rejected:
/// - empty / `.` / `..` (would resolve to the parent dir on join);
/// - `/` or `\` (path separator — would let an operator escape the parent dir);
/// - any control character (NUL panics in `CString` conversion inside
///   `Command` on Unix; ANSI/OSC-8 sequences hijack terminal output);
/// - longer than [`FORK_NAME_MAX_LEN`] bytes.
///
/// Returns `&'static str` because all the rejections are static messages
/// that flow into a `cliclack` validator (which wants `Err(&str)`).
pub fn validate_fork_name(name: &str) -> Result<(), &'static str> {
    if name.is_empty() {
        return Err("name cannot be empty");
    }
    if name.len() > FORK_NAME_MAX_LEN {
        return Err("name is too long (max 64 bytes)");
    }
    if name.contains('/') || name.contains('\\') {
        return Err("name cannot contain `/` or `\\`");
    }
    if name == "." || name == ".." {
        return Err("name cannot be `.` or `..`");
    }
    if name
        .chars()
        .any(|c| c == '\n' || c == '\r' || c == '\t' || is_control_or_escape(c))
    {
        return Err("name cannot contain control characters (NUL, ESC, ANSI sequences, etc.)");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id_accepted(s: &str) {
        assert!(
            validate_identifier("test", s).is_ok(),
            "expected `{}` to be accepted",
            s.escape_debug()
        );
    }
    fn id_rejected(s: &str) {
        assert!(
            validate_identifier("test", s).is_err(),
            "expected `{}` to be rejected",
            s.escape_debug()
        );
    }
    fn msg_accepted(s: &str) {
        assert!(
            validate_message_safe("test", s).is_ok(),
            "expected `{}` to be accepted",
            s.escape_debug()
        );
    }
    fn msg_rejected(s: &str) {
        assert!(
            validate_message_safe("test", s).is_err(),
            "expected `{}` to be rejected",
            s.escape_debug()
        );
    }

    #[test]
    fn identifier_accepts_safe_strings() {
        id_accepted("foo");
        id_accepted("claude-api");
        id_accepted("");
        id_accepted("UPPER-and-lower_123");
        id_accepted("a/b"); // slash is fine for our purposes; not a control char
    }

    #[test]
    fn identifier_rejects_newlines() {
        id_rejected("foo\n");
        id_rejected("foo\nCo-Authored-By: evil@x");
        id_rejected("foo\nbar");
    }

    #[test]
    fn identifier_rejects_carriage_return() {
        id_rejected("foo\r");
        id_rejected("foo\r\nbar");
    }

    #[test]
    fn identifier_rejects_tab() {
        id_rejected("foo\tbar");
    }

    #[test]
    fn identifier_rejects_ansi_escape() {
        id_rejected("\x1b[31mred\x1b[0m");
        id_rejected("foo\x1b]8;;file:///etc/passwd\x07bar\x1b]8;;\x07");
    }

    #[test]
    fn identifier_rejects_null_byte() {
        id_rejected("foo\0bar");
    }

    #[test]
    fn identifier_rejects_del_and_c1() {
        id_rejected("foo\x7fbar"); // DEL
        id_rejected("foo\u{009b}bar"); // CSI (C1)
    }

    #[test]
    fn message_safe_allows_newline_and_tab() {
        msg_accepted("first line\nsecond line");
        msg_accepted("col1\tcol2\ncol3\tcol4");
        msg_accepted("");
    }

    #[test]
    fn message_safe_rejects_carriage_return() {
        msg_rejected("foo\r\n"); // CRLF injection vector
    }

    #[test]
    fn message_safe_rejects_ansi_escape() {
        msg_rejected("commit msg \x1b[31mred\x1b[0m");
    }

    #[test]
    fn message_safe_rejects_null_byte() {
        msg_rejected("foo\0bar");
    }

    #[test]
    fn fork_name_accepts_normal_names() {
        assert!(validate_fork_name("foo-custom").is_ok());
        assert!(validate_fork_name("my_skill_v2").is_ok());
        assert!(validate_fork_name("a").is_ok());
    }

    #[test]
    fn fork_name_rejects_empty_and_dots() {
        assert!(validate_fork_name("").is_err());
        assert!(validate_fork_name(".").is_err());
        assert!(validate_fork_name("..").is_err());
    }

    #[test]
    fn fork_name_rejects_path_separators() {
        assert!(validate_fork_name("foo/bar").is_err());
        assert!(validate_fork_name("foo\\bar").is_err());
    }

    #[test]
    fn fork_name_rejects_nul_byte() {
        assert!(validate_fork_name("foo\0bar").is_err());
    }

    #[test]
    fn fork_name_rejects_control_chars() {
        assert!(validate_fork_name("foo\nbar").is_err());
        assert!(validate_fork_name("foo\rbar").is_err());
        assert!(validate_fork_name("foo\tbar").is_err());
        assert!(validate_fork_name("foo\x1b[31mbar").is_err());
        assert!(validate_fork_name("foo\x7fbar").is_err());
    }

    #[test]
    fn fork_name_rejects_too_long() {
        let too_long = "a".repeat(super::FORK_NAME_MAX_LEN + 1);
        assert!(validate_fork_name(&too_long).is_err());
    }

    #[test]
    fn fork_name_accepts_at_max_len() {
        let at_max = "a".repeat(super::FORK_NAME_MAX_LEN);
        assert!(validate_fork_name(&at_max).is_ok());
    }
}
