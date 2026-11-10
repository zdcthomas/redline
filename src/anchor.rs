//! Text anchors. Threads point at the document by quoted text plus a line
//! hint, so they survive edits elsewhere in the file.

use std::ops::Range;

/// Byte offset of the start of every line.
pub fn line_starts(doc: &str) -> Vec<usize> {
    let mut v = vec![0];
    v.extend(doc.match_indices('\n').map(|(i, _)| i + 1));
    if v.len() > 1 && *v.last().unwrap() == doc.len() {
        v.pop(); // a trailing newline does not start a new line
    }
    v
}

/// 0-based line that holds byte `pos`.
pub fn line_of(doc: &str, pos: usize) -> usize {
    doc.as_bytes()[..pos.min(doc.len())]
        .iter()
        .filter(|b| **b == b'\n')
        .count()
}

/// (0-based line, byte column) of byte `pos`.
pub fn pos(doc: &str, pos: usize) -> (usize, usize) {
    let pos = pos.min(doc.len());
    let start = doc[..pos].rfind('\n').map(|i| i + 1).unwrap_or(0);
    (line_of(doc, pos), pos - start)
}

/// Byte range of lines `l0..=l1`, without the final newline.
pub fn lines_range(doc: &str, l0: usize, l1: usize) -> Range<usize> {
    let ls = line_starts(doc);
    let s = ls[l0.min(ls.len() - 1)];
    let e = match ls.get(l1 + 1) {
        Some(n) => n - 1,
        None => doc.trim_end_matches('\n').len().max(s),
    };
    s..e.max(s)
}

/// All places where `quote` occurs, as (byte offset, line).
pub fn occurrences(doc: &str, quote: &str, whole_line: bool) -> Vec<(usize, usize)> {
    if quote.is_empty() {
        return vec![];
    }
    let bytes = doc.as_bytes();
    let (mut line, mut last) = (0, 0);
    let mut out = vec![];
    for (i, _) in doc.match_indices(quote) {
        let end = i + quote.len();
        if whole_line
            && !((i == 0 || bytes[i - 1] == b'\n') && (end == doc.len() || bytes[end] == b'\n'))
        {
            continue;
        }
        line += bytes[last..i].iter().filter(|b| **b == b'\n').count();
        last = i;
        out.push((i, line));
    }
    out
}

/// Find `quote` in `doc`; with several matches, take the one nearest `hint`.
/// An empty quote means "top of the document".
pub fn resolve(doc: &str, quote: &str, hint: usize, whole_line: bool) -> Option<Range<usize>> {
    if quote.is_empty() {
        return Some(0..0);
    }
    occurrences(doc, quote, whole_line)
        .into_iter()
        .min_by_key(|(_, l)| l.abs_diff(hint))
        .map(|(i, _)| i..i + quote.len())
}

fn blank(doc: &str, ls: &[usize], l: usize) -> bool {
    let e = ls.get(l + 1).copied().unwrap_or(doc.len());
    doc[ls[l]..e].trim().is_empty()
}

/// The edit that inserts `text` as its own block after line `after`
/// (`None` = top). It normalizes the blank lines around the new block.
pub fn block_insert(doc: &str, after: Option<usize>, text: &str) -> (Range<usize>, String) {
    let ls = line_starts(doc);
    let text = text.trim_matches('\n');
    if doc.trim().is_empty() {
        return (0..doc.len(), format!("{text}\n"));
    }
    let n = ls.len();
    // a: end of the last non-blank line at or before `after`
    let a = after
        .and_then(|after| (0..=after.min(n - 1)).rev().find(|l| !blank(doc, &ls, *l)))
        .map(|l| lines_range(doc, l, l).end);
    // b: start of the first non-blank line after `after`
    let from = after.map(|x| x + 1).unwrap_or(0);
    let b = (from..n).find(|l| !blank(doc, &ls, *l)).map(|l| ls[l]);
    let range = a.unwrap_or(0)..b.unwrap_or(doc.len());
    let pre = if a.is_some() { "\n\n" } else { "" };
    let post = if b.is_some() { "\n\n" } else { "\n" };
    (range, format!("{pre}{text}{post}"))
}

/// The edit that removes lines `l0..=l1` and does not leave a double blank line.
pub fn delete_lines(doc: &str, l0: usize, l1: usize) -> (Range<usize>, String) {
    let ls = line_starts(doc);
    let n = ls.len();
    let s = ls[l0];
    let mut next = l1 + 1;
    let before_blank = l0 == 0 || blank(doc, &ls, l0 - 1);
    if before_blank && next < n && blank(doc, &ls, next) {
        next += 1;
    }
    let e = ls.get(next).copied().unwrap_or(doc.len());
    (s..e, String::new())
}

/// Line ranges of the blocks (paragraphs) of a Markdown document. A fenced
/// code block is one block, also with blank lines in it.
pub fn blocks(doc: &str) -> Vec<(usize, usize)> {
    let mut out = vec![];
    let mut start: Option<usize> = None;
    let mut fence = false;
    let mut last = 0;
    for (i, line) in doc.lines().enumerate() {
        last = i;
        let t = line.trim_start();
        let is_fence = t.starts_with("```") || t.starts_with("~~~");
        if fence {
            if is_fence {
                fence = false;
            }
            continue;
        }
        if is_fence {
            fence = true;
            start.get_or_insert(i);
            continue;
        }
        if t.is_empty() {
            if let Some(s) = start.take() {
                out.push((s, i - 1));
            }
        } else {
            start.get_or_insert(i);
        }
    }
    if let Some(s) = start {
        out.push((s, last));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_takes_nearest() {
        let d = "foo\nbar\nfoo\n";
        assert_eq!(resolve(d, "foo", 2, false), Some(8..11));
        assert_eq!(resolve(d, "foo", 0, false), Some(0..3));
        assert_eq!(resolve(d, "zip", 0, false), None);
    }

    #[test]
    fn insert_block_normalizes_blank_lines() {
        let d = "a\n\nb\n";
        let (r, t) = block_insert(d, Some(0), "x");
        let mut s = d.to_string();
        s.replace_range(r, &t);
        assert_eq!(s, "a\n\nx\n\nb\n");
        let (r, t) = block_insert(d, Some(2), "x");
        let mut s = d.to_string();
        s.replace_range(r, &t);
        assert_eq!(s, "a\n\nb\n\nx\n");
        let (r, t) = block_insert(d, None, "x");
        let mut s = d.to_string();
        s.replace_range(r, &t);
        assert_eq!(s, "x\n\na\n\nb\n");
    }

    #[test]
    fn delete_keeps_one_blank() {
        let d = "a\n\nb\n\nc\n";
        let (r, t) = delete_lines(d, 2, 2);
        let mut s = d.to_string();
        s.replace_range(r, &t);
        assert_eq!(s, "a\n\nc\n");
    }

    #[test]
    fn blocks_keep_fences_whole() {
        let d = "# h\n\np1\np2\n\n```\ncode\n\nmore\n```\n";
        assert_eq!(blocks(d), vec![(0, 0), (2, 3), (5, 9)]);
    }
}
