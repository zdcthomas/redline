//! The event log. Humans and agents push events; nothing else is stored.
//! Every tracked document gets a directory below `~/.redline/docs/` with one
//! append-only log per replica (`events/<replica>.jsonl`) and a snapshot of
//! the last text redline saw. The threads are the fold of the events.

use crate::anchor;
use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Write;
use std::ops::Range;
use std::path::{Path, PathBuf};

pub fn home() -> PathBuf {
    if let Some(h) = std::env::var_os("REDLINE_HOME") {
        return h.into();
    }
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| ".".into())
        .join(".redline")
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Comment,
    Edit,
    Insert,
    Delete,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::Comment => "comment",
            Kind::Edit => "edit",
            Kind::Insert => "insert",
            Kind::Delete => "delete",
        }
    }
    pub fn is_proposal(self) -> bool {
        self != Kind::Comment
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Open,
    Accepted,
    Rejected,
    Resolved,
}

impl Status {
    pub fn name(self) -> &'static str {
        match self {
            Status::Open => "open",
            Status::Accepted => "accepted",
            Status::Rejected => "rejected",
            Status::Resolved => "resolved",
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Reply {
    pub author: String,
    pub agent: bool,
    pub at: DateTime<Utc>,
    pub body: String,
}

/// A comment or a proposed change, with its replies. Threads are not stored:
/// `State::fold` builds them from the events.
#[derive(Serialize, Clone, Debug)]
pub struct Thread {
    /// Short number for people and commands (#3). It is the position of the
    /// thread in the event order.
    pub id: u64,
    /// Id of the event that made the thread. Events refer to a thread by it.
    pub eid: String,
    pub kind: Kind,
    pub status: Status,
    pub author: String,
    pub agent: bool,
    pub at: DateTime<Utc>,
    /// Anchor text. Edit/Delete: the text to replace. Insert: the line to
    /// insert after ("" = top). Comment: the text the comment is about.
    pub quote: String,
    /// 0-based line hint for the anchor.
    pub line: usize,
    /// 0-based byte column hint: it picks one of several matches on a line.
    pub col: usize,
    /// Edit/Insert: the proposed text.
    pub new: String,
    /// Insert: add the text on the next line, without blank lines around it.
    pub tight: bool,
    /// Comment body, or the reason for a proposal.
    pub body: String,
    pub replies: Vec<Reply>,
    pub decided_at: Option<DateTime<Utc>>,
    pub decided_by: Option<String>,
    pub note: Option<String>,
}

impl Thread {
    pub fn locate(&self, doc: &str) -> Option<Range<usize>> {
        anchor::resolve(doc, &self.quote, self.line, self.col, self.kind == Kind::Insert)
    }

    /// The document edit that an accept of this proposal makes.
    pub fn apply(&self, doc: &str) -> Option<(Range<usize>, String)> {
        let r = self.locate(doc)?;
        Some(match self.kind {
            Kind::Comment => return None,
            Kind::Edit => (r, self.new.clone()),
            Kind::Delete => {
                let b = doc.as_bytes();
                let whole = (r.start == 0 || b[r.start - 1] == b'\n')
                    && (r.end == doc.len() || b[r.end] == b'\n');
                if whole {
                    let l0 = anchor::line_of(doc, r.start);
                    let l1 = anchor::line_of(doc, r.end);
                    anchor::delete_lines(doc, l0, l1)
                } else {
                    (r, String::new())
                }
            }
            Kind::Insert => {
                let top = self.quote.is_empty();
                if self.tight {
                    if top {
                        (0..0, format!("{}\n", self.new))
                    } else {
                        (r.end..r.end, format!("\n{}", self.new))
                    }
                } else {
                    let after = (!top).then(|| anchor::line_of(doc, r.start));
                    anchor::block_insert(doc, after, &self.new)
                }
            }
        })
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum What {
    Tracked { path: String },
    /// The document text changed. `old` and `new` are whole lines.
    DocEdit { via: String, line: usize, old: String, new: String },
    Proposed {
        kind: Kind,
        line: usize,
        #[serde(default)]
        col: usize,
        quote: String,
        new: String,
        tight: bool,
        why: String,
    },
    Commented {
        line: usize,
        #[serde(default)]
        col: usize,
        quote: String,
        body: String,
    },
    Replied { thread: String, body: String },
    Accepted { thread: String, modified: bool, new: String, note: Option<String> },
    Rejected { thread: String, note: Option<String> },
    Resolved { thread: String },
    Reopened { thread: String },
}

/// One entry of the log. The log is a grow-only set: replicas only add
/// events, a merge is a set union, and every replica folds the events in the
/// same order (`id` = Lamport clock + replica), so all replicas get the same
/// threads.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Event {
    pub id: String,
    pub lamport: u64,
    pub replica: String,
    pub at: DateTime<Utc>,
    pub actor: String,
    pub agent: bool,
    /// Events that this event replaces. The fold ignores them. A run of
    /// edits collapses to one event this way, and a merge that brings an old
    /// event back does no harm.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub supersedes: Vec<String>,
    #[serde(flatten)]
    pub what: What,
}

impl Event {
    /// The thread that this event is about.
    pub fn thread(&self) -> Option<&str> {
        match &self.what {
            What::Proposed { .. } | What::Commented { .. } => Some(&self.id),
            What::Replied { thread, .. }
            | What::Accepted { thread, .. }
            | What::Rejected { thread, .. }
            | What::Resolved { thread }
            | What::Reopened { thread } => Some(thread),
            _ => None,
        }
    }

    /// One-line summary and detail lines. The detail lines start with
    /// "- " (removed text), "+ " (added text) or "  " (other text).
    pub fn describe(&self, st: &State) -> (String, Vec<String>) {
        fn diff(old: &str, new: &str) -> Vec<String> {
            let mut v: Vec<String> = old.lines().map(|l| format!("- {l}")).collect();
            v.extend(new.lines().map(|l| format!("+ {l}")));
            v
        }
        fn text(s: &str) -> Vec<String> {
            s.lines().map(|l| format!("  {l}")).collect()
        }
        let n = |eid: &str| st.num(eid);
        match &self.what {
            What::Tracked { path } => ("started to track the document".into(), text(path)),
            What::DocEdit { via, line, old, new } => {
                let how = match via.as_str() {
                    "file" => "changed the file",
                    "undo" => "undid a change",
                    "delete" => "deleted text",
                    "new" => "added a block",
                    "accept" => "applied an accepted proposal",
                    _ => "edited the document",
                };
                (format!("{how} at L{}", line + 1), diff(old, new))
            }
            What::Proposed { kind, line, quote, new, why, .. } => {
                let mut d = match kind {
                    Kind::Insert => diff("", new),
                    _ => diff(quote, new),
                };
                d.extend(text(why));
                (format!("proposed {} {} at L{}", kind.name(), n(&self.id), line + 1), d)
            }
            What::Commented { line, body, .. } => {
                (format!("commented {} on L{}", n(&self.id), line + 1), text(body))
            }
            What::Replied { thread, body } => (format!("replied on {}", n(thread)), text(body)),
            What::Accepted { thread, modified, new, note } => {
                let mut d = vec![];
                if *modified {
                    d.extend(diff("", new));
                }
                d.extend(text(note.as_deref().unwrap_or("")));
                let m = if *modified { " with changes" } else { "" };
                (format!("accepted {}{m}", n(thread)), d)
            }
            What::Rejected { thread, note } => {
                (format!("rejected {}", n(thread)), text(note.as_deref().unwrap_or("")))
            }
            What::Resolved { thread } => (format!("resolved {}", n(thread)), vec![]),
            What::Reopened { thread } => (format!("reopened {}", n(thread)), vec![]),
        }
    }
}

/// The current state: the fold of all events.
#[derive(Clone, Debug, Default)]
pub struct State {
    pub threads: Vec<Thread>,
    /// All events, in fold order.
    pub events: Vec<Event>,
    pub lamport: u64,
}

impl State {
    pub fn fold(mut events: Vec<Event>) -> State {
        let dead: std::collections::HashSet<String> =
            events.iter().flat_map(|e| e.supersedes.iter().cloned()).collect();
        events.retain(|e| !dead.contains(&e.id));
        events.sort_by(|a, b| a.id.cmp(&b.id));
        events.dedup_by(|a, b| a.id == b.id);
        let mut st = State::default();
        for e in events {
            st.apply(e);
        }
        st
    }

    pub fn thread(&self, id: u64) -> Result<&Thread> {
        self.threads.iter().find(|t| t.id == id).with_context(|| format!("no thread #{id}"))
    }

    /// "#3" for the thread that event `eid` made.
    pub fn num(&self, eid: &str) -> String {
        match self.threads.iter().find(|t| t.eid == eid) {
            Some(t) => format!("#{}", t.id),
            None => "#?".into(),
        }
    }

    pub fn apply(&mut self, e: Event) {
        self.lamport = self.lamport.max(e.lamport);
        let decided = |t: &mut Thread, s: Status, e: &Event, note: &Option<String>| {
            // The first decision in the event order wins.
            if t.status == Status::Open {
                t.status = s;
                t.decided_at = Some(e.at);
                t.decided_by = Some(e.actor.clone());
                t.note = note.clone();
            }
        };
        let make = |kind: Kind, line: usize, col: usize, quote: &str, new: &str, tight: bool, body: &str| Thread {
            id: self.threads.len() as u64 + 1,
            eid: e.id.clone(),
            kind,
            status: Status::Open,
            author: e.actor.clone(),
            agent: e.agent,
            at: e.at,
            quote: quote.into(),
            line,
            col,
            new: new.into(),
            tight,
            body: body.into(),
            replies: vec![],
            decided_at: None,
            decided_by: None,
            note: None,
        };
        match &e.what {
            What::Tracked { .. } => {}
            What::Proposed { kind, line, col, quote, new, tight, why } => {
                let t = make(*kind, *line, *col, quote, new, *tight, why);
                self.threads.push(t);
            }
            What::Commented { line, col, quote, body } => {
                let t = make(Kind::Comment, *line, *col, quote, "", false, body);
                self.threads.push(t);
            }
            What::DocEdit { line, old, new, .. } => {
                for t in self.threads.iter_mut().filter(|t| t.status == Status::Open) {
                    reanchor(t, *line, old, new);
                }
            }
            what => {
                let found = e.thread().and_then(|id| self.threads.iter_mut().find(|t| t.eid == id));
                if let Some(t) = found {
                  match what {
                    What::Replied { body, .. } => t.replies.push(Reply {
                        author: e.actor.clone(),
                        agent: e.agent,
                        at: e.at,
                        body: body.clone(),
                    }),
                    What::Accepted { new, note, .. } => {
                        if t.status == Status::Open {
                            t.new = new.clone();
                        }
                        decided(t, Status::Accepted, &e, note)
                    }
                    What::Rejected { note, .. } => decided(t, Status::Rejected, &e, note),
                    What::Resolved { .. } => decided(t, Status::Resolved, &e, &None),
                    What::Reopened { .. } => {
                        t.status = Status::Open;
                        t.decided_at = None;
                        t.decided_by = None;
                        t.note = None;
                    }
                    _ => {}
                  }
                }
            }
        }
        self.events.push(e);
    }
}

/// Move the anchor of a thread across a document edit that replaced the
/// lines `old` at line `p` with the lines `new`.
fn reanchor(t: &mut Thread, p: usize, old: &str, new: &str) {
    let n_old = old.split_inclusive('\n').count();
    let n_new = new.split_inclusive('\n').count();
    let t_lines = t.quote.split('\n').count();
    if t.line + t_lines <= p {
        return;
    }
    if t.line >= p + n_old {
        t.line = t.line + n_new - n_old;
        return;
    }
    // The edit touched the anchor text.
    let whole_line = t.kind == Kind::Insert;
    if let Some(r) = anchor::resolve(new, &t.quote, t.line.saturating_sub(p), t.col, whole_line) {
        t.line = p + anchor::line_of(new, r.start);
        t.col = anchor::pos(new, r.start).1;
    } else if t.kind == Kind::Comment {
        // The human rewrote the text below a comment: keep the comment on the new text.
        if let Some((i, l)) = new.lines().enumerate().find(|(_, l)| !l.trim().is_empty()) {
            t.quote = l.to_string();
            t.line = p + i;
            t.col = 0;
        }
    }
}

/// The line hunk between two texts: (first changed line, old lines, new lines).
pub fn hunk(old: &str, new: &str) -> Option<(usize, String, String)> {
    if old == new {
        return None;
    }
    let ol: Vec<&str> = old.split_inclusive('\n').collect();
    let nl: Vec<&str> = new.split_inclusive('\n').collect();
    let p = ol.iter().zip(&nl).take_while(|(a, b)| a == b).count();
    let max_s = ol.len().min(nl.len()) - p;
    let s = ol.iter().rev().zip(nl.iter().rev()).take(max_s).take_while(|(a, b)| a == b).count();
    Some((p, ol[p..ol.len() - s].concat(), nl[p..nl.len() - s].concat()))
}

#[derive(Clone, Debug)]
pub struct Actor {
    pub name: String,
    pub agent: bool,
}

/// The text before an edit that replaced lines `old` at `line` with `new`.
fn unapply(doc: &str, line: usize, old: &str, new: &str) -> Option<String> {
    let lines: Vec<&str> = doc.split_inclusive('\n').collect();
    let n = new.split_inclusive('\n').count();
    if line + n > lines.len() || lines[line..line + n].concat() != new {
        return None;
    }
    Some(format!("{}{old}{}", lines[..line].concat(), lines[line + n..].concat()))
}

/// Edits of these kinds collapse into the edit before them.
fn collapsible(via: &str) -> bool {
    matches!(via, "edit" | "file" | "new" | "delete")
}

/// A run of edits ends after this idle time.
const RUN_GAP_MINUTES: i64 = 30;

/// One locked read-change-write cycle. All changes are events: `log` adds an
/// event and applies it to `state`.
pub struct Tx {
    pub state: State,
    pub doc: String,
    replica: String,
    fresh: Vec<Event>,
    /// Ids of superseded events, to remove from this replica's log file.
    dropped: Vec<String>,
}

impl Tx {
    /// Add an event; returns its id.
    pub fn log(&mut self, actor: &Actor, what: What) -> String {
        self.push(actor, what, vec![])
    }

    fn push(&mut self, actor: &Actor, what: What, supersedes: Vec<String>) -> String {
        let lamport = self.state.lamport + 1;
        let e = Event {
            id: format!("{lamport:08x}-{}", self.replica),
            lamport,
            replica: self.replica.clone(),
            at: Utc::now(),
            actor: actor.name.clone(),
            agent: actor.agent,
            supersedes,
            what,
        };
        let id = e.id.clone();
        if e.supersedes.is_empty() {
            self.fresh.push(e.clone());
            self.state.apply(e);
        } else {
            // Fold again without the superseded events.
            self.fresh.retain(|f| !e.supersedes.contains(&f.id));
            self.dropped.extend(e.supersedes.iter().cloned());
            self.fresh.push(e.clone());
            let mut all = std::mem::take(&mut self.state.events);
            all.push(e);
            self.state = State::fold(all);
            self.state.lamport = lamport;
        }
        id
    }

    pub fn edit_doc(&mut self, range: Range<usize>, text: &str, via: &str, actor: &Actor) {
        let mut new = self.doc.clone();
        new.replace_range(range, text);
        self.set_doc(new, via, actor);
    }

    /// Replace the document text and record the change. Edits in a row by
    /// one actor, with no other event between them, are one event: the new
    /// event covers the full change and supersedes the edit before it.
    pub fn set_doc(&mut self, new: String, via: &str, actor: &Actor) {
        if new == self.doc {
            return;
        }
        if collapsible(via)
            && let Some(last) = self.state.events.last()
            && let What::DocEdit { via: last_via, line, old, new: last_new } = &last.what
            && collapsible(last_via)
            && last.actor == actor.name
            && last.agent == actor.agent
            && last.replica == self.replica
            && (Utc::now() - last.at).num_minutes() < RUN_GAP_MINUTES
            && let Some(before) = unapply(&self.doc, *line, old, last_new)
            && let Some((line, old_hunk, new_hunk)) = hunk(&before, &new)
        {
            let mut supersedes = vec![last.id.clone()];
            supersedes.extend(last.supersedes.iter().cloned());
            self.doc = new;
            self.push(actor, What::DocEdit { via: "edit".into(), line, old: old_hunk, new: new_hunk }, supersedes);
            return;
        }
        let Some((line, old_hunk, new_hunk)) = hunk(&self.doc, &new) else { return };
        self.doc = new;
        self.log(actor, What::DocEdit { via: via.into(), line, old: old_hunk, new: new_hunk });
    }
}

pub struct DocStore {
    /// Canonical path of the document.
    pub doc: PathBuf,
    /// State directory of the document.
    pub dir: PathBuf,
    /// Who gets the credit when the file on disk changed between two commands.
    pub file_actor: Actor,
}

fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf29ce484222325, |h, b| (h ^ *b as u64).wrapping_mul(0x100000001b3))
}

/// The id of this machine's log. Each replica appends only to its own file,
/// so a file-sync tool can copy logs between machines with no conflict.
fn replica() -> String {
    let path = home().join("replica");
    if let Ok(s) = fs::read_to_string(&path) {
        let s = s.trim();
        if !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric()) {
            return s.to_string();
        }
    }
    let seed = format!("{:?}{}", std::time::SystemTime::now(), std::process::id());
    let id = format!("{:08x}", fnv1a(seed.as_bytes()) as u32);
    let _ = fs::create_dir_all(home());
    let _ = fs::write(&path, &id);
    id
}

