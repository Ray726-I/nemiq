pub mod document;
pub mod files;
pub mod fonts;
pub mod markdown;
pub mod mutations;
pub mod search;
pub mod theme;
pub mod vim;
pub mod wordcount;
pub mod zoom;

pub use document::{Buffer, EditCommand, VimMode};
pub use files::{normalized_link_url, saves_unchanged, suggested_file_name, toml_value};
pub use fonts::{filter_font_families, selectable_font_families, DEFAULT_EDITOR_FONT};
pub use markdown::{
    closes_fence, code_spans, inline_markup, is_thematic_break, link_url_at, opening_fence_length,
    InlineKind, InlineMarkup, Span,
};
pub use search::find_matches;
pub use theme::OmarchyTheme;
pub use vim::{Command as VimCommand, Key as VimKey, VimState};
pub use wordcount::count_words;
pub use zoom::Zoom;
