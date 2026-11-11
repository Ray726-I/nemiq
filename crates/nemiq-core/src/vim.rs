//! Modal input: a vim-style state machine independent of the UI.
//!
//! The view feeds it the document text, the caret and one key, and gets back a
//! command it can apply (`Move`, `Edit`, `Yank`, …). Keeping key parsing here
//! means every operator/motion combination is testable without a window.

use crate::document::VimMode;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Char(char),
    Esc,
    Enter,
    Backspace,
    Space,
    Tab,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    Delete,
    PageUp,
    PageDown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    None,
    Move {
        pos: usize,
        extend: bool,
    },
    /// Move by display rows; the view owns wrapping, so it resolves this.
    MoveVertical {
        dir: i32,
        count: usize,
    },
    Mode {
        mode: VimMode,
        pos: usize,
        anchor: usize,
    },
    /// Replace `start..end` with `text`, then set caret and anchor.
    Edit {
        start: usize,
        end: usize,
        text: String,
        pos: usize,
        anchor: usize,
    },
    /// Send the range to the register and the system clipboard.
    Yank {
        start: usize,
        end: usize,
        linewise: bool,
    },
    /// `yy`/`Y`: yank whole displayed rows. The view resolves the range,
    /// because one source line can be several rows (and vice versa) when the
    /// text is reflowed for display.
    YankRow {
        count: usize,
    },
    Search {
        forward: bool,
    },
    SearchNext {
        forward: bool,
    },
    CommandLine,
    Save,
    Quit {
        force: bool,
    },
    SaveQuit,
    Undo,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Motion {
    Left,
    Right,
    Up,
    Down,
    WordForward,
    WordBack,
    WordEnd,
    LineStart,
    FirstNonBlank,
    LineEnd,
    DocEnd,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    Delete,
    Change,
    Yank,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Pending {
    #[default]
    None,
    /// `g` pressed, waiting for `g`.
    G,
    /// Operator waiting for a motion.
    Op(Op),
    /// Operator plus `i`/`a`, waiting for the object character.
    Object { op: Op, around: bool },
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct VimState {
    pub mode: VimMode,
    visual_line: bool,
    pending: Pending,
    count: Option<usize>,
    register: String,
    register_linewise: bool,
}

impl VimState {
    pub fn new() -> Self {
        Self {
            mode: VimMode::Insert,
            ..Default::default()
        }
    }

    pub fn with_mode(mode: VimMode) -> Self {
        Self {
            mode,
            ..Default::default()
        }
    }

    pub fn register(&self) -> &str {
        &self.register
    }

    /// The view owns display-row ranges, so it stores what it actually yanked.
    pub fn set_register(&mut self, text: String, linewise: bool) {
        self.register = text;
        self.register_linewise = linewise;
    }

    pub fn is_normal(&self) -> bool {
        self.mode != VimMode::Insert
    }

    pub fn is_visual(&self) -> bool {
        self.mode == VimMode::Visual
    }

    pub fn visual_line(&self) -> bool {
        self.visual_line
    }

    fn reset(&mut self) {
        self.pending = Pending::None;
        self.count = None;
        self.visual_line = false;
    }

    fn take_count(&mut self) -> usize {
        self.count.take().unwrap_or(1).max(1)
    }

    fn push_digit(&mut self, digit: usize) -> bool {
        if self.count.is_none() && digit == 0 {
            return false;
        }
        self.count = Some((self.count.unwrap_or(0) * 10 + digit).min(100_000));
        true
    }

    /// Feed one key. `text` is the document; offsets are char indices.
    pub fn handle(&mut self, text: &str, cursor: usize, anchor: usize, key: Key) -> Command {
        let c: Vec<char> = text.chars().collect();
        let cursor = cursor.min(c.len());

        if self.mode == VimMode::Insert {
            if key != Key::Esc {
                return Command::None;
            }
            self.reset();
            self.mode = VimMode::Normal;
            let pos = match cursor.checked_sub(1) {
                Some(p) if c.get(p).is_some_and(|ch| *ch != '\n') => p,
                _ => cursor,
            };
            return Command::Mode {
                mode: VimMode::Normal,
                pos,
                anchor: pos,
            };
        }

        if key == Key::Esc {
            self.reset();
            self.mode = VimMode::Normal;
            return Command::Mode {
                mode: VimMode::Normal,
                pos: cursor,
                anchor: cursor,
            };
        }

        if let Key::Char(d @ '0'..='9') = key {
            if self.push_digit(d as usize - '0' as usize) {
                return Command::None;
            }
        }

        if self.mode == VimMode::Visual {
            if let Key::Char(ch) = key {
                match ch {
                    'g' | 'i' | 'a' => {}
                    _ => {
                        let (start, end) = visual_range(&c, cursor, anchor, self.visual_line);
                        match ch {
                            'd' | 'x' => {
                                return self.finish(
                                    Op::Delete,
                                    &c,
                                    start,
                                    end,
                                    self.visual_line,
                                    anchor,
                                )
                            }
                            'c' | 's' => {
                                return self.finish(
                                    Op::Change,
                                    &c,
                                    start,
                                    end,
                                    self.visual_line,
                                    anchor,
                                )
                            }
                            'y' => {
                                return self.finish(
                                    Op::Yank,
                                    &c,
                                    start,
                                    end,
                                    self.visual_line,
                                    anchor,
                                )
                            }
                            'o' => {
                                return Command::Move {
                                    pos: anchor,
                                    extend: true,
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
        }

        match self.pending {
            Pending::G => {
                self.pending = Pending::None;
                if key == Key::Char('g') {
                    let n = self.take_count();
                    let target = nth_line_start(&c, n.max(1) - 1);
                    return self.move_or_extend(anchor, first_non_blank(&c, target));
                }
                self.count = None;
                return Command::None;
            }
            Pending::Object { op, around } => {
                self.pending = Pending::None;
                let count = self.count.take().unwrap_or(1);
                let Some(obj) = key_char(key) else {
                    return Command::None;
                };
                let Some((mut start, mut end)) = text_object(&c, cursor, obj, around) else {
                    return Command::None;
                };
                if count > 1 {
                    // Repeat the object forwards for counts: cheap, predictable.
                    for _ in 1..count {
                        let Some((s2, e2)) = text_object(&c, end, obj, around) else {
                            break;
                        };
                        if e2 <= end {
                            break;
                        }
                        start = start.min(s2);
                        end = e2;
                    }
                }
                return self.finish(op, &c, start, end, false, anchor);
            }
            Pending::Op(_) => {}
            Pending::None => {}
        }

        if let Pending::Op(op) = self.pending {
            self.pending = Pending::None;
            return self.with_motion(op, &c, cursor, anchor, key);
        }

        let Key::Char(ch) = key else {
            return self.special_key(&c, cursor, key, anchor);
        };

        match ch {
            'j' | 'k' => {
                let n = self.take_count();
                Command::MoveVertical {
                    dir: if ch == 'j' { 1 } else { -1 },
                    count: n,
                }
            }
            'h' | 'l' | 'w' | 'b' | 'e' | '0' | '^' | '$' => {
                let n = self.take_count();
                let motion = match ch {
                    'h' => Motion::Left,
                    'l' => Motion::Right,
                    'w' => Motion::WordForward,
                    'b' => Motion::WordBack,
                    'e' => Motion::WordEnd,
                    '0' => Motion::LineStart,
                    '^' => Motion::FirstNonBlank,
                    _ => Motion::LineEnd,
                };
                self.move_or_extend(anchor, resolve(&c, cursor, motion, n))
            }
            'G' => {
                let n = self.take_count();
                let target = if n > 1 {
                    nth_line_start(&c, n - 1)
                } else {
                    last_line_start(&c)
                };
                self.move_or_extend(anchor, first_non_blank(&c, target))
            }
            'g' => {
                self.pending = Pending::G;
                Command::None
            }
            'i' | 'a' | 'I' | 'A' | 'o' | 'O' | 's' | 'S' | 'C' => {
                self.enter_insert(&c, cursor, ch)
            }
            'd' | 'c' | 'y' => {
                let op = match ch {
                    'd' => Op::Delete,
                    'c' => Op::Change,
                    _ => Op::Yank,
                };
                self.pending = Pending::Op(op);
                Command::None
            }
            'D' => {
                let end = line_end(&c, cursor);
                self.finish(Op::Delete, &c, cursor, end, false, anchor)
            }
            'Y' => {
                let count = self.take_count();
                Command::YankRow { count }
            }
            'x' | 'X' => {
                let n = self.take_count();
                let (start, end) = if ch == 'x' {
                    (cursor, (cursor + n).min(line_end(&c, cursor)).min(c.len()))
                } else {
                    (cursor.saturating_sub(n), cursor)
                };
                self.finish(Op::Delete, &c, start, end, false, anchor)
            }
            'p' | 'P' => self.paste(&c, cursor, ch == 'p'),
            'u' => {
                self.reset();
                Command::Undo
            }
            'v' | 'V' => {
                let want_line = ch == 'V';
                let was_visual = self.mode == VimMode::Visual;
                // `v`/`V` start visual mode. Inside it the same key leaves,
                // while the other one switches between characterwise and
                // linewise without dropping the selection, as in Vim.
                let (mode, keep_anchor) = if !was_visual {
                    (VimMode::Visual, false)
                } else if want_line == self.visual_line {
                    (VimMode::Normal, false)
                } else {
                    (VimMode::Visual, true)
                };
                self.mode = mode;
                self.visual_line = mode == VimMode::Visual && want_line;
                self.count = None;
                Command::Mode {
                    mode,
                    pos: cursor,
                    anchor: if keep_anchor { anchor } else { cursor },
                }
            }
            'J' => self.join_lines(&c, cursor),
            '/' => {
                self.reset();
                Command::Search { forward: true }
            }
            '?' => {
                self.reset();
                Command::Search { forward: false }
            }
            'n' | 'N' => {
                self.reset();
                Command::SearchNext { forward: ch == 'n' }
            }
            ':' => {
                self.reset();
                Command::CommandLine
            }
            _ => Command::None,
        }
    }

    fn special_key(&mut self, c: &[char], cursor: usize, key: Key, anchor: usize) -> Command {
        let (motion, n) = match key {
            Key::Left => (Motion::Left, 1),
            Key::Right | Key::Space => (Motion::Right, 1),
            Key::Up => return Command::MoveVertical { dir: -1, count: 1 },
            Key::Down | Key::Enter => return Command::MoveVertical { dir: 1, count: 1 },
            Key::Home => (Motion::LineStart, 1),
            Key::End => (Motion::LineEnd, 1),
            Key::PageUp | Key::PageDown => return Command::None,
            _ => return Command::None,
        };
        let target = resolve(c, cursor, motion, n);
        self.move_or_extend(anchor, target)
    }

    fn move_or_extend(&mut self, anchor: usize, pos: usize) -> Command {
        let extend = self.mode == VimMode::Visual;
        Command::Move {
            pos,
            extend: extend || (self.mode == VimMode::Visual && anchor == pos),
        }
    }

    fn with_motion(
        &mut self,
        op: Op,
        c: &[char],
        cursor: usize,
        anchor: usize,
        key: Key,
    ) -> Command {
        let count = self.take_count();
        let Key::Char(ch) = key else {
            return Command::None;
        };

        // Doubled operator works on whole lines.
        if matches!(
            (op, ch),
            (Op::Delete, 'd') | (Op::Change, 'c') | (Op::Yank, 'y')
        ) {
            if op == Op::Yank {
                return Command::YankRow {
                    count: count.max(1),
                };
            }
            let (start, end) = linewise_range(c, cursor, count);
            return self.finish(op, c, start, end, true, anchor);
        }

        if ch == 'i' || ch == 'a' {
            self.pending = Pending::Object {
                op,
                around: ch == 'a',
            };
            self.count = Some(count);
            return Command::None;
        }

        if ch == 'g' {
            self.pending = Pending::G;
            self.count = Some(count);
            return Command::None;
        }

        let motion = match ch {
            'h' => Motion::Left,
            'j' => Motion::Down,
            'k' => Motion::Up,
            'l' => Motion::Right,
            'w' => Motion::WordForward,
            'b' => Motion::WordBack,
            'e' => Motion::WordEnd,
            '0' => Motion::LineStart,
            '^' => Motion::FirstNonBlank,
            '$' => Motion::LineEnd,
            'G' => Motion::DocEnd,
            'x' => Motion::Right,
            _ => return Command::None,
        };

        let (start, end, linewise) = operator_range(c, cursor, motion, count);
        self.finish(op, c, start, end, linewise, anchor)
    }

    fn finish(
        &mut self,
        op: Op,
        c: &[char],
        start: usize,
        end: usize,
        linewise: bool,
        _anchor: usize,
    ) -> Command {
        let start = start.min(c.len());
        let end = end.min(c.len()).max(start);
        let text: String = c[start..end].iter().collect();
        self.register = text;
        self.register_linewise = linewise;

        if op == Op::Yank {
            return Command::Yank {
                start,
                end,
                linewise,
            };
        }

        self.mode = if op == Op::Change {
            VimMode::Insert
        } else {
            VimMode::Normal
        };
        let pos = start;
        Command::Edit {
            start,
            end,
            text: String::new(),
            pos,
            anchor: pos,
        }
    }

    fn enter_insert(&mut self, c: &[char], cursor: usize, ch: char) -> Command {
        let count = self.take_count();
        self.mode = VimMode::Insert;

        // `o` and `O` open a fresh line, so they insert a newline and land on it.
        if let 'o' | 'O' = ch {
            let (at, pos) = if ch == 'o' {
                let at = line_end(c, cursor);
                (at, at + 1)
            } else {
                let at = line_start(c, cursor);
                (at, at)
            };
            return Command::Edit {
                start: at,
                end: at,
                text: "\n".to_string(),
                pos,
                anchor: pos,
            };
        }

        let pos = match ch {
            'i' => cursor,
            'a' => {
                if c.get(cursor).is_some_and(|x| *x == '\n') {
                    cursor
                } else {
                    (cursor + count).min(c.len())
                }
            }
            'I' => first_non_blank(c, cursor),
            'A' => line_end(c, cursor),
            's' => cursor,
            'S' => line_start(c, cursor),
            'C' => cursor,
            _ => return Command::None,
        };

        let (start, end) = match ch {
            's' => (cursor, (cursor + count).min(line_end(c, cursor))),
            'S' => (line_start(c, cursor), line_end(c, cursor)),
            'C' => (cursor, line_end(c, cursor)),
            _ => {
                return Command::Mode {
                    mode: VimMode::Insert,
                    pos,
                    anchor: pos,
                }
            }
        };

        Command::Edit {
            start,
            end,
            text: String::new(),
            pos,
            anchor: pos,
        }
    }

    fn join_lines(&mut self, c: &[char], cursor: usize) -> Command {
        let count = self.take_count();
        let ls = line_start(c, cursor);
        let mut end = line_end(c, cursor);
        if end >= c.len() {
            return Command::None;
        }
        let mut parts: Vec<String> = vec![c[ls..end].iter().collect()];
        for _ in 1..count {
            let next_start = end + 1;
            if next_start > c.len() {
                break;
            }
            let next_end = line_end(c, next_start);
            parts.push(c[next_start..next_end].iter().collect());
            end = next_end;
            if end >= c.len() {
                break;
            }
        }
        let joined = parts
            .iter()
            .map(|p| p.trim())
            .filter(|p| !p.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        let pos = (ls + joined.chars().count().saturating_sub(1)).min(c.len());
        Command::Edit {
            start: ls,
            end,
            text: joined,
            pos,
            anchor: pos,
        }
    }

    fn paste(&mut self, c: &[char], cursor: usize, after: bool) -> Command {
        let count = self.take_count();
        if self.register.is_empty() {
            return Command::None;
        }
        let register = self.register.clone();
        let text = if count > 1 {
            register.repeat(count)
        } else {
            register
        };

        if self.register_linewise {
            let body = text.strip_suffix('\n').unwrap_or(&text).to_string();
            if after {
                let le = line_end(c, cursor);
                if le >= c.len() {
                    let mut t = String::from("\n");
                    t.push_str(&body);
                    t.push('\n');
                    return Command::Edit {
                        start: c.len(),
                        end: c.len(),
                        text: t,
                        pos: c.len() + 1,
                        anchor: c.len() + 1,
                    };
                }
                let at = le + 1;
                let mut t = body;
                t.push('\n');
                let pos = at + leading_ws(&t);
                Command::Edit {
                    start: at,
                    end: at,
                    text: t,
                    pos,
                    anchor: pos,
                }
            } else {
                let at = line_start(c, cursor);
                let mut t = body;
                t.push('\n');
                let pos = at + leading_ws(&t);
                Command::Edit {
                    start: at,
                    end: at,
                    text: t,
                    pos,
                    anchor: pos,
                }
            }
        } else {
            let at = if after && c.get(cursor).is_some_and(|x| *x != '\n') {
                cursor + 1
            } else {
                cursor
            };
            let pos = at + text.chars().count().saturating_sub(1);
            Command::Edit {
                start: at,
                end: at,
                text,
                pos,
                anchor: pos,
            }
        }
    }
}

fn key_char(key: Key) -> Option<char> {
    match key {
        Key::Char(c) => Some(c),
        _ => None,
    }
}

fn line_start(c: &[char], pos: usize) -> usize {
    let pos = pos.min(c.len());
    let mut i = pos;
    while i > 0 && c[i - 1] != '\n' {
        i -= 1;
    }
    i
}

fn line_end(c: &[char], pos: usize) -> usize {
    let pos = pos.min(c.len());
    let mut i = pos;
    while i < c.len() && c[i] != '\n' {
        i += 1;
    }
    i
}

fn first_non_blank(c: &[char], pos: usize) -> usize {
    let le = line_end(c, pos);
    let mut i = line_start(c, pos);
    while i < le && (c[i] == ' ' || c[i] == '\t') {
        i += 1;
    }
    i
}

fn last_line_start(c: &[char]) -> usize {
    if c.is_empty() {
        return 0;
    }
    let mut i = c.len();
    if c[i - 1] == '\n' && i >= 2 {
        i -= 1;
    }
    line_start(c, i - 1)
}

fn nth_line_start(c: &[char], n: usize) -> usize {
    let mut pos = 0;
    for _ in 0..n {
        let next = line_end(c, pos) + 1;
        if next > c.len() {
            break;
        }
        pos = next;
    }
    line_start(c, pos.min(c.len()))
}

fn leading_ws(text: &str) -> usize {
    text.chars().take_while(|c| *c == ' ' || *c == '\t').count()
}

fn column(c: &[char], pos: usize) -> usize {
    pos - line_start(c, pos)
}

fn resolve(c: &[char], cursor: usize, motion: Motion, count: usize) -> usize {
    match motion {
        Motion::Left => {
            let ls = line_start(c, cursor);
            cursor.saturating_sub(count).max(ls)
        }
        Motion::Right => {
            let le = line_end(c, cursor);
            (cursor + count).min(le)
        }
        Motion::Up | Motion::Down => move_vertical(c, cursor, motion, count),
        Motion::WordForward => {
            let mut pos = cursor;
            for _ in 0..count {
                pos = word_forward(c, pos);
            }
            pos
        }
        Motion::WordBack => {
            let mut pos = cursor;
            for _ in 0..count {
                pos = word_back(c, pos);
            }
            pos
        }
        Motion::WordEnd => {
            let mut pos = cursor;
            for _ in 0..count {
                pos = word_end(c, pos);
            }
            pos
        }
        Motion::LineStart => line_start(c, cursor),
        Motion::FirstNonBlank => first_non_blank(c, cursor),
        Motion::LineEnd => {
            let le = line_end(c, cursor);
            if le > line_start(c, cursor) {
                le - 1
            } else {
                le
            }
        }
        Motion::DocEnd => last_line_start(c),
    }
}

fn move_vertical(c: &[char], cursor: usize, motion: Motion, count: usize) -> usize {
    let col = column(c, cursor);
    let mut ls = line_start(c, cursor);
    for _ in 0..count {
        if motion == Motion::Down {
            let le = line_end(c, ls);
            if le >= c.len() {
                break;
            }
            ls = le + 1;
            if ls > c.len() {
                break;
            }
        } else {
            if ls == 0 {
                break;
            }
            ls = line_start(c, ls - 1);
        }
    }
    let le = line_end(c, ls);
    (ls + col).min(le)
}

fn operator_range(c: &[char], cursor: usize, motion: Motion, count: usize) -> (usize, usize, bool) {
    match motion {
        Motion::Left => (cursor.saturating_sub(count), cursor, false),
        Motion::Right => (cursor, (cursor + count).min(line_end(c, cursor)), false),
        Motion::WordForward => {
            let mut target = cursor;
            for _ in 0..count {
                let next = word_forward(c, target);
                if next == target {
                    break;
                }
                target = next;
            }
            if target == cursor {
                (cursor, line_end(c, cursor), false)
            } else {
                (cursor, target, false)
            }
        }
        Motion::WordBack => {
            let mut target = cursor;
            for _ in 0..count {
                let next = word_back(c, target);
                if next == target {
                    break;
                }
                target = next;
            }
            (target, cursor, false)
        }
        Motion::WordEnd => {
            let mut target = cursor;
            for _ in 0..count {
                let next = word_end(c, target);
                if next == target {
                    break;
                }
                target = next;
            }
            (cursor, (target + 1).min(c.len()), false)
        }
        Motion::LineStart => (line_start(c, cursor), cursor, false),
        Motion::FirstNonBlank => (first_non_blank(c, cursor), cursor, false),
        Motion::LineEnd => (cursor, line_end(c, cursor), false),
        Motion::DocEnd => (line_start(c, cursor), c.len(), true),
        Motion::Up | Motion::Down => {
            let target_line = move_vertical(c, cursor, motion, count);
            let a = line_start(c, cursor).min(line_start(c, target_line));
            let b = line_end(c, cursor).max(line_end(c, target_line));
            (a, (b + 1).min(c.len()), true)
        }
    }
}

fn visual_range(c: &[char], cursor: usize, anchor: usize, linewise: bool) -> (usize, usize) {
    let (a, b) = if cursor <= anchor {
        (cursor, anchor)
    } else {
        (anchor, cursor)
    };
    if linewise {
        let start = line_start(c, a);
        let end = (line_end(c, b) + 1).min(c.len());
        (start, end)
    } else {
        (a, (b + 1).min(c.len()))
    }
}

fn linewise_range(c: &[char], cursor: usize, count: usize) -> (usize, usize) {
    let start = line_start(c, cursor);
    let mut end_line_start = start;
    for _ in 1..count.max(1) {
        let next = line_end(c, end_line_start);
        if next >= c.len() {
            break;
        }
        end_line_start = next + 1;
    }
    let end = (line_end(c, end_line_start) + 1).min(c.len());
    (start, end)
}

fn word_forward(c: &[char], pos: usize) -> usize {
    let len = c.len();
    let mut i = pos;
    if i >= len {
        return len;
    }
    let class = classify(c[i]);
    if class != 0 {
        while i < len && classify(c[i]) == class {
            i += 1;
        }
        while i < len && classify(c[i]) == 0 {
            if c[i] == '\n' {
                return i;
            }
            i += 1;
        }
    } else {
        while i < len && classify(c[i]) == 0 {
            if c[i] == '\n' {
                return i;
            }
            i += 1;
        }
    }
    i.min(len)
}

fn word_back(c: &[char], pos: usize) -> usize {
    let mut i = pos;
    if i == 0 {
        return 0;
    }
    i -= 1;
    while i > 0 && classify(c[i]) == 0 {
        i -= 1;
    }
    let class = classify(c[i]);
    if class != 0 {
        while i > 0 && classify(c[i - 1]) == class {
            i -= 1;
        }
    }
    i
}

fn word_end(c: &[char], pos: usize) -> usize {
    let len = c.len();
    let mut i = pos;
    if i + 1 >= len {
        return len.saturating_sub(1);
    }
    i += 1;
    while i < len && classify(c[i]) == 0 {
        i += 1;
    }
    if i >= len {
        return len.saturating_sub(1);
    }
    let class = classify(c[i]);
    while i + 1 < len && classify(c[i + 1]) == class {
        i += 1;
    }
    i
}

/// 0 = whitespace/newline, 1 = word character, 2 = punctuation.
fn classify(ch: char) -> u8 {
    if ch.is_whitespace() {
        0
    } else if ch.is_alphanumeric() || ch == '_' {
        1
    } else {
        2
    }
}

fn text_object(c: &[char], cursor: usize, obj: char, around: bool) -> Option<(usize, usize)> {
    match obj {
        'w' => {
            let cursor = cursor.min(c.len().saturating_sub(1));
            let (mut start, mut end) = if cursor < c.len() && classify(c[cursor]) != 0 {
                let class = classify(c[cursor]);
                let mut s = cursor;
                while s > 0 && classify(c[s - 1]) == class {
                    s -= 1;
                }
                let mut e = cursor;
                while e < c.len() && classify(c[e]) == class {
                    e += 1;
                }
                (s, e)
            } else {
                let mut s = cursor;
                while s < c.len() && classify(c[s]) == 0 && c[s] != '\n' {
                    s += 1;
                }
                let mut e = s;
                while e < c.len() && classify(c[e]) != 0 {
                    e += 1;
                }
                (s, e)
            };
            if around {
                while end < c.len() && (c[end] == ' ' || c[end] == '\t') {
                    end += 1;
                }
                if end == start {
                    start = start.saturating_sub(1);
                }
            }
            Some((start, end))
        }
        q @ ('"' | '\'' | '`') => {
            let ls = line_start(c, cursor);
            let le = line_end(c, cursor);
            let mut open = None;
            let mut i = ls;
            while i < le {
                if c[i] == q {
                    match open {
                        Some(o) => {
                            if cursor > o && cursor <= i {
                                return Some((o + 1, i));
                            }
                            open = Some(i);
                        }
                        None => open = Some(i),
                    }
                }
                i += 1;
            }
            None
        }
        open @ ('(' | '[' | '{') => {
            let close = match open {
                '(' => ')',
                '[' => ']',
                _ => '}',
            };
            let (start, end) = bracket_span(c, cursor, open, close)?;
            if around {
                Some((start, (end + 1).min(c.len())))
            } else {
                Some((start + 1, end))
            }
        }
        _ => None,
    }
}

fn bracket_span(c: &[char], cursor: usize, open: char, close: char) -> Option<(usize, usize)> {
    let mut depth = 0i32;
    let mut i = cursor.min(c.len());
    let mut start = None;
    while i > 0 {
        i -= 1;
        if c[i] == close {
            depth += 1;
        } else if c[i] == open {
            if depth == 0 {
                start = Some(i);
                break;
            }
            depth -= 1;
        }
    }
    let start = start?;
    depth = 0;
    let mut j = start;
    while j < c.len() {
        if c[j] == open {
            depth += 1;
        } else if c[j] == close {
            depth -= 1;
            if depth == 0 {
                return Some((start, j));
            }
        }
        j += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn normal() -> VimState {
        VimState::with_mode(VimMode::Normal)
    }

    fn seq(text: &str, mut cursor: usize, keys: &str) -> (VimState, Command) {
        let mut st = normal();
        let mut anchor = cursor;
        let mut cmd = Command::None;
        for ch in keys.chars() {
            cmd = st.handle(text, cursor, anchor, Key::Char(ch));
            if let Command::Move { pos, extend: false } = cmd {
                cursor = pos;
                anchor = pos;
            }
        }
        (st, cmd)
    }

    #[test]
    fn hjkl_moves_within_lines() {
        let text = "hello\nworld";
        assert_eq!(
            seq(text, 0, "ll").1,
            Command::Move {
                pos: 2,
                extend: false
            }
        );
        assert_eq!(
            seq(text, 2, "h").1,
            Command::Move {
                pos: 1,
                extend: false
            }
        );
        assert_eq!(
            seq(text, 0, "j").1,
            Command::MoveVertical { dir: 1, count: 1 }
        );
        assert_eq!(
            seq(text, 6, "k").1,
            Command::MoveVertical { dir: -1, count: 1 }
        );
    }

    #[test]
    fn line_start_end_and_first_non_blank() {
        let text = "  hello world\nnext";
        assert_eq!(
            seq(text, 8, "$").1,
            Command::Move {
                pos: 12,
                extend: false
            }
        );
        assert_eq!(
            seq(text, 8, "0").1,
            Command::Move {
                pos: 0,
                extend: false
            }
        );
        assert_eq!(
            seq(text, 8, "^").1,
            Command::Move {
                pos: 2,
                extend: false
            }
        );
    }

    #[test]
    fn word_motions() {
        let text = "one two three";
        assert_eq!(
            seq(text, 0, "w").1,
            Command::Move {
                pos: 4,
                extend: false
            }
        );
        assert_eq!(
            seq(text, 4, "w").1,
            Command::Move {
                pos: 8,
                extend: false
            }
        );
        assert_eq!(
            seq(text, 8, "b").1,
            Command::Move {
                pos: 4,
                extend: false
            }
        );
        assert_eq!(
            seq(text, 0, "e").1,
            Command::Move {
                pos: 2,
                extend: false
            }
        );
    }

    #[test]
    fn counts_apply_to_motions() {
        assert_eq!(
            seq("a b c d e", 0, "3w").1,
            Command::Move {
                pos: 6,
                extend: false
            }
        );
    }

    #[test]
    fn gg_and_g_go_to_the_ends() {
        let text = "one\ntwo\nthree";
        assert_eq!(
            seq(text, 10, "gg").1,
            Command::Move {
                pos: 0,
                extend: false
            }
        );
        assert_eq!(
            seq(text, 0, "G").1,
            Command::Move {
                pos: 8,
                extend: false
            }
        );
    }

    #[test]
    fn x_deletes_a_character_into_the_register() {
        let (st, cmd) = seq("abc", 1, "x");
        assert_eq!(
            cmd,
            Command::Edit {
                start: 1,
                end: 2,
                text: String::new(),
                pos: 1,
                anchor: 1
            }
        );
        assert_eq!(st.register(), "b");
    }

    #[test]
    fn dd_deletes_the_line() {
        let text = "one\ntwo\nthree";
        let mut st = normal();
        st.handle(text, 4, 4, Key::Char('d'));
        let cmd = st.handle(text, 4, 4, Key::Char('d'));
        assert_eq!(
            cmd,
            Command::Edit {
                start: 4,
                end: 8,
                text: String::new(),
                pos: 4,
                anchor: 4
            }
        );
        assert_eq!(st.register(), "two\n");
    }

    #[test]
    fn yy_then_p_pastes_below() {
        let text = "one\ntwo";
        let mut st = normal();
        st.handle(text, 0, 0, Key::Char('y'));
        // The view resolves the displayed row and stores what it yanked.
        assert_eq!(
            st.handle(text, 0, 0, Key::Char('y')),
            Command::YankRow { count: 1 }
        );
        st.set_register("one\n".to_string(), true);
        let cmd = st.handle(text, 0, 0, Key::Char('p'));
        assert_eq!(
            cmd,
            Command::Edit {
                start: 4,
                end: 4,
                text: "one\n".to_string(),
                pos: 4,
                anchor: 4
            }
        );
    }

    #[test]
    fn dw_deletes_to_the_next_word() {
        let mut st = normal();
        st.handle("foo bar baz", 0, 0, Key::Char('d'));
        assert_eq!(
            st.handle("foo bar baz", 0, 0, Key::Char('w')),
            Command::Edit {
                start: 0,
                end: 4,
                text: String::new(),
                pos: 0,
                anchor: 0
            }
        );
    }

    #[test]
    fn diw_deletes_the_word_under_the_cursor() {
        let mut st = normal();
        st.handle("foo bar baz", 5, 5, Key::Char('d'));
        st.handle("foo bar baz", 5, 5, Key::Char('i'));
        assert_eq!(
            st.handle("foo bar baz", 5, 5, Key::Char('w')),
            Command::Edit {
                start: 4,
                end: 7,
                text: String::new(),
                pos: 4,
                anchor: 4
            }
        );
    }

    #[test]
    fn yy_and_shift_y_ask_the_view_for_display_rows() {
        assert_eq!(seq("one\ntwo", 0, "yy").1, Command::YankRow { count: 1 });
        assert_eq!(seq("one\ntwo", 0, "2yy").1, Command::YankRow { count: 2 });
        assert_eq!(seq("one\ntwo", 0, "Y").1, Command::YankRow { count: 1 });
    }

    #[test]
    fn visual_selection_yanks() {
        let mut st = normal();
        let cmd = st.handle("hello world", 0, 0, Key::Char('v'));
        assert!(matches!(
            cmd,
            Command::Mode {
                mode: VimMode::Visual,
                ..
            }
        ));
        assert_eq!(
            st.handle("hello world", 1, 0, Key::Char('y')),
            Command::Yank {
                start: 0,
                end: 2,
                linewise: false
            }
        );
    }

    #[test]
    fn visual_line_mode_yanks_whole_lines() {
        let mut st = normal();
        st.handle("one\ntwo\nthree", 0, 0, Key::Char('V'));
        let cmd = st.handle("one\ntwo\nthree", 5, 0, Key::Char('y'));
        assert_eq!(
            cmd,
            Command::Yank {
                start: 0,
                end: 8,
                linewise: true
            }
        );
    }

    #[test]
    fn v_then_shift_v_switches_to_linewise_and_keeps_the_anchor() {
        let mut st = normal();
        st.handle("one\ntwo\nthree", 0, 0, Key::Char('v'));
        assert_eq!(
            st.handle("one\ntwo\nthree", 1, 0, Key::Char('V')),
            Command::Mode {
                mode: VimMode::Visual,
                pos: 1,
                anchor: 0
            }
        );
        assert!(st.is_visual());
        assert!(st.visual_line());
        assert_eq!(
            st.handle("one\ntwo\nthree", 1, 0, Key::Char('y')),
            Command::Yank {
                start: 0,
                end: 4,
                linewise: true
            }
        );
    }

    #[test]
    fn shift_v_twice_leaves_visual_mode() {
        let mut st = normal();
        st.handle("one\ntwo", 0, 0, Key::Char('V'));
        assert_eq!(
            st.handle("one\ntwo", 0, 0, Key::Char('V')),
            Command::Mode {
                mode: VimMode::Normal,
                pos: 0,
                anchor: 0
            }
        );
        assert!(!st.is_visual());
    }

    #[test]
    fn esc_returns_to_normal_one_char_left() {
        let mut st = VimState::new();
        assert_eq!(
            st.handle("abc", 2, 2, Key::Esc),
            Command::Mode {
                mode: VimMode::Normal,
                pos: 1,
                anchor: 1
            }
        );
    }

    #[test]
    fn insert_commands_set_mode() {
        assert_eq!(
            normal().handle("abc", 1, 1, Key::Char('a')),
            Command::Mode {
                mode: VimMode::Insert,
                pos: 2,
                anchor: 2
            }
        );
        assert_eq!(
            normal().handle("abc\nx", 1, 1, Key::Char('o')),
            Command::Edit {
                start: 3,
                end: 3,
                text: "\n".to_string(),
                pos: 4,
                anchor: 4
            }
        );
    }

    #[test]
    fn o_opens_a_line_below() {
        let text = "one\ntwo";
        let mut st = normal();
        let cmd = st.handle(text, 0, 0, Key::Char('o'));
        assert_eq!(
            cmd,
            Command::Edit {
                start: 3,
                end: 3,
                text: "\n".to_string(),
                pos: 4,
                anchor: 4
            }
        );
        assert_eq!(st.mode, VimMode::Insert);
    }

    #[test]
    fn shift_o_opens_a_line_above() {
        let text = "one\ntwo";
        let mut st = normal();
        let cmd = st.handle(text, 5, 5, Key::Char('O'));
        assert_eq!(
            cmd,
            Command::Edit {
                start: 4,
                end: 4,
                text: "\n".to_string(),
                pos: 4,
                anchor: 4
            }
        );
        assert_eq!(st.mode, VimMode::Insert);
    }

    #[test]
    fn search_keys_emit_commands() {
        let mut st = normal();
        assert_eq!(
            st.handle("abc", 0, 0, Key::Char('/')),
            Command::Search { forward: true }
        );
        assert_eq!(
            st.handle("abc", 0, 0, Key::Char('n')),
            Command::SearchNext { forward: true }
        );
        assert_eq!(st.handle("abc", 0, 0, Key::Char(':')), Command::CommandLine);
    }

    #[test]
    fn change_word_enters_insert() {
        let mut st = normal();
        st.handle("foo bar", 0, 0, Key::Char('c'));
        let cmd = st.handle("foo bar", 0, 0, Key::Char('w'));
        assert_eq!(
            cmd,
            Command::Edit {
                start: 0,
                end: 4,
                text: String::new(),
                pos: 0,
                anchor: 0
            }
        );
        assert_eq!(st.mode, VimMode::Insert);
    }

    #[test]
    fn delete_inside_quotes() {
        let mut st = normal();
        let text = "say \"hello\" now";
        st.handle(text, 6, 6, Key::Char('d'));
        st.handle(text, 6, 6, Key::Char('i'));
        let cmd = st.handle(text, 6, 6, Key::Char('"'));
        assert_eq!(
            cmd,
            Command::Edit {
                start: 5,
                end: 10,
                text: String::new(),
                pos: 5,
                anchor: 5
            }
        );
    }

    #[test]
    fn paste_charwise_after_cursor() {
        let mut st = normal();
        st.handle("ab", 0, 0, Key::Char('x'));
        let cmd = st.handle("ab", 0, 0, Key::Char('p'));
        assert_eq!(
            cmd,
            Command::Edit {
                start: 1,
                end: 1,
                text: "a".to_string(),
                pos: 1,
                anchor: 1
            }
        );
    }
}
