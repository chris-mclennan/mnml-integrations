//! Theme — small color palette for the TUI. Kept minimal so v0.1
//! ships a stable interface v0.2 can plug into.

#![allow(dead_code)]

use ratatui::style::Color;

pub struct Theme {
    pub fg: Color,
    pub bg: Color,
    pub accent: Color,
    pub comment: Color,
    pub warn: Color,
    pub error: Color,
}

impl Theme {
    pub fn cyberdream() -> Self {
        Self {
            fg: Color::Rgb(0xe6, 0xd8, 0xff),
            bg: Color::Rgb(0x12, 0x0c, 0x1a),
            accent: Color::Rgb(0x66, 0xaa, 0xff),
            comment: Color::Rgb(0x9a, 0x89, 0xbf),
            warn: Color::Rgb(0xff, 0xb8, 0x66),
            error: Color::Rgb(0xff, 0x6a, 0x6a),
        }
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self::cyberdream()
    }
}
