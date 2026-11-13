//! The colours Trove names itself: the disk usage map's, one for each of the extensions that
//! hold the most space and one for every other file. Everything else reads the theme's tokens.

use nd::prelude::*;

use crate::treemap::SLOTS;

/// Mid tones, so each reads against the light and the dark surface alike. The last is the one
/// every unranked file shares.
const MAP: [u32; SLOTS + 1] = [0x4c9be8, 0xe0a13a, 0x4fb37a, 0xb07ae6, 0xd9744a, 0x3fb4b0, 0xe36fa8, 0x8a93a3];

pub fn slot_color(slot: u8) -> Hsla {
    rgb(MAP[usize::from(slot).min(SLOTS)]).into()
}

/// Every colour above, for the appearance audit to let through.
pub fn map_colors() -> [Hsla; SLOTS + 1] {
    MAP.map(|hex| rgb(hex).into())
}
