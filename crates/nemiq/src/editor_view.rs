//! The editor view: layout, input, painting and overlays.
//!
//! Each source line is shaped with `shape_text` at the centered column width,
//! so long lines wrap. Caret, selection, search highlights and mouse
//! hit-testing all work in char offsets and are converted to wrapped-line byte
//! offsets when talking to GPUI.
//!
//! Every mutation goes through `DocState`/`EditCommand`, never raw text, so a
//! future vim Normal/Visual mode can issue the same commands.

use crate::state::DocState;
use gpui::*;
use nemiq_core::{
    inline_markup, is_thematic_break, opening_fence_length, InlineKind, VimCommand, VimKey,
    VimMode, VimState,
};
use std::ops::Range;
use std::sync::Arc;
use std::time::Duration;

actions!(
    editor,
    [
        Save,
        Open,
        SaveAs,
        NewWindow,
        Print,
        CopyAsHtml,
        Bold,
        Italic,
        Strike,
        Link,
        Find,
        FindReplace,
        FindNext,
        FindPrev,
        ReplaceAll,
        CloseFind,
        ZoomIn,
        ZoomOut,
        ZoomReset,
        Undo,
        Redo,
        Left,
        Right,
        Up,
        Down,
        SelectLeft,
        SelectRight,
        SelectUp,
        SelectDown,
        Home,
        End,
        DocStart,
        DocEnd,
        SelectAll,
        Backspace,
        Delete,
        Paste,
        Cut,
        Copy,
        PageUp,
        PageDown,
        Indent,
        Outdent,
        SearchBackspace,
        Newline,
        WordBackspace,
        WordDelete,
        ToggleFullscreen,
        Shortcuts,
        DialogCancel,
        DialogDiscard,
        DialogSave,
    ]
);

const BASE_FONT_PX: f32 = 20.0;
const FOOTER_PX: f32 = 32.0;

// Slim overlay scrollbar: thin, theme-muted, out of the way.
const SCROLLBAR_W: f32 = 7.0;
const SCROLLBAR_RIGHT: f32 = 14.0;
const SCROLLBAR_MARGIN: f32 = 10.0;
const SCROLLBAR_MIN_THUMB: f32 = 36.0;

#[derive(Clone)]
struct Palette {
    bg: Hsla,
    fg: Hsla,
    muted: Hsla,
    marker: Hsla,
    accent: Hsla,
    code_bg: Hsla,
    selection: Hsla,
    search: Hsla,
    search_current: Hsla,
    mono: SharedString,
}

fn parse_color(s: &str, fallback: u32) -> Hsla {
    let h = s.trim().trim_start_matches('#');
    let v = u32::from_str_radix(h, 16).unwrap_or(fallback);
    if h.len() == 3 {
        let r = (v >> 8 & 0xf) as u32 * 17;
        let g = (v >> 4 & 0xf) as u32 * 17;
        let b = (v & 0xf) as u32 * 17;
        rgb((r << 16) | (g << 8) | b).into()
    } else {
        rgb(v).into()
    }
}

impl Palette {
    fn new(state: &DocState) -> Self {
        let t = &state.theme;
        let dark = t.dark;
        Self {
            bg: parse_color(&t.background, if dark { 0x101010 } else { 0xffffff }),
            fg: parse_color(&t.foreground, if dark { 0xeeeeee } else { 0x222324 }),
            muted: rgb(if dark { 0x909191 } else { 0xaeb1b5 }).into(),
            marker: rgb(if dark { 0x4f525a } else { 0xaeb1b5 }).into(),
            accent: parse_color(&t.accent, if dark { 0x5584aa } else { 0x2077b2 }),
            code_bg: parse_color(&t.code_background, if dark { 0x1e2a24 } else { 0xf2f0e6 }),
            selection: parse_color(&t.selection, if dark { 0x186a9a } else { 0x2077b2 }),
            search: rgb(if dark { 0x725b18 } else { 0xffe58a }).into(),
            search_current: rgb(if dark { 0xb36b20 } else { 0xffad42 }).into(),
            mono: SharedString::from(state.editor_font.clone()),
        }
    }

    /// Cheap identity for "did styling inputs change?" checks.
    fn key(&self) -> String {
        format!(
            "{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{}",
            self.bg,
            self.fg,
            self.muted,
            self.marker,
            self.accent,
            self.code_bg,
            self.selection,
            self.search,
            self.search_current,
            self.mono
        )
    }
}

/// One source line, shaped at the column width (may occupy several rows).
#[derive(Clone)]
struct LineBox {
    start_char: usize,
    char_len: usize,
    shaped: WrappedLine,
    top: Pixels,
    height: Pixels,
    fence: bool,
    /// display char -> local document char
    disp_to_doc: Vec<usize>,
    /// local document char -> display char (usize::MAX when hidden)
    doc_to_disp: Vec<usize>,
    /// Source line text this box was built from (for incremental reuse).
    src: String,
    /// Caret offset within this line, if it was here when built.
    caret_local: Option<usize>,
}

impl LineBox {
    fn display_col(&self, doc_local: usize) -> usize {
        let mut i = doc_local.min(self.doc_to_disp.len());
        loop {
            if i >= self.doc_to_disp.len() {
                return self.disp_to_doc.len();
            }
            if self.doc_to_disp[i] != usize::MAX {
                return self.doc_to_disp[i];
            }
            if i == 0 {
                return 0;
            }
            i -= 1;
        }
    }
    fn display_to_doc(&self, disp: usize) -> usize {
        if disp >= self.disp_to_doc.len() {
            self.char_len
        } else {
            self.disp_to_doc[disp]
        }
    }
    /// Display char range covering document chars [a, b) on this line.
    fn display_range(&self, a: usize, b: usize) -> (usize, usize) {
        let mut first = None;
        let mut last = 0usize;
        for (i, &doc) in self.disp_to_doc.iter().enumerate() {
            if doc >= a && doc < b {
                if first.is_none() {
                    first = Some(i);
                }
                last = i + 1;
            }
        }
        match first {
            Some(f) => (f, last),
            None => (0, 0),
        }
    }
}

struct Scrollbar {
    x: Pixels,
    width: Pixels,
    track_top: Pixels,
    track_height: Pixels,
    thumb_top: Pixels,
    thumb_height: Pixels,
    max_scroll: f32,
}

