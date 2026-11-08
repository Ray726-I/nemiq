//! Small pure helpers the document needs: suggested file names, the "nothing
//! changed" save guard, TOML scalar parsing and link cleanup.

use url::Url;

pub fn suggested_file_name(text: &str) -> String {
    let mut name: String = text.split('\n').next().unwrap_or("").trim().to_string();
    name = name
        .chars()
        .map(|c| {
            if c == '/' || c == '\\' || c.is_control() || c == '\x7f' {
                '-'
            } else {
                c
            }
        })
        .collect();
    name = name
        .chars()
        .take(120)
        .collect::<String>()
        .trim()
        .to_string();
    if name.is_empty() || name == "." || name == ".." {
        name = "Untitled".to_string();
    }
    if !name.to_lowercase().ends_with(".md") {
        name.push_str(".md");
    }
    name
}

/// Saving writes UTF-8 + LF; anything else would be rewritten.
pub fn saves_unchanged(raw: &[u8]) -> bool {
    if raw.contains(&b'\r') {
        return false;
    }
    match std::str::from_utf8(raw) {
        Ok(s) => s.as_bytes() == raw,
        Err(_) => false,
    }
}

/// Quoted TOML value ends at closing quote; rest is inline comment.
pub fn toml_value(text: &str) -> String {
    let v = text.trim();
    if v.is_empty() {
        return String::new();
    }
    let q = v.chars().next().unwrap();
    if q != '"' && q != '\'' {
        return v.to_string();
    }
    let rest = &v[q.len_utf8()..];
    if let Some(close) = rest.find(q) {
        return rest[..close].to_string();
    }
    v.to_string()
}

/// Normalize clipboard text to link URL or "" (http/https/ftp/mailto + www.).
pub fn normalized_link_url(clipboard: &str) -> String {
    let mut cand = clipboard
        .split(['\r', '\n'])
        .next()
        .unwrap_or("")
        .trim()
        .to_string();
    if cand.is_empty() {
        return String::new();
    }
    if cand.to_lowercase().starts_with("www.") {
        cand = format!("https://{cand}");
    }
    // scheme check
    let Some(colon) = cand.find(':') else {
        return String::new();
    };
    let scheme = &cand[..colon];
    if scheme.is_empty()
        || !scheme.chars().next().unwrap().is_ascii_alphabetic()
        || !scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "+.-".contains(c))
    {
        return String::new();
    }
    let Ok(u) = Url::parse(&cand) else {
        return String::new();
    };
    let s = u.scheme().to_lowercase();
    if s == "http" || s == "https" || s == "ftp" {
        if u.host_str().unwrap_or("").is_empty() {
            return String::new();
        }
    } else if s != "mailto" {
        return String::new();
    }
    u.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn names() {
        assert_eq!(
            suggested_file_name("My first draft\nBody"),
            "My first draft.md"
        );
        assert_eq!(suggested_file_name("A/B"), "A-B.md");
        assert_eq!(suggested_file_name(""), "Untitled.md");
        assert_eq!(suggested_file_name("Already.md"), "Already.md");
    }
    #[test]
    fn links() {
        assert_eq!(
            normalized_link_url("www.example.com/path"),
            "https://www.example.com/path"
        );
        assert_eq!(
            normalized_link_url("mailto:writer@example.com"),
            "mailto:writer@example.com"
        );
        assert!(normalized_link_url("example.com").is_empty());
        assert!(normalized_link_url("file:///tmp/private").is_empty());
    }
    #[test]
    fn toml() {
        assert_eq!(toml_value(" \"#1a1b26\" "), "#1a1b26");
        assert_eq!(toml_value("\"#EDE6D6\"   # unbleached cloth"), "#EDE6D6");
        assert_eq!(toml_value("'#445566'# no space"), "#445566");
        assert_eq!(toml_value(" #1a1b26 "), "#1a1b26");
        assert_eq!(toml_value("\"\""), "");
    }
}
