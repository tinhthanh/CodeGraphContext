/// Property coercion for graph backends.
/// FalkorDB and KuzuDB only accept primitives or flat lists.
/// Strings are truncated to MAX_STR_LEN (4096 chars).

const MAX_STR_LEN: usize = 4096;

/// Truncate a string to MAX_STR_LEN if needed.
pub fn truncate_string(s: &str) -> String {
    if s.len() > MAX_STR_LEN {
        s[..MAX_STR_LEN].to_string()
    } else {
        s.to_string()
    }
}

/// Sanitize a string value: truncate if too long.
pub fn sanitize_string(s: &str) -> String {
    truncate_string(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_truncate_short_string() {
        let s = "hello";
        assert_eq!(truncate_string(s), "hello");
    }

    #[test]
    fn test_truncate_long_string() {
        let s = "a".repeat(5000);
        let result = truncate_string(&s);
        assert_eq!(result.len(), MAX_STR_LEN);
    }
}
