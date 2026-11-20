use ratatui::style::{Color, Modifier, Style};

pub const ACCENT: Color = Color::Cyan;
pub const ACCENT_ALT: Color = Color::LightBlue;
pub const SUCCESS: Color = Color::Green;
pub const WARNING: Color = Color::Yellow;
pub const DANGER: Color = Color::Red;
pub const MUTED: Color = Color::DarkGray;
pub const TEXT: Color = Color::White;
pub const SUBTEXT: Color = Color::Gray;
pub const HIGHLIGHT_BG: Color = Color::Rgb(30, 52, 68);
pub const MODAL_BG: Color = Color::Rgb(18, 22, 28);

pub fn title() -> Style {
    Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
}

pub fn border() -> Style {
    Style::default().fg(MUTED)
}

pub fn border_focused() -> Style {
    Style::default().fg(ACCENT)
}

pub fn selected_row() -> Style {
    Style::default()
        .bg(HIGHLIGHT_BG)
        .fg(TEXT)
        .add_modifier(Modifier::BOLD)
}

pub fn table_header() -> Style {
    Style::default()
        .fg(ACCENT_ALT)
        .add_modifier(Modifier::BOLD)
}

pub fn muted() -> Style {
    Style::default().fg(MUTED)
}

pub fn subtext() -> Style {
    Style::default().fg(SUBTEXT)
}

pub fn success() -> Style {
    Style::default().fg(SUCCESS)
}

pub fn warning() -> Style {
    Style::default().fg(WARNING)
}

pub fn danger() -> Style {
    Style::default().fg(DANGER)
}
