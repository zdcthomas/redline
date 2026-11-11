//! The TUI. The human reads the document, edits blocks in their own editor,
//! and decides on the proposals and comments that agents leave.

mod term;
mod theme;
mod ui;
mod view;

use crate::anchor;
use crate::cli::num;
use crate::config::{Config, Popup};
use crate::store::{Actor, DocStore, Kind, State, Status, Tx, What};
use anyhow::{Context, Result, bail};
use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{self, Event as CEvent, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant, SystemTime};

#[derive(PartialEq, Eq, Clone, Copy)]
enum Mode {
    Normal,
    Help,
    History,
    /// The thread list has the focus.
    Threads,
    /// One thread fills the document pane.
    Thread,
}

enum InputPurpose {
    Comment { quote: String, line: usize, col: usize },
    Reply(u64),
    Reject(u64),
}

struct Input {
    title: String,
    hint: &'static str,
    text: String,
    /// Cursor as a byte offset in `text`.
    cur: usize,
    purpose: InputPurpose,
}

enum Purpose {
    /// Replace a byte range of the document.
    Replace(Range<usize>),
    NewBlock { after: Option<usize> },
    Whole,
    /// Change the text of a proposal, then accept it.
    Modify(u64),
    /// Write the text of the open input box.
    InputBody,
}

struct Job {
    purpose: Purpose,
    initial: String,
    tmp: PathBuf,
    title: String,
}

struct Undo {
    doc: String,
    reopen: Option<u64>,
}

pub struct App {
    cfg: Config,
    store: DocStore,
    me: Actor,
    doc: String,
    state: State,
    /// Changes when the document or the threads change.
    version: u64,
    n_lines: usize,
    blocks: Vec<(usize, usize)>,
    ix: view::Index,
    cursor: usize,
    sel: Option<usize>,
    /// The word cursor: a byte offset in the cursor line. `None` = line mode.
    col: Option<usize>,
    /// Start of a word selection: (line, byte offset).
    csel: Option<(usize, usize)>,
    scroll: usize,
    active: Option<u64>,
    side_scroll: usize,
    show_side: bool,
    view: view::View,
    view_key: (u64, usize, Option<u64>, Option<Range<usize>>, Option<Range<usize>>),
    mode: Mode,
    pending: Option<char>,
    flash: Option<(String, Instant, bool)>,
    undo: Vec<Undo>,
    session: Option<(term::Term, Job)>,
    input: Option<Input>,
    detail_scroll: usize,
    hist_scroll: usize,
    last_poll: Instant,
    stamp: (Option<SystemTime>, u64),
    max_id: u64,
    page: usize,
    quit: bool,
}

/// The words of a line: runs of characters that are not whitespace.
fn words(text: &str) -> Vec<Range<usize>> {
    let mut out = vec![];
    let mut start = None;
    for (i, ch) in text.char_indices() {
        if ch.is_whitespace() {
            if let Some(s) = start.take() {
                out.push(s..i);
            }
        } else if start.is_none() {
            start = Some(i);
        }
    }
    if let Some(s) = start {
        out.push(s..text.len());
    }
    out
}

fn shquote(p: &Path) -> String {
    format!("'{}'", p.to_string_lossy().replace('\'', "'\\''"))
}

pub fn run(path: &Path) -> Result<()> {
    let cfg = Config::load();
    let store = DocStore::open(path)?;
    let (_, state, doc) = store.transact(|_| Ok(()))?;
    let me = Actor { name: cfg.author(), agent: false };
    let mut app = App {
        cfg,
        store,
        me,
        doc: String::new(),
        state: State::default(),
        version: 0,
        n_lines: 1,
        blocks: vec![],
        ix: Default::default(),
        cursor: 0,
        sel: None,
        col: None,
        csel: None,
        scroll: 0,
        active: None,
        side_scroll: 0,
        show_side: true,
        view: Default::default(),
        view_key: (u64::MAX, 0, None, None, None),
        mode: Mode::Normal,
        pending: None,
        flash: None,
        undo: vec![],
        session: None,
        input: None,
        detail_scroll: 0,
        hist_scroll: 0,
        last_poll: Instant::now(),
        stamp: (None, 0),
        max_id: 0,
        page: 20,
        quit: false,
    };
    app.max_id = state.threads.iter().map(|t| t.id).max().unwrap_or(0);
    app.adopt(state, doc);
    app.sync_active();
    let mut terminal = ratatui::init();
    let _ = ratatui::crossterm::execute!(std::io::stdout(), event::EnableBracketedPaste);
    let res = app.main_loop(&mut terminal);
    let _ = ratatui::crossterm::execute!(std::io::stdout(), event::DisableBracketedPaste);
    ratatui::restore();
    res
}

