//! redline as a native window, for people who do not live in a terminal.
//!
//! It looks and works like a document editor: a page, a comment column, and
//! buttons. The page is one text surface: click and type. The app saves the
//! text when the person stops. In the suggestion mode a change becomes a
//! suggestion. Select words to comment on them, and press Accept or Reject
//! on a suggestion. It shows the same state as the TUI and the Neovim
//! plugin: the fold of the event log.

use eframe::egui::{
    self, Align, Color32, CursorIcon, FontFamily, FontId, Frame, Key, Margin, RichText, Sense, Stroke, TextFormat,
    text::{CCursor, CCursorRange, LayoutJob},
    text_edit::TextEditState,
};
use redline::anchor;
use redline::ops::Session;
use redline::store::{Kind, Status, Thread, ago};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

// A light document look.
const DESK: Color32 = Color32::from_rgb(241, 243, 246);
const PAPER: Color32 = Color32::from_rgb(255, 255, 255);
const RULE: Color32 = Color32::from_rgb(222, 226, 232);
const INK: Color32 = Color32::from_rgb(36, 41, 48);
const STRONG: Color32 = Color32::from_rgb(12, 14, 18);
const MUTED: Color32 = Color32::from_rgb(106, 115, 128);
const BRAND: Color32 = Color32::from_rgb(207, 52, 48);
const LINK: Color32 = Color32::from_rgb(38, 99, 196);
const ADD: Color32 = Color32::from_rgb(22, 112, 52);
const ADD_BG: Color32 = Color32::from_rgb(220, 247, 227);
const ADD_BG_ON: Color32 = Color32::from_rgb(178, 235, 195);
const DEL: Color32 = Color32::from_rgb(190, 38, 46);
const DEL_BG: Color32 = Color32::from_rgb(255, 232, 230);
const DEL_BG_ON: Color32 = Color32::from_rgb(255, 205, 201);
const PALE: Color32 = Color32::from_rgb(176, 183, 194);
const CODE_BG: Color32 = Color32::from_rgb(240, 242, 245);
const CARD_ON: Color32 = Color32::from_rgb(255, 250, 235);
const AI: Color32 = Color32::from_rgb(120, 76, 196);
/// Highlight colors for comments: (normal, selected). Comments that share
/// lines get different colors.
const TINTS: [(Color32, Color32); 5] = [
    (Color32::from_rgb(255, 242, 189), Color32::from_rgb(255, 221, 110)),
    (Color32::from_rgb(214, 232, 255), Color32::from_rgb(170, 207, 255)),
    (Color32::from_rgb(234, 221, 255), Color32::from_rgb(209, 184, 255)),
    (Color32::from_rgb(207, 243, 239), Color32::from_rgb(160, 228, 220)),
    (Color32::from_rgb(255, 227, 205), Color32::from_rgb(255, 199, 153)),
];

fn body(size: f32) -> FontId {
    FontId::new(size, FontFamily::Proportional)
}
fn mono(size: f32) -> FontId {
    FontId::new(size, FontFamily::Monospace)
}

/// The look of one character of the page.
#[derive(Clone, Copy, PartialEq)]
struct St {
    mono: bool,
    size: f32,
    color: Color32,
    bg: Color32,
    italics: bool,
    strike: bool,
    under: Option<Color32>,
    height: f32,
}

impl St {
    fn format(&self) -> TextFormat {
        TextFormat {
            font_id: if self.mono { mono(self.size) } else { body(self.size) },
            color: self.color,
            background: self.bg,
            italics: self.italics,
            strikethrough: if self.strike { Stroke::new(1.0, self.color) } else { Stroke::NONE },
            underline: self.under.map(|c| Stroke::new(1.0, c)).unwrap_or(Stroke::NONE),
            line_height: (self.height > 0.0).then_some(self.height),
            ..Default::default()
        }
    }
}

/// What a range of the page shows.
#[derive(Clone, Copy, PartialEq)]
enum Paint {
    /// The text that a thread is about.
    Thread(Kind),
    /// The new text of a suggestion. It is on the page but not in the document.
    New,
}

/// One colored range of the page: (bytes, paint, tint, selected).
type Shown = (Range<usize>, Paint, usize, bool);