/// Geometry shared by painting and hit-testing.
fn scrollbar_geometry(
    area: Bounds<Pixels>,
    scroll: f32,
    total: Pixels,
    view_h: Pixels,
    scale: f32,
) -> Option<Scrollbar> {
    if total <= view_h + px(1.) {
        return None;
    }
    let width = px(SCROLLBAR_W * scale);
    let track_top = area.top() + px(SCROLLBAR_MARGIN * scale);
    let track_bottom = area.bottom() - px(SCROLLBAR_MARGIN * scale);
    let track_height = (track_bottom - track_top).max(px(1.));
    let max_scroll = f32::from((total - view_h).max(Pixels::ZERO)).max(0.0);
    let min_thumb = px(SCROLLBAR_MIN_THUMB * scale).min(track_height);
    let ratio = (f32::from(view_h) / f32::from(total)).clamp(0.0, 1.0);
    let thumb_height = (track_height * ratio).max(min_thumb).min(track_height);
    let travel = (track_height - thumb_height).max(Pixels::ZERO);
    let progress = if max_scroll > 0.0 {
        (scroll / max_scroll).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let thumb_top = track_top + travel * progress;
    Some(Scrollbar {
        x: area.right() - px(SCROLLBAR_RIGHT * scale) - width,
        width,
        track_top,
        track_height,
        thumb_top,
        thumb_height,
        max_scroll,
    })
}

struct Layout {
    lines: Vec<LineBox>,
    total_height: Pixels,
    wrap_width: Pixels,
    font_size: Pixels,
    line_height: Pixels,
    revision: u64,
    cursor: usize,
    /// Palette/font identity; when it changes no line may be reused.
    palette_key: String,
}

pub struct EditorView {
    pub state: DocState,
    pub focus: FocusHandle,
    pub cursor: usize,
    pub anchor: usize,
    pub scroll_y: f32,
    scroll_target: f32,
    scroll_anim: Option<Task<()>>,
    scroll_animating: bool,
    scrollbar_alpha: f32,
    scrollbar_hide_task: Option<Task<()>>,
    pub selecting: bool,
    scrollbar_drag: Option<f32>,
    pub search_open: bool,
    pub search_text: String,
    pub replace_open: bool,
    replace_text: String,
    replace_focus: bool,
    pub matches: Vec<usize>,
    pub match_idx: i32,
    revision: u64,
    cache: Option<Arc<Layout>>,
    autosave_task: Option<Task<()>>,
    _watch_task: Option<Task<()>>,
    pub bounds_sub: Option<Subscription>,
    pub pending_external: Option<crate::state::ExternalChange>,
    pub external_deleted: bool,
    pub close_prompt: bool,
    pub close_approved: bool,
    pub shortcuts_open: bool,
    pub font_open: bool,
    font_filter: String,
    font_index: usize,
    font_window: usize,
    font_names: Vec<String>,
    /// Which unsaved-changes button has keyboard focus (0 cancel, 1 discard, 2 save).
    pub close_focus: usize,
    pub vim_enabled: bool,
    vim: VimState,
    pub cmdline_open: bool,
    cmdline_text: String,
}

/// Rows visible in the font picker before it scrolls.
const FONT_ROWS: usize = 10;

impl EditorView {
    pub fn new(vim_enabled: bool, cx: &mut Context<Self>) -> Self {
        let font_names = nemiq_core::selectable_font_families(&cx.text_system().all_font_names());
        let watcher = cx.spawn(async move |this, cx| loop {
            cx.background_executor()
                .timer(Duration::from_millis(1000))
                .await;
            let keep_going = this
                .update(cx, |v: &mut EditorView, cx| {
                    if v.pending_external.is_some() {
                        return true;
                    }
                    if let Some(change) = v.state.check_external() {
                        v.external_deleted =
                            matches!(change, crate::state::ExternalChange::Deleted);
                        v.pending_external = Some(change);
                        cx.notify();
                    }
                    true
                })
                .unwrap_or(false);
            if !keep_going {
                break;
            }
        });
        Self {
            _watch_task: Some(watcher),
            bounds_sub: None,
            state: DocState::new(),
            focus: cx.focus_handle(),
            cursor: 0,
            anchor: 0,
            scroll_y: 0.0,
            scroll_target: 0.0,
            scroll_anim: None,
            scroll_animating: false,
            scrollbar_alpha: 0.0,
            scrollbar_hide_task: None,
            selecting: false,
            scrollbar_drag: None,
            search_open: false,
            search_text: String::new(),
            replace_open: false,
            replace_text: String::new(),
            replace_focus: false,
            matches: vec![],
            match_idx: -1,
            revision: 0,
            cache: None,
            autosave_task: None,
            pending_external: None,
            external_deleted: false,
            close_prompt: false,
            close_approved: false,
            shortcuts_open: false,
            font_open: false,
            font_filter: String::new(),
            font_index: 0,
            font_window: 0,
            font_names,
            close_focus: 2,
            vim_enabled,
            vim: if vim_enabled {
                VimState::with_mode(VimMode::Normal)
            } else {
                VimState::new()
            },
            cmdline_open: false,
            cmdline_text: String::new(),
        }
    }

    fn text(&self) -> String {
        self.state.text()
    }

    fn modal_open(&self) -> bool {
        self.cmdline_open
            || self.search_open
            || self.font_open
            || self.shortcuts_open
            || self.close_prompt
            || self.pending_external.is_some()
    }
    /// True while vim Normal/Visual owns the keyboard.
    fn vim_blocks(&self) -> bool {
        self.vim_enabled && self.vim.is_normal()
    }

    pub fn vim_mode(&self) -> VimMode {
        self.vim.mode
    }

    /// Height of the `:` line, when open.
    fn cmdline_height(&self) -> Pixels {
        px(32.0 * self.state.text_scale() as f32)
    }

    fn chars(&self) -> Vec<char> {
        self.text().chars().collect()
    }
    pub fn sel(&self) -> (usize, usize) {
        (self.anchor.min(self.cursor), self.anchor.max(self.cursor))
    }
    fn font_size(&self) -> Pixels {
        px(BASE_FONT_PX * self.state.text_scale() as f32)
    }
    fn footer_height(&self) -> Pixels {
        px(FOOTER_PX * self.state.text_scale() as f32)
    }

    fn column_width(viewport_w: Pixels, font_size: Pixels) -> Pixels {
        // 65 characters, never wider than the viewport minus 20 characters,
        // and always inside a 24px inset so the window edges stay clear.
        let char_w = font_size * 0.6;
        let sixty_five = char_w * 65.0;
        let narrow = (viewport_w - char_w * 20.0).max(px(360.0));
        sixty_five.min(narrow).min(viewport_w - px(48.0))
    }

    fn top_margin(&self, viewport: Size<Pixels>) -> Pixels {
        let scaled = px(42.0 * self.state.text_scale() as f32);
        scaled.max(viewport.height * 0.05)
    }

    fn find_bar_height(&self) -> Pixels {
        let mut h = Pixels::ZERO;
        if self.search_open {
            let rows = if self.replace_open { 2.0 } else { 1.0 };
            h += px(32.0 * self.state.text_scale() as f32 * rows);
        }
        if self.cmdline_open {
            h += self.cmdline_height();
        }
        h
    }

    /// Height of the editor element (viewport minus footer and find bar).
    fn text_area_height(&self, vp: Size<Pixels>) -> Pixels {
        (vp.height - self.footer_height() - self.find_bar_height()).max(px(1.))
    }

    /// Height of text that is actually visible inside the editor element.
    fn visible_text_height(&self, vp: Size<Pixels>) -> Pixels {
        (self.text_area_height(vp) - self.top_margin(vp)).max(px(1.))
    }

    fn set_scroll(&mut self, y: f32) {
        self.scroll_y = y;
        self.scroll_target = y;
    }

    /// Eased scrolling: each notch moves 120 logical pixels toward a running
    /// target instead of jumping.
    fn animate_scroll(&mut self, cx: &mut Context<Self>) {
        if self.scroll_animating {
            return;
        }
        self.scroll_animating = true;
        self.scroll_anim = Some(cx.spawn(async move |this, cx| loop {
            cx.background_executor()
                .timer(Duration::from_millis(16))
                .await;
            let finished = this
                .update(cx, |v: &mut EditorView, cx| {
                    let diff = v.scroll_target - v.scroll_y;
                    if diff.abs() < 0.5 {
                        v.scroll_y = v.scroll_target;
                        v.scroll_animating = false;
                        cx.notify();
                        true
                    } else {
                        v.scroll_y += diff * 0.32;
                        cx.notify();
                        false
                    }
                })
                .unwrap_or(true);
            if finished {
                break;
            }
        }));
    }

    /// Show the scrollbar while scrolling, then fade it out after a pause.
    fn reveal_scrollbar(&mut self, cx: &mut Context<Self>) {
        self.scrollbar_alpha = 1.0;
        self.scrollbar_hide_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(900))
                .await;
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(16))
                    .await;
                let done = this
                    .update(cx, |v: &mut EditorView, cx| {
                        if v.scrollbar_drag.is_some() {
                            v.scrollbar_alpha = 1.0;
                            cx.notify();
                            return false;
                        }
                        v.scrollbar_alpha = (v.scrollbar_alpha - 0.085).max(0.0);
                        cx.notify();
                        v.scrollbar_alpha <= 0.0
                    })
                    .unwrap_or(true);
                if done {
                    break;
                }
            }
        }));
    }

    fn max_scroll(&mut self, window: &mut Window) -> f32 {
        let layout = self.layout(window);
        let vp = window.viewport_size();
        f32::from((layout.total_height - self.visible_text_height(vp)).max(Pixels::ZERO))
    }

    /// Shaped layout for the current text, reused until text/width/zoom change.
    fn layout(&mut self, window: &mut Window) -> Arc<Layout> {
        let vp = window.viewport_size();
        let font_size = self.font_size();
        let width = Self::column_width(vp.width, font_size);
        if let Some(cache) = &self.cache {
            if cache.revision == self.revision
                && cache.wrap_width == width
                && cache.font_size == font_size
                && cache.cursor == self.cursor
            {
                return cache.clone();
            }
        }
        let prev = self.cache.clone();
        let layout = self.build_layout(window, width, font_size, prev.as_deref());
        let arc = Arc::new(layout);
        self.cache = Some(arc.clone());
        arc
    }

    fn build_layout(
        &self,
        window: &Window,
        wrap_width: Pixels,
        font_size: Pixels,
        prev: Option<&Layout>,
    ) -> Layout {
        let text = self.text();
        let palette = Palette::new(&self.state);
        let palette_key = palette.key();
        let prev = prev.filter(|p| p.palette_key == palette_key);
        let line_height = font_size * 1.4;
        let fences = fence_states(&text);
        let mut lines = Vec::new();
        let mut top = Pixels::ZERO;
        let mut start_char = 0usize;
        for (i, line) in text.split('\n').enumerate() {
            let fence = *fences.get(i).unwrap_or(&false);
            let line_start = start_char;
            let char_len = line.chars().count();
            let caret_local = (self.cursor >= line_start && self.cursor <= line_start + char_len)
                .then(|| self.cursor - line_start);

            // Unchanged line: reuse its shaped text and display map. This keeps
            // typing O(1) in document size.
            if let Some(old) = prev.and_then(|p| p.lines.get(i)) {
                if old.src == line && old.fence == fence && old.caret_local == caret_local {
                    lines.push(LineBox {
                        start_char: line_start,
                        top,
                        src: old.src.clone(),
                        caret_local,
                        ..(*old).clone()
                    });
                    top += old.height;
                    start_char += char_len + 1;
                    continue;
                }
            }

            let styles = line_styles(line, &palette, fence);
            let (display, disp_to_doc, doc_to_disp) =
                build_display(line, fence, line_start, self.cursor);
            let mut display_styles: Vec<CharStyle> = Vec::with_capacity(display.len());
            for &doc in &disp_to_doc {
                display_styles.push(
                    styles
                        .get(doc)
                        .cloned()
                        .unwrap_or_else(|| normal_style(&palette)),
                );
            }
            let runs = styles_to_runs(&display, &display_styles, &palette);
            let shaped_line: SharedString = if display.is_empty() {
                SharedString::from(" ")
            } else {
                SharedString::from(display.iter().collect::<String>())
            };
            let runs = if display.is_empty() {
                vec![TextRun {
                    len: 1,
                    font: Font {
                        family: palette.mono.clone(),
                        features: FontFeatures::default(),
                        fallbacks: None,
                        weight: FontWeight::NORMAL,
                        style: FontStyle::Normal,
                    },
                    color: palette.fg,
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                }]
            } else {
                runs
            };
            let shaped = window
                .text_system()
                .shape_text(shaped_line, font_size, &runs, Some(wrap_width), None)
                .ok()
                .and_then(|mut v| v.drain(..).next());
            let height = shaped
                .as_ref()
                .map(|s| s.size(line_height).height)
                .unwrap_or(line_height);
            let shaped = shaped.unwrap_or_default();
            lines.push(LineBox {
                start_char: line_start,
                char_len,
                shaped,
                top,
                height,
                fence,
                disp_to_doc,
                doc_to_disp,
                src: line.to_string(),
                caret_local,
            });
            top += height;
            start_char += char_len + 1;
        }
        Layout {
            lines,
            total_height: top,
            wrap_width,
            font_size,
            line_height,
            revision: self.revision,
            cursor: self.cursor,
            palette_key,
        }
    }

    fn ensure_cursor_visible(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let layout = self.layout(window);
        let vp = window.viewport_size();
        let view_h = self.visible_text_height(vp);
        let Some((top, height)) = caret_line_rect(&layout, self.cursor) else {
            return;
        };
        let scroll = px(self.scroll_y);
        let mut moved = false;
        if top < scroll {
            let y = f32::from(top).max(0.0);
            self.set_scroll(y);
            moved = true;
        } else if top + height > scroll + view_h {
            let y = f32::from(top + height - view_h).max(0.0);
            self.set_scroll(y);
            moved = true;
        }
        if moved {
            self.reveal_scrollbar(cx);
        }
        self.scroll_to_pos(cx);
    }

    fn scroll_to_pos(&mut self, cx: &mut Context<Self>) {
        // Keep wheel handler bounds in sync after programmatic scrolls.
        cx.notify();
    }

    // ---------- text mutations ----------
    pub fn invalidate(&mut self) {
        self.cache = None;
    }

    fn bump(&mut self) {
        // Keep the previous layout around: build_layout reuses the shaped
        // lines that did not change, so typing stays O(1) in document size.
        self.revision = self.revision.wrapping_add(1);
    }

    fn after_edit(&mut self, cx: &mut Context<Self>) {
        self.bump();
        self.state.refresh_wordcount();
        self.refresh_search();
        self.schedule_autosave(cx);
        cx.notify();
    }

    fn schedule_autosave(&mut self, cx: &mut Context<Self>) {
        self.autosave_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(600))
                .await;
            let _ = this.update(cx, |v: &mut EditorView, cx| {
                v.state.persist_recovery();
                if v.state.auto_save() {
                    cx.notify();
                }
            });
        }));
    }

    fn replace(&mut self, start: usize, end: usize, text: String, cx: &mut Context<Self>) {
        if self.modal_open() || self.vim_blocks() {
            return;
        }
        let caret = self.state.apply_replace(start, end, text);
        self.cursor = caret;
        self.anchor = caret;
        self.after_edit(cx);
    }

    fn delete_selection_or(&mut self, from: usize, to: usize, cx: &mut Context<Self>) {
        self.replace(from, to, String::new(), cx);
    }

    fn backspace(&mut self, _: &Backspace, window: &mut Window, cx: &mut Context<Self>) {
        if self.cmdline_open {
            self.cmdline_text.pop();
            cx.notify();
            return;
        }
        if self.font_open {
            self.font_filter_backspace(cx);
            return;
        }
        if self.vim_blocks() {
            self.vim_key_apply(VimKey::Backspace, window, cx);
            return;
        }
        if self.search_open {
            if self.replace_open && self.replace_focus {
                self.replace_text.pop();
                cx.notify();
            } else {
                self.search_backspace_impl(cx);
            }
            return;
        }
        let (a, b) = self.sel();
        if a != b {
            self.delete_selection_or(a, b, cx);
        } else if self.cursor > 0 {
            let prev = self.state.buffer.step_grapheme(self.cursor, -1);
            self.delete_selection_or(prev, self.cursor, cx);
        }
        self.ensure_cursor_visible(window, cx);
    }

    fn word_backspace(&mut self, _: &WordBackspace, window: &mut Window, cx: &mut Context<Self>) {
        if self.font_open {
            self.font_filter_word_backspace(cx);
            return;
        }
        if self.search_open {
            if self.replace_open && self.replace_focus {
                let mut chars: Vec<char> = self.replace_text.chars().collect();
                while chars.last().is_some_and(|c| c.is_whitespace()) {
                    chars.pop();
                }
                while chars
                    .last()
                    .is_some_and(|c| c.is_alphanumeric() || *c == '_')
                {
                    chars.pop();
                }
                self.replace_text = chars.into_iter().collect();
                cx.notify();
            } else {
                self.search_word_backspace(cx);
            }
            return;
        }
        if self.modal_open() {
            return;
        }
        let (a, b) = self.sel();
        if a != b {
            self.delete_selection_or(a, b, cx);
        } else if self.cursor > 0 {
            let text = self.text();
            let chars: Vec<char> = text.chars().collect();
            let mut i = self.cursor;
            while i > 0 && chars[i - 1].is_whitespace() {
                i -= 1;
            }
            while i > 0 && (chars[i - 1].is_alphanumeric() || chars[i - 1] == '_') {
                i -= 1;
            }
            if i == self.cursor {
                i = self.cursor.saturating_sub(1);
            }
            self.delete_selection_or(i, self.cursor, cx);
        }
        self.ensure_cursor_visible(window, cx);
    }

    fn word_delete(&mut self, _: &WordDelete, window: &mut Window, cx: &mut Context<Self>) {
        // Find/font fields have no caret to move, so deletion forward is a no-op.
        if self.modal_open() {
            return;
        }
        let (a, b) = self.sel();
        if a != b {
            self.delete_selection_or(a, b, cx);
        } else {
            let text = self.text();
            let chars: Vec<char> = text.chars().collect();
            let mut i = self.cursor;
            while i < chars.len() && chars[i].is_whitespace() {
                i += 1;
            }
            while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
            if i == self.cursor && i < chars.len() {
                i += 1;
            }
            if i > self.cursor {
                self.delete_selection_or(self.cursor, i, cx);
            }
        }
        self.ensure_cursor_visible(window, cx);
    }

    fn delete_forward(&mut self, _: &Delete, window: &mut Window, cx: &mut Context<Self>) {
        let (a, b) = self.sel();
        if a != b {
            self.delete_selection_or(a, b, cx);
        } else if self.cursor < self.state.buffer.len_chars() {
            let next = self.state.buffer.step_grapheme(self.cursor, 1);
            self.delete_selection_or(self.cursor, next, cx);
        }
        self.ensure_cursor_visible(window, cx);
    }

    fn move_to(&mut self, pos: usize, extend: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.modal_open() || self.vim_blocks() {
            return;
        }
        let pos = pos.min(self.state.buffer.len_chars());
        self.cursor = pos;
        if !extend {
            self.anchor = pos;
        }
        self.ensure_cursor_visible(window, cx);
        cx.notify();
    }

    fn move_horizontal(&mut self, dir: i32, extend: bool, cx: &mut Context<Self>) {
        if self.modal_open() || self.vim_blocks() {
            return;
        }
        let (a, b) = self.sel();
        if a != b && !extend {
            let target = if dir < 0 { a } else { b };
            self.cursor = target;
            self.anchor = target;
        } else {
            let target = self.state.buffer.step_grapheme(self.cursor, dir);
            self.cursor = target;
            if !extend {
                self.anchor = target;
            }
        }
        cx.notify();
    }

    fn move_vertical(
        &mut self,
        dir: i32,
        extend: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.vim_blocks() {
            let key = if dir < 0 { VimKey::Up } else { VimKey::Down };
            self.vim_key_apply(key, window, cx);
            return;
        }
        if self.modal_open() {
            return;
        }
        let layout = self.layout(window);
        let Some((line_ix, x)) = caret_x(&layout, self.cursor) else {
            return;
        };
        let lh = layout.line_height;
        let line = &layout.lines[line_ix];
        let local_row_y = caret_in_line_y(
            &line.shaped,
            line.display_col(self.cursor - line.start_char),
            lh,
        );
        let target_y = line.top + local_row_y + lh * dir as f32;
        let target = index_for_layout_point(&layout, px(f32::from(x)), target_y);
        self.move_to(target, extend, window, cx);
    }

    fn home(&mut self, extend: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.vim_blocks() {
            self.vim_key_apply(VimKey::Home, window, cx);
            return;
        }
        let vp = window.viewport_size();
        let layout = self.layout(window);
        let _ = vp;
        let line_start = layout
            .lines
            .iter()
            .rev()
            .find(|l| l.start_char <= self.cursor)
            .map(|l| l.start_char)
            .unwrap_or(0);
        self.move_to(line_start, extend, window, cx);
    }

    fn end(&mut self, extend: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.vim_blocks() {
            self.vim_key_apply(VimKey::End, window, cx);
            return;
        }
        let layout = self.layout(window);
        let line_end = layout
            .lines
            .iter()
            .rev()
            .find(|l| l.start_char <= self.cursor)
            .map(|l| l.start_char + l.char_len)
            .unwrap_or(0);
        self.move_to(line_end, extend, window, cx);
    }

    fn page(&mut self, dir: i32, extend: bool, window: &mut Window, cx: &mut Context<Self>) {
        let layout = self.layout(window);
        let vp = window.viewport_size();
        let view_h = self.visible_text_height(vp);
        let Some((line_ix, x)) = caret_x(&layout, self.cursor) else {
            return;
        };
        let line = &layout.lines[line_ix];
        let y = line.top
            + caret_in_line_y(
                &line.shaped,
                line.display_col(self.cursor - line.start_char),
                layout.line_height,
            );
        let target_y = (y + view_h * dir as f32).max(px(0.));
        let target = index_for_layout_point(&layout, x, target_y);
        self.move_to(target, extend, window, cx);
    }

    fn newline(&mut self, _: &Newline, window: &mut Window, cx: &mut Context<Self>) {
        if self.cmdline_open {
            self.cmdline_execute(window, cx);
            return;
        }
        if self.close_prompt {
            self.activate_close_focus(window, cx);
            return;
        }
        if self.font_open {
            self.choose_current_font(cx);
            return;
        }
        if self.vim_blocks() {
            self.vim_key_apply(VimKey::Enter, window, cx);
            return;
        }
        if self.search_open {
            if self.replace_open && self.replace_focus {
                self.search_replace_current(window, cx);
            } else {
                self.goto_match(1, window, cx);
            }
            return;
        }
        let (a, b) = self.sel();
        self.smart_return(a, b, cx);
        self.ensure_cursor_visible(window, cx);
    }

    fn select_all(&mut self, _: &SelectAll, _w: &mut Window, cx: &mut Context<Self>) {
        if self.modal_open() {
            return;
        }
        self.anchor = 0;
        self.cursor = self.state.buffer.len_chars();
        cx.notify();
    }

    fn undo(&mut self, _: &Undo, _w: &mut Window, cx: &mut Context<Self>) {
        if self.modal_open() {
            return;
        }
        if self.state.buffer.undo() {
            self.state.refresh_wordcount();
            let n = self.state.buffer.len_chars();
            self.cursor = self.cursor.min(n);
            self.anchor = self.anchor.min(n);
            self.bump();
            self.refresh_search();
            self.schedule_autosave(cx);
        }
        cx.notify();
    }

    fn redo(&mut self, _: &Redo, _w: &mut Window, cx: &mut Context<Self>) {
        if self.modal_open() {
            return;
        }
        if self.state.buffer.redo() {
            self.state.refresh_wordcount();
            let n = self.state.buffer.len_chars();
            self.cursor = self.cursor.min(n);
            self.anchor = self.anchor.min(n);
            self.bump();
            self.refresh_search();
            self.schedule_autosave(cx);
        }
        cx.notify();
    }

    // ---------- files ----------
    fn save(&mut self, _: &Save, _w: &mut Window, cx: &mut Context<Self>) {
        if self.state.file_path.is_some() {
            let p = self.state.file_path.clone().unwrap();
            self.state.save_to(&p);
        } else {
            self.save_as_dialog(cx);
        }
        cx.notify();
    }
    fn save_as_dialog(&mut self, cx: &mut Context<Self>) {
        if let Some(p) = rfd::FileDialog::new()
            .add_filter("Markdown", &["md", "markdown"])
            .save_file()
        {
            self.state.save_to(&p);
        }
        cx.notify();
    }
    fn save_as(&mut self, _: &SaveAs, _w: &mut Window, cx: &mut Context<Self>) {
        self.save_as_dialog(cx)
    }
    fn open_dialog(&mut self, _: &Open, _w: &mut Window, cx: &mut Context<Self>) {
        if let Some(p) = rfd::FileDialog::new()
            .add_filter("Markdown", &["md", "markdown"])
            .pick_file()
        {
            self.state.open(&p);
            self.cursor = 0;
            self.anchor = 0;
            self.set_scroll(0.0);
            self.bump();
            self.refresh_search();
        }
        cx.notify();
    }

    // ---------- markdown ----------
    fn toggle_wrap(&mut self, marker: &str, cx: &mut Context<Self>) {
        if self.modal_open() {
            return;
        }
        let (a, b) = self.sel();
        let t = self.text();
        if a == b {
            let upto: String = t.chars().take(a).collect();
            let line_start = a - upto.chars().rev().take_while(|&c| c != '\n').count();
            let line: String = t
                .chars()
                .skip(line_start)
                .take_while(|&c| c != '\n')
                .collect();
            let off = a - line_start;
            let kind = match marker {
                "**" => InlineKind::Bold,
                "~~" => InlineKind::Strikethrough,
                _ => InlineKind::Italic,
            };
            let mut span = None;
            for m in inline_markup(&line) {
                if m.kind == kind {
                    let s = m.markers[0].start;
                    let e = m.markers[1].start + m.markers[1].length;
                    if off >= s && off <= e {
                        let base = line_start;
                        span = Some(nemiq_core::mutations::EmphasisSpan {
                            start: base + s,
                            end: base + e,
                            content_start: base + m.content.start,
                            content_end: base + m.content.start + m.content.length,
                            text_end: base + m.content.start + m.content.length,
                        });
                        break;
                    }
                }
            }
            let (nt, nc) = nemiq_core::mutations::toggle_wrap(&t, a, b, marker, span);
            self.state.set_text(nt);
            self.cursor = nc;
            self.anchor = nc;
        } else {
            let (nt, nc) = nemiq_core::mutations::toggle_wrap(&t, a, b, marker, None);
            self.state.set_text(nt);
            self.cursor = nc;
            self.anchor = nc;
        }
        self.bump();
        self.after_edit(cx);
    }

    fn insert_link(&mut self, _: &Link, _w: &mut Window, cx: &mut Context<Self>) {
        if self.modal_open() {
            return;
        }
        let (a, b) = self.sel();
        let selected: String = self
            .text()
            .chars()
            .skip(a)
            .take(b.saturating_sub(a))
            .collect();
        let url = nemiq_core::normalized_link_url(
            &cx.read_from_clipboard()
                .and_then(|i| i.text())
                .unwrap_or_default(),
        );
        let label = if selected.is_empty() {
            "link text".to_string()
        } else {
            selected.clone()
        };
        let url = if url.is_empty() {
            "https://".to_string()
        } else {
            url
        };
        let md = format!("[{label}]({url})");
        let caret = a + md.chars().count();
        self.replace(a, b, md, cx);
        if selected.is_empty() {
            // select the placeholder label
            self.anchor = a + 1;
            self.cursor = a + 1 + label.chars().count();
        } else {
            self.anchor = caret;
            self.cursor = caret;
        }
        cx.notify();
    }

    fn print_document(&mut self, _: &Print, _w: &mut Window, cx: &mut Context<Self>) {
        let stem = self
            .state
            .file_name()
            .trim_end_matches(".md")
            .trim_end_matches(".markdown")
            .to_string();
        let Some(path) = rfd::FileDialog::new()
            .set_file_name(format!("{stem}.pdf"))
            .add_filter("PDF", &["pdf"])
            .add_filter("HTML", &["html", "htm"])
            .save_file()
        else {
            return;
        };
        let text = self.text();
        let title = self.state.file_name();
        let result = crate::export::export_to(
            &text,
            &path,
            &title,
            &self.state.theme,
            &self.state.editor_font,
        );
        self.state.status = match result {
            Ok(()) => format!(
                "Exported {}",
                path.file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default()
            ),
            Err(err) => format!("Export failed: {err}"),
        };
        cx.notify();
    }

    fn new_window(&mut self, _: &NewWindow, _w: &mut Window, _cx: &mut Context<Self>) {
        if let Ok(exe) = std::env::current_exe() {
            let mut cmd = std::process::Command::new(exe);
            if self.vim_enabled {
                cmd.arg("--vim");
            }
            let _ = cmd.spawn();
        }
    }

    fn toggle_fullscreen(
        &mut self,
        _: &ToggleFullscreen,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.toggle_fullscreen();
        cx.notify();
    }

    fn open_shortcuts(&mut self, _: &Shortcuts, _w: &mut Window, cx: &mut Context<Self>) {
        self.shortcuts_open = true;
        cx.notify();
    }

    // ---------- font picker ----------
    fn filtered_fonts(&self) -> Vec<String> {
        nemiq_core::filter_font_families(&self.font_names, &self.font_filter)
    }

    fn open_font_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.font_open = true;
        self.font_filter.clear();
        let fonts = self.filtered_fonts();
        self.font_index = fonts
            .iter()
            .position(|f| *f == self.state.editor_font)
            .unwrap_or(0);
        let rows = fonts.len().min(FONT_ROWS);
        self.font_window = self.font_index.saturating_sub(rows.saturating_sub(1));
        window.focus(&self.focus);
        cx.notify();
    }

    fn ensure_font_visible(&mut self, len: usize) {
        let rows = len.min(FONT_ROWS);
        if rows == 0 {
            self.font_window = 0;
            return;
        }
        if self.font_index < self.font_window {
            self.font_window = self.font_index;
        } else if self.font_index >= self.font_window + rows {
            self.font_window = self.font_index + 1 - rows;
        }
        self.font_window = self.font_window.min(len.saturating_sub(rows));
    }

    fn font_move(&mut self, dir: i32, cx: &mut Context<Self>) {
        let len = self.filtered_fonts().len();
        if len == 0 {
            return;
        }
        self.font_index = (self.font_index as i32 + dir).clamp(0, len as i32 - 1) as usize;
        self.ensure_font_visible(len);
        cx.notify();
    }

    fn font_scroll(&mut self, dir: i32, cx: &mut Context<Self>) {
        let len = self.filtered_fonts().len();
        let rows = len.min(FONT_ROWS);
        if rows == 0 || len <= rows {
            return;
        }
        self.font_window =
            (self.font_window as i32 + dir * 3).clamp(0, (len - rows) as i32) as usize;
        cx.notify();
    }

    fn choose_font(&mut self, family: &str, cx: &mut Context<Self>) {
        self.state.set_editor_font(family);
        self.font_open = false;
        self.font_filter.clear();
        self.bump();
        cx.notify();
    }

    fn choose_current_font(&mut self, cx: &mut Context<Self>) {
        let fonts = self.filtered_fonts();
        if let Some(family) = fonts.get(self.font_index).cloned() {
            self.choose_font(&family, cx);
        }
    }

    fn font_filter_push(&mut self, text: &str, cx: &mut Context<Self>) {
        self.font_filter.push_str(text);
        self.font_index = 0;
        self.font_window = 0;
        cx.notify();
    }

    fn font_filter_word_backspace(&mut self, cx: &mut Context<Self>) {
        let mut chars: Vec<char> = self.font_filter.chars().collect();
        let before = chars.len();
        while chars.last().is_some_and(|c| c.is_whitespace()) {
            chars.pop();
        }
        while chars
            .last()
            .is_some_and(|c| c.is_alphanumeric() || *c == '_')
        {
            chars.pop();
        }
        if chars.len() == before && !chars.is_empty() {
            chars.pop();
        }
        self.font_filter = chars.into_iter().collect();
        self.font_index = 0;
        self.font_window = 0;
        cx.notify();
    }

    fn font_filter_backspace(&mut self, cx: &mut Context<Self>) {
        self.font_filter.pop();
        self.font_index = 0;
        self.font_window = 0;
        cx.notify();
    }

    // ---------- unsaved-changes dialog ----------
    fn cycle_close_focus(&mut self, dir: i32, cx: &mut Context<Self>) {
        self.close_focus = (self.close_focus as i32 + dir).rem_euclid(3) as usize;
        cx.notify();
    }

    fn dialog_cancel(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.close_prompt = false;
        cx.notify();
    }

    fn dialog_discard(&mut self, window: &mut Window, _cx: &mut Context<Self>) {
        self.state.discard_recovery();
        self.close_approved = true;
        self.close_prompt = false;
        window.remove_window();
    }

    fn dialog_save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.state.file_path.is_some() {
            let path = self.state.file_path.clone().unwrap();
            if self.state.save_to(&path) {
                self.close_approved = true;
                window.remove_window();
                return;
            }
        } else if let Some(path) = rfd::FileDialog::new()
            .add_filter("Markdown", &["md", "markdown"])
            .save_file()
        {
            if self.state.save_to(&path) {
                self.close_approved = true;
                window.remove_window();
                return;
            }
        }
        self.close_prompt = false;
        cx.notify();
    }

    fn activate_close_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.close_focus {
            0 => self.dialog_cancel(window, cx),
            1 => self.dialog_discard(window, cx),
            _ => self.dialog_save(window, cx),
        }
    }

    fn copy_as_html(&mut self, _: &CopyAsHtml, _w: &mut Window, cx: &mut Context<Self>) {
        let text = self.text();
        let mut html = String::from("<body>");
        let parser = pulldown_cmark::Parser::new(&text);
        pulldown_cmark::html::push_html(&mut html, parser);
        html.push_str("</body>");
        // GPUI's clipboard only carries plain strings; use wl-copy for real HTML.
        let copied = std::process::Command::new("wl-copy")
            .arg("--type")
            .arg("text/html")
            .stdin(std::process::Stdio::piped())
            .spawn()
            .and_then(|mut child| {
                use std::io::Write;
                child
                    .stdin
                    .as_mut()
                    .map(|s| s.write_all(html.as_bytes()))
                    .transpose()?;
                child.wait()
            })
            .is_ok();
        if !copied {
            cx.write_to_clipboard(ClipboardItem::new_string(self.text()));
        }
        self.state.status = "Copied as HTML.".to_string();
        cx.notify();
    }

    // ---------- clipboard ----------
    fn paste(&mut self, _: &Paste, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|i| i.text()) {
            if self.font_open {
                self.font_filter_push(&text, cx);
                return;
            }
            if self.search_open {
                self.search_append(&text, cx);
                return;
            }
            let (a, b) = self.sel();
            self.replace(a, b, text, cx);
            self.ensure_cursor_visible(window, cx);
        }
    }
    fn cut(&mut self, _: &Cut, w: &mut Window, cx: &mut Context<Self>) {
        let (a, b) = self.sel();
        if a != b {
            let t = self.text();
            let sel: String = t.chars().skip(a).take(b - a).collect();
            cx.write_to_clipboard(ClipboardItem::new_string(sel));
            self.delete_selection_or(a, b, cx);
            let _ = w;
        }
    }
    fn copy(&mut self, _: &Copy, _w: &mut Window, cx: &mut Context<Self>) {
        let (a, b) = self.sel();
        if a != b {
            let t = self.text();
            let sel: String = t.chars().skip(a).take(b - a).collect();
            cx.write_to_clipboard(ClipboardItem::new_string(sel));
        }
    }

    fn indent(&mut self, _: &Indent, _w: &mut Window, cx: &mut Context<Self>) {
        let (a, b) = self.sel();
        if a == b {
            self.replace(a, b, "    ".to_string(), cx);
        } else {
            self.indent_lines(a, b, false, cx);
        }
    }
    fn outdent(&mut self, _: &Outdent, _w: &mut Window, cx: &mut Context<Self>) {
        let (a, b) = self.sel();
        self.indent_lines(a, b, true, cx);
    }
    fn indent_lines(&mut self, a: usize, b: usize, outdent: bool, cx: &mut Context<Self>) {
        if self.modal_open() || self.vim_blocks() {
            return;
        }
        let t = self.text();
        let chars: Vec<char> = t.chars().collect();
        let line_start = {
            let mut i = a;
            while i > 0 && chars[i - 1] != '\n' {
                i -= 1;
            }
            i
        };
        let line_end = {
            let mut i = b.max(a);
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
            i
        };
        let mut out = String::new();
        out.extend(chars[..line_start].iter());
        let block: String = chars[line_start..line_end].iter().collect();
        for (i, line) in block.split('\n').enumerate() {
            if i > 0 {
                out.push('\n');
            }
            let non_empty = !line.trim().is_empty();
            if outdent {
                let mut removed = 0;
                let mut rest = line;
                while removed < 4 {
                    if let Some(r) = rest.strip_prefix(' ') {
                        rest = r;
                        removed += 1;
                    } else if let Some(r) = rest.strip_prefix('\t') {
                        rest = r;
                        removed += 4;
                    } else {
                        break;
                    }
                }
                out.push_str(rest);
            } else if non_empty {
                out.push_str("    ");
                out.push_str(line);
            } else {
                out.push_str(line);
            }
        }
        out.extend(chars[line_end..].iter());
        let shift = out.chars().count() as i64 - chars.len() as i64;
        self.state.set_text(out);
        let new_a = (a as i64 + if outdent { -4 } else { 4 }).max(0) as usize;
        let new_b = (b as i64 + shift).max(new_a as i64) as usize;
        self.anchor = new_a;
        self.cursor = new_b;
        self.bump();
        self.after_edit(cx);
    }

    // ---------- search ----------
    fn refresh_search(&mut self) {
        if self.search_text.is_empty() {
            self.matches = vec![];
            self.match_idx = -1;
            return;
        }
        self.matches = nemiq_core::find_matches(&self.text(), &self.search_text);
        self.match_idx = if self.matches.is_empty() { -1 } else { 0 };
    }
    fn goto_match(&mut self, dir: i32, window: &mut Window, cx: &mut Context<Self>) {
        if self.matches.is_empty() {
            return;
        }
        let n = self.matches.len() as i32;
        self.match_idx = (self.match_idx + dir + n) % n;
        let pos = self.matches[self.match_idx as usize];
        self.anchor = pos;
        self.cursor = pos + self.search_text.chars().count();
        self.ensure_cursor_visible(window, cx);
        cx.notify();
    }
    fn open_search(&mut self, replace: bool, cx: &mut Context<Self>) {
        self.search_open = true;
        self.replace_open = replace;
        self.replace_focus = false;
        self.refresh_search();
        self.jump_to_first_match(cx);
        cx.notify();
    }

    fn search_replace_current(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.search_text.is_empty() || self.matches.is_empty() || self.match_idx < 0 {
            return;
        }
        let pos = self.matches[self.match_idx as usize];
        let qlen = self.search_text.chars().count();
        let caret = self
            .state
            .apply_replace(pos, pos + qlen, self.replace_text.clone());
        let _ = caret;
        self.refresh_search();
        if self.matches.is_empty() {
            self.match_idx = -1;
        } else {
            let next = self
                .matches
                .iter()
                .position(|&m| m >= pos + self.replace_text.chars().count())
                .unwrap_or(0);
            self.match_idx = next as i32;
            self.anchor = self.matches[next];
            self.cursor = self.anchor + self.search_text.chars().count();
        }
        self.after_edit(cx);
        self.ensure_cursor_visible(window, cx);
        cx.notify();
    }

    fn search_replace_all(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.search_text.is_empty() || self.matches.is_empty() {
            return;
        }
        let chars: Vec<char> = self.text().chars().collect();
        let qlen = self.search_text.chars().count();
        let mut out = String::new();
        let mut last = 0usize;
        for &m in &self.matches {
            if m < last || m + qlen > chars.len() {
                continue;
            }
            out.extend(chars[last..m].iter());
            out.push_str(&self.replace_text);
            last = m + qlen;
        }
        if last == 0 {
            return;
        }
        out.extend(chars[last..].iter());
        let total = chars.len();
        self.state.apply_replace(0, total, out);
        self.match_idx = -1;
        self.refresh_search();
        self.after_edit(cx);
        self.ensure_cursor_visible(window, cx);
        cx.notify();
    }
    fn close_search(&mut self, _: &CloseFind, window: &mut Window, cx: &mut Context<Self>) {
        if self.cmdline_open {
            self.cmdline_open = false;
            self.cmdline_text.clear();
            cx.notify();
            return;
        }
        if self.close_prompt {
            self.close_prompt = false;
            cx.notify();
            return;
        }
        if self.shortcuts_open || self.font_open {
            self.shortcuts_open = false;
            self.font_open = false;
            cx.notify();
            return;
        }
        if self.search_open {
            self.search_open = false;
            self.replace_open = false;
            self.replace_focus = false;
            if !self.vim_enabled {
                self.matches.clear();
                self.match_idx = -1;
            }
            cx.notify();
            return;
        }
        if self.vim_enabled {
            self.vim_key_apply(VimKey::Esc, window, cx);
        }
        cx.notify();
    }

    fn vim_key_apply(&mut self, key: VimKey, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.text();
        let cmd = self.vim.handle(&text, self.cursor, self.anchor, key);
        self.apply_vim(cmd, window, cx);
    }

    fn jump_to_first_match(&mut self, cx: &mut Context<Self>) {
        if let Some(&pos) = self.matches.first() {
            self.anchor = pos;
            self.cursor = pos + self.search_text.chars().count();
            cx.notify();
        }
    }

    fn search_word_backspace(&mut self, cx: &mut Context<Self>) {
        let mut chars: Vec<char> = self.search_text.chars().collect();
        let before = chars.len();
        while chars.last().is_some_and(|c| c.is_whitespace()) {
            chars.pop();
        }
        while chars
            .last()
            .is_some_and(|c| c.is_alphanumeric() || *c == '_')
        {
            chars.pop();
        }
        if chars.len() == before && !chars.is_empty() {
            chars.pop();
        }
        self.search_text = chars.into_iter().collect();
        self.refresh_search();
        self.jump_to_first_match(cx);
        cx.notify();
    }

    fn search_backspace_impl(&mut self, cx: &mut Context<Self>) {
        let mut chars: Vec<char> = self.search_text.chars().collect();
        chars.pop();
        self.search_text = chars.into_iter().collect();
        self.refresh_search();
        self.jump_to_first_match(cx);
        cx.notify();
    }

    fn search_append(&mut self, text: &str, cx: &mut Context<Self>) {
        self.search_text.push_str(text);
        self.refresh_search();
        self.jump_to_first_match(cx);
        cx.notify();
    }

    // ---------- vim ----------
    fn vim_key(keystroke: &Keystroke) -> Option<VimKey> {
        match keystroke.key.as_str() {
            "escape" => Some(VimKey::Esc),
            "enter" => Some(VimKey::Enter),
            "backspace" => Some(VimKey::Backspace),
            "space" => Some(VimKey::Space),
            "tab" => Some(VimKey::Tab),
            "left" => Some(VimKey::Left),
            "right" => Some(VimKey::Right),
            "up" => Some(VimKey::Up),
            "down" => Some(VimKey::Down),
            "home" => Some(VimKey::Home),
            "end" => Some(VimKey::End),
            "delete" => Some(VimKey::Delete),
            "pageup" => Some(VimKey::PageUp),
            "pagedown" => Some(VimKey::PageDown),
            _ => {
                let ch = keystroke
                    .key_char
                    .as_deref()
                    .and_then(|s| s.chars().next())
                    .or_else(|| {
                        let mut chars = keystroke.key.chars();
                        match (chars.next(), chars.next()) {
                            (Some(c), None) => Some(c),
                            _ => None,
                        }
                    })?;
                Some(VimKey::Char(ch))
            }
        }
    }

    fn on_vim_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if !self.vim_enabled || self.modal_open() {
            return;
        }
        let ks = &event.keystroke;
        let m = ks.modifiers;
        if m.control || m.alt || m.platform || m.function {
            if m.control && !m.alt && !m.platform && self.vim.is_normal() && ks.key == "r" {
                self.redo(&Redo, window, cx);
                cx.stop_propagation();
            }
            return;
        }
        let Some(key) = Self::vim_key(ks) else {
            return;
        };
        let text = self.text();
        let cmd = self.vim.handle(&text, self.cursor, self.anchor, key);
        let handled = !matches!(cmd, VimCommand::None) || self.vim.is_normal();
        self.apply_vim(cmd, window, cx);
        if handled {
            cx.stop_propagation();
        }
    }

    fn apply_vim(&mut self, cmd: VimCommand, window: &mut Window, cx: &mut Context<Self>) {
        match cmd {
            VimCommand::None => {}
            VimCommand::Move { pos, extend } => {
                let pos = pos.min(self.state.buffer.len_chars());
                self.cursor = pos;
                if !extend {
                    self.anchor = pos;
                }
                self.ensure_cursor_visible(window, cx);
                cx.notify();
            }
            VimCommand::Mode { mode, pos, anchor } => {
                self.vim.mode = mode;
                let len = self.state.buffer.len_chars();
                self.cursor = pos.min(len);
                self.anchor = anchor.min(len);
                self.ensure_cursor_visible(window, cx);
                cx.notify();
            }
            VimCommand::Edit {
                start,
                end,
                text,
                pos,
                anchor,
            } => {
                self.state.apply_replace(start, end, text);
                let len = self.state.buffer.len_chars();
                self.cursor = pos.min(len);
                self.anchor = anchor.min(len);
                self.after_edit(cx);
                self.ensure_cursor_visible(window, cx);
                cx.notify();
            }
            VimCommand::Yank {
                start,
                end,
                linewise,
            } => {
                let t = self.text();
                let yanked: String = t
                    .chars()
                    .skip(start)
                    .take(end.saturating_sub(start))
                    .collect();
                cx.write_to_clipboard(ClipboardItem::new_string(yanked.clone()));
                self.state.status = if linewise {
                    format!("Yanked {} line(s)", yanked.matches('\n').count().max(1))
                } else {
                    format!("Yanked {}", yanked.chars().count())
                };
                if self.vim.is_visual() {
                    self.vim.mode = VimMode::Normal;
                    self.cursor = start;
                    self.anchor = start;
                }
                cx.notify();
            }
            VimCommand::Search { .. } => {
                self.search_open = true;
                self.replace_open = false;
                self.search_text.clear();
                self.matches.clear();
                self.match_idx = -1;
                cx.notify();
            }
            VimCommand::SearchNext { forward } => {
                self.goto_match(if forward { 1 } else { -1 }, window, cx)
            }
            VimCommand::CommandLine => {
                self.cmdline_open = true;
                self.cmdline_text.clear();
                cx.notify();
            }
            VimCommand::Save => self.save(&Save, window, cx),
            VimCommand::Quit { force } => self.vim_quit(force, window, cx),
            VimCommand::SaveQuit => {
                self.save(&Save, window, cx);
                if !self.state.modified {
                    self.vim_quit(true, window, cx);
                }
            }
            VimCommand::Undo => self.undo(&Undo, window, cx),
        }
    }

    fn vim_quit(&mut self, force: bool, window: &mut Window, cx: &mut Context<Self>) {
        if force || !self.state.modified {
            self.state.discard_recovery();
            self.close_approved = true;
            window.remove_window();
            return;
        }
        self.close_prompt = true;
        self.close_focus = 2;
        cx.notify();
    }

    fn cmdline_execute(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let input = self.cmdline_text.trim().to_string();
        self.cmdline_open = false;
        self.cmdline_text.clear();
        let mut parts = input.split_whitespace();
        let cmd = parts.next().unwrap_or("");
        match cmd {
            "" => {}
            "w" | "write" => self.save(&Save, window, cx),
            "q" | "quit" => self.vim_quit(false, window, cx),
            "q!" => self.vim_quit(true, window, cx),
            "wq" | "x" => {
                self.save(&Save, window, cx);
                if !self.state.modified {
                    self.vim_quit(true, window, cx);
                }
            }
            "noh" | "nohlsearch" => {
                self.search_open = false;
                self.search_text.clear();
                self.matches.clear();
                self.match_idx = -1;
                self.state.status = "Search cleared".to_string();
            }
            "e" | "edit" => {
                if let Some(path) = self.state.file_path.clone() {
                    self.state.open(&path);
                    self.cursor = 0;
                    self.anchor = 0;
                    self.bump();
                    self.refresh_search();
                }
            }
            "set" => match parts.next() {
                Some("vim") => {
                    self.vim_enabled = true;
                    self.vim = VimState::with_mode(VimMode::Normal);
                    self.state.save_vim_pref(true);
                    self.state.status = "Vim mode on".to_string();
                }
                Some("novim") => {
                    self.vim_enabled = false;
                    self.vim = VimState::new();
                    self.state.save_vim_pref(false);
                    self.state.status = "Vim mode off".to_string();
                }
                _ => self.state.status = "set: expected vim or novim".to_string(),
            },
            other => self.state.status = format!("Unknown command: :{other}"),
        }
        cx.notify();
    }

    // ---------- zoom ----------
    fn zoom_in(&mut self, _: &ZoomIn, _w: &mut Window, cx: &mut Context<Self>) {
        let z = self.state.zoom.step(1);
        self.state.set_zoom(z);
        self.bump();
        cx.notify();
    }
    fn zoom_out(&mut self, _: &ZoomOut, _w: &mut Window, cx: &mut Context<Self>) {
        let z = self.state.zoom.step(-1);
        self.state.set_zoom(z);
        self.bump();
        cx.notify();
    }
    fn zoom_reset(&mut self, _: &ZoomReset, _w: &mut Window, cx: &mut Context<Self>) {
        self.state.set_zoom(1.0);
        self.bump();
        cx.notify();
    }

    // ---------- mouse ----------
    fn scrollbar_at(&mut self, window: &mut Window) -> Option<Scrollbar> {
        let layout = self.layout(window);
        let vp = window.viewport_size();
        let area = Bounds::new(
            point(Pixels::ZERO, Pixels::ZERO),
            size(vp.width, self.text_area_height(vp)),
        );
        scrollbar_geometry(
            area,
            self.scroll_y,
            layout.total_height,
            self.visible_text_height(vp),
            self.state.text_scale() as f32,
        )
    }

    /// Returns true when the press was consumed by the scrollbar.
    fn scrollbar_mouse_down(
        &mut self,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let scale = self.state.text_scale() as f32;
        let Some(sb) = self.scrollbar_at(window) else {
            return false;
        };
        let x = f32::from(position.x);
        if x < f32::from(sb.x) - 6.0 * scale || x > f32::from(sb.x + sb.width) + 4.0 * scale {
            return false;
        }
        let y = position.y;
        window.focus(&self.focus);
        if y >= sb.thumb_top && y <= sb.thumb_top + sb.thumb_height {
            self.scrollbar_drag = Some(f32::from(y - sb.thumb_top));
        } else {
            let vp = window.viewport_size();
            let page = f32::from(self.visible_text_height(vp)) * 0.9;
            let delta = if y < sb.thumb_top { -page } else { page };
            let y = (self.scroll_y + delta).clamp(0.0, sb.max_scroll);
            self.set_scroll(y);
            self.scrollbar_drag = Some(f32::from(sb.thumb_height) / 2.0);
            self.reveal_scrollbar(cx);
            cx.notify();
        }
        true
    }

    fn scrollbar_mouse_move(
        &mut self,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(grab) = self.scrollbar_drag else {
            return false;
        };
        if let Some(sb) = self.scrollbar_at(window) {
            let travel = f32::from(sb.track_height - sb.thumb_height).max(1.0);
            let offset =
                (f32::from(position.y) - f32::from(sb.track_top) - grab).clamp(0.0, travel);
            let y = offset / travel * sb.max_scroll;
            self.set_scroll(y);
            self.reveal_scrollbar(cx);
            cx.notify();
        }
        true
    }

    fn index_at(&mut self, point: Point<Pixels>, window: &mut Window) -> usize {
        let layout = self.layout(window);
        let vp = window.viewport_size();
        let x0 = (vp.width - layout.wrap_width) / 2.0;
        let y = point.y + px(self.scroll_y) - self.top_margin(window.viewport_size());
        let x = point.x - x0;
        index_for_layout_point(&layout, x, y.max(px(0.)))
    }

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.close_prompt || self.pending_external.is_some() {
            return;
        }
        if self.font_open {
            // Clicks on the panel itself stop propagation; anything reaching
            // here is a click outside and dismisses the picker.
            self.font_open = false;
            self.font_filter.clear();
            cx.notify();
            return;
        }
        if self.shortcuts_open {
            self.shortcuts_open = false;
            cx.notify();
            return;
        }
        if self.search_open && f32::from(event.position.y) < 88.0 {
            return;
        }
        if self.scrollbar_mouse_down(event.position, window, cx) {
            return;
        }
        let vp = window.viewport_size();
        let bottom_reserved = f32::from(self.footer_height())
            + if self.search_open {
                f32::from(px(32.0 * self.state.text_scale() as f32))
            } else {
                0.0
            };
        if f32::from(event.position.y) > f32::from(vp.height) - bottom_reserved {
            return;
        }
        window.focus(&self.focus);
        let idx = self.index_at(event.position, window);
        if event.click_count >= 2 {
            let (a, b) = word_bounds(&self.text(), idx);
            self.anchor = a;
            self.cursor = b;
        } else {
            self.anchor = idx;
            self.cursor = idx;
        }
        self.selecting = true;
        cx.notify();
    }
    fn on_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.scrollbar_mouse_move(event.position, window, cx) {
            return;
        }
        // Hovering the track reveals the bar too.
        let scale = self.state.text_scale() as f32;
        if let Some(sb) = self.scrollbar_at(window) {
            let x = f32::from(event.position.x);
            if x >= f32::from(sb.x) - 6.0 * scale
                && x <= f32::from(sb.x + sb.width) + 4.0 * scale
                && self.scrollbar_alpha < 1.0
            {
                self.reveal_scrollbar(cx);
            }
        }
        if self.selecting && event.dragging() {
            self.cursor = self.index_at(event.position, window);
            cx.notify();
        }
    }
    fn on_mouse_up(&mut self, _: &MouseUpEvent, _w: &mut Window, _cx: &mut Context<Self>) {
        self.selecting = false;
        self.scrollbar_drag = None;
    }

    fn on_scroll(&mut self, event: &ScrollWheelEvent, window: &mut Window, cx: &mut Context<Self>) {
        if event.modifiers.control {
            let up = match event.delta {
                ScrollDelta::Pixels(p) => f32::from(p.y) > 0.0,
                ScrollDelta::Lines(p) => p.y > 0.0,
            };
            if up {
                self.zoom_in(&ZoomIn, window, cx);
            } else {
                self.zoom_out(&ZoomOut, window, cx);
            }
            return;
        }
        // One physical wheel notch travels 120 logical pixels; touchpads report
        // pixels and are used as-is.
        let scale = self.state.text_scale() as f32;
        let dy = match event.delta {
            ScrollDelta::Pixels(p) => f32::from(p.y),
            // Wayland mouse wheels arrive as two summed sources in GPUI
            // (axis_discrete + axis_value120), so a physical notch is 6 here,
            // not the nominal 3. One notch must still move 120 logical px.
            ScrollDelta::Lines(p) => p.y * 20.0 * scale,
        };
        if self.font_open {
            if dy != 0.0 {
                self.font_scroll(if dy > 0.0 { -1 } else { 1 }, cx);
            }
            return;
        }
        if dy == 0.0 {
            return;
        }
        let max = self.max_scroll(window);
        self.scroll_target = (self.scroll_target - dy).clamp(0.0, max);
        self.reveal_scrollbar(cx);
        self.animate_scroll(cx);
    }
}

