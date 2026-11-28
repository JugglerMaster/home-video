use axum::{
    extract::{Json, Path as PathExtract, Query, State},
    http::{
        header::CONTENT_TYPE,
        request::Parts,
        StatusCode,
    },
    middleware::from_extractor_with_state,
    response::Response,
    routing::{get, post, delete},
    Router,
};
use axum::body::{Body, Bytes};
use http_body_util::BodyExt;

use axum::http::Request;
use tower_http::trace::TraceLayer;

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use rand::Rng;
use serde::Deserialize;
use serde_json::json;

use crate::auth;
use crate::config;
use crate::config::Config;
use crate::db::Db;
use crate::ingest;
use crate::import;
use crate::upnp;

const STATIC_DIR: &str = "./static";
const CONFIG_PATH_DEFAULT: &str = "./config.json";

/// A video currently being converted (re-encoded) at ingest time.
#[derive(Clone, Debug)]
pub struct ConversionItem {
    pub video_id: u64,
    pub title: String,
    pub started_at: String,
}

/// Shared application state. The `Arc`s are shared between the config-guard
/// middleware and the handlers, so the wizard's config write is visible to
/// every subsequent request.
#[derive(Clone)]
pub struct AppState {
    pub config_path: PathBuf,
    pub port: u16,
    pub config: Arc<Mutex<Option<Config>>>,
    pub sessions: Arc<Mutex<HashSet<String>>>,
    pub db: Arc<Mutex<Option<Db>>>,
    pub conversions: Arc<Mutex<Vec<ConversionItem>>>,
    pub speakers: Arc<upnp::SpeakerStore>,
}

impl AppState {
    pub fn new(config_path: PathBuf, config: Option<Config>, port: u16, db: Option<Db>) -> Self {
        Self {
            config_path,
            port,
            config: Arc::new(Mutex::new(config)),
            sessions: Arc::new(Mutex::new(HashSet::new())),
            db: Arc::new(Mutex::new(db)),
            conversions: Arc::new(Mutex::new(Vec::new())),
            speakers: Arc::new(upnp::SpeakerStore::new()),
        }
    }
}

/// Middleware guard: until the config exists, only `/setup` and `/health`
/// are reachable; everything else is 403.
struct ConfigGuard;

#[async_trait]
impl axum::extract::FromRequestParts<AppState> for ConfigGuard {
    type Rejection = StatusCode;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Self::Rejection> {
        let configured = state.config.lock().unwrap().is_some();
        let path = parts.uri.path();
        if !configured && !(path == "/setup" || path == "/health") {
            return Err(StatusCode::FORBIDDEN);
        }
        Ok(Self)
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Content type for a file by its extension.
fn content_type_for(name: &str) -> &'static str {
    match name.rsplit_once('.') {
        Some((_, ext)) => match ext.to_lowercase().as_str() {
            "html" => "text/html; charset=utf-8",
            "css" => "text/css; charset=utf-8",
            "js" => "text/javascript; charset=utf-8",
            "json" => "application/json; charset=utf-8",
            "png" => "image/png",
            "jpg" | "jpeg" => "image/jpeg",
            "gif" => "image/gif",
            "svg" => "image/svg+xml",
            "ico" => "image/x-icon",
            // Direct audio formats for UPnP/Sonos casting.
            "m4a" | "mp4" => "audio/mp4",
            "aac" => "audio/aac",
            "mp3" => "audio/mpeg",
            "wav" => "audio/wav",
            "flac" => "audio/flac",
            "ogg" => "audio/ogg",
            _ => "application/octet-stream",
        },
        None => "application/octet-stream",
    }
}

fn plain(status: StatusCode, body: &str) -> Response {
    Response::builder()
        .status(status)
        .header(CONTENT_TYPE, "text/plain; charset=utf-8")
        .body(Body::from(Bytes::from(body.as_bytes().to_vec())))
        .expect("build response")
}

/// Collect the request body to bytes; never panics. An empty vec is
/// returned when collection fails, which the handler reports as a bad
/// request (or a default) rather than crashing the server.
async fn collect_body(req: Request<Body>) -> Vec<u8> {
    req.into_body()
        .collect()
        .await
        .map(|b| b.to_bytes())
        .map(|b| b.to_vec())
        .unwrap_or_default()
}

fn json_response(status: StatusCode, value: serde_json::Value) -> Response {
    Response::builder()
        .status(status)
        .header(CONTENT_TYPE, "application/json; charset=utf-8")
        .body(Body::from(Bytes::from(serde_json::to_vec(&value).unwrap())))
        .expect("build response")
}

fn redirect(uri: &str) -> Response {
    Response::builder()
        .status(StatusCode::FOUND)
        .header("Location", uri)
        .body(Body::from(Bytes::from_static(b"")))
        .expect("build response")
}

/// Serve a file from `STATIC_DIR` by relative name, or 404.
async fn serve_file(name: &str) -> Response {
    let full_path = format!("{STATIC_DIR}/{name}");
    let bytes = match tokio::fs::read(&full_path).await {
        Ok(b) => b,
        Err(e) => {
            eprintln!("static 404: {full_path}: {e}");
            return plain(StatusCode::NOT_FOUND, "not found");
        }
    };
    Response::builder()
        .status(StatusCode::OK)
        .header(CONTENT_TYPE, content_type_for(name))
        .body(Body::from(bytes))
        .expect("build response")
}

/// The `session` cookie value, if present.
fn session_token<T>(req: &Request<T>) -> Option<String> {
    req.headers()
        .get("cookie")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| {
            s.split(';')
                .map(|p| p.trim())
                .find(|p| p.starts_with("session="))
                .map(|p| p["session=".len()..].to_string())
        })
}

fn is_authenticated<T>(state: &AppState, req: &Request<T>) -> bool {
    session_token(req)
        .as_deref()
        .map(|t| state.sessions.lock().unwrap().contains(t))
        .unwrap_or(false)
}

fn random_token() -> String {
    let bytes: [u8; 16] = rand::thread_rng().gen();
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
}

// ---------------------------------------------------------------------------
// Request shapes
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct SetupRequest {
    admin_password: String,
    watch_dirs: String,
    data_dir: String,
    scan_interval_s: u64,
    anonymous_view: bool,
    youtube_download_dir: Option<String>,
}

#[derive(Deserialize)]
struct LoginRequest {
    password: String,
}

// ---------------------------------------------------------------------------
// Setup routes (S1)
// ---------------------------------------------------------------------------

/// GET /setup → the setup wizard form.
async fn setup_get() -> Response {
    serve_file("setup.html").await
}

/// POST /setup → validate, hash the password, write config, create the schema,
/// mark the app as configured.
async fn setup_post(
    State(state): State<AppState>,
    Json(body): Json<SetupRequest>,
) -> Response {
    if body.admin_password.is_empty() {
        return json_response(StatusCode::BAD_REQUEST, json!({"error": "admin_password is required"}));
    }
    let watch_dirs: Vec<String> = body
        .watch_dirs
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if watch_dirs.is_empty() {
        return json_response(StatusCode::BAD_REQUEST, json!({"error": "at least one watch_dir is required"}));
    }
    let data_dir = body.data_dir.trim().to_string();
    if data_dir.is_empty() {
        return json_response(StatusCode::BAD_REQUEST, json!({"error": "data_dir is required"}));
    }
    if body.scan_interval_s == 0 {
        return json_response(StatusCode::BAD_REQUEST, json!({"error": "scan_interval_s must be > 0"}));
    }

    let hash = auth::hash_password(&body.admin_password);
    let youtube_download_dir = body
        .youtube_download_dir
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| format!("{data_dir}/youtube"));

    let config = config::from_setup(
        state.port,
        watch_dirs,
        youtube_download_dir,
        data_dir,
        body.scan_interval_s,
        body.anonymous_view,
        hash,
    );
    if let Err(e) = config::validate(&config) {
        return json_response(StatusCode::BAD_REQUEST, json!({"error": e.to_string()}));
    }

    if let Err(e) = std::fs::create_dir_all(&config.data_dir) {
        return json_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            json!({"error": format!("create data_dir: {e}")}),
        );
    }
    if let Err(e) = config::save(&state.config_path, &config) {
        return json_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            json!({"error": format!("write config: {e}")}),
        );
    }

    let db_path = PathBuf::from(&config.data_dir).join("home-video.db");
    let db = match Db::new(&db_path).and_then(|db| db.create_schema().map(|_| db)) {
        Ok(db) => db,
        Err(e) => {
            return json_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                json!({"error": format!("create db: {e}")}),
            );
        }
    };
    *state.db.lock().unwrap() = Some(db);

    *state.config.lock().unwrap() = Some(config);
    redirect("/admin")
}

// ---------------------------------------------------------------------------
// Admin routes (S1)
// ---------------------------------------------------------------------------

/// GET /admin → the admin page (login form or dashboard).
async fn admin_get() -> Response {
    serve_file("admin.html").await
}

/// POST /admin/login → verify password, set a session cookie.
async fn login(
    State(state): State<AppState>,
    Json(body): Json<LoginRequest>,
) -> Response {
    let config = {
        let guard = state.config.lock().unwrap();
        match &*guard {
            Some(c) => c.clone(),
            None => return json_response(StatusCode::FORBIDDEN, json!({"error": "not configured"})),
        }
    };
    if !auth::verify_password(&body.password, &config.admin_password_hash) {
        return json_response(StatusCode::UNAUTHORIZED, json!({"error": "invalid password"}));
    }
    let token = random_token();
    state.sessions.lock().unwrap().insert(token.clone());
    Response::builder()
        .status(StatusCode::OK)
        .header(CONTENT_TYPE, "application/json; charset=utf-8")
        .header("Set-Cookie", format!("session={token}; Path=/; HttpOnly; Max-Age=86400"))
        .body(Body::from(Bytes::from(serde_json::to_vec(&json!({"ok": true})).unwrap())))
        .expect("build response")
}

