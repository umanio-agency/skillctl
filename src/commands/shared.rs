/// Compact a possibly-long description into a single hint line:
/// strip newlines/runs of whitespace, cut at the first sentence end (`. `)
/// when reasonable, otherwise cap at ~100 chars with an ellipsis.
pub fn short_hint(desc: &str) -> String {
    let normalized = desc.split_whitespace().collect::<Vec<_>>().join(" ");
    const CAP: usize = 100;
    if let Some(period) = normalized.find('.')
        && period <= CAP
    {
        return normalized[..=period].to_string();
    }
    if normalized.chars().count() <= CAP {
        return normalized;
    }
    let truncated: String = normalized.chars().take(CAP).collect();
    format!("{truncated}…")
}

#[cfg(test)]
mod tests {
    use super::short_hint;

    #[test]
    fn keeps_short_descriptions() {
        assert_eq!(short_hint("simple"), "simple");
    }

    #[test]
    fn cuts_at_first_sentence() {
        assert_eq!(
            short_hint("First sentence. Second sentence."),
            "First sentence."
        );
    }

    #[test]
    fn truncates_when_no_period() {
        let desc = "a".repeat(200);
        let out = short_hint(&desc);
        assert!(out.ends_with('…'));
        assert_eq!(out.chars().count(), 101);
    }

    #[test]
    fn normalizes_whitespace() {
        assert_eq!(short_hint("  multi\n line   spaces"), "multi line spaces");
    }
}