impl App {
    fn main_loop(&mut self, terminal: &mut DefaultTerminal) -> Result<()> {
        let mut dirty = true;
        while !self.quit {
            if let Some((t, _)) = &mut self.session {
                dirty |= t.dirty.swap(false, std::sync::atomic::Ordering::Relaxed);
                if let Some(ok) = t.exited() {
                    let (_, job) = self.session.take().unwrap();
                    self.finish_edit(job, ok);
                    dirty = true;
                }
            }
            if dirty {
                terminal.draw(|f| self.render(f))?;
                dirty = false;
            }
            let wait = if self.session.is_some() { 12 } else { 150 };
            if event::poll(Duration::from_millis(wait))? {
                match event::read()? {
                    CEvent::Key(k) if k.kind != KeyEventKind::Release => self.on_key(k, terminal)?,
                    CEvent::Paste(s) => self.on_paste(&s),
                    _ => {}
                }
                dirty = true;
            }
            if self.session.is_none() && self.last_poll.elapsed() > Duration::from_millis(250) {
                self.last_poll = Instant::now();
                dirty |= self.poll_disk();
                if matches!(&self.flash, Some((_, at, _)) if at.elapsed() > Duration::from_secs(5)) {
                    self.flash = None;
                    dirty = true;
                }
            }
        }
        Ok(())
    }

    // ── state ────────────────────────────────────────────────────────────

    fn say(&mut self, msg: impl Into<String>) {
        self.flash = Some((msg.into(), Instant::now(), false));
    }
    fn warn(&mut self, msg: impl Into<String>) {
        self.flash = Some((msg.into(), Instant::now(), true));
    }

    fn adopt(&mut self, state: State, doc: String) {
        self.doc = doc;
        self.state = state;
        self.version += 1;
        self.n_lines = anchor::line_starts(&self.doc).len();
        self.blocks = anchor::blocks(&self.doc);
        self.ix = view::index(&self.doc, &self.state.threads);
        self.cursor = self.cursor.min(self.n_lines - 1);
        if let Some(s) = self.sel {
            self.sel = Some(s.min(self.n_lines - 1));
        }
        self.csel = None;
        self.snap_col();
        if self.active.is_some_and(|a| !self.ix.order.contains(&a)) {
            self.active = None;
        }
        self.stamp = self.store.stamp();
    }

    /// Run a transaction and take over its result. `None` after an error.
    fn tx<T>(&mut self, f: impl FnOnce(&mut Tx, &Actor) -> Result<T>) -> Option<T> {
        let me = self.me.clone();
        match self.store.transact(|tx| f(tx, &me)) {
            Ok((out, state, doc)) => {
                self.adopt(state, doc);
                Some(out)
            }
            Err(e) => {
                self.warn(format!("{e:#}"));
                None
            }
        }
    }

    /// Reload when the agent CLI or another program changed the files.
    fn poll_disk(&mut self) -> bool {
        if self.store.stamp() == self.stamp {
            return false;
        }
        self.tx(|_, _| Ok(()));
        let fresh: Vec<String> = self
            .state
            .threads
            .iter()
            .filter(|t| t.id > self.max_id && t.agent)
            .map(|t| format!("{} #{} from {}", t.kind.name(), t.id, t.author))
            .collect();
        self.max_id = self.state.threads.iter().map(|t| t.id).max().unwrap_or(0).max(self.max_id);
        if !fresh.is_empty() {
            self.say(format!("new: {}  ·  n to go there", fresh.join(", ")));
        }
        self.sync_active();
        true
    }

    fn block_at(&self, line: usize) -> Option<(usize, usize)> {
        self.blocks.iter().copied().find(|(a, b)| (*a..=*b).contains(&line))
    }

    /// The lines an action applies to: the selection, or the block at the cursor.
    fn target(&self) -> Option<(usize, usize)> {
        match self.sel {
            Some(s) => Some((s.min(self.cursor), s.max(self.cursor))),
            None => self.block_at(self.cursor),
        }
    }

    fn sync_active(&mut self) {
        let here = self.ix.line_threads.get(&self.cursor);
        if !self.active.is_some_and(|a| here.is_some_and(|h| h.contains(&a))) {
            self.active = here.and_then(|h| h.first().copied());
        }
    }

    fn move_to(&mut self, line: usize) {
        self.cursor = line.min(self.n_lines - 1);
        self.snap_col();
        self.sync_active();
    }

    // ── word cursor ──────────────────────────────────────────────────────

    fn line_str(&self, line: usize) -> &str {
        &self.doc[anchor::lines_range(&self.doc, line, line)]
    }