/// The text of the document as formatted text. Each character stays, so
/// the person can type in it: a Markdown mark shows pale, a heading is
/// large, and a thread colors its text.
fn style_doc(text: &str, shown: &[Shown]) -> LayoutJob {
    let mut out: Vec<(usize, St)> = Vec::with_capacity(text.len());
    let mut fence = false;
    let mut at = 0;
    for full in text.split_inclusive('\n') {
        let l = full.strip_suffix('\n').unwrap_or(full);
        let t = l.trim_start();
        let indent = l.len() - t.len();
        let is_fence = t.starts_with("```") || t.starts_with("~~~");
        let code_line = fence || is_fence;
        if is_fence {
            fence = !fence;
        }
        let plain = St { mono: false, size: 16.0, color: INK, bg: Color32::TRANSPARENT, italics: false, strike: false, under: None, height: 25.0 };
        let hashes = t.chars().take_while(|c| *c == '#').count();
        // `lead` bytes at the start of the line are a Markdown mark.
        let (base, lead, lead_color) = if code_line {
            (St { mono: true, size: 13.5, color: if is_fence { PALE } else { INK }, bg: CODE_BG, height: 21.0, ..plain }, 0, PALE)
        } else if t.starts_with('|') {
            let rule = t.chars().all(|c| matches!(c, '|' | '-' | ':' | ' '));
            (St { mono: true, size: 13.5, color: if rule { RULE } else { INK }, height: 22.0, ..plain }, 0, PALE)
        } else if (1..=6).contains(&hashes) && t[hashes..].starts_with(' ') {
            let size = match hashes {
                1 => 30.0,
                2 => 22.0,
                _ => 18.0,
            };
            (St { size, color: STRONG, height: size * 1.5, ..plain }, indent + hashes + 1, PALE)
        } else if t.starts_with('>') {
            (St { color: MUTED, italics: true, ..plain }, indent + 1, PALE)
        } else {
            let tb = t.as_bytes();
            let digits = t.chars().take_while(|c| c.is_ascii_digit()).count();
            if tb.len() >= 2 && matches!(tb[0], b'-' | b'*' | b'+') && tb[1] == b' ' {
                (plain, indent + 2, BRAND)
            } else if digits > 0 && t[digits..].starts_with(". ") {
                (plain, indent + digits + 2, BRAND)
            } else {
                (plain, 0, PALE)
            }
        };
        let cs: Vec<(usize, char)> = l.char_indices().collect();
        let mut st = vec![base; cs.len()];
        let pale = St { color: PALE, ..base };
        let (mut code, mut strong, mut em) = (false, false, false);
        let mut i = 0;
        while i < cs.len() {
            let (off, c) = cs[i];
            let next = cs.get(i + 1).map(|x| x.1);
            if off < lead {
                st[i] = St { color: lead_color, ..base };
            } else if code_line {
            } else if c == '`' {
                code = !code;
                st[i] = pale;
            } else if code {
                st[i] = St { mono: true, size: base.size * 0.88, bg: CODE_BG, ..base };
            } else if c == '*' && next == Some('*') {
                strong = !strong;
                st[i] = pale;
                st[i + 1] = pale;
                i += 1;
            } else if c == '*' && {
                // A `*` is a mark only when it touches a word: "2 * 3" stays as text.
                let before = i > 0 && !cs[i - 1].1.is_whitespace();
                let after = next.is_some_and(|n| !n.is_whitespace()) && cs[i + 1..].iter().any(|x| x.1 == '*');
                if em { before } else { after }
            } {
                em = !em;
                st[i] = pale;
            } else {
                // [text](address)
                let link = (c == '[')
                    .then(|| {
                        let close = cs[i + 1..].iter().position(|x| x.1 == ']' || x.1 == '[').map(|p| i + 1 + p)?;
                        (cs[close].1 == ']' && close > i + 1 && cs.get(close + 1).map(|x| x.1) == Some('(')).then_some(())?;
                        let end = cs[close + 2..].iter().position(|x| x.1 == ')').map(|p| close + 2 + p)?;
                        Some((close, end))
                    })
                    .flatten();
                let mut cur = base;
                if strong {
                    cur.color = STRONG;
                    cur.under = Some(Color32::from_rgb(200, 205, 212));
                }
                cur.italics |= em;
                if let Some((close, end)) = link {
                    st[i] = pale;
                    for s in &mut st[i + 1..close] {
                        *s = St { color: LINK, under: Some(LINK), ..cur };
                    }
                    for s in &mut st[close..=end] {
                        *s = St { size: base.size * 0.8, ..pale };
                    }
                    i = end;
                } else {
                    st[i] = cur;
                }
            }
            i += 1;
        }
        out.extend(cs.iter().zip(&st).map(|((off, _), s)| (at + off, *s)));
        if full.len() > l.len() {
            out.push((at + l.len(), St { bg: Color32::TRANSPARENT, under: None, strike: false, ..base }));
        }
        at += full.len();
    }
    // the threads: a suggestion wins over a comment on the same text
    for pass in [true, false] {
        for (r, kind, tint, on) in shown {
            if (*kind == Paint::Thread(Kind::Comment)) != pass || r.is_empty() {
                continue;
            }
            let a = out.partition_point(|x| x.0 < r.start);
            let b = out.partition_point(|x| x.0 < r.end);
            for (off, s) in &mut out[a..b] {
                if text.as_bytes()[*off] == b'\n' {
                    continue;
                }
                match kind {
                    Paint::Thread(Kind::Comment) => s.bg = if *on { TINTS[*tint].1 } else { TINTS[*tint].0 },
                    Paint::Thread(Kind::Insert) => s.under = Some(ADD),
                    Paint::New => {
                        s.color = ADD;
                        s.under = None;
                        s.bg = if *on { ADD_BG_ON } else { ADD_BG };
                    }
                    Paint::Thread(_) => {
                        s.color = DEL;
                        s.strike = true;
                        s.under = None;
                        s.bg = if *on { DEL_BG_ON } else { DEL_BG };
                    }
                }
            }
        }
    }
    let mut job = LayoutJob::default();
    let mut i = 0;
    while i < out.len() {
        let mut j = i + 1;
        while j < out.len() && out[j].1 == out[i].1 {
            j += 1;
        }
        let end = out.get(j).map(|x| x.0).unwrap_or(text.len());
        job.append(&text[out[i].0..end], 0.0, out[i].1.format());
        i = j;
    }
    job
}

/// The one change between two texts: (bytes of `old`, bytes of `new`).
fn diff(old: &str, new: &str) -> Option<(Range<usize>, Range<usize>)> {
    if old == new {
        return None;
    }
    let (ob, nb) = (old.as_bytes(), new.as_bytes());
    let mut p = ob.iter().zip(nb).take_while(|(a, b)| a == b).count();
    while !old.is_char_boundary(p) {
        p -= 1;
    }
    let max = ob.len().min(nb.len()) - p;
    let mut s = ob.iter().rev().zip(nb.iter().rev()).take(max).take_while(|(a, b)| a == b).count();
    while !old.is_char_boundary(ob.len() - s) {
        s -= 1;
    }
    Some((p..ob.len() - s, p..nb.len() - s))
}

fn byte_of(s: &str, chars: usize) -> usize {
    s.char_indices().nth(chars).map(|(b, _)| b).unwrap_or(s.len())
}

fn char_of(s: &str, byte: usize) -> usize {
    s[..byte.min(s.len())].chars().count()
}

fn kind_label(k: Kind) -> (&'static str, Color32) {
    match k {
        Kind::Comment => ("Comment", Color32::from_rgb(150, 110, 0)),
        Kind::Edit => ("Suggested edit", LINK),
        Kind::Insert => ("Suggested addition", ADD),
        Kind::Delete => ("Suggested deletion", DEL),
    }
}

/// One thread drawn in the text.
struct Mark {
    id: u64,
    kind: Kind,
    range: Range<usize>,
    tint: usize,
}

struct Composer {
    quote: String,
    line: usize,
    col: usize,
    text: String,
    focus: bool,
}

#[derive(PartialEq, Clone, Copy)]
enum Tab {
    Comments,
    Closed,
    Activity,
}

struct App {
    s: Session,
    /// (session version, marks, (thread, first line) in document order).
    marks: (u64, Vec<Mark>, Vec<(u64, Option<usize>)>),
    active: Option<u64>,
    /// The text on the page: the document plus the new text of each
    /// suggestion. The person types in it.
    view: String,
    /// `view` before the person typed.
    view0: String,
    /// The new text of each suggestion in `view0`: (bytes, thread).
    segs: Vec<(Range<usize>, u64)>,
    /// The document with the change that the person typed. It differs from
    /// the saved text for a moment.
    buf: String,
    /// Time of the last key.
    typed: Instant,
    /// The selected bytes of `buf`, or the place of the text cursor.
    sel: Range<usize>,
    /// Suggestion mode: a change becomes a suggestion.
    suggest: bool,
    /// Put the keyboard focus in the page.
    focus: bool,
    composer: Option<Composer>,
    reply: Option<(u64, String)>,
    reject: Option<(u64, String)>,
    /// Accept with changes: (thread, the changed text).
    modify: Option<(u64, String)>,
    tab: Tab,
    flash: Option<(String, Instant, bool)>,
    /// Scroll the page / the comment column to the selected thread once.
    reveal: (bool, bool),
    last_poll: Instant,
    seen_max: u64,
}

