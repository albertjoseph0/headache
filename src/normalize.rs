//! Turning what a player typed into something we can count.
//!
//! Exact and near-exact matches are handled here in code. Only answers that survive
//! this step need Jev to decide whether they name the same thing as an existing answer.

use unicode_normalization::UnicodeNormalization;

use crate::config::MAX_ANSWER_CHARS;

/// What we show back to players: trimmed, single-spaced, length-capped.
pub fn display(raw: &str) -> String {
    let collapsed = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    let trimmed =
        collapsed.trim_matches(|c: char| c.is_ascii_punctuation() && c != '\'' && c != '&');
    trimmed.chars().take(MAX_ANSWER_CHARS).collect()
}

/// Matching key: lowercase, accents and punctuation removed, leading article dropped.
/// "The Mona Lisa!" and "mona lisa" share a key.
pub fn key(raw: &str) -> String {
    let lowered: String = display(raw)
        .nfkd()
        .filter(|c| !unicode_normalization::char::is_combining_mark(*c))
        .collect::<String>()
        .to_lowercase()
        .replace('&', " and ")
        .replace('\'', "");
    let spaced: String = lowered
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect();
    let words: Vec<&str> = spaced.split_whitespace().collect();
    let words = match words.as_slice() {
        [first, rest @ ..] if !rest.is_empty() && matches!(*first, "a" | "an" | "the") => rest,
        all => all,
    };
    words.join(" ")
}

/// Singular/plural spellings of a key. Used only to look up answers already in the
/// school, so a wrong guess here can only match an answer that exists.
pub fn variants(key: &str) -> Vec<String> {
    let mut out = vec![format!("{key}s"), format!("{key}es")];
    if let Some(stem) = key.strip_suffix("ies") {
        out.push(format!("{stem}y"));
    }
    if let Some(stem) = key.strip_suffix("es") {
        out.push(stem.to_string());
    }
    if let Some(stem) = key.strip_suffix('s') {
        out.push(stem.to_string());
    }
    if let Some(stem) = key.strip_suffix('y') {
        out.push(format!("{stem}ies"));
    }
    out.retain(|v| !v.is_empty() && v != key);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_ignore_case_punctuation_accents_and_articles() {
        assert_eq!(key("  The Mona   Lisa! "), "mona lisa");
        assert_eq!(key("Crème brûlée"), "creme brulee");
        assert_eq!(key("Jell-O"), "jell o");
        assert_eq!(key("Mac & Cheese"), "mac and cheese");
        assert_eq!(key("McDonald's"), "mcdonalds");
        assert_eq!(key("The"), "the");
        assert_eq!(key("A"), "a");
    }

    #[test]
    fn display_is_tidy_and_capped() {
        assert_eq!(display("  hello   world!!! "), "hello world");
        assert_eq!(display("rock 'n' roll"), "rock 'n' roll");
        assert_eq!(display(&"x".repeat(200)).chars().count(), MAX_ANSWER_CHARS);
    }

    #[test]
    fn variants_cover_common_plurals() {
        assert!(variants("egg").contains(&"eggs".to_string()));
        assert!(variants("eggs").contains(&"egg".to_string()));
        assert!(variants("cherries").contains(&"cherry".to_string()));
        assert!(variants("cherry").contains(&"cherries".to_string()));
        assert!(variants("tomatoes").contains(&"tomato".to_string()));
    }
}