    /// Keep the text cursor on a character of the cursor line.
    fn snap_col(&mut self) {
        if let Some(c) = self.col {
            let text = self.line_str(self.cursor);
            let mut c = c.min(text.len());
            while !text.is_char_boundary(c) {
                c -= 1;
            }
            if c == text.len() {
                c = text.char_indices().next_back().map(|(i, _)| i).unwrap_or(0);
            }
            self.col = Some(c);
        }
    }

    /// The character at (line, col), as a byte range of the document.
    fn char_at(&self, line: usize, col: usize) -> Range<usize> {
        let start = anchor::lines_range(&self.doc, line, line).start;
        let text = self.line_str(line);
        let col = col.min(text.len());
        let len = text[col..].chars().next().map(|c| c.len_utf8()).unwrap_or(0);
        start + col..start + col + len
    }

    /// The character below the text cursor, as a byte range of the document.
    pub(super) fn word_range(&self) -> Option<Range<usize>> {
        Some(self.char_at(self.cursor, self.col?))
    }

    /// The selected text: an exact range from the start of the selection to
    /// the text cursor, both included.
    pub(super) fn char_range(&self) -> Option<Range<usize>> {
        let (al, ac) = self.csel?;
        let cur = self.word_range()?;
        let a = self.char_at(al.min(self.n_lines - 1), ac);
        Some(a.start.min(cur.start)..a.end.max(cur.end))
    }

    /// Move the text cursor by a word. With a selection, a forward step goes
    /// to the end of a word, so `v` then `w` selects exactly one word.
    fn word_step(&mut self, fwd: bool) {
        let to_end = fwd && self.csel.is_some();
        let stop = |text: &str, w: &Range<usize>| -> usize {
            if to_end { text[..w.end].char_indices().next_back().map(|(i, _)| i).unwrap_or(w.start) } else { w.start }
        };
        let text = self.line_str(self.cursor);
        let ws = words(text);
        let Some(c) = self.col else {
            self.col = Some(ws.first().map(|w| w.start).unwrap_or(0));
            return;
        };
        let here = if fwd { ws.iter().find(|w| stop(text, w) > c) } else { ws.iter().rev().find(|w| w.start < c) };
        if let Some(w) = here {
            self.col = Some(stop(text, w));
            return;
        }
        // go to the next / previous line that has words
        let mut l = self.cursor;
        loop {
            l = if fwd { l + 1 } else { l.wrapping_sub(1) };
            if l >= self.n_lines {
                return;
            }
            let text = self.line_str(l);
            let ws = words(text);
            if let Some(w) = if fwd { ws.first() } else { ws.last() } {
                let col = stop(text, w);
                self.cursor = l;
                self.col = Some(col);
                self.sync_active();
                return;
            }
        }
    }

    fn char_step(&mut self, fwd: bool) {
        let text = self.line_str(self.cursor);
        let c = self.col.unwrap_or(0).min(text.len());
        self.col = Some(if fwd {
            text[c..].chars().next().map(|ch| c + ch.len_utf8()).filter(|n| *n < text.len()).unwrap_or(c)
        } else {
            text[..c].char_indices().next_back().map(|(i, _)| i).unwrap_or(0)
        });
    }

    fn step_thread(&mut self, fwd: bool) {
        let order = &self.ix.order;
        if order.is_empty() {
            return self.say("no open threads");
        }
        let line = |id: &u64| self.ix.thread_line.get(id).copied();
        let next = match self.active.and_then(|a| order.iter().position(|x| *x == a)) {
            Some(i) if fwd => order[(i + 1) % order.len()],
            Some(i) => order[(i + order.len() - 1) % order.len()],
            None if fwd => *order.iter().find(|id| line(id).is_some_and(|l| l >= self.cursor)).unwrap_or(&order[0]),
            None => *order
                .iter()
                .rev()
                .find(|id| line(id).is_some_and(|l| l <= self.cursor))
                .unwrap_or(order.last().unwrap()),
        };
        if let Some(l) = line(&next) {
            self.cursor = l;
        }
        self.sel = None;
        self.active = Some(next);
    }

    fn active_thread(&mut self) -> Option<crate::store::Thread> {
        let t = self.active.and_then(|a| self.state.threads.iter().find(|t| t.id == a).cloned());
        if t.is_none() {
            self.say("no thread here  ·  n goes to the next thread");
        }
        t
    }

    // ── document changes ─────────────────────────────────────────────────