fn style(ctx: &egui::Context) {
    ctx.set_theme(egui::Theme::Light);
    let mut v = egui::Visuals::light();
    v.panel_fill = DESK;
    v.window_fill = PAPER;
    v.override_text_color = Some(INK);
    v.extreme_bg_color = PAPER;
    v.selection.bg_fill = Color32::from_rgb(188, 214, 255);
    v.selection.stroke = Stroke::new(1.0, LINK);
    v.widgets.inactive.corner_radius = 6.0.into();
    v.widgets.hovered.corner_radius = 6.0.into();
    v.widgets.active.corner_radius = 6.0.into();
    ctx.set_visuals(v);
    ctx.all_styles_mut(|st| {
        st.spacing.button_padding = egui::vec2(10.0, 5.0);
        st.spacing.item_spacing = egui::vec2(8.0, 6.0);
        for (style, font) in st.text_styles.iter_mut() {
            font.size = match style {
                egui::TextStyle::Heading => 22.0,
                egui::TextStyle::Small => 12.0,
                egui::TextStyle::Monospace => 13.5,
                _ => 14.5,
            };
        }
    });
}

/// Open a document. Makes the file if it does not exist.
fn open_doc(path: &Path) -> anyhow::Result<Session> {
    if !path.exists() {
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, "")?;
    }
    Session::open(path)
}

fn title(path: &Path) -> String {
    let name = path.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    format!("{name} — redline")
}

/// The documents that redline knows and that still exist, newest first.
fn tracked_docs() -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> =
        redline::store::tracked().into_iter().map(|(p, _)| PathBuf::from(p)).filter(|p| p.is_file()).collect();
    v.sort_by_key(|p| std::cmp::Reverse(p.metadata().and_then(|m| m.modified()).ok()));
    v
}

/// The file for a new document with this name: in the Documents folder.
fn new_path(name: &str) -> PathBuf {
    let home = PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".into()));
    let dir = Some(home.join("Documents")).filter(|d| d.is_dir()).unwrap_or(home);
    let name: String = name.trim().chars().map(|c| if c == '/' || c == '\\' { '-' } else { c }).collect();
    dir.join(if name.ends_with(".md") { name } else { format!("{name}.md") })
}

fn main() -> eframe::Result {
    let first = std::env::args().nth(1).map(|p| match open_doc(Path::new(&p)) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("redline-app: {e:#}");
            std::process::exit(1);
        }
    });
    let name = first.as_ref().map(|s| title(&s.store.doc)).unwrap_or_else(|| "redline".into());
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_title(name).with_app_id("redline").with_inner_size([1280.0, 880.0]),
        ..Default::default()
    };
    eframe::run_native(
        "redline",
        options,
        Box::new(move |cc| {
            style(&cc.egui_ctx);
            Ok(Box::new(Root { app: first.map(App::new), docs: tracked_docs(), name: String::new(), error: None }))
        }),
    )
}

enum Go {
    Open(PathBuf),
    Home,
}

/// The window: the start page, or one document.
struct Root {
    app: Option<App>,
    docs: Vec<PathBuf>,
    /// Name of a new document.
    name: String,
    error: Option<String>,
}

impl Root {
    fn start(&mut self, ui: &mut egui::Ui, go: &mut Option<Go>) {
        egui::CentralPanel::default().frame(Frame::new().fill(DESK)).show(ui, |ui| {
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                let w = (ui.available_width() - 32.0).min(620.0);
                let pad = ((ui.available_width() - w) / 2.0).max(16.0);
                ui.add_space(56.0);
                ui.horizontal(|ui| {
                    ui.add_space(pad);
                    ui.vertical(|ui| {
                        ui.set_width(w);
                        ui.label(RichText::new("redline").color(BRAND).strong().size(30.0));
                        ui.label(RichText::new("Review the suggestions of an AI on your documents. You decide on each one.").color(MUTED).size(15.0));
                        ui.add_space(22.0);
                        if let Some(e) = &self.error {
                            ui.label(RichText::new(e).color(DEL));
                            ui.add_space(8.0);
                        }
                        ui.label(RichText::new("Your documents").color(STRONG).size(17.0));
                        ui.add_space(6.0);
                        if self.docs.is_empty() {
                            ui.label(RichText::new("There are no documents yet.").color(MUTED));
                        }
                        for p in &self.docs {
                            let resp = Frame::new()
                                .fill(PAPER)
                                .stroke(Stroke::new(1.0, RULE))
                                .corner_radius(8.0)
                                .inner_margin(Margin::same(12))
                                .show(ui, |ui| {
                                    ui.set_width(ui.available_width());
                                    let name = p.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                                    ui.label(RichText::new(name).color(STRONG).size(15.5));
                                    ui.label(RichText::new(p.display().to_string()).color(MUTED).size(12.0));
                                })
                                .response;
                            if resp.interact(Sense::click()).on_hover_cursor(CursorIcon::PointingHand).clicked() {
                                *go = Some(Go::Open(p.clone()));
                            }
                            ui.add_space(8.0);
                        }
                        ui.add_space(18.0);
                        ui.label(RichText::new("New document").color(STRONG).size(17.0));
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            let r = ui.add(egui::TextEdit::singleline(&mut self.name).hint_text("Name, for example: Checkout spec").desired_width(320.0));
                            let enter = r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                            let click = ui.add(egui::Button::new(RichText::new("Create").color(PAPER)).fill(LINK)).clicked();
                            if (click || enter) && !self.name.trim().is_empty() {
                                *go = Some(Go::Open(new_path(&self.name)));
                                self.name.clear();
                            }
                        });
                        ui.add_space(4.0);
                        ui.label(RichText::new("The app saves it as a Markdown file in your Documents folder.\nTo open a file that is not in the list, drop it on this window.").color(MUTED).size(12.5));
                    });
                });
            });
        });
    }
}

impl eframe::App for Root {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let mut go = ui.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).find(|p| !p.as_os_str().is_empty())).map(Go::Open);
        match &mut self.app {
            Some(app) => {
                if app.frame(ui) {
                    go = Some(Go::Home);
                }
            }
            None => self.start(ui, &mut go),
        }
        match go {
            Some(Go::Home) => {
                if let Some(mut app) = self.app.take() {
                    app.commit();
                }
                self.docs = tracked_docs();
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::Title("redline".into()));
            }
            Some(Go::Open(p)) => {
                if let Some(app) = &mut self.app {
                    app.commit();
                }
                match open_doc(&p) {
                    Ok(s) => {
                        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Title(title(&s.store.doc)));
                        self.app = Some(App::new(s));
                        self.error = None;
                    }
                    Err(e) => match &mut self.app {
                        Some(app) => app.fail(e),
                        None => self.error = Some(format!("{e:#}")),
                    },
                }
            }
            None => {}
        }
    }
}

