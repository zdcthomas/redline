//! The command-line interface. The TUI is for the human. All other commands
//! are for an agent: they read the document, and they add proposals and
//! comments. No command accepts, rejects or edits: only the human does that.

use crate::anchor;
use crate::store::{Actor, DocStore, Event, Kind, Reply, Status, Thread, What, ago, tracked};
use anyhow::{Context, Result, bail};
use chrono::{Local, Utc};
use clap::{Args, Parser, Subcommand};
use std::io::Read;
use std::path::PathBuf;
use std::time::{Duration, Instant};

const SKILL: &str = include_str!("../skill/redline/SKILL.md");

#[derive(Parser)]
#[command(
    name = "redline",
    version,
    about = "A keyboard-driven spec editor. The human writes; agents suggest.",
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
    /// Author name for the history. Default: $REDLINE_AUTHOR, then "agent".
    #[arg(long = "as", global = true)]
    author: Option<String>,
}

impl Who {
    fn actor(&self) -> Actor {
        let name = self
            .author
            .clone()
            .or_else(|| std::env::var("REDLINE_AUTHOR").ok())
            .unwrap_or_else(|| "agent".into());
        Actor { name, agent: true }
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
    /// List the threads (comments and proposals).
    Threads {
        file: PathBuf,
        /// Include the accepted, rejected and resolved threads.
        #[arg(long)]
        all: bool,
        #[arg(long)]
        json: bool,
    },
    /// Propose a change. The human accepts or rejects it in the TUI.
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
    /// Print the history of changes and decisions.
    Log {
        file: PathBuf,
        /// Show only the last N events.
        #[arg(short)]
        n: Option<usize>,
        #[arg(long)]
        json: bool,
    },
    /// Block until the human does something (a decision, a comment, an edit),
    /// then print the new events.
    Wait {
        file: PathBuf,
        /// Seconds to wait.
        #[arg(long, default_value_t = 100)]
        timeout: u64,
        #[arg(long)]
        json: bool,
    },
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

pub fn new_thread(kind: Kind, who: &Actor, quote: String, line: usize) -> Thread {
    Thread {
        id: 0,
        kind,
        status: Status::Open,
        author: who.name.clone(),
        agent: who.agent,
        at: Utc::now(),
        quote,
        line,
        new: String::new(),
        tight: false,
        body: String::new(),
        replies: vec![],
        decided_at: None,
        decided_by: None,
        note: None,
    }
}

/// Add a thread in a transaction; returns its id.
pub fn add_thread(tx: &mut crate::store::Tx, who: &Actor, mut t: Thread) -> u64 {
    t.id = tx.state.next_id.max(1);
    tx.state.next_id = t.id + 1;
    let what = if t.kind == Kind::Comment {
        What::Commented { id: t.id, line: t.line, quote: t.quote.clone(), body: t.body.clone() }
    } else {
        What::Proposed {
            id: t.id,
            kind: t.kind,
            line: t.line,
            old: t.quote.clone(),
            new: t.new.clone(),
            why: t.body.clone(),
        }
    };
    tx.log(who, what);
    let id = t.id;
    tx.state.threads.push(t);
    id
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

fn print_event(e: &Event) {
    let (head, detail) = e.describe();
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
                println!("{}", serde_json::to_string_pretty(&serde_json::json!({
                    "path": store.doc, "lines": lines, "open_threads": open,
                }))?);
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
        Cmd::Threads { file, all, json } => {
            let store = DocStore::open(&file)?;
            let (_, state, doc) = store.transact(|_| Ok(()))?;
            let list: Vec<&Thread> =
                state.threads.iter().filter(|t| all || t.status == Status::Open).collect();
            if json {
                println!("{}", serde_json::to_string_pretty(&list)?);
            } else if list.is_empty() {
                println!("no threads");
            } else {
                for t in list {
                    print_thread(t, &doc);
                }
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
                let (who, t) = match &what {
                    Propose::Edit { target, new, why, who, .. } => {
                        let who = who.actor();
                        let (quote, line) = find_target(&tx.doc, target)?;
                        let new = text_arg(new)?.trim_matches('\n').to_string();
                        if new == quote {
                            bail!("the new text is the same as the old text");
                        }
                        let mut t = new_thread(kind, &who, quote, line);
                        t.new = new;
                        t.body = why.clone();
                        (who, t)
                    }
                    Propose::Delete { target, why, who, .. } => {
                        let who = who.actor();
                        let (quote, line) = find_target(&tx.doc, target)?;
                        let mut t = new_thread(kind, &who, quote, line);
                        t.body = why.clone();
                        (who, t)
                    }
                    Propose::Insert { after_line, after, text, tight, why, who, .. } => {
                        let who = who.actor();
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
                        let anchor_line = line.and_then(|l| (0..=l).rev().find(|i| !lines[*i].trim().is_empty()));
                        let quote = anchor_line.map(|l| lines[l].to_string()).unwrap_or_default();
                        let mut t = new_thread(kind, &who, quote, anchor_line.unwrap_or(0));
                        t.new = text_arg(text)?.trim_matches('\n').to_string();
                        if t.new.trim().is_empty() {
                            bail!("--text is empty");
                        }
                        t.tight = *tight;
                        t.body = why.clone();
                        (who, t)
                    }
                };
                let line = t.line;
                Ok((add_thread(tx, &who, t), line))
            })?;
            println!("proposed {} #{id} at L{}. The human decides in the TUI. Run `redline wait FILE` for the decision.", kind.name(), line + 1);
            Ok(())
        }
        Cmd::Comment { file, target, body, who } => {
            let store = DocStore::open(&file)?;
            let who = who.actor();
            let (id, ..) = store.transact(|tx| {
                let (quote, line) = find_target(&tx.doc, &target)?;
                let mut t = new_thread(Kind::Comment, &who, quote, line);
                t.body = text_arg(&body)?.trim().to_string();
                if t.body.is_empty() {
                    bail!("--body is empty");
                }
                Ok(add_thread(tx, &who, t))
            })?;
            println!("commented #{id}");
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
                let t = tx.state.thread(id)?;
                if t.status != Status::Open {
                    bail!("thread #{id} is {}", t.status.name());
                }
                t.replies.push(Reply { author: who.name.clone(), agent: true, at: Utc::now(), body: body.clone() });
                tx.log(&who, What::Replied { id, body });
                Ok(())
            })?;
            println!("replied on #{id}");
            Ok(())
        }
        Cmd::Log { file, n, json } => {
            let store = DocStore::open(&file)?;
            store.transact(|_| Ok(()))?;
            let h = store.history()?;
            let skip = n.map(|n| h.len().saturating_sub(n)).unwrap_or(0);
            if json {
                println!("{}", serde_json::to_string_pretty(&h[skip..])?);
            } else {
                h[skip..].iter().for_each(print_event);
            }
            Ok(())
        }
        Cmd::Wait { file, timeout, json } => {
            let store = DocStore::open(&file)?;
            let start = Instant::now();
            loop {
                let (_, state, doc) = store.transact(|_| Ok(()))?;
                let h = store.history()?;
                let fresh: Vec<&Event> = h
                    .iter()
                    .skip(state.agent_seen)
                    .filter(|e| !e.agent && !matches!(e.what, What::Tracked { .. }))
                    .collect();
                if !fresh.is_empty() {
                    if json {
                        println!("{}", serde_json::to_string_pretty(&fresh)?);
                    } else {
                        fresh.iter().for_each(|e| print_event(e));
                        let open = state.threads.iter().filter(|t| t.status == Status::Open).count();
                        println!("\n{open} open threads, {} lines. Run `redline show` for the current text.", anchor::line_starts(&doc).len());
                    }
                    let seen = h.len();
                    store.transact(|tx| {
                        tx.state.agent_seen = seen;
                        Ok(())
                    })?;
                    return Ok(());
                }
                if start.elapsed() >= Duration::from_secs(timeout) {
                    println!("no human activity in {timeout}s. Run `redline wait` again, or continue with other work.");
                    return Ok(());
                }
                std::thread::sleep(Duration::from_millis(400));
            }
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
