//! redline as a native window, for people who do not live in a terminal.
//!
//! It looks and works like a document editor: a page, a comment column, and
//! buttons. Click a paragraph to edit it, select words to comment on them,
//! and press Accept or Reject on a suggestion. There are no modes and no
//! key commands to learn. It shows the same state as the TUI and the Neovim
//! plugin: the fold of the event log.

use eframe::egui::{
    self, Align, Color32, CursorIcon, FontFamily, FontId, Frame, Key, Margin, RichText, Sense, Stroke, TextFormat,
    text::{CCursor, CCursorRange, LayoutJob},
};
use redline::anchor;
use redline::ops::Session;
use redline::store::{Kind, Thread, ago};
use std::ops::Range;
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
const CODE_BG: Color32 = Color32::from_rgb(240, 242, 245);
const EDIT_BG: Color32 = Color32::from_rgb(248, 250, 255);
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
    /// Proposed text, shown at this offset.
    inject: Option<(usize, String)>,
    tint: usize,
}

/// The paragraph that the person edits now.
struct Editing {
    /// The text that this edit replaces. Empty for a new paragraph.
    range: Range<usize>,
    /// For a new paragraph: the line that it goes after (`None` = top).
    new_after: Option<Option<usize>>,
    text: String,
    initial: String,
    /// Put the text cursor at this screen position (from the click).
    place: Option<egui::Pos2>,
    focus: bool,
    /// Selected bytes of `text`.
    sel: Option<Range<usize>>,
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
    Activity,
}

enum Act {
    Edit(Range<usize>, Option<egui::Pos2>),
    New(Option<usize>),
    Commit,
    Comment,
}

