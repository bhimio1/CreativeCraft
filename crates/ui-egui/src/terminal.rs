//! A terminal inside the window: a real PTY (`portable-pty`; ConPTY on Windows) running one
//! program, its output parsed by `vt100` and drawn cell by cell with egui's monospace font.
//!
//! Enough of a terminal for agent CLIs' interactive UIs: 256 colours and true colour, the
//! alternate screen, application cursor keys, bracketed paste, scrollback, cursor-position and
//! device-attribute replies, mouse selection and copy (double-click a word, triple-click a line),
//! mouse reporting for full-screen programs that ask for it (Shift+drag still selects), and
//! Ctrl +/−/0 to change the text size. Ctrl+C copies when there is a selection and otherwise
//! interrupts, as in most desktop terminals.

use crate::theme;
use creativecraft_engine::TerminalSettings;
use creativecraft_engine::agent::LaunchSpec;
use egui::{Color32, FontId, Pos2, Rect, Sense, vec2};
use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

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
    /// A mouse button held down while the program receives the mouse, and the cell last reported.
    mouse_down: Option<u8>,
    mouse_cell: Option<(u16, u16)>,
    /// Ctrl +/−/0 asked for a bigger (+1), smaller (−1) or default (0) text size; the page applies it.
    pub zoom: Option<i8>,
}

