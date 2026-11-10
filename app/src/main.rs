//! redline as a native window. It shows the same state as the TUI and the
//! Neovim plugin: the fold of the event log. The human reviews proposals and
//! comments with the keyboard, and edits blocks in a text box.

use eframe::egui::{
    self, Align, Align2, Color32, FontFamily, FontId, Frame, Key, Margin, RichText, Sense, Stroke, TextFormat,
    text::LayoutJob,
};
use redline::anchor;
use redline::ops::Session;
use redline::store::{Kind, Thread, ago};
use std::ops::Range;
use std::time::{Duration, Instant};

const BG: Color32 = Color32::from_rgb(20, 22, 29);
const BAR: Color32 = Color32::from_rgb(24, 26, 34);
const CURSOR_BG: Color32 = Color32::from_rgb(34, 38, 50);
const BLOCK_BG: Color32 = Color32::from_rgb(26, 29, 38);
const TEXT: Color32 = Color32::from_rgb(214, 220, 232);
const DIM: Color32 = Color32::from_rgb(120, 128, 150);
const FAINT: Color32 = Color32::from_rgb(70, 76, 96);
const ACCENT: Color32 = Color32::from_rgb(255, 92, 87);
const ADD: Color32 = Color32::from_rgb(130, 214, 140);
const ADD_BG: Color32 = Color32::from_rgb(28, 54, 38);
const DEL: Color32 = Color32::from_rgb(240, 120, 124);
const DEL_BG: Color32 = Color32::from_rgb(64, 30, 36);
const NOTE_BG: Color32 = Color32::from_rgb(58, 50, 24);
const AGENT: Color32 = Color32::from_rgb(187, 154, 247);
const HUMAN: Color32 = Color32::from_rgb(122, 162, 247);
const CODE: Color32 = Color32::from_rgb(137, 201, 214);
const H2: Color32 = Color32::from_rgb(255, 170, 110);
/// Comments that share lines get different colors.
const PALETTE: [Color32; 5] = [
    Color32::from_rgb(240, 198, 116),
    Color32::from_rgb(122, 162, 247),
    Color32::from_rgb(187, 154, 247),
    Color32::from_rgb(137, 201, 214),
    Color32::from_rgb(255, 170, 110),
];

fn kind_color(k: Kind) -> Color32 {
    match k {
        Kind::Comment => PALETTE[0],
        Kind::Edit => H2,
        Kind::Insert => ADD,
        Kind::Delete => DEL,
    }
}
fn kind_icon(k: Kind) -> &'static str {
    match k {
        Kind::Comment => "●",
        Kind::Edit => "~",
        Kind::Insert => "+",
        Kind::Delete => "−",
    }
}

/// One thread drawn in the text.
struct Mark {
    id: u64,
    kind: Kind,
    /// Text that the thread covers.
    range: Range<usize>,
    /// Proposed text, shown at this offset.
    inject: Option<(usize, String)>,
    color: Color32,
}

enum Purpose {
    Comment { quote: String, line: usize },
    Reply(u64),
    Reject(u64),
    Modify(u64),
    Edit(Range<usize>),
    NewBlock { after: Option<usize> },
}

struct Dialog {
    title: String,
    hint: &'static str,
    text: String,
    initial: String,
    purpose: Purpose,
    /// A dialog shows from the frame after the key that opened it, so that
    /// the key does not land in the text box.
    fresh: bool,
}

struct App {
    s: Session,
    /// (session version, marks, first line of each open thread).
    marks: (u64, Vec<Mark>, Vec<(u64, Option<usize>)>),
    cursor: usize,
    sel: Option<usize>,
    active: Option<u64>,
    dialog: Option<Dialog>,
    flash: Option<(String, Instant, bool)>,
    history: bool,
    help: bool,
    pending: Option<char>,
    /// Scroll the document / the thread list to the current item once.
    reveal: (bool, bool),
    last_poll: Instant,
    seen_max: u64,
}

fn main() -> eframe::Result {
    let Some(path) = std::env::args().nth(1) else {
        eprintln!("usage: redline-app FILE");
        std::process::exit(2);
    };
    let path = std::path::PathBuf::from(path);
    if !path.exists() {
        std::fs::write(&path, "").ok();
    }
    let s = match Session::open(&path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("redline-app: {e:#}");
            std::process::exit(1);
        }
    };
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(format!("redline — {name}"))
            .with_app_id("redline")
            .with_inner_size([1240.0, 860.0]),
        ..Default::default()
    };
    eframe::run_native(
        "redline",
        options,
        Box::new(move |cc| {
            let mut v = egui::Visuals::dark();
            v.panel_fill = BG;
            v.window_fill = BAR;
            v.override_text_color = Some(TEXT);
            v.extreme_bg_color = Color32::from_rgb(14, 15, 20);
            v.selection.bg_fill = Color32::from_rgb(48, 58, 88);
            cc.egui_ctx.set_visuals(v);
            let seen_max = s.state.threads.iter().map(|t| t.id).max().unwrap_or(0);
            let mut app = App {
                s,
                marks: (0, vec![], vec![]),
                cursor: 0,
                sel: None,
                active: None,
                dialog: None,
                flash: None,
                history: false,
                help: false,
                pending: None,
                reveal: (true, true),
                last_poll: Instant::now(),
                seen_max,
            };
            app.sync_active();
            Ok(Box::new(app))
        }),
    )
}