impl DocStore {
    pub fn open(path: &Path) -> Result<Self> {
        let doc = fs::canonicalize(path).with_context(|| format!("cannot open {}", path.display()))?;
        if !doc.is_file() {
            bail!("{} is not a file", doc.display());
        }
        let stem: String = doc
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default()
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
            .collect();
        let hash = fnv1a(doc.to_string_lossy().as_bytes()) as u32;
        let dir = home().join("docs").join(format!("{stem}-{hash:08x}"));
        fs::create_dir_all(dir.join("events")).with_context(|| format!("cannot create {}", dir.display()))?;
        let meta = dir.join("meta.json");
        if !meta.exists() {
            fs::write(meta, serde_json::to_string_pretty(&serde_json::json!({ "path": doc }))?)?;
        }
        Ok(Self { doc, dir, file_actor: Actor { name: "unknown".into(), agent: false } })
    }

    fn snapshot_path(&self) -> PathBuf {
        self.dir.join("snapshot.md")
    }

    /// Changes when the document or the log changes.
    pub fn stamp(&self) -> (Option<std::time::SystemTime>, u64) {
        let size: u64 = fs::read_dir(self.dir.join("events"))
            .map(|rd| rd.flatten().filter_map(|e| e.metadata().ok()).map(|m| m.len()).sum())
            .unwrap_or(0);
        (fs::metadata(&self.doc).and_then(|m| m.modified()).ok(), size)
    }

