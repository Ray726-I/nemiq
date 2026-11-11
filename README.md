# Nemiq

A quiet Markdown writing app for the desktop. Rust and
[GPUI](https://github.com/zed-industries/zed/tree/main/crates/gpui), no webview,
no bundled browser engine.

The window is the document: a centered column of text, live Markdown styling,
and a slim footer with the file name, word count and a few actions. Everything
else is a keyboard shortcut away.

## Features

- Centered 65-character writing column with Omawrite's 140% proportional
  line height. Long lines wrap.
- Hard-wrapped paragraphs and list items reflow to fill the reading column,
  so source line breaks never leave ragged gaps (the file itself keeps its line
  breaks; this is display only).
- Live Markdown preview: headings keep their larger size and show their `#`
  markers only while the caret is on the line, unordered lists become real
  bullets (`•` for the top level, `◦` nested), quote markers disappear,
  thematic breaks become rules, and fenced code blocks lose their fence lines.
- Tables align into columns with the pipes hidden, the header in bold, the
  separator turned into a rule, and the type shrunk to fit wide tables, unless
  the caret is inside the table, where the source stays visible.
- Styling for bold, italic, strikethrough, inline code, links, quotes and
  lists; inline markers reappear when the caret is in or against them. Links
  always stay compact (no jumping as the caret moves); put the caret on one to
  see its destination in a tooltip, and press `Ctrl+E` to expand the link's
  source for editing (`Esc` closes it again).
- Theme colours from Omarchy's `colors.toml` (background, foreground, accent,
  selection, code panels), including the fallback mixing used when a theme
  leaves a value out. The file is re-read when it changes, so switching themes
  applies without restarting.
- Autosave 600 ms after you stop typing, atomic writes with a direct-write
  fallback, an autosave guard for CRLF and non-UTF-8 files, crash recovery for
  unsaved work, and detection of external edits or deletions with Keep/Reload.
- Word count that understands CJK, diacritic-insensitive search, text zoom from
  50% to 300%, and remembered zoom, font choice and window geometry.
- Smart Return: paragraph breaks, a single newline inside fenced code, and
  list markers carried onto the next line. Tab inserts four spaces; with text
  selected, Tab and Shift+Tab indent and outdent whole lines.
- Font picker with a filter field, every usable family drawn in its own face,
  and a check mark on the current choice.
- Command palette (`Ctrl+Shift+P`): type to filter every action, arrow keys to
  pick, Enter to run.
- Optional line numbers (`Ctrl+Shift+L`, or `:set number` / `:set nonumber` in
  vim mode), remembered across runs.
- Find and replace: `Ctrl+H` adds a replace field, with Replace and Replace
  All (a single undo step). Enter accepts the match and closes the find bar;
  `Ctrl+G` keeps cycling after that.
- Copy the whole document as HTML (`Ctrl+Shift+C`), with a real `text/html`
  clipboard entry when `wl-copy` is available.
- Export to PDF or HTML with the current theme and the bundled typeface, from
  the window (`Ctrl+P`) or the command line (`--export`).
- Optional vim-style modal editing: Normal, Insert and Visual modes, motions,
  operators (`d`, `c`, `y`), text objects (`iw`, `i"`, `i(`), counts, registers
  with `p`/`P`, `u` and `Ctrl+r`, `/` search, `:` commands (`:w`, `:q`, `:wq`,
  `:set novim`, `:set number`), a block caret and a mode label. Off unless you
  ask for it.

## Build

Requires a stable Rust toolchain. Linux is the supported platform today, on
both Wayland and X11; Wayland needs `libxkbcommon` and X11 sessions need the
usual xcb libraries.

Nothing in the editor is Linux-specific except the optional `wl-copy` helper
for the HTML clipboard and reading Omarchy's theme file, and both degrade
gracefully. A macOS build may therefore work, but it has not been tried.
Windows is not supported — GPUI has no Windows backend.

```sh
cargo run -p nemiq                          # empty document
cargo run -p nemiq -- notes.md              # open (or start) a file
cargo run -p nemiq -- --vim notes.md        # modal editing
cargo run -p nemiq -- --export out.pdf notes.md   # convert and exit
cargo test --workspace                      # core text logic and app state tests
cargo build --release -p nemiq              # binary at target/release/nemiq
```

`--export` writes HTML for a `.html` target and PDF otherwise. PDF rendering
uses a headless Chromium (`chromium` or `google-chrome`).

The editor bundles IBM Plex Mono (`fonts/`, SIL OFL 1.1) so it looks the same
on any machine.

## Shortcuts