    /// Apply an edit that was computed on the text in memory.
    fn edit(&mut self, range: Range<usize>, text: &str, via: &'static str) -> bool {
        let before = self.doc.clone();
        let ok = self
            .tx(|tx, me| {
                if tx.doc != before {
                    bail!("the file changed on disk during the edit; do the edit again");
                }
                tx.edit_doc(range, text, via, me);
                Ok(())
            })
            .is_some();
        if ok && self.doc != before {
            self.undo.push(Undo { doc: before, reopen: None });
        }
        ok
    }

    fn delete_target(&mut self) {
        let Some((l0, l1)) = self.target() else { return self.say("nothing to delete here") };
        let (range, text) = anchor::delete_lines(&self.doc, l0, l1);
        if self.edit(range, &text, "delete") {
            self.sel = None;
            self.move_to(l0);
            self.say(format!("deleted {} lines  ·  u to undo", l1 - l0 + 1));
        }
    }

    fn undo(&mut self) {
        let Some(u) = self.undo.pop() else { return self.say("nothing to undo") };
        let reopen = u.reopen;
        self.tx(|tx, me| {
            if let Some(id) = reopen {
                let t = tx.state.thread(id)?;
                if t.status == Status::Accepted {
                    let thread = t.eid.clone();
                    tx.log(me, What::Reopened { thread });
                }
            }
            tx.set_doc(u.doc, "undo", me);
            Ok(())
        });
        self.sync_active();
        self.say("undone");
    }

    fn accept(&mut self, id: u64, changed: Option<String>) {
        let before = self.doc.clone();
        let line = self.tx(|tx, me| {
            let mut t = tx.state.thread(id)?.clone();
            if t.status != Status::Open || !t.kind.is_proposal() {
                bail!("#{id} is not an open proposal");
            }
            let modified = changed.is_some();
            if let Some(n) = changed {
                t.new = n;
            }
            let (range, text) = t
                .apply(&tx.doc)
                .context("the text that this proposal points at is gone; reject it (x)")?;
            let line = anchor::line_of(&tx.doc, range.start);
            tx.log(me, What::Accepted { thread: t.eid.clone(), modified, new: t.new.clone(), note: None });
            tx.edit_doc(range, &text, "accept", me);
            Ok(line)
        });
        if let Some(line) = line {
            self.undo.push(Undo { doc: before, reopen: Some(id) });
            self.move_to(line);
            self.say(format!("accepted #{id}  ·  u to undo"));
        }
    }

    fn submit_input(&mut self, inp: Input) {
        let body = inp.text.trim().to_string();
        match inp.purpose {
            InputPurpose::Comment { quote, line, col } => {
                if body.is_empty() {
                    return self.say("empty comment: not added");
                }
                let id = self.tx(|tx, me| {
                    let eid = tx.log(me, What::Commented { line, col, quote, body });
                    Ok(num(&tx.state, &eid))
                });
                if let Some(id) = id {
                    self.max_id = self.max_id.max(id);
                    self.sel = None;
                    self.csel = None;
                    self.active = Some(id);
                    self.say(format!("comment #{id} added"));
                }
            }
            InputPurpose::Reply(id) => {
                if body.is_empty() {
                    return self.say("empty reply: not added");
                }
                self.tx(|tx, me| {
                    let thread = tx.state.thread(id)?.eid.clone();
                    tx.log(me, What::Replied { thread, body });
                    Ok(())
                });
            }
            InputPurpose::Reject(id) => {
                let note = (!body.is_empty()).then_some(body);
                let ok = self.tx(|tx, me| {
                    let thread = tx.state.thread(id)?.eid.clone();
                    tx.log(me, What::Rejected { thread, note });
                    Ok(())
                });
                if ok.is_some() {
                    self.sync_active();
                    self.say(format!("rejected #{id}"));
                }
            }
        }
    }

    fn resolve(&mut self, id: u64) {
        let ok = self.tx(|tx, me| {
            let thread = tx.state.thread(id)?.eid.clone();
            tx.log(me, What::Resolved { thread });
            Ok(())
        });
        if ok.is_some() {
            self.sync_active();
            self.say(format!("resolved #{id}"));
        }
    }

    // ── editor ───────────────────────────────────────────────────────────

    fn popup_rect(&self, area: ratatui::layout::Rect) -> ratatui::layout::Rect {
        let (pw, ph) = self.cfg.popup_pct();
        let w = (area.width as u32 * pw as u32 / 100).max(20).min(area.width as u32) as u16;
        let h = (area.height as u32 * ph as u32 / 100).max(8).min(area.height as u32) as u16;
        ratatui::layout::Rect::new(area.x + (area.width - w) / 2, area.y + (area.height - h) / 2, w, h)
    }

