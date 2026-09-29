//! Terminal colour support. The UI is drawn with 24-bit RGB colours; after
//! each frame is laid out, [`apply`] adapts the buffer to what the terminal can
//! actually show — the nearest xterm-256 colour, or no colour at all when
//! `NO_COLOR` is set.

use ratatui::buffer::Buffer;
use ratatui::style::{Color, Modifier};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ColorMode {
    /// 24-bit colour, drawn as designed.
    TrueColor,
    /// The xterm 256-colour palette (e.g. macOS Terminal.app).
    Ansi256,
    /// Monochrome: colours are dropped, and highlighted backgrounds (the
    /// selection, search hits, toasts) are shown in reverse video instead.
    None,
}

impl ColorMode {
    /// Pick a mode from the environment.
    ///
    /// In order: `NO_COLOR` (any non-empty value, per <https://no-color.org>),
    /// an explicit `HN_TUI_COLOR` of `truecolor`/`24bit`, `256` or `none`,
    /// `COLORTERM=truecolor|24bit`, and finally terminals known to lack 24-bit
    /// support. Anything else keeps full colour, as before.
    pub fn from_env() -> Self {
        Self::detect(|k| std::env::var(k).ok())
    }

    fn detect(var: impl Fn(&str) -> Option<String>) -> Self {
        let set = |k: &str| var(k).filter(|v| !v.is_empty());
        if set("NO_COLOR").is_some() {
            return ColorMode::None;
        }
        match set("HN_TUI_COLOR").as_deref().map(str::to_ascii_lowercase) {
            Some(v) if v == "truecolor" || v == "24bit" => return ColorMode::TrueColor,
            Some(v) if v == "256" => return ColorMode::Ansi256,
            Some(v) if v == "none" || v == "never" => return ColorMode::None,
            _ => {}
        }
        if matches!(set("COLORTERM").as_deref(), Some("truecolor" | "24bit")) {
            return ColorMode::TrueColor;
        }
        if set("TERM_PROGRAM").as_deref() == Some("Apple_Terminal") {
            return ColorMode::Ansi256;
        }
        ColorMode::TrueColor
    }
}

/// Adapt every cell of a rendered frame to `mode`. `base_bg` is the app's
/// background colour, which monochrome mode leaves to the terminal default
/// rather than rendering as reverse video.
pub fn apply(buf: &mut Buffer, mode: ColorMode, base_bg: Color) {
    match mode {
        ColorMode::TrueColor => {}
        ColorMode::Ansi256 => {
            for cell in &mut buf.content {
                cell.fg = to_ansi256(cell.fg);
                cell.bg = to_ansi256(cell.bg);
            }
        }
        ColorMode::None => {
            for cell in &mut buf.content {
                let highlighted = cell.bg != Color::Reset && cell.bg != base_bg;
                cell.fg = Color::Reset;
                cell.bg = Color::Reset;
                if highlighted {
                    cell.modifier.insert(Modifier::REVERSED);
                }
            }
        }
    }
}

/// Map an RGB colour to the nearest xterm-256 palette entry, choosing between
/// the 6×6×6 colour cube and the 24-step grey ramp. Other colours pass through.
pub fn to_ansi256(color: Color) -> Color {
    let Color::Rgb(r, g, b) = color else {
        return color;
    };
    const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
    let level = |v: u8| -> usize {
        (0..6)
            .min_by_key(|&i| (LEVELS[i] as i32 - v as i32).abs())
            .unwrap_or(0)
    };
    let (ri, gi, bi) = (level(r), level(g), level(b));
    let cube = (LEVELS[ri], LEVELS[gi], LEVELS[bi]);
    let cube_idx = 16 + 36 * ri + 6 * gi + bi;

    let avg = (r as u32 + g as u32 + b as u32) / 3;
    let grey_i = (avg.saturating_sub(8) / 10).min(23) as u8;
    let grey_v = 8 + 10 * grey_i;
    let grey = (grey_v, grey_v, grey_v);

    let dist = |(cr, cg, cb): (u8, u8, u8)| {
        let d = |a: u8, b: u8| (a as i32 - b as i32).pow(2);
        d(r, cr) + d(g, cg) + d(b, cb)
    };
    if dist(grey) < dist(cube) {
        Color::Indexed(232 + grey_i)
    } else {
        Color::Indexed(cube_idx as u8)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::layout::Rect;
    use ratatui::style::Style;

    fn detect(vars: &[(&str, &str)]) -> ColorMode {
        ColorMode::detect(|k| {
            vars.iter()
                .find(|(name, _)| *name == k)
                .map(|(_, v)| v.to_string())
        })
    }

    #[test]
    fn detection_order() {
        assert_eq!(detect(&[]), ColorMode::TrueColor);
        assert_eq!(detect(&[("NO_COLOR", "1")]), ColorMode::None);
        assert_eq!(detect(&[("NO_COLOR", "")]), ColorMode::TrueColor); // empty = unset
        assert_eq!(
            detect(&[("TERM_PROGRAM", "Apple_Terminal")]),
            ColorMode::Ansi256
        );
        assert_eq!(
            detect(&[
                ("TERM_PROGRAM", "Apple_Terminal"),
                ("COLORTERM", "truecolor")
            ]),
            ColorMode::TrueColor
        );
        // The explicit override beats auto-detection, but not NO_COLOR.
        assert_eq!(
            detect(&[("COLORTERM", "truecolor"), ("HN_TUI_COLOR", "256")]),
            ColorMode::Ansi256
        );
        assert_eq!(
            detect(&[
                ("HN_TUI_COLOR", "TrueColor"),
                ("TERM_PROGRAM", "Apple_Terminal")
            ]),
            ColorMode::TrueColor
        );
        assert_eq!(
            detect(&[("NO_COLOR", "1"), ("HN_TUI_COLOR", "truecolor")]),
            ColorMode::None
        );
    }

    #[test]
    fn rgb_maps_to_nearest_palette_entry() {
        assert_eq!(to_ansi256(Color::Rgb(0, 0, 0)), Color::Indexed(16));
        assert_eq!(to_ansi256(Color::Rgb(255, 255, 255)), Color::Indexed(231));
        assert_eq!(to_ansi256(Color::Rgb(255, 0, 0)), Color::Indexed(196));
        // HN orange lands on the cube's orange.
        assert_eq!(to_ansi256(Color::Rgb(255, 102, 0)), Color::Indexed(202));
        // Dark greys prefer the grey ramp over the coarse cube.
        assert_eq!(to_ansi256(Color::Rgb(20, 22, 26)), Color::Indexed(233));
        // Non-RGB colours are untouched.
        assert_eq!(to_ansi256(Color::Green), Color::Green);
    }

    #[test]
    fn monochrome_reverses_highlights_but_not_the_base_background() {
        let base = Color::Rgb(20, 22, 26);
        let mut buf = Buffer::empty(Rect::new(0, 0, 2, 1));
        buf[(0, 0)].set_style(Style::default().fg(Color::White).bg(base));
        buf[(1, 0)].set_style(
            Style::default()
                .fg(Color::Black)
                .bg(Color::Rgb(255, 102, 0)),
        );
        apply(&mut buf, ColorMode::None, base);

        assert_eq!(buf[(0, 0)].fg, Color::Reset);
        assert_eq!(buf[(0, 0)].bg, Color::Reset);
        assert!(!buf[(0, 0)].modifier.contains(Modifier::REVERSED));
        assert_eq!(buf[(1, 0)].bg, Color::Reset);
        assert!(buf[(1, 0)].modifier.contains(Modifier::REVERSED));
    }
}