impl App {
    fn new(s: Session) -> Self {
        let seen_max = s.state.threads.iter().map(|t| t.id).max().unwrap_or(0);
        let mut app = App {
            s,
            marks: (0, vec![], vec![]),
            active: None,
            view: String::new(),
            view0: String::new(),
            segs: vec![],
            buf: String::new(),
            typed: Instant::now(),
            sel: 0..0,
            suggest: false,
            focus: true,
            composer: None,
            reply: None,
            reject: None,
            modify: None,
            tab: Tab::Comments,
            flash: None,
            reveal: (false, false),
            last_poll: Instant::now(),
            seen_max,
        };
        app.refresh();
        app
    }

    // ── state ────────────────────────────────────────────────────────────

    fn say(&mut self, m: impl Into<String>) {
        self.flash = Some((m.into(), Instant::now(), false));
    }
    fn fail(&mut self, e: anyhow::Error) {
        self.flash = Some((format!("{e:#}"), Instant::now(), true));
    }
    fn done(&mut self, r: anyhow::Result<()>, ok: &str) {
        match r {
            Ok(()) => {
                self.refresh();
                self.say(ok);
            }
            Err(e) => self.fail(e),
        }
    }

    /// Find the marks again after the state or the text changed.
    fn refresh(&mut self) {
        if self.marks.0 == self.s.version {
            return;
        }
        let doc = &self.s.doc;
        let mut marks: Vec<Mark> = vec![];
        let mut places = vec![];
        // new text of the suggestions: (place in the document, text, thread)
        let mut adds: Vec<(usize, String, u64)> = vec![];
        let lines = |m: &Range<usize>| anchor::line_of(doc, m.start)..=anchor::line_of(doc, m.end);
        for (t, r) in self.s.open_threads() {
            places.push((t.id, r.as_ref().map(|r| anchor::line_of(doc, r.start))));
            let Some(r) = r else { continue };
            match t.kind {
                Kind::Insert => {
                    let gap = if t.tight { "" } else { "\n" };
                    let text = if t.quote.is_empty() { format!("{}\n{gap}", t.new) } else { format!("\n{gap}{}", t.new) };
                    adds.push((r.end, text, t.id));
                    marks.push(Mark { id: t.id, kind: t.kind, range: r, tint: 0 });
                }
                Kind::Comment => {
                    let mine = lines(&r);
                    let used: Vec<usize> = marks
                        .iter()
                        .filter(|m| m.kind == Kind::Comment)
                        .filter(|m| {
                            let o = lines(&m.range);
                            o.start() <= mine.end() && mine.start() <= o.end()
                        })
                        .map(|m| m.tint)
                        .collect();
                    let tint = (0..TINTS.len()).find(|c| !used.contains(c)).unwrap_or(marks.len() % TINTS.len());
                    marks.push(Mark { id: t.id, kind: t.kind, range: r, tint });
                }
                _ => {
                    if t.kind == Kind::Edit && !t.new.is_empty() {
                        adds.push((r.end, t.new.clone(), t.id));
                    }
                    marks.push(Mark { id: t.id, kind: t.kind, range: r, tint: 0 });
                }
            }
        }
        adds.sort_by_key(|a| a.0);
        let (mut view, mut segs, mut from) = (String::new(), vec![], 0);
        for (at, text, id) in adds {
            view.push_str(&doc[from..at]);
            segs.push((view.len()..view.len() + text.len(), id));
            view.push_str(&text);
            from = at;
        }
        view.push_str(&doc[from..]);
        self.view0 = view.clone();
        self.view = view;
        self.segs = segs;
        self.marks = (self.s.version, marks, places);
        self.buf = self.s.doc.clone();
        if self.active.is_some_and(|a| !self.marks.2.iter().any(|p| p.0 == a)) {
            self.active = None;
        }
    }

    fn thread(&self, id: u64) -> Option<&Thread> {
        self.s.state.threads.iter().find(|t| t.id == id)
    }

    /// Save the text that the person typed. In the suggestion mode the
    /// change becomes a suggestion and the text goes back.
    fn commit(&mut self) {
        let Some((old, new)) = diff(&self.s.doc, &self.buf) else { return };
        if !self.suggest {
            let text = self.buf[new].to_string();
            if let Err(e) = self.s.edit(old, &text, "edit") {
                // The file changed in a different program: show that text.
                let _ = self.s.poll();
                self.marks.0 = 0;
                self.refresh();
                self.fail(e);
            } else {
                self.refresh();
            }
            return;
        }
        let doc = self.s.doc.clone();
        if doc.trim().is_empty() {
            let r = self.s.propose(Kind::Insert, String::new(), 0, 0, self.buf.trim().to_string()).map(|_| ());
            return self.done(r, "Suggestion added");
        }
        // A suggestion is about full words: grow the change to word borders.
        let word = |c: char| c.is_alphanumeric() || c == '_' || c == '\'';
        let (mut a, mut b) = (old.start, old.end);
        let (mut na, mut nb) = (new.start, new.end);
        let grow_left = |a: &mut usize, na: &mut usize, f: &dyn Fn(char) -> bool| {
            while let Some(c) = doc[..*a].chars().next_back().filter(|c| f(*c)) {
                *a -= c.len_utf8();
                *na -= c.len_utf8();
            }
        };
        let grow_right = |b: &mut usize, nb: &mut usize, f: &dyn Fn(char) -> bool| {
            while let Some(c) = doc[*b..].chars().next().filter(|c| f(*c)) {
                *b += c.len_utf8();
                *nb += c.len_utf8();
            }
        };
        grow_left(&mut a, &mut na, &word);
        grow_right(&mut b, &mut nb, &word);
        if doc[a..b].trim().is_empty() {
            // New text between words: attach it to the word before it, or after it.
            grow_left(&mut a, &mut na, &|c| !word(c) && c != '\n');
            grow_left(&mut a, &mut na, &word);
            if doc[a..b].trim().is_empty() {
                grow_left(&mut a, &mut na, &|c| !word(c));
                grow_left(&mut a, &mut na, &word);
            }
            if doc[a..b].trim().is_empty() {
                grow_right(&mut b, &mut nb, &|c| !word(c));
                grow_right(&mut b, &mut nb, &word);
            }
        }
        let (quote, text) = (doc[a..b].to_string(), self.buf[na..nb].to_string());
        let (line, col) = anchor::pos(&doc, a);
        let kind = if text.trim().is_empty() { Kind::Delete } else { Kind::Edit };
        match self.s.propose(kind, quote, line, col, if kind == Kind::Delete { String::new() } else { text }) {
            Ok(id) => {
                self.seen_max = self.seen_max.max(id);
                self.refresh();
                self.active = Some(id);
                self.reveal.1 = true;
                self.tab = Tab::Comments;
                self.say("Suggestion added. The text does not change until a person accepts it.");
            }
            Err(e) => {
                self.buf = self.s.doc.clone();
                self.view = self.view0.clone();
                self.fail(e);
            }
        }
    }