struct App {
    s: Session,
    /// (session version, marks, (thread, first line) in document order).
    marks: (u64, Vec<Mark>, Vec<(u64, Option<usize>)>),
    active: Option<u64>,
    editing: Option<Editing>,
    composer: Option<Composer>,
    reply: Option<(u64, String)>,
    reject: Option<(u64, String)>,
    tab: Tab,
    flash: Option<(String, Instant, bool)>,
    /// Scroll the page / the comment column to the selected thread once.
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
    let name = path.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(format!("{name} — redline"))
            .with_app_id("redline")
            .with_inner_size([1280.0, 880.0]),
        ..Default::default()
    };
    eframe::run_native(
        "redline",
        options,
        Box::new(move |cc| {
            cc.egui_ctx.set_theme(egui::Theme::Light);
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
            cc.egui_ctx.set_visuals(v);
            cc.egui_ctx.all_styles_mut(|st| {
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
            let seen_max = s.state.threads.iter().map(|t| t.id).max().unwrap_or(0);
            let mut app = App {
                s,
                marks: (0, vec![], vec![]),
                active: None,
                editing: None,
                composer: None,
                reply: None,
                reject: None,
                tab: Tab::Comments,
                flash: None,
                reveal: (false, false),
                last_poll: Instant::now(),
                seen_max,
            };
            app.refresh();
            Ok(Box::new(app))
        }),
    )
}

impl App {
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
        let lines = |m: &Range<usize>| anchor::line_of(doc, m.start)..=anchor::line_of(doc, m.end);
        for (t, r) in self.s.open_threads() {
            places.push((t.id, r.as_ref().map(|r| anchor::line_of(doc, r.start))));
            let Some(r) = r else { continue };
            match t.kind {
                Kind::Insert => {
                    let gap = if t.tight { "" } else { "\n" };
                    let text = if t.quote.is_empty() { format!("{}\n{gap}", t.new) } else { format!("\n{gap}{}", t.new) };
                    marks.push(Mark { id: t.id, kind: t.kind, range: r.end..r.end, inject: Some((r.end, text)), tint: 0 });
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
                    marks.push(Mark { id: t.id, kind: t.kind, range: r, inject: None, tint });
                }
                _ => {
                    let clash = marks.iter().any(|m| m.kind != Kind::Comment && m.range.start < r.end && r.start < m.range.end);
                    if !clash {
                        let inject = (t.kind == Kind::Edit).then(|| (r.end, t.new.clone()));
                        marks.push(Mark { id: t.id, kind: t.kind, range: r, inject, tint: 0 });
                    }
                }
            }
        }
        self.marks = (self.s.version, marks, places);
        if self.active.is_some_and(|a| !self.marks.2.iter().any(|p| p.0 == a)) {
            self.active = None;
        }
    }

    fn thread(&self, id: u64) -> Option<&Thread> {
        self.s.state.threads.iter().find(|t| t.id == id)
    }

    /// Save the paragraph that the person edits.
    fn commit(&mut self) {
        let Some(ed) = self.editing.take() else { return };
        let text = ed.text.trim_end().to_string();
        if text == ed.initial {
            return;
        }
        let doc = &self.s.doc;
        let r = match ed.new_after {
            Some(after) => {
                if text.trim().is_empty() {
                    return;
                }
                let (r, t) = anchor::block_insert(doc, after, &text);
                self.s.edit(r, &t, "new")
            }
            None if text.trim().is_empty() => {
                let (l0, l1) = (anchor::line_of(doc, ed.range.start), anchor::line_of(doc, ed.range.end));
                let (r, t) = anchor::delete_lines(doc, l0, l1);
                self.s.edit(r, &t, "delete")
            }
            None => self.s.edit(ed.range, &text, "edit"),
        };
        self.done(r, "Saved");
    }

    /// Start a comment on the selected words, or on the full paragraph.
    fn start_comment(&mut self) {
        let Some(ed) = &self.editing else {
            return self.say("Click in a paragraph first. Select words to comment on only those words.");
        };
        if ed.new_after.is_some() {
            return self.say("Finish the new paragraph first.");
        }
        let text = ed.text.trim_end();
        let sel = ed.sel.clone().filter(|s| !s.is_empty() && s.end <= text.len());
        let (a, b) = sel.map(|s| (s.start, s.end)).unwrap_or((0, text.len()));
        let quote = text[a..b].trim_matches('\n').to_string();
        if quote.trim().is_empty() {
            return self.say("There is no text to comment on.");
        }
        let start = ed.range.start + a + (text[a..b].len() - text[a..b].trim_start_matches('\n').len());
        self.commit();
        let (line, col) = anchor::pos(&self.s.doc, start);
        self.tab = Tab::Comments;
        self.composer = Some(Composer { quote, line, col, text: String::new(), focus: true });
    }

    fn act(&mut self, a: Act) {
        match a {
            Act::Commit => self.commit(),
            Act::Comment => self.start_comment(),
            Act::Edit(range, place) => {
                self.commit();
                // The commit can move the text: find the paragraph at the click again.
                let doc = &self.s.doc;
                let range = if range.end <= doc.len() && doc.is_char_boundary(range.start) && doc.is_char_boundary(range.end) {
                    range
                } else {
                    return;
                };
                let text = doc[range.clone()].to_string();
                let line = anchor::line_of(doc, range.start);
                let end = anchor::line_of(doc, range.end);
                if let Some(m) = self.marks.1.iter().find(|m| {
                    let l = anchor::line_of(doc, m.range.start);
                    (line..=end).contains(&l)
                }) {
                    self.active = Some(m.id);
                    self.reveal.1 = true;
                }
                self.editing = Some(Editing { range, new_after: None, initial: text.clone(), text, place, focus: true, sel: None });
            }
            Act::New(after) => {
                self.commit();
                let at = match after {
                    Some(l) => anchor::lines_range(&self.s.doc, l, l).end,
                    None => 0,
                };
                self.editing = Some(Editing {
                    range: at..at,
                    new_after: Some(after),
                    text: String::new(),
                    initial: String::new(),
                    place: None,
                    focus: true,
                    sel: None,
                });
            }
        }
    }

    // ── the page ─────────────────────────────────────────────────────────

    /// Add one source line to `job` as formatted text. Markdown marks are
    /// not shown: a heading is large text, a list item gets a bullet.
    fn add_line(&self, job: &mut LayoutJob, r: &Range<usize>, fence: &mut bool) {
        let doc = &self.s.doc;
        let text = &doc[r.clone()];
        let t = text.trim_start();
        let indent = text.len() - t.len();
        let is_fence = t.starts_with("```") || t.starts_with("~~~");
        let code_line = *fence || is_fence;
        if is_fence {
            *fence = !*fence;
        }
        let hashes = t.chars().take_while(|c| *c == '#').count();
        let heading = !code_line && (1..=6).contains(&hashes) && t[hashes..].starts_with(' ');
        let mut skip = r.start;
        let base = if code_line {
            TextFormat { font_id: mono(13.5), color: if is_fence { MUTED } else { INK }, background: CODE_BG, ..Default::default() }
        } else if heading {
            skip = r.start + indent + hashes + 1;
            let size = match hashes {
                1 => 30.0,
                2 => 22.0,
                _ => 18.0,
            };
            TextFormat { font_id: body(size), color: STRONG, ..Default::default() }
        } else if t.starts_with('>') {
            skip = r.start + indent + 1 + t[1..].starts_with(' ') as usize;
            TextFormat { font_id: body(16.0), color: MUTED, italics: true, ..Default::default() }
        } else {
            TextFormat { font_id: body(16.0), color: INK, line_height: Some(25.0), ..Default::default() }
        };
        if !code_line && !heading {
            let tb = t.as_bytes();
            if tb.len() >= 2 && matches!(tb[0], b'-' | b'*' | b'+') && tb[1] == b' ' {
                skip = r.start + indent + 2;
                let rest = &t[2..];
                // A task item keeps its box: the bullet goes away.
                let bullet = if rest.starts_with("[ ] ") || rest.starts_with("[x] ") || rest.starts_with("[X] ") { "" } else { "•  " };
                job.append(&format!("{}  {bullet}", &text[..indent]), 0.0, TextFormat { color: BRAND, ..base.clone() });
            }
        }

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
                            font_id: if new.contains('\n') { body(16.0) } else { base.font_id.clone() },
                            color: ADD,
                            background: if on { ADD_BG_ON } else { ADD_BG },
                            line_height: base.line_height,
                            ..Default::default()
                        };
                        // show list items of the new text with a bullet, as on the page
                        let shown: Vec<String> = new
                            .split('\n')
                            .map(|l| match l.strip_prefix("- ").or(l.strip_prefix("* ")) {
                                Some(rest) => format!("  •  {rest}"),
                                None => l.to_string(),
                            })
                            .collect();
                        job.append(&shown.join("\n"), 0.0, f);
                    }
                }
            }
        };
        let (mut in_code, mut strong) = (false, false);
        for w in cuts.windows(2) {
            let (a, b) = (w[0], w[1]);
            inject(job, a);
            let a = a.max(skip);
            if a >= b {
                continue;
            }
            let mut f = base.clone();
            // a suggestion wins over a comment on the same text
            let cover = |k: fn(Kind) -> bool| here.iter().find(|m| k(m.kind) && m.range.start <= a && b <= m.range.end && !m.range.is_empty());
            if let Some(m) = cover(|k| k != Kind::Comment) {
                f.color = DEL;
                f.strikethrough = Stroke::new(1.0, DEL);
                f.background = if self.active == Some(m.id) { DEL_BG_ON } else { DEL_BG };
            } else if let Some(m) = cover(|k| k == Kind::Comment) {
                let (normal, on) = TINTS[m.tint];
                f.background = if self.active == Some(m.id) { on } else { normal };
            }
            if code_line {
                job.append(&doc[a..b], 0.0, f);
                continue;
            }
            // `code` and **strong**, with the marks hidden
            for (i, part) in doc[a..b].split('`').enumerate() {
                if i > 0 {
                    in_code = !in_code;
                }
                if in_code {
                    let mut pf = f.clone();
                    pf.font_id = mono(base.font_id.size * 0.88);
                    if pf.background == Color32::TRANSPARENT {
                        pf.background = CODE_BG;
                    }
                    job.append(part, 0.0, pf);
                    continue;
                }
                for (j, piece) in part.split("**").enumerate() {
                    if j > 0 {
                        strong = !strong;
                    }
                    let mut pf = f.clone();
                    if strong && pf.color == INK {
                        pf.color = STRONG;
                        pf.underline = Stroke::new(0.0, STRONG);
                    }
                    job.append(piece, 0.0, pf);
                }
            }
        }
        inject(job, r.end);
    }

    fn block_job(&self, l0: usize, l1: usize, width: f32) -> LayoutJob {
        let mut job = LayoutJob::default();
        job.wrap.max_width = width;
        let mut fence = false;
        for l in l0..=l1 {
            let r = anchor::lines_range(&self.s.doc, l, l);
            self.add_line(&mut job, &r, &mut fence);
            if l < l1 {
                job.append("\n", 0.0, TextFormat { font_id: body(16.0), ..Default::default() });
            }
        }
        if job.text.is_empty() {
            job.append(" ", 0.0, TextFormat::default());
        }
        job
    }

    /// The text box for the paragraph that the person edits.
    fn editor(&mut self, ui: &mut egui::Ui, width: f32, acts: &mut Vec<Act>) {
        let Some(ed) = &mut self.editing else { return };
        let code = ed.text.trim_start().starts_with("```");
        let out = Frame::new()
            .fill(EDIT_BG)
            .stroke(Stroke::new(1.0, LINK))
            .corner_radius(6.0)
            .inner_margin(Margin::symmetric(10, 8))
            .outer_margin(Margin { left: -11, right: -11, top: -4, bottom: 0 })
            .show(ui, |ui| {
                egui::TextEdit::multiline(&mut ed.text)
                    .font(if code { mono(13.5) } else { body(16.0) })
                    .frame(Frame::NONE)
                    .desired_width(width)
                    .desired_rows(1)
                    .hint_text("Write here…")
                    .show(ui)
            })
            .inner;
        let mut out = out;
        if ed.focus {
            ed.focus = false;
            out.response.request_focus();
            if let Some(p) = ed.place.take() {
                let c = out.galley.cursor_from_pos(p - out.galley_pos);
                out.state.cursor.set_char_range(Some(CCursorRange::one(c)));
            } else {
                let end = CCursor::new(ed.text.chars().count());
                out.state.cursor.set_char_range(Some(CCursorRange::one(end)));
            }
            out.state.clone().store(ui.ctx(), out.response.id);
        }
        // Keep the selection when the box loses the focus: the person then
        // clicks a button that needs it.
        let cur = out.state.cursor.char_range().map(|c| {
            let r = c.as_sorted_char_range();
            let byte = |ci: usize| ed.text.char_indices().nth(ci).map(|(b, _)| b).unwrap_or(ed.text.len());
            byte(r.start.0)..byte(r.end.0)
        });
        if cur.as_ref().is_some_and(|c| !c.is_empty()) || out.response.has_focus() {
            ed.sel = cur;
        }
        let has_sel = ed.sel.as_ref().is_some_and(|s| !s.is_empty());
        let is_new = ed.new_after.is_some();
        let row = ui
            .horizontal(|ui| {
                if ui.add(egui::Button::new(RichText::new("Done").color(PAPER)).fill(LINK)).clicked() {
                    acts.push(Act::Commit);
                }
                if !is_new {
                    let label = if has_sel { "Comment on selection" } else { "Comment on paragraph" };
                    if ui.button(label).clicked() {
                        acts.push(Act::Comment);
                    }
                }
                ui.label(RichText::new("Changes save when you click outside the box.").color(MUTED).size(12.5));
            })
            .response;
        // A click outside the box and its buttons ends the edit.
        let over_row = ui.rect_contains_pointer(row.rect.expand(6.0));
        if out.response.lost_focus() && !over_row && acts.is_empty() {
            acts.push(Act::Commit);
        }
    }

    fn page(&mut self, ui: &mut egui::Ui) {
        let mut acts: Vec<Act> = vec![];
        let blocks = anchor::blocks(&self.s.doc);
        let reveal_line = self.reveal.0.then(|| self.active).flatten().and_then(|a| self.marks.2.iter().find(|p| p.0 == a).and_then(|p| p.1));
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
                      ui.vertical(|ui| {
                        let width = page_w - 112.0;
                        ui.set_width(width);
                        ui.spacing_mut().item_spacing.y = 0.0;
                        let new_at = self.editing.as_ref().and_then(|e| e.new_after);
                        if new_at == Some(None) {
                            self.editor(ui, width, &mut acts);
                            ui.add_space(14.0);
                        }
                        for (l0, l1) in &blocks {
                            let range = anchor::lines_range(&self.s.doc, *l0, *l1);
                            let editing_this = self.editing.as_ref().is_some_and(|e| e.new_after.is_none() && e.range == range);
                            if editing_this {
                                self.editor(ui, width, &mut acts);
                            } else {
                                let resp = ui
                                    .add(egui::Label::new(self.block_job(*l0, *l1, width)).selectable(false).sense(Sense::click()))
                                    .on_hover_cursor(CursorIcon::Text);
                                if resp.clicked() {
                                    acts.push(Act::Edit(range.clone(), resp.interact_pointer_pos()));
                                }
                                if reveal_line.is_some_and(|l| (*l0..=*l1).contains(&l)) {
                                    resp.scroll_to_me(Some(Align::Center));
                                }
                            }
                            if new_at == Some(Some(*l1)) {
                                ui.add_space(14.0);
                                self.editor(ui, width, &mut acts);
                            }
                            ui.add_space(14.0);
                        }
                        ui.add_space(6.0);
                        let last = blocks.last().map(|b| b.1);
                        if self.editing.is_none() {
                            let hint = if blocks.is_empty() { "Click here to start to write" } else { "+ Add a paragraph" };
                            let resp = ui
                                .add(egui::Label::new(RichText::new(hint).color(MUTED).size(15.0)).selectable(false).sense(Sense::click()))
                                .on_hover_cursor(CursorIcon::PointingHand);
                            if resp.clicked() {
                                acts.push(Act::New(last));
                            }
                        }
                      });
                    });
            });
            ui.add_space(48.0);
        });
        self.reveal.0 = false;
        // A comment starts before the commit: it needs the selection.
        acts.sort_by_key(|a| !matches!(a, Act::Comment));
        if let Some(a) = acts.into_iter().next() {
            self.act(a);
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
        let resp = Frame::new()
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
                            if ui.button("Reject").clicked() {
                                self.reject = Some((t.id, String::new()));
                                self.reply = None;
                            }
                        } else if ui.button("Resolve").clicked() {
                            let r = self.s.resolve(t.id);
                            self.done(r, "Comment resolved");
                        }
                        if ui.button("Reply").clicked() {
                            self.reply = Some((t.id, String::new()));
                            self.reject = None;
                        }
                    });
                }
            })
            .response;
        if on && self.reveal.1 {
            resp.scroll_to_me(None);
        }
        if resp.interact(Sense::click()).clicked() {
            self.active = Some(t.id);
            self.reveal.0 = true;
        }
    }

    fn column(&mut self, ui: &mut egui::Ui) {
        ui.add_space(14.0);
        let open = self.marks.2.len();
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.tab, Tab::Comments, format!("Comments and suggestions ({open})"));
            ui.selectable_value(&mut self.tab, Tab::Activity, "Activity");
        });
        ui.add_space(6.0);
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.set_width(ui.available_width() - 6.0);
            if self.tab == Tab::Activity {
                return self.activity(ui);
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

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // Live updates from the agent and from other front ends. Not during
        // an edit: the text under the edit must not move.
        if self.last_poll.elapsed() > Duration::from_millis(300) && self.editing.is_none() {
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
                let name = self.s.store.doc.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                ui.label(RichText::new(name).color(STRONG).size(17.0));
                ui.add_space(12.0);
                if ui.button("Undo").on_hover_text("Reverse the last change to the document (Ctrl+Z)").clicked() {
                    undo = true;
                }
                if ui.button("Comment").on_hover_text("Comment on the selected words, or on the paragraph (Ctrl+Alt+M)").clicked() || comment_key {
                    self.start_comment();
                }
                ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| match &self.flash {
                    Some((m, _, err)) => {
                        ui.label(RichText::new(m).color(if *err { DEL } else { ADD }));
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
    }
}