fn body_font(size: f32) -> FontId {
    FontId::new(size, FontFamily::Proportional)
}
fn mono(size: f32) -> FontId {
    FontId::new(size, FontFamily::Monospace)
}

impl App {
    // ── state ────────────────────────────────────────────────────────────

    fn say(&mut self, m: impl Into<String>) {
        self.flash = Some((m.into(), Instant::now(), false));
    }
    fn fail(&mut self, e: anyhow::Error) {
        self.flash = Some((format!("{e:#}"), Instant::now(), true));
    }

    fn lines(&self) -> Vec<Range<usize>> {
        let n = anchor::line_starts(&self.s.doc).len();
        (0..n).map(|i| anchor::lines_range(&self.s.doc, i, i)).collect()
    }

    /// Find the marks again after the state or the text changed.
    fn refresh(&mut self) {
        if self.marks.0 == self.s.version {
            return;
        }
        let doc = &self.s.doc;
        let mut marks: Vec<Mark> = vec![];
        let mut places = vec![];
        for (t, r) in self.s.open_threads() {
            places.push((t.id, r.as_ref().map(|r| anchor::line_of(doc, r.start))));
            let Some(r) = r else { continue };
            let lines = |m: &Range<usize>| anchor::line_of(doc, m.start)..=anchor::line_of(doc, m.end);
            match t.kind {
                Kind::Insert => {
                    let gap = if t.tight { "" } else { "\n" };
                    let text = if t.quote.is_empty() { format!("{}\n{gap}", t.new) } else { format!("\n{gap}{}", t.new) };
                    marks.push(Mark { id: t.id, kind: t.kind, range: r.end..r.end, inject: Some((r.end, text)), color: ADD });
                }
                Kind::Comment => {
                    // A color that no comment on the same lines has.
                    let mine = lines(&r);
                    let used: Vec<Color32> = marks
                        .iter()
                        .filter(|m| m.kind == Kind::Comment)
                        .filter(|m| {
                            let o = lines(&m.range);
                            o.start() <= mine.end() && mine.start() <= o.end()
                        })
                        .map(|m| m.color)
                        .collect();
                    let color = PALETTE.iter().copied().find(|c| !used.contains(c)).unwrap_or(PALETTE[marks.len() % PALETTE.len()]);
                    marks.push(Mark { id: t.id, kind: t.kind, range: r, inject: None, color });
                }
                _ => {
                    let clash = marks.iter().any(|m| m.kind != Kind::Comment && m.range.start < r.end && r.start < m.range.end);
                    if !clash {
                        let inject = (t.kind == Kind::Edit).then(|| (r.end, t.new.clone()));
                        marks.push(Mark { id: t.id, kind: t.kind, range: r, inject, color: DEL });
                    }
                }
            }
        }
        self.marks = (self.s.version, marks, places);
        let n = self.lines().len();
        self.cursor = self.cursor.min(n - 1);
        if self.active.is_some_and(|a| !self.marks.2.iter().any(|p| p.0 == a)) {
            self.active = None;
        }
    }

    fn thread(&self, id: u64) -> Option<&Thread> {
        self.s.state.threads.iter().find(|t| t.id == id)
    }

    fn threads_on(&self, line: usize) -> Vec<u64> {
        let doc = &self.s.doc;
        self.marks
            .1
            .iter()
            .filter(|m| (anchor::line_of(doc, m.range.start)..=anchor::line_of(doc, m.range.end)).contains(&line))
            .map(|m| m.id)
            .collect()
    }

    fn sync_active(&mut self) {
        self.refresh();
        let here = self.threads_on(self.cursor);
        if !self.active.is_some_and(|a| here.contains(&a)) {
            self.active = here.first().copied();
            self.reveal.1 = true;
        }
    }

    fn move_to(&mut self, line: usize) {
        self.cursor = line.min(self.lines().len() - 1);
        self.reveal.0 = true;
        self.sync_active();
    }

    fn block_at(&self, line: usize) -> Option<(usize, usize)> {
        anchor::blocks(&self.s.doc).into_iter().find(|(a, b)| (*a..=*b).contains(&line))
    }

    /// The lines an action applies to: the selection, or the block at the cursor.
    fn target(&self) -> Option<(usize, usize)> {
        match self.sel {
            Some(s) => Some((s.min(self.cursor), s.max(self.cursor))),
            None => self.block_at(self.cursor),
        }
    }

