//! Persistent state. Every tracked document gets a directory below
//! `~/.redline/docs/` with the open threads (`state.json`), an append-only
//! history (`history.jsonl`) and a snapshot of the last text redline saw.

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

/// A comment or a proposed change, with its replies.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Thread {
    pub id: u64,
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
    /// Edit/Insert: the proposed text.
    #[serde(default)]
    pub new: String,
    /// Insert: add the text on the next line, without blank lines around it.
    #[serde(default)]
    pub tight: bool,
    /// Comment body, or the reason for a proposal.
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub replies: Vec<Reply>,
    #[serde(default)]
    pub decided_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub decided_by: Option<String>,
    #[serde(default)]
    pub note: Option<String>,
}

impl Thread {
    pub fn locate(&self, doc: &str) -> Option<Range<usize>> {
        anchor::resolve(doc, &self.quote, self.line, self.kind == Kind::Insert)
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

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct State {
    #[serde(default)]
    pub next_id: u64,
    #[serde(default)]
    pub threads: Vec<Thread>,
    /// Number of history events that the agent already read with `redline wait`.
    #[serde(default)]
    pub agent_seen: usize,
}

impl State {
    pub fn thread(&mut self, id: u64) -> Result<&mut Thread> {
        self.threads
            .iter_mut()
            .find(|t| t.id == id)
            .with_context(|| format!("no thread #{id}"))
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum What {
    Tracked { path: String },
    DocEdit { via: String, line: usize, old: String, new: String },
    Proposed { id: u64, kind: Kind, line: usize, old: String, new: String, why: String },
    Commented { id: u64, line: usize, quote: String, body: String },
    Replied { id: u64, body: String },
    Accepted { id: u64, modified: bool, new: String, note: Option<String> },
    Rejected { id: u64, note: Option<String> },
    Resolved { id: u64 },
    Reopened { id: u64 },
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Event {
    pub at: DateTime<Utc>,
    pub actor: String,
    pub agent: bool,
    #[serde(flatten)]
    pub what: What,
}

impl Event {
    /// One-line summary and detail lines. The detail lines start with
    /// "- " (removed text), "+ " (added text) or "  " (other text).
    pub fn describe(&self) -> (String, Vec<String>) {
        fn diff(old: &str, new: &str) -> Vec<String> {
            let mut v: Vec<String> = old.lines().map(|l| format!("- {l}")).collect();
            v.extend(new.lines().map(|l| format!("+ {l}")));
            v
        }
        fn text(s: &str) -> Vec<String> {
            s.lines().map(|l| format!("  {l}")).collect()
        }
        match &self.what {
            What::Tracked { path } => ("started to track the document".into(), text(path)),
            What::DocEdit { via, line, old, new } => {
                let how = match via.as_str() {
                    "external" => "changed the file outside redline",
                    "undo" => "undid a change",
                    "delete" => "deleted text",
                    "new" => "added a block",
                    _ => "edited the document",
                };
                (format!("{how} at L{}", line + 1), diff(old, new))
            }
            What::Proposed { id, kind, line, old, new, why } => {
                let mut d = match kind {
                    Kind::Insert => diff("", new),
                    _ => diff(old, new),
                };
                d.extend(text(why));
                (format!("proposed {} #{id} at L{}", kind.name(), line + 1), d)
            }
            What::Commented { id, line, body, .. } => {
                (format!("commented #{id} on L{}", line + 1), text(body))
            }
            What::Replied { id, body } => (format!("replied on #{id}"), text(body)),
            What::Accepted { id, modified, new, note } => {
                let mut d = vec![];
                if *modified {
                    d.extend(diff("", new));
                }
                d.extend(text(note.as_deref().unwrap_or("")));
                let m = if *modified { " with changes" } else { "" };
                (format!("accepted #{id}{m}"), d)
            }
            What::Rejected { id, note } => {
                (format!("rejected #{id}"), text(note.as_deref().unwrap_or("")))
            }
            What::Resolved { id } => (format!("resolved #{id}"), vec![]),
            What::Reopened { id } => (format!("reopened #{id} (undo)"), vec![]),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Actor {
    pub name: String,
    pub agent: bool,
}

/// One locked read-change-write cycle on a document and its state.
pub struct Tx {
    pub state: State,
    pub doc: String,
    pub events: Vec<Event>,
    doc_dirty: bool,
}

impl Tx {
    pub fn log(&mut self, actor: &Actor, what: What) {
        self.events.push(Event { at: Utc::now(), actor: actor.name.clone(), agent: actor.agent, what });
    }

    pub fn edit_doc(&mut self, range: Range<usize>, text: &str, via: Option<&str>, actor: &Actor) {
        let mut new = self.doc.clone();
        new.replace_range(range, text);
        self.set_doc(new, via, actor);
    }

    /// Replace the document text and move the thread anchors with the change.
    pub fn set_doc(&mut self, new: String, via: Option<&str>, actor: &Actor) {
        if new == self.doc {
            return;
        }
        let old = std::mem::replace(&mut self.doc, new);
        self.doc_dirty = true;
        let ol: Vec<&str> = old.split_inclusive('\n').collect();
        let nl: Vec<&str> = self.doc.split_inclusive('\n').collect();
        let p = ol.iter().zip(&nl).take_while(|(a, b)| a == b).count();
        let max_s = ol.len().min(nl.len()) - p;
        let s = ol.iter().rev().zip(nl.iter().rev()).take(max_s).take_while(|(a, b)| a == b).count();
        let h0: usize = ol[..p].iter().map(|l| l.len()).sum();
        let old_hunk = ol[p..ol.len() - s].concat();
        let new_hunk = nl[p..nl.len() - s].concat();
        let h1 = h0 + old_hunk.len();
        let delta = nl.len() as isize - ol.len() as isize;

        for t in self.state.threads.iter_mut().filter(|t| t.status == Status::Open) {
            match t.locate(&old) {
                Some(r) if r.end <= h0 => {}
                Some(r) if r.start >= h1 && !r.is_empty() => {
                    t.line = (t.line as isize + delta).max(0) as usize;
                }
                _ => {
                    if let Some(r) = t.locate(&self.doc) {
                        t.line = anchor::line_of(&self.doc, r.start);
                    } else if t.kind == Kind::Comment {
                        // The human rewrote the text below a comment: keep the
                        // comment on the new text.
                        if let Some((i, l)) =
                            new_hunk.lines().enumerate().find(|(_, l)| !l.trim().is_empty())
                        {
                            t.quote = l.to_string();
                            t.line = p + i;
                        }
                    }
                }
            }
        }
        if let Some(via) = via {
            self.log(
                actor,
                What::DocEdit {
                    via: via.into(),
                    line: p,
                    old: old_hunk.trim_end_matches('\n').into(),
                    new: new_hunk.trim_end_matches('\n').into(),
                },
            );
        }
    }
}

pub struct DocStore {
    /// Canonical path of the document.
    pub doc: PathBuf,
    /// State directory of the document.
    pub dir: PathBuf,
}

fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf29ce484222325, |h, b| (h ^ *b as u64).wrapping_mul(0x100000001b3))
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
        fs::create_dir_all(&dir).with_context(|| format!("cannot create {}", dir.display()))?;
        let meta = dir.join("meta.json");
        if !meta.exists() {
            fs::write(meta, serde_json::to_string_pretty(&serde_json::json!({ "path": doc }))?)?;
        }
        Ok(Self { doc, dir })
    }

    pub fn state_path(&self) -> PathBuf {
        self.dir.join("state.json")
    }
    fn history_path(&self) -> PathBuf {
        self.dir.join("history.jsonl")
    }
    fn snapshot_path(&self) -> PathBuf {
        self.dir.join("snapshot.md")
    }

    pub fn load(&self) -> Result<State> {
        match fs::read_to_string(self.state_path()) {
            Ok(s) => Ok(serde_json::from_str(&s).context("state.json is damaged")?),
            Err(_) => Ok(State { next_id: 1, ..Default::default() }),
        }
    }

    pub fn read_doc(&self) -> Result<String> {
        fs::read_to_string(&self.doc).with_context(|| format!("cannot read {}", self.doc.display()))
    }

    pub fn history(&self) -> Result<Vec<Event>> {
        let Ok(s) = fs::read_to_string(self.history_path()) else {
            return Ok(vec![]);
        };
        Ok(s.lines().filter_map(|l| serde_json::from_str(l).ok()).collect())
    }

    /// Run `f` with the lock held. Records changes that something made to the
    /// file outside redline, then saves the state, the document and the
    /// new history events. Returns the result of `f`, the state and the text.
    pub fn transact<T>(&self, f: impl FnOnce(&mut Tx) -> Result<T>) -> Result<(T, State, String)> {
        let lock = fs::File::create(self.dir.join("lock"))?;
        lock.lock()?;
        let disk = self.read_doc()?;
        let mut tx = Tx { state: self.load()?, doc: disk.clone(), events: vec![], doc_dirty: false };
        let snapshot = fs::read_to_string(self.snapshot_path()).ok();
        match &snapshot {
            None => {
                let who = Actor { name: "redline".into(), agent: false };
                tx.log(&who, What::Tracked { path: self.doc.to_string_lossy().into() });
            }
            Some(s) if *s != disk => {
                tx.doc = s.clone();
                let who = Actor { name: "external".into(), agent: false };
                tx.set_doc(disk.clone(), Some("external"), &who);
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
        let tmp = self.dir.join("state.json.tmp");
        fs::write(&tmp, serde_json::to_string_pretty(&tx.state)?)?;
        fs::rename(tmp, self.state_path())?;
        if !tx.events.is_empty() {
            let mut h = fs::OpenOptions::new().create(true).append(true).open(self.history_path())?;
            for e in &tx.events {
                writeln!(h, "{}", serde_json::to_string(e)?)?;
            }
        }
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
