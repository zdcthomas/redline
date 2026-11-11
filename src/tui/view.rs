//! Turns the document plus the open threads into styled, wrapped rows.
//! Proposals show inline like suggestions in a word processor: removed text
//! is red and struck through, added text is green.

use super::theme::*;
use crate::anchor;
use crate::store::{Kind, Status, Thread};
use ratatui::style::{Modifier, Style};
use ratatui::text::Span;
use std::collections::HashMap;
use std::ops::Range;
use unicode_width::UnicodeWidthChar;

/// Where the open threads are in the document.
#[derive(Default)]
pub struct Index {
    /// Open thread ids in document order; threads with a lost anchor are last.
    pub order: Vec<u64>,
    /// First line of each thread that has an anchor.
    pub thread_line: HashMap<u64, usize>,
    /// Threads that touch each line.
    pub line_threads: HashMap<usize, Vec<u64>>,
    pub ranges: HashMap<u64, Range<usize>>,
}

pub fn index(doc: &str, threads: &[Thread]) -> Index {
    let mut ix = Index::default();
    let mut located: Vec<(&Thread, Option<Range<usize>>)> = threads
        .iter()
        .filter(|t| t.status == Status::Open)
        .map(|t| (t, t.locate(doc)))
        .collect();
    located.sort_by_key(|(t, r)| (r.as_ref().map(|r| r.start).unwrap_or(usize::MAX), t.id));
    for (t, r) in located {
        ix.order.push(t.id);
        let Some(r) = r else { continue };
        let l0 = anchor::line_of(doc, r.start);
        let l1 = anchor::line_of(doc, r.end);
        ix.thread_line.insert(t.id, l0);
        for l in l0..=l1 {
            ix.line_threads.entry(l).or_default().push(t.id);
        }
        ix.ranges.insert(t.id, r);
    }
    ix
}

pub struct Row {
    /// Source line that this row belongs to.
    pub src: usize,
    /// First row of the source line.
    pub first: bool,
    pub spans: Vec<Span<'static>>,
}

#[derive(Default)]
pub struct View {
    pub rows: Vec<Row>,
    /// (first row, row count) of each source line.
    pub line_rows: Vec<(usize, usize)>,
}

struct Claim {
    id: u64,
    kind: Kind,
    hl: Range<usize>,
}

type Cells = Vec<(char, Style)>;

/// `sel` is the selected text and `cur` is the word below the word cursor,
/// both as byte ranges of the document.
pub fn build(
    doc: &str,
    threads: &[Thread],
    ix: &Index,
    active: Option<u64>,
    width: usize,
    sel: Option<&Range<usize>>,
    cur: Option<&Range<usize>>,
) -> View {
    let ls = anchor::line_starts(doc);
    let mut claims: Vec<Claim> = vec![];
    let mut injects: HashMap<usize, Vec<(u64, String)>> = HashMap::new();
    for id in &ix.order {
        let (Some(r), Some(t)) = (ix.ranges.get(id), threads.iter().find(|t| t.id == *id)) else {
            continue;
        };
        if t.kind == Kind::Insert {
            let gap = if t.tight { "" } else { "\n" };
            let text = if t.quote.is_empty() {
                format!("{}\n{gap}", t.new)
            } else {
                format!("\n{gap}{}", t.new)
            };
            injects.entry(r.end).or_default().push((t.id, text));
            continue;
        }
        if claims.iter().any(|c| c.hl.start < r.end && r.start < c.hl.end) {
            continue; // overlaps a thread that is already drawn; the sidebar still shows it
        }
        claims.push(Claim { id: t.id, kind: t.kind, hl: r.clone() });
        if t.kind == Kind::Edit {
            injects.entry(r.end).or_default().push((t.id, t.new.clone()));
        }
    }

    let mut view = View::default();
    let mut fence = false;
    for i in 0..ls.len() {
        let r = anchor::lines_range(doc, i, i);
        let text = &doc[r.clone()];
        let styles = md_styles(text, &mut fence);
        let here: Vec<&Claim> = claims.iter().filter(|c| c.hl.start <= r.end && r.start < c.hl.end).collect();
        let mut logical: Vec<Cells> = vec![vec![]];
        let emit = |pos: usize, logical: &mut Vec<Cells>| {
            for (id, text) in injects.get(&pos).into_iter().flatten() {
                let mut st = Style::new().fg(ADD);
                if active == Some(*id) {
                    st = st.bg(ADD_BG).add_modifier(Modifier::BOLD);
                }
                for ch in text.chars() {
                    if ch == '\n' {
                        logical.push(vec![]);
                    } else {
                        logical.last_mut().unwrap().push((ch, st));
                    }
                }
            }
        };
        emit(r.start, &mut logical);
        for (k, (off, ch)) in text.char_indices().enumerate() {
            let b = r.start + off;
            if off > 0 {
                emit(b, &mut logical);
            }
            let mut st = styles[k];
            if let Some(c) = here.iter().find(|c| c.hl.contains(&b)) {
                let on = active == Some(c.id);
                st = match c.kind {
                    Kind::Comment => st.bg(if on { NOTE_BG_ACTIVE } else { NOTE_BG }),
                    _ => {
                        let s = st.fg(DEL).add_modifier(Modifier::CROSSED_OUT);
                        if on { s.bg(DEL_BG) } else { s }
                    }
                };
            }
            if sel.is_some_and(|r| r.contains(&b)) {
                st = st.bg(SELECT_BG);
            }
            if cur.is_some_and(|r| r.contains(&b)) {
                st = st.bg(H2).fg(ratatui::style::Color::Rgb(18, 18, 24));
            }
            let row = logical.last_mut().unwrap();
            if ch == '\t' {
                row.extend([(' ', st); 4]);
            } else {
                row.push((ch, st));
            }
        }
        if !text.is_empty() {
            emit(r.end, &mut logical);
        }

        let start = view.rows.len();
        for cells in &logical {
            for part in wrap(cells, width) {
                view.rows.push(Row { src: i, first: view.rows.len() == start, spans: to_spans(&part) });
            }
        }
        view.line_rows.push((start, view.rows.len() - start));
    }
    view
}

