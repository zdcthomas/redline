//! The command-line interface. It is the contract for every front end: the
//! agent, the bundled TUI and editor integrations all push events through it.
//!
//! An agent proposes and comments. The decision verbs (accept, reject,
//! resolve, reopen, sync) need `--human`: an editor integration passes it.
//! With `--stdin` a command works on the text of an unsaved editor buffer.

use crate::anchor;
use crate::config::Config;
use crate::store::{Actor, DocStore, Event, Kind, State, Status, Thread, Tx, What, ago, hunk, tracked};
use anyhow::{Context, Result, bail};
use chrono::Local;
use clap::{Args, Parser, Subcommand};
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::time::{Duration, Instant};

const SKILL: &str = include_str!("../skill/redline/SKILL.md");

#[derive(Parser)]
#[command(
    name = "redline",
    version,
    about = "A spec review tool. The human writes; agents suggest. Every action is an event.",
    args_conflicts_with_subcommands = true
)]
pub struct Cli {
    #[command(subcommand)]
    cmd: Option<Cmd>,
    /// Document to open in the TUI.
    file: Option<PathBuf>,
}

#[derive(Args)]
struct Who {
    /// Author name for the history. Default: $REDLINE_AUTHOR, then "agent"
    /// (or the configured author with --human).
    #[arg(long = "as")]
    author: Option<String>,
    /// Act as the human (for editor integrations). Also: REDLINE_ROLE=human.
    #[arg(long)]
    human: bool,
}

impl Who {
    fn is_human(&self) -> bool {
        self.human || std::env::var("REDLINE_ROLE").is_ok_and(|r| r == "human")
    }
    fn actor(&self) -> Actor {
        let human = self.is_human();
        let name = self
            .author
            .clone()
            .or_else(|| std::env::var("REDLINE_AUTHOR").ok())
            .unwrap_or_else(|| if human { Config::load().author() } else { "agent".into() });
        Actor { name, agent: !human }
    }
    /// The actor for a verb that only the human has.
    fn human(&self, verb: &str) -> Result<Actor> {
        if !self.is_human() {
            bail!("only the human can {verb}. An agent proposes and comments. An editor integration passes --human");
        }
        Ok(self.actor())
    }
}

#[derive(Args)]
struct Buf {
    /// Read the current document text from stdin (an unsaved editor buffer)
    /// and do not read or write the file.
    #[arg(long)]
    stdin: bool,
    /// Print JSON.
    #[arg(long)]
    json: bool,
}

impl Buf {
    /// The text to work on: the buffer from stdin, or the file.
    fn text(&self, tx: &Tx) -> Result<String> {
        if !self.stdin {
            return Ok(tx.doc.clone());
        }
        let mut s = String::new();
        std::io::stdin().read_to_string(&mut s)?;
        Ok(s)
    }
}

