//! Colors. The palette expects a dark, truecolor terminal.

use ratatui::style::{Color, Modifier, Style};

pub const ACCENT: Color = Color::Rgb(255, 92, 87);
pub const TEXT: Color = Color::Rgb(214, 220, 232);
pub const DIM: Color = Color::Rgb(108, 116, 138);
pub const FAINT: Color = Color::Rgb(62, 68, 86);
pub const ADD: Color = Color::Rgb(130, 214, 140);
pub const ADD_BG: Color = Color::Rgb(28, 54, 38);
pub const DEL: Color = Color::Rgb(240, 120, 124);
pub const DEL_BG: Color = Color::Rgb(64, 30, 36);
pub const NOTE: Color = Color::Rgb(240, 198, 116);
pub const NOTE_BG: Color = Color::Rgb(58, 50, 24);
pub const NOTE_BG_ACTIVE: Color = Color::Rgb(98, 82, 30);
pub const AGENT: Color = Color::Rgb(187, 154, 247);
pub const HUMAN: Color = Color::Rgb(122, 162, 247);
pub const CODE: Color = Color::Rgb(137, 201, 214);
pub const H2: Color = Color::Rgb(255, 170, 110);
pub const H3: Color = Color::Rgb(122, 162, 247);
pub const CURSOR_BG: Color = Color::Rgb(34, 38, 50);
pub const SELECT_BG: Color = Color::Rgb(48, 58, 88);
pub const CARD_BG: Color = Color::Rgb(34, 38, 50);
pub const BAR_BG: Color = Color::Rgb(24, 26, 34);

pub fn fg(c: Color) -> Style {
    Style::new().fg(c)
}
pub fn bold(c: Color) -> Style {
    Style::new().fg(c).add_modifier(Modifier::BOLD)
}

use crate::store::Kind;
pub fn kind_color(k: Kind) -> Color {
    match k {
        Kind::Comment => NOTE,
        Kind::Edit => H2,
        Kind::Insert => ADD,
        Kind::Delete => DEL,
    }
}
pub fn kind_icon(k: Kind) -> &'static str {
    match k {
        Kind::Comment => "●",
        Kind::Edit => "~",
        Kind::Insert => "+",
        Kind::Delete => "−",
    }
}