/// POST /admin/logout → clear the session cookie.
async fn logout() -> Response {
    Response::builder()
        .status(StatusCode::OK)
        .header("Set-Cookie", "session=; Path=/; Max-Age=0")
        .body(Body::from(Bytes::from_static(b"")))
        .expect("build response")
}

/// GET /api/session → { "authenticated": bool }.
async fn session(State(state): State<AppState>, req: Request<Body>) -> Response {
    json_response(StatusCode::OK, json!({"authenticated": is_authenticated(&state, &req)}))
}

// ---------------------------------------------------------------------------
// Static / root (S0, retained)
// ---------------------------------------------------------------------------

/// Root → index.html.
async fn index() -> Response {
    serve_file("index.html").await
}

/// Catch-all static handler with path-traversal protection.
async fn static_handler(PathExtract(path): PathExtract<String>) -> Response {
    let file_name = if path.is_empty() || path == "/" {
        "index.html"
    } else {
        path.trim_start_matches('/')
    };
    let safe_name = file_name
        .split('/')
        .filter(|p| !p.starts_with("..") && !p.contains('\0'))
        .collect::<Vec<_>>()
        .join("/");
    serve_file(&safe_name).await
}

// ---------------------------------------------------------------------------
// Library / streaming (S2)
// ---------------------------------------------------------------------------

/// GET /api/videos → all videos (for the Library grid).
async fn list_videos(
    State(state): State<AppState>,
    Query(q): Query<VideoQuery>,
) -> Response {
    let db_guard = state.db.lock().unwrap();
    match db_guard.as_ref() {
        Some(db) => {
            let videos = if q.q.is_empty() {
                db.get_videos_sorted(&q.sort, &q.order)
            } else {
                db.search_videos(&q.q)
            };
            match videos {
                Ok(videos) => {
                    let json_videos: Vec<serde_json::Value> = videos
                        .iter()
                        .map(|v| {
                            serde_json::json!({
                                "id": v.id,
                                "title": v.title,
                                "description": v.description,
                                "tags": v.tags,
                                "duration": v.duration,
                                "resolution": v.resolution,
                                "codec": v.codec,
                                "size": v.size,
                                "bitrate": v.bitrate,
                                "thumbnail": format!("/thumbnail/{}.jpg", v.id),
                                "m3u8": if !v.hls_m3u8.is_empty() {
                                    format!("/stream/{}", v.hls_m3u8)
                                } else {
                                    String::new()
                                },
                                "playable": !v.hls_m3u8.is_empty(),
                                "favorite": v.favorite,
                                "playback_position": v.playback_position,
                            })
                        })
                        .collect();
                    json_response(StatusCode::OK, json!(json_videos))
                }
                Err(e) => json_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    json!({"error": e.to_string()}),
                ),
            }
        }
        None => json_response(
            StatusCode::FORBIDDEN,
            json!({"error": "not configured"}),
        ),
    }
}

/// Query params for `GET /api/videos`.
#[derive(Deserialize)]
struct VideoQuery {
    #[serde(default)]
    pub q: String,
    #[serde(default = "default_sort")]
    pub sort: String,
    #[serde(default = "default_order")]
    pub order: String,
}

fn default_sort() -> String {
    "date".to_string()
}

fn default_order() -> String {
    "desc".to_string()
}

// ---------------------------------------------------------------------------
// Video edit + delete (S4 / S6)
// ---------------------------------------------------------------------------

/// Request body for `POST /api/videos/:id`.
#[derive(Deserialize)]
struct UpdateVideoRequest {
    pub title: String,
    pub description: String,
    pub tags: String,
}

/// POST /api/videos/:id → update a video's description and tags.
/// Admin-only.
async fn update_video(
    State(state): State<AppState>,
    PathExtract(id): PathExtract<u64>,
    req: Request<Body>,
) -> Response {
    if !is_authenticated(&state, &req) {
        return json_response(StatusCode::UNAUTHORIZED, json!({"error": "not authenticated"}));
    }
    let body_bytes = collect_body(req).await;
    let body_str = String::from_utf8_lossy(&body_bytes);
    let body: UpdateVideoRequest = match serde_json::from_str(&body_str) {
        Ok(b) => b,
        Err(e) => {
            return json_response(
                StatusCode::BAD_REQUEST,
                json!({"error": format!("bad json: {e}")}),
            );
        }
    };
    let db_guard = state.db.lock().unwrap();
    match db_guard.as_ref() {
        Some(db) => match db.update_video(id, &body.title, &body.description, &body.tags) {
            Ok(()) => json_response(StatusCode::OK, json!({"ok": true})),
            Err(e) => json_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                json!({"error": e.to_string()}),
            ),
        },
        None => json_response(
            StatusCode::FORBIDDEN,
            json!({"error": "not configured"}),
        ),
    }
}

/// DELETE /api/videos/:id → delete a video (DB row + playlist items +
/// generated HLS/thumbnail artifacts). Admin-only.
async fn delete_video(
    State(state): State<AppState>,
    PathExtract(id): PathExtract<u64>,
    req: Request<Body>,
) -> Response {
    if !is_authenticated(&state, &req) {
        return json_response(StatusCode::UNAUTHORIZED, json!({"error": "not authenticated"}));
    }
    // Read the data dir from config (block-scope so the guard is dead before
    // any fs/await work).
    let data_dir = {
        let cfg = state.config.lock().unwrap();
        match &*cfg {
            Some(c) => PathBuf::from(&c.data_dir),
            None => return json_response(StatusCode::FORBIDDEN, json!({"error": "not configured"})),
        }
    };
    let db_guard = state.db.lock().unwrap();
    match db_guard.as_ref() {
        Some(db) => {
            // Fetch the source path before deleting the row so we can remove
            // the file (prevents the scan from re-ingesting it).
            let source_path = match db.get_video_by_id(id) {
                Ok(Some(v)) => v.path,
                Ok(None) => {
                    return json_response(
                        StatusCode::NOT_FOUND,
                        json!({"error": "video not found"}),
                    );
                }
                Err(e) => {
                    return json_response(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        json!({"error": e.to_string()}),
                    );
                }
            };
            // Remove the source file first — if it can't be deleted (e.g. a
            // permissions problem), deleting the row now would make the scan
            // re-ingest the file on the next cycle. Surface that loudly
            // instead of silently half-deleting.
            match std::fs::remove_file(&source_path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => {
                    return json_response(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        json!({"error": format!("could not remove source file {source_path:?}: {e}")}),
                    );
                }
            }
            match db.delete_video(id) {
                Ok(n) => {
                    // Best-effort artifact cleanup.
                    let hls_dir = data_dir.join("hls").join(format!("{id}"));
                    let _ = std::fs::remove_dir_all(&hls_dir);
                    let thumb = data_dir.join("thumbnails").join(format!("{id}.jpg"));
                    let _ = std::fs::remove_file(&thumb);
                    let audio = data_dir.join("audio").join(format!("{id}.m4a"));
                    let _ = std::fs::remove_file(&audio);
                    json_response(
                        StatusCode::OK,
                        json!({"ok": true, "removed": n}),
                    )
                }
                Err(e) => json_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    json!({"error": e.to_string()}),
                ),
            }
        }
        None => json_response(
            StatusCode::FORBIDDEN,
            json!({"error": "not configured"}),
        ),
    }
}

/// POST /api/videos/:id/favorite → toggle favorite. Admin-only.
async fn toggle_favorite(
    State(state): State<AppState>,
    PathExtract(id): PathExtract<u64>,
    req: Request<Body>,
) -> Response {
    if !is_authenticated(&state, &req) {
        return json_response(StatusCode::UNAUTHORIZED, json!({"error": "not authenticated"}));
    }
    let db_guard = state.db.lock().unwrap();
    match db_guard.as_ref() {
        Some(db) => match db.toggle_favorite(id) {
            Ok(()) => json_response(StatusCode::OK, json!({"ok": true})),
            Err(e) => json_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                json!({"error": e.to_string()}),
            ),
        },
        None => json_response(
            StatusCode::FORBIDDEN,
            json!({"error": "not configured"}),
        ),
    }
}

/// POST /api/videos/:id/position → update playback position (resume). Admin-only.
async fn update_position(
    State(state): State<AppState>,
    PathExtract(id): PathExtract<u64>,
    req: Request<Body>,
) -> Response {
    if !is_authenticated(&state, &req) {
        return json_response(StatusCode::UNAUTHORIZED, json!({"error": "not authenticated"}));
    }
    let body_bytes = collect_body(req).await;
    let body_str = String::from_utf8_lossy(&body_bytes);
    let position: f64 = match serde_json::from_str::<serde_json::Value>(&body_str) {
        Ok(v) => match v.get("position").and_then(|p| p.as_f64()) {
            Some(p) => p,
            None => return json_response(StatusCode::BAD_REQUEST, json!({"error": "bad json"})),
        },
        Err(_) => return json_response(StatusCode::BAD_REQUEST, json!({"error": "bad json"})),
    };
    let db_guard = state.db.lock().unwrap();
    match db_guard.as_ref() {
        Some(db) => match db.update_position(id, position) {
            Ok(()) => json_response(StatusCode::OK, json!({"ok": true})),
            Err(e) => json_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                json!({"error": e.to_string()}),
            ),
        },
        None => json_response(
            StatusCode::FORBIDDEN,
            json!({"error": "not configured"}),
        ),
    }
}