#[derive(Args)]
struct Target {
    /// Exact text in the document. Use "-" to read it from stdin.
    #[arg(long, visible_aliases = ["old", "on"], allow_hyphen_values = true)]
    text: Option<String>,
    /// Line range from `redline show`, for example 12 or 12-15.
    #[arg(long)]
    lines: Option<String>,
    /// If --text occurs more than once, take the occurrence nearest this line.
    #[arg(long)]
    near_line: Option<usize>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Open the TUI (the same as `redline FILE`).
    Open { file: PathBuf },
    /// Print the document with line numbers and the open threads.
    Show {
        file: PathBuf,
        #[arg(long)]
        json: bool,
    },
    /// List the threads, with the position of each anchor.
    Threads {
        file: PathBuf,
        /// Include the accepted, rejected and resolved threads.
        #[arg(long)]
        all: bool,
        #[command(flatten)]
        buf: Buf,
    },
    /// Print one thread.
    Thread {
        file: PathBuf,
        id: u64,
        #[command(flatten)]
        buf: Buf,
    },
    /// Propose a change. The human accepts or rejects it.
    Propose {
        #[command(subcommand)]
        what: Propose,
    },
    /// Comment on a part of the document.
    Comment {
        file: PathBuf,
        #[command(flatten)]
        target: Target,
        /// The comment. Use "-" to read it from stdin.
        #[arg(long, allow_hyphen_values = true)]
        body: String,
        #[command(flatten)]
        who: Who,
        #[command(flatten)]
        buf: Buf,
    },
    /// Reply on a thread.
    Reply {
        file: PathBuf,
        id: u64,
        #[arg(long, allow_hyphen_values = true)]
        body: String,
        #[command(flatten)]
        who: Who,
    },
    /// [human] Accept a proposal and apply it to the document.
    Accept {
        file: PathBuf,
        id: u64,
        /// Accept with this text in place of the proposed text.
        #[arg(long, allow_hyphen_values = true)]
        new: Option<String>,
        #[command(flatten)]
        who: Who,
        #[command(flatten)]
        buf: Buf,
    },
    /// [human] Reject a proposal.
    Reject {
        file: PathBuf,
        id: u64,
        /// The reason. The agent reads it.
        #[arg(long, allow_hyphen_values = true)]
        note: Option<String>,
        #[command(flatten)]
        who: Who,
    },
    /// [human] Resolve a comment.
    Resolve {
        file: PathBuf,
        id: u64,
        #[command(flatten)]
        who: Who,
    },
    /// [human] Open a decided thread again.
    Reopen {
        file: PathBuf,
        id: u64,
        #[command(flatten)]
        who: Who,
    },
    /// [human] Record the changes of the saved file as the human's edit.
    /// An editor integration calls this after each save.
    Sync {
        file: PathBuf,
        #[command(flatten)]
        who: Who,
    },
    /// Print the event history.
    Log {
        file: PathBuf,
        /// Show only the last N events.
        #[arg(short)]
        n: Option<usize>,
        #[arg(long)]
        json: bool,
    },
    /// Print each new event as it arrives, one JSON object per line. Runs
    /// until it is stopped. Editor integrations use it for live updates.
    Watch { file: PathBuf },
    /// [agent] Block until the human does something, then print the new events.
    Wait {
        file: PathBuf,
        /// Seconds to wait.
        #[arg(long, default_value_t = 100)]
        timeout: u64,
        #[arg(long)]
        json: bool,
    },
    /// Add the events of another replica's log (a .jsonl file). A merge is a
    /// set union, so the order and the number of merges do not matter.
    Merge { file: PathBuf, logs: Vec<PathBuf> },
    /// List the tracked documents.
    Docs,
    /// The agent skill that teaches an agent to use redline.
    Skill {
        #[command(subcommand)]
        what: SkillCmd,
    },
}

#[derive(Subcommand)]
enum Propose {
    /// Replace text.
    Edit {
        file: PathBuf,
        #[command(flatten)]
        target: Target,
        /// The replacement text. Use "-" to read it from stdin.
        #[arg(long, allow_hyphen_values = true)]
        new: String,
        /// The reason for the change.
        #[arg(long, default_value = "", allow_hyphen_values = true)]
        why: String,
        #[command(flatten)]
        who: Who,
        #[command(flatten)]
        buf: Buf,
    },
    /// Add text after a line.
    Insert {
        file: PathBuf,
        /// Insert after this line number. 0 = top of the document.
        #[arg(long)]
        after_line: Option<usize>,
        /// Insert after the line that contains this text.
        #[arg(long, allow_hyphen_values = true)]
        after: Option<String>,
        /// The text to add. Use "-" to read it from stdin.
        #[arg(long, allow_hyphen_values = true)]
        text: String,
        /// Add the text on the next line, with no blank lines around it
        /// (for a list item or a table row).
        #[arg(long)]
        tight: bool,
        #[arg(long, default_value = "", allow_hyphen_values = true)]
        why: String,
        #[command(flatten)]
        who: Who,
    },
    /// Remove text.
    Delete {
        file: PathBuf,
        #[command(flatten)]
        target: Target,
        #[arg(long, default_value = "", allow_hyphen_values = true)]
        why: String,
        #[command(flatten)]
        who: Who,
        #[command(flatten)]
        buf: Buf,
    },
}

#[derive(Subcommand)]
enum SkillCmd {
    /// Write SKILL.md to a skills directory (default ~/.claude/skills/redline).
    Install {
        #[arg(long)]
        dir: Option<PathBuf>,
    },
    /// Print SKILL.md.
    Show,
}

fn text_arg(s: &str) -> Result<String> {
    if s != "-" {
        return Ok(s.to_string());
    }
    let mut buf = String::new();
    std::io::stdin().read_to_string(&mut buf)?;
    Ok(buf)
}