    fn step_thread(&mut self, fwd: bool) {
        let order = &self.marks.2;
        if order.is_empty() {
            return self.say("no open threads");
        }
        let i = self.active.and_then(|a| order.iter().position(|p| p.0 == a));
        let next = match i {
            Some(i) if fwd => (i + 1) % order.len(),
            Some(i) => (i + order.len() - 1) % order.len(),
            None if fwd => order.iter().position(|p| p.1.is_some_and(|l| l >= self.cursor)).unwrap_or(0),
            None => order.iter().rposition(|p| p.1.is_some_and(|l| l <= self.cursor)).unwrap_or(order.len() - 1),
        };
        let (id, line) = order[next];
        if let Some(l) = line {
            self.cursor = l;
        }
        self.sel = None;
        self.active = Some(id);
        self.reveal = (true, true);
    }

    fn open(&mut self, title: String, hint: &'static str, text: String, purpose: Purpose) {
        self.dialog = Some(Dialog { title, hint, initial: text.clone(), text, purpose, fresh: true });
    }

    // ── actions ──────────────────────────────────────────────────────────

    fn after(&mut self, r: anyhow::Result<()>, ok: String) {
        match r {
            Ok(()) => {
                self.sync_active();
                self.say(ok);
            }
            Err(e) => self.fail(e),
        }
    }

    fn accept(&mut self, id: u64, changed: Option<String>) {
        match self.s.accept(id, changed) {
            Ok(line) => {
                self.move_to(line);
                self.say(format!("accepted #{id}  ·  u to undo"));
            }
            Err(e) => self.fail(e),
        }
    }

    fn submit(&mut self, d: Dialog) {
        let text = d.text.trim_end().to_string();
        let body = text.trim().to_string();
        match d.purpose {
            Purpose::Comment { quote, line } => {
                if body.is_empty() {
                    return self.say("empty comment: not added");
                }
                match self.s.comment(quote, line, 0, body) {
                    Ok(id) => {
                        self.sel = None;
                        self.seen_max = self.seen_max.max(id);
                        self.refresh();
                        self.active = Some(id);
                        self.reveal.1 = true;
                        self.say(format!("comment #{id} added"));
                    }
                    Err(e) => self.fail(e),
                }
            }
            Purpose::Reply(id) => {
                if !body.is_empty() {
                    let r = self.s.reply(id, body);
                    self.after(r, format!("replied on #{id}"));
                }
            }
            Purpose::Reject(id) => {
                let r = self.s.reject(id, (!body.is_empty()).then_some(body));
                self.after(r, format!("rejected #{id}"));
            }
            Purpose::Modify(id) => {
                if text == d.initial {
                    self.say("no change  ·  a accepts the proposal as it is");
                } else {
                    self.accept(id, Some(text));
                }
            }
            Purpose::Edit(range) => {
                if text == d.initial {
                    return self.say("no change");
                }
                let l0 = anchor::line_of(&self.s.doc, range.start);
                let r = if body.is_empty() {
                    let l1 = anchor::line_of(&self.s.doc, range.end);
                    let (r, t) = anchor::delete_lines(&self.s.doc, l0, l1);
                    self.s.edit(r, &t, "delete")
                } else {
                    self.s.edit(range, &text, "edit")
                };
                self.sel = None;
                self.after(r, "saved  ·  u to undo".into());
                self.move_to(l0);
            }
            Purpose::NewBlock { after } => {
                if body.is_empty() {
                    return self.say("empty block: nothing added");
                }
                let (r, t) = anchor::block_insert(&self.s.doc, after, &text);
                let pos = r.start + t.len() - t.trim_start_matches('\n').len();
                let res = self.s.edit(r, &t, "new");
                self.after(res, "block added  ·  u to undo".into());
                let line = anchor::line_of(&self.s.doc, pos);
                self.move_to(line);
            }
        }
    }

