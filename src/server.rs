use axum::{
    extract::Path as PathExtract,
    http::{header::CONTENT_TYPE, StatusCode},
    response::Response,
    routing::get,
    Router,
};
use axum::body::{Body, Bytes};
use tower_http::trace::TraceLayer;

const STATIC_DIR: &str = "./static";

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

/// A plain-text response with the given status.
fn plain(status: StatusCode, body: &str) -> Response {
    Response::builder()
        .status(status)
        .header(CONTENT_TYPE, "text/plain; charset=utf-8")
        .body(Body::from(Bytes::from(body.as_bytes().to_vec())))
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

/// Boot the web server. Serves the static UI from `static/` and a health
/// endpoint. Returns after the listener is closed.
pub async fn run() -> anyhow::Result<()> {
    let app = Router::new()
        .route("/", get(index))
        .route("/*path", get(static_handler))
        .route("/health", get(|| async { "ok".to_string() }))
        .layer(TraceLayer::new_for_http());

    let port = std::env::var("HOME_VIDEO_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(8080);
    let addr = std::net::SocketAddr::from(([0, 0, 0, 0], port));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    println!("home-video listening on http://{addr}");
    axum::serve(listener, app).await?;
    Ok(())
}