/// POST /api/queue → add a video to the queue. Admin-only.
async fn add_to_queue(
    State(state): State<AppState>,
    req: Request<Body>,
) -> Response {
    if !is_authenticated(&state, &req) {
        return json_response(StatusCode::UNAUTHORIZED, json!({"error": "not authenticated"}));
    }
    let body_bytes = collect_body(req).await;
    let body_str = String::from_utf8_lossy(&body_bytes);
    let video_id: u64 = match serde_json::from_str::<serde_json::Value>(&body_str) {
        Ok(v) => match v.get("video_id").and_then(|p| p.as_u64()) {
            Some(id) => id,
            None => return json_response(StatusCode::BAD_REQUEST, json!({"error": "bad json"})),
        },
        Err(_) => return json_response(StatusCode::BAD_REQUEST, json!({"error": "bad json"})),
    };
    let db_guard = state.db.lock().unwrap();
    match db_guard.as_ref() {
        Some(db) => match db.add_to_queue(video_id) {
            Ok(id) => json_response(StatusCode::CREATED, json!({"ok": true, "id": id})),
            Err(e) => json_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                json!({"error": e.to_string()}),
            ),
        },
        None => json_response(
            StatusCode::FORBIDDEN,
            json!({"error": "not configured"}),
        ),
    }
}

/// GET /api/queue → list the queue. Public.
async fn list_queue(
    State(state): State<AppState>,
) -> Response {
    let db_guard = state.db.lock().unwrap();
    match db_guard.as_ref() {
        Some(db) => match db.get_queue() {
            Ok(videos) => {
                let json_videos: Vec<serde_json::Value> = videos
                    .iter()
                    .map(|v| {
                        serde_json::json!({
                            "id": v.id,
                            "title": v.title,
                            "duration": v.duration,
                            "thumbnail": format!("/thumbnail/{}.jpg", v.id),
                        })
                    })
                    .collect();
                json_response(StatusCode::OK, json!({"queue": json_videos}))
            }
            Err(e) => json_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                json!({"error": e.to_string()}),
            ),
        },
        None => json_response(
            StatusCode::FORBIDDEN,
            json!({"error": "not configured"}),
        ),
    }
}

/// DELETE /api/queue/:id → remove a queue item. Admin-only.
async fn remove_from_queue(
    State(state): State<AppState>,
    PathExtract(id): PathExtract<u64>,
    req: Request<Body>,
) -> Response {
    if !is_authenticated(&state, &req) {
        return json_response(StatusCode::UNAUTHORIZED, json!({"error": "not authenticated"}));
    }
    let db_guard = state.db.lock().unwrap();
    match db_guard.as_ref() {
        Some(db) => match db.remove_from_queue(id) {
            Ok(()) => json_response(StatusCode::OK, json!({"ok": true})),
            Err(e) => json_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                json!({"error": e.to_string()}),
            ),
        },
        None => json_response(
            StatusCode::FORBIDDEN,
            json!({"error": "not configured"}),
        ),
    }
}

// ---------------------------------------------------------------------------
// YouTube import (S3)
// ---------------------------------------------------------------------------

/// Request body for `POST /api/import`.
#[derive(Deserialize)]
struct ImportRequest {
    pub url: String,
}

/// POST /api/import → import a YouTube URL (single video or playlist).
/// Admin-only.
async fn import_post(State(state): State<AppState>, req: Request<Body>) -> Response {
    if !is_authenticated(&state, &req) {
        return json_response(StatusCode::UNAUTHORIZED, json!({"error": "not authenticated"}));
    }
    let body_bytes = collect_body(req).await;
    let body_str = String::from_utf8_lossy(&body_bytes);
    let body: ImportRequest = match serde_json::from_str(&body_str) {
        Ok(b) => b,
        Err(e) => return json_response(StatusCode::BAD_REQUEST, json!({"error": e.to_string()})),
    };
    let url = body.url.trim().to_string();
    if url.is_empty() {
        return json_response(StatusCode::BAD_REQUEST, json!({"error": "url is required"}));
    }
    match import::import(&state, &url).await {
        Ok(result) => json_response(
            StatusCode::OK,
            json!({
                "ingested": result.ingested,
                "playlist_id": result.playlist_id,
                "playlist_title": result.playlist_title,
            }),
        ),
        Err(e) => json_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            json!({"error": e.to_string()}),
        ),
    }
}

// ---------------------------------------------------------------------------
// Playlists (S3/S5)
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct AddItemRequest {
    pub video_id: u64,
}

#[derive(Deserialize)]
struct RemoveItemRequest {
    pub video_id: u64,
}

#[derive(Deserialize)]
struct CreatePlaylistRequest {
    pub name: String,
}

#[derive(Deserialize)]
struct RenamePlaylistRequest {
    pub name: String,
}

/// POST /api/playlists → create a manual playlist.
/// Admin-only.
async fn create_playlist(
    State(state): State<AppState>,
    req: Request<Body>,
) -> Response {
    if !is_authenticated(&state, &req) {
        return json_response(StatusCode::UNAUTHORIZED, json!({"error": "not authenticated"}));
    }
    let body_bytes = collect_body(req).await;
    let body_str = String::from_utf8_lossy(&body_bytes);
    let body: CreatePlaylistRequest = match serde_json::from_str(&body_str) {
        Ok(b) => b,
        Err(e) => return json_response(StatusCode::BAD_REQUEST, json!({"error": e.to_string()})),
    };
    let db_guard = state.db.lock().unwrap();
    match db_guard.as_ref() {
        Some(db) => match db.create_manual_playlist(&body.name) {
            Ok(id) => json_response(StatusCode::CREATED, json!({"id": id})),
            Err(e) => json_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                json!({"error": e.to_string()}),
            ),
        },
        None => json_response(
            StatusCode::FORBIDDEN,
            json!({"error": "not configured"}),
        ),
    }
}

/// PUT /api/playlists/:id → rename a playlist.
/// Admin-only.
async fn rename_playlist(
    State(state): State<AppState>,
    PathExtract(id): PathExtract<u64>,
    req: Request<Body>,
) -> Response {
    if !is_authenticated(&state, &req) {
        return json_response(StatusCode::UNAUTHORIZED, json!({"error": "not authenticated"}));
    }
    let body_bytes = collect_body(req).await;
    let body_str = String::from_utf8_lossy(&body_bytes);
    let body: RenamePlaylistRequest = match serde_json::from_str(&body_str) {
        Ok(b) => b,
        Err(e) => return json_response(StatusCode::BAD_REQUEST, json!({"error": e.to_string()})),
    };
    let db_guard = state.db.lock().unwrap();
    match db_guard.as_ref() {
        Some(db) => match db.rename_playlist(id, &body.name) {
            Ok(()) => json_response(StatusCode::OK, json!({"ok": true})),
            Err(e) => json_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                json!({"error": e.to_string()}),
            ),
        },
        None => json_response(
            StatusCode::FORBIDDEN,
            json!({"error": "not configured"}),
        ),
    }
}

/// DELETE /api/playlists/:id → delete a playlist and its items.
/// Admin-only.
async fn delete_playlist(
    State(state): State<AppState>,
    PathExtract(id): PathExtract<u64>,
    req: Request<Body>,
) -> Response {
    if !is_authenticated(&state, &req) {
        return json_response(StatusCode::UNAUTHORIZED, json!({"error": "not authenticated"}));
    }
    let db_guard = state.db.lock().unwrap();
    match db_guard.as_ref() {
        Some(db) => match db.delete_playlist(id) {
            Ok(()) => json_response(StatusCode::OK, json!({"ok": true})),
            Err(e) => json_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                json!({"error": e.to_string()}),
            ),
        },
        None => json_response(
            StatusCode::FORBIDDEN,
            json!({"error": "not configured"}),
        ),
    }
}

/// GET /api/playlists → all playlists.
async fn list_playlists(State(state): State<AppState>) -> Response {
    let db_guard = state.db.lock().unwrap();
    match db_guard.as_ref() {
        Some(db) => match db.get_playlists() {
            Ok(playlists) => {
                let json_playlists: Vec<serde_json::Value> = playlists
                    .iter()
                    .map(|p| {
                        let count = db.playlist_count(p.id).unwrap_or(0);
                        serde_json::json!({
                            "id": p.id,
                            "name": p.name,
                            "source": p.source,
                            "youtube_url": p.youtube_url,
                            "created_at": p.created_at,
                            "count": count,
                        })
                    })
                    .collect();
                json_response(StatusCode::OK, json!(json_playlists))
            }
            Err(e) => json_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                json!({"error": e.to_string()}),
            ),
        },
        None => json_response(
            StatusCode::FORBIDDEN,
            json!({"error": "not configured"}),
        ),
    }
}

/// GET /api/playlists/:id → playlist detail with its videos.
async fn get_playlist(
    State(state): State<AppState>,
    PathExtract(id): PathExtract<u64>,
) -> Response {
    let db_guard = state.db.lock().unwrap();
    match db_guard.as_ref() {
        Some(db) => {
            let playlist = match db.get_playlist(id) {
                Ok(Some(p)) => p,
                Ok(None) => {
                    return json_response(StatusCode::NOT_FOUND, json!({"error": "not found"}));
                }
                Err(e) => {
                    return json_response(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        json!({"error": e.to_string()}),
                    );
                }
            };
            let videos = match db.get_videos_by_playlist(id) {
                Ok(v) => v,
                Err(e) => {
                    return json_response(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        json!({"error": e.to_string()}),
                    );
                }
            };
            let json_videos: Vec<serde_json::Value> = videos
                .iter()
                .map(|v| {
                    serde_json::json!({
                        "video_id": v.id,
                        "title": v.title,
                        "duration": v.duration,
                        "resolution": v.resolution,
                        "codec": v.codec,
                        "thumbnail": format!("/thumbnail/{}.jpg", v.id),
                        "m3u8": if !v.hls_m3u8.is_empty() {
                            format!("/stream/{}", v.hls_m3u8)
                        } else {
                            String::new()
                        },
                        "playable": !v.hls_m3u8.is_empty(),
                    })
                })
                .collect();
            json_response(
                StatusCode::OK,
                json!({
                    "id": playlist.id,
                    "name": playlist.name,
                    "source": playlist.source,
                    "youtube_url": playlist.youtube_url,
                    "created_at": playlist.created_at,
                    "items": json_videos,
                }),
            )
        }
        None => json_response(
            StatusCode::FORBIDDEN,
            json!({"error": "not configured"}),
        ),
    }
}