    fn key_char(&mut self, c: char) {
        let pending = self.pending.take();
        let last = self.lines().len() - 1;
        let active = self.active.and_then(|a| self.thread(a)).cloned();
        let none = "no thread here  ·  n goes to the next thread";
        match c {
            'j' => self.move_to(self.cursor + 1),
            'k' => self.move_to(self.cursor.saturating_sub(1)),
            'g' if pending == Some('g') => self.move_to(0),
            'g' => self.pending = Some('g'),
            'G' => self.move_to(last),
            '}' => {
                let next = anchor::blocks(&self.s.doc).iter().find(|b| b.0 > self.cursor).map(|b| b.0).unwrap_or(last);
                self.move_to(next);
            }
            '{' => {
                let prev = anchor::blocks(&self.s.doc).iter().rev().find(|b| b.0 < self.cursor).map(|b| b.0).unwrap_or(0);
                self.move_to(prev);
            }
            'n' | ']' => self.step_thread(true),
            'N' | '[' => self.step_thread(false),
            'v' | 'V' => self.sel = if self.sel.is_some() { None } else { Some(self.cursor) },
            'd' if self.sel.is_some() || pending == Some('d') => match self.target() {
                Some((l0, l1)) => {
                    let (r, t) = anchor::delete_lines(&self.s.doc, l0, l1);
                    let res = self.s.edit(r, &t, "delete");
                    self.sel = None;
                    self.after(res, format!("deleted {} lines  ·  u to undo", l1 - l0 + 1));
                    self.move_to(l0);
                }
                None => self.say("nothing to delete here"),
            },
            'd' => self.pending = Some('d'),
            'e' => match self.target() {
                Some((l0, l1)) => {
                    let range = anchor::lines_range(&self.s.doc, l0, l1);
                    let text = self.s.doc[range.clone()].to_string();
                    let title = if l0 == l1 { format!("Edit L{}", l0 + 1) } else { format!("Edit L{}–{}", l0 + 1, l1 + 1) };
                    self.open(title, "", text, Purpose::Edit(range));
                }
                None => {
                    let after = self.cursor.checked_sub(1);
                    self.open("New block".into(), "", String::new(), Purpose::NewBlock { after });
                }
            },
            'o' => {
                let after = Some(self.target().map(|t| t.1).unwrap_or(self.cursor));
                self.open("New block".into(), "", String::new(), Purpose::NewBlock { after });
            }
            'O' => {
                let after = self.target().map(|t| t.0).unwrap_or(self.cursor).checked_sub(1);
                self.open("New block".into(), "", String::new(), Purpose::NewBlock { after });
            }
            'c' => match self.target() {
                Some((l0, l1)) => {
                    let quote = self.s.doc[anchor::lines_range(&self.s.doc, l0, l1)].to_string();
                    let title = if l0 == l1 { format!("Comment on L{}", l0 + 1) } else { format!("Comment on L{}–{}", l0 + 1, l1 + 1) };
                    self.open(title, "", String::new(), Purpose::Comment { quote, line: l0 });
                }
                None => self.say("no text to comment on"),
            },
            'a' => match active {
                Some(t) if t.kind.is_proposal() => self.accept(t.id, None),
                Some(_) => self.say("a comment has nothing to accept: R resolves it"),
                None => self.say(none),
            },
            'x' => match active {
                Some(t) if t.kind.is_proposal() => {
                    self.open(format!("Reject #{}", t.id), "reason (optional): the agent reads it", String::new(), Purpose::Reject(t.id));
                }
                Some(_) => self.say("R resolves a comment"),
                None => self.say(none),
            },
            'm' => match active {
                Some(t) if matches!(t.kind, Kind::Edit | Kind::Insert) => {
                    self.open(format!("Change #{}, then accept", t.id), "", t.new.clone(), Purpose::Modify(t.id));
                }
                Some(_) => self.say("only an edit or an insert has text to change"),
                None => self.say(none),
            },
            'r' => match active {
                Some(t) => self.open(format!("Reply on #{}", t.id), "", String::new(), Purpose::Reply(t.id)),
                None => self.say(none),
            },
            'R' => match active {
                Some(t) if t.kind == Kind::Comment => {
                    let r = self.s.resolve(t.id);
                    self.after(r, format!("resolved #{}", t.id));
                }
                Some(_) => self.say("a proposal needs a decision: a accept, x reject"),
                None => self.say(none),
            },
            'u' => match self.s.undo() {
                Ok(true) => {
                    self.sync_active();
                    self.say("undone");
                }
                Ok(false) => self.say("nothing to undo"),
                Err(e) => self.fail(e),
            },
            'H' => self.history = !self.history,
            '?' => self.help = true,
            _ => {}
        }
    }

    fn keys(&mut self, ui: &egui::Ui) {
        let events = ui.input(|i| i.events.clone());
        for ev in events {
            if self.dialog.is_some() {
                return;
            }
            if self.help {
                if matches!(ev, egui::Event::Text(_) | egui::Event::Key { pressed: true, .. }) {
                    self.help = false;
                }
                continue;
            }
            match ev {
                egui::Event::Text(t) => {
                    self.flash = None;
                    if self.history {
                        if t == "H" || t == "q" {
                            self.history = false;
                        }
                        continue;
                    }
                    for c in t.chars() {
                        self.key_char(c);
                    }
                }
                egui::Event::Key { key, pressed: true, modifiers, .. } => match key {
                    Key::Escape => {
                        self.sel = None;
                        self.history = false;
                        self.flash = None;
                    }
                    Key::ArrowDown => self.move_to(self.cursor + 1),
                    Key::ArrowUp => self.move_to(self.cursor.saturating_sub(1)),
                    Key::PageDown => self.move_to(self.cursor + 15),
                    Key::PageUp => self.move_to(self.cursor.saturating_sub(15)),
                    Key::D if modifiers.ctrl => self.move_to(self.cursor + 10),
                    Key::U if modifiers.ctrl => self.move_to(self.cursor.saturating_sub(10)),
                    Key::Enter => self.key_char('e'),
                    Key::Tab => {
                        let here = self.threads_on(self.cursor);
                        if !here.is_empty() {
                            let i = self.active.and_then(|a| here.iter().position(|x| *x == a)).map(|i| i + 1).unwrap_or(0);
                            self.active = Some(here[i % here.len()]);
                            self.reveal.1 = true;
                        }
                    }
                    _ => {}
                },
                _ => {}
            }
        }
    }