    fn start_edit(&mut self, terminal: &mut DefaultTerminal, purpose: Purpose, initial: String, title: String) -> Result<()> {
        let tmp = self.store.dir.join(format!("edit-{}.md", std::process::id()));
        let seed = if initial.is_empty() { String::new() } else { format!("{initial}\n") };
        std::fs::write(&tmp, seed)?;
        let editor = self.cfg.editor();
        let cmdline = format!("{editor} {}", shquote(&tmp));
        let cwd = self.store.doc.parent().unwrap_or(Path::new("/")).to_path_buf();
        let job = Job { purpose, initial, tmp, title };
        let in_tmux = std::env::var_os("TMUX").is_some();
        let mode = match self.cfg.popup {
            Popup::Auto if in_tmux => Popup::Tmux,
            Popup::Auto => Popup::Embedded,
            m => m,
        };
        match mode {
            Popup::Tmux => {
                // The popup writes the exit status of the editor to a file. No
                // file means that tmux did not show the popup.
                let (w, h) = self.cfg.popup_pct();
                let status = job.tmp.with_extension("status");
                let _ = std::fs::remove_file(&status);
                let wrapped = format!("{cmdline}; echo $? > {}", shquote(&status));
                let _ = Command::new("tmux")
                    .args(["display-popup", "-E", "-w", &format!("{w}%"), "-h", &format!("{h}%")])
                    .args(["-T", &format!(" redline · {} ", job.title), "-d"])
                    .arg(&cwd)
                    .arg(&wrapped)
                    .stderr(std::process::Stdio::null())
                    .status();
                terminal.clear()?;
                let code = std::fs::read_to_string(&status).ok();
                let _ = std::fs::remove_file(&status);
                match code {
                    Some(c) => self.finish_edit(job, c.trim() == "0"),
                    None => self.embed(terminal, &cmdline, &cwd, job)?,
                }
            }
            Popup::Fullscreen => {
                ratatui::restore();
                let st = Command::new("sh").arg("-c").arg(&cmdline).current_dir(&cwd).status();
                *terminal = ratatui::init();
                terminal.clear()?;
                self.finish_edit(job, st.map(|s| s.success()).unwrap_or(false));
            }
            _ => self.embed(terminal, &cmdline, &cwd, job)?,
        }
        Ok(())
    }

    /// Run the editor in a floating terminal inside the TUI.
    fn embed(&mut self, terminal: &mut DefaultTerminal, cmdline: &str, cwd: &Path, job: Job) -> Result<()> {
        let size = terminal.size()?;
        let r = self.popup_rect(ratatui::layout::Rect::new(0, 0, size.width, size.height));
        let t = term::Term::spawn(cmdline, cwd, r.height.saturating_sub(2), r.width.saturating_sub(2))
            .with_context(|| format!("cannot start `{cmdline}`"))?;
        self.session = Some((t, job));
        Ok(())
    }

    fn finish_edit(&mut self, job: Job, ok: bool) {
        let text = std::fs::read_to_string(&job.tmp).unwrap_or_default();
        let _ = std::fs::remove_file(&job.tmp);
        let text = text.trim_end().to_string();
        if !ok {
            return self.say("the editor exited with an error: no change");
        }
        match job.purpose {
            Purpose::InputBody => {
                if let Some(inp) = &mut self.input {
                    inp.cur = text.len();
                    inp.text = text;
                }
            }
            Purpose::Modify(id) => {
                if text == job.initial {
                    self.say("no change  ·  a accepts the proposal as it is");
                } else {
                    self.accept(id, Some(text));
                }
            }
            Purpose::Replace(range) => {
                if text == job.initial {
                    return self.say("no change");
                }
                let l0 = anchor::line_of(&self.doc, range.start);
                if text.trim().is_empty() {
                    let l1 = anchor::line_of(&self.doc, range.end);
                    let (r, t) = anchor::delete_lines(&self.doc, l0, l1);
                    self.edit(r, &t, "delete");
                } else if self.edit(range, &text, "edit") {
                    self.say("saved  ·  u to undo");
                }
                self.sel = None;
                self.move_to(l0);
            }
            Purpose::NewBlock { after } => {
                if text.trim().is_empty() {
                    return self.say("empty block: nothing added");
                }
                let (r, t) = anchor::block_insert(&self.doc, after, &text);
                let pos = r.start + t.len() - t.trim_start_matches('\n').len();
                if self.edit(r, &t, "new") {
                    let line = anchor::line_of(&self.doc, pos);
                    self.move_to(line);
                    self.say("block added  ·  u to undo");
                }
            }
            Purpose::Whole => {
                if text == job.initial {
                    return self.say("no change");
                }
                let len = self.doc.len();
                if self.edit(0..len, &format!("{text}\n"), "edit") {
                    self.say("saved  ·  u to undo");
                    self.move_to(self.cursor);
                }
            }
        }
    }

