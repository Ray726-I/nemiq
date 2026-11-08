//! Case- and diacritic-insensitive search.
//!
//! A plain Latin or Greek letter in the query also matches its accented forms;
//! an accented letter matches only itself (ş and ș are interchangeable, but s
//! is not). Offsets are char indices.

use unicode_normalization::UnicodeNormalization;

fn comma_below(c: char) -> char {
    match c {
        '\u{015F}' => '\u{0219}',
        '\u{0163}' => '\u{021B}',
        _ => c,
    }
}

fn plain_map(c: char) -> char {
    match c {
        'ł' | 'Ł' => 'l',
        'đ' | 'Đ' => 'd',
        'ø' | 'Ø' => 'o',
        'ħ' | 'Ħ' => 'h',
        'ı' => 'i',
        'ß' => 'ß', // no expansion, so "strasse" does not match "Straße"
        _ => c,
    }
}

fn is_latin_or_greek(c: char) -> bool {
    // Cheap blocks: Basic Latin, Latin-1/Ext, Greek.
    matches!(c, '\u{0000}'..='\u{024F}' | '\u{0370}'..='\u{03FF}' | '\u{1E00}'..='\u{1EFF}')
}

/// Fold one char: lowercase, strip canonical diacritics for Latin/Greek.
fn fold_char(c: char) -> char {
    let lower = c.to_lowercase().next().unwrap_or(c);
    let mapped = plain_map(lower);
    if !is_latin_or_greek(mapped) {
        return comma_below(mapped);
    }
    // NFD, take base (first non-mark), lowercase already done.
    let mut base = mapped;
    for d in mapped.to_string().nfd() {
        if !unicode_normalization::char::is_combining_mark(d) {
            base = d.to_lowercase().next().unwrap_or(d);
            break;
        }
    }
    comma_below(plain_map(base))
}

fn fold_str(s: &str) -> Vec<char> {
    // Per-char fold keeps 1 char per char (CJK/Hangul untouched, surrogates = Rust chars).
    s.chars().map(fold_char).collect()
}

/// Where the query occurs in the text, as char indices. The fold pass keeps one
/// char per char, so byte offsets from the regex can be mapped directly.
pub fn find_matches(text: &str, query: &str) -> Vec<usize> {
    let t: Vec<char> = text.chars().collect();
    let q: Vec<char> = query.chars().collect();
    if q.is_empty() || t.len() < q.len() {
        return vec![];
    }
    let ft = fold_str(text);
    let fq = fold_str(query);
    // Query positions where folded != lowercased => must match exactly.
    let mut exact: Vec<(usize, char)> = vec![];
    for (i, &qc) in q.iter().enumerate() {
        let lower = qc.to_lowercase().next().unwrap_or(qc);
        if fq[i] != lower {
            exact.push((i, comma_below(lower)));
        }
    }
    let mut out = vec![];
    let mut from = 0usize;
    while from + q.len() <= t.len() {
        if ft[from..from + q.len()] == fq[..] {
            let ok = exact.iter().all(|(di, want)| {
                comma_below(t[from + di].to_lowercase().next().unwrap_or(t[from + di])) == *want
            });
            if ok {
                out.push(from);
                from += q.len();
                continue;
            }
        }
        from += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn accented_letters() {
        assert_eq!(find_matches("Țară și ŞTIRI", "tara"), vec![0]);
        assert_eq!(find_matches("Țară și ŞTIRI", "si"), vec![5]);
        assert_eq!(find_matches("Țară și ŞTIRI", "stiri"), vec![8]);
        assert_eq!(find_matches("în câmp", "in camp"), vec![0]);
        assert_eq!(find_matches("élève, Łódź, Straße", "eleve"), vec![0]);
        assert_eq!(find_matches("élève, Łódź, Straße", "lodz"), vec![7]);
        assert_eq!(find_matches("tara țară", "țară"), vec![5]);
        assert_eq!(find_matches("tara Ţară", "țară"), vec![5]);
        assert_eq!(find_matches("mama mamă", "mamă"), vec![5]);
        assert_eq!(find_matches("й и", "и"), vec![2]);
        assert_eq!(find_matches("が か", "か"), vec![2]);
        assert_eq!(find_matches("한 하", "하"), vec![2]);
        assert_eq!(find_matches("aaa", "aa"), vec![0]);
        assert_eq!(find_matches("tțara", "țara"), vec![1]);
    }
}