impl Focusable for EditorView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl EditorView {
    fn to_utf16(&self, off: usize) -> usize {
        self.chars().iter().take(off).map(|c| c.len_utf16()).sum()
    }
    fn from_utf16(&self, off: usize) -> usize {
        let mut u16c = 0;
        for (i, c) in self.chars().iter().enumerate() {
            if u16c >= off {
                return i;
            }
            u16c += c.len_utf16();
        }
        self.chars().len()
    }
}

impl EntityInputHandler for EditorView {
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        adjusted_range: &mut Option<Range<usize>>,
        _w: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        let t = self.text();
        let a = self.from_utf16(range.start);
        let b = self.from_utf16(range.end);
        adjusted_range.replace(self.to_utf16(a)..self.to_utf16(b));
        Some(t.chars().skip(a).take(b.saturating_sub(a)).collect())
    }
    fn selected_text_range(
        &mut self,
        _: bool,
        _w: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        let (a, b) = self.sel();
        Some(UTF16Selection {
            range: self.to_utf16(a)..self.to_utf16(b),
            reversed: self.cursor < self.anchor,
        })
    }
    fn marked_text_range(&self, _w: &mut Window, _cx: &mut Context<Self>) -> Option<Range<usize>> {
        None
    }
    fn unmark_text(&mut self, _w: &mut Window, _cx: &mut Context<Self>) {}
    fn replace_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        new_text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (a, b) = range
            .map(|r| (self.from_utf16(r.start), self.from_utf16(r.end)))
            .unwrap_or_else(|| self.sel());
        if self.close_prompt || self.pending_external.is_some() {
            return;
        }
        if self.cmdline_open {
            self.cmdline_text.push_str(new_text);
            cx.notify();
            return;
        }
        if self.font_open {
            self.font_filter_push(new_text, cx);
            return;
        }
        if self.search_open {
            if self.replace_open && self.replace_focus {
                self.replace_text.push_str(new_text);
                cx.notify();
            } else {
                self.search_append(new_text, cx);
            }
            return;
        }
        if self.vim_blocks() {
            return;
        }
        if new_text.chars().all(|c| c == '\n' || c == '\r') && !new_text.is_empty() {
            self.smart_return(a, b, cx);
            return;
        }
        self.replace(a, b, new_text.to_string(), cx);
        self.ensure_cursor_visible(window, cx);
    }
    fn replace_and_mark_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        new_text: &str,
        _sel: Option<Range<usize>>,
        _w: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.vim_blocks() {
            return;
        }
        let (a, b) = range
            .map(|r| (self.from_utf16(r.start), self.from_utf16(r.end)))
            .unwrap_or_else(|| self.sel());
        self.replace(a, b, new_text.to_string(), cx);
    }
    fn bounds_for_range(
        &mut self,
        _range_utf16: Range<usize>,
        _element_bounds: Bounds<Pixels>,
        _w: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        None
    }
    fn character_index_for_point(
        &mut self,
        point: Point<Pixels>,
        window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        let idx = self.index_at(point, window);
        Some(self.to_utf16(idx))
    }
}

