use crate::manager::{AppManager, SavedApp};
use axum::extract::{Path, Query, State};
use axum::http::{header, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use axum::body::Body;
use mime_guess::from_path;
use rust_embed::RustEmbed;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

#[derive(RustEmbed)]
#[folder = "public/"]
struct Assets;

pub async fn run(manager: Arc<AppManager>) {
    let app = Router::new()
        .route("/api/apps", get(list_apps).post(add_app))
        .route("/api/apps/:id", put(update_app).delete(delete_app))
        .route("/api/apps/:id/start", post(start_app))
        .route("/api/apps/:id/stop", post(stop_app))
        .route("/api/apps/:id/restart", post(restart_app))
        .route("/api/apps/:id/logs", get(get_logs))
        .route("/api/apps/:id/applogs", get(get_app_logs))
        .route("/api/apps/:id/applogs/export", get(export_app_logs))
        .route("/api/pick-folder", get(pick_folder))
        .route("/api/pick-file", get(pick_file))
        .route("/api/logs", get(get_server_logs))
        .fallback(static_handler)
        .with_state(manager);

    let listener = match tokio::net::TcpListener::bind("127.0.0.1:1234").await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("Failed to bind port 1234: {}", e);
            return;
        }
    };
    axum::serve(listener, app).await.unwrap();
}

async fn static_handler(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };
    match Assets::get(path) {
        Some(c) => {
            let mime = from_path(path).first_or_octet_stream();
            let body = match c.data {
                std::borrow::Cow::Borrowed(b) => Body::from(b),
                std::borrow::Cow::Owned(v) => Body::from(v),
            };
            (StatusCode::OK, [(header::CONTENT_TYPE, mime.as_ref().to_owned())], body).into_response()
        }
        None => match Assets::get("index.html") {
            Some(c) => {
                let body = match c.data {
                    std::borrow::Cow::Borrowed(b) => Body::from(b),
                    std::borrow::Cow::Owned(v) => Body::from(v),
                };
                (StatusCode::OK, [(header::CONTENT_TYPE, "text/html".to_owned())], body).into_response()
            }
            None => StatusCode::NOT_FOUND.into_response(),
        },
    }
}

// ─── Types ──────────────────────────────────────────────────────────

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AppReq {
    name: String,
    project_dir: String,
    project_type: String,
    #[serde(default)]
    build_steps: Vec<String>,
    run_command: Option<String>,
    static_dir: Option<String>,
    port: Option<u16>,
    #[serde(default)]
    env_vars: HashMap<String, String>,
    #[serde(default)]
    auto_start: bool,
    #[serde(default)]
    script_file: Option<String>,
}

#[derive(Deserialize)]
struct StartQuery {
    #[serde(rename = "skipBuild")]
    skip_build: Option<String>,
}