    /// The place in the document of a place in `view0`.
    fn to_doc(&self, x: usize) -> usize {
        let mut out = x;
        for (r, _) in &self.segs {
            if r.end <= x {
                out -= r.len();
            } else if r.start < x {
                out -= x - r.start;
            }
        }
        out
    }

    /// The place in `view0` of a place in the document. `end`: the place
    /// is the end of a range, so new text at that place comes after it.
    fn to_view(&self, x: usize, end: bool) -> usize {
        let mut out = x;
        for (r, _) in &self.segs {
            let at = self.to_doc(r.start);
            if at < x || (at == x && !end) {
                out += r.len();
            }
        }
        out
    }

    /// Start a comment on the selected words, or on the paragraph at the
    /// text cursor.
    fn start_comment(&mut self) {
        self.commit();
        let doc = &self.s.doc;
        let ok = |i: usize| i <= doc.len() && doc.is_char_boundary(i);
        let (mut a, mut b) = (self.to_doc(self.sel.start.min(self.view0.len())), self.to_doc(self.sel.end.min(self.view0.len())));
        if !ok(a) || !ok(b) {
            (a, b) = (doc.len(), doc.len());
        }
        if a == b {
            let line = anchor::line_of(doc, a);
            match anchor::blocks(doc).into_iter().find(|(l0, l1)| (*l0..=*l1).contains(&line)) {
                Some((l0, l1)) => {
                    let r = anchor::lines_range(doc, l0, l1);
                    (a, b) = (r.start, r.end);
                }
                None => return self.say("Click in a paragraph first. Select words to comment on only those words."),
            }
        }
        let raw = &doc[a..b];
        let quote = raw.trim().to_string();
        if quote.is_empty() {
            return self.say("There is no text to comment on.");
        }
        let start = a + (raw.len() - raw.trim_start().len());
        let (line, col) = anchor::pos(doc, start);
        self.tab = Tab::Comments;
        self.composer = Some(Composer { quote, line, col, text: String::new(), focus: true });
    }

    // ── the page ─────────────────────────────────────────────────────────

    /// The threads at their place in the text on the page. The place moves
    /// with the text that the person typed and that is not saved yet.
    fn shown(&self) -> Vec<(u64, Shown)> {
        let mut base: Vec<(u64, Shown)> = self
            .marks
            .1
            .iter()
            .map(|m| {
                let r = self.to_view(m.range.start, false)..self.to_view(m.range.end, true);
                (m.id, (r, Paint::Thread(m.kind), m.tint, self.active == Some(m.id)))
            })
            .collect();
        base.extend(self.segs.iter().map(|(r, id)| (*id, (r.clone(), Paint::New, 0, self.active == Some(*id)))));
        let change = diff(&self.view0, &self.view);
        base.into_iter()
            .filter_map(|(id, (r, paint, tint, on))| {
                let r = match &change {
                    None => r,
                    Some((old, _)) if r.end <= old.start => r,
                    Some((old, new)) if r.start >= old.end => {
                        let by = new.end as isize - old.end as isize;
                        (r.start as isize + by) as usize..(r.end as isize + by) as usize
                    }
                    _ => return None,
                };
                Some((id, (r, paint, tint, on)))
            })
            .collect()
    }

