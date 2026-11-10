//! Markdown export: a self-contained HTML page, and a PDF rendered from it by
//! a headless Chromium when one is installed.

use nemiq_core::OmarchyTheme;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

const FONT_REGULAR: &[u8] = include_bytes!("../../../fonts/IBMPlexMono-Regular.ttf");
const FONT_BOLD: &[u8] = include_bytes!("../../../fonts/IBMPlexMono-Bold.ttf");
const FONT_ITALIC: &[u8] = include_bytes!("../../../fonts/IBMPlexMono-Italic.ttf");

/// Minimal base64 so the bundled typeface can be inlined without another crate.
fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = ((chunk[0] as u32) << 16)
            | ((*chunk.get(1).unwrap_or(&0) as u32) << 8)
            | *chunk.get(2).unwrap_or(&0) as u32;
        out.push(TABLE[(n >> 18) as usize & 63] as char);
        out.push(TABLE[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            TABLE[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

/// `@font-face` rules carrying the bundled IBM Plex Mono.
fn font_faces() -> &'static str {
    static FACES: OnceLock<String> = OnceLock::new();
    FACES.get_or_init(|| {
        format!(
            "@font-face {{ font-family: 'Nemiq Mono'; font-weight: 400; font-style: normal; src: url(data:font/ttf;base64,{}) format('truetype'); }}\n\
@font-face {{ font-family: 'Nemiq Mono'; font-weight: 700; font-style: normal; src: url(data:font/ttf;base64,{}) format('truetype'); }}\n\
@font-face {{ font-family: 'Nemiq Mono'; font-weight: 400; font-style: italic; src: url(data:font/ttf;base64,{}) format('truetype'); }}\n",
            base64(FONT_REGULAR),
            base64(FONT_BOLD),
            base64(FONT_ITALIC),
        )
    })
}

const CSS_BODY: &str = r#"
  :root { color-scheme: dark light; }
  @page { size: A4; margin: 0; }
  html { background: var(--bg); }
  body {
    min-height: 100vh;
    box-sizing: border-box;
    margin: 0 auto;
    max-width: 42rem;
    padding: 2.5rem 1.5rem 3rem;
    background: var(--bg);
    color: var(--fg);
    font-family: var(--font);
    font-size: 16px;
    line-height: 1.6;
    word-wrap: break-word;
  }
  h1, h2, h3, h4, h5, h6 { line-height: 1.25; margin: 1.6em 0 0.5em; }
  h1 { font-size: 1.8em; } h2 { font-size: 1.45em; } h3 { font-size: 1.2em; }
  p, ul, ol, blockquote, pre, table { margin: 0 0 1em; }
  a { color: var(--accent); }
  code { background: var(--code); padding: 0.1em 0.3em; border-radius: 3px; }
  pre { background: var(--code); padding: 1em; border-radius: 6px; overflow-x: auto; }
  pre code { background: none; padding: 0; }
  blockquote { border-left: 3px solid var(--muted); margin-left: 0; padding-left: 1em; color: var(--muted); }
  hr { border: none; border-top: 1px solid var(--muted); margin: 2em 0; }
  img { max-width: 100%; }
  ::selection { background: var(--sel); }
"#;

fn css_vars(theme: &OmarchyTheme, font: &str) -> String {
    let muted = if theme.dark { "#8a8a8a" } else { "#6b6b6b" };
    format!(
        "  :root {{\n    --bg: {};\n    --fg: {};\n    --accent: {};\n    --muted: {};\n    --code: {};\n    --sel: {};\n    --font: 'Nemiq Mono', '{}', ui-monospace, monospace;\n  }}\n",
        theme.background,
        theme.foreground,
        theme.accent,
        muted,
        theme.code_background,
        theme.selection,
        font.replace('\'', ""),
    )
}

/// Markdown to a standalone HTML document using the current theme.
pub fn html_document(text: &str, title: &str, theme: &OmarchyTheme, font: &str) -> String {
    let mut body = String::new();
    let mut options = pulldown_cmark::Options::empty();
    options.insert(pulldown_cmark::Options::ENABLE_STRIKETHROUGH);
    options.insert(pulldown_cmark::Options::ENABLE_TABLES);
    options.insert(pulldown_cmark::Options::ENABLE_TASKLISTS);
    options.insert(pulldown_cmark::Options::ENABLE_FOOTNOTES);
    pulldown_cmark::html::push_html(&mut body, pulldown_cmark::Parser::new_ext(text, options));
    format!(
        "<!doctype html>\n<html>\n<head>\n<meta charset=\"utf-8\">\n<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n<title>{}</title>\n<style>\n{}{}{}\n</style>\n</head>\n<body>\n{}\n</body>\n</html>\n",
        escape(title),
        font_faces(),
        css_vars(theme, font),
        CSS_BODY,
        body
    )
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn chromium_binary() -> Option<&'static str> {
    const CANDIDATES: [&str; 4] = [
        "chromium",
        "chromium-browser",
        "google-chrome-stable",
        "google-chrome",
    ];
    CANDIDATES.into_iter().find(|name| {
        Command::new("sh")
            .arg("-c")
            .arg(format!("command -v {name}"))
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    })
}

fn html_to_pdf(html: &str, out: &Path) -> Result<(), String> {
    let chrome = chromium_binary()
        .ok_or_else(|| "PDF export needs chromium or google-chrome installed".to_string())?;
    let tmp: PathBuf =
        std::env::temp_dir().join(format!("nemiq-export-{}.html", std::process::id()));
    let profile = std::env::temp_dir().join(format!("nemiq-chrome-{}", std::process::id()));
    std::fs::write(&tmp, html).map_err(|e| e.to_string())?;
    let url = format!("file://{}", tmp.display());
    let out_abs = std::fs::canonicalize(out)
        .or_else(|_| {
            std::fs::write(out, b"");
            std::fs::canonicalize(out)
        })
        .map_err(|e| e.to_string())?;
    let result = Command::new(chrome)
        .args([
            "--headless",
            "--disable-gpu",
            "--no-sandbox",
            "--no-pdf-header-footer",
            "--virtual-time-budget=3000",
        ])
        .arg(format!("--user-data-dir={}", profile.display()))
        .arg(format!("--print-to-pdf={}", out_abs.display()))
        .arg(&url)
        .output();
    let _ = std::fs::remove_file(&tmp);
    let _ = std::fs::remove_dir_all(&profile);
    match result {
        Ok(o) if o.status.success() && out_abs.exists() => Ok(()),
        Ok(o) => Err(String::from_utf8_lossy(&o.stderr)
            .lines()
            .next()
            .unwrap_or("chromium failed")
            .to_string()),
        Err(e) => Err(e.to_string()),
    }
}

/// Write the document to `path`, choosing PDF or HTML by extension.
pub fn export_to(
    text: &str,
    path: &Path,
    title: &str,
    theme: &OmarchyTheme,
    font: &str,
) -> Result<(), String> {
    let html = html_document(text, title, theme, font);
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    if ext == "html" || ext == "htm" {
        std::fs::write(path, html).map_err(|e| e.to_string())
    } else {
        html_to_pdf(&html, path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn theme() -> OmarchyTheme {
        OmarchyTheme::load(Path::new("/nonexistent"), true)
    }

    #[test]
    fn html_contains_markup_and_theme() {
        let html = html_document(
            "# Hi\n\n**bold** and `code`\n",
            "Hi",
            &theme(),
            "IBM Plex Mono",
        );
        assert!(html.contains("<h1>Hi</h1>"));
        assert!(html.contains("<strong>bold</strong>"));
        assert!(html.contains("--bg:"), "theme variables are inlined");
        assert!(html.contains("IBM Plex Mono"));
    }

    #[test]
    fn title_is_escaped() {
        let html = html_document("x", "a<b>&c", &theme(), "Mono");
        assert!(html.contains("<title>a&lt;b&gt;&amp;c</title>"));
    }
}
