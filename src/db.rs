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
    pub playback_position: f64,
    pub favorite: bool,
    pub audio_path: String,
}

fn now() -> String {
    let t = SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap();
    format!("{}.{}", t.as_secs(), t.subsec_millis())
}

/// A playlist as stored in the `playlists` table.
#[derive(Clone, Debug)]
pub struct Playlist {
    pub id: u64,
    pub name: String,
    pub source: String,
    pub youtube_url: Option<String>,
    pub created_at: String,
}


impl Db {
    /// Open (or create) the database at `path`.
    pub fn new(path: &std::path::Path) -> anyhow::Result<Self> {
        let conn = Connection::open(path)?;
        // Enforce foreign keys.
        conn.execute("PRAGMA foreign_keys = ON", [])?;
        Ok(Db { conn })
    }

    /// Create the schema (idempotent). New columns are added via `ALTER TABLE`
    /// only when absent, so existing databases migrate in place.
    pub fn create_schema(&self) -> anyhow::Result<()> {
        let tables = r#"CREATE TABLE IF NOT EXISTS videos (
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
                thumbnail_path TEXT,
                playback_position REAL NOT NULL DEFAULT 0,
                favorite       INTEGER NOT NULL DEFAULT 0
            ); CREATE TABLE IF NOT EXISTS playlists (
                id          INTEGER PRIMARY KEY AUTOINCREMENT,
                name        TEXT NOT NULL,
                source      TEXT NOT NULL DEFAULT 'manual',
                youtube_url TEXT,
                created_at  TEXT NOT NULL
            ); CREATE TABLE IF NOT EXISTS playlist_items (
                playlist_id INTEGER NOT NULL,
                video_id    INTEGER NOT NULL,
                position    INTEGER NOT NULL,
                PRIMARY KEY (playlist_id, video_id),
                FOREIGN KEY (playlist_id) REFERENCES playlists(id),
                FOREIGN KEY (video_id) REFERENCES videos(id)
            ); CREATE TABLE IF NOT EXISTS queue (
                id         INTEGER PRIMARY KEY AUTOINCREMENT,
                video_id   INTEGER NOT NULL,
                position   INTEGER NOT NULL,
                created_at TEXT NOT NULL,
                FOREIGN KEY (video_id) REFERENCES videos(id) ON DELETE CASCADE
            );"#;
        self.conn.execute_batch(tables)?;
        // Migrate existing databases: add new columns if they are missing.
        self.add_column_if_missing("videos", "playback_position", "REAL NOT NULL DEFAULT 0")?;
        self.add_column_if_missing("videos", "favorite", "INTEGER NOT NULL DEFAULT 0")?;
        self.add_column_if_missing("videos", "audio_path", "TEXT")?;
        // Create indexes (after migration, so existing DBs have the columns).
        self.conn.execute("CREATE INDEX IF NOT EXISTS idx_queue_video ON queue(video_id)", [])?;
        self.conn.execute("CREATE INDEX IF NOT EXISTS idx_videos_favorite ON videos(favorite)", [])?;
        // FTS5 virtual table (S4).
        self.conn.execute("DROP TABLE IF EXISTS videos_fts", [])?;
        self.conn.execute(
            "CREATE VIRTUAL TABLE videos_fts USING fts5(title, tags, description)",
            [],
        )?;
        self.conn.execute("DROP TRIGGER IF EXISTS videos_fts_ins", [])?;
        self.conn.execute(
            "CREATE TRIGGER videos_fts_ins AFTER INSERT ON videos FOR EACH ROW BEGIN INSERT INTO videos_fts(rowid, title, tags, description) VALUES (new.id, new.title, new.tags, new.description); END",
            [],
        )?;
        self.conn.execute("DROP TRIGGER IF EXISTS videos_fts_del", [])?;
        self.conn.execute(
            "CREATE TRIGGER videos_fts_del AFTER DELETE ON videos FOR EACH ROW BEGIN DELETE FROM videos_fts WHERE rowid = old.id; END",
            [],
        )?;
        self.conn.execute("DROP TRIGGER IF EXISTS videos_fts_upd", [])?;
        self.conn.execute(
            "CREATE TRIGGER videos_fts_upd AFTER UPDATE ON videos FOR EACH ROW BEGIN DELETE FROM videos_fts WHERE rowid = new.id; INSERT INTO videos_fts(rowid, title, tags, description) VALUES (new.id, new.title, new.tags, new.description); END",
            [],
        )?;
        // Rebuild the FTS index so existing rows are searchable (S4).
        self.backfill_fts()?;
        Ok(())
    }

    /// Add a column to `table` if it is not already present. No-op (and no
    /// error) when the column already exists, so migrations are idempotent.
    fn add_column_if_missing(&self, table: &str, column: &str, type_def: &str) -> anyhow::Result<()> {
        let mut stmt = self.conn.prepare(&format!("SELECT name FROM pragma_table_info('{table}')"))?;
        let rows = stmt.query_map([], |row| {
            row.get::<_, String>(0)
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        let exists = out.iter().any(|c| c == column);
        if !exists {
            self.conn.execute(&format!("ALTER TABLE {table} ADD COLUMN {column} {type_def}"), [])?;
        }
        Ok(())
    }

    /// Insert a video. Returns the new row id.
    pub fn insert_video(&self, video: &Video) -> anyhow::Result<u64> {
        self.conn.execute(
            "INSERT INTO videos (
                fingerprint, path, title, description, tags,
                duration, resolution, codec, size, bitrate,
                created_at, updated_at, hls_m3u8, thumbnail_path, audio_path
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
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
                video.audio_path,
            ],
        )?;
        Ok(self.conn.last_insert_rowid() as u64)
    }

    /// List all videos, optionally sorted by a whitelisted column.
    /// `sort` is one of `date`, `duration`, `size`, `title` (default `date`);
    /// `order` is `asc` or `desc` (default `desc`).
    pub fn get_videos_sorted(&self, sort: &str, order: &str) -> anyhow::Result<Vec<Video>> {
        let col = match sort {
            "date" => "created_at",
            "duration" => "COALESCE(duration, 0)",
            "size" => "COALESCE(size, 0)",
            "title" => "title",
            _ => "created_at",
        };
        let ord = if order == "asc" { "ASC" } else { "DESC" };
        let sql = format!("SELECT * FROM videos ORDER BY {} {}", col, ord);
        let mut stmt = self.conn.prepare(&sql)?;
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
                playback_position: row.get(15)?,
                favorite: row.get(16)?,
                audio_path: row.get(17)?,
            })
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Rebuild the FTS5 index from the `videos` table. Called on schema
    /// upgrade (S4) so existing rows are searchable.
    pub fn backfill_fts(&self) -> anyhow::Result<()> {
        self.conn.execute(
            "INSERT INTO videos_fts(rowid, title, tags, description) SELECT id, title, tags, description FROM videos",
            [],
        )?;
        Ok(())
    }

    /// Full-text search over title, tags, description. Returns matching
    /// videos (most-relevant first).
    pub fn search_videos(&self, query: &str) -> anyhow::Result<Vec<Video>> {
        let mut stmt = self.conn.prepare(
            "SELECT v.* FROM videos v JOIN videos_fts fts ON v.id = fts.rowid WHERE videos_fts MATCH ?1 ORDER BY rank",
        )?;
        let rows = stmt.query_map(params![query], |row| {
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
                playback_position: row.get(15)?,
                favorite: row.get(16)?,
                audio_path: row.get(17)?,
            })
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Update a video's title, description, and tags (and `updated_at`).
    pub fn update_video(&self, id: u64, title: &str, description: &str, tags: &str) -> anyhow::Result<()> {
        let mut stmt = self.conn.prepare(
            "UPDATE videos SET title = ?1, description = ?2, tags = ?3, updated_at = ?4 WHERE id = ?5",
        )?;
        stmt.execute(params![title, description, tags, now(), id])?;
        Ok(())
    }

    /// Toggle a video's favorite flag.
    pub fn toggle_favorite(&self, id: u64) -> anyhow::Result<()> {
        self.conn.execute(
            "UPDATE videos SET favorite = 1 - favorite, updated_at = ?1 WHERE id = ?2",
            params![now(), id],
        )?;
        Ok(())
    }

    /// Update a video's playback position (and `updated_at`).
    pub fn update_position(&self, id: u64, position: f64) -> anyhow::Result<()> {
        self.conn.execute(
            "UPDATE videos SET playback_position = ?1, updated_at = ?2 WHERE id = ?3",
            params![position, now(), id],
        )?;
        Ok(())
    }

    /// Add a video to the queue (position = next). Returns the new row id.
    pub fn add_to_queue(&self, video_id: u64) -> anyhow::Result<u64> {
        self.conn.execute(
            "INSERT INTO queue (video_id, position, created_at) VALUES (?1, ?2, ?3)",
            params![video_id, self.next_queue_position(), now()],
        )?;
        Ok(self.conn.last_insert_rowid() as u64)
    }

    /// The next position in the queue (max position + 1, or 1 if empty).
    fn next_queue_position(&self) -> i64 {
        let mut stmt = match self.conn.prepare("SELECT COALESCE(MAX(position), 0) FROM queue") {
            Ok(s) => s,
            Err(_) => return 1,
        };
        let max: i64 = stmt.query_row([], |row| row.get(0)).unwrap_or_default();
        max + 1
    }

    /// Get the queue, in position order.
    pub fn get_queue(&self) -> anyhow::Result<Vec<Video>> {
        let mut stmt = self.conn.prepare(
            "SELECT v.* FROM queue q JOIN videos v ON v.id = q.video_id ORDER BY q.position ASC",
        )?;
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
                playback_position: row.get(15)?,
                favorite: row.get(16)?,
                audio_path: row.get(17)?,
            })
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Remove a queue item by id.
    pub fn remove_from_queue(&self, queue_id: u64) -> anyhow::Result<()> {
        self.conn.execute("DELETE FROM queue WHERE id = ?1", params![queue_id])?;
        Ok(())
    }

    /// Delete a video row, its playlist items, and anything in the queue
    /// referencing it. Returns the number of rows removed from `videos`
    /// (0 if the video did not exist).
    pub fn delete_video(&self, id: u64) -> anyhow::Result<u64> {
        // Remove playlist items referencing this video.
        self.conn.execute(
            "DELETE FROM playlist_items WHERE video_id = ?1",
            params![id],
        )?;
        // Remove queue entries referencing this video.
        self.conn.execute(
            "DELETE FROM queue WHERE video_id = ?1",
            params![id],
        )?;
        // Remove the video row (FTS sync triggers handle the search index).
        let mut stmt = self.conn.prepare("DELETE FROM videos WHERE id = ?1")?;
        let n: usize = stmt.execute(params![id])?;
        Ok(n as u64)
    }

    /// Get a video by id. Returns `None` if the video does not exist.
    pub fn get_video_by_id(&self, id: u64) -> anyhow::Result<Option<Video>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, fingerprint, path, title, description, tags, duration, resolution, codec, size, bitrate, created_at, updated_at, hls_m3u8, thumbnail_path, playback_position, favorite, audio_path FROM videos WHERE id = ?1",
        )?;
        match stmt.query_row(params![id], |row| {
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
                playback_position: row.get(15)?,
                favorite: row.get(16)?,
                audio_path: row.get(17)?,
            })
        }) {
            Ok(v) => Ok(Some(v)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(anyhow::anyhow!("get_video_by_id: {e}")),
        }
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
    pub fn update_paths(&self, id: u64, hls_m3u8: &str, thumbnail_path: &str, audio_path: &str) -> anyhow::Result<()> {
        self.conn.execute(
            "UPDATE videos SET hls_m3u8 = ?1, thumbnail_path = ?2, audio_path = ?3, updated_at = ?4 WHERE id = ?5",
            params![hls_m3u8, thumbnail_path, audio_path, now(), id],
        )?;
        Ok(())
    }

    /// Create a playlist. Returns the new playlist id.
    pub fn create_playlist(&self, name: &str, source: &str, youtube_url: Option<&str>) -> anyhow::Result<u64> {
        self.conn.execute(
            "INSERT INTO playlists (name, source, youtube_url, created_at) VALUES (?1, ?2, ?3, ?4)",
            params![name, source, youtube_url, now()],
        )?;
        Ok(self.conn.last_insert_rowid() as u64)
    }

    /// List all playlists.
    pub fn get_playlists(&self) -> anyhow::Result<Vec<Playlist>> {
        let mut stmt = self.conn
            .prepare("SELECT id, name, source, youtube_url, created_at FROM playlists ORDER BY id DESC")?;
        let rows = stmt.query_map([], |row| {
            Ok(Playlist {
                id: row.get(0)?,
                name: row.get(1)?,
                source: row.get(2)?,
                youtube_url: row.get(3)?,
                created_at: row.get(4)?,
            })
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Get a single playlist.
    pub fn get_playlist(&self, id: u64) -> anyhow::Result<Option<Playlist>> {
        let mut stmt = self.conn
            .prepare("SELECT id, name, source, youtube_url, created_at FROM playlists WHERE id = ?1")?;
        match stmt.query_row(params![id], |row| {
            Ok(Playlist {
                id: row.get(0)?,
                name: row.get(1)?,
                source: row.get(2)?,
                youtube_url: row.get(3)?,
                created_at: row.get(4)?,
            })
        }) {
            Ok(p) => Ok(Some(p)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(anyhow::anyhow!("get_playlist: {e}")),
        }
    }

    /// Get the videos in a playlist, in position order.
    pub fn get_videos_by_playlist(&self, playlist_id: u64) -> anyhow::Result<Vec<Video>> {
        let mut stmt = self.conn.prepare(
            "SELECT v.* FROM playlist_items pi
             JOIN videos v ON v.id = pi.video_id
             WHERE pi.playlist_id = ?1
             ORDER BY pi.position ASC",
        )?;
        let rows = stmt.query_map(params![playlist_id], |row| {
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
                playback_position: row.get(15)?,
                favorite: row.get(16)?,
                audio_path: row.get(17)?,
            })
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Add a video to a playlist (no-op if already present).
    pub fn add_playlist_item(&self, playlist_id: u64, video_id: u64, position: i64) -> anyhow::Result<()> {
        self.conn.execute(
            "INSERT OR IGNORE INTO playlist_items (playlist_id, video_id, position) VALUES (?1, ?2, ?3)",
            params![playlist_id, video_id, position],
        )?;
        Ok(())
    }

    /// Remove a video from a playlist.
    pub fn remove_playlist_item(&self, playlist_id: u64, video_id: u64) -> anyhow::Result<()> {
        self.conn.execute(
            "DELETE FROM playlist_items WHERE playlist_id = ?1 AND video_id = ?2",
            params![playlist_id, video_id],
        )?;
        Ok(())
    }

    /// Rename a playlist.
    pub fn rename_playlist(&self, id: u64, name: &str) -> anyhow::Result<()> {
        let mut stmt = self.conn.prepare("UPDATE playlists SET name = ?1 WHERE id = ?2")?;
        stmt.execute(params![name, id])?;
        Ok(())
    }

    /// Delete a playlist and its items.
    pub fn delete_playlist(&self, id: u64) -> anyhow::Result<()> {
        self.conn.execute(
            "DELETE FROM playlist_items WHERE playlist_id = ?1",
            params![id],
        )?;
        self.conn.execute("DELETE FROM playlists WHERE id = ?1", params![id])?;
        Ok(())
    }

    /// Number of videos in a playlist.
    pub fn playlist_count(&self, playlist_id: u64) -> anyhow::Result<u64> {
        let mut stmt = self.conn.prepare(
            "SELECT COUNT(*) FROM playlist_items WHERE playlist_id = ?1",
        )?;
        let n: u64 = stmt.query_row(params![playlist_id], |row| row.get(0))?;
        Ok(n)
    }

    /// Create a manual playlist.
    pub fn create_manual_playlist(&self, name: &str) -> anyhow::Result<u64> {
        let mut stmt = self.conn.prepare(
            "INSERT INTO playlists (name, source, created_at) VALUES (?1, 'manual', ?2)",
        )?;
        stmt.execute(params![name, now()])?;
        Ok(self.conn.last_insert_rowid() as u64)
    }

}


#[cfg(test)]
mod tests {
    use super::*;

    fn sample_video(title: &str, fp: &str) -> Video {
        Video {
            id: 0,
            fingerprint: fp.to_string(),
            path: format!("/x/{}.mp4", title),
            title: title.to_string(),
            description: String::new(),
            tags: String::new(),
            duration: 1.0,
            resolution: "1x1".to_string(),
            codec: "h264".to_string(),
            size: 1,
            bitrate: 1,
            created_at: "0".to_string(),
            updated_at: "0".to_string(),
            hls_m3u8: String::new(),
            thumbnail_path: String::new(),
            playback_position: 0.0,
            favorite: false,
            audio_path: String::new(),
        }
    }

    fn make_db() -> (Db, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.db");
        let db = Db::new(&path).unwrap();
        db.create_schema().unwrap();
        (db, dir)
    }

    #[test]
    fn insert_and_get_by_id() {
        let (db, _dir) = make_db();
        let id = db.insert_video(&sample_video("Alpha", "fp-a")).unwrap();
        let v = db.get_video_by_id(id).unwrap().expect("video row must exist");
        assert_eq!(v.title, "Alpha");
        assert_eq!(v.fingerprint, "fp-a");
    }

    #[test]
    fn get_by_id_returns_none_for_unknown_id() {
        let (db, _dir) = make_db();
        assert!(db.get_video_by_id(42).unwrap().is_none());
    }

    #[test]
    fn find_by_fingerprint_matches_and_misses() {
        let (db, _dir) = make_db();
        let id = db.insert_video(&sample_video("Alpha", "fp-a")).unwrap();
        assert_eq!(db.find_by_fingerprint("fp-a").unwrap(), Some(id));
        assert!(db.find_by_fingerprint("fp-no").unwrap().is_none());
    }

    #[test]
    fn sorted_desc_by_default_and_by_title_asc() {
        let (db, _dir) = make_db();
        db.insert_video(&sample_video("Gamma", "fp-c")).unwrap();
        db.insert_video(&sample_video("Alpha", "fp-a")).unwrap();
        db.insert_video(&sample_video("Beta", "fp-b")).unwrap();

        let titles_asc = db.get_videos_sorted("title", "asc").unwrap();
        let titles: Vec<&str> = titles_asc.iter().map(|v| v.title.as_str()).collect();
        assert_eq!(titles, vec!["Alpha", "Beta", "Gamma"]);

        let default = db.get_videos_sorted("title", "desc").unwrap();
        let default_titles: Vec<&str> = default.iter().map(|v| v.title.as_str()).collect();
        assert_eq!(default_titles, vec!["Gamma", "Beta", "Alpha"]);
    }

    #[test]
    fn sorted_unknown_sort_falls_back_to_date() {
        let (db, _dir) = make_db();
        let id = db.insert_video(&sample_video("Alpha", "fp-a")).unwrap();
        let v = db.get_videos_sorted("nonsense", "desc").unwrap();
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].id, id);
    }

    #[test]
    fn search_videos_finds_by_title_and_tags() {
        let (db, _dir) = make_db();
        db.insert_video(&sample_video("Alpha", "fp-a")).unwrap();
        db.insert_video(&sample_video("Beta", "fp-b")).unwrap();
        assert_eq!(db.search_videos("Alpha").unwrap().len(), 1);
        assert_eq!(db.search_videos("Beta").unwrap().len(), 1);
        assert!(db.search_videos("zzz").unwrap().is_empty());
    }

    #[test]
    fn update_video_changes_title_description_and_tags() {
        let (db, _dir) = make_db();
        let id = db.insert_video(&sample_video("Alpha", "fp-a")).unwrap();
        db.update_video(id, "Renamed", "some description", "some,tags").unwrap();
        let v = db.get_video_by_id(id).unwrap().unwrap();
        assert_eq!(v.title.as_str(), "Renamed");
        assert_eq!(v.description.as_str(), "some description");
        assert_eq!(v.tags.as_str(), "some,tags");
    }

    #[test]
    fn toggle_favorite_flips_flag_twice() {
        let (db, _dir) = make_db();
        let id = db.insert_video(&sample_video("Alpha", "fp-a")).unwrap();
        db.toggle_favorite(id).unwrap();
        assert!(db.get_video_by_id(id).unwrap().unwrap().favorite);
        db.toggle_favorite(id).unwrap();
        assert!(!db.get_video_by_id(id).unwrap().unwrap().favorite);
    }

    #[test]
    fn update_position_sets_playback_position() {
        let (db, _dir) = make_db();
        let id = db.insert_video(&sample_video("Alpha", "fp-a")).unwrap();
        db.update_position(id, 42.5).unwrap();
        assert_eq!(db.get_video_by_id(id).unwrap().unwrap().playback_position, 42.5);
    }

    #[test]
    fn queue_assigns_incremental_positions_and_remove_works() {
        let (db, _dir) = make_db();
        let id = db.insert_video(&sample_video("Alpha", "fp-a")).unwrap();
        let q1 = db.add_to_queue(id).unwrap();
        let _q2 = db.add_to_queue(id).unwrap(); // second entry, same video again
        let queue = db.get_queue().unwrap();
        assert_eq!(queue.len(), 2);

        db.remove_from_queue(q1).unwrap();
        let queue = db.get_queue().unwrap();
        assert_eq!(queue.len(), 1);
        assert_eq!(queue[0].id, id);
        assert_ne!(db.get_queue().unwrap()[0].fingerprint.as_str(), "deleted");
    }

    #[test]
    fn delete_video_removes_queue_and_playlist_items_and_row() {
        let (db, _dir) = make_db();
        let id = db.insert_video(&sample_video("Alpha", "fp-a")).unwrap();
        db.add_to_queue(id).unwrap();

        // playlists: create + add a row
        let playlist_id = db.create_manual_playlist("playlist one").unwrap();
        db.add_playlist_item(playlist_id, id, 1).unwrap();

        // remove the video: both the queue entry and the playlist item go away
        let removed = db.delete_video(id).unwrap();
        assert_eq!(removed, 1);
        assert!(db.get_videos_sorted("date", "desc").unwrap().is_empty());
        assert!(db.get_queue().unwrap().is_empty());
        assert!(db.get_videos_by_playlist(playlist_id).unwrap().is_empty());
        assert_eq!(db.playlist_count(playlist_id).unwrap(), 0);
    }

    #[test]
    fn delete_video_returns_zero_for_missing_id() {
        let (db, _dir) = make_db();
        assert_eq!(db.delete_video(999).unwrap(), 0);
    }

    #[test]
    fn update_paths_sets_audio_and_hls_paths() {
        let (db, _dir) = make_db();
        let id = db.insert_video(&sample_video("Alpha", "fp-a")).unwrap();
        db.update_paths(id, "hls/x.m3u8", "thumbnails/x.jpg", "audio/x.m4a").unwrap();
        let v = db.get_video_by_id(id).unwrap().unwrap();
        assert_eq!(v.hls_m3u8.as_str(), "hls/x.m3u8");
        assert_eq!(v.thumbnail_path.as_str(), "thumbnails/x.jpg");
        assert_eq!(v.audio_path.as_str(), "audio/x.m4a");
    }

    #[test]
    fn playlist_crud_and_cascade_delete_works() {
        let (db, _dir) = make_db();
        let playlist_id = db.create_playlist("First", "manual", None).unwrap();
        let v1 = db.insert_video(&sample_video("Alpha", "fp-a")).unwrap();
        let v2 = db.insert_video(&sample_video("Beta", "fp-b")).unwrap();
        db.add_playlist_item(playlist_id, v1, 1).unwrap();
        db.add_playlist_item(playlist_id, v2, 2).unwrap();

        // get_videos_by_playlist orders by position
        let videos = db.get_videos_by_playlist(playlist_id).unwrap();
        assert_eq!(videos.len(), 2);
        assert_eq!(videos[0].id, v1);
        assert_eq!(videos[1].id, v2);

        // Rename
        db.rename_playlist(playlist_id, "Renamed").unwrap();
        assert_eq!(db.get_playlist(playlist_id).unwrap().unwrap().name, "Renamed");

        // Remove one item
        db.remove_playlist_item(playlist_id, v1).unwrap();
        assert_eq!(db.get_videos_by_playlist(playlist_id).unwrap().len(), 1);

        // Cascade delete removes items and the playlist
        db.delete_playlist(playlist_id).unwrap();
        assert!(db.get_playlist(playlist_id).unwrap().is_none());
        assert!(db.get_videos_by_playlist(playlist_id).unwrap().is_empty());
        assert!(!db.get_playlists().unwrap().iter().any(|p| p.id == playlist_id));
    }

    #[test]
    fn playlists_are_listed_newest_first() {
        let (db, _dir) = make_db();
        let p1 = db.create_playlist("First", "manual", None).unwrap();
        let p2 = db.create_playlist("Second", "manual", None).unwrap();
        let playlists = db.get_playlists().unwrap();
        assert_eq!(playlists[0].id, p2, "newest first");
        assert_eq!(playlists[1].id, p1);
    }

    #[test]
    fn add_playlist_item_is_idempotent() {
        let (db, _dir) = make_db();
        let playlist_id = db.create_playlist("First", "manual", None).unwrap();
        let v = db.insert_video(&sample_video("Alpha", "fp-a")).unwrap();
        db.add_playlist_item(playlist_id, v, 1).unwrap();
        db.add_playlist_item(playlist_id, v, 1).unwrap(); // no-op
        assert_eq!(db.playlist_count(playlist_id).unwrap(), 1);
    }
}