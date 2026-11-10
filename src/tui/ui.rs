//! Drawing.

use super::theme::*;
use super::view::{self, wrap_str};
use super::{App, Mode};
use crate::store::{Kind, Thread, ago};
use chrono::Local;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

fn clip(mut v: Vec<String>, max: usize) -> Vec<String> {
    if v.len() > max {
        v.truncate(max);
        if let Some(l) = v.last_mut() {
            l.push_str(" …");
        }
    }
    v
}

impl App {
    pub(super) fn render(&mut self, f: &mut Frame) {
        let area = f.area();
        let [top, body, status] =
            Layout::vertical([Constraint::Length(1), Constraint::Min(1), Constraint::Length(1)]).areas(area);
        let side_w = if self.show_side && area.width >= 84 { (area.width * 36 / 100).clamp(34, 56) } else { 0 };
        let [doc, side] = Layout::horizontal([Constraint::Min(10), Constraint::Length(side_w)]).areas(body);
        self.page = body.height as usize;
        self.draw_top(f, top);
        if self.mode == Mode::History {
            self.draw_history(f, body);
        } else {
            self.draw_doc(f, doc);
            if side_w > 0 {
                self.draw_side(f, side);
            }
        }
        self.draw_status(f, status);
        if self.mode == Mode::Help {
            self.draw_help(f, area);
        }
        if self.input.is_some() {
            self.draw_input(f, body);
        }
        if self.session.is_some() {
            self.draw_term(f, area);
        }
    }

    fn draw_top(&self, f: &mut Frame, area: Rect) {
        let buf = f.buffer_mut();
        buf.set_style(area, Style::new().bg(BAR_BG));
        let name = self.store.doc.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let mut dir = self.store.doc.parent().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
        if let Ok(home) = std::env::var("HOME") {
            if dir.starts_with(&home) {
                dir = dir.replacen(&home, "~", 1);
            }
        }
        let left = Line::from(vec![
            Span::styled(" ▌", fg(ACCENT)),
            Span::styled("redline", bold(ACCENT)),
            Span::styled(format!("   {name}"), bold(TEXT)),
            Span::styled(format!("  {dir}"), fg(DIM)),
        ]);
        let open = self.ix.order.len();
        let decided = self.state.threads.len() - open;
        let right = format!("  {open} open · {decided} decided ");
        let w = (right.width() as u16).min(area.width);
        buf.set_line(area.x, area.y, &left, area.width);
        buf.set_string(area.right() - w, area.y, right, Style::new().fg(DIM).bg(BAR_BG));
    }