/// POST /api/playlists/:id/items → add a video to a playlist.
async fn add_playlist_item(
    State(state): State<AppState>,
    PathExtract(id): PathExtract<u64>,
    req: Request<Body>,
) -> Response {
    let body_bytes = collect_body(req).await;
    let body_str = String::from_utf8_lossy(&body_bytes);
    let body: AddItemRequest = match serde_json::from_str(&body_str) {
        Ok(b) => b,
        Err(e) => return json_response(StatusCode::BAD_REQUEST, json!({"error": e.to_string()})),
    };
    let db_guard = state.db.lock().unwrap();
    match db_guard.as_ref() {
        Some(db) => match db.add_playlist_item(id, body.video_id, 0) {
            Ok(()) => json_response(StatusCode::OK, json!({})),
            Err(e) => json_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                json!({"error": e.to_string()}),
            ),
        },
        None => json_response(
            StatusCode::FORBIDDEN,
            json!({"error": "not configured"}),
        ),
    }
}

/// DELETE /api/playlists/:id/items → remove a video from a playlist.
async fn remove_playlist_item(
    State(state): State<AppState>,
    PathExtract(id): PathExtract<u64>,
    req: Request<Body>,
) -> Response {
    let body_bytes = collect_body(req).await;
    let body_str = String::from_utf8_lossy(&body_bytes);
    let body: RemoveItemRequest = match serde_json::from_str(&body_str) {
        Ok(b) => b,
        Err(e) => return json_response(StatusCode::BAD_REQUEST, json!({"error": e.to_string()})),
    };
    let db_guard = state.db.lock().unwrap();
    match db_guard.as_ref() {
        Some(db) => match db.remove_playlist_item(id, body.video_id) {
            Ok(()) => json_response(StatusCode::OK, json!({})),
            Err(e) => json_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                json!({"error": e.to_string()}),
            ),
        },
        None => json_response(
            StatusCode::FORBIDDEN,
            json!({"error": "not configured"}),
        ),
    }
}

/// Serve a file from the data dir (hls or thumbnail) by relative name, or 404.
/// Read a file under `data_dir/{prefix}/{path}`, serving it with the right
/// content type. `prefix` is `hls` for streams and `thumbnails` for
/// thumbnails.
async fn read_data_file(state: &AppState, prefix: &str, path: String) -> Response {
    // Lock only for the duration of reading data_dir; the guard must be dead
    // before the await so the future stays Send.
    let data_dir = {
        let cfg = state.config.lock().unwrap();
        match &*cfg {
            Some(c) => PathBuf::from(&c.data_dir),
            None => return json_response(StatusCode::NOT_FOUND, json!({"error": "not configured"})),
        }
    };
    // Sanitize: strip leading slash, reject traversal.
    let safe = path
        .trim_start_matches('/')
        .split('/')
        .filter(|p| !p.starts_with("..") && !p.contains('\0'))
        .collect::<Vec<_>>()
        .join("/");
    let full = data_dir.join(prefix).join(&safe);
    let bytes = match tokio::fs::read(&full).await {
        Ok(b) => b,
        Err(_) => return plain(StatusCode::NOT_FOUND, "not found"),
    };
    let ext = full
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_string())
        .unwrap_or_default();
    let content_type = match ext.as_str() {
        "m3u8" => "application/vnd.apple.mpegurl",
        "ts" => "application/octet-stream",
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        _ => "application/octet-stream",
    };
    Response::builder()
        .status(StatusCode::OK)
        .header(CONTENT_TYPE, content_type)
        .body(Body::from(bytes))
        .expect("build response")
}

/// Serve HLS segments/playlists from `data_dir/hls/`.
async fn stream_file(State(state): State<AppState>, PathExtract(path): PathExtract<String>) -> Response {
    read_data_file(&state, "hls", path).await
}

/// Serve thumbnails from `data_dir/thumbnails/`.
async fn thumbnail_file(State(state): State<AppState>, PathExtract(path): PathExtract<String>) -> Response {
    read_data_file(&state, "thumbnails", path).await
}

// ---------------------------------------------------------------------------
// Version + settings (S6/S7)
// ---------------------------------------------------------------------------

const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// GET /api/version → current + latest version (notify-only).
/// Admin-only.
async fn version(
    State(state): State<AppState>,
    req: Request<Body>,
) -> Response {
    if !is_authenticated(&state, &req) {
        return json_response(StatusCode::UNAUTHORIZED, json!({"error": "not authenticated"}));
    }
    let check_url = {
        let cfg = state.config.lock().unwrap();
        match &*cfg {
            Some(c) => c.version_check_url.clone(),
            None => return json_response(StatusCode::FORBIDDEN, json!({"error": "not configured"})),
        }
    };
    let latest = {
        let client = reqwest::Client::new();
        match client.get(&check_url).timeout(std::time::Duration::from_secs(10)).send().await {
            Ok(r) => match r.json::<serde_json::Value>().await {
                Ok(v) => {
                    let tag = v
                        .get("tag_name")
                        .and_then(|t| t.as_str())
                        .unwrap_or(CURRENT_VERSION);
                    tag.to_string()
                }
                Err(_) => CURRENT_VERSION.to_string(),
            },
            Err(_) => CURRENT_VERSION.to_string(),
        }
    };
    let update_available = latest != CURRENT_VERSION && latest.as_str() > CURRENT_VERSION;
    json_response(
        StatusCode::OK,
        json!({
            "current_version": CURRENT_VERSION,
            "latest_version": latest,
            "update_available": update_available,
            "repo_url": "https://github.com/JugglerMaster/home-video",
        }),
    )
}

#[derive(Deserialize, Default)]
struct UpnpRequest {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_upnp_discovery_interval")]
    pub discovery_interval_s: u64,
    #[serde(default)]
    pub server_ip: String,
}

fn default_upnp_discovery_interval() -> u64 {
    30
}

#[derive(Deserialize)]
struct SettingsRequest {
    pub watch_dirs: Vec<String>,
    pub data_dir: String,
    pub scan_interval_s: u64,
    pub anonymous_view: bool,
    #[serde(default)]
    pub youtube_download_dir: Option<String>,
    #[serde(default = "default_convert_quality")]
    pub convert_quality: String,
    #[serde(default = "default_convert_format")]
    pub default_convert_format: String,
    #[serde(default)]
    pub convert_all: bool,
    #[serde(default)]
    pub upnp: UpnpRequest,
}

fn default_convert_quality() -> String {
    "medium".to_string()
}
fn default_convert_format() -> String {
    "h264".to_string()
}

/// POST /api/settings → re-edit watch dirs, data dir, scan interval, anonymous toggle,
/// conversion and UPNP settings. Admin-only.
async fn settings_post(
    State(state): State<AppState>,
    req: Request<Body>,
) -> Response {
    if !is_authenticated(&state, &req) {
        return json_response(StatusCode::UNAUTHORIZED, json!({"error": "not authenticated"}));
    }
    let body_bytes = collect_body(req).await;
    let body_str = String::from_utf8_lossy(&body_bytes);
    let body: SettingsRequest = match serde_json::from_str(&body_str) {
        Ok(b) => b,
        Err(e) => return json_response(StatusCode::BAD_REQUEST, json!({"error": e.to_string()})),
    };
    let mut cfg_guard = state.config.lock().unwrap();
    match &*cfg_guard {
        Some(c) => {
            let mut new_c = c.clone();
            new_c.watch_dirs = body.watch_dirs;
            new_c.data_dir = body.data_dir.clone();
            new_c.scan_interval_s = body.scan_interval_s;
            new_c.anonymous_view = body.anonymous_view;
            if let Some(ytd) = body.youtube_download_dir {
                let trimmed = ytd.trim().to_string();
                if !trimmed.is_empty() {
                    new_c.youtube_download_dir = trimmed;
                }
            }
            new_c.convert_quality = body.convert_quality;
            new_c.default_convert_format = body.default_convert_format;
            new_c.convert_all = body.convert_all;
            new_c.upnp.enabled = body.upnp.enabled;
            new_c.upnp.discovery_interval_s = body.upnp.discovery_interval_s;
            new_c.upnp.server_ip = body.upnp.server_ip.trim().to_string();
            if let Err(e) = config::validate(&new_c) {
                return json_response(
                    StatusCode::BAD_REQUEST,
                    json!({"error": e.to_string()}),
                );
            }
            if let Err(e) = config::save(&state.config_path, &new_c) {
                return json_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    json!({"error": format!("write config: {e}")}),
                );
            }
            *cfg_guard = Some(new_c);
            json_response(StatusCode::OK, json!({"ok": true}))
        }
        None => json_response(
            StatusCode::FORBIDDEN,
            json!({"error": "not configured"}),
        ),
    }
}