    // ── keys ─────────────────────────────────────────────────────────────

    fn on_paste(&mut self, s: &str) {
        if let Some((t, _)) = &mut self.session {
            t.send_paste(s);
        } else if let Some(inp) = &mut self.input {
            inp.text.insert_str(inp.cur, s);
            inp.cur += s.len();
        }
    }

    fn on_key(&mut self, k: KeyEvent, terminal: &mut DefaultTerminal) -> Result<()> {
        if let Some((t, _)) = &mut self.session {
            t.send_key(k);
            return Ok(());
        }
        if self.input.is_some() {
            return self.input_key(k, terminal);
        }
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        match self.mode {
            Mode::Help => self.mode = Mode::Normal,
            Mode::History => match k.code {
                KeyCode::Char('j') | KeyCode::Down => self.hist_scroll += 1,
                KeyCode::Char('k') | KeyCode::Up => self.hist_scroll = self.hist_scroll.saturating_sub(1),
                KeyCode::Char('d') if ctrl => self.hist_scroll += self.page / 2,
                KeyCode::Char('u') if ctrl => self.hist_scroll = self.hist_scroll.saturating_sub(self.page / 2),
                KeyCode::PageDown | KeyCode::Char(' ') => self.hist_scroll += self.page,
                KeyCode::PageUp => self.hist_scroll = self.hist_scroll.saturating_sub(self.page),
                KeyCode::Char('g') => self.hist_scroll = 0,
                KeyCode::Char('G') => self.hist_scroll = usize::MAX / 2,
                KeyCode::Char('q' | 'H') | KeyCode::Esc => self.mode = Mode::Normal,
                _ => {}
            },
            Mode::Threads => {
                match k.code {
                    KeyCode::Char('j' | 'n') | KeyCode::Down => self.step_thread(true),
                    KeyCode::Char('k' | 'N') | KeyCode::Up => self.step_thread(false),
                    KeyCode::Enter | KeyCode::Char('o' | 'l') if self.active.is_some() => {
                        self.detail_scroll = 0;
                        self.mode = Mode::Thread;
                    }
                    KeyCode::Esc | KeyCode::Char('q' | 't' | 'h') => self.mode = Mode::Normal,
                    KeyCode::Char('a' | 'x' | 'm' | 'r' | 'R' | 'u' | '?' | 'H') => self.normal_key(k, terminal)?,
                    _ => {}
                }
                if self.active.is_none() && !self.ix.order.is_empty() {
                    self.step_thread(true);
                }
            }
            Mode::Thread => {
                match k.code {
                    KeyCode::Char('j') | KeyCode::Down => self.detail_scroll += 1,
                    KeyCode::Char('k') | KeyCode::Up => self.detail_scroll = self.detail_scroll.saturating_sub(1),
                    KeyCode::Char('n') => self.step_thread(true),
                    KeyCode::Char('N') => self.step_thread(false),
                    KeyCode::Esc | KeyCode::Char('q' | 'h') => self.mode = Mode::Threads,
                    KeyCode::Char('a' | 'x' | 'm' | 'r' | 'R' | 'u') => self.normal_key(k, terminal)?,
                    _ => {}
                }
                if self.active.is_none() {
                    self.mode = Mode::Threads;
                    if !self.ix.order.is_empty() {
                        self.step_thread(true);
                    }
                }
            }
            Mode::Normal => self.normal_key(k, terminal)?,
        }
        Ok(())
    }

    fn open_input(&mut self, title: String, hint: &'static str, purpose: InputPurpose) {
        self.input = Some(Input { title, hint, text: String::new(), cur: 0, purpose });
    }