    fn draw_doc(&mut self, f: &mut Frame, area: Rect) {
        let numw = self.n_lines.to_string().len().max(2);
        let gutter = numw + 5;
        let width = (area.width as usize).saturating_sub(gutter + 1).max(8);
        let key = (self.version, width, self.active);
        if key != self.view_key {
            self.view = view::build(&self.doc, &self.state.threads, &self.ix, self.active, width);
            self.view_key = key;
        }
        let h = area.height as usize;
        let rows = &self.view.rows;
        let (r0, cnt) = self.view.line_rows[self.cursor];
        let so = 3.min(h / 4);
        if r0 < self.scroll + so {
            self.scroll = r0.saturating_sub(so);
        }
        if r0 + cnt + so > self.scroll + h {
            self.scroll = (r0 + cnt + so).saturating_sub(h);
        }
        self.scroll = self.scroll.min(rows.len().saturating_sub(h)).min(r0);

        let target = self.target();
        let sel = self.sel.map(|s| (s.min(self.cursor), s.max(self.cursor)));
        let buf = f.buffer_mut();
        for y in 0..h {
            let Some(row) = rows.get(self.scroll + y) else { break };
            let line = row.src;
            let yy = area.y + y as u16;
            let is_cur = line == self.cursor;
            let in_sel = sel.is_some_and(|(a, b)| (a..=b).contains(&line));
            if in_sel {
                buf.set_style(Rect::new(area.x, yy, area.width, 1), Style::new().bg(SELECT_BG));
            } else if is_cur {
                buf.set_style(Rect::new(area.x, yy, area.width, 1), Style::new().bg(CURSOR_BG));
            }
            if row.first {
                let st = if is_cur { bold(TEXT) } else { fg(FAINT) };
                buf.set_string(area.x + 1, yy, format!("{:>numw$}", line + 1), st);
                if let Some(ids) = self.ix.line_threads.get(&line) {
                    let id = self.active.filter(|a| ids.contains(a)).unwrap_or(ids[0]);
                    if let Some(t) = self.state.threads.iter().find(|t| t.id == id) {
                        let st = if self.active == Some(id) { bold(kind_color(t.kind)) } else { fg(kind_color(t.kind)) };
                        buf.set_string(area.x + numw as u16 + 3, yy, kind_icon(t.kind), st);
                    }
                }
            }
            let bx = area.x + numw as u16 + 2;
            if is_cur {
                buf.set_string(bx, yy, "▌", fg(ACCENT));
            } else if target.is_some_and(|(a, b)| (a..=b).contains(&line)) {
                buf.set_string(bx, yy, "▏", fg(Color::Rgb(150, 70, 70)));
            }
            buf.set_line(area.x + gutter as u16, yy, &Line::from(row.spans.clone()), width as u16);
        }
        if self.doc.trim().is_empty() {
            let msg = "The document is empty.  Press o to write the first block.";
            buf.set_string(area.x + gutter as u16, area.y + 2.min(area.height - 1), msg, fg(DIM));
        }
    }

