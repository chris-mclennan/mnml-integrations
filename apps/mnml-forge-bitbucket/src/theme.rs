//! Follow mnml's family theme.
//!
//! mnml writes its resolved active theme to `~/.config/mnml/current-theme.toml`
//! (see mnml's `docs/THEMING.md`). This app reads that file and remaps its
//! ratatui named colours onto mnml's `[base_30]` palette, so it matches whatever
//! theme mnml is on — and retints live when mnml switches theme (mtime poll,
//! once per render tick). When the file is absent (mnml not installed / never
//! run) [`remap`] is a no-op, so the app keeps its own colours.
//!
//! Self-contained: no extra deps, no wire-protocol coupling. Path logic mirrors
//! mnml's ( `$XDG_CONFIG_HOME` else `$HOME/.config` — not `dirs::config_dir()`,
//! which is `~/Library/...` on macOS).

use std::path::PathBuf;
use std::sync::{OnceLock, RwLock};
use std::time::SystemTime;

use ratatui::style::Color;

/// mnml's palette, as ratatui `Color`s. One field per role this app maps onto.
#[derive(Clone, Copy)]
pub struct Palette {
    pub fg: Color,
    pub dim: Color,
    /// Parsed from mnml's palette for completeness; this app's `remap`
    /// leaves backgrounds (`Color::Reset`) untouched, so it's unused today.
    #[allow(dead_code)]
    pub bg: Color,
    pub red: Color,
    pub green: Color,
    pub yellow: Color,
    pub blue: Color,
    pub cyan: Color,
    pub purple: Color,
}

fn theme_path() -> Option<PathBuf> {
    let xdg = std::env::var("XDG_CONFIG_HOME")
        .ok()
        .filter(|x| !x.is_empty());
    if let Some(x) = xdg {
        return Some(PathBuf::from(x).join("mnml").join("current-theme.toml"));
    }
    std::env::var_os("HOME").map(|h| {
        PathBuf::from(h)
            .join(".config")
            .join("mnml")
            .join("current-theme.toml")
    })
}

fn parse_hex(s: &str) -> Option<Color> {
    let s = s.trim().strip_prefix('#')?;
    if s.len() != 6 {
        return None;
    }
    let h = |a: usize| u8::from_str_radix(&s[a..a + 2], 16).ok();
    Some(Color::Rgb(h(0)?, h(2)?, h(4)?))
}

/// Project the `[base_30]` table of mnml's theme file onto a [`Palette`].
fn project(src: &str) -> Palette {
    let mut m = std::collections::HashMap::new();
    let mut in_base30 = false;
    for line in src.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') {
            in_base30 = line == "[base_30]";
            continue;
        }
        if !in_base30 {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        if let Some(c) = parse_hex(v.trim().trim_matches('"')) {
            m.insert(k.trim().to_string(), c);
        }
    }
    let g = |k: &str, d: Color| m.get(k).copied().unwrap_or(d);
    Palette {
        fg: g("white", Color::Gray),
        // The dim/secondary role — mnml emits it as light_grey.
        dim: g("light_grey", Color::DarkGray),
        bg: g("one_bg", Color::Reset),
        red: g("red", Color::Red),
        green: g("green", Color::Green),
        yellow: g("yellow", Color::Yellow),
        blue: g("blue", Color::Blue),
        cyan: g("cyan", Color::Cyan),
        purple: g("purple", Color::Magenta),
    }
}

struct State {
    palette: Option<Palette>,
    mtime: Option<SystemTime>,
}

fn state() -> &'static RwLock<State> {
    static S: OnceLock<RwLock<State>> = OnceLock::new();
    S.get_or_init(|| {
        RwLock::new(State {
            palette: None,
            mtime: None,
        })
    })
}

/// The current family palette, or `None` when mnml's theme file isn't present.
pub fn palette() -> Option<Palette> {
    state().read().ok().and_then(|s| s.palette)
}

/// Reload the palette if mnml's theme file changed. Call once per render tick:
/// a single `stat()` in steady state, a full parse only on a theme switch.
pub fn poll_refresh() {
    let path = match theme_path() {
        Some(p) => p,
        None => return,
    };
    let mtime = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
    let unchanged = state().read().map(|s| s.mtime == mtime).unwrap_or(false);
    if unchanged {
        return;
    }
    let palette = std::fs::read_to_string(&path).ok().map(|src| project(&src));
    if let Ok(mut s) = state().write() {
        s.palette = palette;
        s.mtime = mtime;
    }
}

/// Remap a ratatui named colour onto mnml's palette when a theme file is
/// present (else returns it unchanged). Apply at the render chokepoint
/// (`color_to_rgba` for blit, or per-widget for standalone) so the app follows
/// mnml's theme — most importantly `DarkGray → dim` (the dim/secondary text).
pub fn remap(c: Color) -> Color {
    let p = match palette() {
        Some(p) => p,
        None => return c,
    };
    match c {
        Color::DarkGray => p.dim,
        Color::Gray => p.fg,
        Color::Red => p.red,
        Color::Green => p.green,
        Color::Yellow => p.yellow,
        Color::Blue => p.blue,
        Color::Cyan => p.cyan,
        Color::Magenta => p.purple,
        other => other,
    }
}
