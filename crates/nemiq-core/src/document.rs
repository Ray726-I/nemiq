//! Vim-ready document model.
//! `Buffer` owns text (ropey). All mutations go through `EditCommand`
//! so future Normal/Insert/Visual modes issue the same ops as the UI.
//! No GPUI dependency.

use ropey::Rope;
use unicode_segmentation::UnicodeSegmentation;

/// Modal state reserved for future vim integration. UI stays in Insert.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VimMode {
    #[default]
    Insert,
    Normal,
    Visual,
    Command,
}

/// Every text mutation the editor can perform. One command == one undo step.
#[derive(Debug, Clone)]
pub enum EditCommand {
    ReplaceRange {
        start: usize,
        end: usize,
        text: String,
    },
    Insert {
        offset: usize,
        text: String,
    },
    SmartReturn {
        offset: usize,
        sel_end: usize,
    },
    Indent {
        start_line: usize,
        end_line: usize,
        outdent: bool,
    },
    ToggleWrap {
        start: usize,
        end: usize,
        marker: String,
    },
}

#[derive(Debug, Clone, Default)]
pub struct Buffer {
    rope: Rope,
    undo: Vec<String>,
    redo: Vec<String>,
    pub vim_mode: VimMode,
}

impl Buffer {
    pub fn new(text: &str) -> Self {
        Self {
            rope: Rope::from_str(&text.replace("\r\n", "\n").replace('\r', "\n")),
            undo: vec![],
            redo: vec![],
            vim_mode: VimMode::Insert,
        }
    }
    pub fn text(&self) -> String {
        self.rope.to_string()
    }
    pub fn len_chars(&self) -> usize {
        self.rope.len_chars()
    }
    fn snapshot(&mut self) {
        self.undo.push(self.rope.to_string());
        self.redo.clear();
    }
    pub fn undo(&mut self) -> bool {
        if let Some(prev) = self.undo.pop() {
            self.redo.push(self.rope.to_string());
            self.rope = Rope::from_str(&prev);
            true
        } else {
            false
        }
    }
    pub fn redo(&mut self) -> bool {
        if let Some(next) = self.redo.pop() {
            self.undo.push(self.rope.to_string());
            self.rope = Rope::from_str(&next);
            true
        } else {
            false
        }
    }
    fn replace_char_range(&mut self, start: usize, end: usize, text: &str) {
        let len = self.len_chars();
        let s = start.min(len);
        let e = end.min(len).max(s);
        self.rope.remove(s..e);
        self.rope.insert(s, text);
    }
    /// Apply one undoable command, returns new caret char offset.
    pub fn apply(&mut self, cmd: EditCommand) -> usize {
        self.snapshot();
        match cmd {
            EditCommand::ReplaceRange { start, end, text } => {
                let t = text.replace("\r\n", "\n").replace('\r', "\n");
                self.replace_char_range(start, end, &t);
                start + t.chars().count()
            }
            EditCommand::Insert { offset, text } => {
                let t = text.replace("\r\n", "\n").replace('\r', "\n");
                let o = offset.min(self.len_chars());
                self.rope.insert(o, &t);
                o + t.chars().count()
            }
            EditCommand::SmartReturn { offset, sel_end: _ } => {
                // Paragraph break: an empty line takes one newline, otherwise a
                // blank line is inserted too.
                let txt = self.text();
                let chars: Vec<char> = txt.chars().collect();
                let o = offset.min(chars.len());
                // find line head/tail
                let mut ls = o;
                while ls > 0 && chars[ls - 1] != '\n' {
                    ls -= 1;
                }
                let mut le = o;
                while le < chars.len() && chars[le] != '\n' {
                    le += 1;
                }
                let head: String = chars[ls..o].iter().collect();
                let rest: String = chars[o..le].iter().collect();
                let ins = if head.trim().is_empty() && rest.trim().is_empty() {
                    "\n"
                } else {
                    "\n\n"
                };
                self.rope.insert(o, ins);
                o + ins.len()
            }
            EditCommand::Indent {
                start_line,
                end_line,
                outdent,
            } => {
                let lines: Vec<String> = self.rope.lines().map(|l| l.to_string()).collect();
                let mut out = lines.clone();
                for i in start_line..=end_line.min(lines.len().saturating_sub(1)) {
                    if outdent {
                        // remove up to 4 leading spaces
                        let mut rm = 0;
                        for c in out[i].chars().take(4) {
                            if c == ' ' {
                                rm += 1;
                            } else {
                                break;
                            }
                        }
                        out[i] = out[i][rm..].to_string();
                    } else {
                        out[i] = format!("    {}", out[i]);
                    }
                }
                self.rope = Rope::from_str(&out.join(""));
                self.len_chars()
            }
            EditCommand::ToggleWrap { start, end, marker } => {
                // Delegate to mutations::toggle_wrap pure function.
                let txt = self.text();
                let (ntxt, caret) = crate::mutations::toggle_wrap(&txt, start, end, &marker, None);
                self.rope = Rope::from_str(&ntxt);
                caret
            }
        }
    }
    /// Move one grapheme cluster, so combining marks and emoji stay whole.
    pub fn step_grapheme(&self, offset: usize, dir: i32) -> usize {
        let s = self.text();
        let gs = s
            .grapheme_indices(true)
            .map(|(b, _)| s[..b].chars().count());
        let mut bounds: Vec<usize> = gs.collect();
        bounds.push(s.chars().count());
        if dir > 0 {
            bounds
                .into_iter()
                .find(|&b| b > offset)
                .unwrap_or(s.chars().count())
        } else {
            bounds.into_iter().rev().find(|&b| b < offset).unwrap_or(0)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn undo_redo() {
        let mut b = Buffer::new("hello world");
        b.apply(EditCommand::ReplaceRange {
            start: 0,
            end: 5,
            text: "**hello**".into(),
        });
        assert_eq!(b.text(), "**hello** world");
        assert!(b.undo());
        assert_eq!(b.text(), "hello world");
        assert!(b.redo());
        assert_eq!(b.text(), "**hello** world");
    }
    #[test]
    fn grapheme_step() {
        let b = Buffer::new("a😀b");
        assert_eq!(b.step_grapheme(1, 1), 2); // ropey chars: emoji = 1 char
        assert_eq!(b.step_grapheme(2, -1), 1);
    }
}
