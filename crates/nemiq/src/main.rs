mod cli;
mod editor_view;
mod export;
mod state;

use clap::Parser;
use editor_view::*;
use gpui::*;
use std::borrow::Cow;

fn main() {
    let args = cli::Cli::parse();
    let file = args.file.clone();

    if let Some(out) = args.export.clone() {
        let Some(input) = file.clone() else {
            eprintln!("--export needs an input file");
            std::process::exit(2);
        };
        let text = match std::fs::read_to_string(&input) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("Could not read {input}: {e}");
                std::process::exit(1);
            }
        };
        let home = dirs::home_dir().unwrap_or_default();
        let theme = nemiq_core::OmarchyTheme::load(&home, true);
        let font = std::fs::read_to_string(
            dirs::config_dir()
                .unwrap_or_default()
                .join("nemiq")
                .join("font"),
        )
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| nemiq_core::DEFAULT_EDITOR_FONT.to_string());
        let title = std::path::Path::new(&input)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "Nemiq".to_string());
        match export::export_to(&text, std::path::Path::new(&out), &title, &theme, &font) {
            Ok(()) => {
                println!("Wrote {out}");
                return;
            }
            Err(e) => {
                eprintln!("Export failed: {e}");
                std::process::exit(1);
            }
        }
    }

    let vim = args.vim || crate::state::DocState::vim_pref();

    Application::new().run(move |cx: &mut App| {
        // Bundle the default typeface so the app looks right on any system.
        let fonts: Vec<Cow<'static, [u8]>> = vec![
            Cow::Borrowed(include_bytes!("../../../fonts/IBMPlexMono-Regular.ttf")),
            Cow::Borrowed(include_bytes!("../../../fonts/IBMPlexMono-Italic.ttf")),
            Cow::Borrowed(include_bytes!("../../../fonts/IBMPlexMono-Bold.ttf")),
            Cow::Borrowed(include_bytes!("../../../fonts/IBMPlexMono-BoldItalic.ttf")),
        ];
        if let Err(err) = cx.text_system().add_fonts(fonts) {
            eprintln!("Could not load bundled fonts: {err}");
        }

        cx.bind_keys([
            KeyBinding::new("ctrl-s", Save, Some("Editor")),
            KeyBinding::new("ctrl-shift-s", SaveAs, Some("Editor")),
            KeyBinding::new("ctrl-o", Open, Some("Editor")),
            KeyBinding::new("ctrl-n", NewWindow, Some("Editor")),
            KeyBinding::new("ctrl-p", Print, Some("Editor")),
            KeyBinding::new("ctrl-b", Bold, Some("Editor")),
            KeyBinding::new("ctrl-i", Italic, Some("Editor")),
            KeyBinding::new("ctrl-shift-x", Strike, Some("Editor")),
            KeyBinding::new("ctrl-k", Link, Some("Editor")),
            KeyBinding::new("ctrl-shift-c", CopyAsHtml, Some("Editor")),
            KeyBinding::new("ctrl-f", Find, Some("Editor")),
            KeyBinding::new("ctrl-h", FindReplace, Some("Editor")),
            KeyBinding::new("ctrl-g", FindNext, Some("Editor")),
            KeyBinding::new("escape", CloseFind, Some("Editor")),
            KeyBinding::new("ctrl-=", ZoomIn, Some("Editor")),
            KeyBinding::new("ctrl-+", ZoomIn, Some("Editor")),
            KeyBinding::new("ctrl--", ZoomOut, Some("Editor")),
            KeyBinding::new("ctrl-0", ZoomReset, Some("Editor")),
            KeyBinding::new("ctrl-z", Undo, Some("Editor")),
            KeyBinding::new("ctrl-shift-z", Redo, Some("Editor")),
            KeyBinding::new("ctrl-y", Redo, Some("Editor")),
            KeyBinding::new("ctrl-a", SelectAll, Some("Editor")),
            KeyBinding::new("left", Left, Some("Editor")),
            KeyBinding::new("right", Right, Some("Editor")),
            KeyBinding::new("up", Up, Some("Editor")),
            KeyBinding::new("down", Down, Some("Editor")),
            KeyBinding::new("shift-left", SelectLeft, Some("Editor")),
            KeyBinding::new("shift-right", SelectRight, Some("Editor")),
            KeyBinding::new("shift-up", SelectUp, Some("Editor")),
            KeyBinding::new("shift-down", SelectDown, Some("Editor")),
            KeyBinding::new("home", Home, Some("Editor")),
            KeyBinding::new("end", End, Some("Editor")),
            KeyBinding::new("ctrl-home", DocStart, Some("Editor")),
            KeyBinding::new("ctrl-end", DocEnd, Some("Editor")),
            KeyBinding::new("enter", Newline, Some("Editor")),
            KeyBinding::new("backspace", Backspace, Some("Editor")),
            KeyBinding::new("ctrl-backspace", WordBackspace, Some("Editor")),
            KeyBinding::new("ctrl-delete", WordDelete, Some("Editor")),
            KeyBinding::new("delete", Delete, Some("Editor")),
            KeyBinding::new("ctrl-v", Paste, Some("Editor")),
            KeyBinding::new("ctrl-c", Copy, Some("Editor")),
            KeyBinding::new("ctrl-x", Cut, Some("Editor")),
            KeyBinding::new("pageup", PageUp, Some("Editor")),
            KeyBinding::new("pagedown", PageDown, Some("Editor")),
            KeyBinding::new("tab", Indent, Some("Editor")),
            KeyBinding::new("shift-tab", Outdent, Some("Editor")),
            KeyBinding::new("f11", ToggleFullscreen, Some("Editor")),
            KeyBinding::new("f1", Shortcuts, Some("Editor")),
            KeyBinding::new("ctrl-/", Shortcuts, Some("Editor")),
            KeyBinding::new("ctrl-?", Shortcuts, Some("Editor")),
            KeyBinding::new("super-f", ToggleFullscreen, Some("Editor")),
            KeyBinding::new("shift-enter", FindPrev, Some("Editor")),
            KeyBinding::new("ctrl-enter", ReplaceAll, Some("Editor")),
            KeyBinding::new("alt-enter", ReplaceAll, Some("Editor")),
            KeyBinding::new("alt-c", DialogCancel, Some("Editor")),
            KeyBinding::new("alt-d", DialogDiscard, Some("Editor")),
            KeyBinding::new("alt-s", DialogSave, Some("Editor")),
        ]);

        let bounds = match crate::state::load_window_geometry() {
            Some((x, y, w, h)) => Bounds::new(point(px(x), px(y)), size(px(w), px(h))),
            None => Bounds::centered(None, size(px(1280.), px(820.)), cx),
        };
        let window = cx
            .open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    window_min_size: Some(size(px(720.), px(520.))),
                    app_id: Some("nemiq".to_string()),
                    ..Default::default()
                },
                |_, cx| {
                    let view = cx.new(|cx| EditorView::new(vim, cx));
                    if let Some(f) = file.clone() {
                        view.update(cx, |v, _| {
                            v.state.open_or_start(std::path::Path::new(&f));
                            v.cursor = 0;
                            v.anchor = 0;
                        });
                    }
                    view
                },
            )
            .unwrap();
        let weak = window
            .update(cx, |view, window, cx| {
                view.bounds_sub = Some(cx.observe_window_bounds(window, |v, _, cx| {
                    v.invalidate();
                    cx.notify();
                }));
                window.focus(&view.focus);
                cx.entity().downgrade()
            })
            .unwrap();
        window
            .update(cx, |_, window, cx| {
                let weak = weak.clone();
                window.on_window_should_close(cx, move |window, cx| {
                    let b = window.bounds();
                    crate::state::save_window_geometry(
                        f32::from(b.origin.x),
                        f32::from(b.origin.y),
                        f32::from(b.size.width),
                        f32::from(b.size.height),
                    );
                    weak.update(cx, |v, cx| {
                        if v.close_approved || !v.state.modified {
                            return true;
                        }
                        if !v.close_prompt {
                            // Default focus is Save.
                            v.close_focus = 2;
                        }
                        v.close_prompt = true;
                        cx.notify();
                        false
                    })
                    .unwrap_or(true)
                });
            })
            .ok();
        cx.activate(true);
    });
}
