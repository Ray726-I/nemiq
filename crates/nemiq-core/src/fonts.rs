//! Which font families the picker offers.
//!
//! Icon, symbol and emoji families are dropped, the per-script Noto families
//! collapse to the three Latin ones, and fontconfig's generic aliases and
//! duplicate foundry entries are removed. The picker filters the rest with a
//! case-insensitive substring match.

pub const DEFAULT_EDITOR_FONT: &str = "IBM Plex Mono";

const KEPT_NOTO: [&str; 3] = ["Noto Sans", "Noto Sans Mono", "Noto Serif"];

const GENERIC_ALIASES: [&str; 6] = [
    "Monospace",
    "Mono",
    "Sans Serif",
    "Sans",
    "Serif",
    "System-ui",
];

/// Case-insensitive test for symbol/icon/emoji families, which otherwise
/// claim Latin coverage because they map glyphs onto ASCII.
fn is_symbol_font(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.contains("symbol")
        || lower.contains("dingbat")
        || lower.contains("emoji")
        || lower.contains("icon")
        || lower.contains("awesome")
        || lower == "d050000l"
}

/// Drop the "Family [Foundry]" suffix fontconfig sometimes appends.
fn clean_family(name: &str) -> String {
    let trimmed = name.trim();
    let without_foundry = match trimmed.rfind(" [") {
        Some(ix) if trimmed.ends_with(']') => trimmed[..ix].trim_end().to_string(),
        _ => trimmed.to_string(),
    };
    without_foundry
}

/// The families offered by the picker: the bundled default first, then every
/// usable Latin family sorted case-insensitively.
pub fn selectable_font_families(families: &[String]) -> Vec<String> {
    let mut selectable: Vec<String> = Vec::new();
    for family in families {
        let family = clean_family(family);
        if family.is_empty() || family.starts_with('.') {
            continue;
        }
        if family.starts_with("Noto ") && !KEPT_NOTO.contains(&family.as_str()) {
            continue;
        }
        if GENERIC_ALIASES
            .iter()
            .any(|alias| alias.eq_ignore_ascii_case(&family))
            || is_symbol_font(&family)
        {
            continue;
        }
        if family.eq_ignore_ascii_case(DEFAULT_EDITOR_FONT)
            || selectable
                .iter()
                .any(|existing| existing.eq_ignore_ascii_case(&family))
        {
            continue;
        }
        selectable.push(family);
    }
    selectable.sort_by_key(|family| family.to_lowercase());
    selectable.insert(0, DEFAULT_EDITOR_FONT.to_string());
    selectable
}

/// Apply the picker's filter box to an already selectable list.
pub fn filter_font_families(families: &[String], query: &str) -> Vec<String> {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return families.to_vec();
    }
    families
        .iter()
        .filter(|family| family.to_lowercase().contains(&query))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn default_first_then_sorted() {
        let fonts = list(&["Zed Mono", "IBM Plex Mono", "Arial"]);
        assert_eq!(
            selectable_font_families(&fonts),
            list(&["IBM Plex Mono", "Arial", "Zed Mono"])
        );
    }

    #[test]
    fn drops_symbols_noto_flood_and_aliases() {
        let fonts = list(&[
            "Noto Sans",
            "Noto Serif Devanagari",
            "Font Awesome 6 Free",
            "JetBrainsMono Nerd Font",
            "Monospace",
            "Arial",
            ".Hidden",
            "D050000L",
            "Arial",
        ]);
        assert_eq!(
            selectable_font_families(&fonts),
            list(&[
                "IBM Plex Mono",
                "Arial",
                "JetBrainsMono Nerd Font",
                "Noto Sans"
            ])
        );
    }

    #[test]
    fn strips_foundry_suffix() {
        let fonts = list(&["Cantarell [Foundry]", "Cantarell [Other]"]);
        assert_eq!(
            selectable_font_families(&fonts),
            list(&["IBM Plex Mono", "Cantarell"])
        );
    }

    #[test]
    fn filter_is_case_insensitive_substring() {
        let fonts = list(&[
            "IBM Plex Mono",
            "Arial",
            "JetBrainsMono Nerd Font",
            "Libre Franklin",
        ]);
        assert_eq!(
            filter_font_families(&fonts, "  MONO "),
            list(&["IBM Plex Mono", "JetBrainsMono Nerd Font"])
        );
        assert_eq!(filter_font_families(&fonts, ""), fonts);
        assert!(filter_font_families(&fonts, "nope").is_empty());
    }
}
