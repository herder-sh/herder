//! Fuzzy filtering for the pickers: what a search box keeps of a list, best match first.
//!
//! Matching is [`nucleo_matcher`]'s (Helix's matcher): the query's characters in order, case
//! ignored unless the query has a capital, words of the query matched separately.

use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};

/// The indices of `items` whose text matches `query`, best first; every index, in order, for
/// an empty query.
pub fn filter<T>(query: &str, items: &[T], text: impl Fn(&T) -> String) -> Vec<usize> {
    let query = query.trim();
    if query.is_empty() {
        return (0..items.len()).collect();
    }
    let pattern = Pattern::parse(query, CaseMatching::Smart, Normalization::Smart);
    let mut matcher = Matcher::new(Config::DEFAULT);
    let mut buf = Vec::new();
    let mut scored: Vec<(usize, u32)> = items
        .iter()
        .enumerate()
        .filter_map(|(at, item)| {
            let text = text(item);
            let score = pattern.score(Utf32Str::new(&text, &mut buf), &mut matcher)?;
            Some((at, score))
        })
        .collect();
    // Stable: equal scores keep the list's order.
    scored.sort_by_key(|&(_, score)| std::cmp::Reverse(score));
    scored.into_iter().map(|(at, _)| at).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_query_keeps_everything_in_order() {
        assert_eq!(filter("  ", &["b", "a"], |s| (*s).to_owned()), [0, 1]);
    }

    #[test]
    fn letters_in_order_match_and_the_closest_comes_first() {
        let items = ["new session", "switch", "inbox", "stop the turn"];
        let text = |s: &&str| (*s).to_owned();
        assert_eq!(filter("sw", &items, text), [1]);
        // `st` matches `stop` outright and `switch` and `new session` only loosely.
        assert_eq!(filter("st", &items, text).first(), Some(&3));
        assert!(filter("xyz", &items, text).is_empty());
        // Case is ignored for a lowercase query.
        assert_eq!(filter("INB", &["inbox"], text), Vec::<usize>::new());
        assert_eq!(filter("inb", &["Inbox"], text), [0]);
    }
}
