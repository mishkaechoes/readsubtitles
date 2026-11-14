//! Text normalisation and comparison for subtitle reads.

/// Collapse an OCR read into a single speakable line.
///
/// Vision returns one string per detected text region, which for a two-line
/// subtitle means two strings that are really one sentence. Joining them with a
/// space and collapsing whitespace gives the synthesiser something it can read
/// with sensible prosody instead of a hard stop mid-clause.
#[must_use]
pub fn normalize(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for word in raw.split_whitespace() {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(word);
    }
    out
}

/// Key used when comparing two reads for "is this the same subtitle".
///
/// Case and punctuation are dropped because OCR is least reliable on exactly
/// those, and they never distinguish one subtitle from another.
fn compare_key(s: &str) -> Vec<char> {
    s.chars()
        .filter(|c| c.is_alphanumeric() || c.is_whitespace())
        .flat_map(char::to_lowercase)
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .collect()
}

/// Similarity in `0.0..=1.0`, as 1 minus normalised edit distance.
///
/// Used to decide whether a new read is a fresh subtitle or the previous one
/// recognised slightly differently as it faded.
#[must_use]
pub fn similarity(a: &str, b: &str) -> f64 {
    let (a, b) = (compare_key(a), compare_key(b));
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    let longest = a.len().max(b.len());
    if longest == 0 {
        return 1.0;
    }
    1.0 - (levenshtein(&a, &b) as f64 / longest as f64)
}

/// Standard two-row Levenshtein distance.
fn levenshtein(a: &[char], b: &[char]) -> usize {
    if a.is_empty() {
        return b.len();
    }
    if b.is_empty() {
        return a.len();
    }

    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0usize; b.len() + 1];

    for (i, ca) in a.iter().enumerate() {
        cur[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            cur[j + 1] = (prev[j + 1] + 1).min(cur[j] + 1).min(prev[j] + cost);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_joins_lines_and_collapses_whitespace() {
        assert_eq!(
            normalize("  I don't know\n  what you mean. "),
            "I don't know what you mean."
        );
    }

    #[test]
    fn identical_text_is_fully_similar() {
        assert_eq!(similarity("Hello there.", "Hello there."), 1.0);
    }

    #[test]
    fn punctuation_and_case_do_not_count_as_difference() {
        assert_eq!(similarity("Hello, there!", "hello there"), 1.0);
    }

    #[test]
    fn one_character_misread_stays_above_the_dedupe_threshold() {
        let s = similarity("It was a six-hour surgery.", "It was a six-hour surgerv.");
        assert!(s > 0.9, "expected near-identical, got {s}");
    }

    #[test]
    fn different_sentences_are_well_below_the_threshold() {
        let s = similarity("It was a six-hour surgery.", "And how did it go?");
        assert!(s < 0.5, "expected clearly different, got {s}");
    }

    #[test]
    fn empty_against_text_is_zero() {
        assert_eq!(similarity("", "anything"), 0.0);
    }
}