| Keys | Action |
| --- | --- |
| `Ctrl+S`, `Ctrl+Shift+S` | Save, save as |
| `Ctrl+O` | Open a file |
| `Ctrl+N` | New window |
| `Ctrl+Z`, `Ctrl+Shift+Z`, `Ctrl+Y` | Undo, redo |
| `Ctrl+F`, `Ctrl+H` | Find, or find and replace |
| `Ctrl+G` | Next match |
| `Enter` in find | Accept the match and close the find bar |
| `Shift+Enter` in find | Previous match |
| `Tab` in replace | Switch between the find and replace fields |
| `Ctrl+Enter`, `Alt+Enter` | Replace all |
| `Ctrl+P` | Export as PDF or HTML |
| `Ctrl+E` | Expand the link under the caret |
| `Ctrl+Shift+L` | Toggle line numbers |
| `Esc` | Close find, dialog or picker |
| `Ctrl+=`, `Ctrl+-`, `Ctrl+0` | Zoom in, out, reset |
| `Ctrl` + wheel | Zoom |
| `Ctrl+B`, `Ctrl+I`, `Ctrl+Shift+X`, `Ctrl+K` | Bold, italic, strikethrough, link |
| `Ctrl+Shift+C` | Copy document as HTML |
| `Ctrl+A` | Select all |
| `Ctrl+Left`, `Ctrl+Right` | Jump a word backwards / forwards |
| `Ctrl+Shift+Left`, `Ctrl+Shift+Right` | Extend the selection a word at a time |
| `Ctrl+Backspace`, `Ctrl+Delete` | Delete a word backwards / forwards |
| `Tab`, `Shift+Tab` | Indent, outdent |
| `PageUp`, `PageDown` | Scroll a screen |
| `Ctrl+Shift+P` | Command palette |
| `F1`, `Ctrl+/`, `Ctrl+?` | Shortcut list |
| `F11`, `Super+F` | Toggle fullscreen |

## Vim mode

Run `nemiq --vim file.md`, or press `Ctrl+Shift+P` and choose **Toggle Vim
Mode**; the choice is remembered for next time. To leave it, `:set novim` from
the `:` line or the same palette entry — the `:` line only exists while vim mode
is on, so it cannot be the way in. Insert mode behaves exactly like the normal
editor, so input methods and normal typing keep working.

`j` and `k` move down and up one *displayed* row, so wrapped paragraphs behave
like real lines and counts work (`3j`). `yy`/`Y` yank that displayed row, and the
yanked range flashes briefly, like Neovim's. Yanks and `p` share the system
clipboard, so text copied in another app pastes with `p` too.

## What is stored where

- `~/.config/nemiq/zoom`, `font`, `window`, `vim`, `numbers` — remembered text
  size, editor font, window rectangle, whether modal editing is on, and whether
  line numbers are shown.
- `~/.local/share/nemiq/recovery.json` — snapshot of unsaved work, written
  while you type and removed on a clean save or exit.
- `~/.local/state/omarchy/current/theme/colors.toml` — read for colours on
  Omarchy; elsewhere the built-in light or dark colours follow the system
  appearance.

## Layout

- `crates/nemiq-core/` — pure text logic, no UI dependencies: `document`
  (`Buffer`, `EditCommand`, undo), `markdown` (inline and fence scanning),
  `mutations`, `search`, `wordcount`, `files`, `zoom`, `theme`, `fonts`, and
  `vim` (the modal state machine).
  Regexes compile once and prose lines are cheap-rejected before any matching.

- `crates/nemiq/` — the GPUI app: `main.rs` (CLI, keymap, bundled fonts,
  window), `state.rs` (file, autosave, recovery, external-change state),
  `editor_view.rs` (shaped text, input, painting, search bar, font picker,
  dialogs) and `export.rs` (HTML/PDF). Lines that did not change keep their
  shaped output, so typing in a long document only re-shapes what moved.
- `fonts/` — IBM Plex Mono with its OFL license.
- `licenses/` — Apache-2.0 and MPL-2.0 texts from dependencies, plus the MIT
  text of the Omawrite port this started from.
- `vendor/xattr/` — small patch so `xattr 0.2.3` builds against current `libc`.

## Credits

Nemiq started as a Rust/GPUI port of [Omawrite](https://github.com/omacom/omawrite)
by David Heinemeier Hansson, MIT licensed, and grew from there.

## License

MIT, see `LICENSE`. Third-party licenses are listed in
`THIRD_PARTY_NOTICES.md`; the bundled font is under the SIL Open Font License
1.1 (`fonts/OFL.txt`).
