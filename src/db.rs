// Database module (SQLite via rusqlite, FTS5 search).
//
// S1: schema (videos, playlists, playlist_items, FTS5 virtual table).
// S4: FTS5 search wired into Library (sync triggers added there).
//
// See SPEC.md §9.

use rusqlite::Connection;

/// The SQLite connection for home-video.
pub struct Db {
    conn: Connection,
}

impl Db {
    /// Open (or create) the database at `path`.
    pub fn new(path: &std::path::Path) -> anyhow::Result<Self> {
        let conn = Connection::open(path)?;
        // Enforce foreign keys.
        conn.execute("PRAGMA foreign_keys = ON", [])?;
        Ok(Db { conn })
    }

    /// Create the schema (idempotent).
    pub fn create_schema(&self) -> anyhow::Result<()> {
        let schema = r#"
            CREATE TABLE IF NOT EXISTS videos (
                id             INTEGER PRIMARY KEY AUTOINCREMENT,
                fingerprint    TEXT NOT NULL UNIQUE,
                path           TEXT NOT NULL,
                title          TEXT NOT NULL DEFAULT '',
                description    TEXT NOT NULL DEFAULT '',
                tags           TEXT NOT NULL DEFAULT '',
                duration       REAL,
                resolution     TEXT,
                codec          TEXT,
                size           INTEGER,
                bitrate        INTEGER,
                created_at     TEXT NOT NULL,
                updated_at     TEXT NOT NULL,
                hls_m3u8       TEXT,
                thumbnail_path TEXT
            );

            CREATE TABLE IF NOT EXISTS playlists (
                id          INTEGER PRIMARY KEY AUTOINCREMENT,
                name        TEXT NOT NULL,
                source      TEXT NOT NULL DEFAULT 'manual',
                youtube_url TEXT,
                created_at  TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS playlist_items (
                playlist_id INTEGER NOT NULL,
                video_id    INTEGER NOT NULL,
                position    INTEGER NOT NULL,
                PRIMARY KEY (playlist_id, video_id),
                FOREIGN KEY (playlist_id) REFERENCES playlists(id),
                FOREIGN KEY (video_id) REFERENCES videos(id)
            );

            -- FTS5 virtual table over title, tags, description.
            -- Content-sync triggers are added in S4 when search is wired up.
            CREATE VIRTUAL TABLE IF NOT EXISTS videos_fts USING fts5(
                title,
                tags,
                description,
                content='videos',
                content_rowid='id'
            );
        "#;
        self.conn.execute_batch(schema)?;
        Ok(())
    }

    /// The underlying connection (for query helpers in later slices).
    pub fn conn(&self) -> &Connection {
        &self.conn
    }
}