impl EditorView {
    fn smart_return(&mut self, a: usize, b: usize, cx: &mut Context<Self>) {
        if a != b {
            self.replace(a, b, "\n".to_string(), cx);
            return;
        }
        let t = self.text();
        let chars: Vec<char> = t.chars().collect();
        let mut ls = a;
        while ls > 0 && chars[ls - 1] != '\n' {
            ls -= 1;
        }
        let mut le = a;
        while le < chars.len() && chars[le] != '\n' {
            le += 1;
        }
        let line: String = chars[ls..le].iter().collect();
        let head: String = chars[ls..a].iter().collect();
        let rest: String = chars[a..le].iter().collect();

        // List continuation: keep the marker, or drop an empty item.
        if let Some(trimmed) = list_prefix(&line) {
            if line.trim().is_empty() || rest.trim().is_empty() {
                // empty item: drop the marker and leave a plain blank line
                self.replace(ls, le, String::new(), cx);
                self.replace(ls, ls, "\n".to_string(), cx);
                return;
            }
            self.replace(a, a, format!("\n{trimmed}"), cx);
            return;
        }
        if inside_fence_at(&t, a) {
            self.replace(a, a, "\n".to_string(), cx);
            return;
        }
        let ins = if head.trim().is_empty() && rest.trim().is_empty() {
            "\n"
        } else {
            "\n\n"
        };
        self.replace(a, a, ins.to_string(), cx);
    }
}

