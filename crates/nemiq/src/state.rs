//! Document and file state machine.
//!
//! No GPUI here — the view calls into it. Text lives in `nemiq_core::Buffer`
//! and every change is an `EditCommand`, which keeps undo and any future modal
//! editing in one place.

use nemiq_core::{count_words, saves_unchanged, Buffer, EditCommand, OmarchyTheme, Zoom};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

fn data_dir() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("nemiq")
}

#[derive(Debug, Serialize, Deserialize, Default)]
struct Recovery {
    file_url: String,
    text: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ExternalChange {
    Modified,
    Deleted,
}

pub struct DocState {
    pub buffer: Buffer,
    pub file_path: Option<PathBuf>,
    pub modified: bool,
    pub status: String,
    pub word_count: usize,
    pub zoom: Zoom,
    pub desktop_scale: f64,
    pub theme: OmarchyTheme,
    pub editor_font: String,
    known_contents: Option<Vec<u8>>,
    known_text: String,
    pub autosave_suspended: bool,
    pub show_numbers: bool,
    last_saved_text: String,
    state_dir: PathBuf,
}

impl DocState {
    pub fn new() -> Self {
        Self::with_state_dir(data_dir())
    }

    fn with_state_dir(state_dir: PathBuf) -> Self {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));
        let theme = OmarchyTheme::load(&home, true);
        // persisted zoom
        let zoom_val = std::fs::read_to_string(config_path())
            .ok()
            .and_then(|s| s.parse::<f64>().ok())
            .unwrap_or(1.0);
        let mut s = Self {
            buffer: Buffer::new(""),
            file_path: None,
            modified: false,
            status: String::new(),
            word_count: 0,
            zoom: Zoom::new(1.0, zoom_val),
            desktop_scale: 1.0,
            theme,
            editor_font: std::fs::read_to_string(font_path())
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "IBM Plex Mono".to_string()),
            known_contents: None,
            known_text: String::new(),
            autosave_suspended: false,
            show_numbers: Self::numbers_pref(),
            last_saved_text: String::new(),
            state_dir,
        };
        s.restore_recovery();
        s.refresh_wordcount();
        s
    }

    pub fn file_name(&self) -> String {
        self.file_path
            .as_ref()
            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
            .unwrap_or_else(|| "Untitled.md".to_string())
    }

    pub fn text(&self) -> String {
        self.buffer.text()
    }

    pub fn refresh_wordcount(&mut self) {
        self.word_count = count_words(&self.text());
    }

    pub fn set_text(&mut self, text: String) {
        let len = self.buffer.len_chars();
        self.buffer.apply(EditCommand::ReplaceRange {
            start: 0,
            end: len,
            text,
        });
        self.on_edited();
    }

    pub fn apply_replace(&mut self, start: usize, end: usize, text: String) -> usize {
        let c = self
            .buffer
            .apply(EditCommand::ReplaceRange { start, end, text });
        self.on_edited();
        c
    }

    fn on_edited(&mut self) {
        let text = self.text();
        let baseline_known = self.known_contents.is_some() || self.file_path.is_none();
        if baseline_known && text == self.known_text {
            self.modified = false;
            self.clear_recovery();
            self.status = if self.known_contents.is_some() {
                format!("Saved {}", self.file_name())
            } else {
                String::new()
            };
        } else {
            self.modified = true;
            self.status = "Unsaved".to_string();
        }
        self.refresh_wordcount();
    }

    pub fn open(&mut self, path: &Path) -> bool {
        match std::fs::read(path) {
            Ok(raw) => {
                let text = String::from_utf8_lossy(&raw)
                    .replace("\r\n", "\n")
                    .replace('\r', "\n");
                // read in text mode for comparison (strip \r)
                let contents = text.as_bytes().to_vec();
                self.buffer = Buffer::new(&text);
                self.known_contents = Some(contents.clone());
                self.known_text = text.clone();
                self.last_saved_text = text;
                self.autosave_suspended = false;
                self.file_path = Some(path.to_path_buf());
                self.modified = false;
                self.clear_recovery();
                if saves_unchanged(&raw) {
                    self.status = format!("Opened {}", self.file_name());
                } else {
                    self.autosave_suspended = true;
                    self.status = format!(
                        "Opened {}. Auto-save is off to keep its encoding and line endings.",
                        self.file_name()
                    );
                }
                self.refresh_wordcount();
                true
            }
            Err(_) => {
                self.status = format!(
                    "Could not open {}.",
                    path.file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_default()
                );
                false
            }
        }
    }

    /// `nemiq new.md`: a missing file starts blank and is written on save.
    pub fn open_or_start(&mut self, path: &Path) {
        if self.modified {
            self.status = format!(
                "Recovered unsaved changes; {} was not opened.",
                path.file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default()
            );
            return;
        }
        if path.exists() {
            self.open(path);
            return;
        }
        if let Some(parent) = path.parent() {
            if !parent.exists() {
                self.status = format!(
                    "Could not open {}.",
                    path.file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_default()
                );
                return;
            }
        }
        self.buffer = Buffer::new("");
        self.known_contents = None;
        self.known_text = String::new();
        self.file_path = Some(path.to_path_buf());
        self.modified = false;
        self.clear_recovery();
        self.status = format!("New file {}", self.file_name());
        self.refresh_wordcount();
    }

    pub fn save_to(&mut self, path: &Path) -> bool {
        let text = self.text();
        // atomic write via temp + rename (QSaveFile equiv, direct fallback)
        let tmp = path.with_extension("tmp-nemiq");
        let write_ok =
            std::fs::write(&tmp, text.as_bytes()).is_ok() && std::fs::rename(&tmp, path).is_ok();
        if !write_ok {
            // fallback: direct write (folder where only file itself writable)
            if std::fs::write(path, text.as_bytes()).is_err() {
                self.status = format!(
                    "Could not save {}.",
                    path.file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_default()
                );
                return false;
            }
        }
        let contents = text.as_bytes().to_vec();
        self.known_contents = Some(contents);
        self.known_text = text.clone();
        self.last_saved_text = text;
        self.autosave_suspended = false;
        self.file_path = Some(path.to_path_buf());
        self.modified = false;
        self.status = format!("Saved {}", self.file_name());
        self.clear_recovery();
        self.refresh_wordcount();
        true
    }

    pub fn disk_matches_known(&self) -> bool {
        let Some(p) = &self.file_path else {
            return false;
        };
        let Some(known) = &self.known_contents else {
            return false;
        };
        match std::fs::read(p) {
            Ok(raw) => {
                // text-mode compare
                let s = String::from_utf8_lossy(&raw)
                    .replace("\r\n", "\n")
                    .replace('\r', "\n");
                s.as_bytes() == known.as_slice()
            }
            Err(_) => false,
        }
    }

    /// Called by autosave timer + file watcher. Returns external change if any.
    pub fn check_external(&mut self) -> Option<ExternalChange> {
        let Some(p) = &self.file_path else {
            return None;
        };
        if !p.exists() {
            if self.known_contents.is_some() {
                self.known_contents = None;
                self.known_text = String::new();
                self.autosave_suspended = true;
                return Some(ExternalChange::Deleted);
            }
            return None;
        }
        if self.known_contents.is_some() && !self.disk_matches_known() {
            self.known_contents = None;
            self.known_text = String::new();
            self.autosave_suspended = true;
            return Some(ExternalChange::Modified);
        }
        None
    }

    pub fn auto_save(&mut self) -> bool {
        if !self.modified || self.autosave_suspended {
            return false;
        }
        let Some(p) = self.file_path.clone() else {
            return false;
        };
        if self.known_contents.is_none() {
            // new file not yet saved / removed file waits for explicit save
            self.autosave_suspended = true;
            return false;
        }
        if !self.disk_matches_known() {
            self.known_contents = None;
            self.known_text = String::new();
            self.autosave_suspended = true;
            return false;
        }
        self.save_to(&p);
        true
    }

    pub fn keep_external(&mut self) {
        if let Some(p) = self.file_path.clone() {
            if let Ok(raw) = std::fs::read(&p) {
                let text = String::from_utf8_lossy(&raw)
                    .replace("\r\n", "\n")
                    .replace('\r', "\n");
                self.known_contents = Some(text.as_bytes().to_vec());
                self.known_text = text;
                self.autosave_suspended = !saves_unchanged(&raw);
            } else {
                self.known_contents = None;
                self.known_text = String::new();
            }
        }
        self.modified = true;
        self.persist_recovery();
        self.status = "Kept your version".to_string();
    }

    pub fn reload(&mut self) {
        if let Some(p) = self.file_path.clone() {
            self.open(&p);
        }
    }

    pub fn persist_recovery(&self) {
        self.write_recovery();
    }

    fn recovery_file(&self) -> PathBuf {
        self.state_dir.join("recovery.json")
    }

    fn write_recovery(&self) {
        if !self.modified {
            return;
        }
        let _ = std::fs::create_dir_all(&self.state_dir);
        let r = Recovery {
            file_url: self
                .file_path
                .as_ref()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default(),
            text: self.text(),
        };
        let _ = std::fs::write(
            self.recovery_file(),
            serde_json::to_string(&r).unwrap_or_default(),
        );
    }

    fn restore_recovery(&mut self) {
        let Ok(raw) = std::fs::read(self.recovery_file()) else {
            return;
        };
        let Ok(r): Result<Recovery, _> = serde_json::from_slice(&raw) else {
            let _ = std::fs::remove_file(self.recovery_file());
            return;
        };
        if r.text.is_empty() && r.file_url.is_empty() {
            return;
        }
        self.buffer = Buffer::new(&r.text);
        if !r.file_url.is_empty() {
            let p = PathBuf::from(&r.file_url);
            self.file_path = Some(p.clone());
            if let Ok(raw) = std::fs::read(&p) {
                let t = String::from_utf8_lossy(&raw)
                    .replace("\r\n", "\n")
                    .replace('\r', "\n");
                self.known_contents = Some(t.as_bytes().to_vec());
                self.known_text = t;
            }
            self.autosave_suspended = true; // never autosave a recovered draft over its file
            self.modified = true;
            self.status = "Recovered unsaved changes".to_string();
            self.refresh_wordcount();
        } else if !r.text.is_empty() {
            self.modified = true;
            self.status = "Recovered unsaved changes".to_string();
            self.refresh_wordcount();
        }
    }

    pub fn clear_recovery(&self) {
        let _ = std::fs::remove_file(self.recovery_file());
    }

    pub fn discard_recovery(&mut self) {
        self.clear_recovery();
        self.autosave_suspended = true; // discarded edits must never reach file via autosave
    }

    pub fn set_zoom(&mut self, z: f64) {
        self.zoom = Zoom::new(self.desktop_scale, z);
        let _ = std::fs::create_dir_all(config_dir());
        let _ = std::fs::write(config_path(), format!("{}", self.zoom.zoom));
        self.status = format!("Text size {}%", (self.zoom.zoom * 100.0).round() as i64);
    }
    /// Whether vim-style modal editing is enabled, remembered across runs.
    pub fn vim_pref() -> bool {
        std::fs::read_to_string(vim_path())
            .map(|s| s.trim() == "1")
            .unwrap_or(false)
    }

    pub fn numbers_pref() -> bool {
        std::fs::read_to_string(numbers_path())
            .map(|s| s.trim() == "1")
            .unwrap_or(false)
    }

    pub fn save_numbers_pref(&self, on: bool) {
        let _ = std::fs::create_dir_all(config_dir());
        let _ = std::fs::write(numbers_path(), if on { "1" } else { "0" });
    }

    pub fn save_vim_pref(&self, on: bool) {
        let _ = std::fs::create_dir_all(config_dir());
        let _ = std::fs::write(vim_path(), if on { "1" } else { "0" });
    }

    pub fn text_scale(&self) -> f64 {
        self.desktop_scale * self.zoom.zoom
    }
}

