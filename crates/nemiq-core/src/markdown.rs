//! Inline markdown scanning: emphasis, strikethrough, links, code spans and
//! fenced blocks.
//!
//! Every offset is a char index; the UI converts to bytes when it hands spans
//! to the renderer.

use fancy_regex::Regex as FRegex;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InlineKind {
    Bold,
    Italic,
    Strikethrough,
    Link,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub start: usize,
    pub length: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InlineMarkup {
    pub kind: InlineKind,
    pub content: Span,
    pub markers: [Span; 2],
    pub destination: Option<Span>,
}

fn chars_of(s: &str) -> Vec<char> {
    s.chars().collect()
}
fn byte_to_char(b: usize, s: &str) -> usize {
    s[..b.min(s.len())].chars().count()
}
#[allow(dead_code)]
fn char_to_byte(c: usize, s: &str) -> usize {
    s.chars().take(c).map(|x| x.len_utf8()).sum()
}

/// Compile each pattern once. `fancy_regex` compilation (especially with
/// lookaround/backreferences) costs milliseconds, so doing it per line made
/// layout O(lines x ms).
macro_rules! cached_regex {
    ($name:ident, $pat:expr) => {
        fn $name() -> &'static FRegex {
            static RE: std::sync::OnceLock<FRegex> = std::sync::OnceLock::new();
            RE.get_or_init(|| FRegex::new($pat).unwrap())
        }
    };
}

cached_regex!(thematic_re, r"^\s{0,3}([-*_])(?:\s*\1){2,}\s*$");
cached_regex!(opening_fence_re, r"^ {0,3}(?:(`{3,})[^`]*|(~{3,}).*)$");
cached_regex!(closing_fence_re, r"^ {0,3}(`{3,}|~{3,})\s*$");
cached_regex!(code_span_re, r"`([^`]+)`");
cached_regex!(
    triple_re,
    r"(?<!\*)\*\*\*(?![\s*])(.*?[^ \t\r\n\f\v*])\*\*\*(?!\*)"
);
cached_regex!(bold_re, r"\*\*(.+?)\*\*|(?<!\w)__(.+?)__(?!\w)");
cached_regex!(
    italic_re,
    r"(?<!\*)\*(?![\s*])([^*\n]*[^ \t\r\n\f\v*])\*(?!\*)|(?<!\w)_([^_\n]+)_(?!\w)"
);
cached_regex!(strike_re, r"(?<!~)~~(?![\s~])(.*?[^ \t\r\n\f\v~])~~(?!~)");
cached_regex!(link_re, r"\[([^\]]+)\]\(((?:\\.|\(?:[^()]*\)|[^()])+)\)");

pub fn is_thematic_break(text: &str) -> bool {
    // Cheap reject: prose almost never consists solely of - * _ and spaces.
    if !text
        .chars()
        .all(|c| c == ' ' || c == '\t' || c == '-' || c == '*' || c == '_')
    {
        return false;
    }
    let r = thematic_re();
    r.is_match(text).unwrap_or(false)
}

pub fn opening_fence_length(text: &str) -> usize {
    // Cheap reject before touching the regex engine.
    let head = text.trim_start_matches(' ');
    if text.len() - head.len() > 3 || !(head.starts_with("```") || head.starts_with("~~~")) {
        return 0;
    }
    let r = opening_fence_re();
    if let Ok(Some(caps)) = r.captures(text) {
        for i in 1..=2 {
            if let Some(gm) = caps.get(i) {
                let chcount = gm.as_str().chars().count();
                if chcount >= 3 {
                    return chcount;
                }
            }
        }
    }
    0
}

pub fn closes_fence(text: &str, opening_len: usize, marker: char) -> bool {
    let head = text.trim_start_matches(' ');
    if text.len() - head.len() > 3 || !(head.starts_with("```") || head.starts_with("~~~")) {
        return false;
    }
    let r = closing_fence_re();
    if let Ok(Some(caps)) = r.captures(text) {
        if let Some(g) = caps.get(1) {
            let s = g.as_str();
            if s.chars().count() >= opening_len && s.starts_with(marker) {
                return true;
            }
        }
    }
    false
}

fn escaped_at(chars: &[char], pos: usize) -> bool {
    let mut bs = 0;
    let mut i = pos as isize - 1;
    while i >= 0 && chars[i as usize] == '\\' {
        bs += 1;
        i -= 1;
    }
    bs % 2 == 1
}

