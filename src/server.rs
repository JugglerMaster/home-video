use axum::{
    extract::{Json, Path as PathExtract, State},
    http::{
        header::CONTENT_TYPE,
        request::Parts,
        StatusCode,
    },
    middleware::from_extractor_with_state,
    response::Response,
    routing::{get, post},
    Router,
};
use axum::body::{Body, Bytes};
use axum::http::Request;
use tower_http::trace::TraceLayer;

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use rand::Rng;
use serde::Deserialize;
use serde_json::json;

use crate::auth;
use crate::config;
use crate::config::Config;
use crate::db::Db;

const STATIC_DIR: &str = "./static";
const CONFIG_PATH_DEFAULT: &str = "./config.json";

/// Shared application state. The `Arc`s are shared between the config-guard
/// middleware and the handlers, so the wizard's config write is visible to
/// every subsequent request.
#[derive(Clone)]
pub struct AppState {
    pub config_path: PathBuf,
    pub port: u16,
    pub config: Arc<Mutex<Option<Config>>>,
    pub sessions: Arc<Mutex<HashSet<String>>>,
}

impl AppState {
    pub fn new(config_path: PathBuf, config: Option<Config>, port: u16) -> Self {
        Self {
            config_path,
            port,
            config: Arc::new(Mutex::new(config)),
            sessions: Arc::new(Mutex::new(HashSet::new())),
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
fn session_token(req: &Request<Body>) -> Option<String> {
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

fn is_authenticated(state: &AppState, req: &Request<Body>) -> bool {
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
    if let Err(e) = Db::new(&db_path).and_then(|db| db.create_schema().map(|_| ())) {
        return json_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            json!({"error": format!("init db: {e}")}),
        );
    }

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
        .header("Set-Cookie", format!("session={token}; Path=/; HttpOnly; Max-Age=86400"))
        .body(Body::from(Bytes::from_static(b"")))
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
    let state = AppState::new(config_path, existing_config, port);

    let app = Router::new()
        .route("/", get(index))
        .route("/setup", get(setup_get).post(setup_post))
        .route("/admin", get(admin_get))
        .route("/admin/login", post(login))
        .route("/admin/logout", post(logout))
        .route("/api/session", get(session))
        .route("/health", get(|| async { "ok".to_string() }))
        .route("/*path", get(static_handler))
        .with_state(state.clone())
        .layer(from_extractor_with_state::<ConfigGuard, AppState>(state.clone()))
        .layer(TraceLayer::new_for_http());

    let addr = std::net::SocketAddr::from(([0, 0, 0, 0], port));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    println!("home-video listening on http://{addr}");
    axum::serve(listener, app).await?;
    Ok(())
}