/// Find the text a command points at: (quote, 0-based line).
fn find_target(doc: &str, t: &Target) -> Result<(String, usize)> {
    match (&t.text, &t.lines) {
        (Some(text), None) => {
            let text = text_arg(text)?;
            let text = text.trim_matches('\n').to_string();
            let occ = anchor::occurrences(doc, &text, false);
            match (occ.len(), t.near_line) {
                (0, _) => bail!("the text is not in the document. Run `redline show` and copy it exactly"),
                (1, _) => Ok((text, occ[0].1)),
                (_, Some(near)) => {
                    let near = near.saturating_sub(1);
                    let line = occ.iter().map(|o| o.1).min_by_key(|l| l.abs_diff(near)).unwrap();
                    Ok((text, line))
                }
                _ => {
                    let lines: Vec<String> = occ.iter().map(|o| (o.1 + 1).to_string()).collect();
                    bail!("the text occurs {} times (lines {}). Add --near-line N, or use --lines", occ.len(), lines.join(", "))
                }
            }
        }
        (None, Some(spec)) => {
            let (a, b) = spec.split_once('-').unwrap_or((spec, spec));
            let a: usize = a.trim().parse().context("--lines needs N or N-M")?;
            let b: usize = b.trim().parse().context("--lines needs N or N-M")?;
            let n = anchor::line_starts(doc).len();
            if a == 0 || b < a || b > n {
                bail!("--lines {spec} is out of range: the document has {n} lines");
            }
            let quote = doc[anchor::lines_range(doc, a - 1, b - 1)].to_string();
            if quote.trim().is_empty() {
                bail!("lines {spec} are blank");
            }
            Ok((quote, a - 1))
        }
        _ => bail!("give one of --text or --lines"),
    }
}

/// The number of the thread that event `eid` made.
pub fn num(st: &State, eid: &str) -> u64 {
    st.threads.iter().find(|t| t.eid == eid).map(|t| t.id).unwrap_or(0)
}

fn open_thread(st: &State, id: u64) -> Result<&Thread> {
    let t = st.thread(id)?;
    if t.status != Status::Open {
        bail!("thread #{id} is {}", t.status.name());
    }
    Ok(t)
}

fn indent(prefix: &str, s: &str) {
    for l in s.lines() {
        println!("{prefix}{l}");
    }
}

fn print_thread(t: &Thread, doc: &str) {
    let place = match t.locate(doc) {
        _ if t.status != Status::Open => String::new(),
        Some(r) => format!("L{} ", anchor::line_of(doc, r.start) + 1),
        None => "anchor lost ".into(),
    };
    println!("#{} {} by {} [{}] {}· {}", t.id, t.kind.name(), t.author, t.status.name(), place, ago(t.at));
    match t.kind {
        Kind::Comment => indent("  > ", &t.quote),
        Kind::Edit => {
            indent("  - ", &t.quote);
            indent("  + ", &t.new);
        }
        Kind::Delete => indent("  - ", &t.quote),
        Kind::Insert => {
            let after = if t.quote.is_empty() { "(top)" } else { &t.quote };
            indent("  after: ", after);
            indent("  + ", &t.new);
        }
    }
    indent("  ", &t.body);
    if let Some(n) = &t.note {
        indent("  decision note: ", n);
    }
    for r in &t.replies {
        indent(&format!("  ↳ {}: ", r.author), &r.body);
    }
}

/// A thread as JSON, with the position of its anchor in `doc`:
/// `range.start` / `range.end` are {line, col}, 0-based, col in bytes.
fn thread_json(t: &Thread, doc: &str) -> Value {
    let mut v = serde_json::to_value(t).unwrap_or_default();
    let range = (t.status == Status::Open).then(|| t.locate(doc)).flatten();
    v["range"] = match range {
        Some(r) => {
            let (sl, sc) = anchor::pos(doc, r.start);
            let (el, ec) = anchor::pos(doc, r.end);
            json!({ "start": { "line": sl, "col": sc }, "end": { "line": el, "col": ec } })
        }
        None => Value::Null,
    };
    v
}

fn event_json(e: &Event, st: &State) -> Value {
    let mut v = serde_json::to_value(e).unwrap_or_default();
    v["summary"] = json!(e.describe(st).0);
    if let Some(eid) = e.thread() {
        v["thread_id"] = json!(num(st, eid));
    }
    v
}

fn print_event(e: &Event, st: &State) {
    let (head, detail) = e.describe(st);
    let who = if e.agent { format!("{} (agent)", e.actor) } else { e.actor.clone() };
    println!("{}  {who}  {head}", e.at.with_timezone(&Local).format("%Y-%m-%d %H:%M"));
    for d in detail {
        println!("    {d}");
    }
}

