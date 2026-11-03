//! The views' sizes: list rows (shared by the columns view and the search results) and grid
//! tiles, each a few fixed steps, stepped with Ctrl+= and Ctrl+- and kept in the settings.

use trove_preview::ThumbSize;

use crate::settings::TroveSettings;
use crate::tab::View;

/// A list row: its height, the box its picture takes and the icon drawn until a thumbnail
/// exists.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RowSize {
    pub height: f32,
    pub art: f32,
    pub glyph: f32,
}

/// A grid tile: the cell, the picture's box inside it and the icon drawn until a thumbnail
/// exists. The cell is the picture plus the name's two lines and the padding round both.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TileSize {
    pub tile: (f32, f32),
    pub art: (f32, f32),
    pub glyph: f32,
}

/// The first is Finder's density.
pub const ROWS: [RowSize; 3] = [
    RowSize { height: 28., art: 20., glyph: 16. },
    RowSize { height: 36., art: 28., glyph: 20. },
    RowSize { height: 48., art: 40., glyph: 24. },
];

const fn tile(art: (f32, f32), glyph: f32) -> TileSize {
    TileSize { tile: (art.0 + 24., art.1 + 44.), art, glyph }
}

pub const TILES: [TileSize; 5] = [
    tile((72., 48.), 40.),
    tile((88., 60.), 48.),
    tile((120., 80.), 64.),
    tile((160., 108.), 80.),
    tile((208., 140.), 96.),
];

pub const GRID_DEFAULT: usize = 1;

/// What each view is zoomed to, as indexes into [`ROWS`] and [`TILES`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Zoom {
    pub list: usize,
    pub grid: usize,
}

impl Default for Zoom {
    fn default() -> Self {
        Self { list: 0, grid: GRID_DEFAULT }
    }
}

impl Zoom {
    pub fn of(settings: &TroveSettings) -> Self {
        Self {
            list: usize::from(settings.list_zoom).min(ROWS.len() - 1),
            grid: usize::from(settings.grid_zoom).min(TILES.len() - 1),
        }
    }

    pub fn row(&self) -> RowSize {
        ROWS[self.list.min(ROWS.len() - 1)]
    }

    pub fn tile(&self) -> TileSize {
        TILES[self.grid.min(TILES.len() - 1)]
    }

    /// One step bigger (`by` 1) or smaller (-1) in `view`, held at the ends.
    pub fn step(self, view: View, by: isize) -> Self {
        let next = |at: usize, len: usize| at.saturating_add_signed(by).min(len - 1);
        match view {
            View::Grid => Self { grid: next(self.grid, TILES.len()), ..self },
            View::List | View::Columns => Self { list: next(self.list, ROWS.len()), ..self },
        }
    }

    /// `view` back at its default size.
    pub fn reset(self, view: View) -> Self {
        let default = Self::default();
        match view {
            View::Grid => Self { grid: default.grid, ..self },
            View::List | View::Columns => Self { list: default.list, ..self },
        }
    }
}

/// The freedesktop size to ask for a picture `long_side` points across: Normal is 128 pixels,
/// Large 256.
pub fn thumb_size(long_side: f32) -> ThumbSize {
    if long_side > ThumbSize::Normal.pixels() as f32 { ThumbSize::Large } else { ThumbSize::Normal }
}