    fn normal_key(&mut self, k: KeyEvent, terminal: &mut DefaultTerminal) -> Result<()> {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let pending = self.pending.take();
        let last = self.n_lines - 1;
        // With a word selection, c / e / d act on exactly the selected words.
        if let (Some(range), KeyCode::Char(ch @ ('c' | 'e' | 'd')), false) = (self.char_range(), k.code, ctrl) {
            let text = self.doc[range.clone()].to_string();
            match ch {
                'c' => {
                    let (line, col) = anchor::pos(&self.doc, range.start);
                    let short: String = text.chars().take(28).collect();
                    let more = if text.chars().count() > 28 { "…" } else { "" };
                    self.open_input(format!("Comment on “{short}{more}”"), "", InputPurpose::Comment { quote: text, line, col });
                }
                'e' => self.start_edit(terminal, Purpose::Replace(range), text, "selected words".into())?,
                _ => {
                    if self.edit(range, "", "delete") {
                        self.say("deleted the selected words  ·  u to undo");
                    }
                }
            }
            return Ok(());
        }
        match k.code {
            KeyCode::Char('c') if ctrl => self.quit = true,
            KeyCode::Char('w') => self.word_step(true),
            KeyCode::Char('b') => self.word_step(false),
            KeyCode::Char('l') | KeyCode::Right => self.char_step(true),
            KeyCode::Char('h') | KeyCode::Left => self.char_step(false),
            KeyCode::Char('v') if self.col.is_some() => {
                self.sel = None;
                self.csel = if self.csel.is_some() { None } else { Some((self.cursor, self.col.unwrap_or(0))) };
            }
            KeyCode::Char('d') if ctrl => self.move_to(self.cursor + (self.page / 2).max(1)),
            KeyCode::Char('u') if ctrl => self.move_to(self.cursor.saturating_sub((self.page / 2).max(1))),
            KeyCode::PageDown => self.move_to(self.cursor + self.page.max(1)),
            KeyCode::PageUp => self.move_to(self.cursor.saturating_sub(self.page.max(1))),
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Char('j') | KeyCode::Down => self.move_to(self.cursor + 1),
            KeyCode::Char('k') | KeyCode::Up => self.move_to(self.cursor.saturating_sub(1)),
            KeyCode::Char('g') if pending == Some('g') => self.move_to(0),
            KeyCode::Char('g') => self.pending = Some('g'),
            KeyCode::Char('G') => self.move_to(last),
            KeyCode::Char('}') => {
                let next = self.blocks.iter().find(|b| b.0 > self.cursor).map(|b| b.0).unwrap_or(last);
                self.move_to(next);
            }
            KeyCode::Char('{') => {
                let prev = self.blocks.iter().rev().find(|b| b.0 < self.cursor).map(|b| b.0).unwrap_or(0);
                self.move_to(prev);
            }
            KeyCode::Char('n' | ']') => self.step_thread(true),
            KeyCode::Char('N' | '[') => self.step_thread(false),
            KeyCode::Tab => {
                if let Some(h) = self.ix.line_threads.get(&self.cursor) {
                    let i = self.active.and_then(|a| h.iter().position(|x| *x == a)).map(|i| i + 1).unwrap_or(0);
                    self.active = Some(h[i % h.len()]);
                }
            }
            KeyCode::Char('v' | 'V') => {
                self.csel = None;
                self.sel = if self.sel.is_some() { None } else { Some(self.cursor) };
            }
            KeyCode::Esc => {
                // first the selection, then the word cursor
                if self.sel.is_none() && self.csel.is_none() {
                    self.col = None;
                }
                self.sel = None;
                self.csel = None;
                self.flash = None;
            }
            KeyCode::Char('d') if self.sel.is_some() || pending == Some('d') => self.delete_target(),
            KeyCode::Char('d') => self.pending = Some('d'),
            KeyCode::Char('e') | KeyCode::Enter => match self.target() {
                Some((l0, l1)) => {
                    let range = anchor::lines_range(&self.doc, l0, l1);
                    let initial = self.doc[range.clone()].to_string();
                    let title = if l0 == l1 { format!("L{}", l0 + 1) } else { format!("L{}–{}", l0 + 1, l1 + 1) };
                    self.start_edit(terminal, Purpose::Replace(range), initial, title)?;
                }
                None => {
                    let after = self.cursor.checked_sub(1);
                    self.start_edit(terminal, Purpose::NewBlock { after }, String::new(), "new block".into())?;
                }
            },
            KeyCode::Char('E') => {
                let initial = self.doc.trim_end().to_string();
                self.start_edit(terminal, Purpose::Whole, initial, "whole document".into())?;
            }
            KeyCode::Char('o') => {
                let after = Some(self.target().map(|t| t.1).unwrap_or(self.cursor));
                self.start_edit(terminal, Purpose::NewBlock { after }, String::new(), "new block".into())?;
            }
            KeyCode::Char('O') => {
                let after = self.target().map(|t| t.0).unwrap_or(self.cursor).checked_sub(1);
                self.start_edit(terminal, Purpose::NewBlock { after }, String::new(), "new block".into())?;
            }
            KeyCode::Char('c') => match self.target() {
                Some((l0, l1)) => {
                    let quote = self.doc[anchor::lines_range(&self.doc, l0, l1)].to_string();
                    if quote.trim().is_empty() {
                        self.say("no text to comment on");
                    } else {
                        let title = if l0 == l1 { format!("Comment on L{}", l0 + 1) } else { format!("Comment on L{}–{}", l0 + 1, l1 + 1) };
                        self.open_input(title, "", InputPurpose::Comment { quote, line: l0, col: 0 });
                    }
                }
                None => self.say("no text to comment on"),
            },
            KeyCode::Char('r') => {
                if let Some(t) = self.active_thread() {
                    self.open_input(format!("Reply on #{}", t.id), "", InputPurpose::Reply(t.id));
                }
            }
            KeyCode::Char('R') => {
                if let Some(t) = self.active_thread() {
                    if t.kind == Kind::Comment {
                        self.resolve(t.id);
                    } else {
                        self.say("a proposal needs a decision: a accept, x reject");
                    }
                }
            }
            KeyCode::Char('a') => {
                if let Some(t) = self.active_thread() {
                    if t.kind.is_proposal() {
                        self.accept(t.id, None);
                    } else {
                        self.say("a comment has nothing to accept: R resolves it");
                    }
                }
            }
            KeyCode::Char('x') => {
                if let Some(t) = self.active_thread() {
                    if t.kind.is_proposal() {
                        self.open_input(
                            format!("Reject #{}", t.id),
                            "reason (optional): the agent reads it",
                            InputPurpose::Reject(t.id),
                        );
                    } else {
                        self.say("R resolves a comment");
                    }
                }
            }
            KeyCode::Char('m') => {
                if let Some(t) = self.active_thread() {
                    if matches!(t.kind, Kind::Edit | Kind::Insert) {
                        self.start_edit(terminal, Purpose::Modify(t.id), t.new.clone(), format!("change #{}, then accept", t.id))?;
                    } else {
                        self.say("only an edit or an insert has text to change");
                    }
                }
            }
            KeyCode::Char('u') => self.undo(),
            KeyCode::Char('H') => {
                self.hist_scroll = 0;
                self.mode = Mode::History;
            }
            KeyCode::Char('t') => {
                if self.active.is_none() {
                    self.step_thread(true);
                }
                self.mode = Mode::Threads;
            }
            KeyCode::Char('?') => self.mode = Mode::Help,
            KeyCode::Char('\\') => self.show_side = !self.show_side,
            _ => {}
        }
        Ok(())
    }