/// True when the caret sits in a fenced block, including just after an opening
/// fence's backticks or before a closing one.
fn inside_fence_at(text: &str, offset: usize) -> bool {
    let mut line_start = 0usize;
    let mut line_ix = 0usize;
    for (i, c) in text.chars().enumerate() {
        if i >= offset {
            break;
        }
        if c == '\n' {
            line_ix += 1;
            line_start = i + 1;
        }
    }
    let line_text = text
        .chars()
        .skip(line_start)
        .take_while(|&c| c != '\n')
        .collect::<String>();
    let col = offset - line_start;
    let fences = fence_states(text);
    let here = *fences.get(line_ix).unwrap_or(&false);
    let prev = line_ix
        .checked_sub(1)
        .and_then(|i| fences.get(i).copied())
        .unwrap_or(false);
    if here && !prev {
        // opening fence line: only past its backticks is code
        if let Some(pos) = line_text.find(['`', '~']) {
            let run = line_text[pos..]
                .chars()
                .take_while(|&c| c == '`' || c == '~')
                .count();
            return col >= pos + run.max(3);
        }
        return false;
    }
    if prev && !here {
        // closing fence: anywhere before its end
        return col < line_text.chars().count();
    }
    here
}

fn list_prefix(line: &str) -> Option<String> {
    let indent: String = line.chars().take_while(|c| c.is_whitespace()).collect();
    let t = line.trim_start();
    if let Some(rest) = t
        .strip_prefix("- ")
        .or(t.strip_prefix("* ").or(t.strip_prefix("+ ")))
    {
        if rest.trim().is_empty() {
            return None;
        }
        return Some(format!("{indent}{} ", &t[..1]));
    }
    let mut num_end = 0usize;
    for (i, c) in t.char_indices() {
        if c.is_ascii_digit() {
            num_end = i + c.len_utf8();
        } else {
            break;
        }
    }
    if num_end > 0 {
        let rest = &t[num_end..];
        if rest.starts_with(". ") || rest.starts_with(") ") {
            if rest.trim().is_empty() {
                return None;
            }
            let n: u64 = t[..num_end].parse().unwrap_or(1);
            let sep = &rest[..2];
            return Some(format!("{indent}{n}{sep}"));
        }
    }
    if t.starts_with("> ") || t == ">" {
        return Some(format!("{indent}> "));
    }
    None
}

fn word_bounds(text: &str, at: usize) -> (usize, usize) {
    let chars: Vec<char> = text.chars().collect();
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    let mut a = at.min(chars.len());
    let mut b = a;
    while a > 0 && is_word(chars[a - 1]) {
        a -= 1;
    }
    while b < chars.len() && is_word(chars[b]) {
        b += 1;
    }
    (a, b)
}

// ---------------- layout helpers ----------------

fn caret_line_rect(layout: &Layout, cursor: usize) -> Option<(Pixels, Pixels)> {
    let line = layout.lines.iter().rev().find(|l| l.start_char <= cursor)?;
    let local = cursor - line.start_char;
    let y = caret_in_line_y(&line.shaped, line.display_col(local), layout.line_height);
    Some((line.top + y, layout.line_height))
}

fn caret_x(layout: &Layout, cursor: usize) -> Option<(usize, Pixels)> {
    for (i, line) in layout.lines.iter().enumerate() {
        if cursor >= line.start_char && cursor <= line.start_char + line.char_len {
            let local = line.display_col(cursor - line.start_char);
            let byte = char_to_byte(line.shaped.text.as_ref(), local);
            let p = line.shaped.position_for_index(byte, layout.line_height);
            return Some((i, px(p.map(|p| f32::from(p.x)).unwrap_or(0.0))));
        }
    }
    None
}

fn caret_in_line_y(shaped: &WrappedLine, local_chars: usize, lh: Pixels) -> Pixels {
    let byte = char_to_byte(shaped.text.as_ref(), local_chars);
    shaped
        .position_for_index(byte, lh)
        .map(|p| px(f32::from(p.y)))
        .unwrap_or(px(0.))
}

fn index_for_layout_point(layout: &Layout, x: Pixels, y: Pixels) -> usize {
    for line in layout.lines.iter() {
        if y < line.top + line.height || std::ptr::eq(line, layout.lines.last().unwrap()) {
            let local = point(x.max(px(0.)), (y - line.top).max(px(0.)));
            let byte = match line
                .shaped
                .closest_index_for_position(local, layout.line_height)
            {
                Ok(b) => b,
                Err(b) => b,
            };
            let display_chars = byte_to_char(line.shaped.text.as_ref(), byte);
            let local_chars = line.display_to_doc(display_chars).min(line.char_len);
            return line.start_char + local_chars;
        }
    }
    0
}

fn char_to_byte(s: &str, chars: usize) -> usize {
    s.char_indices()
        .nth(chars)
        .map(|(b, _)| b)
        .unwrap_or(s.len())
}
fn byte_to_char(s: &str, byte: usize) -> usize {
    s[..byte.min(s.len())].chars().count()
}

/// Whether each source line is inside (or is) a fenced code block.
pub fn fence_states(text: &str) -> Vec<bool> {
    let lines: Vec<&str> = text.split('\n').collect();
    let mut inside = false;
    let mut fence_char = '`';
    let mut fence_len = 0usize;
    let mut out = vec![false; lines.len()];
    for (i, line) in lines.iter().enumerate() {
        if inside {
            out[i] = true;
            if nemiq_core::closes_fence(line, fence_len, fence_char) {
                inside = false;
            }
            continue;
        }
        let ol = opening_fence_length(line);
        if ol > 0 {
            inside = true;
            fence_len = ol;
            fence_char = if line.trim_start().starts_with('~') {
                '~'
            } else {
                '`'
            };
            out[i] = true;
        }
    }
    out
}

// ---------------- markdown runs ----------------

#[derive(Clone, PartialEq)]
struct CharStyle {
    weight: FontWeight,
    italic: bool,
    color: Hsla,
    underline: bool,
    strike: bool,
    bg: Option<Hsla>,
    family: SharedString,
}

fn normal_style(p: &Palette) -> CharStyle {
    CharStyle {
        weight: FontWeight::NORMAL,
        italic: false,
        color: p.fg,
        underline: false,
        strike: false,
        bg: None,
        family: p.mono.clone(),
    }
}

fn line_styles(line: &str, p: &Palette, fence: bool) -> Vec<CharStyle> {
    let chars: Vec<char> = line.chars().collect();
    let mut styles = vec![normal_style(p); chars.len()];
    let set =
        |styles: &mut Vec<CharStyle>, start: usize, len: usize, f: &dyn Fn(&mut CharStyle)| {
            for i in start..(start + len).min(styles.len()) {
                f(&mut styles[i]);
            }
        };

    if fence {
        for s in styles.iter_mut() {
            s.bg = Some(p.code_bg);
        }
        let leading = chars.iter().take_while(|&&c| c == ' ').count();
        let run = chars[leading..]
            .iter()
            .take_while(|&&c| c == '`' || c == '~')
            .count();
        set(&mut styles, leading, run, &|s| s.color = p.marker);
        return styles;
    }

    if let Some(h) = heading_marker(&chars) {
        set(&mut styles, 0, h, &|s| s.color = p.marker);
        for s in styles.iter_mut().skip(h) {
            s.weight = FontWeight::BOLD;
        }
    } else if let Some((m, rest)) = quote_marker(&chars) {
        set(&mut styles, 0, m, &|s| s.color = p.marker);
        for s in styles.iter_mut().skip(rest) {
            s.italic = true;
            s.color = p.muted;
        }
    } else if let Some(m) = list_marker(&chars) {
        set(&mut styles, 0, m, &|s| s.color = p.marker);
    }
    if is_thematic_break(line) {
        for s in styles.iter_mut() {
            s.color = p.marker;
        }
    }

    for span in nemiq_core::code_spans(line) {
        set(&mut styles, span.start, span.length, &|s| {
            s.bg = Some(p.code_bg);
            s.italic = false;
            s.weight = FontWeight::NORMAL;
        });
    }
    for m in inline_markup(line) {
        let (cs, cl) = (m.content.start, m.content.length);
        match m.kind {
            InlineKind::Bold => set(&mut styles, cs, cl, &|s| s.weight = FontWeight::BOLD),
            InlineKind::Italic => set(&mut styles, cs, cl, &|s| s.italic = true),
            InlineKind::Strikethrough => set(&mut styles, cs, cl, &|s| s.strike = true),
            InlineKind::Link => set(&mut styles, cs, cl, &|s| {
                s.color = p.accent;
                s.underline = true;
            }),
        }
        for marker in m.markers {
            set(&mut styles, marker.start, marker.length, &|s| {
                s.color = p.marker;
                s.weight = FontWeight::NORMAL;
                s.italic = false;
                s.underline = false;
            });
        }
    }
    styles
}