    /// Read the logs of all replicas. Needs no lock: a half-written last
    /// line does not parse, and the next read gets it.
    pub fn events(&self) -> Vec<Event> {
        let Ok(rd) = fs::read_dir(self.dir.join("events")) else { return vec![] };
        rd.flatten()
            .filter_map(|e| fs::read_to_string(e.path()).ok())
            .flat_map(|s| s.lines().filter_map(|l| serde_json::from_str(l).ok()).collect::<Vec<Event>>())
            .collect()
    }

    pub fn load(&self) -> State {
        State::fold(self.events())
    }

    pub fn read_doc(&self) -> Result<String> {
        fs::read_to_string(&self.doc).with_context(|| format!("cannot read {}", self.doc.display()))
    }

    fn append(&self, events: &[Event]) -> Result<()> {
        for e in events {
            let path = self.dir.join("events").join(format!("{}.jsonl", e.replica));
            let mut f = fs::OpenOptions::new().create(true).append(true).open(path)?;
            writeln!(f, "{}", serde_json::to_string(e)?)?;
        }
        Ok(())
    }

    /// Add events from another replica's log. Returns the number of new events.
    pub fn merge(&self, foreign: Vec<Event>) -> Result<usize> {
        let lock = fs::File::create(self.dir.join("lock"))?;
        lock.lock()?;
        let known: std::collections::HashSet<String> = self.events().into_iter().map(|e| e.id).collect();
        let fresh: Vec<Event> = foreign.into_iter().filter(|e| !known.contains(&e.id)).collect();
        self.append(&fresh)?;
        Ok(fresh.len())
    }

