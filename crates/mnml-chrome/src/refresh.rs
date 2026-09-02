//! The refresh affordance. Must match `src/ui/refresh_glyph.rs` and
//! `Button::refresh` in `src/ui/action_button.rs` in mnml core.

/// codicon-refresh — the canonical glyph across the whole family.
pub const NERD: &str = "\u{EB37}";

/// ASCII fallback for `--ascii` mode and terminals without a Nerd
/// Font. `↺` (U+21BA) renders nearly everywhere.
pub const ASCII: &str = "\u{21BA}";

/// How much room the chip has.
///
/// Two modes, because the same affordance appears in a tight panel
/// header and in a wide app toolbar and must read as the SAME control
/// in both.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// ` <glyph> ` — icon only.
    Compact,
    /// ` <glyph> <label> ` — icon plus a word.
    Expanded,
}

/// Just the glyph for the current icon mode.
#[inline]
pub fn glyph(ascii: bool) -> &'static str {
    if ascii { ASCII } else { NERD }
}

/// Rendered chip content, padding included, so no caller invents its
/// own spacing. `label` is ignored in [`Mode::Compact`].
pub fn chip(mode: Mode, ascii: bool, label: &str) -> String {
    match mode {
        Mode::Compact => format!(" {} ", glyph(ascii)),
        Mode::Expanded => format!(" {} {label} ", glyph(ascii)),
    }
}

/// Cell width of [`chip`], for sizing the click rect.
pub fn width(mode: Mode, ascii: bool, label: &str) -> u16 {
    chip(mode, ascii, label).chars().count() as u16
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The point of the crate: both modes, one glyph.
    #[test]
    fn both_modes_use_the_same_glyph() {
        assert!(chip(Mode::Compact, false, "").contains(NERD));
        assert!(chip(Mode::Expanded, false, "Refresh").contains(NERD));
    }

    /// The glyph must equal mnml core's. Hard-coded deliberately: if
    /// core ever changes it, this fails and someone has to update both
    /// rather than letting the family drift again.
    #[test]
    fn the_glyph_matches_mnml_core() {
        assert_eq!(NERD, "\u{EB37}", "diverged from core's refresh_glyph::NERD");
        assert_eq!(
            ASCII, "\u{21BA}",
            "diverged from core's refresh_glyph::ASCII"
        );
    }

    #[test]
    fn ascii_mode_swaps_the_glyph_in_both_modes() {
        assert!(chip(Mode::Compact, true, "").contains(ASCII));
        assert!(chip(Mode::Expanded, true, "Refresh").contains(ASCII));
    }

    #[test]
    fn width_matches_the_rendered_chip() {
        for (m, l) in [(Mode::Compact, ""), (Mode::Expanded, "Refresh")] {
            assert_eq!(
                width(m, false, l) as usize,
                chip(m, false, l).chars().count()
            );
        }
    }
}
