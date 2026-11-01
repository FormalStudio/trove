//! The file system as Trove sees it: entries, kinds, sorting, streaming directory listings,
//! watching, volumes and places. No UI types.

pub mod entry;

pub use entry::{Entry, EntryId, EntryKind, FileKind, Meta, SortKey, SortOrder, natural_cmp, sort_entries};