    fn page(&mut self, ui: &mut egui::Ui) {
        let shown = self.shown();
        let styles: Vec<Shown> = shown.iter().map(|s| s.1.clone()).collect();
        let reveal = self.reveal.0.then_some(self.active).flatten().and_then(|a| shown.iter().find(|s| s.0 == a)).map(|s| s.1.0.start);
        let mut changed = false;
        let mut lost_focus = false;
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            let page_w = (ui.available_width() - 64.0).clamp(320.0, 820.0);
            let pad = ((ui.available_width() - page_w) / 2.0).max(16.0);
            ui.add_space(28.0);
            ui.horizontal(|ui| {
                ui.add_space(pad);
                Frame::new()
                    .fill(PAPER)
                    .stroke(Stroke::new(1.0, RULE))
                    .corner_radius(8.0)
                    .inner_margin(Margin { left: 56, right: 56, top: 48, bottom: 56 })
                    .show(ui, |ui| {
                        let width = page_w - 112.0;
                        ui.set_width(width);
                        let mut layouter = |ui: &egui::Ui, text: &dyn egui::TextBuffer, wrap: f32| {
                            let mut job = style_doc(text.as_str(), &styles);
                            job.wrap.max_width = wrap;
                            ui.fonts_mut(|f| f.layout_job(job))
                        };
                        let id = egui::Id::unique("page");
                        let out = egui::TextEdit::multiline(&mut self.view)
                            .id(id)
                            .frame(Frame::NONE)
                            .desired_width(width)
                            .desired_rows(16)
                            .hint_text(RichText::new("Write here…").color(PALE).size(16.0))
                            .layouter(&mut layouter)
                            .show(ui);
                        changed = out.response.changed();
                        lost_focus = out.response.lost_focus();
                        if self.focus {
                            self.focus = false;
                            out.response.request_focus();
                        }
                        // the selection, and the thread at the text cursor
                        if let Some(c) = out.cursor_range {
                            let r = c.as_sorted_char_range();
                            let sel = byte_of(&self.view, r.start.0)..byte_of(&self.view, r.end.0);
                            if sel != self.sel && sel.is_empty() && !changed {
                                let at = sel.start;
                                let hit = shown.iter().filter(|s| s.1.0.start <= at && at <= s.1.0.end).min_by_key(|s| s.1.0.len());
                                if let Some(h) = hit {
                                    if self.active != Some(h.0) {
                                        self.active = Some(h.0);
                                        self.reveal.1 = true;
                                        self.tab = Tab::Comments;
                                    }
                                }
                            }
                            self.sel = sel;
                        }
                        // a click on a card: go to its text
                        if let Some(at) = reveal {
                            let c = CCursor::new(char_of(&self.view, at));
                            let rect = out.galley.pos_from_cursor(c).translate(out.galley_pos.to_vec2());
                            ui.scroll_to_rect(rect.expand(60.0), Some(Align::Center));
                            if let Some(mut state) = TextEditState::load(ui.ctx(), id) {
                                state.cursor.set_char_range(Some(CCursorRange::one(c)));
                                state.store(ui.ctx(), id);
                            }
                            self.sel = at..at;
                        }
                    });
            });
            ui.add_space(48.0);
        });
        self.reveal.0 = false;
        if changed {
            self.typed = Instant::now();
        }
        // The change that the person typed, as a change of the document.
        match diff(&self.view0, &self.view) {
            None => {
                if self.buf != self.s.doc {
                    self.buf = self.s.doc.clone();
                }
            }
            Some((old, new)) => {
                let in_new_text = self.segs.iter().any(|(r, _)| {
                    if old.is_empty() { r.start < old.start && old.start < r.end } else { old.start < r.end && r.start < old.end }
                });
                if in_new_text {
                    self.view = self.view0.clone();
                    self.buf = self.s.doc.clone();
                    self.say("The green text is a suggestion. Press Change on its card to change it.");
                } else {
                    let (a, b) = (self.to_doc(old.start), self.to_doc(old.end));
                    self.buf = format!("{}{}{}", &self.s.doc[..a], &self.view[new], &self.s.doc[b..]);
                }
            }
        }
        // Save when the person stops. A suggestion waits longer: one burst
        // of keys is one suggestion.
        let wait = Duration::from_millis(if self.suggest { 1600 } else { 700 });
        if self.buf != self.s.doc && (lost_focus || self.typed.elapsed() > wait) {
            self.commit();
        }
    }

    // ── the comment column ───────────────────────────────────────────────

    fn person(ui: &mut egui::Ui, name: &str, agent: bool) {
        let c = if agent { AI } else { LINK };
        let (rect, _) = ui.allocate_exact_size(egui::vec2(24.0, 24.0), Sense::hover());
        ui.painter().circle_filled(rect.center(), 12.0, c);
        let initial = name.chars().next().unwrap_or('?').to_uppercase().to_string();
        ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, initial, body(12.5), PAPER);
        ui.label(RichText::new(name).color(STRONG).size(14.0));
        if agent {
            Frame::new().fill(Color32::from_rgb(238, 230, 255)).corner_radius(4.0).inner_margin(Margin::symmetric(5, 0)).show(ui, |ui| {
                ui.label(RichText::new("AI").color(AI).size(11.0));
            });
        }
    }

    fn card(&mut self, ui: &mut egui::Ui, t: &Thread, line: Option<usize>) {
        let on = self.active == Some(t.id);
        let (label, color) = kind_label(t.kind);
        // The card senses a click first, then its content: a button in the
        // card gets its click, and a click on the rest selects the card.
        let resp = ui.scope_builder(egui::UiBuilder::new().sense(Sense::click()), |ui| { Frame::new()
            .fill(if on { CARD_ON } else { PAPER })
            .stroke(Stroke::new(1.0, if on { Color32::from_rgb(232, 200, 110) } else { RULE }))
            .corner_radius(8.0)
            .inner_margin(Margin::same(12))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    Self::person(ui, &t.author, t.agent);
                    ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                        let when = ago(t.at);
                        ui.label(RichText::new(if when == "now" { when } else { format!("{when} ago") }).color(MUTED).size(12.0));
                    });
                });
                ui.label(RichText::new(label).color(color).size(12.0));
                let clip = |s: &str, n: usize| -> String {
                    let v: Vec<&str> = s.lines().collect();
                    if on || v.len() <= n { s.to_string() } else { format!("{} …", v[..n].join("\n")) }
                };
                match t.kind {
                    Kind::Comment => {
                        let q = clip(&t.quote, 2);
                        let q = if q.chars().count() > 140 && !on { format!("{} …", q.chars().take(140).collect::<String>()) } else { q };
                        ui.label(RichText::new(format!("“{q}”")).color(MUTED).italics().size(13.0));
                    }
                    _ => {
                        if t.kind != Kind::Insert {
                            ui.label(RichText::new(clip(&t.quote, 3)).color(DEL).strikethrough().background_color(DEL_BG));
                        }
                        if t.kind != Kind::Delete {
                            ui.label(RichText::new(clip(&t.new, 3)).color(ADD).background_color(ADD_BG));
                        }
                    }
                }
                if !t.body.is_empty() {
                    ui.label(RichText::new(clip(&t.body, 5)).color(INK));
                }
                if line.is_none() {
                    ui.label(RichText::new("The text of this thread is no longer in the document.").color(DEL).size(12.5));
                }
                for r in &t.replies {
                    ui.add_space(2.0);
                    ui.separator();
                    ui.horizontal(|ui| Self::person(ui, &r.author, r.agent));
                    ui.label(&r.body);
                }
                ui.add_space(4.0);

                // one small form at a time: reply, or reject with a reason
                let replying = self.reply.as_ref().is_some_and(|r| r.0 == t.id);
                let rejecting = self.reject.as_ref().is_some_and(|r| r.0 == t.id);
                if replying {
                    let text = &mut self.reply.as_mut().unwrap().1;
                    let r = ui.add(egui::TextEdit::multiline(text).desired_rows(2).desired_width(f32::INFINITY).hint_text("Reply…"));
                    if !r.has_focus() && text.is_empty() {
                        r.request_focus();
                    }
                    let send = ui.input(|i| i.modifiers.command && i.key_pressed(Key::Enter));
                    ui.horizontal(|ui| {
                        if ui.add(egui::Button::new(RichText::new("Send").color(PAPER)).fill(LINK)).clicked() || send {
                            let (id, body) = self.reply.take().unwrap();
                            if !body.trim().is_empty() {
                                let r = self.s.reply(id, body.trim().to_string());
                                self.done(r, "Reply sent");
                            }
                        } else if ui.button("Cancel").clicked() {
                            self.reply = None;
                        }
                    });
                } else if rejecting {
                    let text = &mut self.reject.as_mut().unwrap().1;
                    let r = ui.add(
                        egui::TextEdit::multiline(text)
                            .desired_rows(2)
                            .desired_width(f32::INFINITY)
                            .hint_text("Why? (optional — the AI reads this and learns your preference)"),
                    );
                    if !r.has_focus() && text.is_empty() {
                        r.request_focus();
                    }
                    ui.horizontal(|ui| {
                        if ui.add(egui::Button::new(RichText::new("Reject suggestion").color(PAPER)).fill(DEL)).clicked() {
                            let (id, why) = self.reject.take().unwrap();
                            let why = why.trim().to_string();
                            let r = self.s.reject(id, (!why.is_empty()).then_some(why));
                            self.done(r, "Suggestion rejected");
                        } else if ui.button("Cancel").clicked() {
                            self.reject = None;
                        }
                    });
                } else if self.modify.as_ref().is_some_and(|m| m.0 == t.id) {
                    let text = &mut self.modify.as_mut().unwrap().1;
                    ui.label(RichText::new("Change the suggested text. Then accept it.").color(MUTED).size(12.5));
                    ui.add(egui::TextEdit::multiline(text).desired_rows(3).desired_width(f32::INFINITY));
                    ui.horizontal(|ui| {
                        if ui.add(egui::Button::new(RichText::new("Accept with changes").color(PAPER)).fill(ADD)).clicked() {
                            let (id, new) = self.modify.take().unwrap();
                            self.commit();
                            match self.s.accept(id, Some(new)) {
                                Ok(_) => {
                                    self.refresh();
                                    self.say("Suggestion accepted with your changes. Undo reverses it.");
                                }
                                Err(e) => self.fail(e),
                            }
                        } else if ui.button("Cancel").clicked() {
                            self.modify = None;
                        }
                    });
                } else {
                    ui.horizontal(|ui| {
                        if t.kind.is_proposal() {
                            if ui.add(egui::Button::new(RichText::new("Accept").color(PAPER)).fill(ADD)).clicked() {
                                self.commit();
                                match self.s.accept(t.id, None) {
                                    Ok(_) => {
                                        self.refresh();
                                        self.say("Suggestion accepted. Undo reverses it.");
                                    }
                                    Err(e) => self.fail(e),
                                }
                            }
                            if t.kind != Kind::Delete && ui.button("Change").on_hover_text("Change the suggested text, then accept it").clicked() {
                                self.modify = Some((t.id, t.new.clone()));
                                (self.reply, self.reject) = (None, None);
                            }
                            if ui.button("Reject").clicked() {
                                self.reject = Some((t.id, String::new()));
                                (self.reply, self.modify) = (None, None);
                            }
                        } else if ui.button("Resolve").clicked() {
                            let r = self.s.resolve(t.id);
                            self.done(r, "Comment resolved");
                        }
                        if ui.button("Reply").clicked() {
                            self.reply = Some((t.id, String::new()));
                            (self.reject, self.modify) = (None, None);
                        }
                    });
                }
            }); })
            .response;
        if on && self.reveal.1 {
            resp.scroll_to_me(None);
        }
        if resp.clicked() {
            self.active = Some(t.id);
            self.reveal.0 = true;
        }
    }

    /// A thread that is not open: what the decision was, and who made it.
    fn closed_card(&mut self, ui: &mut egui::Ui, t: &Thread) {
        let (label, color) = kind_label(t.kind);
        Frame::new().fill(PAPER).stroke(Stroke::new(1.0, RULE)).corner_radius(8.0).inner_margin(Margin::same(12)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| Self::person(ui, &t.author, t.agent));
            ui.label(RichText::new(label).color(color).size(12.0));
            let clip = |s: &str| -> String {
                let v: Vec<&str> = s.lines().collect();
                if v.len() <= 3 { s.to_string() } else { format!("{} …", v[..3].join("\n")) }
            };
            match t.kind {
                Kind::Comment => {
                    ui.label(RichText::new(format!("“{}”", clip(&t.quote))).color(MUTED).italics().size(13.0));
                }
                _ => {
                    if t.kind != Kind::Insert {
                        ui.label(RichText::new(clip(&t.quote)).color(DEL).strikethrough().background_color(DEL_BG));
                    }
                    if t.kind != Kind::Delete {
                        ui.label(RichText::new(clip(&t.new)).color(ADD).background_color(ADD_BG));
                    }
                }
            }
            if !t.body.is_empty() {
                ui.label(RichText::new(clip(&t.body)).color(INK));
            }
            for r in &t.replies {
                ui.separator();
                ui.horizontal(|ui| Self::person(ui, &r.author, r.agent));
                ui.label(&r.body);
            }
            ui.add_space(4.0);
            let (word, c) = match t.status {
                Status::Accepted => ("Accepted", ADD),
                Status::Rejected => ("Rejected", DEL),
                _ => ("Resolved", MUTED),
            };
            let by = t.decided_by.as_deref().map(|b| format!(" by {b}")).unwrap_or_default();
            let when = t.decided_at.map(ago).map(|w| if w == "now" { ", now".to_string() } else { format!(", {w} ago") }).unwrap_or_default();
            ui.label(RichText::new(format!("{word}{by}{when}")).color(c).size(12.5));
            if let Some(n) = t.note.as_ref().filter(|n| !n.is_empty()) {
                ui.label(RichText::new(format!("Reason: {n}")).color(INK).size(13.0));
            }
            // An accepted suggestion is in the text: Undo reverses it.
            if t.status != Status::Accepted && ui.button("Open again").clicked() {
                let r = self.s.reopen(t.id);
                self.done(r, "Thread opened again");
                self.tab = Tab::Comments;
                self.active = Some(t.id);
                self.reveal = (true, true);
            }
        });
    }

    fn column(&mut self, ui: &mut egui::Ui) {
        ui.add_space(14.0);
        let open = self.marks.2.len();
        ui.horizontal(|ui| {
            let closed = self.s.closed_threads().len();
            ui.selectable_value(&mut self.tab, Tab::Comments, format!("To review ({open})"));
            ui.selectable_value(&mut self.tab, Tab::Closed, format!("Closed ({closed})"));
            ui.selectable_value(&mut self.tab, Tab::Activity, "Activity");
        });
        ui.add_space(6.0);
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.set_width(ui.available_width() - 6.0);
            if self.tab == Tab::Activity {
                return self.activity(ui);
            }
            if self.tab == Tab::Closed {
                let closed: Vec<Thread> = self.s.closed_threads().into_iter().cloned().collect();
                if closed.is_empty() {
                    ui.add_space(18.0);
                    ui.label(RichText::new("No decisions yet.").color(STRONG));
                    ui.label(RichText::new("Accepted, rejected and resolved items show here.").color(MUTED));
                }
                for t in &closed {
                    self.closed_card(ui, t);
                    ui.add_space(8.0);
                }
                return;
            }
            if let Some(c) = &mut self.composer {
                let mut done: Option<bool> = None;
                Frame::new().fill(PAPER).stroke(Stroke::new(1.5, LINK)).corner_radius(8.0).inner_margin(Margin::same(12)).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(RichText::new("New comment").color(STRONG).size(14.0));
                    let q: String = c.quote.chars().take(160).collect();
                    ui.label(RichText::new(format!("“{q}{}”", if c.quote.chars().count() > 160 { " …" } else { "" })).color(MUTED).italics().size(13.0));
                    let r = ui.add(egui::TextEdit::multiline(&mut c.text).desired_rows(3).desired_width(f32::INFINITY).hint_text("Write a comment…"));
                    if c.focus {
                        c.focus = false;
                        r.request_focus();
                    }
                    let send = ui.input(|i| i.modifiers.command && i.key_pressed(Key::Enter));
                    ui.horizontal(|ui| {
                        if ui.add(egui::Button::new(RichText::new("Comment").color(PAPER)).fill(LINK)).clicked() || send {
                            done = Some(true);
                        }
                        if ui.button("Cancel").clicked() {
                            done = Some(false);
                        }
                    });
                });
                ui.add_space(8.0);
                if let Some(save) = done {
                    let c = self.composer.take().unwrap();
                    if save && !c.text.trim().is_empty() {
                        match self.s.comment(c.quote, c.line, c.col, c.text.trim().to_string()) {
                            Ok(id) => {
                                self.seen_max = self.seen_max.max(id);
                                self.refresh();
                                self.active = Some(id);
                                self.reveal = (true, true);
                                self.say("Comment added");
                            }
                            Err(e) => self.fail(e),
                        }
                    }
                }
            }
            let order = self.marks.2.clone();
            if order.is_empty() && self.composer.is_none() {
                ui.add_space(18.0);
                ui.label(RichText::new("No open comments or suggestions.").color(STRONG));
                ui.label(RichText::new("To comment: click in a paragraph, select words if you want, then press “Comment”.\n\nSuggestions from the AI show here and in the text. You decide on each one.").color(MUTED));
            }
            for (id, line) in order {
                if let Some(t) = self.thread(id).cloned() {
                    self.card(ui, &t, line);
                    ui.add_space(8.0);
                }
            }
        });
        self.reveal.1 = false;
    }

    fn activity(&self, ui: &mut egui::Ui) {
        for e in self.s.state.events.iter().rev() {
            let (head, detail) = e.describe(&self.s.state);
            Frame::new().fill(PAPER).stroke(Stroke::new(1.0, RULE)).corner_radius(8.0).inner_margin(Margin::same(10)).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    Self::person(ui, &e.actor, e.agent);
                    ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                        let when = ago(e.at);
                        ui.label(RichText::new(if when == "now" { when } else { format!("{when} ago") }).color(MUTED).size(12.0));
                    });
                });
                ui.label(head);
                for d in detail.iter().take(6) {
                    let (c, bg) = match d.chars().next() {
                        Some('-') => (DEL, DEL_BG),
                        Some('+') => (ADD, ADD_BG),
                        _ => (MUTED, Color32::TRANSPARENT),
                    };
                    let text = d.get(2..).unwrap_or(d);
                    if !text.trim().is_empty() {
                        ui.label(RichText::new(text).color(c).background_color(bg).size(13.0));
                    }
                }
                if detail.len() > 6 {
                    ui.label(RichText::new(format!("… {} more lines", detail.len() - 6)).color(MUTED).size(12.0));
                }
            });
            ui.add_space(6.0);
        }
    }
}