/// Marker characters hidden unless the caret sits in or against their markup.
/// Returns the visible line, and for every visible char its offset in the
/// source line plus the original width of the run it replaced.
fn build_display(
    line: &str,
    fence: bool,
    line_start: usize,
    cursor: usize,
) -> (Vec<char>, Vec<usize>, Vec<usize>) {
    let chars: Vec<char> = line.chars().collect();
    let mut hide = vec![false; chars.len()];
    if !fence {
        for m in inline_markup(line) {
            let start = m.markers[0].start;
            let end = m.markers[1].start + m.markers[1].length;
            let caret_local = cursor.saturating_sub(line_start);
            let at_caret = caret_local >= start && caret_local <= end;
            if !at_caret {
                for marker in m.markers {
                    for i in marker.start..(marker.start + marker.length).min(hide.len()) {
                        hide[i] = true;
                    }
                }
            }
        }
    }
    let mut display = Vec::with_capacity(chars.len());
    let mut disp_to_doc = Vec::with_capacity(chars.len());
    let mut doc_to_disp = vec![usize::MAX; chars.len()];
    for (i, &c) in chars.iter().enumerate() {
        if hide[i] {
            continue;
        }
        doc_to_disp[i] = display.len();
        disp_to_doc.push(i);
        display.push(c);
    }
    (display, disp_to_doc, doc_to_disp)
}

fn styles_to_runs(display: &[char], styles: &[CharStyle], p: &Palette) -> Vec<TextRun> {
    let _ = p;
    // Coalesce equal neighbouring styles into runs (byte lengths).
    let mut runs = Vec::new();
    let mut i = 0usize;
    let chars = display;
    while i < chars.len() {
        let style = styles[i].clone();
        let mut j = i + 1;
        while j < chars.len() && styles[j] == style {
            j += 1;
        }
        let byte_len: usize = chars[i..j].iter().map(|c| c.len_utf8()).sum();
        runs.push(TextRun {
            len: byte_len,
            font: Font {
                family: style.family,
                features: FontFeatures::default(),
                fallbacks: None,
                weight: style.weight,
                style: if style.italic {
                    FontStyle::Italic
                } else {
                    FontStyle::Normal
                },
            },
            color: style.color,
            background_color: style.bg,
            underline: style.underline.then(|| UnderlineStyle {
                color: Some(p.accent),
                thickness: px(1.),
                wavy: false,
            }),
            strikethrough: style.strike.then(|| StrikethroughStyle {
                color: Some(p.fg),
                thickness: px(1.),
            }),
        });
        i = j;
    }
    runs
}

fn heading_marker(chars: &[char]) -> Option<usize> {
    let n = chars.iter().take_while(|&&c| c == '#').count();
    if (1..=6).contains(&n) && chars.get(n) == Some(&' ') {
        Some(n + 1)
    } else {
        None
    }
}

/// (marker width incl. spaces, content start)
fn quote_marker(chars: &[char]) -> Option<(usize, usize)> {
    let mut i = 0;
    while i < chars.len() && chars[i] == ' ' {
        i += 1;
    }
    let start = i;
    let mut marks = 0;
    while i < chars.len() && chars[i] == '>' {
        marks += 1;
        i += 1;
    }
    if marks == 0 {
        return None;
    }
    if i < chars.len() && chars[i] == ' ' {
        i += 1;
    }
    Some((i - start, i))
}

fn list_marker(chars: &[char]) -> Option<usize> {
    let mut i = 0;
    while i < chars.len() && chars[i] == ' ' {
        i += 1;
    }
    let start = i;
    if i < chars.len() && matches!(chars[i], '-' | '+' | '*') {
        let m = i;
        i += 1;
        if i < chars.len() && chars[i] == ' ' {
            // a thematic break is not a bullet
            if is_thematic_break(&chars.iter().collect::<String>()) {
                return None;
            }
            return Some(i + 1 - 0 * m);
        }
    }
    let digits = chars[i..].iter().take_while(|c| c.is_ascii_digit()).count();
    if digits > 0 {
        i += digits;
        if i + 1 < chars.len() && (chars[i] == '.' || chars[i] == ')') && chars[i + 1] == ' ' {
            return Some(i + 2);
        }
    }
    let _ = start;
    None
}

// ---------------- element ----------------

pub struct EditorElement {
    pub view: Entity<EditorView>,
}

impl IntoElement for EditorElement {
    type Element = Self;
    fn into_element(self) -> Self::Element {
        self
    }
}

pub struct Prepaint {
    layout: Arc<Layout>,
    palette: Palette,
    cursor: usize,
    anchor: usize,
    search_text: String,
    matches: Vec<usize>,
    match_idx: i32,
    x0: Pixels,
    y0: Pixels,
    scroll: Pixels,
    focused: bool,
    scrollbar_alpha: f32,
    vim_normal: bool,
}

impl Element for EditorElement {
    type RequestLayoutState = ();
    type PrepaintState = Prepaint;

    fn id(&self) -> Option<ElementId> {
        None
    }
    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }
    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _insp: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = relative(1.).into();
        let _ = window;
        (window.request_layout(style, [], cx), ())
    }
    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _insp: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _req: &mut Self::RequestLayoutState,
        _window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let view = self.view.read(cx);
        let layout = match &view.cache {
            Some(c)
                if c.revision == view.revision
                    && c.wrap_width == view.wrap_width_for(_window)
                    && c.font_size == view.font_size()
                    && c.cursor == view.cursor =>
            {
                c.clone()
            }
            _ => Arc::new(view.build_layout(
                _window,
                view.wrap_width_for(_window),
                view.font_size(),
                view.cache.as_deref(),
            )),
        };
        Prepaint {
            x0: bounds.left() + (bounds.size.width - layout.wrap_width) / 2.0,
            y0: bounds.top() + view.top_margin(_window.viewport_size()),
            scroll: px(view.scroll_y),
            scrollbar_alpha: view.scrollbar_alpha,
            layout,
            palette: Palette::new(&view.state),
            cursor: view.cursor,
            anchor: view.anchor,
            search_text: view.search_text.clone(),
            matches: view.matches.clone(),
            match_idx: view.match_idx,
            focused: view.focus.is_focused(_window),
            vim_normal: view.vim_enabled && view.vim.is_normal(),
        }
    }
    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _insp: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _req: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus = self.view.read(cx).focus.clone();
        window.handle_input(
            &focus,
            ElementInputHandler::new(bounds, self.view.clone()),
            cx,
        );

        let p = &prepaint;
        let lh = p.layout.line_height;
        window.paint_quad(fill(bounds, p.palette.bg));

        // Fence panels
        for line in &p.layout.lines {
            if !line.fence {
                continue;
            }
            let top = p.y0 + line.top - p.scroll;
            if top + line.height < bounds.top() || top > bounds.bottom() {
                continue;
            }
            window.paint_quad(fill(
                Bounds::new(point(p.x0, top), size(p.layout.wrap_width, line.height)),
                p.palette.code_bg,
            ));
        }

        // Text backgrounds (inline code, fence runs), below any selection.
        for line in &p.layout.lines {
            let top = p.y0 + line.top - p.scroll;
            if top + line.height < bounds.top() || top > bounds.bottom() {
                continue;
            }
            line.shaped
                .paint_background(point(p.x0, top), lh, TextAlign::Left, None, window, cx)
                .ok();
        }

        // Selection
        let (sa, sb) = (p.anchor.min(p.cursor), p.anchor.max(p.cursor));
        if sa != sb {
            for line in &p.layout.lines {
                let ls = line.start_char;
                let le = line.start_char + line.char_len;
                if sb < ls || sa > le {
                    continue;
                }
                let top = p.y0 + line.top - p.scroll;
                if top + line.height < bounds.top() || top > bounds.bottom() {
                    continue;
                }
                let la = sa.max(ls) - ls;
                let lb = sb.min(le) - ls;
                let (la, lb) = line.display_range(la, lb);
                if la >= lb {
                    continue;
                }
                paint_selection(
                    window,
                    &line.shaped,
                    la,
                    lb,
                    line.disp_to_doc.len(),
                    point(p.x0, top),
                    lh,
                    p.layout.wrap_width,
                    p.palette.selection,
                );
            }
        }

        // Search highlights
        if !p.search_text.is_empty() {
            let qlen = p.search_text.chars().count();
            for (mi, &m) in p.matches.iter().enumerate() {
                let color = if mi as i32 == p.match_idx {
                    p.palette.search_current
                } else {
                    p.palette.search
                };
                for line in &p.layout.lines {
                    let ls = line.start_char;
                    let le = line.start_char + line.char_len;
                    if m < ls || m + qlen > le {
                        continue;
                    }
                    let top = p.y0 + line.top - p.scroll;
                    if top + line.height < bounds.top() || top > bounds.bottom() {
                        continue;
                    }
                    let (la, lb) = line.display_range(m - ls, m - ls + qlen);
                    if la >= lb {
                        continue;
                    }
                    paint_selection(
                        window,
                        &line.shaped,
                        la,
                        lb,
                        line.disp_to_doc.len(),
                        point(p.x0, top),
                        lh,
                        p.layout.wrap_width,
                        color,
                    );
                }
            }
        }

        // Text
        for line in &p.layout.lines {
            let top = p.y0 + line.top - p.scroll;
            if top + line.height < bounds.top() {
                continue;
            }
            if top > bounds.bottom() {
                break;
            }
            line.shaped
                .paint(point(p.x0, top), lh, TextAlign::Left, None, window, cx)
                .ok();
        }

        // Caret
        if p.focused && sa == sb {
            if let Some((line_ix, x)) = caret_x(&p.layout, p.cursor) {
                let line = &p.layout.lines[line_ix];
                let y = p.y0 + line.top - p.scroll
                    + caret_in_line_y(
                        &line.shaped,
                        line.display_col(p.cursor - line.start_char),
                        lh,
                    );
                if y + lh >= bounds.top() && y <= bounds.bottom() {
                    if p.vim_normal {
                        // Block caret while Normal/Visual mode owns the keys.
                        window.paint_quad(fill(
                            Bounds::new(point(p.x0 + x, y), size(p.layout.font_size * 0.62, lh)),
                            p.palette.fg.opacity(0.35),
                        ));
                    } else {
                        window.paint_quad(fill(
                            Bounds::new(point(p.x0 + x, y), size(px(1.4), lh)),
                            p.palette.fg,
                        ));
                    }
                }
            }
        }

        // Placeholder
        if p.layout.lines.len() == 1 && p.layout.lines[0].char_len == 0 && !p.focused {
            let placeholder = "# Start writing";
            let style = window.text_style();
            let run = TextRun {
                len: placeholder.len(),
                font: Font {
                    family: p.palette.mono.clone(),
                    ..style.font()
                },
                color: p.palette.muted,
                background_color: None,
                underline: None,
                strikethrough: None,
            };
            if let Ok(mut lines) = window.text_system().shape_text(
                SharedString::from(placeholder),
                p.layout.font_size,
                &[run],
                None,
                None,
            ) {
                if let Some(l) = lines.drain(..).next() {
                    l.paint(point(p.x0, p.y0), lh, TextAlign::Left, None, window, cx)
                        .ok();
                }
            }
        }

        // Scrollbar overlay.
        let scale = f32::from(p.layout.font_size) / BASE_FONT_PX;
        let text_top = p.y0 - bounds.top();
        let view_h = (bounds.size.height - text_top).max(px(1.));
        if let Some(sb) = scrollbar_geometry(
            bounds,
            f32::from(p.scroll),
            p.layout.total_height,
            view_h,
            scale,
        ) {
            if p.scrollbar_alpha > 0.01 {
                let hot = p.scrollbar_alpha > 0.99;
                window.paint_quad(
                    fill(
                        Bounds::new(point(sb.x, sb.thumb_top), size(sb.width, sb.thumb_height)),
                        p.palette
                            .muted
                            .opacity((if hot { 0.6 } else { 0.45 }) * p.scrollbar_alpha),
                    )
                    .corner_radii(sb.width / 2.0),
                );
            }
        }
    }
}

fn paint_selection(
    window: &mut Window,
    shaped: &WrappedLine,
    la: usize,
    lb: usize,
    char_len: usize,
    origin: Point<Pixels>,
    lh: Pixels,
    width: Pixels,
    color: Hsla,
) {
    let la = la.min(char_len);
    let lb = lb.min(char_len).max(la);
    let a_byte = char_to_byte(shaped.text.as_ref(), la);
    let b_byte = char_to_byte(shaped.text.as_ref(), lb);
    let Some(pa) = shaped.position_for_index(a_byte, lh) else {
        return;
    };
    let Some(pb) = shaped.position_for_index(b_byte, lh) else {
        return;
    };
    let row_a = (f32::from(pa.y) / f32::from(lh)).floor();
    let row_b = (f32::from(pb.y) / f32::from(lh)).floor();
    if (row_a - row_b).abs() < 0.5 {
        if pa.x < pb.x {
            window.paint_quad(fill(
                Bounds::from_corners(
                    point(origin.x + pa.x, origin.y + pa.y),
                    point(origin.x + pb.x, origin.y + pb.y + lh),
                ),
                color,
            ));
        }
        return;
    }
    // first row: from a to right edge
    window.paint_quad(fill(
        Bounds::from_corners(
            point(origin.x + pa.x, origin.y + pa.y),
            point(origin.x + width, origin.y + pa.y + lh),
        ),
        color,
    ));
    // middle rows
    let mut row = row_a + 1.0;
    while row < row_b {
        window.paint_quad(fill(
            Bounds::new(
                point(origin.x, origin.y + px(row as f32 * f32::from(lh))),
                size(width, lh),
            ),
            color,
        ));
        row += 1.0;
    }
    // last row: left edge to b
    window.paint_quad(fill(
        Bounds::from_corners(
            point(origin.x, origin.y + pb.y),
            point(origin.x + pb.x, origin.y + pb.y + lh),
        ),
        color,
    ));
}

impl EditorView {
    fn wrap_width_for(&self, window: &Window) -> Pixels {
        let vp = window.viewport_size();
        Self::column_width(vp.width, self.font_size())
    }
}

