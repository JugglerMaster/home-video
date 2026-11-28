use home_video::db::{Db, Video};

// Keep temp dirs alive for the whole test run (a TempDir drops its contents
// when it goes out of scope, which would delete the DB mid-test).
static TEMP_DIRS:
    std::sync::Mutex<Vec<tempfile::TempDir>> = std::sync::Mutex::new(Vec::new());

fn make_db() -> (Db, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.db");
    TEMP_DIRS.lock().unwrap().push(dir);
    let db = Db::new(&path).unwrap();
    db.create_schema().unwrap();
    (db, path)
}

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
        codec: "x".to_string(),
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

#[tokio::test]
async fn delete_video_updates_fts() {
    let (db, _path) = make_db();
    let id = db
        .insert_video(&sample_video("Alpha", "fp1"))
        .unwrap();
    // This is the line that failed in the container:
    // "no such column: deleted.id"
    let n = db.delete_video(id).expect("delete_video should succeed");
    assert_eq!(n, 1);
    assert!(db.get_videos_sorted("date", "desc").unwrap().is_empty());
}

#[tokio::test]
async fn delete_video_updates_fts_searchable() {
    let (db, _path) = make_db();
    let id1 = db.insert_video(&sample_video("Alpha", "fp1")).unwrap();
    let _id2 = db.insert_video(&sample_video("Beta", "fp2")).unwrap();

    assert_eq!(db.search_videos("Alpha").unwrap().len(), 1);
    assert_eq!(db.search_videos("Beta").unwrap().len(), 1);

    db.delete_video(id1).unwrap();
    assert_eq!(db.search_videos("Alpha").unwrap().len(), 0);
    assert_eq!(db.search_videos("Beta").unwrap().len(), 1);
    assert_eq!(db.get_videos_sorted("date", "desc").unwrap().len(), 1);
}