#[derive(Serialize)]
struct Msg {
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

fn ok(m: &str) -> (StatusCode, Json<Msg>) {
    (StatusCode::OK, Json(Msg { message: Some(m.into()), id: None, error: None }))
}
fn ok_id(m: &str, id: u32) -> (StatusCode, Json<Msg>) {
    (StatusCode::OK, Json(Msg { message: Some(m.into()), id: Some(id), error: None }))
}
fn err(m: &str) -> (StatusCode, Json<Msg>) {
    (StatusCode::BAD_REQUEST, Json(Msg { message: None, id: None, error: Some(m.into()) }))
}

// ─── Handlers ───────────────────────────────────────────────────────

async fn list_apps(State(mgr): State<Arc<AppManager>>) -> impl IntoResponse {
    Json(mgr.list_apps())
}

async fn add_app(State(mgr): State<Arc<AppManager>>, Json(body): Json<AppReq>) -> impl IntoResponse {
    let entry = SavedApp {
        id: 0,
        name: body.name,
        project_dir: body.project_dir,
        project_type: body.project_type,
        build_steps: body.build_steps,
        run_command: body.run_command,
        static_dir: body.static_dir,
        port: body.port,
        env_vars: body.env_vars,
        auto_start: body.auto_start,
        script_file: body.script_file,
    };
    let id = mgr.add_app(entry);
    ok_id("Added", id)
}

async fn update_app(State(mgr): State<Arc<AppManager>>, Path(id): Path<u32>, Json(body): Json<AppReq>) -> impl IntoResponse {
    let entry = SavedApp {
        id,
        name: body.name,
        project_dir: body.project_dir,
        project_type: body.project_type,
        build_steps: body.build_steps,
        run_command: body.run_command,
        static_dir: body.static_dir,
        port: body.port,
        env_vars: body.env_vars,
        auto_start: body.auto_start,
        script_file: body.script_file,
    };
    match mgr.update_app(id, entry) {
        Ok(()) => ok("Updated"),
        Err(e) => err(&e),
    }
}

async fn delete_app(State(mgr): State<Arc<AppManager>>, Path(id): Path<u32>) -> impl IntoResponse {
    match mgr.delete_app(id) {
        Ok(()) => ok("Deleted"),
        Err(e) => err(&e),
    }
}

async fn start_app(State(mgr): State<Arc<AppManager>>, Path(id): Path<u32>, Query(q): Query<StartQuery>) -> impl IntoResponse {
    let skip = q.skip_build.as_deref() == Some("true");
    match mgr.start_app(id, skip).await {
        Ok(()) => ok("Started"),
        Err(e) => err(&e),
    }
}

async fn stop_app(State(mgr): State<Arc<AppManager>>, Path(id): Path<u32>) -> impl IntoResponse {
    match mgr.stop_app(id) {
        Ok(()) => ok("Stopped"),
        Err(e) => err(&e),
    }
}

async fn restart_app(State(mgr): State<Arc<AppManager>>, Path(id): Path<u32>, Query(q): Query<StartQuery>) -> impl IntoResponse {
    let skip = q.skip_build.as_deref() == Some("true");
    match mgr.restart_app(id, skip).await {
        Ok(()) => ok("Restarted"),
        Err(e) => err(&e),
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct LogsResp { logs: String, build_logs: String }

async fn get_logs(State(mgr): State<Arc<AppManager>>, Path(id): Path<u32>) -> impl IntoResponse {
    match mgr.get_logs(id) {
        Ok((logs, build_logs)) => (StatusCode::OK, Json(LogsResp { logs, build_logs })).into_response(),
        Err(e) => err(&e).into_response(),
    }
}

#[derive(Serialize)]
struct LogResp { log: String }

async fn get_app_logs(State(mgr): State<Arc<AppManager>>, Path(id): Path<u32>) -> impl IntoResponse {
    match mgr.get_app_log(id) {
        Ok(log) => (StatusCode::OK, Json(LogResp { log })).into_response(),
        Err(e) => err(&e).into_response(),
    }
}

async fn export_app_logs(State(mgr): State<Arc<AppManager>>, Path(id): Path<u32>) -> impl IntoResponse {
    let app_name = mgr.list_apps().iter().find(|a| a.id == id).map(|a| a.name.replace(' ', "_")).unwrap_or_else(|| format!("app-{}", id));
    match mgr.get_app_log(id) {
        Ok(log) => {
            let fname = format!("{}-logs.log", app_name);
            (StatusCode::OK, [
                (header::CONTENT_TYPE, "text/plain; charset=utf-8"),
                (header::CONTENT_DISPOSITION, &format!("attachment; filename=\"{}\"", fname)),
            ], log).into_response()
        }
        Err(e) => err(&e).into_response(),
    }
}

async fn get_server_logs(State(mgr): State<Arc<AppManager>>) -> impl IntoResponse {
    Json(LogResp { log: mgr.get_server_log() })
}

// ─── Native File Dialogs ────────────────────────────────────────────

#[derive(Serialize)]
struct PickResp { path: Option<String> }

async fn pick_folder() -> impl IntoResponse {
    let path = tokio::task::spawn_blocking(|| {
        rfd::FileDialog::new().set_title("Select Project Folder").pick_folder().map(|p| p.to_string_lossy().to_string())
    }).await.unwrap_or(None);
    Json(PickResp { path })
}

#[derive(Deserialize)]
struct PickQ { ext: Option<String> }

async fn pick_file(Query(q): Query<PickQ>) -> impl IntoResponse {
    let ext = q.ext.unwrap_or_else(|| "yml".into());
    let path = tokio::task::spawn_blocking(move || {
        let mut d = rfd::FileDialog::new().set_title("Select File");
        if ext == "yml" { d = d.add_filter("YAML", &["yml", "yaml"]); }
        else if ext == "script" { d = d.add_filter("Scripts", &["ps1", "bat", "cmd", "sh"]); }
        d.pick_file().map(|p| p.to_string_lossy().to_string())
    }).await.unwrap_or(None);
    Json(PickResp { path })
}
