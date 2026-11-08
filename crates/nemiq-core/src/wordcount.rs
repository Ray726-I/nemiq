//! Word counting for the status line. Runs of letters and numbers form a word;
//! CJK characters (Han, Hiragana, Katakana) count one by one.

use fancy_regex::Regex;
use std::sync::OnceLock;

fn cjk_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"(?=[\p{L}\p{N}])[\p{Han}\p{Hiragana}\p{Katakana}]").unwrap())
}

fn word_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"[\p{L}\p{N}]+(?:['-][\p{L}\p{N}]+)*").unwrap())
}

/// Count words. CJK chars count individually, rest space-delimited.
pub fn count_words(text: &str) -> usize {
    let cjk = cjk_re();
    let count_cjk = cjk.find_iter(text).filter_map(|r| r.ok()).count();
    // replace CJK with space for word pass
    let mut non_cjk = text.to_string();
    // collect match ranges (byte) then replace
    if let Ok(matches) = cjk.find_iter(text).collect::<Result<Vec<_>, _>>() {
        // replace from end to keep indices valid
        let mut s = text.to_string();
        for m in matches.iter().rev() {
            s.replace_range(m.start()..m.end(), " ");
        }
        non_cjk = s;
    }
    let w = word_re();
    let word_count = w.find_iter(&non_cjk).filter_map(|r| r.ok()).count();
    count_cjk + word_count
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mixed_scripts() {
        assert_eq!(count_words("one two-three don't 42"), 4);
        assert_eq!(count_words("你好世界"), 4);
        assert_eq!(count_words("你好 世界"), 4);
        assert_eq!(count_words("中文 iPad AI"), 4);
        assert_eq!(count_words("foo中bar"), 3);
        assert_eq!(count_words("日本語テスト"), 6);
        assert_eq!(count_words("你好，世界。"), 4);
        assert_eq!(count_words("コーヒー"), 4);
        assert_eq!(count_words("か\u{3099}"), 1);
        assert_eq!(count_words("𠀀𠀁 foo"), 3);
        assert_eq!(count_words("안녕하세요 세계"), 2);
        assert_eq!(count_words(""), 0);
    }
}
