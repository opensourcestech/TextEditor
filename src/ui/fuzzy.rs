/// Fuzzy matching: every query character must appear in order,
/// and matches at word starts or in a row score higher.
///
/// Returns the score and the matched character indices in `candidate`
/// (used to highlight them), or `None` if the query does not match.
pub fn fuzzy_match(query: &str, candidate: &str) -> Option<(i32, Vec<usize>)> {
    let query: Vec<char> = query
        .chars()
        .filter(|c| !c.is_whitespace())
        .map(fold)
        .collect();
    if query.is_empty() {
        return Some((0, Vec::new()));
    }
    let chars: Vec<char> = candidate.chars().collect();
    let lower: Vec<char> = chars.iter().copied().map(fold).collect();

    // Cheap rejection before the full search: most candidates fail here.
    let mut next = 0;
    for &c in &lower {
        if next < query.len() && c == query[next] {
            next += 1;
        }
    }
    if next < query.len() {
        return None;
    }

    let (m, n) = (query.len(), chars.len());
    let bonus: Vec<i32> = (0..n).map(|j| start_bonus(&chars, j)).collect();

    // best[i][j]: best score with query[..=i] matched and query[i] at candidate[j].
    // from[i][j]: where query[i - 1] was matched on that best path.
    const NONE: i32 = i32::MIN / 2;
    let mut best = vec![vec![NONE; n]; m];
    let mut from = vec![vec![0usize; n]; m];
    for i in 0..m {
        // Best score in the previous row at least two characters back (a gap).
        let (mut gap_best, mut gap_at) = (NONE, 0);
        for j in 0..n {
            if i > 0 && j >= 2 && best[i - 1][j - 2] > gap_best {
                gap_best = best[i - 1][j - 2];
                gap_at = j - 2;
            }
            if lower[j] != query[i] {
                continue;
            }
            let base = 1 + bonus[j];
            if i == 0 {
                // Matches that start late in the string are slightly worse.
                best[0][j] = base - (j as i32).min(12) / 3;
                continue;
            }
            if j >= 1 && best[i - 1][j - 1] > NONE {
                best[i][j] = best[i - 1][j - 1] + base + 6;
                from[i][j] = j - 1;
            }
            if gap_best > NONE && gap_best + base > best[i][j] {
                best[i][j] = gap_best + base;
                from[i][j] = gap_at;
            }
        }
    }

    let (mut j, score) = (0..n)
        .map(|j| (j, best[m - 1][j]))
        .max_by_key(|&(j, s)| (s, std::cmp::Reverse(j)))?;
    if score <= NONE {
        return None;
    }
    let mut positions = vec![0; m];
    for i in (0..m).rev() {
        positions[i] = j;
        j = from[i][j];
    }
    Some((score, positions))
}

fn fold(c: char) -> char {
    c.to_lowercase().next().unwrap_or(c)
}

/// Extra score for a character that starts a word: the first character,
/// one after a separator, or an uppercase letter in camelCase.
fn start_bonus(chars: &[char], j: usize) -> i32 {
    let Some(&prev) = j.checked_sub(1).and_then(|p| chars.get(p)) else {
        return 10;
    };
    match prev {
        '/' | '\\' => 9,
        '_' | '-' | '.' | ' ' | ':' => 8,
        _ if prev.is_lowercase() && chars[j].is_uppercase() => 7,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requires_characters_in_order() {
        assert!(fuzzy_match("abc", "a_b_c").is_some());
        assert!(fuzzy_match("cba", "a_b_c").is_none());
        assert!(fuzzy_match("", "anything").is_some());
    }

    #[test]
    fn ignores_case_and_spaces() {
        assert!(fuzzy_match("SAVE as", "File: Save As…").is_some());
    }

    #[test]
    fn prefers_word_starts_and_runs() {
        let score = |q, c| fuzzy_match(q, c).unwrap().0;
        assert!(score("app", "src/app.rs") > score("app", "src/wrapper.rs"));
        assert!(score("mr", "src/main.rs") > score("mr", "src/summary"));
        assert!(score("gtl", "Goto: Go to Line") > score("gtl", "gentle"));
    }

    #[test]
    fn reports_best_positions() {
        let (_, pos) = fuzzy_match("ar", "src/app.rs").unwrap();
        // 'a' at the start of "app", 'r' at the start of "rs".
        assert_eq!(pos, vec![4, 8]);
    }
}
