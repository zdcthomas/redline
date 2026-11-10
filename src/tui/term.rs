//! A terminal inside the TUI: runs the user's editor on a pty and draws its
//! screen in a floating pane, like a tmux popup but with no tmux.

use anyhow::Result;
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use ratatui::buffer::Buffer;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use std::io::{Read, Write};
use std::num::NonZeroU16;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

pub struct Term {
    parser: Arc<Mutex<vt100::Parser>>,
    writer: Box<dyn Write + Send>,
    child: Box<dyn Child + Send + Sync>,
    master: Box<dyn MasterPty + Send>,
    /// The reader thread sets this when the screen changed.
    pub dirty: Arc<AtomicBool>,
    size: (u16, u16),
}

fn nz(n: u16) -> NonZeroU16 {
    NonZeroU16::new(n.max(1)).unwrap()
}

impl Term {
    pub fn spawn(cmdline: &str, cwd: &Path, rows: u16, cols: u16) -> Result<Self> {
        let pair = native_pty_system().openpty(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })?;
        let mut cmd = CommandBuilder::new("sh");
        cmd.arg("-c");
        cmd.arg(cmdline);
        cmd.cwd(cwd);
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        let child = pair.slave.spawn_command(cmd)?;
        drop(pair.slave);
        let mut reader = pair.master.try_clone_reader()?;
        let writer = pair.master.take_writer()?;
        let parser = Arc::new(Mutex::new(vt100::Parser::new(nz(rows), nz(cols), 0)));
        let dirty = Arc::new(AtomicBool::new(true));
        let (p, d) = (parser.clone(), dirty.clone());
        std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            while let Ok(n) = reader.read(&mut buf) {
                if n == 0 {
                    break;
                }
                p.lock().unwrap().process(&buf[..n]);
                d.store(true, Ordering::Relaxed);
            }
            d.store(true, Ordering::Relaxed);
        });
        Ok(Self { parser, writer, child, master: pair.master, dirty, size: (rows, cols) })
    }

    pub fn resize(&mut self, rows: u16, cols: u16) {
        if (rows, cols) == self.size || rows == 0 || cols == 0 {
            return;
        }
        self.size = (rows, cols);
        let _ = self.master.resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 });
        self.parser.lock().unwrap().set_size(nz(rows), nz(cols));
    }

    /// `Some(success)` after the editor exits.
    pub fn exited(&mut self) -> Option<bool> {
        self.child.try_wait().ok().flatten().map(|s| s.success())
    }

    pub fn send_key(&mut self, key: KeyEvent) {
        let app_cursor = self.parser.lock().unwrap().screen().application_cursor();
        let _ = self.writer.write_all(&encode(key, app_cursor));
        let _ = self.writer.flush();
    }

    pub fn send_paste(&mut self, text: &str) {
        let bracketed = self.parser.lock().unwrap().screen().bracketed_paste();
        let _ = if bracketed {
            self.writer.write_all(format!("\x1b[200~{text}\x1b[201~").as_bytes())
        } else {
            self.writer.write_all(text.as_bytes())
        };
        let _ = self.writer.flush();
    }

    /// Draw the screen; returns the cursor position when the cursor shows.
    pub fn render(&self, area: Rect, buf: &mut Buffer) -> Option<(u16, u16)> {
        let parser = self.parser.lock().unwrap();
        let screen = parser.screen();
        for row in 0..area.height {
            for col in 0..area.width {
                let Some(cell) = screen.cell(row, col) else { continue };
                if cell.is_wide_continuation() {
                    continue;
                }
                let mut st = Style::new().fg(color(cell.fgcolor())).bg(color(cell.bgcolor()));
                let mut m = Modifier::empty();
                m.set(Modifier::BOLD, cell.bold());
                m.set(Modifier::DIM, cell.dim());
                m.set(Modifier::ITALIC, cell.italic());
                m.set(Modifier::UNDERLINED, cell.underline());
                m.set(Modifier::REVERSED, cell.inverse());
                st = st.add_modifier(m);
                if let Some(out) = buf.cell_mut((area.x + col, area.y + row)) {
                    out.reset();
                    out.set_symbol(if cell.has_contents() { cell.contents() } else { " " });
                    out.set_style(st);
                }
            }
        }
        if screen.hide_cursor() {
            return None;
        }
        let (r, c) = screen.cursor_position();
        (r < area.height && c < area.width).then_some((area.x + c, area.y + r))
    }
}

