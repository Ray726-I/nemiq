//! Pure text mutations over a `String` plus char offsets.
//!
//! Kept free of GPUI so the view and any future modal editing share exactly the
//! same operations.

/// Normalize CRLF/CR -> LF, replace char range, compute caret.
pub fn replace_range(text: &str, start: usize, end: usize, replacement: &str) -> (String, usize) {
    let t: Vec<char> = text.chars().collect();
    let s = start.min(t.len());
    let e = end.min(t.len()).max(s);
    let ins: String = replacement.replace("\r\n", "\n").replace('\r', "\n");
    let mut out = String::new();
    out.extend(t[..s].iter());
    out.push_str(&ins);
    out.extend(t[e..].iter());
    let caret = s + ins.chars().count();
    (out, caret)
}

#[derive(Debug, Clone, Copy)]
pub struct EmphasisSpan {
    pub start: usize,
    pub end: usize,
    pub content_start: usize,
    pub content_end: usize,
    pub text_end: usize,
}

/// Toggle wrap with marker (**/*/~~). `span` = emphasisSpanAt caret when no selection.
/// Returns (new_text, new_caret).
pub fn toggle_wrap(
    text: &str,
    start: usize,
    end: usize,
    marker: &str,
    span: Option<EmphasisSpan>,
) -> (String, usize) {
    let t: Vec<char> = text.chars().collect();
    let mlen = marker.chars().count();
    let mark = marker.chars().next().unwrap_or('*');
    // Caret with no selection and a known emphasis span: unwrap it.
    if start == end {
        if let Some(sp) = span {
            if start == sp.end {
                return (text.to_string(), sp.text_end);
            } else if start >= sp.text_end {
                return (text.to_string(), sp.end);
            } else {
                let content: String = t[sp.content_start..sp.content_end].iter().collect();
                let caret_rel = start.saturating_sub(sp.content_start);
                let (ntxt, _) = replace_range(text, sp.start, sp.end, &content);
                // caret stays relative within unwrapped content
                let nc = (sp.start + caret_rel).min(ntxt.chars().count());
                // recompute via replace caret
                let _ = nc;
                let ins_len = content.chars().count();
                let nc2 = (sp.start + caret_rel.min(ins_len)).min(ntxt.chars().count());
                return (ntxt, nc2);
            }
        }
    }
    let run_from = |pos: isize, step: isize| -> usize {
        let mut run = 0usize;
        loop {
            let p = pos + run as isize * step;
            if p < 0 || p >= t.len() as isize {
                break;
            }
            if t[p as usize] != mark {
                break;
            }
            run += 1;
            // bound for selection variant
            if step > 0 && p >= end as isize {
                break;
            }
            if step < 0 && p < start as isize {
                break;
            }
        }
        run
    };
    let holds = |run: usize| run == mlen || (mark == '*' && run == 3);
    let has_marker = |part: &str| {
        let mut cur = 0usize;
        let pc: Vec<char> = part.chars().collect();
        while cur < pc.len() {
            if pc[cur] == mark {
                let mut r = 0;
                while cur + r < pc.len() && pc[cur + r] == mark {
                    r += 1;
                }
                if holds(r) {
                    return true;
                }
                cur += r;
            } else {
                cur += 1;
            }
        }
        false
    };
    let selected: String = t[start.min(t.len())..end.min(t.len()).max(start)]
        .iter()
        .collect();
    // outside markers?
    let before = if start >= mlen {
        run_from(start as isize - 1, -1)
    } else {
        0
    };
    // run_from counts bounded; approximate outside check:
    let mut b_run = 0usize;
    {
        let mut p = start as isize - 1;
        while p >= 0 && t[p as usize] == mark {
            b_run += 1;
            p -= 1;
        }
    }
    let mut a_run = 0usize;
    {
        let mut p = end;
        while p < t.len() && t[p] == mark {
            a_run += 1;
            p += 1;
        }
    }
    let _ = (before, run_from);
    if holds(b_run) && holds(a_run) && !has_marker(&selected) && start >= mlen {
        // unwrap outside
        let inner = selected;
        let (ntxt, _) = replace_range(text, start - mlen, end + mlen, &inner);
        let nc = start - mlen + inner.chars().count().min(end - start);
        // select inner
        let _ = nc;
        return (ntxt, start - mlen + (end - start));
    }
    // inside markers?
    let sch: Vec<char> = selected.chars().collect();
    let mut s_run = 0;
    while s_run < sch.len() && sch[s_run] == mark {
        s_run += 1;
    }
    let mut e_run = 0;
    while e_run < sch.len() && sch[sch.len() - 1 - e_run] == mark {
        e_run += 1;
    }
    if selected.chars().count() >= 2 * mlen
        && s_run >= mlen
        && e_run >= mlen
        && !has_marker(&sch[mlen..sch.len() - mlen].iter().collect::<String>())
    {
        let inner: String = sch[mlen..sch.len() - mlen].iter().collect();
        let (ntxt, _) = replace_range(text, start, end, &inner);
        return (ntxt, start + inner.chars().count());
    }
    // wrap
    let wrapped = format!("{marker}{selected}{marker}");
    let (ntxt, _) = replace_range(text, start, end, &wrapped);
    (ntxt, start + mlen + selected.chars().count())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn wrap_unwrap() {
        let (t, _) = replace_range("alpha omega", 5, 5, "one\r\ntwo");
        assert_eq!(t, "alphaone\ntwo omega");
        let (w, c) = toggle_wrap("a word b", 2, 6, "**", None);
        assert_eq!(w, "a **word** b");
        assert_eq!(c, 2 + 2 + 4);
        let (u, _) = toggle_wrap("a **word** b", 4, 8, "**", None);
        assert_eq!(u, "a word b");
    }
}