fn to_spans(cells: &[(char, Style)]) -> Vec<Span<'static>> {
    let mut out: Vec<Span<'static>> = vec![];
    let mut cur = String::new();
    let mut st = Style::new();
    for (ch, s) in cells {
        if *s != st && !cur.is_empty() {
            out.push(Span::styled(std::mem::take(&mut cur), st));
        }
        st = *s;
        cur.push(*ch);
    }
    if !cur.is_empty() {
        out.push(Span::styled(cur, st));
    }
    out
}

pub fn wrap(cells: &[(char, Style)], width: usize) -> Vec<Cells> {
    let cw = |c: char| c.width().unwrap_or(0);
    let mut rows = vec![];
    let (mut start, mut w, mut space, mut i) = (0, 0, None, 0);
    while i < cells.len() {
        let c = cw(cells[i].0);
        if width > 0 && w + c > width && i > start {
            let brk = match space {
                Some(s) if s >= start => s + 1,
                _ => i,
            };
            rows.push(cells[start..brk].to_vec());
            start = brk;
            w = cells[start..i].iter().map(|x| cw(x.0)).sum();
            space = None;
            continue;
        }
        if cells[i].0 == ' ' {
            space = Some(i);
        }
        w += c;
        i += 1;
    }
    rows.push(cells[start..].to_vec());
    rows
}

/// Word-wrap plain text.
pub fn wrap_str(s: &str, width: usize) -> Vec<String> {
    let mut out = vec![];
    for line in s.lines() {
        let cells: Cells = line.chars().map(|c| (c, Style::new())).collect();
        for part in wrap(&cells, width) {
            out.push(part.iter().map(|x| x.0).collect::<String>().trim_end().to_string());
        }
    }
    out
}

/// A style for every char of a Markdown line. Light-weight: headings, code
/// fences, quotes, list markers, `code` and **bold**.
fn md_styles(text: &str, fence: &mut bool) -> Vec<Style> {
    let n = text.chars().count();
    let t = text.trim_start();
    let indent = n - t.chars().count();
    if t.starts_with("```") || t.starts_with("~~~") {
        *fence = !*fence;
        return vec![fg(DIM); n];
    }
    if *fence {
        return vec![fg(CODE); n];
    }
    let hashes = t.chars().take_while(|c| *c == '#').count();
    let base = if (1..=6).contains(&hashes) && t[hashes..].starts_with(' ') {
        match hashes {
            1 => bold(ACCENT),
            2 => bold(H2),
            _ => bold(H3),
        }
    } else if t.starts_with('>') {
        fg(DIM).add_modifier(Modifier::ITALIC)
    } else if t.len() >= 3 && (t.chars().all(|c| c == '-') || t.chars().all(|c| c == '*')) {
        fg(FAINT)
    } else {
        fg(TEXT)
    };
    let mut st = vec![base; n];

    // list marker and checkbox
    let tb = t.as_bytes();
    let digits = t.chars().take_while(|c| c.is_ascii_digit()).count();
    let marker = if tb.len() >= 2 && matches!(tb[0], b'-' | b'*' | b'+') && tb[1] == b' ' {
        1
    } else if digits > 0 && t[digits..].starts_with(". ") {
        digits + 1
    } else {
        0
    };
    if marker > 0 {
        for s in &mut st[indent..indent + marker] {
            *s = bold(ACCENT);
        }
        let rest = &t[marker..];
        let boxc = if rest.starts_with(" [ ] ") {
            Some(NOTE)
        } else if rest.starts_with(" [x] ") || rest.starts_with(" [X] ") {
            Some(ADD)
        } else {
            None
        };
        if let Some(c) = boxc {
            for s in &mut st[indent + marker + 1..indent + marker + 4] {
                *s = fg(c);
            }
        }
    }

    // inline `code` and **bold**
    let chars: Vec<char> = text.chars().collect();
    let (mut code, mut strong, mut i) = (false, false, 0);
    while i < n {
        if chars[i] == '`' {
            code = !code;
            st[i] = fg(DIM);
        } else if code {
            st[i] = st[i].fg(CODE);
        } else if chars[i] == '*' && chars.get(i + 1) == Some(&'*') {
            strong = !strong;
            st[i] = fg(DIM);
            st[i + 1] = fg(DIM);
            i += 1;
        } else if strong {
            st[i] = st[i].add_modifier(Modifier::BOLD).fg(ratatui::style::Color::White);
        }
        i += 1;
    }
    st
}
