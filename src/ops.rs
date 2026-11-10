//! A review session for a front end that runs in one process (the native
//! app): the current state plus the human's actions. Each action is one
//! transaction on the event log.

use crate::anchor;
use crate::config::Config;
use crate::store::{Actor, DocStore, State, Status, Thread, Tx, What};
use anyhow::{Context, Result, bail};
use std::ops::Range;
use std::path::Path;
use std::time::SystemTime;

struct Undo {
    doc: String,
    reopen: Option<u64>,
}

pub struct Session {
    pub store: DocStore,
    pub me: Actor,
    pub state: State,
    pub doc: String,
    /// Changes each time the state or the text changes.
    pub version: u64,
    undo: Vec<Undo>,
    stamp: (Option<SystemTime>, u64),
}

impl Session {
    pub fn open(path: &Path) -> Result<Self> {
        let store = DocStore::open(path)?;
        let me = Actor { name: Config::load().author(), agent: false };
        let (_, state, doc) = store.transact(|_| Ok(()))?;
        let stamp = store.stamp();
        Ok(Self { store, me, state, doc, version: 1, undo: vec![], stamp })
    }

    fn tx<T>(&mut self, f: impl FnOnce(&mut Tx, &Actor) -> Result<T>) -> Result<T> {
        let me = self.me.clone();
        let (out, state, doc) = self.store.transact(|tx| f(tx, &me))?;
        self.state = state;
        self.doc = doc;
        self.version += 1;
        self.stamp = self.store.stamp();
        Ok(out)
    }

    /// Reload when an agent or another program changed the log or the file.
    pub fn poll(&mut self) -> Result<bool> {
        if self.store.stamp() == self.stamp {
            return Ok(false);
        }
        self.tx(|_, _| Ok(()))?;
        Ok(true)
    }

    /// The open threads with their place in the text, in document order.
    /// A thread whose anchor text is gone comes last, with no range.
    pub fn open_threads(&self) -> Vec<(&Thread, Option<Range<usize>>)> {
        let mut v: Vec<_> = self
            .state
            .threads
            .iter()
            .filter(|t| t.status == Status::Open)
            .map(|t| (t, t.locate(&self.doc)))
            .collect();
        v.sort_by_key(|(t, r)| (r.as_ref().map(|r| r.start).unwrap_or(usize::MAX), t.id));
        v
    }

    /// Accept a proposal, with `changed` in place of the proposed text if
    /// given. Returns the line of the change.
    pub fn accept(&mut self, id: u64, changed: Option<String>) -> Result<usize> {
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
            let (range, text) =
                t.apply(&tx.doc).context("the text that this proposal points at is gone. Reject it")?;
            let line = anchor::line_of(&tx.doc, range.start);
            tx.log(me, What::Accepted { thread: t.eid.clone(), modified, new: t.new.clone(), note: None });
            tx.edit_doc(range, &text, "accept", me);
            Ok(line)
        })?;
        self.undo.push(Undo { doc: before, reopen: Some(id) });
        Ok(line)
    }

    pub fn reject(&mut self, id: u64, note: Option<String>) -> Result<()> {
        self.tx(|tx, me| {
            let thread = tx.state.thread(id)?.eid.clone();
            tx.log(me, What::Rejected { thread, note });
            Ok(())
        })
    }

    pub fn resolve(&mut self, id: u64) -> Result<()> {
        self.tx(|tx, me| {
            let thread = tx.state.thread(id)?.eid.clone();
            tx.log(me, What::Resolved { thread });
            Ok(())
        })
    }

    pub fn reply(&mut self, id: u64, body: String) -> Result<()> {
        self.tx(|tx, me| {
            let thread = tx.state.thread(id)?.eid.clone();
            tx.log(me, What::Replied { thread, body });
            Ok(())
        })
    }

    /// Comment on `quote`, which starts at (line, col). Returns the thread number.
    pub fn comment(&mut self, quote: String, line: usize, col: usize, body: String) -> Result<u64> {
        self.tx(|tx, me| {
            let eid = tx.log(me, What::Commented { line, col, quote, body });
            Ok(crate::cli::num(&tx.state, &eid))
        })
    }

    /// Replace a byte range of the text that this session shows.
    pub fn edit(&mut self, range: Range<usize>, text: &str, via: &str) -> Result<()> {
        let before = self.doc.clone();
        self.tx(|tx, me| {
            if tx.doc != before {
                bail!("the file changed on disk during the edit. Do the edit again");
            }
            tx.edit_doc(range, text, via, me);
            Ok(())
        })?;
        if self.doc != before {
            self.undo.push(Undo { doc: before, reopen: None });
        }
        Ok(())
    }

    /// Undo the last change of this session. An undo of an accept opens the
    /// proposal again. Returns false when there is nothing to undo.
    pub fn undo(&mut self) -> Result<bool> {
        let Some(u) = self.undo.pop() else { return Ok(false) };
        self.tx(|tx, me| {
            if let Some(id) = u.reopen {
                let t = tx.state.thread(id)?;
                if t.status == Status::Accepted {
                    let thread = t.eid.clone();
                    tx.log(me, What::Reopened { thread });
                }
            }
            tx.set_doc(u.doc, "undo", me);
            Ok(())
        })?;
        Ok(true)
    }
}