fn config_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("nemiq")
}
fn config_path() -> PathBuf {
    config_dir().join("zoom")
}
fn font_path() -> PathBuf {
    config_dir().join("font")
}
fn vim_path() -> PathBuf {
    config_dir().join("vim")
}
fn numbers_path() -> PathBuf {
    config_dir().join("numbers")
}
fn window_path() -> PathBuf {
    config_dir().join("window")
}

/// Saved window rectangle: (x, y, width, height).
pub fn load_window_geometry() -> Option<(f32, f32, f32, f32)> {
    let raw = std::fs::read_to_string(window_path()).ok()?;
    let parts: Vec<f32> = raw
        .split_whitespace()
        .filter_map(|p| p.parse().ok())
        .collect();
    if parts.len() == 4 {
        Some((parts[0], parts[1], parts[2], parts[3]))
    } else {
        None
    }
}

pub fn save_window_geometry(x: f32, y: f32, width: f32, height: f32) {
    let _ = std::fs::create_dir_all(config_dir());
    let _ = std::fs::write(window_path(), format!("{x} {y} {width} {height}"));
}

impl DocState {
    pub fn set_editor_font(&mut self, family: &str) {
        if family.is_empty() || family == self.editor_font {
            return;
        }
        self.editor_font = family.to_string();
        let _ = std::fs::create_dir_all(config_dir());
        let _ = std::fs::write(font_path(), family);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn state_in(dir: &TempDir) -> DocState {
        DocState::with_state_dir(dir.path().join("state"))
    }

    #[test]
    fn editing_and_reverting_returns_to_clean() {
        let tmp = TempDir::new().unwrap();
        let mut s = state_in(&tmp);
        assert!(!s.modified);

        s.set_text("hello world".to_string());
        assert!(s.modified);
        assert_eq!(s.word_count, 2);
        assert_eq!(s.status, "Unsaved");

        s.set_text(String::new());
        assert!(!s.modified);
        assert_eq!(s.word_count, 0);
    }

    #[test]
    fn open_reads_the_file_and_starts_clean() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("note.md");
        fs::write(&file, "one\ntwo\n").unwrap();
        let mut s = state_in(&tmp);

        assert!(s.open(&file));
        assert_eq!(s.text(), "one\ntwo\n");
        assert_eq!(s.word_count, 2);
        assert_eq!(s.file_name(), "note.md");
        assert!(!s.modified);
        assert!(!s.autosave_suspended);
    }

