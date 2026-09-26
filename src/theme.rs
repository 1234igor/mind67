//! Two palettes. The canonical one is plate: white paper, black ink, one blue.
//! Kept in one place rather than sprinkled through paint.

use gpui::{Hsla, Rgba, rgb};

#[derive(Clone, Copy, Debug)]
pub struct Theme {
    pub dark: bool,
    /// The ground everything is drawn on.
    pub bg: u32,
    /// Text.
    pub ink: u32,
    pub ink_soft: u32,
    /// For chrome that has to be *read* — menu hints, panel captions. Kept at
    /// 4.5:1 against `bg` in both palettes.
    pub ink_quiet: u32,
    /// A whisper, for the placeholder in an empty node. Deliberately below
    /// reading contrast: it marks a place rather than saying anything.
    pub ink_faint: u32,
    /// Edges.
    pub line: u32,
    /// Focus + edge-edit accents.
    pub accent: u32,
    pub edge_accent: u32,
    pub hover: u32,
}

/// Engraved plate: white ground, black line, one blue for what you are on.
pub const LIGHT: Theme = Theme {
    dark: false,
    bg: 0xffffff,
    ink: 0x000000,
    ink_soft: 0x555555,
    ink_quiet: 0x777777,
    ink_faint: 0xa8a8a8,
    line: 0x1a1a1a,
    accent: 0x1140e0,
    edge_accent: 0x1140e0,
    hover: 0x707070,
};

/// The same plate inverted, for working at night.
pub const DARK: Theme = Theme {
    dark: true,
    bg: 0x0b0d10,
    ink: 0xf2f2ef,
    ink_soft: 0x9aa0a8,
    ink_quiet: 0x7d838b,
    ink_faint: 0x4a5058,
    line: 0xb8bcc0,
    accent: 0x5b8cff,
    edge_accent: 0x5b8cff,
    hover: 0x8b929b,
};

impl Theme {
    pub fn toggled(self) -> Theme {
        if self.dark { LIGHT } else { DARK }
    }

    pub fn from_dark(dark: bool) -> Theme {
        if dark { DARK } else { LIGHT }
    }
}

/// `0xRRGGBB` → GPUI color.
pub fn c(v: u32) -> Rgba {
    rgb(v)
}

/// `0xRRGGBB` + alpha → GPUI color, for the parts that need to sit over the map.
pub fn ca(v: u32, alpha: f32) -> Hsla {
    let mut h: Hsla = rgb(v).into();
    h.a = alpha.clamp(0.0, 1.0);
    h
}