impl App {
    /// Draw the document window. Returns true when the person asks for the
    /// list of documents.
    fn frame(&mut self, ui: &mut egui::Ui) -> bool {
        let mut home = false;
        // Live updates from the agent and from other front ends. Not while
        // typed text waits for its save: that text must not move.
        if self.last_poll.elapsed() > Duration::from_millis(300) && self.buf == self.s.doc {
            self.last_poll = Instant::now();
            match self.s.poll() {
                Ok(true) => {
                    let fresh: Vec<String> = self
                        .s
                        .state
                        .threads
                        .iter()
                        .filter(|t| t.id > self.seen_max && t.agent)
                        .map(|t| t.author.clone())
                        .collect();
                    self.seen_max = self.s.state.threads.iter().map(|t| t.id).max().unwrap_or(0).max(self.seen_max);
                    self.refresh();
                    if let Some(who) = fresh.first() {
                        let n = fresh.len();
                        self.say(format!("{n} new {} from {who}", if n == 1 { "suggestion" } else { "suggestions" }));
                    }
                }
                Ok(false) => {}
                Err(e) => self.fail(e),
            }
        }
        ui.request_repaint_after(Duration::from_millis(350));
        if matches!(&self.flash, Some((_, at, _)) if at.elapsed() > Duration::from_secs(5)) {
            self.flash = None;
        }
        self.refresh();

        // Ctrl+Z outside a text box reverses the last change to the document.
        let typing = ui.memory(|m| m.focused().is_some());
        let undo_key = !typing && ui.input(|i| i.modifiers.command && i.key_pressed(Key::Z));
        let comment_key = ui.input(|i| i.modifiers.command && i.modifiers.alt && i.key_pressed(Key::M));
        let mut undo = undo_key;

        let bar = Frame::new().fill(PAPER).inner_margin(Margin::symmetric(16, 10)).stroke(Stroke::new(1.0, RULE));
        egui::Panel::top("top").frame(bar).show_separator_line(false).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("redline").color(BRAND).strong().size(15.0));
                if ui.button("Documents").on_hover_text("Go to the list of documents").clicked() {
                    home = true;
                }
                let name = self.s.store.doc.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                ui.label(RichText::new(name).color(STRONG).size(17.0));
                ui.add_space(12.0);
                if ui.button("Undo").on_hover_text("Reverse the last change to the document (Ctrl+Z)").clicked() {
                    undo = true;
                }
                if ui.button("Comment").on_hover_text("Comment on the selected words, or on the paragraph (Ctrl+Alt+M)").clicked() || comment_key {
                    self.start_comment();
                }
                ui.add_space(12.0);
                let mut mode = self.suggest;
                ui.selectable_value(&mut mode, false, "Editing").on_hover_text("What you type changes the document");
                ui.selectable_value(&mut mode, true, "Suggesting").on_hover_text("What you type becomes a suggestion. The document changes when a person accepts it");
                if mode != self.suggest {
                    self.commit();
                    self.suggest = mode;
                    self.focus = true;
                }
                ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| match &self.flash {
                    Some((m, _, err)) => {
                        ui.label(RichText::new(m).color(if *err { DEL } else { ADD }));
                    }
                    None if self.buf != self.s.doc => {
                        ui.label(RichText::new(if self.suggest { "Suggesting…" } else { "Saving…" }).color(MUTED));
                    }
                    None => {
                        let open = self.marks.2.len();
                        let text = match open {
                            0 => "Nothing to review".to_string(),
                            1 => "1 item to review".to_string(),
                            n => format!("{n} items to review"),
                        };
                        ui.label(RichText::new(text).color(MUTED));
                    }
                });
            });
        });
        if undo {
            self.commit();
            match self.s.undo() {
                Ok(true) => {
                    self.refresh();
                    self.say("Change reversed");
                }
                Ok(false) => self.say("There is nothing to undo"),
                Err(e) => self.fail(e),
            }
        }
        let side = Frame::new().fill(DESK).inner_margin(Margin { left: 4, right: 12, top: 0, bottom: 0 });
        egui::Panel::right("threads").frame(side).show_separator_line(false).default_size(390.0).size_range(300.0..=560.0).show(ui, |ui| {
            self.column(ui);
        });
        egui::CentralPanel::default().frame(Frame::new().fill(DESK)).show(ui, |ui| {
            self.page(ui);
        });
        home
    }
}