impl Terminal {
    pub fn spawn(spec: &LaunchSpec, ctx: &egui::Context, scrollback: u32) -> Result<Terminal, String> {
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
        let parser = Arc::new(Mutex::new(vt100::Parser::new(rows, cols, scrollback.clamp(100, 100_000) as usize)));
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

        Ok(Terminal {
            parser,
            writer,
            master: pair.master,
            child,
            exited,
            size: (rows, cols),
            selection: None,
            scroll_acc: 0.0,
            mouse_down: None,
            mouse_cell: None,
            zoom: None,
        })
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
    pub fn show(&mut self, ui: &mut egui::Ui, settings: &TerminalSettings) -> egui::Response {
        let t = theme::Tokens::get();
        let font = FontId::monospace(f32::from(settings.font_size.clamp(9, 28)));
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

        // ── mouse: to the program when it asks (unless Shift is held), else selection and scrollback
        let (mode, encoding) = self.parser.lock().map(|p| (p.screen().mouse_protocol_mode(), p.screen().mouse_protocol_encoding())).unwrap_or_default();
        let shift = ui.input(|i| i.modifiers.shift);
        let reporting = settings.mouse_reporting && mode != vt100::MouseProtocolMode::None && !shift && !self.exited();
        if reporting {
            self.selection = None;
            self.report_mouse(ui, inner, &cell_at, mode, encoding, resp.hovered());
        } else if resp.triple_clicked()
            && let Some(p) = resp.interact_pointer_pos()
        {
            let (r, _) = cell_at(p);
            self.selection = Some(((r, 0), (r, cols - 1)));
        } else if resp.double_clicked()
            && let Some(p) = resp.interact_pointer_pos()
        {
            let (r, c) = cell_at(p);
            self.selection = self.word_at(r, c, cols);
        } else if resp.drag_started()
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
        if resp.hovered() && !reporting {
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

    /// The word (or run of non-space, non-punctuation cells) around (`r`, `c`).
    fn word_at(&self, r: u16, c: u16, cols: u16) -> Option<((u16, u16), (u16, u16))> {
        let p = self.parser.lock().ok()?;
        let screen = p.screen();
        let in_word = |c: u16| {
            screen.cell(r, c).is_some_and(|cell| {
                let s = cell.contents();
                !s.is_empty() && s.chars().all(|ch| ch.is_alphanumeric() || "_-./~:@%+#".contains(ch))
            })
        };
        if !in_word(c) {
            return None;
        }
        let (mut a, mut b) = (c, c);
        while a > 0 && in_word(a - 1) {
            a -= 1;
        }
        while b + 1 < cols && in_word(b + 1) {
            b += 1;
        }
        Some(((r, a), (r, b)))
    }

    /// Mouse presses, releases, drags and the wheel, reported to a program that enabled the mouse.
    fn report_mouse(
        &mut self,
        ui: &egui::Ui,
        inner: Rect,
        cell_at: &dyn Fn(Pos2) -> (u16, u16),
        mode: vt100::MouseProtocolMode,
        encoding: vt100::MouseProtocolEncoding,
        hovered: bool,
    ) {
        use vt100::MouseProtocolMode as Mode;
        let events = ui.input(|i| i.events.clone());
        for ev in events {
            match ev {
                egui::Event::PointerButton { pos, button, pressed, modifiers } => {
                    let code = match button {
                        egui::PointerButton::Primary => 0,
                        egui::PointerButton::Middle => 1,
                        egui::PointerButton::Secondary => 2,
                        _ => continue,
                    };
                    let cell = cell_at(pos);
                    if pressed && inner.contains(pos) {
                        self.mouse_down = Some(code);
                        self.mouse_cell = Some(cell);
                        self.send(&mouse_report(encoding, code, true, cell, modifiers));
                    } else if !pressed && self.mouse_down == Some(code) {
                        self.mouse_down = None;
                        if mode != Mode::Press {
                            self.send(&mouse_report(encoding, code, false, cell, modifiers));
                        }
                    }
                }
                egui::Event::PointerMoved(pos) => {
                    let cell = cell_at(pos);
                    if self.mouse_cell == Some(cell) || !inner.contains(pos) {
                        continue;
                    }
                    let motion = match (self.mouse_down, mode) {
                        (Some(b), Mode::ButtonMotion | Mode::AnyMotion) => Some(b),
                        (None, Mode::AnyMotion) => Some(3),
                        _ => None,
                    };
                    self.mouse_cell = Some(cell);
                    if let Some(b) = motion {
                        self.send(&mouse_report(encoding, b + 32, true, cell, egui::Modifiers::NONE));
                    }
                }
                egui::Event::MouseWheel { delta, modifiers, .. } if hovered => {
                    let Some(pos) = ui.input(|i| i.pointer.hover_pos()) else { continue };
                    let lines = if delta.y.abs() >= 1.0 { delta.y.abs().round().min(5.0) as usize } else { 1 };
                    let code = if delta.y > 0.0 { 64 } else { 65 };
                    for _ in 0..lines {
                        self.send(&mouse_report(encoding, code, true, cell_at(pos), modifiers));
                    }
                }
                _ => {}
            }
        }
    }

    fn input(&mut self, ui: &egui::Ui, ev: &egui::Event) {
        // Ctrl +/−/0: text size, for the page to apply
        if let egui::Event::Key { key, pressed: true, modifiers, .. } = ev
            && modifiers.command
            && !modifiers.shift_only()
        {
            let zoom = match key {
                egui::Key::Plus | egui::Key::Equals => Some(1),
                egui::Key::Minus => Some(-1),
                egui::Key::Num0 => Some(0),
                _ => None,
            };
            if zoom.is_some() {
                self.zoom = zoom;
                return;
            }
        }
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

/// One mouse event for a program, in the encoding it asked for. `button`: 0 left, 1 middle, 2
/// right, 3 no button (motion), 64/65 wheel up/down, +32 for motion; `cell` is (row, col).
/// Shift isn't passed on: it's what selects text instead.
fn mouse_report(encoding: vt100::MouseProtocolEncoding, button: u8, press: bool, cell: (u16, u16), m: egui::Modifiers) -> Vec<u8> {
    let mods = if m.alt { 8 } else { 0 } + if m.ctrl { 16 } else { 0 };
    let (row, col) = (u32::from(cell.0) + 1, u32::from(cell.1) + 1);
    match encoding {
        vt100::MouseProtocolEncoding::Sgr => format!("\x1b[<{};{col};{row}{}", button + mods, if press { 'M' } else { 'm' }).into_bytes(),
        enc => {
            // the classic form: release is button 3, every number offset by 32
            let b = u32::from(if press { button } else { 3 } + mods) + 32;
            let mut out = b"\x1b[M".to_vec();
            for n in [b, col + 32, row + 32] {
                if enc == vt100::MouseProtocolEncoding::Utf8 {
                    let mut buf = [0u8; 4];
                    out.extend_from_slice(char::from_u32(n).unwrap_or(' ').encode_utf8(&mut buf).as_bytes());
                } else {
                    out.push(n.min(255) as u8);
                }
            }
            out
        }
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

    #[test]
    fn mouse_reports_in_each_encoding() {
        use vt100::MouseProtocolEncoding as E;
        let none = egui::Modifiers::NONE;
        assert_eq!(mouse_report(E::Sgr, 0, true, (4, 9), none), b"\x1b[<0;10;5M");
        assert_eq!(mouse_report(E::Sgr, 0, false, (4, 9), none), b"\x1b[<0;10;5m");
        assert_eq!(mouse_report(E::Sgr, 64, true, (0, 0), egui::Modifiers::CTRL), b"\x1b[<80;1;1M");
        // classic: release is button 3; numbers + 32
        assert_eq!(mouse_report(E::Default, 0, true, (4, 9), none), [0x1b, b'[', b'M', 32, 42, 37]);
        assert_eq!(mouse_report(E::Default, 2, false, (0, 0), none), [0x1b, b'[', b'M', 35, 33, 33]);
        // UTF-8: big coordinates become multi-byte characters
        let wide = mouse_report(E::Utf8, 0, true, (0, 299), none);
        assert_eq!(&wide[..4], &[0x1b, b'[', b'M', 32]);
        assert_eq!(std::str::from_utf8(&wide[4..]).unwrap().chars().next(), char::from_u32(300 + 32));
    }

    /// A real program in the terminal turns on SGR mouse reporting; a click on the widget reaches
    /// it as an SGR press and release at the clicked cell.
    #[cfg(unix)]
    #[test]
    fn clicks_reach_a_program_that_asked_for_the_mouse() {
        let dir = std::env::temp_dir().join(format!("cc-term-mouse-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let got = dir.join("got");
        // enable press/release reporting in SGR form, then record the next 20 bytes of input (a press and a release at this cell)
        let script = format!("printf '\\033[?1000h\\033[?1006h'; stty raw -echo; head -c 20 > '{}'", got.display());
        let spec = LaunchSpec { program: "sh".into(), args: vec!["-c".into(), script], env: Vec::new(), cwd: dir.clone() };
        let ctx = egui::Context::default();
        let mut term = Terminal::spawn(&spec, &ctx, 1000).unwrap();
        let settings = TerminalSettings::default();
        let parser = term.parser.clone();
        let size = egui::Rect::from_min_size(Pos2::ZERO, vec2(800.0, 400.0));
        let mut frame = |events: Vec<egui::Event>| {
            let input = egui::RawInput { screen_rect: Some(size), events, ..Default::default() };
            let mut out = ctx.run_ui(input, |ui| {
                egui::CentralPanel::default().frame(egui::Frame::NONE).show(ui, |ui| {
                    term.show(ui, &settings);
                });
            });
            // no renderer here: drop the texture updates deliberately
            out.textures_delta.clear();
        };
        // wait for the program to switch the mouse on
        let started = std::time::Instant::now();
        loop {
            frame(Vec::new());
            let on = parser.lock().unwrap().screen().mouse_protocol_mode() != vt100::MouseProtocolMode::None;
            if on || started.elapsed() > std::time::Duration::from_secs(10) {
                assert!(on, "the program enabled the mouse");
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        // a click at x=100, y=60 → inside the 10 px margin; work out the cell the same way
        let at = Pos2::new(100.0, 60.0);
        let press = egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE };
        let release = egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE };
        frame(vec![egui::Event::PointerMoved(at), press]);
        frame(vec![release]);
        let started = std::time::Instant::now();
        let text = loop {
            let t = std::fs::read(&got).unwrap_or_default();
            if t.len() >= 20 || started.elapsed() > std::time::Duration::from_secs(10) {
                break String::from_utf8_lossy(&t).into_owned();
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        };
        let (cw, ch) = ctx.fonts_mut(|f| {
            let font = FontId::monospace(f32::from(settings.font_size));
            (f.glyph_width(&font, 'M'), f.row_height(&font))
        });
        let (col, row) = (((at.x - 10.0) / cw).floor() as u32 + 1, ((at.y - 10.0) / ch).floor() as u32 + 1);
        assert!(text.starts_with(&format!("\x1b[<0;{col};{row}M\x1b[<0;{col};{row}m")), "{text:?} (expected col {col}, row {row})");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
