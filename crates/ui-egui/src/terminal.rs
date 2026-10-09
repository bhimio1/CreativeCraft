//! A terminal inside the window: a real PTY (`portable-pty`; ConPTY on Windows) running one
//! program, its output parsed by `vt100` and drawn cell by cell with egui's monospace font.
//!
//! Enough of a terminal for agent CLIs' interactive UIs: 256 colours and true colour, the
//! alternate screen, application cursor keys, bracketed paste, scrollback, cursor-position and
//! device-attribute replies, mouse selection and copy. Ctrl+C copies when there is a selection and
//! otherwise interrupts, as in most desktop terminals.

use crate::theme;
use creativecraft_engine::agent::LaunchSpec;
use egui::{Color32, FontId, Pos2, Rect, Sense, vec2};
use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

const SCROLLBACK: usize = 5000;
const FONT_SIZE: f32 = 13.0;

type Writer = Arc<Mutex<Box<dyn Write + Send>>>;

pub struct Terminal {
    parser: Arc<Mutex<vt100::Parser>>,
    writer: Writer,
    master: Box<dyn MasterPty + Send>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
    exited: Arc<AtomicBool>,
    size: (u16, u16),
    /// Selection in visible-grid cells: (anchor, head) as (row, col).
    selection: Option<((u16, u16), (u16, u16))>,
    /// Fractional wheel movement not yet turned into whole lines.
    scroll_acc: f32,
}

impl Terminal {
    pub fn spawn(spec: &LaunchSpec, ctx: &egui::Context) -> Result<Terminal, String> {
        let (rows, cols) = (24, 80);
        let pair =
            native_pty_system().openpty(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 }).map_err(|e| format!("couldn't open a terminal: {e}"))?;
        // the program runs on the host; inside Flatpak that's `flatpak-spawn --host …`, with the
        // environment and directory as its flags
        let mut host = creativecraft_platform::HostCommand::new(&spec.program);
        host.args(&spec.args).current_dir(&spec.cwd).env("TERM", "xterm-256color").env("COLORTERM", "truecolor");
        for (k, v) in &spec.env {
            host.env(k, v);
        }
        let (program, args) = host.argv();
        let mut cmd = CommandBuilder::new(&program);
        cmd.args(&args);
        if let Some(dir) = host.outer_cwd() {
            cmd.cwd(dir);
        }
        if !creativecraft_platform::in_flatpak() {
            cmd.env_remove("NO_COLOR");
        }
        for (k, v) in host.outer_env() {
            cmd.env(k, v);
        }
        let child = pair.slave.spawn_command(cmd).map_err(|e| format!("couldn't start {}: {e}", spec.program))?;
        drop(pair.slave);
        let mut reader = pair.master.try_clone_reader().map_err(|e| e.to_string())?;
        let writer: Writer = Arc::new(Mutex::new(pair.master.take_writer().map_err(|e| e.to_string())?));
        let parser = Arc::new(Mutex::new(vt100::Parser::new(rows, cols, SCROLLBACK)));
        let exited = Arc::new(AtomicBool::new(false));