    // ── drawing ──────────────────────────────────────────────────────────

    /// One source line as styled text, with the marks of the threads.
    fn line_job(&self, r: &Range<usize>, fence: &mut bool, width: f32) -> LayoutJob {
        let doc = &self.s.doc;
        let text = &doc[r.clone()];
        let t = text.trim_start();
        let mut job = LayoutJob::default();
        job.wrap.max_width = width;

        // base format of the line
        let is_fence = t.starts_with("```") || t.starts_with("~~~");
        let code_line = *fence || is_fence;
        if is_fence {
            *fence = !*fence;
        }
        let hashes = t.chars().take_while(|c| *c == '#').count();
        let heading = !code_line && (1..=6).contains(&hashes) && t[hashes..].starts_with(' ');
        let base = if code_line {
            TextFormat { font_id: mono(14.0), color: if is_fence { FAINT } else { CODE }, ..Default::default() }
        } else if heading {
            let (size, color) = match hashes {
                1 => (27.0, ACCENT),
                2 => (21.0, H2),
                _ => (17.5, HUMAN),
            };
            TextFormat { font_id: body_font(size), color, ..Default::default() }
        } else if t.starts_with('>') {
            TextFormat { font_id: body_font(15.5), color: DIM, italics: true, ..Default::default() }
        } else {
            TextFormat { font_id: body_font(15.5), color: TEXT, ..Default::default() }
        };

        // split the line where marks start and stop
        let here: Vec<&Mark> = self.marks.1.iter().filter(|m| m.range.start <= r.end && r.start <= m.range.end).collect();
        let mut cuts: Vec<usize> = vec![r.start, r.end];
        for m in &here {
            cuts.push(m.range.start.clamp(r.start, r.end));
            cuts.push(m.range.end.clamp(r.start, r.end));
        }
        cuts.sort();
        cuts.dedup();
        let inject = |job: &mut LayoutJob, at: usize| {
            for m in &here {
                if let Some((pos, new)) = &m.inject {
                    if *pos == at {
                        let on = self.active == Some(m.id);
                        let f = TextFormat {
                            font_id: base.font_id.clone(),
                            color: ADD,
                            background: if on { ADD_BG } else { Color32::TRANSPARENT },
                            ..Default::default()
                        };
                        job.append(new, 0.0, f);
                    }
                }
            }
        };
        let mut in_code = false;
        for w in cuts.windows(2) {
            let (a, b) = (w[0], w[1]);
            inject(&mut job, a);
            let mut f = base.clone();
            // a proposal wins over a comment on the same text
            let cover = |k: fn(Kind) -> bool| here.iter().find(|m| k(m.kind) && m.range.start <= a && b <= m.range.end && !m.range.is_empty());
            if let Some(m) = cover(|k| k != Kind::Comment) {
                f.color = DEL;
                f.strikethrough = Stroke::new(1.0, DEL);
                if self.active == Some(m.id) {
                    f.background = DEL_BG;
                }
            } else if let Some(m) = cover(|k| k == Kind::Comment) {
                f.underline = Stroke::new(1.5, m.color);
                if self.active == Some(m.id) {
                    f.background = NOTE_BG;
                }
            }
            // inline `code`
            for (i, part) in doc[a..b].split('`').enumerate() {
                if i > 0 {
                    in_code = !in_code;
                    job.append("`", 0.0, TextFormat { color: FAINT, ..f.clone() });
                }
                let mut pf = f.clone();
                if in_code && !code_line {
                    pf.font_id = mono(base.font_id.size * 0.92);
                    if pf.color == TEXT {
                        pf.color = CODE;
                    }
                }
                job.append(part, 0.0, pf);
            }
        }
        if cuts.len() == 1 {
            inject(&mut job, r.start);
        } else {
            inject(&mut job, r.end);
        }
        if job.text.is_empty() {
            job.append(" ", 0.0, base);
        }
        job
    }