impl Render for EditorView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Build/reuse the layout once per frame here; EditorElement::prepaint
        // would otherwise rebuild it on every repaint after a resize.
        let _ = self.layout(window);
        let palette = Palette::new(&self.state);
        let modified = if self.state.modified { "* " } else { "" };
        let title = format!("{}{} - Nemiq", modified, self.state.file_name());
        window.set_window_title(&title);

        let status = self.state.status.clone();
        let words = format!(
            "{} {}",
            self.state.word_count,
            if self.state.word_count == 1 {
                "Word"
            } else {
                "Words"
            }
        );

        let scale = self.state.text_scale() as f32;
        let vim_badge = if self.vim_enabled {
            let label = match self.vim.mode {
                VimMode::Insert => "INSERT",
                VimMode::Normal => "NORMAL",
                VimMode::Visual => {
                    if self.vim.visual_line() {
                        "VISUAL LINE"
                    } else {
                        "VISUAL"
                    }
                }
                VimMode::Command => "COMMAND",
            };
            Some(div().text_color(palette.accent).child(label))
        } else {
            None
        };
        let cmdline_bar = self.cmdline_open.then(|| {
            div()
                .flex()
                .flex_row()
                .items_center()
                .h(self.cmdline_height())
                .px_3()
                .gap_2()
                .border_t_1()
                .border_color(palette.muted.opacity(0.25))
                .text_size(px(13.0 * scale))
                .text_color(palette.fg)
                .cursor_text()
                .child(div().text_color(palette.muted).child(":"))
                .child(div().child(self.cmdline_text.clone()))
                .child(
                    div()
                        .w(px(1.5))
                        .h(px(16.0 * scale))
                        .bg(palette.fg.opacity(0.7)),
                )
        });
        let search_bar = self.search_open.then(|| {
            let count = if self.matches.is_empty() {
                "0/0".to_string()
            } else {
                format!("{}/{}", self.match_idx + 1, self.matches.len())
            };
            let label = if self.search_text.is_empty() {
                "Find".to_string()
            } else {
                self.search_text.clone()
            };
            let label_color = if self.search_text.is_empty() {
                palette.muted
            } else {
                palette.fg
            };
            let replace_label = if self.replace_text.is_empty() {
                "Replace".to_string()
            } else {
                self.replace_text.clone()
            };
            let replace_color = if self.replace_focus {
                palette.fg
            } else {
                palette.muted
            };
            let row = px(32.0 * scale);
            div()
                .flex()
                .flex_col()
                .border_t_1()
                .border_color(palette.muted.opacity(0.25))
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .h(row)
                        .px_3()
                        .gap_3()
                        .text_size(px(13.0 * scale))
                        .text_color(palette.muted)
                        .child(
                            div()
                                .flex_1()
                                .cursor_text()
                                .text_color(label_color)
                                .child(label)
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|v: &mut Self, _, _, cx| {
                                        v.replace_focus = false;
                                        cx.stop_propagation();
                                        cx.notify();
                                    }),
                                ),
                        )
                        .child(div().child(count))
                        .child(div().cursor_pointer().px_1().child("↑").on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|v: &mut Self, _, w, cx| {
                                v.goto_match(-1, w, cx);
                                cx.stop_propagation();
                            }),
                        ))
                        .child(div().cursor_pointer().px_1().child("↓").on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|v: &mut Self, _, w, cx| {
                                v.goto_match(1, w, cx);
                                cx.stop_propagation();
                            }),
                        ))
                        .child(div().cursor_pointer().px_1().child("×").on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|v: &mut Self, _, _, cx| {
                                v.search_open = false;
                                v.replace_open = false;
                                v.replace_focus = false;
                                v.matches.clear();
                                v.match_idx = -1;
                                cx.stop_propagation();
                                cx.notify();
                            }),
                        )),
                )
                .children(self.replace_open.then(|| {
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .h(row)
                        .px_3()
                        .gap_3()
                        .border_t_1()
                        .border_color(palette.muted.opacity(0.15))
                        .text_size(px(13.0 * scale))
                        .child(
                            div()
                                .flex_1()
                                .cursor_text()
                                .text_color(replace_color)
                                .child(replace_label)
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|v: &mut Self, _, _, cx| {
                                        v.replace_focus = true;
                                        cx.stop_propagation();
                                        cx.notify();
                                    }),
                                ),
                        )
                        .child(
                            div()
                                .cursor_pointer()
                                .px_1()
                                .text_color(palette.fg)
                                .child("Replace")
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|v: &mut Self, _, w, cx| {
                                        v.search_replace_current(w, cx);
                                        cx.stop_propagation();
                                    }),
                                ),
                        )
                        .child(
                            div()
                                .cursor_pointer()
                                .px_1()
                                .text_color(palette.fg)
                                .child("All")
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|v: &mut Self, _, w, cx| {
                                        v.search_replace_all(w, cx);
                                        cx.stop_propagation();
                                    }),
                                ),
                        )
                }))
        });

        let shortcuts_overlay = self.shortcuts_open.then(|| {
            const ROWS: &[(&str, &str)] = &[
                ("Ctrl+S", "Save"),
                ("Ctrl+Shift+S", "Save As"),
                ("Ctrl+O", "Open"),
                ("Ctrl+N", "New Window"),
                ("Ctrl+P", "Export PDF / HTML"),
                ("Ctrl+Z", "Undo"),
                ("Ctrl+Shift+Z / Ctrl+Y", "Redo"),
                ("F11 / Super+F", "Fullscreen"),
                ("Ctrl+F / Ctrl+H", "Find"),
                ("Ctrl+Enter / Alt+Enter", "Replace All"),
                ("Ctrl+G / Enter", "Next Match"),
                ("Shift+Enter", "Previous Match"),
                ("Escape", "Close Find"),
                ("Ctrl+B", "Bold"),
                ("Ctrl+I", "Italic"),
                ("Ctrl+Shift+X", "Strikethrough"),
                ("Ctrl+K", "Link"),
                ("Ctrl+Shift+C", "Copy as HTML"),
                ("Ctrl+= / Ctrl++", "Increase Text Size"),
                ("Ctrl+-", "Decrease Text Size"),
                ("Ctrl+0", "Reset Text Size"),
                ("Ctrl+scroll", "Change Text Size"),
                ("Ctrl+? / Ctrl+/ / F1", "Shortcuts"),
            ];
            let column = |col: usize| {
                div().flex().flex_col().flex_1().gap_1().children(
                    ROWS.iter()
                        .enumerate()
                        .filter(move |(i, _)| i % 2 == col)
                        .map(|(_, (key, desc))| {
                            div()
                                .flex()
                                .flex_row()
                                .gap_2()
                                .text_size(px(12.0 * scale))
                                .child(div().text_color(palette.fg).child(*key))
                                .child(div().text_color(palette.muted).child(*desc))
                        }),
                )
            };
            div()
                .absolute()
                .inset_0()
                .flex()
                .items_center()
                .justify_center()
                .bg(palette.bg.opacity(0.8))
                .child(
                    div()
                        .w(px(680.0 * scale))
                        .p_4()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .rounded(px(6.))
                        .border_1()
                        .border_color(palette.muted.opacity(0.35))
                        .bg(palette.bg)
                        .text_color(palette.fg)
                        .child(
                            div()
                                .text_size(px(15.0 * scale))
                                .child("Keyboard shortcuts"),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .gap_6()
                                .child(column(0))
                                .child(column(1)),
                        )
                        .child(
                            div().flex().justify_end().child(
                                div()
                                    .px_3()
                                    .py_1()
                                    .rounded(px(4.))
                                    .bg(parse_color(
                                        &self.state.theme.accent,
                                        if self.state.theme.dark {
                                            0x5584aa
                                        } else {
                                            0x2077b2
                                        },
                                    ))
                                    .text_color(rgb(0xffffff))
                                    .cursor_pointer()
                                    .child("Close")
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(|v: &mut Self, _, _, cx| {
                                            v.shortcuts_open = false;
                                            cx.notify();
                                        }),
                                    ),
                            ),
                        ),
                )
        });

        let font_overlay = self.font_open.then(|| {
            let fonts = self.filtered_fonts();
            let rows = fonts.len().min(FONT_ROWS);
            let start = self.font_window.min(fonts.len().saturating_sub(rows));
            let visible: Vec<(usize, String)> = fonts
                .iter()
                .enumerate()
                .skip(start)
                .take(rows)
                .map(|(i, family)| (i, family.clone()))
                .collect();
            let list_height = px((rows.max(1) as f32) * 36.0 * scale);
            let current = self.state.editor_font.clone();
            let rows_el: Vec<AnyElement> = if visible.is_empty() {
                vec![div()
                    .px_2()
                    .py_2()
                    .text_size(px(13.0 * scale))
                    .text_color(palette.muted)
                    .child("No fonts")
                    .into_any_element()]
            } else {
                visible
                    .into_iter()
                    .map(|(_, family)| {
                        let selected = family == current;
                        let is_default = family == nemiq_core::DEFAULT_EDITOR_FONT;
                        let mut badge = String::new();
                        if selected {
                            badge.push('\u{2713}');
                        }
                        if is_default {
                            if !badge.is_empty() {
                                badge.push(' ');
                            }
                            badge.push_str("Default");
                        }
                        let pick = family.clone();
                        div()
                            .h(px(36.0 * scale))
                            .px_2()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_2()
                            .rounded(px(5.))
                            .bg(if selected {
                                palette.accent.opacity(0.22)
                            } else {
                                palette.bg
                            })
                            .cursor_pointer()
                            .child(
                                div()
                                    .flex_1()
                                    .overflow_hidden()
                                    .text_size(px(16.0 * scale))
                                    .font_family(SharedString::from(family.clone()))
                                    .child(family),
                            )
                            .child(
                                div()
                                    .text_size(px(11.0 * scale))
                                    .font_family(palette.mono.clone())
                                    .text_color(palette.muted)
                                    .child(badge),
                            )
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |v: &mut Self, _, _, cx| {
                                    v.choose_font(&pick, cx);
                                }),
                            )
                            .into_any_element()
                    })
                    .collect()
            };
            div()
                .absolute()
                .left(px(24.0 * scale))
                .bottom(px(f32::from(self.footer_height()) + 14.0 * scale))
                .w(px(320.0 * scale))
                .p_2()
                .flex()
                .flex_col()
                .gap_1()
                .rounded(px(9.))
                .border_1()
                .border_color(palette.muted.opacity(0.35))
                .bg(palette.bg)
                .text_color(palette.fg)
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|_: &mut Self, _, _, cx| cx.stop_propagation()),
                )
                .child(
                    div()
                        .h(px(34.0 * scale))
                        .px_2()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_1()
                        .cursor_text()
                        .text_size(px(14.0 * scale))
                        .text_color(if self.font_filter.is_empty() {
                            palette.muted
                        } else {
                            palette.fg
                        })
                        .child(if self.font_filter.is_empty() {
                            "Filter fonts".to_string()
                        } else {
                            self.font_filter.clone()
                        })
                        .child(
                            div()
                                .w(px(1.5))
                                .h(px(18.0 * scale))
                                .bg(palette.fg.opacity(0.7)),
                        ),
                )
                .child(div().h(px(1.)).w_full().bg(palette.muted.opacity(0.3)))
                .child(
                    div()
                        .h(list_height)
                        .flex()
                        .flex_col()
                        .overflow_hidden()
                        .children(rows_el),
                )
        });

        let close_overlay = self.close_prompt.then(|| {
            let file = self.state.file_name();
            div()
                .absolute()
                .inset_0()
                .flex()
                .items_center()
                .justify_center()
                .bg(palette.bg.opacity(0.75))
                .child(
                    div()
                        .w(px(420.0 * scale))
                        .p_4()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .rounded(px(6.))
                        .border_1()
                        .border_color(palette.muted.opacity(0.35))
                        .bg(palette.bg)
                        .text_color(palette.fg)
                        .child(div().text_size(px(15.0 * scale)).child("Unsaved changes"))
                        .child(
                            div()
                                .text_size(px(13.0 * scale))
                                .text_color(palette.muted)
                                .child(format!("Save changes to {file} before closing?")),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .gap_2()
                                .justify_end()
                                .child(
                                    div()
                                        .px_3()
                                        .py_1()
                                        .rounded(px(4.))
                                        .border_1()
                                        .border_color(if self.close_focus == 0 {
                                            palette.fg.opacity(0.85)
                                        } else {
                                            palette.fg.opacity(0.0)
                                        })
                                        .bg(palette.muted.opacity(0.15))
                                        .cursor_pointer()
                                        .child("Cancel")
                                        .on_mouse_down(
                                            MouseButton::Left,
                                            cx.listener(|v: &mut Self, _, w, cx| {
                                                v.dialog_cancel(w, cx)
                                            }),
                                        ),
                                )
                                .child(
                                    div()
                                        .px_3()
                                        .py_1()
                                        .rounded(px(4.))
                                        .border_1()
                                        .border_color(if self.close_focus == 1 {
                                            palette.fg.opacity(0.85)
                                        } else {
                                            palette.fg.opacity(0.0)
                                        })
                                        .bg(palette.muted.opacity(0.15))
                                        .cursor_pointer()
                                        .child("Discard")
                                        .on_mouse_down(
                                            MouseButton::Left,
                                            cx.listener(|v: &mut Self, _, w, cx| {
                                                v.dialog_discard(w, cx)
                                            }),
                                        ),
                                )
                                .child(
                                    div()
                                        .px_3()
                                        .py_1()
                                        .rounded(px(4.))
                                        .border_1()
                                        .border_color(if self.close_focus == 2 {
                                            palette.fg.opacity(0.85)
                                        } else {
                                            palette.fg.opacity(0.0)
                                        })
                                        .bg(parse_color(
                                            &self.state.theme.accent,
                                            if self.state.theme.dark {
                                                0x5584aa
                                            } else {
                                                0x2077b2
                                            },
                                        ))
                                        .text_color(rgb(0xffffff))
                                        .cursor_pointer()
                                        .child("Save")
                                        .on_mouse_down(
                                            MouseButton::Left,
                                            cx.listener(|v: &mut Self, _, w, cx| {
                                                v.dialog_save(w, cx)
                                            }),
                                        ),
                                ),
                        ),
                )
        });

        let external_overlay = self.pending_external.clone().map(|change| {
            let deleted = matches!(change, crate::state::ExternalChange::Deleted);
            let title = if deleted {
                "File removed"
            } else {
                "File changed"
            };
            let message = if deleted {
                "This file was removed outside Nemiq. Keep your text as an unsaved document?"
            } else {
                "This file changed outside Nemiq. Reloading will discard your changes."
            };
            let mut buttons = div().flex().flex_row().gap_2().justify_end();
            buttons = buttons.child(
                div()
                    .px_3()
                    .py_1()
                    .rounded(px(4.))
                    .bg(palette.muted.opacity(0.15))
                    .cursor_pointer()
                    .child("Keep Mine")
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|v: &mut Self, _, _, cx| {
                            v.state.keep_external();
                            v.pending_external = None;
                            v.schedule_autosave(cx);
                            cx.notify();
                        }),
                    ),
            );
            if !deleted {
                buttons = buttons.child(
                    div()
                        .px_3()
                        .py_1()
                        .rounded(px(4.))
                        .bg(parse_color(
                            &self.state.theme.accent,
                            if self.state.theme.dark {
                                0x5584aa
                            } else {
                                0x2077b2
                            },
                        ))
                        .text_color(rgb(0xffffff))
                        .cursor_pointer()
                        .child("Reload")
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|v: &mut Self, _, w, cx| {
                                v.state.reload();
                                v.pending_external = None;
                                v.cursor = 0;
                                v.anchor = 0;
                                v.set_scroll(0.0);
                                v.bump();
                                let _ = w;
                                cx.notify();
                            }),
                        ),
                );
            }
            div()
                .absolute()
                .inset_0()
                .flex()
                .items_center()
                .justify_center()
                .bg(palette.bg.opacity(0.75))
                .child(
                    div()
                        .w(px(440.0 * scale))
                        .p_4()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .rounded(px(6.))
                        .border_1()
                        .border_color(palette.muted.opacity(0.35))
                        .bg(palette.bg)
                        .text_color(palette.fg)
                        .child(div().text_size(px(15.0 * scale)).child(title))
                        .child(
                            div()
                                .text_size(px(13.0 * scale))
                                .text_color(palette.muted)
                                .child(message),
                        )
                        .child(buttons),
                )
        });

        div()
            .relative()
            .flex()
            .flex_col()
            .size_full()
            .bg(palette.bg)
            .text_color(palette.fg)
            .font_family(palette.mono.clone())
            .key_context("Editor")
            .track_focus(&self.focus_handle(cx))
            // actions
            .on_action(cx.listener(|v: &mut Self, a: &Save, w, cx| v.save(a, w, cx)))
            .on_action(cx.listener(|v: &mut Self, a: &SaveAs, w, cx| v.save_as(a, w, cx)))
            .on_action(cx.listener(|v: &mut Self, a: &Open, w, cx| v.open_dialog(a, w, cx)))
            .on_action(cx.listener(|v: &mut Self, a: &Backspace, w, cx| v.backspace(a, w, cx)))
            .on_action(cx.listener(|v: &mut Self, a: &Delete, w, cx| v.delete_forward(a, w, cx)))
            .on_action(cx.listener(|v: &mut Self, a: &Newline, w, cx| v.newline(a, w, cx)))
            .on_action(
                cx.listener(|v: &mut Self, a: &WordBackspace, w, cx| v.word_backspace(a, w, cx)),
            )
            .on_action(cx.listener(|v: &mut Self, a: &WordDelete, w, cx| v.word_delete(a, w, cx)))
            .on_action(cx.listener(|v: &mut Self, _: &Left, w, cx| {
                if v.vim_blocks() {
                    v.vim_key_apply(VimKey::Left, w, cx);
                } else {
                    v.move_horizontal(-1, false, cx);
                    v.ensure_cursor_visible(w, cx);
                }
            }))
            .on_action(cx.listener(|v: &mut Self, _: &Right, w, cx| {
                if v.vim_blocks() {
                    v.vim_key_apply(VimKey::Right, w, cx);
                } else {
                    v.move_horizontal(1, false, cx);
                    v.ensure_cursor_visible(w, cx);
                }
            }))
            .on_action(
                cx.listener(|v: &mut Self, _: &SelectLeft, _w, cx| v.move_horizontal(-1, true, cx)),
            )
            .on_action(
                cx.listener(|v: &mut Self, _: &SelectRight, _w, cx| v.move_horizontal(1, true, cx)),
            )
            .on_action(cx.listener(|v: &mut Self, _: &Up, w, cx| {
                if v.font_open {
                    v.font_move(-1, cx);
                } else {
                    v.move_vertical(-1, false, w, cx);
                }
            }))
            .on_action(cx.listener(|v: &mut Self, _: &Down, w, cx| {
                if v.font_open {
                    v.font_move(1, cx);
                } else {
                    v.move_vertical(1, false, w, cx);
                }
            }))
            .on_action(
                cx.listener(|v: &mut Self, _: &SelectUp, w, cx| v.move_vertical(-1, true, w, cx)),
            )
            .on_action(
                cx.listener(|v: &mut Self, _: &SelectDown, w, cx| v.move_vertical(1, true, w, cx)),
            )
            .on_action(cx.listener(|v: &mut Self, _: &Home, w, cx| v.home(false, w, cx)))
            .on_action(cx.listener(|v: &mut Self, _: &End, w, cx| v.end(false, w, cx)))
            .on_action(cx.listener(|v: &mut Self, _: &DocStart, w, cx| v.move_to(0, false, w, cx)))
            .on_action(cx.listener(|v: &mut Self, _: &DocEnd, w, cx| {
                let n = v.state.buffer.len_chars();
                v.move_to(n, false, w, cx)
            }))
            .on_action(cx.listener(|v: &mut Self, a: &SelectAll, w, cx| v.select_all(a, w, cx)))
            .on_action(cx.listener(|v: &mut Self, a: &Undo, w, cx| v.undo(a, w, cx)))
            .on_action(cx.listener(|v: &mut Self, a: &Redo, w, cx| v.redo(a, w, cx)))
            .on_action(cx.listener(|v: &mut Self, a: &Paste, w, cx| v.paste(a, w, cx)))
            .on_action(cx.listener(|v: &mut Self, a: &Cut, w, cx| v.cut(a, w, cx)))
            .on_action(cx.listener(|v: &mut Self, a: &Copy, w, cx| v.copy(a, w, cx)))
            .on_action(cx.listener(|v: &mut Self, _: &PageUp, w, cx| v.page(-1, false, w, cx)))
            .on_action(cx.listener(|v: &mut Self, _: &PageDown, w, cx| v.page(1, false, w, cx)))
            .on_action(cx.listener(|v: &mut Self, a: &Indent, w, cx| {
                if v.close_prompt {
                    v.cycle_close_focus(1, cx);
                } else if v.font_open {
                    v.font_move(1, cx);
                } else if v.search_open && v.replace_open {
                    v.replace_focus = !v.replace_focus;
                    cx.notify();
                } else {
                    v.indent(a, w, cx);
                }
            }))
            .on_action(cx.listener(|v: &mut Self, a: &Outdent, w, cx| {
                if v.close_prompt {
                    v.cycle_close_focus(-1, cx);
                } else if v.font_open {
                    v.font_move(-1, cx);
                } else {
                    v.outdent(a, w, cx);
                }
            }))
            .on_action(cx.listener(|v: &mut Self, _: &Bold, _, cx| v.toggle_wrap("**", cx)))
            .on_action(cx.listener(|v: &mut Self, _: &Italic, _, cx| v.toggle_wrap("*", cx)))
            .on_action(cx.listener(|v: &mut Self, _: &Strike, _, cx| v.toggle_wrap("~~", cx)))
            .on_action(cx.listener(|v: &mut Self, a: &Link, w, cx| v.insert_link(a, w, cx)))
            .on_action(cx.listener(|v: &mut Self, a: &CopyAsHtml, w, cx| v.copy_as_html(a, w, cx)))
            .on_action(cx.listener(|v: &mut Self, _: &Find, _, cx| v.open_search(false, cx)))
            .on_action(cx.listener(|v: &mut Self, _: &FindReplace, _, cx| v.open_search(true, cx)))
            .on_action(cx.listener(|v: &mut Self, _: &FindNext, w, cx| v.goto_match(1, w, cx)))
            .on_action(cx.listener(|v: &mut Self, _: &ReplaceAll, w, cx| {
                if v.search_open && v.replace_open {
                    v.search_replace_all(w, cx);
                }
            }))
            .on_action(cx.listener(|v: &mut Self, _: &FindPrev, w, cx| v.goto_match(-1, w, cx)))
            .on_action(cx.listener(|v: &mut Self, a: &CloseFind, w, cx| v.close_search(a, w, cx)))
            .on_action(cx.listener(|v: &mut Self, a: &ZoomIn, w, cx| v.zoom_in(a, w, cx)))
            .on_action(cx.listener(|v: &mut Self, a: &ZoomOut, w, cx| v.zoom_out(a, w, cx)))
            .on_action(cx.listener(|v: &mut Self, a: &ZoomReset, w, cx| v.zoom_reset(a, w, cx)))
            .on_action(cx.listener(|v: &mut Self, a: &NewWindow, w, cx| v.new_window(a, w, cx)))
            .on_action(cx.listener(|v: &mut Self, a: &Print, w, cx| v.print_document(a, w, cx)))
            .on_action(cx.listener(|v: &mut Self, a: &ToggleFullscreen, w, cx| {
                v.toggle_fullscreen(a, w, cx)
            }))
            .on_action(cx.listener(|v: &mut Self, a: &Shortcuts, w, cx| v.open_shortcuts(a, w, cx)))
            .on_action(cx.listener(|v: &mut Self, _: &DialogCancel, w, cx| {
                if v.close_prompt {
                    v.dialog_cancel(w, cx);
                }
            }))
            .on_action(cx.listener(|v: &mut Self, _: &DialogDiscard, w, cx| {
                if v.close_prompt {
                    v.dialog_discard(w, cx);
                }
            }))
            .on_action(cx.listener(|v: &mut Self, _: &DialogSave, w, cx| {
                if v.close_prompt {
                    v.dialog_save(w, cx);
                }
            }))
            .on_key_down(cx.listener(|v: &mut Self, e, w, cx| v.on_vim_key(e, w, cx)))
            // mouse
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|v: &mut Self, e, w, cx| v.on_mouse_down(e, w, cx)),
            )
            .on_mouse_move(cx.listener(|v: &mut Self, e, w, cx| v.on_mouse_move(e, w, cx)))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|v: &mut Self, e, w, cx| v.on_mouse_up(e, w, cx)),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|v: &mut Self, e, w, cx| v.on_mouse_up(e, w, cx)),
            )
            .on_scroll_wheel(cx.listener(|v: &mut Self, e, w, cx| v.on_scroll(e, w, cx)))
            .child(
                div()
                    .flex_1()
                    .w_full()
                    .relative()
                    .overflow_hidden()
                    .cursor_text()
                    .child(EditorElement { view: cx.entity() }),
            )
            .children(search_bar)
            .children(cmdline_bar)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .h(self.footer_height())
                    .px_3()
                    .gap_3()
                    .opacity(0.55)
                    .text_size(px(11.0 * scale))
                    .text_color(palette.muted)
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_3()
                            .child(
                                div()
                                    .cursor_pointer()
                                    .child(IconElement {
                                        kind: IconKind::Save,
                                        color: palette.muted,
                                    })
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(|v: &mut Self, _, _, cx| {
                                            if v.state.file_path.is_some() {
                                                let p = v.state.file_path.clone().unwrap();
                                                v.state.save_to(&p);
                                            } else {
                                                v.save_as_dialog(cx);
                                            }
                                            cx.notify();
                                        }),
                                    ),
                            )
                            .child(
                                div()
                                    .cursor_pointer()
                                    .child(IconElement {
                                        kind: IconKind::Open,
                                        color: palette.muted,
                                    })
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(|v: &mut Self, _, _, cx| {
                                            if let Some(p) = rfd::FileDialog::new()
                                                .add_filter("Markdown", &["md", "markdown"])
                                                .pick_file()
                                            {
                                                v.state.open(&p);
                                                v.cursor = 0;
                                                v.anchor = 0;
                                                v.set_scroll(0.0);
                                                v.bump();
                                                v.refresh_search();
                                            }
                                            cx.notify();
                                        }),
                                    ),
                            )
                            .child(
                                div()
                                    .cursor_pointer()
                                    .child(IconElement {
                                        kind: IconKind::Font,
                                        color: palette.muted,
                                    })
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(|v: &mut Self, _, w, cx| {
                                            v.open_font_picker(w, cx);
                                            // Don't let this same click reach the
                                            // backdrop handler that dismisses it.
                                            cx.stop_propagation();
                                        }),
                                    ),
                            )
                            .child(div().child(status))
                            .children(vim_badge),
                    )
                    .child(div().child(words)),
            )
            .children(shortcuts_overlay)
            .children(font_overlay)
            .children(close_overlay)
            .children(external_overlay)
    }
}