    #[test]
    fn opening_a_crlf_file_turns_autosave_off() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("crlf.md");
        fs::write(&file, "one\r\ntwo\r\n").unwrap();
        let mut s = state_in(&tmp);

        assert!(s.open(&file));
        assert!(s.autosave_suspended);
        s.set_text("one\ntwo\nchanged\n".to_string());
        assert!(!s.auto_save());
        assert_eq!(fs::read_to_string(&file).unwrap(), "one\r\ntwo\r\n");
    }

    #[test]
    fn autosave_writes_when_the_file_is_clean() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("note.md");
        fs::write(&file, "one\n").unwrap();
        let mut s = state_in(&tmp);
        assert!(s.open(&file));

        s.set_text("one two\n".to_string());
        assert!(s.modified);
        assert!(s.auto_save());
        assert!(!s.modified);
        assert_eq!(fs::read_to_string(&file).unwrap(), "one two\n");
    }

    #[test]
    fn external_edit_is_reported_once_and_blocks_autosave() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("note.md");
        fs::write(&file, "mine\n").unwrap();
        let mut s = state_in(&tmp);
        assert!(s.open(&file));
        s.set_text("my edit\n".to_string());

        fs::write(&file, "theirs\n").unwrap();
        assert_eq!(s.check_external(), Some(ExternalChange::Modified));
        assert!(s.autosave_suspended);
        assert_eq!(s.check_external(), None);
        assert!(!s.auto_save());
        assert_eq!(fs::read_to_string(&file).unwrap(), "theirs\n");
    }

    #[test]
    fn deleted_file_is_reported() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("gone.md");
        fs::write(&file, "x\n").unwrap();
        let mut s = state_in(&tmp);
        assert!(s.open(&file));
        fs::remove_file(&file).unwrap();

        assert_eq!(s.check_external(), Some(ExternalChange::Deleted));
        assert!(s.autosave_suspended);
    }

    #[test]
    fn autosave_never_writes_a_file_that_was_never_saved() {
        let tmp = TempDir::new().unwrap();
        let mut s = state_in(&tmp);
        let file = tmp.path().join("new.md");
        s.open_or_start(&file);
        assert_eq!(s.file_path.as_deref(), Some(file.as_path()));
        assert!(!s.modified);

        s.set_text("draft".to_string());
        assert!(!s.auto_save());
        assert!(s.autosave_suspended);
        assert!(!file.exists());
    }

    #[test]
    fn recovery_round_trips_unsaved_work() {
        let tmp = TempDir::new().unwrap();
        let state_dir = tmp.path().join("state");
        {
            let mut s = DocState::with_state_dir(state_dir.clone());
            s.set_text("unsaved draft".to_string());
            s.persist_recovery();
        }

        let mut restored = DocState::with_state_dir(state_dir.clone());
        assert_eq!(restored.text(), "unsaved draft");
        assert!(restored.modified);
        assert!(restored.status.contains("Recovered"));

        restored.discard_recovery();
        let third = DocState::with_state_dir(state_dir);
        assert_eq!(third.text(), "");
        assert!(!third.modified);
    }
}
