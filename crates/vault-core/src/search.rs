//! Free-text to FTS5 MATCH query building.

/// Turn free user input into a safe FTS5 MATCH expression.
///
/// Each whitespace-separated term is quoted as an FTS5 string (embedded
/// double quotes escaped by doubling) and the quoted terms are joined with
/// spaces, i.e. implicit AND. This can never produce an FTS5 syntax error
/// from arbitrary user input, and special characters (`-`, `*`, `NEAR`, ...)
/// lose their operator meaning inside a quoted string.
pub fn build_match_query(input: &str) -> String {
    input
        .split_whitespace()
        .map(|term| format!("\"{}\"", term.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_each_term() {
        assert_eq!(build_match_query("hello world"), "\"hello\" \"world\"");
    }

    #[test]
    fn empty_input_yields_empty_query() {
        assert_eq!(build_match_query(""), "");
        assert_eq!(build_match_query("   \t\n"), "");
    }

    #[test]
    fn escapes_embedded_quotes() {
        // The term `"hi"` becomes the FTS5 string `"""hi"""` (doubled quotes).
        assert_eq!(build_match_query("say \"hi\""), "\"say\" \"\"\"hi\"\"\"");
    }

    #[test]
    fn neutralizes_fts_operators() {
        // Terms that would be invalid or operator-like unquoted.
        assert_eq!(build_match_query("-not NEAR *"), "\"-not\" \"NEAR\" \"*\"");
    }
}
