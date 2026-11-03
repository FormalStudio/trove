//! The file system as Trove sees it: entries, kinds, sorting, streaming directory listings,
//! watching, volumes and places. No UI types.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod entry;
pub mod listing;
pub mod places;
pub mod volumes;

pub use entry::{
    Entry, EntryId, EntryKind, FileKind, IdMap, IdSet, Meta, SortKey, SortOrder, natural_cmp, natural_key, sort_entries,
};
pub use listing::{Change, ListError, ListEvent, ListOptions, Listing, list};
pub use places::{
    Place, PlaceIcon, add_bookmark, bookmarks, file_uri, parse_templates_dir, recent, remove_bookmark, templates_dir,
    trash_dir, uri_path, user_dirs,
};
pub use volumes::{Usage, Volume, VolumeKind, is_network_path, unmount, usage, volumes, watch_volumes};