        let (p, w, done, ctx) = (parser.clone(), writer.clone(), exited.clone(), ctx.clone());
        std::thread::spawn(move || {
            let mut buf = [0u8; 16 * 1024];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => feed(&p, &w, &buf[..n]),
                }
                ctx.request_repaint();
            }
            done.store(true, Ordering::Relaxed);
            ctx.request_repaint();
        });

        Ok(Terminal { parser, writer, master: pair.master, child, exited, size: (rows, cols), selection: None, scroll_acc: 0.0 })
    }

    /// Has the program ended?
    pub fn exited(&self) -> bool {
        self.exited.load(Ordering::Relaxed)
    }

    pub fn kill(&mut self) {
        let _ = self.child.kill();
    }

    fn send(&self, bytes: &[u8]) {
        if let Ok(mut w) = self.writer.lock() {
            let _ = w.write_all(bytes);
            let _ = w.flush();
        }
    }

    fn resize(&mut self, rows: u16, cols: u16) {
        if (rows, cols) == self.size || rows == 0 || cols == 0 {
            return;
        }
        self.size = (rows, cols);
        let _ = self.master.resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 });
        if let Ok(mut p) = self.parser.lock() {
            p.screen_mut().set_size(rows, cols);
        }
        self.selection = None;
    }

    /// Fill the available space with the terminal and handle its input.
    pub fn show(&mut self, ui: &mut egui::Ui) -> egui::Response {
        let t = theme::Tokens::get();
        let font = FontId::monospace(FONT_SIZE);
        let (cw, ch) = ui.ctx().fonts_mut(|f| (f.glyph_width(&font, 'M'), f.row_height(&font)));
        let (rect, resp) = ui.allocate_exact_size(ui.available_size(), Sense::click_and_drag());
        let inner = rect.shrink(10.0);
        let cols = ((inner.width() / cw).floor() as u16).max(2);
        let rows = ((inner.height() / ch).floor() as u16).max(2);
        self.resize(rows, cols);

        // take the keyboard when clicked, or when nothing else has it (so typing just works)
        if resp.clicked() || resp.drag_started() || ui.memory(|m| m.focused().is_none()) {
            resp.request_focus();
        }
        let focused = resp.has_focus();
        if resp.has_focus() {
            // keep Tab, arrows and Escape for the program, not egui's focus navigation
            ui.memory_mut(|m| m.set_focus_lock_filter(resp.id, egui::EventFilter { tab: true, horizontal_arrows: true, vertical_arrows: true, escape: true }));
        }
        let cell_at = |p: Pos2| -> (u16, u16) {
            let c = ((p.x - inner.left()) / cw).floor().clamp(0.0, f32::from(cols - 1)) as u16;
            let r = ((p.y - inner.top()) / ch).floor().clamp(0.0, f32::from(rows - 1)) as u16;
            (r, c)
        };

        // ── mouse: selection and wheel
        if resp.drag_started()
            && let Some(p) = resp.interact_pointer_pos()
        {
            let c = cell_at(p);
            self.selection = Some((c, c));
        } else if resp.dragged()
            && let (Some(p), Some((a, _))) = (resp.interact_pointer_pos(), self.selection)
        {
            self.selection = Some((a, cell_at(p)));
        } else if resp.clicked() {
            self.selection = None;
        }
        if resp.hovered() {
            let dy = ui.input(|i| i.smooth_scroll_delta.y);
            self.scroll_acc += dy / ch;
            let lines = self.scroll_acc.trunc() as i32;
            if lines != 0 {
                self.scroll_acc -= lines as f32;
                self.scroll(lines);
            }
        }

        // ── keyboard
        if focused {
            let events = ui.input(|i| i.events.clone());
            for ev in events {
                self.input(ui, &ev);
            }
        }

        // ── draw
        let Ok(parser) = self.parser.lock() else { return resp };
        let screen = parser.screen();
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 10.0, term_bg(&t));
        let sel = self.selection.map(|(a, b)| if a <= b { (a, b) } else { (b, a) });
        let in_sel = |r: u16, c: u16| sel.is_some_and(|(s, e)| (r, c) >= s && (r, c) <= e && s != e);
        let cell_rect = |r: u16, c: u16, w: u16| Rect::from_min_size(inner.min + vec2(f32::from(c) * cw, f32::from(r) * ch), vec2(f32::from(w) * cw, ch));

        for r in 0..rows {
            let mut run = String::new();
            let mut run_start = 0u16;
            let mut run_color = Color32::TRANSPARENT;
            let flush = |run: &mut String, start: u16, color: Color32| {
                if !run.trim().is_empty() {
                    painter.text(cell_rect(r, start, 1).min, egui::Align2::LEFT_TOP, run.as_str(), font.clone(), color);
                }
                run.clear();
            };
            for c in 0..cols {
                let Some(cell) = screen.cell(r, c) else { continue };
                if cell.is_wide_continuation() {
                    continue;
                }
                let width = if cell.is_wide() { 2 } else { 1 };
                let (mut fg, mut bg) = (color(cell.fgcolor(), true, cell.bold(), &t), color(cell.bgcolor(), false, false, &t));
                if cell.inverse() {
                    std::mem::swap(&mut fg, &mut bg);
                    if bg == Color32::TRANSPARENT {
                        bg = term_fg(&t);
                    }
                    if fg == Color32::TRANSPARENT {
                        fg = term_bg(&t);
                    }
                }
                if cell.dim() {
                    fg = fg.gamma_multiply(0.6);
                }
                if in_sel(r, c) {
                    bg = t.ink.gamma_multiply(0.25);
                }
                if bg != Color32::TRANSPARENT {
                    painter.rect_filled(cell_rect(r, c, width), 0.0, bg);
                }
                if cell.underline() {
                    let y = cell_rect(r, c, width).bottom() - 1.5;
                    painter.line_segment([Pos2::new(cell_rect(r, c, 1).left(), y), Pos2::new(cell_rect(r, c, width).right(), y)], egui::Stroke::new(1.0, fg));
                }
                let s = cell.contents();
                // ASCII in one style is laid out as a run (the font is monospaced for it); anything
                // else is placed in its own cell so fallback glyphs can't push the grid out of line
                let ascii = s.len() == 1 && s.is_ascii();
                if ascii && fg == run_color && !run.is_empty() {
                    run.push_str(s);
                    continue;
                }
                flush(&mut run, run_start, run_color);
                if ascii {
                    run.push_str(s);
                    run_start = c;
                    run_color = fg;
                } else if cell.has_contents() {
                    painter.text(cell_rect(r, c, width).center(), egui::Align2::CENTER_CENTER, s, font.clone(), fg);
                } else {
                    run.push(' ');
                    run_start = c;
                    run_color = fg;
                }
            }
            flush(&mut run, run_start, run_color);
        }

        if !screen.hide_cursor() && screen.scrollback() == 0 && !self.exited() {
            let (cr, cc) = screen.cursor_position();
            let cur = cell_rect(cr, cc, 1);
            if focused {
                painter.rect_filled(cur, 1.0, term_fg(&t).gamma_multiply(0.7));
            } else {
                painter.rect_stroke(cur, 1.0, egui::Stroke::new(1.0, term_fg(&t).gamma_multiply(0.6)), egui::StrokeKind::Inside);
            }
        }
        if screen.scrollback() > 0 {
            let label = format!("↑ {} lines", screen.scrollback());
            painter.text(rect.right_top() + vec2(-14.0, 10.0), egui::Align2::RIGHT_TOP, label, FontId::proportional(11.0), t.muted);
        }
        resp
    }

    fn scroll(&mut self, lines: i32) {
        let Ok(mut p) = self.parser.lock() else { return };
        if p.screen().alternate_screen() {
            // full-screen programs scroll themselves: send arrow keys
            drop(p);
            let key: &[u8] = if lines > 0 { b"\x1b[A" } else { b"\x1b[B" };
            for _ in 0..lines.unsigned_abs().min(10) {
                self.send(key);
            }
            return;
        }
        let now = p.screen().scrollback() as i64;
        let next = (now + i64::from(lines)).max(0) as usize;
        p.screen_mut().set_scrollback(next);
    }

    fn reset_scroll(&self) {
        if let Ok(mut p) = self.parser.lock()
            && p.screen().scrollback() != 0
        {
            p.screen_mut().set_scrollback(0);
        }
    }

    fn input(&mut self, ui: &egui::Ui, ev: &egui::Event) {
        let (app_cursor, bracketed) = self.parser.lock().map(|p| (p.screen().application_cursor(), p.screen().bracketed_paste())).unwrap_or((false, false));
        let bytes: Vec<u8> = match ev {
            egui::Event::Text(s) => s.as_bytes().to_vec(),
            egui::Event::Paste(s) => {
                let s = s.replace("\r\n", "\r").replace('\n', "\r");
                if bracketed { format!("\x1b[200~{s}\x1b[201~").into_bytes() } else { s.into_bytes() }
            }
            egui::Event::Copy => {
                if let Some(text) = self.selected_text() {
                    ui.ctx().copy_text(text);
                    self.selection = None;
                    return;
                }
                if ui.input(|i| i.modifiers.mac_cmd) {
                    return;
                }
                vec![0x03]
            }
            egui::Event::Cut => vec![0x18],
            egui::Event::Key { key, pressed: true, modifiers, .. } => match key_bytes(*key, *modifiers, app_cursor) {
                Some(b) => b,
                None => return,
            },
            _ => return,
        };
        if !bytes.is_empty() {
            self.reset_scroll();
            self.selection = None;
            self.send(&bytes);
        }
    }

    fn selected_text(&self) -> Option<String> {
        let ((a, b), p) = (self.selection?, self.parser.lock().ok()?);
        let (s, e) = if a <= b { (a, b) } else { (b, a) };
        if s == e {
            return None;
        }
        let text = p.screen().contents_between(s.0, s.1, e.0, e.1 + 1);
        (!text.is_empty()).then_some(text)
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

/// Process output, answering the two queries interactive programs wait on: cursor position
/// (`ESC[6n`) and primary device attributes (`ESC[c`).
fn feed(parser: &Mutex<vt100::Parser>, writer: &Writer, mut bytes: &[u8]) {
    let Ok(mut p) = parser.lock() else { return };
    while let Some((at, len, cpr)) = find_query(bytes) {
        p.process(&bytes[..at + len]);
        let reply = if cpr {
            let (r, c) = p.screen().cursor_position();
            format!("\x1b[{};{}R", r + 1, c + 1)
        } else {
            "\x1b[?62;22c".to_string()
        };
        if let Ok(mut w) = writer.lock() {
            let _ = w.write_all(reply.as_bytes());
            let _ = w.flush();
        }
        bytes = &bytes[at + len..];
    }
    p.process(bytes);
}

/// The first query in `bytes`: (offset, length, is-cursor-position-request).
fn find_query(bytes: &[u8]) -> Option<(usize, usize, bool)> {
    const QUERIES: [(&[u8], bool); 3] = [(b"\x1b[6n", true), (b"\x1b[c", false), (b"\x1b[0c", false)];
    QUERIES.iter().filter_map(|(q, cpr)| bytes.windows(q.len()).position(|w| w == *q).map(|at| (at, q.len(), *cpr))).min_by_key(|(at, _, _)| *at)
}

/// The bytes a key sends (xterm conventions).
fn key_bytes(key: egui::Key, m: egui::Modifiers, app_cursor: bool) -> Option<Vec<u8>> {
    use egui::Key as K;
    let modcode = 1 + u8::from(m.shift) + 2 * u8::from(m.alt) + 4 * u8::from(m.ctrl);
    let csi = |tail: &str| -> Vec<u8> {
        // arrows, Home/End: ESC[1;<mod>X with modifiers, else ESC[X (or ESCOX in app-cursor mode)
        if modcode > 1 {
            format!("\x1b[1;{modcode}{tail}").into_bytes()
        } else if app_cursor {
            format!("\x1bO{tail}").into_bytes()
        } else {
            format!("\x1b[{tail}").into_bytes()
        }
    };
    let tilde = |n: u8| -> Vec<u8> { if modcode > 1 { format!("\x1b[{n};{modcode}~").into_bytes() } else { format!("\x1b[{n}~").into_bytes() } };
    Some(match key {
        // Shift/Alt+Enter: a newline in the prompt for CLIs that support it
        K::Enter if m.shift || m.alt => b"\x1b\r".to_vec(),
        K::Enter => b"\r".to_vec(),
        K::Backspace if m.alt => b"\x1b\x7f".to_vec(),
        K::Backspace if m.ctrl => vec![0x08],
        K::Backspace => vec![0x7f],
        K::Tab if m.shift => b"\x1b[Z".to_vec(),
        K::Tab => b"\t".to_vec(),
        K::Escape => vec![0x1b],
        K::ArrowUp => csi("A"),
        K::ArrowDown => csi("B"),
        K::ArrowRight => csi("C"),
        K::ArrowLeft => csi("D"),
        K::Home => csi("H"),
        K::End => csi("F"),
        K::Insert => tilde(2),
        K::Delete => tilde(3),
        K::PageUp => tilde(5),
        K::PageDown => tilde(6),
        K::F1 => b"\x1bOP".to_vec(),
        K::F2 => b"\x1bOQ".to_vec(),
        K::F3 => b"\x1bOR".to_vec(),
        K::F4 => b"\x1bOS".to_vec(),
        K::F5 => tilde(15),
        K::F6 => tilde(17),
        K::F7 => tilde(18),
        K::F8 => tilde(19),
        K::F9 => tilde(20),
        K::F10 => tilde(21),
        K::F11 => tilde(23),
        K::F12 => tilde(24),
        K::Space if m.ctrl => vec![0],
        _ if m.ctrl && !m.alt => {
            // Ctrl+letter → control byte (Ctrl+C/X/V arrive as Copy/Cut/Paste events instead)
            let name = key.name();
            let c = name.chars().next().filter(|c| name.len() == 1 && c.is_ascii_alphabetic())?;
            vec![(c.to_ascii_uppercase() as u8) - b'A' + 1]
        }
        _ if m.alt && !m.ctrl => {
            // Alt+letter → ESC letter (egui sends no text while Alt is held on most platforms)
            let name = key.name();
            let c = name.chars().next().filter(|c| name.len() == 1 && c.is_ascii_alphanumeric())?;
            let c = if m.shift { c.to_ascii_uppercase() } else { c.to_ascii_lowercase() };
            vec![0x1b, c as u8]
        }
        _ => return None,
    })
}

fn term_bg(t: &theme::Tokens) -> Color32 {
    if theme::is_dark() { Color32::from_rgb(0x12, 0x12, 0x12) } else { t.row }
}

fn term_fg(t: &theme::Tokens) -> Color32 {
    t.ink
}

/// CreativeCraft's 16-colour palette, tuned to read on each theme's terminal background.
fn ansi16(i: u8) -> Color32 {
    const DARK: [u32; 16] = [
        0x2a2a2a, 0xe06c62, 0x8cc68a, 0xdcbc6c, 0x6fa3e0, 0xc08ad8, 0x6cc4c0, 0xd8d6cf, 0x6b6a66, 0xf08a80, 0xa6dca4, 0xf0d488, 0x8fbcf0, 0xd6a6ec, 0x8adcd8,
        0xf6f5f0,
    ];
    const LIGHT: [u32; 16] = [
        0x2b2b29, 0xb8382e, 0x2e7d3c, 0x8a6a10, 0x2a5fb0, 0x8a3fa8, 0x1e7a78, 0xd0cec6, 0x6a6963, 0xd04a3e, 0x3a9a4c, 0xa88420, 0x3a74cc, 0xa456c4, 0x2a9490,
        0xf6f5f1,
    ];
    let v = if theme::is_dark() { DARK } else { LIGHT }[usize::from(i & 15)];
    Color32::from_rgb((v >> 16) as u8, (v >> 8) as u8, v as u8)
}

/// A cell colour; `Default` is transparent for backgrounds and the ink colour for text.
fn color(c: vt100::Color, fg: bool, bold: bool, t: &theme::Tokens) -> Color32 {
    match c {
        vt100::Color::Default if fg => term_fg(t),
        vt100::Color::Default => Color32::TRANSPARENT,
        // bold text in one of the 8 base colours shows in its bright variant, as terminals do
        vt100::Color::Idx(i) if i < 8 && bold && fg => ansi16(i + 8),
        vt100::Color::Idx(i) if i < 16 => ansi16(i),
        vt100::Color::Idx(i) if i < 232 => {
            let i = i - 16;
            let level = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
            Color32::from_rgb(level(i / 36), level((i / 6) % 6), level(i % 6))
        }
        vt100::Color::Idx(i) => {
            let g = 8 + (i - 232) * 10;
            Color32::from_rgb(g, g, g)
        }
        vt100::Color::Rgb(r, g, b) => Color32::from_rgb(r, g, b),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_queries_in_order() {
        assert_eq!(find_query(b"hello\x1b[6nworld"), Some((5, 4, true)));
        assert_eq!(find_query(b"\x1b[c then \x1b[6n"), Some((0, 3, false)));
        assert_eq!(find_query(b"plain text"), None);
    }

    #[test]
    fn keys() {
        let none = egui::Modifiers::NONE;
        assert_eq!(key_bytes(egui::Key::ArrowUp, none, false), Some(b"\x1b[A".to_vec()));
        assert_eq!(key_bytes(egui::Key::ArrowUp, none, true), Some(b"\x1bOA".to_vec()));
        assert_eq!(key_bytes(egui::Key::ArrowLeft, egui::Modifiers::CTRL, false), Some(b"\x1b[1;5D".to_vec()));
        assert_eq!(key_bytes(egui::Key::D, egui::Modifiers::CTRL, false), Some(vec![4]));
        assert_eq!(key_bytes(egui::Key::Enter, egui::Modifiers::SHIFT, false), Some(b"\x1b\r".to_vec()));
        assert_eq!(key_bytes(egui::Key::A, none, false), None, "plain letters come as Text events");
    }

    #[test]
    fn colour_cube() {
        let t = theme::Tokens::get();
        assert_eq!(color(vt100::Color::Idx(16), true, false, &t), Color32::from_rgb(0, 0, 0));
        assert_eq!(color(vt100::Color::Idx(231), true, false, &t), Color32::from_rgb(255, 255, 255));
        assert_eq!(color(vt100::Color::Default, false, false, &t), Color32::TRANSPARENT);
    }
}
