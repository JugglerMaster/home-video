// Database module (SQLite via rusqlite, FTS5 search).
//
// S1: schema (videos, playlists, playlist_items, FTS5 virtual table).
// S2: video insert/list/dedupe helpers.
// S4: FTS5 search wired into Library (sync triggers added there).
//
// See SPEC.md §9.

use std::time::SystemTime;

use rusqlite::{params, Connection};

/// The SQLite connection for home-video.
pub struct Db {
    conn: Connection,
}

/// A video as stored in the `videos` table.
#[derive(Clone, Debug)]
pub struct Video {
    pub id: u64,
    pub fingerprint: String,
    pub path: String,
    pub title: String,
    pub description: String,
    pub tags: String,
    pub duration: f64,
    pub resolution: String,
    pub codec: String,
    pub size: i64,
    pub bitrate: i64,
    pub created_at: String,
    pub updated_at: String,
    pub hls_m3u8: String,
    pub thumbnail_path: String,
}

fn now() -> String {
    let t = SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap();
    format!("{}.{}", t.as_secs(), t.subsec_nanos() / 1_000_000)
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

    /// Insert a video. Returns the new row id.
    pub fn insert_video(&self, video: &Video) -> anyhow::Result<u64> {
        self.conn.execute(
            "INSERT INTO videos (
                fingerprint, path, title, description, tags,
                duration, resolution, codec, size, bitrate,
                created_at, updated_at, hls_m3u8, thumbnail_path
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            params![
                video.fingerprint,
                video.path,
                video.title,
                video.description,
                video.tags,
                video.duration,
                video.resolution,
                video.codec,
                video.size,
                video.bitrate,
                video.created_at,
                video.updated_at,
                video.hls_m3u8,
                video.thumbnail_path,
            ],
        )?;
        Ok(self.conn.last_insert_rowid() as u64)
    }

    /// List all videos.
    pub fn get_videos(&self) -> anyhow::Result<Vec<Video>> {
        let mut stmt = self.conn.prepare("SELECT * FROM videos ORDER BY id DESC")?;
        let rows = stmt.query_map([], |row| {
            Ok(Video {
                id: row.get(0)?,
                fingerprint: row.get(1)?,
                path: row.get(2)?,
                title: row.get(3)?,
                description: row.get(4)?,
                tags: row.get(5)?,
                duration: row.get(6)?,
                resolution: row.get(7)?,
                codec: row.get(8)?,
                size: row.get(9)?,
                bitrate: row.get(10)?,
                created_at: row.get(11)?,
                updated_at: row.get(12)?,
                hls_m3u8: row.get(13)?,
                thumbnail_path: row.get(14)?,
            })
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Find a video by fingerprint (dedupe check). Returns `None` if the
    /// fingerprint is not yet in the DB.
    pub fn find_by_fingerprint(&self, fp: &str) -> anyhow::Result<Option<u64>> {
        let mut stmt = self.conn.prepare("SELECT id FROM videos WHERE fingerprint = ?1")?;
        match stmt.query_row(params![fp], |row| row.get::<_, u64>(0)) {
            Ok(id) => Ok(Some(id)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(anyhow::anyhow!("find_by_fingerprint: {e}")),
        }
    }

    /// Update the hls_m3u8 and thumbnail_path for a video.
    pub fn update_paths(&self, id: u64, hls_m3u8: &str, thumbnail_path: &str) -> anyhow::Result<()> {
        self.conn.execute(
            "UPDATE videos SET hls_m3u8 = ?1, thumbnail_path = ?2, updated_at = ?3 WHERE id = ?4",
            params![hls_m3u8, thumbnail_path, now(), id],
        )?;
        Ok(())
    }

}