/// Inline code spans (backticks included), char-indexed.
pub fn code_spans(text: &str) -> Vec<Span> {
    let chars = chars_of(text);
    let mut out = vec![];
    if !text.contains('`') {
        return out;
    }
    let code_re = code_span_re();
    let mut from_byte = 0usize;
    while from_byte < text.len() {
        let Some(rel) = text[from_byte..].find('`') else {
            break;
        };
        let abs_byte = from_byte + rel;
        let abs_char = byte_to_char(abs_byte, text);
        if escaped_at(&chars, abs_char) {
            from_byte = abs_byte + 1;
            continue;
        }
        let substr = &text[abs_byte..];
        match code_re.captures_from_pos(substr, 0) {
            Ok(Some(caps)) => {
                if let Some(m0) = caps.get(0) {
                    if m0.start() == 0 {
                        let len_chars = m0.as_str().chars().count();
                        out.push(Span {
                            start: abs_char,
                            length: len_chars,
                        });
                        from_byte = abs_byte + m0.as_str().len();
                        continue;
                    }
                }
                from_byte = abs_byte + 1;
            }
            _ => from_byte = abs_byte + 1,
        }
    }
    out
}

struct Candidate {
    kind: InlineKind,
    content: Span,
    markers: [Span; 2],
    dest: Option<Span>,
}

fn try_append(
    markup: &mut Vec<InlineMarkup>,
    code: &[Span],
    chars: &[char],
    item: Candidate,
) -> bool {
    let in_code = |s: usize, len: usize| {
        code.iter()
            .any(|c| s < c.start + c.length && c.start < s + len)
    };
    for m in item.markers {
        if escaped_at(chars, m.start) {
            return false;
        }
        if in_code(m.start, m.length) {
            return false;
        }
    }
    markup.push(InlineMarkup {
        kind: item.kind,
        content: item.content,
        markers: item.markers,
        destination: item.dest,
    });
    true
}

pub fn inline_markup(text: &str) -> Vec<InlineMarkup> {
    let mut markup: Vec<InlineMarkup> = vec![];
    if !(text.contains('*') || text.contains('_') || text.contains('[') || text.contains('~')) {
        return markup;
    }
    if is_thematic_break(text) {
        return markup;
    }
    let code = code_spans(text);
    let chars = chars_of(text);

    // Track claimed chars for *** runs (mask so later patterns don't split).
    let mut claimed = vec![false; chars.len()];
    let bi_re = triple_re();
    // Collect via captures_iter manually (need byte->char conversion).
    if let Ok(caps_iter) = bi_re.captures_iter(text).collect::<Result<Vec<_>, _>>() {
        for caps in caps_iter {
            let whole = caps.get(0).unwrap();
            let inner = caps.get(1).unwrap();
            let ws = byte_to_char(whole.start(), text);
            let wl = whole.as_str().chars().count();
            let cs = byte_to_char(inner.start(), text);
            let cl = inner.as_str().chars().count();
            let end = ws + wl;
            if try_append(
                &mut markup,
                &code,
                &chars,
                Candidate {
                    kind: InlineKind::Bold,
                    content: Span {
                        start: ws + 2,
                        length: wl - 4,
                    },
                    markers: [
                        Span {
                            start: ws,
                            length: 2,
                        },
                        Span {
                            start: end - 2,
                            length: 2,
                        },
                    ],
                    dest: None,
                },
            ) {
                markup.push(InlineMarkup {
                    kind: InlineKind::Italic,
                    content: Span {
                        start: cs,
                        length: cl,
                    },
                    markers: [
                        Span {
                            start: ws + 2,
                            length: 1,
                        },
                        Span {
                            start: end - 3,
                            length: 1,
                        },
                    ],
                    destination: None,
                });
                for i in [ws, ws + 1, ws + 2, end - 3, end - 2, end - 1] {
                    if i < claimed.len() {
                        claimed[i] = true;
                    }
                }
            }
        }
    }
    // Build unclaimed string: replace claimed with object-replacement char.
    let unclaimed: String = chars
        .iter()
        .enumerate()
        .map(|(i, &c)| if claimed[i] { '\u{FFFC}' } else { c })
        .collect();

    macro_rules! collect {
        ($re:expr, $kind:expr, $mlen:expr, $groups:expr) => {{
            let r = $re;
            if let Ok(v) = r.captures_iter(&unclaimed).collect::<Result<Vec<_>, _>>() {
                for caps in v {
                    let whole = caps.get(0).unwrap();
                    let ws = byte_to_char(whole.start(), &unclaimed);
                    let wl = whole.as_str().chars().count();
                    // find which group matched
                    let mut cg: Option<(usize, usize)> = None;
                    for g in $groups {
                        if let Some(m) = caps.get(g) {
                            cg = Some((
                                byte_to_char(m.start(), &unclaimed),
                                m.as_str().chars().count(),
                            ));
                            break;
                        }
                    }
                    if let Some((cs, cl)) = cg {
                        let (m0, m1) = $mlen(ws, wl);
                        try_append(
                            &mut markup,
                            &code,
                            &chars,
                            Candidate {
                                kind: $kind,
                                content: Span {
                                    start: cs,
                                    length: cl,
                                },
                                markers: [m0, m1],
                                dest: None,
                            },
                        );
                    }
                }
            }
        }};
    }
    collect!(
        bold_re(),
        InlineKind::Bold,
        |ws: usize, wl: usize| (
            Span {
                start: ws,
                length: 2
            },
            Span {
                start: ws + wl - 2,
                length: 2
            }
        ),
        [1usize, 2usize]
    );
    collect!(
        italic_re(),
        InlineKind::Italic,
        |ws: usize, wl: usize| (
            Span {
                start: ws,
                length: 1
            },
            Span {
                start: ws + wl - 1,
                length: 1
            }
        ),
        [1usize, 2usize]
    );
    collect!(
        strike_re(),
        InlineKind::Strikethrough,
        |ws: usize, wl: usize| (
            Span {
                start: ws,
                length: 2
            },
            Span {
                start: ws + wl - 2,
                length: 2
            }
        ),
        [1usize]
    );
    // Links: capture text + destination.
    {
        let r = link_re();
        if let Ok(v) = r.captures_iter(&unclaimed).collect::<Result<Vec<_>, _>>() {
            for caps in v {
                let whole = caps.get(0).unwrap();
                let c = caps.get(1).unwrap();
                let d = caps.get(2).unwrap();
                let ws = byte_to_char(whole.start(), &unclaimed);
                let wl = whole.as_str().chars().count();
                let cs = byte_to_char(c.start(), &unclaimed);
                let cl = c.as_str().chars().count();
                let ds = byte_to_char(d.start(), &unclaimed);
                let dl = d.as_str().chars().count();
                let ce = cs + cl;
                try_append(
                    &mut markup,
                    &code,
                    &chars,
                    Candidate {
                        kind: InlineKind::Link,
                        content: Span {
                            start: cs,
                            length: cl,
                        },
                        markers: [
                            Span {
                                start: ws,
                                length: 1,
                            },
                            Span {
                                start: ce,
                                length: ws + wl - ce,
                            },
                        ],
                        dest: Some(Span {
                            start: ds,
                            length: dl,
                        }),
                    },
                );
            }
        }
    }
    markup
}

