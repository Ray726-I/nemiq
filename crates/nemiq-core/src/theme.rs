//! Reader for Omarchy's generated `colors.toml`, found at
//! `~/.local/state/omarchy/current/theme/colors.toml`.

use crate::files::toml_value;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq)]
pub struct OmarchyTheme {
    pub dark: bool,
    pub background: String,
    pub foreground: String,
    pub accent: String,
    pub selection: String,
    pub selection_foreground: String,
    pub code_background: String,
}

fn valid_color(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    // Accept #rgb/#rrggbb/#rrggbbaa + a few named (black/white) for tests.
    if let Some(h) = s.strip_prefix('#') {
        matches!(h.len(), 3 | 4 | 6 | 8) && h.chars().all(|c| c.is_ascii_hexdigit())
    } else {
        matches!(s, "black" | "white" | "red" | "green" | "blue")
    }
}

fn parse_hex(s: &str) -> Option<(f64, f64, f64)> {
    let h = s.strip_prefix('#')?;
    let (r, g, b) = match h.len() {
        6 | 8 => (
            u8::from_str_radix(&h[0..2], 16).ok()?,
            u8::from_str_radix(&h[2..4], 16).ok()?,
            u8::from_str_radix(&h[4..6], 16).ok()?,
        ),
        3 => (
            u8::from_str_radix(&format!("{}{}", &h[0..1], &h[0..1]), 16).ok()?,
            u8::from_str_radix(&format!("{}{}", &h[1..2], &h[1..2]), 16).ok()?,
            u8::from_str_radix(&format!("{}{}", &h[2..3], &h[2..3]), 16).ok()?,
        ),
        _ => return None,
    };
    Some((r as f64 / 255.0, g as f64 / 255.0, b as f64 / 255.0))
}

fn luminance(s: &str) -> Option<f64> {
    parse_hex(s).map(|(r, g, b)| 0.299 * r + 0.587 * g + 0.114 * b)
}

fn blend_hex(page: &str, fg: &str, amt: f64) -> String {
    let (pr, pg, pb) = parse_hex(page).unwrap_or((0.0, 0.0, 0.0));
    let (fr, fg_, fb) = parse_hex(fg).unwrap_or((1.0, 1.0, 1.0));
    let r = ((pr + (fr - pr) * amt) * 255.0).round() as u8;
    let g = ((pg + (fg_ - pg) * amt) * 255.0).round() as u8;
    let b = ((pb + (fb - pb) * amt) * 255.0).round() as u8;
    format!("#{r:02x}{g:02x}{b:02x}")
}

impl OmarchyTheme {
    pub fn colors_path(home: &Path) -> PathBuf {
        home.join(".local/state/omarchy/current/theme/colors.toml")
    }
    pub fn load(home: &Path, system_dark: bool) -> Self {
        let mut mode = String::new();
        let mut bg = String::new();
        let mut fg = String::new();
        let mut accent = String::new();
        let mut sel = String::new();
        let mut sel_bg = String::new();
        let mut sel_fg = String::new();
        let mut bright_fg = String::new();
        let mut lighter_bg = String::new();
        if let Ok(txt) = std::fs::read_to_string(Self::colors_path(home)) {
            for line in txt.lines() {
                let line = line.trim();
                if line.is_empty() || line.starts_with('#') {
                    continue;
                }
                let Some(eq) = line.find('=') else { continue };
                let k = line[..eq].trim();
                let v = toml_value(&line[eq + 1..]);
                match k {
                    "mode" => mode = v,
                    "background" => bg = v,
                    "foreground" => fg = v,
                    "accent" => accent = v,
                    "selection" => sel = v,
                    "selection_background" => sel_bg = v,
                    "selection_foreground" => sel_fg = v,
                    "bright_foreground" => bright_fg = v,
                    "lighter_background" => lighter_bg = v,
                    _ => {}
                }
            }
        }
        let mut dark = system_dark;
        if mode == "dark" {
            dark = true;
        } else if mode == "light" {
            dark = false;
        } else if valid_color(&bg) {
            if let Some(l) = luminance(&bg) {
                dark = l < 0.5;
            }
        }
        let (mut b, mut f, mut a, mut s) = if dark {
            ("#101010", "#eeeeee", "#5584aa", "#186a9a")
        } else {
            ("#ffffff", "#222324", "#2077b2", "#2077b2")
        };
        if valid_color(&bg) {
            b = Box::leak(bg.into_boxed_str());
        }
        if valid_color(&fg) {
            f = Box::leak(fg.into_boxed_str());
        }
        if valid_color(&accent) {
            a = Box::leak(accent.into_boxed_str());
        }
        // selection_background wins, then selection
        if valid_color(&sel_bg) {
            s = Box::leak(sel_bg.into_boxed_str());
        }
        if valid_color(&sel) {
            s = Box::leak(sel.into_boxed_str());
        }
        let mut sfg: &str = f;
        if valid_color(&bright_fg) {
            sfg = Box::leak(bright_fg.into_boxed_str());
        }
        if valid_color(&sel_fg) {
            sfg = Box::leak(sel_fg.into_boxed_str());
        }
        let code = if valid_color(&lighter_bg) && lighter_bg != b {
            lighter_bg
        } else {
            blend_hex(b, f, 0.06)
        };
        Self {
            dark,
            background: b.to_string(),
            foreground: f.to_string(),
            accent: a.to_string(),
            selection: s.to_string(),
            selection_foreground: sfg.to_string(),
            code_background: code,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reloads_when_the_generated_colours_change() {
        let home = std::env::temp_dir().join(format!("nemiq-theme-{}", std::process::id()));
        let dir = home.join(".local/state/omarchy/current/theme");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("colors.toml");

        std::fs::write(
            &path,
            "mode = \"dark\"\nbackground = \"#101010\"\nforeground = \"#eeeeee\"\n",
        )
        .unwrap();
        let dark = OmarchyTheme::load(&home, true);
        assert!(dark.dark);
        assert_eq!(dark.background, "#101010");

        std::fs::write(
            &path,
            "mode = \"light\"\nbackground = \"#fdf6e3\"\nforeground = \"#073642\"\n",
        )
        .unwrap();
        let light = OmarchyTheme::load(&home, true);
        assert!(!light.dark);
        assert_eq!(light.background, "#fdf6e3");
        assert_ne!(dark, light);

        std::fs::remove_dir_all(&home).ok();
    }

    /// The theme switch swaps a symlink, so the file is briefly missing.
    #[test]
    fn missing_file_falls_back_without_panicking() {
        let missing = std::env::temp_dir().join("nemiq-theme-does-not-exist");
        let theme = OmarchyTheme::load(&missing, true);
        assert_eq!(theme.background, "#101010");
    }
}
