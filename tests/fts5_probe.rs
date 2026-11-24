use rusqlite::Connection;

static TEMP_DIRS: std::sync::Mutex<Vec<tempfile::TempDir>> =
    std::sync::Mutex::new(Vec::new());

fn open() -> (Connection, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.db");
    let conn = Connection::open(&path).unwrap();
    TEMP_DIRS.lock().unwrap().push(dir);
    (conn, path)
}

#[tokio::test]
async fn old_id_works_on_plain_table() {
    let (conn, _path) = open();
    conn.execute_batch(
        "CREATE TABLE videos (id INTEGER PRIMARY KEY, title TEXT); \
         CREATE TABLE videos_fts (rowid INTEGER, title TEXT);",
    )
        .unwrap();
    conn.execute("INSERT INTO videos(id, title) VALUES (1, 'x')", [])
        .unwrap();
    conn.execute("INSERT INTO videos_fts(rowid, title) VALUES (1, 'x')", [])
        .unwrap();
    conn.execute(
        "CREATE TRIGGER t AFTER DELETE ON videos FOR EACH ROW \
         BEGIN DELETE FROM videos_fts WHERE rowid = old.id; END",
        [],
    )
        .unwrap();
    match conn.execute("DELETE FROM videos WHERE id = 1", []) {
        Ok(_) => println!("old.id (plain) DELETE ok"),
        Err(e) => eprintln!("old.id (plain) DELETE failed: {e}"),
    }
}

#[tokio::test]
async fn old_id_works_on_fts5() {
    let (conn, _path) = open();
    conn.execute_batch("CREATE TABLE videos (id INTEGER PRIMARY KEY, title TEXT);")
        .unwrap();
    conn.execute("CREATE VIRTUAL TABLE videos_fts USING fts5(title)", [])
        .unwrap();
    conn.execute("INSERT INTO videos(id, title) VALUES (1, 'x')", [])
        .unwrap();
    conn.execute("INSERT INTO videos_fts(rowid, title) VALUES (1, 'x')", [])
        .unwrap();
    conn.execute(
        "CREATE TRIGGER t AFTER DELETE ON videos FOR EACH ROW \
         BEGIN DELETE FROM videos_fts WHERE rowid = old.id; END",
        [],
    )
        .unwrap();
    match conn.execute("DELETE FROM videos WHERE id = 1", []) {
        Ok(_) => println!("old.id (fts5) DELETE ok"),
        Err(e) => eprintln!("old.id (fts5) DELETE failed: {e}"),
    }
}