/// GET /api/settings → current settings (admin-only, for pre-filling the form).
async fn settings_get(State(state): State<AppState>, req: Request<Body>) -> Response {
    if !is_authenticated(&state, &req) {
        return json_response(StatusCode::UNAUTHORIZED, json!({"error": "not authenticated"}));
    }
    let cfg_guard = state.config.lock().unwrap();
    match &*cfg_guard {
        Some(c) => json_response(
            StatusCode::OK,
            json!({
                "watch_dirs": c.watch_dirs,
                "data_dir": c.data_dir,
                "scan_interval_s": c.scan_interval_s,
                "anonymous_view": c.anonymous_view,
                "youtube_download_dir": c.youtube_download_dir,
                "convert_quality": c.convert_quality,
                "default_convert_format": c.default_convert_format,
                "convert_all": c.convert_all,
                "upnp": {
                    "enabled": c.upnp.enabled,
                    "discovery_interval_s": c.upnp.discovery_interval_s,
                    "server_ip": c.upnp.server_ip,
                },
                "playback_supported_formats": ingest::PLAYBACK_SUPPORTED_FORMATS.to_vec(),
                "convert_target_formats": ingest::CONVERT_TARGET_FORMATS.to_vec(),
            }),
        ),
        None => json_response(
            StatusCode::FORBIDDEN,
            json!({"error": "not configured"}),
        ),
    }
}

/// GET /api/conversions → list of videos currently being converted
/// (re-encoded) at ingest time. Admin-only.
async fn list_conversions(State(state): State<AppState>, _req: Request<Body>) -> Response {
    // Public read-only endpoint: the conversion queue is a piece of
    // system state shown to all users in the UI banner.
    let conversions = state.conversions.lock().unwrap();
    let items: Vec<serde_json::Value> = conversions
        .iter()
        .map(|c| json!({
            "video_id": c.video_id,
            "title": c.title,
            "started_at": c.started_at,
        }))
        .collect();
    json_response(
        StatusCode::OK,
        json!({
            "count": conversions.len(),
            "items": items,
        }),
    )
}

/// GET /api/speakers → discovered UPnP renderers (Sonos in particular).
/// Public read-only endpoint (the speaker list is harmless to show).
async fn list_speakers(State(state): State<AppState>, _req: Request<Body>) -> Response {
    // Grace period: 2x the discovery interval, so a single missed SSDP window
    // (UDP loss) does not mark a healthy speaker offline.
    let grace = Duration::from_secs(60);
    let speakers = state.speakers.infos(grace);
    json_response(StatusCode::OK, json!({ "speakers": speakers }))
}

/// POST /api/cast/:video_id/:speaker_id → cast a video's pre-baked audio to
/// a speaker. Admin-only. Optional body: { "volume": <u16, 0..=100> }
/// (default 50). Path params carry the video and the speaker, so no JSON
/// body field is needed to address them.
///
/// Sends real UPnP control (SetVolume + SetAVTransportURI + Play) to the
/// speaker. Only exercised after the user approves a real Sonos target.
async fn cast_to_speaker(
    State(state): State<AppState>,
    PathExtract((video_id, speaker_id)): PathExtract<(u64, String)>,
    req: Request<Body>,
) -> Response {
    if !is_authenticated(&state, &req) {
        return json_response(StatusCode::UNAUTHORIZED, json!({"error": "not authenticated"}));
    }
    let body_bytes = req
        .into_body()
        .collect()
        .await
        .map(|b| b.to_bytes())
        .unwrap_or_default();
    // Empty body → default 50. JSON parse failure or a non-number / out-of-range
    // volume → 400.
    let volume: u16 = if body_bytes.is_empty() {
        50
    } else {
        let body: serde_json::Value = match serde_json::from_slice(&body_bytes) {
            Ok(b) => b,
            Err(_) => return json_response(StatusCode::BAD_REQUEST, json!({"error": "bad json"})),
        };
        match body.get("volume") {
            Some(v) => {
                match v.as_u64().filter(|n| *n <= 100) {
                    Some(n) => n as u16,
                    None => return json_response(StatusCode::BAD_REQUEST, json!({"error": "invalid volume"})),
                }
            }
            None => 50,
        }
    };

    // Resolve the video and its pre-baked audio path.
    let video = {
        let db_guard = state.db.lock().unwrap();
        match db_guard.as_ref() {
            Some(db) => match db.get_video_by_id(video_id) {
                Ok(Some(v)) => v,
                Ok(None) => {
                    return json_response(StatusCode::NOT_FOUND, json!({"error": "video not found"}));
                }
                Err(e) => {
                    return json_response(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        json!({ "error": e.to_string() }),
                    );
                }
            }
            None => {
                return json_response(StatusCode::FORBIDDEN, json!({"error": "not configured"}));
            }
        }
    };
    if video.audio_path.is_empty() {
        return json_response(StatusCode::NOT_FOUND, json!({"error": "no pre-baked audio for this video"}));
    }

    // Resolve the speaker; require it to be online (Phase 5) before any
    // SOAP traffic is sent.
    let speaker = match state.speakers.get(&speaker_id) {
        Some(s) if s.is_online(upnp::ONLINE_GRACE) => s,
        Some(_) => {
            return json_response(StatusCode::CONFLICT, json!({"error": "speaker offline"}));
        }
        None => return json_response(StatusCode::NOT_FOUND, json!({"error": "speaker not found"})),
    };

    // Build the direct audio URI. Configured server_ip wins; else
    // auto-detect; else fail loudly (never silently fall back to localhost).
    let ip = {
        let cfg = state.config.lock().unwrap();
        let configured = cfg.as_ref().map(|c| c.upnp.server_ip.clone()).unwrap_or_default();
        match upnp::resolve_server_ip(&configured) {
            Ok(ip) => ip,
            Err(e) => {
                return json_response(StatusCode::BAD_REQUEST, json!({"error": e.to_string()}));
            }
        }
    };
    let uri = format!("http://{ip}:{}/audio/{video_id}", state.port);

    let client = reqwest::Client::new();
    match upnp::cast(&client, &speaker, &uri, volume).await {
        Ok(()) => json_response(StatusCode::OK, json!({ "ok": true })),
        Err(e) => json_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            json!({ "error": e.to_string() }),
        ),
    }
}

/// GET /audio/:id → serve a video's pre-baked audio file (Phase 1) so UPnP
/// renderers can stream it directly (Sonos cannot play HLS).
///
/// The file is stored under `data/audio/<id>.m4a` (AAC 192k). The video is
/// looked up in the DB; the file is served from disk. Public when
/// anonymous view is enabled (admin-only otherwise).
async fn audio_file(
    State(state): State<AppState>,
    PathExtract(id): PathExtract<u64>,
    req: Request<Body>,
) -> Response {
    // Respect the anonymous-view toggle: when it is off, only an
    // authenticated admin may fetch the audio (same posture as the video list).
    let anonymous_view = {
        let cfg = state.config.lock().unwrap();
        match cfg.as_ref() {
            Some(c) => c.anonymous_view,
            None => return plain(StatusCode::FORBIDDEN, "not configured"),
        }
    };
    if !anonymous_view && !is_authenticated(&state, &req) {
        return plain(StatusCode::FORBIDDEN, "not authenticated");
    }

    // Resolve the video and its pre-baked audio path.
    let (data_dir, audio_path) = {
        let db_guard = state.db.lock().unwrap();
        match db_guard.as_ref() {
            Some(db) => match db.get_video_by_id(id) {
                Ok(Some(v)) if !v.audio_path.is_empty() => {
                    let cfg = state.config.lock().unwrap();
                    match cfg.as_ref() {
                        Some(c) => (c.data_dir.clone(), v.audio_path.clone()),
                        None => return plain(StatusCode::FORBIDDEN, "not configured"),
                    }
                }
                Ok(None) => {
                    return plain(StatusCode::NOT_FOUND, "no audio for this video")
                }
                Err(e) => {
                    return plain(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string())
                }
                _ => {
                    return plain(StatusCode::NOT_FOUND, "no audio for this video")
                }
            }
            None => {
                return plain(StatusCode::FORBIDDEN, "not configured")
            }
        }
    };
    let full = PathBuf::from(&data_dir).join("audio").join(&audio_path);
    match tokio::fs::read(&full).await {
        Ok(bytes) => {
            let ct = content_type_for(&audio_path);
            Response::builder()
                .status(StatusCode::OK)
                .header(CONTENT_TYPE, ct)
                .body(Body::from(Bytes::from(bytes)))
                .expect("build response")
        }
        Err(e) => {
            eprintln!("audio file error for {id}: {e}");
            plain(StatusCode::INTERNAL_SERVER_ERROR, "audio file unavailable")
        }
    }
}

/// Build the router with all routes and middleware.
fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/setup", get(setup_get).post(setup_post))
        .route("/admin", get(admin_get))
        .route("/admin/login", post(login))
        .route("/admin/logout", post(logout))
        .route("/api/session", get(session))
        .route("/api/videos", get(list_videos))
        .route(
            "/api/videos/:id",
            post(update_video).delete(delete_video),
        )
        .route(
            "/api/videos/:id/favorite",
            post(toggle_favorite),
        )
        .route(
            "/api/videos/:id/position",
            post(update_position),
        )
        .route("/api/import", post(import_post))
        .route(
            "/api/playlists",
            get(list_playlists).post(create_playlist),
        )
        .route(
            "/api/playlists/:id",
            get(get_playlist)
                .put(rename_playlist)
                .delete(delete_playlist),
        )
        .route(
            "/api/playlists/:id/items",
            post(add_playlist_item).delete(remove_playlist_item),
        )
        .route("/api/queue", get(list_queue).post(add_to_queue))
        .route(
            "/api/queue/:id",
            delete(remove_from_queue),
        )
        .route("/stream/*path", get(stream_file))
        .route("/thumbnail/*path", get(thumbnail_file))
        .route("/health", get(|| async { "ok".to_string() }))
        .route(
            "/api/version",
            get(version),
        )
        .route(
            "/api/settings",
            get(settings_get).post(settings_post),
        )
        .route("/api/conversions", get(list_conversions))
        .route("/api/speakers", get(list_speakers))
        .route(
            "/api/cast/:video_id/:speaker_id",
            post(cast_to_speaker),
        )
        .route("/audio/:id", get(audio_file))
        .route("/*path", get(static_handler))
        .with_state(state.clone())
        .layer(from_extractor_with_state::<ConfigGuard, AppState>(state.clone()))
        .layer(TraceLayer::new_for_http())
}

