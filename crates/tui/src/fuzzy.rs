//! Fuzzy matching for the command palette and the list pickers: every query
//! character must appear in order (case-insensitive). Ranked: prefix, then a
//! substring at a word start, then any substring, then a subsequence.

/// A match: lower `score` ranks first; `indices` are the matched character
/// positions in the candidate, for highlighting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FuzzyMatch {
    pub(crate) score: u8,
    pub(crate) indices: Vec<usize>,
}

pub(crate) fn fuzzy_match(candidate: &str, query: &str) -> Option<FuzzyMatch> {
    let query: Vec<char> = query.chars().flat_map(char::to_lowercase).collect();
    if query.is_empty() {
        return Some(FuzzyMatch {
            score: 0,
            indices: Vec::new(),
        });
    }
    let chars: Vec<char> = candidate
        .chars()
        .map(|ch| ch.to_lowercase().next().unwrap_or(ch))
        .collect();
    let contiguous_at = |start: usize| -> bool {
        start + query.len() <= chars.len() && chars[start..start + query.len()] == query[..]
    };
    let range = |start: usize| (start..start + query.len()).collect::<Vec<_>>();
    if contiguous_at(0) {
        return Some(FuzzyMatch {
            score: 0,
            indices: range(0),
        });
    }
    let word_start = |index: usize| {
        index == 0 || matches!(chars[index - 1], '/' | '-' | '_' | ' ' | '.' | ':' | '@')
    };
    if let Some(start) = (1..chars.len()).find(|&i| word_start(i) && contiguous_at(i)) {
        return Some(FuzzyMatch {
            score: 1,
            indices: range(start),
        });
    }
    if let Some(start) = (1..chars.len()).find(|&i| contiguous_at(i)) {
        return Some(FuzzyMatch {
            score: 2,
            indices: range(start),
        });
    }
    let mut indices = Vec::with_capacity(query.len());
    let mut next = 0usize;
    for needed in &query {
        let found = (next..chars.len()).find(|&i| chars[i] == *needed)?;
        indices.push(found);
        next = found + 1;
    }
    Some(FuzzyMatch { score: 3, indices })
}

/// Filters and ranks `items` by `key`, keeping the original order on ties
/// of score and length.
pub(crate) fn rank<'a, T>(
    items: &'a [T],
    query: &str,
    key: impl Fn(&T) -> &str,
) -> Vec<(&'a T, FuzzyMatch)> {
    let mut matched = items
        .iter()
        .enumerate()
        .filter_map(|(index, item)| {
            let text = key(item);
            fuzzy_match(text, query).map(|m| (m.score, text.chars().count(), index, item, m))
        })
        .collect::<Vec<_>>();
    if !query.is_empty() {
        matched.sort_by_key(|(score, len, index, _, _)| (*score, *len, *index));
    }
    matched
        .into_iter()
        .map(|(_, _, _, item, m)| (item, m))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefix_ranks_before_word_start_and_subsequence() {
        let commands = ["tree", "resume", "approve", "rewind", "permissions"];
        let ranked: Vec<&str> = rank(&commands, "re", |c| c)
            .into_iter()
            .map(|(c, _)| *c)
            .collect();
        assert_eq!(ranked, vec!["resume", "rewind", "tree", "approve"]);
    }

    #[test]
    fn match_reports_highlight_indices() {
        assert_eq!(fuzzy_match("model", "mo").unwrap().indices, vec![0, 1]);
        let sub = fuzzy_match("approve", "re").unwrap();
        assert_eq!(sub.score, 3);
        assert_eq!(sub.indices, vec![3, 6]);
        assert!(fuzzy_match("model", "xyz").is_none());
    }

    #[test]
    fn matching_ignores_case_and_handles_wide_chars() {
        assert!(fuzzy_match("Claude-Opus", "opus").is_some());
        assert_eq!(
            fuzzy_match("修复 宽字符", "宽字").unwrap().indices,
            vec![3, 4]
        );
    }

    #[test]
    fn empty_query_keeps_everything_in_order() {
        let items = ["b", "a"];
        let ranked: Vec<&str> = rank(&items, "", |c| c)
            .into_iter()
            .map(|(c, _)| *c)
            .collect();
        assert_eq!(ranked, vec!["b", "a"]);
    }
}