    fn draw_doc(&mut self, ui: &mut egui::Ui) {
        let lines = self.lines();
        let target = self.target();
        let sel = self.sel.map(|s| (s.min(self.cursor), s.max(self.cursor)));
        let mut clicked = None;
        let mut fence = false;
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            ui.add_space(10.0);
            let width = (ui.available_width() - 96.0).min(860.0);
            for (i, r) in lines.iter().enumerate() {
                let job = self.line_job(r, &mut fence, width);
                let is_cur = i == self.cursor;
                let in_sel = sel.is_some_and(|(a, b)| (a..=b).contains(&i));
                let in_block = target.is_some_and(|(a, b)| (a..=b).contains(&i));
                let fill = if in_sel {
                    Color32::from_rgb(48, 58, 88)
                } else if is_cur {
                    CURSOR_BG
                } else if in_block {
                    BLOCK_BG
                } else {
                    Color32::TRANSPARENT
                };
                let resp = Frame::new()
                    .fill(fill)
                    .inner_margin(Margin::symmetric(0, 2))
                    .show(ui, |ui| {
                        ui.horizontal_top(|ui| {
                            ui.spacing_mut().item_spacing.x = 0.0;
                            let (num, _) = ui.allocate_exact_size(egui::vec2(52.0, 20.0), Sense::hover());
                            ui.painter().text(
                                num.right_top() + egui::vec2(-8.0, 3.0),
                                Align2::RIGHT_TOP,
                                (i + 1).to_string(),
                                mono(11.5),
                                if is_cur { TEXT } else { FAINT },
                            );
                            let (bar, _) = ui.allocate_exact_size(egui::vec2(22.0, 20.0), Sense::hover());
                            if is_cur {
                                ui.painter().rect_filled(
                                    egui::Rect::from_min_size(bar.left_top() + egui::vec2(0.0, -2.0), egui::vec2(3.0, 24.0)),
                                    0.0,
                                    ACCENT,
                                );
                            }
                            let ids = self.threads_on(i);
                            if let Some(t) = self.active.filter(|a| ids.contains(a)).or(ids.first().copied()).and_then(|id| self.thread(id)) {
                                ui.painter().text(bar.center_top() + egui::vec2(2.0, 2.0), Align2::CENTER_TOP, kind_icon(t.kind), mono(13.0), kind_color(t.kind));
                            }
                            ui.add(egui::Label::new(job).selectable(false));
                            ui.allocate_space(egui::vec2(ui.available_width(), 1.0));
                        });
                    })
                    .response;
                if is_cur && self.reveal.0 {
                    resp.scroll_to_me(Some(Align::Center));
                }
                if resp.interact(Sense::click()).clicked() {
                    clicked = Some(i);
                }
            }
            ui.add_space(40.0);
            if self.s.doc.trim().is_empty() {
                ui.label(RichText::new("        The document is empty. Press o to write the first block.").color(DIM));
            }
        });
        self.reveal.0 = false;
        if let Some(i) = clicked {
            self.move_to(i);
            self.reveal.0 = false;
        }
    }

    fn draw_threads(&mut self, ui: &mut egui::Ui) {
        let order = self.marks.2.clone();
        let mut clicked = None;
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.add_space(6.0);
            ui.label(RichText::new("THREADS").color(DIM).strong().size(12.0));
            ui.label(RichText::new(format!("{} open", order.len())).color(DIM).size(12.0));
        });
        ui.add_space(6.0);
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            if order.is_empty() {
                ui.label(RichText::new("  No open threads.\n\n  c  comment on the block\n  v  select lines first\n\n  An agent adds proposals with\n  `redline propose …`").color(DIM));
            }
            for (id, line) in &order {
                let Some(t) = self.thread(*id).cloned() else { continue };
                let on = self.active == Some(*id);
                let kc = kind_color(t.kind);
                let resp = Frame::new()
                    .fill(if on { CURSOR_BG } else { Color32::TRANSPARENT })
                    .inner_margin(Margin { left: 14, right: 10, top: 8, bottom: 8 })
                    .corner_radius(4.0)
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.spacing_mut().item_spacing.y = 3.0;
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(format!("#{} {} {}", t.id, kind_icon(t.kind), t.kind.name())).color(kc).strong());
                            ui.label(RichText::new(&t.author).color(if t.agent { AGENT } else { HUMAN }));
                            ui.label(RichText::new(ago(t.at)).color(DIM));
                            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| match line {
                                Some(l) => ui.label(RichText::new(format!("L{}", l + 1)).color(DIM)),
                                None => ui.label(RichText::new("anchor lost").color(DEL)),
                            });
                        });
                        let clip = |s: &str, n: usize| -> String {
                            let v: Vec<&str> = s.lines().collect();
                            if on || v.len() <= n { s.to_string() } else { format!("{} …", v[..n].join("\n")) }
                        };
                        match t.kind {
                            Kind::Comment => {
                                ui.label(RichText::new(format!("“{}”", clip(&t.quote, 2))).color(DIM).italics());
                            }
                            _ => {
                                if t.kind != Kind::Insert {
                                    ui.label(RichText::new(clip(&t.quote, 3)).color(DEL).strikethrough());
                                }
                                if t.kind != Kind::Delete {
                                    ui.label(RichText::new(clip(&t.new, 3)).color(ADD));
                                }
                            }
                        }
                        if !t.body.is_empty() {
                            ui.label(clip(&t.body, 4));
                        }
                        for r in &t.replies {
                            ui.horizontal_wrapped(|ui| {
                                ui.spacing_mut().item_spacing.x = 4.0;
                                ui.label(RichText::new("↳").color(DIM));
                                ui.label(RichText::new(format!("{}:", r.author)).color(if r.agent { AGENT } else { HUMAN }));
                                ui.label(&r.body);
                            });
                        }
                        if on {
                            let hint = if t.kind.is_proposal() { "a accept · x reject · m modify · r reply" } else { "r reply · R resolve" };
                            ui.label(RichText::new(hint).color(DIM).size(12.0));
                        }
                    })
                    .response;
                let bar = egui::Rect::from_min_max(resp.rect.left_top(), egui::pos2(resp.rect.left() + 3.0, resp.rect.bottom()));
                ui.painter().rect_filled(bar, 1.0, if on { kc } else { FAINT });
                if on && self.reveal.1 {
                    resp.scroll_to_me(None);
                }
                if resp.interact(Sense::click()).clicked() {
                    clicked = Some((*id, *line));
                }
                ui.add_space(6.0);
            }
        });
        self.reveal.1 = false;
        if let Some((id, line)) = clicked {
            self.active = Some(id);
            if let Some(l) = line {
                self.cursor = l;
                self.reveal.0 = true;
            }
        }
    }

    fn draw_history(&self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.add_space(12.0);
            for e in self.s.state.events.iter().rev() {
                let (head, detail) = e.describe(&self.s.state);
                ui.horizontal(|ui| {
                    ui.add_space(16.0);
                    ui.label(RichText::new(format!("{:>4} ago", ago(e.at))).color(DIM).font(mono(12.5)));
                    ui.label(RichText::new(&e.actor).color(if e.agent { AGENT } else { HUMAN }).strong());
                    ui.label(head);
                });
                for d in detail.iter().take(8) {
                    let c = match d.chars().next() {
                        Some('-') => DEL,
                        Some('+') => ADD,
                        _ => DIM,
                    };
                    ui.horizontal(|ui| {
                        ui.add_space(110.0);
                        ui.label(RichText::new(d).color(c).font(mono(12.5)));
                    });
                }
                if detail.len() > 8 {
                    ui.horizontal(|ui| {
                        ui.add_space(110.0);
                        ui.label(RichText::new(format!("… {} more lines", detail.len() - 8)).color(FAINT));
                    });
                }
                ui.add_space(6.0);
            }
        });
    }

    fn draw_dialog(&mut self, ui: &egui::Ui) {
        let Some(d) = &mut self.dialog else { return };
        if d.fresh {
            d.fresh = false;
            ui.request_repaint();
            return;
        }
        let code = matches!(d.purpose, Purpose::Edit(_) | Purpose::NewBlock { .. } | Purpose::Modify(_));
        let mut done: Option<bool> = None;
        let resp = egui::Modal::new(egui::Id::unique("redline-dialog")).show(ui.ctx(), |ui| {
            ui.set_width(if code { 760.0 } else { 560.0 });
            ui.label(RichText::new(&d.title).color(PALETTE[0]).strong().size(15.0));
            ui.add_space(6.0);
            let edit = egui::TextEdit::multiline(&mut d.text)
                .hint_text(d.hint)
                .font(if code { mono(14.0) } else { body_font(15.0) })
                .desired_width(f32::INFINITY)
                .desired_rows(if code { 12 } else { 4 });
            let r = egui::ScrollArea::vertical().max_height(480.0).show(ui, |ui| ui.add(edit)).inner;
            r.request_focus();
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if ui.button("Save").clicked() {
                    done = Some(true);
                }
                if ui.button("Cancel").clicked() {
                    done = Some(false);
                }
                ui.label(RichText::new("ctrl-enter saves · esc cancels").color(DIM).size(12.0));
            });
            if ui.input(|i| i.modifiers.command && i.key_pressed(Key::Enter)) {
                done = Some(true);
            }
        });
        if resp.should_close() && done.is_none() {
            done = Some(false);
        }
        if let Some(save) = done {
            let d = self.dialog.take().unwrap();
            if save {
                self.submit(d);
            }
        }
    }

    fn draw_help(&self, ui: &egui::Ui) {
        let keys: &[(&str, &str)] = &[
            ("", "MOVE"),
            ("j k  ↓ ↑", "line down / up"),
            ("{ }", "previous / next block"),
            ("gg  G", "top / bottom"),
            ("n N", "next / previous thread"),
            ("tab", "next thread on the same line"),
            ("", "WRITE"),
            ("e  enter", "edit the block, or the selected lines"),
            ("o O", "new block below / above"),
            ("v", "select lines"),
            ("dd", "delete the block (d on a selection)"),
            ("u", "undo"),
            ("", "REVIEW"),
            ("a", "accept the proposal"),
            ("m", "modify the proposal, then accept"),
            ("x", "reject the proposal, with a reason"),
            ("c", "comment on the block or selection"),
            ("r  R", "reply / resolve the comment"),
            ("", "OTHER"),
            ("H", "history of events"),
            ("?", "this help"),
        ];
        egui::Modal::new(egui::Id::unique("redline-help")).show(ui.ctx(), |ui| {
            ui.set_width(420.0);
            ui.label(RichText::new("redline keys").strong().size(16.0));
            for (k, d) in keys {
                if k.is_empty() {
                    ui.add_space(6.0);
                    ui.label(RichText::new(*d).color(ACCENT).strong().size(12.0));
                } else {
                    ui.horizontal(|ui| {
                        ui.add_sized([110.0, 18.0], egui::Label::new(RichText::new(*k).font(mono(13.0)).strong()));
                        ui.label(RichText::new(*d).color(DIM));
                    });
                }
            }
            ui.add_space(6.0);
            ui.label(RichText::new("any key closes").color(FAINT).size(12.0));
        });
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // live updates from the agent CLI and other front ends
        if self.last_poll.elapsed() > Duration::from_millis(300) && self.dialog.is_none() {
            self.last_poll = Instant::now();
            match self.s.poll() {
                Ok(true) => {
                    let fresh: Vec<String> = self
                        .s
                        .state
                        .threads
                        .iter()
                        .filter(|t| t.id > self.seen_max && t.agent)
                        .map(|t| format!("{} #{} from {}", t.kind.name(), t.id, t.author))
                        .collect();
                    self.seen_max = self.s.state.threads.iter().map(|t| t.id).max().unwrap_or(0).max(self.seen_max);
                    self.sync_active();
                    if !fresh.is_empty() {
                        self.say(format!("new: {}  ·  n to go there", fresh.join(", ")));
                    }
                }
                Ok(false) => {}
                Err(e) => self.fail(e),
            }
        }
        ui.request_repaint_after(Duration::from_millis(350));
        if matches!(&self.flash, Some((_, at, _)) if at.elapsed() > Duration::from_secs(6)) {
            self.flash = None;
        }
        self.refresh();
        self.keys(ui);
        self.refresh();

        let open = self.marks.2.len();
        let decided = self.s.state.threads.len() - open;
        let bar = Frame::new().fill(BAR).inner_margin(Margin::symmetric(12, 6));
        egui::Panel::top("top").frame(bar).show_separator_line(false).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("▌redline").color(ACCENT).strong().size(15.0));
                let name = self.s.store.doc.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                ui.label(RichText::new(name).strong());
                let dir = self.s.store.doc.parent().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
                ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                    ui.label(RichText::new(format!("{open} open · {decided} decided")).color(DIM));
                    ui.with_layout(egui::Layout::left_to_right(Align::Center), |ui| {
                        ui.add(egui::Label::new(RichText::new(dir).color(DIM)).truncate());
                    });
                });
            });
        });
        egui::Panel::bottom("status").frame(bar).show_separator_line(false).show(ui, |ui| {
            ui.horizontal(|ui| {
                let (mode, c) = if self.dialog.is_some() {
                    ("WRITE", PALETTE[0])
                } else if self.history {
                    ("HISTORY", AGENT)
                } else if self.sel.is_some() {
                    ("SELECT", HUMAN)
                } else {
                    ("NORMAL", ACCENT)
                };
                Frame::new().fill(c).inner_margin(Margin::symmetric(8, 1)).corner_radius(3.0).show(ui, |ui| {
                    ui.label(RichText::new(mode).color(Color32::from_rgb(18, 18, 24)).strong().size(12.0));
                });
                ui.label(RichText::new(format!("L{}/{}", self.cursor + 1, self.lines().len())).color(DIM));
                let active = self.active.and_then(|a| self.thread(a));
                match &self.flash {
                    Some((m, _, err)) => {
                        ui.label(RichText::new(m).color(if *err { DEL } else { TEXT }));
                    }
                    None => {
                        let hint = if self.history {
                            "H or esc: back"
                        } else if self.sel.is_some() {
                            "e edit lines · c comment · d delete · esc cancel"
                        } else if active.is_some_and(|t| t.kind.is_proposal()) {
                            "a accept · x reject · m modify · r reply · n next thread"
                        } else if active.is_some() {
                            "r reply · R resolve · n next thread"
                        } else {
                            "e edit block · o new block · c comment · v select · n next thread · ? help"
                        };
                        ui.label(RichText::new(hint).color(DIM));
                    }
                }
            });
        });
        let side = Frame::new().fill(BG).inner_margin(Margin::symmetric(6, 0));
        egui::Panel::right("threads").frame(side).default_size(400.0).size_range(300.0..=620.0).show(ui, |ui| {
            self.draw_threads(ui);
        });
        egui::CentralPanel::default().frame(Frame::new().fill(BG)).show(ui, |ui| {
            if self.history {
                self.draw_history(ui);
            } else {
                self.draw_doc(ui);
            }
        });
        if self.help {
            self.draw_help(ui);
        }
        self.draw_dialog(ui);
    }
}