    /// Run `f` with the lock held. First records a change of the file that
    /// happened since the last command. Then saves the document and appends
    /// the new events. Returns the result of `f`, the state and the text.
    pub fn transact<T>(&self, f: impl FnOnce(&mut Tx) -> Result<T>) -> Result<(T, State, String)> {
        let lock = fs::File::create(self.dir.join("lock"))?;
        lock.lock()?;
        let disk = self.read_doc()?;
        let mut tx = Tx { state: self.load(), doc: disk.clone(), replica: replica(), fresh: vec![], dropped: vec![] };
        let snapshot = fs::read_to_string(self.snapshot_path()).ok();
        match &snapshot {
            None => {
                let who = Actor { name: "redline".into(), agent: false };
                tx.log(&who, What::Tracked { path: self.doc.to_string_lossy().into() });
            }
            Some(s) if *s != disk => {
                tx.doc = s.clone();
                tx.set_doc(disk.clone(), "file", &self.file_actor);
            }
            _ => {}
        }
        let out = f(&mut tx)?;
        if tx.doc != disk {
            fs::write(&self.doc, &tx.doc)?;
        }
        if snapshot.as_deref() != Some(tx.doc.as_str()) {
            fs::write(self.snapshot_path(), &tx.doc)?;
        }
        if !tx.dropped.is_empty() {
            // Compaction: this replica is the only writer of its file.
            let path = self.dir.join("events").join(format!("{}.jsonl", tx.replica));
            if let Ok(s) = fs::read_to_string(&path) {
                let keep: String = s
                    .lines()
                    .filter(|l| {
                        serde_json::from_str::<Event>(l).map(|e| !tx.dropped.contains(&e.id)).unwrap_or(true)
                    })
                    .map(|l| format!("{l}\n"))
                    .collect();
                let tmp = path.with_extension("tmp");
                fs::write(&tmp, keep)?;
                fs::rename(tmp, &path)?;
            }
        }
        self.append(&tx.fresh)?;
        Ok((out, tx.state, tx.doc))
    }
}

