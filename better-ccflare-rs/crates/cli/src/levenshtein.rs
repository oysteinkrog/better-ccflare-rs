//! Levenshtein distance for mode typo suggestions.

/// Compute the Levenshtein edit distance between two strings.
pub fn distance(a: &str, b: &str) -> usize {
    let a_bytes = a.as_bytes();
    let b_bytes = b.as_bytes();
    let m = a_bytes.len();
    let n = b_bytes.len();

    if m == 0 {
        return n;
    }
    if n == 0 {
        return m;
    }

    let mut prev: Vec<usize> = (0..=n).collect();
    let mut curr = vec![0usize; n + 1];

    for i in 1..=m {
        curr[0] = i;
        for j in 1..=n {
            let cost = if a_bytes[i - 1] == b_bytes[j - 1] {
                0
            } else {
                1
            };
            curr[j] = (prev[j] + 1).min(curr[j - 1] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }

    prev[n]
}

/// Return up to `max` suggestions from `candidates` where the Levenshtein
/// distance to `input` is at most `threshold`.
pub fn suggest(input: &str, candidates: &[&str], threshold: usize, max: usize) -> Vec<String> {
    let input_lower = input.to_lowercase();
    let mut suggestions: Vec<(usize, String)> = candidates
        .iter()
        .filter_map(|c| {
            let c_lower = c.to_lowercase();
            if c_lower == input_lower {
                return None; // Skip exact case-insensitive match
            }
            let d = distance(&input_lower, &c_lower);
            if d <= threshold {
                Some((d, c.to_string()))
            } else {
                None
            }
        })
        .collect();

    suggestions.sort_by_key(|(d, _)| *d);
    suggestions.into_iter().take(max).map(|(_, s)| s).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_strings() {
        assert_eq!(distance("test", "test"), 0);
    }

    #[test]
    fn empty_strings() {
        assert_eq!(distance("", ""), 0);
        assert_eq!(distance("abc", ""), 3);
        assert_eq!(distance("", "abc"), 3);
    }

    #[test]
    fn single_edit() {
        assert_eq!(distance("claude-oauth", "claude-outh"), 1);
        assert_eq!(distance("console", "consol"), 1);
    }

    #[test]
    fn two_edits() {
        assert_eq!(distance("claude-oauth", "claude-oeth"), 2);
    }

    #[test]
    fn suggest_typo() {
        let modes = &[
            "claude-oauth",
            "console",
            "zai",
            "minimax",
            "nanogpt",
            "anthropic-compatible",
            "openai-compatible",
            "vertex-ai",
        ];

        let suggestions = suggest("claude-outh", modes, 2, 3);
        assert!(!suggestions.is_empty());
        assert!(suggestions.contains(&"claude-oauth".to_string()));
    }

    #[test]
    fn suggest_no_match() {
        let modes = &["claude-oauth", "console"];
        let suggestions = suggest("xyzxyz", modes, 2, 3);
        assert!(suggestions.is_empty());
    }

    #[test]
    fn suggest_exact_skipped() {
        let modes = &["claude-oauth", "console"];
        let suggestions = suggest("console", modes, 2, 3);
        assert!(!suggestions.contains(&"console".to_string()));
    }
}