impl Drop for Term {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

fn color(c: vt100::Color) -> Color {
    match c {
        vt100::Color::Default => Color::Reset,
        vt100::Color::Idx(i) => Color::Indexed(i),
        vt100::Color::Rgb(r, g, b) => Color::Rgb(r, g, b),
    }
}

/// Encode a key the way xterm sends it.
fn encode(key: KeyEvent, app_cursor: bool) -> Vec<u8> {
    let m = key.modifiers;
    let (ctrl, alt, shift) =
        (m.contains(KeyModifiers::CONTROL), m.contains(KeyModifiers::ALT), m.contains(KeyModifiers::SHIFT));
    // xterm modifier parameter: 1 + shift(1) + alt(2) + ctrl(4)
    let modp = 1 + shift as u8 + 2 * alt as u8 + 4 * ctrl as u8;
    let csi = |fin: char| -> Vec<u8> {
        if modp > 1 {
            format!("\x1b[1;{modp}{fin}").into_bytes()
        } else if app_cursor && "ABCD".contains(fin) {
            format!("\x1bO{fin}").into_bytes()
        } else {
            format!("\x1b[{fin}").into_bytes()
        }
    };
    let tilde = |n: u8| -> Vec<u8> {
        if modp > 1 { format!("\x1b[{n};{modp}~").into_bytes() } else { format!("\x1b[{n}~").into_bytes() }
    };
    let with_alt = |mut v: Vec<u8>| {
        if alt {
            v.insert(0, 0x1b);
        }
        v
    };
    match key.code {
        KeyCode::Char(c) => {
            if ctrl {
                let b = match c.to_ascii_lowercase() {
                    '@' | ' ' | '2' => 0,
                    c @ 'a'..='z' => c as u8 - b'a' + 1,
                    '[' | '3' => 27,
                    '\\' | '4' => 28,
                    ']' | '5' => 29,
                    '^' | '6' => 30,
                    '_' | '7' | '/' => 31,
                    '?' | '8' => 127,
                    _ => return with_alt(c.to_string().into_bytes()),
                };
                with_alt(vec![b])
            } else {
                with_alt(c.to_string().into_bytes())
            }
        }
        KeyCode::Enter => with_alt(vec![b'\r']),
        KeyCode::Esc => vec![0x1b],
        KeyCode::Backspace => with_alt(vec![if ctrl { 0x08 } else { 0x7f }]),
        KeyCode::Tab => vec![b'\t'],
        KeyCode::BackTab => b"\x1b[Z".to_vec(),
        KeyCode::Up => csi('A'),
        KeyCode::Down => csi('B'),
        KeyCode::Right => csi('C'),
        KeyCode::Left => csi('D'),
        KeyCode::Home => csi('H'),
        KeyCode::End => csi('F'),
        KeyCode::Insert => tilde(2),
        KeyCode::Delete => tilde(3),
        KeyCode::PageUp => tilde(5),
        KeyCode::PageDown => tilde(6),
        KeyCode::F(n) => match n {
            1..=4 => vec![0x1b, b'O', b'P' + (n - 1)],
            5 => tilde(15),
            6..=8 => tilde(11 + n),
            9..=10 => tilde(11 + n),
            11..=12 => tilde(12 + n),
            _ => vec![],
        },
        _ => vec![],
    }
}