    fn card(&self, t: &Thread, w: usize, active: bool) -> Vec<Line<'static>> {
        let kc = kind_color(t.kind);
        let bar = Span::styled("▎", if active { fg(kc) } else { fg(FAINT) });
        let mut out: Vec<Line<'static>> = vec![];
        let mut push = |spans: Vec<Span<'static>>| {
            let mut v = vec![Span::raw(" "), bar.clone(), Span::raw(" ")];
            v.extend(spans);
            out.push(Line::from(v));
        };
        let place = match self.ix.thread_line.get(&t.id) {
            Some(l) => format!("L{}", l + 1),
            None => "anchor lost".into(),
        };
        let who = if t.agent { AGENT } else { HUMAN };
        let head = format!("#{} {} {}", t.id, kind_icon(t.kind), t.kind.name());
        let meta = format!("  {} · {}", t.author, ago(t.at));
        let pad = w.saturating_sub(head.width() + meta.width() + place.width());
        push(vec![
            Span::styled(head, bold(kc)),
            Span::styled(format!("  {}", t.author), fg(who)),
            Span::styled(format!(" · {}", ago(t.at)), fg(DIM)),
            Span::raw(" ".repeat(pad)),
            Span::styled(place, if self.ix.thread_line.contains_key(&t.id) { fg(DIM) } else { fg(DEL) }),
        ]);
        let max = if active { 10 } else { 2 };
        let diff = |sign: &str, text: &str, c: Color, strike: bool| -> Vec<Vec<Span<'static>>> {
            clip(wrap_str(text, w.saturating_sub(2)), max)
                .into_iter()
                .map(|l| {
                    let mut st = fg(c);
                    if strike {
                        st = st.add_modifier(Modifier::CROSSED_OUT);
                    }
                    vec![Span::styled(format!("{sign} "), fg(c)), Span::styled(l, st)]
                })
                .collect()
        };
        let rows = match t.kind {
            Kind::Comment => clip(wrap_str(&t.quote, w.saturating_sub(2)), if active { 3 } else { 1 })
                .into_iter()
                .map(|l| vec![Span::styled(format!("“{l}”"), fg(DIM).add_modifier(Modifier::ITALIC))])
                .collect(),
            Kind::Edit => {
                let mut v = diff("−", &t.quote, DEL, true);
                v.extend(diff("+", &t.new, ADD, false));
                v
            }
            Kind::Insert => diff("+", &t.new, ADD, false),
            Kind::Delete => diff("−", &t.quote, DEL, true),
        };
        rows.into_iter().for_each(&mut push);
        for l in clip(wrap_str(&t.body, w), if active { 12 } else { 3 }) {
            push(vec![Span::styled(l, fg(TEXT))]);
        }
        for r in &t.replies {
            let c = if r.agent { AGENT } else { HUMAN };
            let lines = clip(wrap_str(&format!("{}: {}", r.author, r.body), w.saturating_sub(2)), if active { 12 } else { 2 });
            for (i, l) in lines.into_iter().enumerate() {
                let lead = if i == 0 { "↳ " } else { "  " };
                let n = if i == 0 { r.author.len().min(l.len()) } else { 0 };
                let (a, b) = l.split_at(n);
                push(vec![Span::styled(lead, fg(DIM)), Span::styled(a.to_string(), fg(c)), Span::styled(b.to_string(), fg(TEXT))]);
            }
        }
        if active {
            let hint = if t.kind.is_proposal() {
                "a accept · x reject · m modify · r reply"
            } else {
                "r reply · R resolve"
            };
            push(vec![Span::styled(hint, fg(DIM))]);
        }
        out
    }

    fn draw_side(&mut self, f: &mut Frame, area: Rect) {
        let buf = f.buffer_mut();
        for y in area.top()..area.bottom() {
            buf.set_string(area.x, y, "│", fg(FAINT));
        }
        let inner = Rect::new(area.x + 1, area.y, area.width - 1, area.height);
        let open = self.ix.order.len();
        let head = Line::from(vec![
            Span::styled(" THREADS", bold(DIM)),
            Span::styled(format!("  {open} open"), fg(DIM)),
        ]);
        buf.set_line(inner.x, inner.y, &head, inner.width);
        if open == 0 {
            let tips = ["No open threads.", "", "c  comment on the block", "v  select lines first", "", "An agent adds proposals with", "`redline propose …`"];
            for (i, t) in tips.iter().enumerate() {
                buf.set_string(inner.x + 2, inner.y + 2 + i as u16, t, fg(DIM));
            }
            return;
        }
        let w = (inner.width as usize).saturating_sub(4);
        let mut lines: Vec<(Line<'static>, bool)> = vec![];
        let mut focus = (0, 0);
        for id in &self.ix.order {
            let Some(t) = self.state.threads.iter().find(|t| t.id == *id) else { continue };
            let active = self.active == Some(*id);
            let card = self.card(t, w, active);
            if active {
                focus = (lines.len(), lines.len() + card.len());
            }
            lines.extend(card.into_iter().map(|l| (l, active)));
            lines.push((Line::default(), false));
        }
        let h = (inner.height as usize).saturating_sub(2);
        if focus.1 > self.side_scroll + h {
            self.side_scroll = focus.1 - h;
        }
        if focus.0 < self.side_scroll {
            self.side_scroll = focus.0;
        }
        self.side_scroll = self.side_scroll.min(lines.len().saturating_sub(h));
        for (y, (line, active)) in lines.iter().skip(self.side_scroll).take(h).enumerate() {
            let yy = inner.y + 2 + y as u16;
            if *active {
                buf.set_style(Rect::new(inner.x, yy, inner.width, 1), Style::new().bg(CARD_BG));
            }
            buf.set_line(inner.x, yy, line, inner.width);
        }
    }

    fn draw_status(&self, f: &mut Frame, area: Rect) {
        let buf = f.buffer_mut();
        buf.set_style(area, Style::new().bg(BAR_BG));
        let pill = |name: &str, c: Color| {
            Span::styled(format!(" {name} "), Style::new().bg(c).fg(Color::Rgb(18, 18, 24)).add_modifier(Modifier::BOLD))
        };
        let mode = if self.session.is_some() {
            pill("EDITOR", ADD)
        } else if self.input.is_some() {
            pill("WRITE", NOTE)
        } else if self.mode == Mode::History {
            pill("HISTORY", AGENT)
        } else if self.sel.is_some() {
            pill("SELECT", HUMAN)
        } else {
            pill("NORMAL", ACCENT)
        };
        let mut spans = vec![mode, Span::styled(format!("  L{}/{}  ", self.cursor + 1, self.n_lines), fg(DIM))];
        let active = self.active.and_then(|a| self.state.threads.iter().find(|t| t.id == a));
        match &self.flash {
            Some((msg, _, err)) => spans.push(Span::styled(msg.clone(), if *err { bold(DEL) } else { fg(TEXT) })),
            None => {
                let hint = if self.session.is_some() {
                    "save and quit the editor to apply the change"
                } else if self.mode == Mode::History {
                    "j/k scroll · q back"
                } else if self.sel.is_some() {
                    "e edit lines · c comment · d delete · esc cancel"
                } else if active.is_some_and(|t| t.kind.is_proposal()) {
                    "a accept · x reject · m modify · r reply · n next thread"
                } else if active.is_some() {
                    "r reply · R resolve · n next thread"
                } else {
                    "e edit block · o new block · c comment · v select · n next thread"
                };
                spans.push(Span::styled(hint, fg(DIM)));
            }
        }
        buf.set_line(area.x, area.y, &Line::from(spans), area.width);
        let right = "? help · q quit ";
        if area.width as usize > right.len() + 90 {
            buf.set_string(area.right() - right.len() as u16, area.y, right, fg(DIM));
        }
    }

    fn draw_history(&mut self, f: &mut Frame, area: Rect) {
        let mut lines: Vec<Line> = vec![Line::default()];
        for e in self.history.iter().rev() {
            let (head, detail) = e.describe();
            let who = if e.agent { AGENT } else { HUMAN };
            lines.push(Line::from(vec![
                Span::styled(format!("  {}  ", e.at.with_timezone(&Local).format("%b %d %H:%M")), fg(DIM)),
                Span::styled(format!("{:<10} ", e.actor), bold(who)),
                Span::styled(head, fg(TEXT)),
            ]));
            let n = detail.len();
            for d in detail.into_iter().take(8) {
                let c = match d.chars().next() {
                    Some('-') => DEL,
                    Some('+') => ADD,
                    _ => DIM,
                };
                lines.push(Line::from(Span::styled(format!("{:27}{d}", ""), fg(c))));
            }
            if n > 8 {
                lines.push(Line::from(Span::styled(format!("{:27}… {} more lines", "", n - 8), fg(FAINT))));
            }
        }
        if self.history.is_empty() {
            lines.push(Line::from(Span::styled("  No history yet.", fg(DIM))));
        }
        self.hist_scroll = self.hist_scroll.min(lines.len().saturating_sub(area.height as usize));
        f.render_widget(Paragraph::new(lines).scroll((self.hist_scroll as u16, 0)), area);
    }

    fn draw_help(&self, f: &mut Frame, area: Rect) {
        let keys: &[(&str, &str)] = &[
            ("", "MOVE"),
            ("j k  ↓ ↑", "line down / up"),
            ("{ }", "previous / next block"),
            ("ctrl-d ctrl-u", "half page down / up"),
            ("gg  G", "top / bottom"),
            ("n N", "next / previous thread"),
            ("tab", "next thread on the same line"),
            ("", "WRITE (opens your editor)"),
            ("e  enter", "edit the block, or the selected lines"),
            ("o O", "new block below / above"),
            ("E", "edit the whole document"),
            ("v", "select lines"),
            ("dd", "delete the block (d on a selection)"),
            ("u", "undo"),
            ("", "REVIEW"),
            ("a", "accept the proposal"),
            ("m", "modify the proposal, then accept"),
            ("x", "reject the proposal, with a reason"),
            ("c", "comment on the block or selection"),
            ("r", "reply on the thread"),
            ("R", "resolve the comment"),
            ("", "OTHER"),
            ("H", "history of changes and decisions"),
            ("\\", "show / hide the threads"),
            ("q", "quit"),
        ];
        let w = 62.min(area.width);
        let h = (keys.len() as u16 + 2).min(area.height);
        let r = Rect::new(area.x + (area.width - w) / 2, area.y + (area.height - h) / 2, w, h);
        let lines: Vec<Line> = keys
            .iter()
            .map(|(k, d)| {
                if k.is_empty() {
                    Line::from(Span::styled(format!(" {d}"), bold(ACCENT)))
                } else {
                    Line::from(vec![Span::styled(format!("   {k:<16}"), bold(TEXT)), Span::styled(*d, fg(DIM))])
                }
            })
            .collect();
        f.render_widget(Clear, r);
        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(fg(FAINT))
            .title(Span::styled(" redline keys ", bold(TEXT)))
            .title_bottom(Span::styled(" any key closes ", fg(DIM)));
        f.render_widget(Paragraph::new(lines).block(block), r);
    }

    fn draw_input(&self, f: &mut Frame, area: Rect) {
        let Some(inp) = &self.input else { return };
        let w = 76.min(area.width.saturating_sub(4)).max(20);
        let iw = (w - 4) as usize;
        let mut rows = vec![String::new()];
        let (mut cur, mut col, mut off) = ((0, 0), 0, 0);
        for ch in inp.text.chars() {
            if off == inp.cur {
                cur = (rows.len() - 1, col);
            }
            if ch == '\n' {
                rows.push(String::new());
                col = 0;
            } else {
                let cw = ch.width().unwrap_or(0);
                if col + cw > iw {
                    rows.push(String::new());
                    col = 0;
                }
                rows.last_mut().unwrap().push(ch);
                col += cw;
            }
            off += ch.len_utf8();
        }
        if off == inp.cur {
            cur = (rows.len() - 1, col);
        }
        let max = 8.min(area.height.saturating_sub(4) as usize).max(1);
        let start = (cur.0 + 1).saturating_sub(max);
        let shown = rows.len().min(max);
        let h = shown as u16 + 2;
        let r = Rect::new(area.x + (area.width - w) / 2, area.bottom().saturating_sub(h + 1), w, h);
        f.render_widget(Clear, r);
        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(fg(NOTE))
            .title(Span::styled(format!(" {} ", inp.title), bold(NOTE)))
            .title_bottom(Span::styled(" ⏎ save · alt-⏎ new line · ^e editor · esc cancel ", fg(DIM)));
        let lines: Vec<Line> = if inp.text.is_empty() && !inp.hint.is_empty() {
            vec![Line::from(Span::styled(format!(" {}", inp.hint), fg(FAINT)))]
        } else {
            rows.iter().skip(start).take(max).map(|l| Line::from(Span::styled(format!(" {l}"), fg(TEXT)))).collect()
        };
        f.render_widget(Paragraph::new(lines).block(block), r);
        f.set_cursor_position((r.x + 2 + cur.1 as u16, r.y + 1 + (cur.0 - start) as u16));
    }

    fn draw_term(&mut self, f: &mut Frame, area: Rect) {
        let r = self.popup_rect(area);
        let editor = self.cfg.editor();
        let name = editor.split_whitespace().next().unwrap_or("editor").rsplit('/').next().unwrap_or("editor").to_string();
        let Some((term, job)) = &mut self.session else { return };
        f.render_widget(Clear, r);
        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(fg(ACCENT))
            .title(Span::styled(format!(" {name} · {} ", job.title), bold(ACCENT)))
            .title_bottom(Span::styled(" save and quit to apply · quit with an error to cancel ", fg(DIM)));
        let inner = block.inner(r);
        f.render_widget(block, r);
        term.resize(inner.height, inner.width);
        if let Some(pos) = term.render(inner, f.buffer_mut()) {
            f.set_cursor_position(pos);
        }
    }
}

