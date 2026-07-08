use ratatui::style::{Color, Modifier, Style};

pub const BG: Color = Color::Rgb(0x10, 0x12, 0x1c);
pub const PANEL: Color = Color::Rgb(0x2c, 0x1e, 0x31);
pub const PANEL_DARK: Color = Color::Rgb(0x1e, 0x40, 0x44);
pub const BORDER: Color = Color::Rgb(0x5e, 0x5b, 0x8c);
pub const BORDER_ACTIVE: Color = Color::Rgb(0x36, 0xc5, 0xf4);
pub const TEXT: Color = Color::Rgb(0xf6, 0xe8, 0xe0);
pub const MUTED: Color = Color::Rgb(0xb0, 0xa7, 0xb8);
pub const ACCENT: Color = Color::Rgb(0xf3, 0xa8, 0x33);
pub const GREEN: Color = Color::Rgb(0x5a, 0xb5, 0x52);
pub const LIME: Color = Color::Rgb(0x9d, 0xe6, 0x4e);
pub const CYAN: Color = Color::Rgb(0x6d, 0xea, 0xd6);
pub const BLUE: Color = Color::Rgb(0x33, 0x88, 0xde);
pub const RED: Color = Color::Rgb(0xec, 0x27, 0x3f);
pub const ORANGE: Color = Color::Rgb(0xe9, 0x85, 0x37);
pub const SELECT: Color = Color::Rgb(0x6b, 0x26, 0x43);

pub fn base() -> Style {
    Style::default().fg(TEXT).bg(BG)
}

pub fn panel() -> Style {
    Style::default().fg(TEXT).bg(PANEL)
}

pub fn panel_dark() -> Style {
    Style::default().fg(TEXT).bg(PANEL_DARK)
}

pub fn active_title() -> Style {
    Style::default()
        .fg(ACCENT)
        .bg(PANEL)
        .add_modifier(Modifier::BOLD)
}

pub fn inactive_title() -> Style {
    Style::default()
        .fg(MUTED)
        .bg(PANEL)
        .add_modifier(Modifier::BOLD)
}

pub fn header() -> Style {
    Style::default()
        .fg(CYAN)
        .bg(PANEL)
        .add_modifier(Modifier::BOLD)
}

pub fn selected() -> Style {
    Style::default()
        .fg(Color::White)
        .bg(SELECT)
        .add_modifier(Modifier::BOLD)
}

pub fn marked() -> Style {
    Style::default().fg(LIME).bg(PANEL)
}

pub fn muted() -> Style {
    Style::default().fg(MUTED).bg(PANEL)
}

pub fn ok() -> Style {
    Style::default().fg(GREEN).bg(PANEL)
}

pub fn warn() -> Style {
    Style::default().fg(ORANGE).bg(PANEL)
}

pub fn error() -> Style {
    Style::default()
        .fg(RED)
        .bg(PANEL)
        .add_modifier(Modifier::BOLD)
}
