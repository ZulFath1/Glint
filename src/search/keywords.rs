//! Matching the search text against keyword lists (system actions,
//! Settings pages, Glint commands, music controls).
//!
//! For each keyword ("empty recycle bin"):
//! - the text is the whole keyword: exact (best),
//! - the keyword starts with the text ("empty re"): prefix, 2+ letters,
//! - a later word starts with the text ("bin", "recycle"): 3+ letters.
//!
//! One letter never matches (typing "l" shouldn't offer to lock the PC).

use super::score;

/// How well `query` (lowercase, single spaces) matches the best of
/// `keywords` (lowercase). None = not at all.
pub fn score<S: AsRef<str>>(query: &str, keywords: &[S]) -> Option<u16> {
    let q = query;
    if q.chars().count() < 2 {
        return None;
    }
    keywords
        .iter()
        .filter_map(|k| {
            let k = k.as_ref();
            if k == q {
                Some(score::KEYWORD_EXACT)
            } else if k.starts_with(q) {
                Some(score::KEYWORD_PREFIX)
            } else if q.chars().count() >= 3
                && k.match_indices(' ').any(|(i, _)| k[i + 1..].starts_with(q))
            {
                Some(score::KEYWORD_WORD)
            } else {
                None
            }
        })
        .max()
}

/// The `max` best matches among `entries` (by `keywords_of`), best first;
/// equal scores keep the table order.
pub fn best<'a, T>(
    entries: &'a [T],
    query: &str,
    max: usize,
    keywords_of: impl Fn(&T) -> &[&'static str],
) -> Vec<(u16, &'a T)> {
    let mut hits: Vec<(u16, &T)> = entries
        .iter()
        .filter_map(|e| score(query, keywords_of(e)).map(|s| (s, e)))
        .collect();
    hits.sort_by_key(|h| std::cmp::Reverse(h.0));
    hits.truncate(max);
    hits
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_prefix_word() {
        let k = ["empty recycle bin", "trash"];
        assert_eq!(score("trash", &k), Some(score::KEYWORD_EXACT));
        assert_eq!(score("empty re", &k), Some(score::KEYWORD_PREFIX));
        assert_eq!(score("tr", &k), Some(score::KEYWORD_PREFIX));
        assert_eq!(score("recycle", &k), Some(score::KEYWORD_WORD));
        assert_eq!(score("bin", &k), Some(score::KEYWORD_WORD));
        // Too short, or inside a word.
        assert_eq!(score("t", &k), None);
        assert_eq!(score("bi", &k), None);
        assert_eq!(score("ycle", &k), None);
        assert_eq!(score("trashcan", &k), None);
        assert_eq!(score("", &k), None);
    }

    #[test]
    fn best_keeps_order_and_limit() {
        let entries = [("a", ["sound"]), ("b", ["sound settings"]), ("c", ["so"])];
        let got: Vec<&str> = best(&entries, "so", 2, |e| &e.1[..])
            .into_iter()
            .map(|(_, e)| e.0)
            .collect();
        // "so" is exact for c, a prefix for a and b.
        assert_eq!(got, ["c", "a"]);
    }
}
