// Database module (SQLite via rusqlite, FTS5 search).
//
// S1: schema (videos, playlists, playlist_items, FTS5 virtual table).
// S4: FTS5 search wired into Library.
//
// See SPEC.md §9.

/// Placeholder for the SQLite connection.
pub struct Db {
    #[allow(dead_code)]
    path: std::path::PathBuf,
}