// ---------------- footer icons ----------------

#[derive(Clone, Copy)]
pub enum IconKind {
    Save,
    Open,
    Font,
}

pub struct IconElement {
    kind: IconKind,
    color: Hsla,
}

impl IntoElement for IconElement {
    type Element = Self;
    fn into_element(self) -> Self::Element {
        self
    }
}

fn stroke(window: &mut Window, from: Point<Pixels>, to: Point<Pixels>, width: f32, color: Hsla) {
    let dx = f32::from(to.x) - f32::from(from.x);
    let dy = f32::from(to.y) - f32::from(from.y);
    let len = (dx * dx + dy * dy).sqrt();
    let steps = (len / (width * 0.7).max(0.6)).ceil().max(1.0) as i32;
    for step in 0..=steps {
        let t = step as f32 / steps as f32;
        let x = f32::from(from.x) + dx * t;
        let y = f32::from(from.y) + dy * t;
        window.paint_quad(fill(
            Bounds::new(
                point(px(x - width / 2.0), px(y - width / 2.0)),
                size(px(width), px(width)),
            ),
            color,
        ));
    }
}

impl Element for IconElement {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }
    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }
    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _insp: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style::default();
        style.size.width = px(16.).into();
        style.size.height = px(16.).into();
        (window.request_layout(style, [], cx), ())
    }
    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _insp: Option<&gpui::InspectorElementId>,
        _b: Bounds<Pixels>,
        _r: &mut Self::RequestLayoutState,
        _w: &mut Window,
        _cx: &mut App,
    ) -> Self::PrepaintState {
    }
    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _insp: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _r: &mut Self::RequestLayoutState,
        _p: &mut Self::PrepaintState,
        window: &mut Window,
        _cx: &mut App,
    ) {
        let w = 1.3;
        let p = |x: f32, y: f32| point(bounds.left() + px(x), bounds.top() + px(y));
        match self.kind {
            IconKind::Save => {
                stroke(window, p(2.5, 2.5), p(10.5, 2.5), w, self.color);
                stroke(window, p(10.5, 2.5), p(13.5, 5.5), w, self.color);
                stroke(window, p(13.5, 5.5), p(13.5, 13.5), w, self.color);
                stroke(window, p(13.5, 13.5), p(2.5, 13.5), w, self.color);
                stroke(window, p(2.5, 13.5), p(2.5, 2.5), w, self.color);
                stroke(window, p(5.5, 2.5), p(5.5, 6.0), w, self.color);
                stroke(window, p(5.5, 6.0), p(10.0, 6.0), w, self.color);
                stroke(window, p(10.0, 6.0), p(10.0, 2.5), w, self.color);
                stroke(window, p(4.5, 13.5), p(4.5, 9.5), w, self.color);
                stroke(window, p(4.5, 9.5), p(11.5, 9.5), w, self.color);
                stroke(window, p(11.5, 9.5), p(11.5, 13.5), w, self.color);
            }
            IconKind::Font => {
                stroke(window, p(3.0, 13.5), p(8.0, 2.5), w, self.color);
                stroke(window, p(8.0, 2.5), p(13.0, 13.5), w, self.color);
                stroke(window, p(5.0, 9.5), p(11.0, 9.5), w, self.color);
            }
            IconKind::Open => {
                stroke(window, p(2.5, 13.0), p(2.5, 3.5), w, self.color);
                stroke(window, p(2.5, 3.5), p(6.5, 3.5), w, self.color);
                stroke(window, p(6.5, 3.5), p(8.5, 5.5), w, self.color);
                stroke(window, p(8.5, 5.5), p(13.5, 5.5), w, self.color);
                stroke(window, p(13.5, 5.5), p(13.5, 13.0), w, self.color);
                stroke(window, p(13.5, 13.0), p(2.5, 13.0), w, self.color);
            }
        }
    }
}