    fn input_key(&mut self, k: KeyEvent, terminal: &mut DefaultTerminal) -> Result<()> {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let alt = k.modifiers.contains(KeyModifiers::ALT);
        let inp = self.input.as_mut().unwrap();
        let prev = |s: &str, i: usize| s[..i].char_indices().next_back().map(|(j, _)| j).unwrap_or(0);
        match k.code {
            KeyCode::Esc => self.input = None,
            KeyCode::Char('c') if ctrl => self.input = None,
            KeyCode::Enter if alt => {
                inp.text.insert(inp.cur, '\n');
                inp.cur += 1;
            }
            KeyCode::Char('j') if ctrl => {
                inp.text.insert(inp.cur, '\n');
                inp.cur += 1;
            }
            KeyCode::Enter => {
                let inp = self.input.take().unwrap();
                self.submit_input(inp);
            }
            KeyCode::Char('e') if ctrl => {
                let (initial, title) = (inp.text.clone(), inp.title.clone());
                self.start_edit(terminal, Purpose::InputBody, initial, title)?;
            }
            KeyCode::Char('u') if ctrl => {
                inp.text.drain(..inp.cur);
                inp.cur = 0;
            }
            KeyCode::Char('w') if ctrl => {
                let head = inp.text[..inp.cur].trim_end();
                let start = head.rfind(char::is_whitespace).map(|i| i + 1).unwrap_or(0);
                inp.text.drain(start..inp.cur);
                inp.cur = start;
            }
            KeyCode::Char('a') if ctrl => inp.cur = 0,
            KeyCode::Home => inp.cur = 0,
            KeyCode::End => inp.cur = inp.text.len(),
            KeyCode::Left => inp.cur = prev(&inp.text, inp.cur),
            KeyCode::Right => {
                if let Some(c) = inp.text[inp.cur..].chars().next() {
                    inp.cur += c.len_utf8();
                }
            }
            KeyCode::Backspace => {
                let p = prev(&inp.text, inp.cur);
                inp.text.drain(p..inp.cur);
                inp.cur = p;
            }
            KeyCode::Delete => {
                if let Some(c) = inp.text[inp.cur..].chars().next() {
                    inp.text.drain(inp.cur..inp.cur + c.len_utf8());
                }
            }
            KeyCode::Char(c) if !ctrl => {
                inp.text.insert(inp.cur, c);
                inp.cur += c.len_utf8();
            }
            _ => {}
        }
        Ok(())
    }
}