/// URL under caret offset (char index) within a single line.
pub fn link_url_at(text: &str, position: usize) -> String {
    let chars = chars_of(text);
    if position >= chars.len() {
        return String::new();
    }
    for item in inline_markup(text) {
        if item.kind != InlineKind::Link {
            continue;
        }
        if position < item.content.start || position >= item.content.start + item.content.length {
            continue;
        }
        if let Some(d) = item.destination {
            let dest: String = chars[d.start..d.start + d.length].iter().collect();
            // Unescape punctuation: \( -> (
            let mut out = String::new();
            let mut it = dest.chars().peekable();
            while let Some(c) = it.next() {
                if c == '\\' {
                    if let Some(&n) = it.peek() {
                        if n.is_ascii_punctuation() {
                            out.push(n);
                            it.next();
                            continue;
                        }
                    }
                    out.push(c);
                } else {
                    out.push(c);
                }
            }
            return out;
        }
    }
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn strike_and_code() {
        let m = inline_markup("a ~~gone~~ b");
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].kind, InlineKind::Strikethrough);
        assert_eq!(m[0].content.start, 4);
        assert_eq!(m[0].content.length, 4);
        assert!(inline_markup("`~~x~~`").is_empty());
        assert!(inline_markup("a ~~ b ~~ c").is_empty());
        assert!(inline_markup("~~~x~~~").is_empty());
    }
    #[test]
    fn links() {
        let t = "Read [site](https://example.com/docs) now";
        let s = t.chars().collect::<Vec<_>>().iter().collect::<String>();
        let start = t.chars().collect::<Vec<_>>();
        let idx = start.iter().collect::<String>().find("site").unwrap();
        let _ = (s, idx);
        // char index of "site"
        let cidx: usize = t[..t.find("site").unwrap()].chars().count();
        assert_eq!(link_url_at(t, cidx), "https://example.com/docs");
        assert_eq!(link_url_at(t, cidx + 3), "https://example.com/docs");
        assert!(link_url_at(t, cidx - 1).is_empty());
    }
    #[test]
    fn fences() {
        assert_eq!(opening_fence_length("```ruby"), 3);
        assert_eq!(opening_fence_length("    ```"), 0);
        assert!(!closes_fence("``` text", 3, '`'));
        assert!(closes_fence("```", 3, '`'));
    }
    #[test]
    fn thematic() {
        assert!(is_thematic_break("***"));
        assert!(!is_thematic_break("**bold**"));
    }
}