pub fn run() -> Result<()> {
    let cli = Cli::parse();
    let cmd = match (cli.cmd, cli.file) {
        (Some(c), _) => c,
        (None, Some(file)) => Cmd::Open { file },
        (None, None) => {
            use clap::CommandFactory;
            Cli::command().print_help()?;
            return Ok(());
        }
    };
    match cmd {
        Cmd::Open { file } => {
            if !file.exists() {
                std::fs::write(&file, "")?;
            }
            crate::tui::run(&file)
        }
        Cmd::Show { file, json } => {
            let store = DocStore::open(&file)?;
            let (_, state, doc) = store.transact(|_| Ok(()))?;
            let open: Vec<&Thread> = state.threads.iter().filter(|t| t.status == Status::Open).collect();
            if json {
                let lines: Vec<&str> = doc.lines().collect();
                let threads: Vec<Value> = open.iter().map(|t| thread_json(t, &doc)).collect();
                println!("{}", json!({ "path": store.doc, "lines": lines, "open_threads": threads }));
                return Ok(());
            }
            let n = anchor::line_starts(&doc).len();
            println!("{} — {n} lines, {} open threads", store.doc.display(), open.len());
            for (i, l) in doc.lines().enumerate() {
                println!("{:>4} | {l}", i + 1);
            }
            if !open.is_empty() {
                println!("\n── open threads ──");
                for t in open {
                    print_thread(t, &doc);
                }
            }
            Ok(())
        }
        Cmd::Threads { file, all, buf } => {
            let store = DocStore::open(&file)?;
            let (text, state, _) = store.transact(|tx| buf.text(tx))?;
            let list: Vec<&Thread> = state.threads.iter().filter(|t| all || t.status == Status::Open).collect();
            if buf.json {
                println!("{}", Value::Array(list.iter().map(|t| thread_json(t, &text)).collect()));
            } else if list.is_empty() {
                println!("no threads");
            } else {
                for t in list {
                    print_thread(t, &text);
                }
            }
            Ok(())
        }
        Cmd::Thread { file, id, buf } => {
            let store = DocStore::open(&file)?;
            let (text, state, _) = store.transact(|tx| buf.text(tx))?;
            let t = state.thread(id)?;
            if buf.json {
                println!("{}", thread_json(t, &text));
            } else {
                print_thread(t, &text);
            }
            Ok(())
        }
        Cmd::Propose { what } => {
            let (file, kind) = match &what {
                Propose::Edit { file, .. } => (file.clone(), Kind::Edit),
                Propose::Insert { file, .. } => (file.clone(), Kind::Insert),
                Propose::Delete { file, .. } => (file.clone(), Kind::Delete),
            };
            let store = DocStore::open(&file)?;
            let ((id, line), ..) = store.transact(|tx| {
                let (who, line, quote, new, tight, why) = match &what {
                    Propose::Edit { target, new, why, who, buf, .. } => {
                        let (quote, line) = find_target(&buf.text(tx)?, target)?;
                        let new = text_arg(new)?.trim_matches('\n').to_string();
                        if new == quote {
                            bail!("the new text is the same as the old text");
                        }
                        (who.actor(), line, quote, new, false, why.clone())
                    }
                    Propose::Delete { target, why, who, buf, .. } => {
                        let (quote, line) = find_target(&buf.text(tx)?, target)?;
                        (who.actor(), line, quote, String::new(), false, why.clone())
                    }
                    Propose::Insert { after_line, after, text, tight, why, who, .. } => {
                        let n = anchor::line_starts(&tx.doc).len();
                        let line = match (after_line, after) {
                            (Some(l), None) if *l <= n => l.checked_sub(1),
                            (Some(l), None) => bail!("--after-line {l} is out of range: the document has {n} lines"),
                            (None, Some(a)) => {
                                let t = Target { text: Some(a.clone()), lines: None, near_line: None };
                                Some(find_target(&tx.doc, &t)?.1)
                            }
                            _ => bail!("give one of --after-line or --after"),
                        };
                        // Anchor on the last line with text at or before the given line.
                        let lines: Vec<&str> = tx.doc.lines().collect();
                        let at = line.and_then(|l| {
                            (0..=l.min(lines.len().saturating_sub(1))).rev().find(|i| lines.get(*i).is_some_and(|x| !x.trim().is_empty()))
                        });
                        let quote = at.map(|l| lines[l].to_string()).unwrap_or_default();
                        let new = text_arg(text)?.trim_matches('\n').to_string();
                        if new.trim().is_empty() {
                            bail!("--text is empty");
                        }
                        (who.actor(), at.unwrap_or(0), quote, new, *tight, why.clone())
                    }
                };
                let eid = tx.log(&who, What::Proposed { kind, line, quote, new, tight, why });
                Ok((num(&tx.state, &eid), line))
            })?;
            println!("proposed {} #{id} at L{}. The human decides. Run `redline wait FILE` for the decision.", kind.name(), line + 1);
            Ok(())
        }
        Cmd::Comment { file, target, body, who, buf } => {
            let store = DocStore::open(&file)?;
            let who = who.actor();
            let (id, ..) = store.transact(|tx| {
                let (quote, line) = find_target(&buf.text(tx)?, &target)?;
                let body = text_arg(&body)?.trim().to_string();
                if body.is_empty() {
                    bail!("--body is empty");
                }
                let eid = tx.log(&who, What::Commented { line, quote, body });
                Ok(num(&tx.state, &eid))
            })?;
            if buf.json {
                println!("{}", json!({ "id": id }));
            } else {
                println!("commented #{id}");
            }
            Ok(())
        }
        Cmd::Reply { file, id, body, who } => {
            let store = DocStore::open(&file)?;
            let who = who.actor();
            store.transact(|tx| {
                let body = text_arg(&body)?.trim().to_string();
                if body.is_empty() {
                    bail!("--body is empty");
                }
                let thread = open_thread(&tx.state, id)?.eid.clone();
                tx.log(&who, What::Replied { thread, body });
                Ok(())
            })?;
            println!("replied on #{id}");
            Ok(())
        }
        Cmd::Accept { file, id, new, who, buf } => {
            let mut store = DocStore::open(&file)?;
            let who = who.human("accept")?;
            store.file_actor = who.clone();
            let (edit, ..) = store.transact(|tx| {
                let mut t = open_thread(&tx.state, id)?.clone();
                if !t.kind.is_proposal() {
                    bail!("#{id} is a comment. Use `redline resolve`");
                }
                let modified = new.is_some();
                if let Some(n) = &new {
                    t.new = text_arg(n)?.trim_matches('\n').to_string();
                }
                let text = buf.text(tx)?;
                let (range, repl) = t
                    .apply(&text)
                    .context("the text that this proposal points at is not in the document. Reject the proposal")?;
                let mut after = text.clone();
                after.replace_range(range, &repl);
                let edit = hunk(&text, &after);
                tx.log(&who, What::Accepted { thread: t.eid.clone(), modified, new: t.new.clone(), note: None });
                if !buf.stdin {
                    tx.set_doc(after, "accept", &who);
                }
                Ok(edit)
            })?;
            if buf.json {
                // The edit as whole lines: replace `remove` lines at `line` with `insert`.
                let edit = edit.map(|(line, old, new)| {
                    json!({ "line": line, "remove": old.split_inclusive('\n').count(), "insert": new.lines().collect::<Vec<_>>() })
                });
                println!("{}", json!({ "id": id, "applied": !buf.stdin, "edit": edit }));
            } else {
                println!("accepted #{id}");
            }
            Ok(())
        }
        Cmd::Reject { file, id, note, who } => {
            let store = DocStore::open(&file)?;
            let who = who.human("reject")?;
            store.transact(|tx| {
                let t = open_thread(&tx.state, id)?;
                if !t.kind.is_proposal() {
                    bail!("#{id} is a comment. Use `redline resolve`");
                }
                let thread = t.eid.clone();
                let note = note.clone().map(|n| n.trim().to_string()).filter(|n| !n.is_empty());
                tx.log(&who, What::Rejected { thread, note });
                Ok(())
            })?;
            println!("rejected #{id}");
            Ok(())
        }
        Cmd::Resolve { file, id, who } => {
            let store = DocStore::open(&file)?;
            let who = who.human("resolve")?;
            store.transact(|tx| {
                let t = open_thread(&tx.state, id)?;
                if t.kind.is_proposal() {
                    bail!("#{id} is a proposal. Use `redline accept` or `redline reject`");
                }
                let thread = t.eid.clone();
                tx.log(&who, What::Resolved { thread });
                Ok(())
            })?;
            println!("resolved #{id}");
            Ok(())
        }
        Cmd::Reopen { file, id, who } => {
            let store = DocStore::open(&file)?;
            let who = who.human("reopen")?;
            store.transact(|tx| {
                let t = tx.state.thread(id)?;
                if t.status == Status::Open {
                    bail!("thread #{id} is open");
                }
                let thread = t.eid.clone();
                tx.log(&who, What::Reopened { thread });
                Ok(())
            })?;
            println!("reopened #{id}");
            Ok(())
        }
        Cmd::Sync { file, who } => {
            let mut store = DocStore::open(&file)?;
            store.file_actor = who.human("sync")?;
            store.transact(|_| Ok(()))?;
            Ok(())
        }
        Cmd::Log { file, n, json } => {
            let store = DocStore::open(&file)?;
            let (_, st, _) = store.transact(|_| Ok(()))?;
            let skip = n.map(|n| st.events.len().saturating_sub(n)).unwrap_or(0);
            if json {
                println!("{}", Value::Array(st.events[skip..].iter().map(|e| event_json(e, &st)).collect()));
            } else {
                st.events[skip..].iter().for_each(|e| print_event(e, &st));
            }
            Ok(())
        }
        Cmd::Watch { file } => {
            let store = DocStore::open(&file)?;
            let mut seen: std::collections::HashSet<String> = store.events().into_iter().map(|e| e.id).collect();
            let mut stamp = store.stamp();
            let out = std::io::stdout();
            loop {
                std::thread::sleep(Duration::from_millis(200));
                let now = store.stamp();
                if now == stamp {
                    continue;
                }
                stamp = now;
                let st = store.load();
                for e in st.events.iter().filter(|e| !seen.contains(&e.id)) {
                    let mut o = out.lock();
                    if writeln!(o, "{}", event_json(e, &st)).and_then(|_| o.flush()).is_err() {
                        return Ok(()); // the reader went away
                    }
                }
                seen.extend(st.events.into_iter().map(|e| e.id));
            }
        }
        Cmd::Wait { file, timeout, json } => {
            let store = DocStore::open(&file)?;
            let mark = store.dir.join("agent-seen");
            let seen: u64 = std::fs::read_to_string(&mark).ok().and_then(|s| s.trim().parse().ok()).unwrap_or(0);
            let start = Instant::now();
            loop {
                let (_, st, doc) = store.transact(|_| Ok(()))?;
                let fresh: Vec<&Event> = st
                    .events
                    .iter()
                    .filter(|e| e.lamport > seen && !e.agent && !matches!(e.what, What::Tracked { .. }))
                    .collect();
                if !fresh.is_empty() {
                    if json {
                        println!("{}", Value::Array(fresh.iter().map(|e| event_json(e, &st)).collect()));
                    } else {
                        fresh.iter().for_each(|e| print_event(e, &st));
                        let open = st.threads.iter().filter(|t| t.status == Status::Open).count();
                        println!("\n{open} open threads, {} lines. Run `redline show` for the current text.", anchor::line_starts(&doc).len());
                    }
                    std::fs::write(&mark, st.lamport.to_string())?;
                    return Ok(());
                }
                if start.elapsed() >= Duration::from_secs(timeout) {
                    println!("no human activity in {timeout}s. Run `redline wait` again, or continue with other work.");
                    return Ok(());
                }
                std::thread::sleep(Duration::from_millis(400));
            }
        }
        Cmd::Merge { file, logs } => {
            let store = DocStore::open(&file)?;
            let mut foreign: Vec<Event> = vec![];
            for log in &logs {
                let s = std::fs::read_to_string(log).with_context(|| format!("cannot read {}", log.display()))?;
                foreign.extend(s.lines().filter_map(|l| serde_json::from_str::<Event>(l).ok()));
            }
            let n = store.merge(foreign)?;
            println!("merged {n} new events");
            Ok(())
        }
        Cmd::Docs => {
            for (path, dir) in tracked() {
                println!("{path}\n    {}", dir.display());
            }
            Ok(())
        }
        Cmd::Skill { what: SkillCmd::Show } => {
            print!("{SKILL}");
            Ok(())
        }
        Cmd::Skill { what: SkillCmd::Install { dir } } => {
            let dir = dir.unwrap_or_else(|| {
                PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".claude/skills/redline")
            });
            std::fs::create_dir_all(&dir)?;
            std::fs::write(dir.join("SKILL.md"), SKILL)?;
            println!("wrote {}", dir.join("SKILL.md").display());
            Ok(())
        }
    }
}