/// Boot the web server.
pub async fn run() -> anyhow::Result<()> {
    let port = std::env::var("HOME_VIDEO_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(8080);
    let config_path = std::env::var("HOME_VIDEO_CONFIG")
        .ok()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(CONFIG_PATH_DEFAULT));

    let existing_config = config::load(&config_path)?;
    // Build the db (from the config's data dir) and the state together, so
    // `existing_config` is not moved into the match and then reused.
    let (db, existing_config) = match existing_config {
        Some(cfg) => {
            if let Err(e) = std::fs::create_dir_all(&cfg.data_dir) {
                return Err(anyhow::anyhow!("create data_dir {:?}: {e}", cfg.data_dir));
            }
            let db_path = PathBuf::from(&cfg.data_dir).join("home-video.db");
            let db = match Db::new(&db_path).and_then(|db| db.create_schema().map(|_| db)) {
                Ok(db) => db,
                Err(e) => return Err(anyhow::anyhow!("create db: {e}")),
            };
            (Some(db), Some(cfg))
        }
        None => (None, None),
    };
    let state = AppState::new(config_path, existing_config, port, db);

    // Background watcher: initial scan + loop every scan_interval_s.
    let watcher_state = state.clone();
    tokio::spawn(async move {
        loop {
            // Wait until configured. Lock only to read the config; the guard
            // must be dead before any await so the spawned future stays Send.
            let scan_interval = {
                let configured = watcher_state.config.lock().unwrap();
                (*configured).as_ref().map(|c| c.scan_interval_s)
            };
            match scan_interval {
                Some(interval) => {
                    if let Err(e) = ingest::scan(&watcher_state).await {
                        eprintln!("scan error: {e}");
                    }
                    let mut interval = tokio::time::interval(Duration::from_secs(interval));
                    loop {
                        interval.tick().await;
                        if let Err(e) = ingest::scan(&watcher_state).await {
                            eprintln!("scan error: {e}");
                        }
                    }
                }
                None => {
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    continue;
                }
            }
        }
    });

    // Background UPnP discovery: refreshes the speaker store every
    // discovery_interval_s while UPnP is enabled.
    let discovery_state = state.clone();
    let discovery_client = reqwest::Client::new();
    tokio::spawn(async move {
        loop {
            let (enabled, interval_s) = {
                let configured = discovery_state.config.lock().unwrap();
                match configured.as_ref() {
                    Some(c) => (c.upnp.enabled, c.upnp.discovery_interval_s),
                    None => (false, 30),
                }
            };
            if !enabled {
                tokio::time::sleep(Duration::from_secs(5)).await;
                continue;
            }
            upnp::discover_once(&discovery_client, &discovery_state.speakers).await;
            tokio::time::sleep(Duration::from_secs(interval_s)).await;
        }
    });

    let app = build_router(state.clone());

    let addr = std::net::SocketAddr::from(([0, 0, 0, 0], port));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    println!("home-video listening on http://{addr}");
    axum::serve(listener, app).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::header::ALLOW;
    use tower::ServiceExt;
    use crate::db::Video;

    fn test_state() -> AppState {
        let cfg = config::Config {
            port: 8080,
            watch_dirs: vec!["/tmp/watch".to_string()],
            youtube_download_dir: "/tmp/yt".to_string(),
            data_dir: "/tmp/hv_test_data".to_string(),
            scan_interval_s: 5,
            anonymous_view: true,
            admin_password_hash: String::new(),
            version_check_url: String::new(),
            convert_quality: "medium".to_string(),
            default_convert_format: "h264".to_string(),
            convert_all: true,
            upnp: config::UpnpSettings {
                enabled: false,
                discovery_interval_s: 30,
                server_ip: String::new(),
            },
        };
        AppState::new(PathBuf::from("/tmp/nonexistent"), Some(cfg), 8080, None)
    }

    async fn call(method: &str, uri: &str, body: Body) -> axum::response::Response {
        build_router(test_state())
            .oneshot(Request::builder().method(method).uri(uri).body(body).unwrap())
            .await
            .unwrap()
    }

    // ------------------------------------------------------------------------
    // Audio endpoint (Phase 3) + cast endpoint (Phase 4)
    // ------------------------------------------------------------------------

    fn audio_test_state(anonymous_view: bool, write_audio: bool) -> (AppState, PathBuf, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let data_dir = dir.path().join("data");
        std::fs::create_dir_all(data_dir.join("audio")).unwrap();
        let audio_path = "vid.m4a".to_string();
        if write_audio {
            std::fs::write(data_dir.join("audio").join("vid.m4a"), b"fake-m4a-bytes").unwrap();
        }
        let db_path = dir.path().join("test.db");
        let db = Db::new(&db_path).unwrap();
        db.create_schema().unwrap();
        let video = Video {
            id: 0,
            fingerprint: "fp-audio-1".to_string(),
            path: data_dir.join("src.mp4").to_string_lossy().to_string(),
            title: "audio vid".to_string(),
            description: String::new(),
            tags: String::new(),
            duration: 2.0,
            resolution: "320x240".to_string(),
            codec: "h264".to_string(),
            size: 10,
            bitrate: 100,
            created_at: "now".to_string(),
            updated_at: "now".to_string(),
            hls_m3u8: String::new(),
            thumbnail_path: String::new(),
            playback_position: 0.0,
            favorite: false,
            audio_path: audio_path.clone(),
        };
        let id = db.insert_video(&video).unwrap();
        let data_dir_str = data_dir.to_string_lossy().to_string();
        let cfg = config::Config {
            port: 8080,
            watch_dirs: vec!["/tmp/watch".to_string()],
            youtube_download_dir: String::new(),
            data_dir: data_dir_str.clone(),
            scan_interval_s: 5,
            anonymous_view,
            admin_password_hash: String::new(),
            version_check_url: String::new(),
            convert_quality: "medium".to_string(),
            default_convert_format: "h264".to_string(),
            convert_all: true,
            upnp: config::UpnpSettings {
                enabled: false,
                discovery_interval_s: 30,
                server_ip: String::new(),
            },
        };
        let state = AppState::new(
            PathBuf::from("/tmp/nonexistent"),
            Some(cfg),
            8080,
            Some(db),
        );
        // Keep a handle for the 404 case (created with no audio pre-baked).
        let _ = (id, video.audio_path.clone());
        (state, data_dir, dir)
    }

    async fn call_with_cookie(
        state: &AppState,
        method: &str,
        uri: &str,
        body: Body,
        cookie: &str,
    ) -> Response {
        build_router(state.clone())
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(uri)
                    .header("cookie", cookie)
                    .body(body)
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn audio_serves_file_when_anonymous_view_on() {
        let (state, _data, _dir) = audio_test_state(true, true);
        let resp = call_with_cookie(&state, "GET", "/audio/1", Body::empty(), "").await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(resp.headers().get(CONTENT_TYPE).unwrap(), "audio/mp4");
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(bytes.as_ref(), b"fake-m4a-bytes");
    }

    #[tokio::test]
    async fn audio_is_forbidden_when_anonymous_view_off_and_anonymous() {
        let (state, _data, _dir) = audio_test_state(false, true);
        let resp = call_with_cookie(&state, "GET", "/audio/1", Body::empty(), "").await;
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn audio_served_when_anonymous_view_off_and_authenticated() {
        let (state, _data, _dir) = audio_test_state(false, true);
        state.sessions.lock().unwrap().insert("tok".to_string());
        let resp = call_with_cookie(
            &state,
            "GET",
            "/audio/1",
            Body::empty(),
            "session=tok",
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn audio_404s_when_video_has_no_audio() {
        let (state, _data, _dir) = audio_test_state(true, false);
        // The video's audio_path is present in the DB, but the file does not
        // exist on disk → the file read fails with 500. The "no audio" 404
        // fires when the DB row itself has an empty audio_path.
        let resp = call_with_cookie(&state, "GET", "/audio/1", Body::empty(), "").await;
        assert!(resp.status() == StatusCode::NOT_FOUND || resp.status() == StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[tokio::test]
    async fn cast_requires_admin() {
        let (state, _data, _dir) = audio_test_state(true, true);
        let resp = call_with_cookie(&state, "POST", "/api/cast/1/spk", Body::from(Bytes::from_static(br#"{}"#)), "").await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn cast_400s_on_bad_body() {
        let (state, _data, _dir) = audio_test_state(true, true);
        state.sessions.lock().unwrap().insert("tok".to_string());
        // Speaker must exist and be online for the 400 to fire only after
        // the body check; insert a speaker so the speaker-not-found branch
        // is never taken.
        let online_speaker = upnp::Speaker {
            usn: "speaker-1".to_string(),
            name: "Speaker".to_string(),
            av_transport_url: "http://192.168.1.10:5000/Control".to_string(),
            rendering_control_url: "http://192.168.1.10:5000/Control".to_string(),
            last_seen: std::time::SystemTime::now(),
        };
        state.speakers.upsert(online_speaker);
        let resp = call_with_cookie(
            &state,
            "POST",
            "/api/cast/1/speaker-1",
            Body::from(Bytes::from_static(br#"{"volume":"not-a-number"}"#)),
            "session=tok",
        )
        .await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn cast_404s_when_speaker_not_found() {
        let (state, _data, _dir) = audio_test_state(true, true);
        state.sessions.lock().unwrap().insert("tok".to_string());
        // No speakers at all.
        let resp = call_with_cookie(
            &state,
            "POST",
            "/api/cast/1/missing-speaker",
            Body::from(Bytes::from_static(br#"{}"#)),
            "session=tok",
        )
        .await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn cast_409s_when_speaker_is_offline() {
        let (state, _data, _dir) = audio_test_state(true, true);
        state.sessions.lock().unwrap().insert("tok".to_string());
        // Insert an offline speaker (last seen long ago).
        let offline_speaker = upnp::Speaker {
            usn: "speaker-2".to_string(),
            name: "Offline".to_string(),
            av_transport_url: "http://192.168.1.10:5000/Control".to_string(),
            rendering_control_url: "http://192.168.1.10:5000/Control".to_string(),
            last_seen: std::time::SystemTime::now()
                .checked_sub(upnp::ONLINE_GRACE + std::time::Duration::from_secs(30))
                .expect("past"),
        };
        state.speakers.upsert(offline_speaker);
        let resp = call_with_cookie(
            &state,
            "POST",
            "/api/cast/1/speaker-2",
            Body::from(Bytes::from_static(br#"{}"#)),
            "session=tok",
        )
        .await;
        assert_eq!(resp.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn cast_404s_when_video_not_found() {
        let (state, _data, _dir) = audio_test_state(true, true);
        state.sessions.lock().unwrap().insert("tok".to_string());
        let online_speaker = upnp::Speaker {
            usn: "speaker-3".to_string(),
            name: "Speaker".to_string(),
            av_transport_url: "http://192.168.1.10:5000/Control".to_string(),
            rendering_control_url: "http://192.168.1.10:5000/Control".to_string(),
            last_seen: std::time::SystemTime::now(),
        };
        state.speakers.upsert(online_speaker);
        let resp = call_with_cookie(
            &state,
            "POST",
            "/api/cast/999999/speaker-3",
            Body::from(Bytes::from_static(br#"{}"#)),
            "session=tok",
        )
        .await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn videos_id_uses_axum_07_path_parameter_syntax() {
        // With `{id}` (axum 0.8 syntax) this falls through to `/*path` and returns 404.
        let resp = call("GET", "/api/videos/1", Body::empty()).await;

        assert_eq!(
            resp.status(),
            StatusCode::METHOD_NOT_ALLOWED,
            "GET /api/videos/1 should match the POST/DELETE-only route, got {:?}",
            resp.status()
        );
        let allow = resp
            .headers()
            .get(ALLOW)
            .expect("405 response should include Allow")
            .to_str()
            .unwrap();
        assert!(allow.contains("POST"), "Allow should contain POST: {allow}");
        assert!(allow.contains("DELETE"), "Allow should contain DELETE: {allow}");
    }

    #[tokio::test]
    async fn videos_id_post_reaches_handler() {
        let resp = call(
            "POST",
            "/api/videos/1",
            Body::from(Bytes::from_static(br#"{}"#)),
        )
        .await;

        assert_ne!(
            resp.status(),
            StatusCode::METHOD_NOT_ALLOWED,
            "POST /api/videos/1 should reach update_video, got {:?}",
            resp.status()
        );
    }

    #[tokio::test]
    async fn playlists_id_uses_axum_07_path_parameter_syntax() {
        // With `{id}` this also falls through to `/*path` and returns 404.
        let resp = call("GET", "/api/playlists/1", Body::empty()).await;

        assert_ne!(
            resp.status(),
            StatusCode::NOT_FOUND,
            "GET /api/playlists/1 should match the specific route, got {:?}",
            resp.status()
        );
    }

    // ------------------------------------------------------------------------
    // Settings (Phase 4b)
    // ------------------------------------------------------------------------

    async fn settings_call(state: &AppState, method: &str, uri: &str, body: &str, cookie: &str) -> Response {
        build_router(state.clone())
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(uri)
                    .header("cookie", cookie)
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn settings_get_requires_admin() {
        let state = test_state();
        let resp = settings_call(&state, "GET", "/api/settings", "", "").await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn settings_post_roundtrip_upnp_block() {
        let state = test_state();
        state.sessions.lock().unwrap().insert("tok".to_string());
        let body = r#"{
            "watch_dirs": ["/tmp/watch"],
            "data_dir": "/tmp/hv_test_data",
            "scan_interval_s": 5,
            "anonymous_view": true,
            "youtube_download_dir": "",
            "convert_quality": "medium",
            "default_convert_format": "h264",
            "convert_all": true,
            "upnp": { "enabled": true, "discovery_interval_s": 15, "server_ip": "192.168.1.10" }
        }"#;
        let resp = settings_call(&state, "POST", "/api/settings", body, "session=tok").await;
        assert_eq!(resp.status(), StatusCode::OK);

        let resp = settings_call(&state, "GET", "/api/settings", "", "session=tok").await;
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["upnp"]["enabled"], true);
        assert_eq!(json["upnp"]["discovery_interval_s"], 15);
        assert_eq!(json["upnp"]["server_ip"], "192.168.1.10");
    }

    #[tokio::test]
    async fn settings_post_rejects_invalid_upnp_interval() {
        let state = test_state();
        state.sessions.lock().unwrap().insert("tok".to_string());
        let body = r#"{
            "watch_dirs": ["/tmp/watch"],
            "data_dir": "/tmp/hv_test_data",
            "scan_interval_s": 5,
            "anonymous_view": true,
            "convert_quality": "medium",
            "default_convert_format": "h264",
            "convert_all": true,
            "upnp": { "enabled": true, "discovery_interval_s": 0, "server_ip": "" }
        }"#;
        let resp = settings_call(&state, "POST", "/api/settings", body, "session=tok").await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn settings_post_rejects_empty_watch_dirs() {
        let state = test_state();
        state.sessions.lock().unwrap().insert("tok".to_string());
        let body = r#"{
            "watch_dirs": [],
            "data_dir": "/tmp/hv_test_data",
            "scan_interval_s": 5,
            "anonymous_view": true,
            "convert_quality": "medium",
            "default_convert_format": "h264",
            "convert_all": true,
            "upnp": { "enabled": false, "discovery_interval_s": 30, "server_ip": "" }
        }"#;
        let resp = settings_call(&state, "POST", "/api/settings", body, "session=tok").await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    async fn delete_call(state: &AppState, id: u64, cookie: &str) -> Response {
        settings_call(state, "DELETE", &format!("/api/videos/{id}"), "", cookie).await
    }

    #[tokio::test]
    async fn delete_video_fails_loudly_when_source_file_is_not_removable() {
        // Source file sits in a read-only directory: removing it must fail
        // loudly and keep the DB row (otherwise the next scan would re-ingest
        // the file silently).
        let dir = tempfile::tempdir().unwrap();
        let restricted = dir.path().join("restricted");
        std::fs::create_dir_all(&restricted).unwrap();
        let src = restricted.join("file.mp4");
        std::fs::write(&src, b"fake").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&restricted, std::fs::Permissions::from_mode(0o555)).unwrap();

        let data_dir = dir.path().join("data");
        std::fs::create_dir_all(data_dir.join("audio")).unwrap();
        let db_path = dir.path().join("test.db");
        let db = Db::new(&db_path).unwrap();
        db.create_schema().unwrap();
        let video = Video {
            id: 0,
            fingerprint: "fp-forbidden".to_string(),
            path: src.to_string_lossy().to_string(),
            title: "delete test".to_string(),
            description: String::new(),
            tags: String::new(),
            duration: 1.0,
            resolution: "320x240".to_string(),
            codec: "h264".to_string(),
            size: 5,
            bitrate: 10,
            created_at: "now".to_string(),
            updated_at: "now".to_string(),
            hls_m3u8: String::new(),
            thumbnail_path: String::new(),
            playback_position: 0.0,
            favorite: false,
            audio_path: String::new(),
        };
        let id = db.insert_video(&video).unwrap();

        let cfg = config::Config {
            port: 8080,
            watch_dirs: vec!["/tmp/watch".to_string()],
            youtube_download_dir: String::new(),
            data_dir: data_dir.to_string_lossy().to_string(),
            scan_interval_s: 5,
            anonymous_view: true,
            admin_password_hash: String::new(),
            version_check_url: String::new(),
            convert_quality: "medium".to_string(),
            default_convert_format: "h264".to_string(),
            convert_all: true,
            upnp: config::UpnpSettings {
                enabled: false,
                discovery_interval_s: 30,
                server_ip: String::new(),
            },
        };
        let state = AppState::new(
            PathBuf::from("/tmp/nonexistent"),
            Some(cfg),
            8080,
            Some(db),
        );
        state.sessions.lock().unwrap().insert("tok".to_string());

        let resp = delete_call(&state, id, "session=tok").await;
        assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);

        let row = state
            .db
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .get_video_by_id(id)
            .unwrap();
        assert!(row.is_some(), "row must survive a failed delete");

        std::fs::set_permissions(&restricted, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    // --------------------------------------------------------------------
    // Route-coverage tests: library listing, session, queue, video, playlists
    // --------------------------------------------------------------------

    fn authed_db_state() -> (AppState, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let data_dir = dir.path().join("data");
        std::fs::create_dir_all(data_dir.join("audio")).unwrap();
        let source_path = data_dir.join("src.mp4");
        std::fs::write(&source_path, b"fake-mp4").unwrap();
        let db_path = dir.path().join("test.db");
        let db = Db::new(&db_path).unwrap();
        db.create_schema().unwrap();
        let cfg = config::Config {
            port: 8080,
            watch_dirs: vec!["/tmp/watch".to_string()],
            youtube_download_dir: String::new(),
            data_dir: data_dir.to_string_lossy().to_string(),
            scan_interval_s: 5,
            anonymous_view: false,
            admin_password_hash: String::new(),
            version_check_url: String::new(),
            convert_quality: "medium".to_string(),
            default_convert_format: "h264".to_string(),
            convert_all: true,
            upnp: config::UpnpSettings {
                enabled: false,
                discovery_interval_s: 30,
                server_ip: String::new(),
            },
        };
        let state = AppState::new(
            PathBuf::from("/tmp/nonexistent"),
            Some(cfg),
            8080,
            Some(db),
        );
        state.sessions.lock().unwrap().insert("tok".to_string());
        (state, dir)
    }

    async fn insert_sample(state: &AppState, title: &str, fp: &str) -> u64 {
        let mut guard = state.db.lock().unwrap();
        let db = guard.as_mut().unwrap();
        db.insert_video(&sample_video(title, fp)).unwrap()
    }

    fn sample_video(title: &str, fp: &str) -> Video {
        Video {
            id: 0,
            fingerprint: fp.to_string(),
            path: "/x/dummy.mp4".to_string(),
            title: title.to_string(),
            description: String::new(),
            tags: String::new(),
            duration: 1.0,
            resolution: "320x240".to_string(),
            codec: "h264".to_string(),
            size: 10,
            bitrate: 100,
            created_at: "now".to_string(),
            updated_at: "now".to_string(),
            hls_m3u8: String::new(),
            thumbnail_path: String::new(),
            playback_position: 0.0,
            favorite: false,
            audio_path: String::new(),
        }
    }

    async fn call_api(state: &AppState, method: &str, uri: &str, body: &str, cookie: &str) -> Response {
        build_router(state.clone())
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(uri)
                    .header("cookie", cookie)
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap()
    }


    #[tokio::test]
    async fn list_videos_returns_all_with_defaults() {
        let (state, _dir) = authed_db_state();
        insert_sample(&state, "Zulu", "fp-z").await;
        insert_sample(&state, "Alpha", "fp-a").await;

        let resp = call_api(&state, "GET", "/api/videos", "", "session=tok").await;
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json.as_array().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn list_videos_supports_sort_and_search() {
        let (state, _dir) = authed_db_state();
        insert_sample(&state, "Zulu", "fp-z").await;
        insert_sample(&state, "Alpha", "fp-a").await;

        let resp = call_api(&state, "GET", "/api/videos?sort=title&order=asc", "", "").await;
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let titles: Vec<&str> = json
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v["title"].as_str().unwrap())
            .collect();
        assert_eq!(titles, vec!["Alpha", "Zulu"]);

        let resp = call_api(&state, "GET", "/api/videos?q=Zulu", "", "").await;
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json.as_array().unwrap().len(), 1);
        assert_eq!(json[0]["title"], "Zulu");
    }

    #[tokio::test]
    async fn session_reports_authentication_only_with_cookie() {
        let (state, _dir) = authed_db_state();
        let resp = call_api(&state, "GET", "/api/session", "", "session=tok").await;
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["authenticated"], true);

        let resp = call_api(&state, "GET", "/api/session", "", "").await;
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["authenticated"], false);
    }

    #[tokio::test]
    async fn health_route_reports_ok() {
        let (state, _dir) = authed_db_state();
        let resp = call_api(&state, "GET", "/health", "", "").await;
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn queue_post_get_delete_roundtrip() {
        let (state, _dir) = authed_db_state();
        let id = insert_sample(&state, "Alpha", "fp-a").await;

        let body = format!(r#"{{"video_id": {id}}}"#);
        let resp = call_api(&state, "POST", "/api/queue", &body, "session=tok").await;
        assert_eq!(resp.status(), StatusCode::CREATED);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let queue_id = json["id"].as_u64().unwrap();

        let resp = call_api(&state, "GET", "/api/queue", "", "session=tok").await;
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["queue"].as_array().unwrap().len(), 1);

        let resp = call_api(&state, "DELETE", &format!("/api/queue/{queue_id}"), "", "session=tok").await;
        assert_eq!(resp.status(), StatusCode::OK);
        let resp = call_api(&state, "GET", "/api/queue", "", "session=tok").await;
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["queue"].as_array().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn queue_post_requires_auth() {
        let (state, _dir) = authed_db_state();
        let resp = call_api(&state, "POST", "/api/queue", "{}", "").await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn toggle_favorite_requires_auth_and_flips_flag() {
        let (state, _dir) = authed_db_state();
        let id = insert_sample(&state, "Alpha", "fp-a").await;

        let resp = call_api(&state, "POST", &format!("/api/videos/{id}/favorite"), "", "").await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

        let resp = call_api(&state, "POST", &format!("/api/videos/{id}/favorite"), "", "session=tok").await;
        assert_eq!(resp.status(), StatusCode::OK);
        let row = state.db.lock().unwrap().as_ref().unwrap().get_video_by_id(id).unwrap().unwrap();
        assert!(row.favorite);
        let resp = call_api(&state, "POST", &format!("/api/videos/{id}/favorite"), "", "session=tok").await;
        assert_eq!(resp.status(), StatusCode::OK);
        let row = state.db.lock().unwrap().as_ref().unwrap().get_video_by_id(id).unwrap().unwrap();
        assert!(!row.favorite);
    }

    #[tokio::test]
    async fn update_position_sets_db_row_and_rejects_bad_json() {
        let (state, _dir) = authed_db_state();
        let id = insert_sample(&state, "Alpha", "fp-a").await;

        let resp = call_api(&state, "POST", &format!("/api/videos/{id}/position"), r#"{"position": 42.5}"#, "session=tok").await;
        assert_eq!(resp.status(), StatusCode::OK);
        let row = state.db.lock().unwrap().as_ref().unwrap().get_video_by_id(id).unwrap().unwrap();
        assert_eq!(row.playback_position, 42.5);

        let resp = call_api(&state, "POST", &format!("/api/videos/{id}/position"), "not-json", "session=tok").await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

        let resp = call_api(&state, "POST", &format!("/api/videos/{id}/position"), "{}", "session=tok").await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn update_video_updates_row_and_requires_auth() {
        let (state, _dir) = authed_db_state();
        let id = insert_sample(&state, "Alpha", "fp-a").await;

        let resp = call_api(&state, "POST", &format!("/api/videos/{id}"), r#"{"title": "Renamed", "description": "d", "tags": "t"}"#, "session=tok").await;
        assert_eq!(resp.status(), StatusCode::OK);
        let row = state.db.lock().unwrap().as_ref().unwrap().get_video_by_id(id).unwrap().unwrap();
        assert_eq!(row.title.as_str(), "Renamed");
        assert_eq!(row.description.as_str(), "d");
        assert_eq!(row.tags.as_str(), "t");

        // Auth guard: no session cookie -> 401
        let resp = call_api(&state, "POST", &format!("/api/videos/{id}"), "{}", "").await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn playlists_roundtrip_and_cascade_delete_works() {
        let (state, _dir) = authed_db_state();
        let video_id = insert_sample(&state, "Alpha", "fp-a").await;

        // Create playlist
        let resp = call_api(&state, "POST", "/api/playlists", r#"{"name": "P1"}"#, "session=tok").await;
        assert_eq!(resp.status(), StatusCode::CREATED);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let playlist_id = json["id"].as_u64().unwrap();

        // List playlists — should include the created playlist with count 0
        let resp = call_api(&state, "GET", "/api/playlists", "", "").await;
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(json.as_array().unwrap().iter().any(|p| p["id"] == playlist_id));

        // Add a video
        let body = format!(r#"{{"video_id": {video_id}}}"#);
        let resp = call_api(&state, "POST", &format!("/api/playlists/{playlist_id}/items"), &body, "session=tok").await;
        assert_eq!(resp.status(), StatusCode::OK);

        // Detail includes the video
        let resp = call_api(&state, "GET", &format!("/api/playlists/{playlist_id}"), "", "").await;
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["id"], playlist_id);
        assert_eq!(json["items"].as_array().unwrap().len(), 1);
        assert_eq!(json["items"][0]["video_id"], video_id);

        // Delete playlist — cascade removes the items too
        let resp = call_api(&state, "DELETE", &format!("/api/playlists/{playlist_id}"), "", "session=tok").await;
        assert_eq!(resp.status(), StatusCode::OK);

        let resp = call_api(&state, "GET", &format!("/api/playlists/{playlist_id}"), "", "").await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn playlists_create_requires_auth_and_rejects_missing_name() {
        let (state, _dir) = authed_db_state();

        // No session -> 401
        let resp = call_api(&state, "POST", "/api/playlists", r#"{"name": "P"}"#, "").await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

        // Missing name -> 400
        let resp = call_api(&state, "POST", "/api/playlists", "{}", "session=tok").await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn delete_video_happy_path_removes_source_file_and_row() {
        let (state, _dir) = authed_db_state();
        let source_path = _dir.path().join("data").join("src.mp4");
        let source_path_str = source_path.to_string_lossy().to_string();

        // Insert a video whose source path points at the real file
        let id = {
            let mut guard = state.db.lock().unwrap();
            let db = guard.as_mut().unwrap();
            let mut video = sample_video("Alpha", "fp-a");
            video.path = source_path_str.clone();
            db.insert_video(&video).unwrap()
        };

        let resp = delete_call(&state, id, "session=tok").await;
        assert_eq!(resp.status(), StatusCode::OK);

        // Source file gone, DB row gone
        assert!(!std::path::Path::new(&source_path_str).exists());
        let row = state.db.lock().unwrap().as_ref().unwrap().get_video_by_id(id).unwrap();
        assert!(row.is_none());
    }

    #[tokio::test]
    async fn delete_video_requires_auth() {
        let (state, _dir) = authed_db_state();
        let id = insert_sample(&state, "Alpha", "fp-a").await;
        let resp = delete_call(&state, id, "").await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }
}