/// All tracked documents: (document path, state directory).
pub fn tracked() -> Vec<(String, PathBuf)> {
    let Ok(rd) = fs::read_dir(home().join("docs")) else {
        return vec![];
    };
    let mut v: Vec<_> = rd
        .flatten()
        .filter_map(|e| {
            let meta = fs::read_to_string(e.path().join("meta.json")).ok()?;
            let j: serde_json::Value = serde_json::from_str(&meta).ok()?;
            Some((j["path"].as_str()?.to_string(), e.path()))
        })
        .collect();
    v.sort();
    v
}

/// "3m", "2h", "5d": the age of a timestamp.
pub fn ago(at: DateTime<Utc>) -> String {
    let s = (Utc::now() - at).num_seconds().max(0);
    match s {
        0..60 => "now".into(),
        60..3600 => format!("{}m", s / 60),
        3600..86400 => format!("{}h", s / 3600),
        _ => format!("{}d", s / 86400),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tx(doc: &str) -> Tx {
        Tx { state: State::default(), doc: doc.into(), replica: "r1".into(), fresh: vec![], dropped: vec![] }
    }
    fn human() -> Actor {
        Actor { name: "zach".into(), agent: false }
    }
    fn agent() -> Actor {
        Actor { name: "claude".into(), agent: true }
    }
    fn edits(st: &State) -> Vec<(String, String)> {
        st.events
            .iter()
            .filter_map(|e| match &e.what {
                What::DocEdit { old, new, .. } => Some((old.clone(), new.clone())),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn edits_in_a_row_are_one_event() {
        let mut t = tx("a\nb\nc\n");
        t.set_doc("a\nB\nc\n".into(), "edit", &human());
        t.set_doc("a\nB\nC\n".into(), "file", &human());
        t.set_doc("a\nB\nC\nd\n".into(), "edit", &human());
        assert_eq!(edits(&t.state), vec![("b\nc\n".to_string(), "B\nC\nd\n".to_string())]);
        assert_eq!(t.fresh.len(), 1);
        // A replica that still has the old events gets the same state.
        assert_eq!(t.fresh[0].supersedes.len(), 2);
    }

    #[test]
    fn another_event_ends_the_run() {
        let mut t = tx("a\nb\n");
        t.set_doc("a\nB\n".into(), "edit", &human());
        t.log(&agent(), What::Commented { line: 0, col: 0, quote: "a".into(), body: "?".into() });
        t.set_doc("A\nB\n".into(), "edit", &human());
        assert_eq!(edits(&t.state).len(), 2);
    }

    #[test]
    fn fold_does_not_depend_on_arrival_order() {
        let mut t = tx("one\ntwo\nthree\n");
        let p = t.log(&agent(), What::Proposed { kind: Kind::Edit, line: 1, col: 0, quote: "two".into(), new: "2".into(), tight: false, why: String::new() });
        let c = t.log(&agent(), What::Commented { line: 2, col: 0, quote: "three".into(), body: "why?".into() });
        t.set_doc("zero\none\ntwo\nthree\n".into(), "edit", &human());
        t.log(&human(), What::Rejected { thread: p.clone(), note: Some("no".into()) });
        t.log(&human(), What::Accepted { thread: p, modified: false, new: "2".into(), note: None });
        t.log(&human(), What::Replied { thread: c, body: "because".into() });
        let mut shuffled = t.fresh.clone();
        shuffled.reverse();
        shuffled.extend(t.fresh.clone()); // duplicates do no harm
        let st = State::fold(shuffled);
        assert_eq!(st.threads.len(), 2);
        assert_eq!(st.threads[0].status, Status::Rejected); // the first decision wins
        assert_eq!(st.threads[1].line, 3); // the edit above moved the anchor
        assert_eq!(st.threads[1].replies.len(), 1);
        assert_eq!(st.threads.iter().map(|t| t.line).collect::<Vec<_>>(), t.state.threads.iter().map(|t| t.line).collect::<Vec<_>>());
    }
}
