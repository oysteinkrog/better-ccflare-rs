/// Calculate the Levenshtein distance between two strings.
///
/// Used for CLI typo suggestions (e.g. "Did you mean 'session'?").
pub fn levenshtein_distance(s1: &str, s2: &str) -> usize {
    let s1_chars: Vec<char> = s1.chars().collect();
    let s2_chars: Vec<char> = s2.chars().collect();
    let len1 = s1_chars.len();
    let len2 = s2_chars.len();

    let mut matrix = vec![vec![0usize; len1 + 1]; len2 + 1];

    for (i, val) in matrix[0].iter_mut().enumerate().take(len1 + 1) {
        *val = i;
    }
    for (j, row) in matrix.iter_mut().enumerate().take(len2 + 1) {
        row[0] = j;
    }

    for j in 1..=len2 {
        for i in 1..=len1 {
            let cost = if s1_chars[i - 1] == s2_chars[j - 1] {
                0
            } else {
                1
            };
            matrix[j][i] = (matrix[j][i - 1] + 1)
                .min(matrix[j - 1][i] + 1)
                .min(matrix[j - 1][i - 1] + cost);
        }
    }

    matrix[len2][len1]
}

/// Find the closest match from a list of candidates using Levenshtein distance.
pub fn find_closest_match<'a>(
    input: &str,
    candidates: &[&'a str],
    max_distance: usize,
) -> Option<&'a str> {
    let mut best: Option<&str> = None;
    let mut best_distance = usize::MAX;

    for &candidate in candidates {
        let d = levenshtein_distance(input, candidate);
        if d < best_distance && d <= max_distance {
            best_distance = d;
            best = Some(candidate);
        }
    }

    best
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levenshtein_identical() {
        assert_eq!(levenshtein_distance("abc", "abc"), 0);
    }

    #[test]
    fn levenshtein_empty() {
        assert_eq!(levenshtein_distance("", "abc"), 3);
        assert_eq!(levenshtein_distance("abc", ""), 3);
        assert_eq!(levenshtein_distance("", ""), 0);
    }

    #[test]
    fn levenshtein_single_edit() {
        assert_eq!(levenshtein_distance("kitten", "sitten"), 1);
    }

    #[test]
    fn levenshtein_known_distance() {
        assert_eq!(levenshtein_distance("kitten", "sitting"), 3);
    }

    #[test]
    fn find_closest_match_found() {
        let candidates = &["session", "round-robin", "random"];
        assert_eq!(find_closest_match("sesion", candidates, 2), Some("session"));
    }

    #[test]
    fn find_closest_match_too_far() {
        let candidates = &["session", "round-robin"];
        assert_eq!(find_closest_match("xyz", candidates, 2), None);
    }
}
