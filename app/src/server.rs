use crate::manager::{AppManager, SavedApp};
use axum::extract::{Path, Query, State};
use axum::http::{header, StatusCode, Uri};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use axum::body::Body;
use futures_util::stream::Stream;
use mime_guess::from_path;
use rust_embed::RustEmbed;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::convert::Infallible;
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
        .route("/api/apps/reorder", post(reorder_apps))
        .route("/api/apps/:id/logs", get(get_logs))
        .route("/api/apps/:id/logs/stream", get(stream_logs))
        .route("/api/apps/:id/applogs", get(get_app_logs))
        .route("/api/apps/:id/applogs/export", get(export_app_logs))
        .route("/api/pick-folder", get(pick_folder))
        .route("/api/pick-file", get(pick_file))
        .route("/api/apps/:id/open-explorer", post(open_explorer))
        .route("/api/apps/:id/open-terminal", post(open_terminal))
        .route("/api/update-check", get(check_update))
        .route("/api/update-open", post(open_update_page))
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
        order: 0,
    };
    let id = mgr.add_app(entry);
    ok_id("Added", id)
}

#[derive(Deserialize)]
struct ReorderReq {
    ids: Vec<u32>,
}

async fn reorder_apps(State(mgr): State<Arc<AppManager>>, Json(body): Json<ReorderReq>) -> impl IntoResponse {
    match mgr.reorder_apps(body.ids) {
        Ok(()) => ok("Reordered"),
        Err(e) => err(&e),
    }
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
        order: 0,
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

async fn stream_logs(
    State(mgr): State<Arc<AppManager>>,
    Path(id): Path<u32>,
) -> Response {
    let (logs_snap, build_snap, mut rx) = match mgr.subscribe_logs(id) {
        Ok(v) => v,
        Err(e) => return err(&e).into_response(),
    };

    // Emit a one-time "snapshot" event, then stream live lines.
    let stream = async_stream::stream! {
        let payload = serde_json::json!({
            "logs": logs_snap,
            "buildLogs": build_snap,
        });
        yield Ok::<_, Infallible>(Event::default().event("snapshot").data(payload.to_string()));

        loop {
            match rx.recv().await {
                Ok(line) => {
                    let payload = serde_json::json!({
                        "kind": line.kind,
                        "text": line.text,
                    });
                    yield Ok(Event::default().event("line").data(payload.to_string()));
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    yield Ok(Event::default().event("lag").data(n.to_string()));
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    };

    // Annotate stream type so Sse<S> is well-typed
    let stream: std::pin::Pin<Box<dyn Stream<Item = Result<Event, Infallible>> + Send>> =
        Box::pin(stream);

    Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response()
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

// ─── Open path in system explorer / terminal ───────────────────────

#[derive(Serialize)]
struct OkResp { ok: bool, error: Option<String> }

fn ok_resp() -> Json<OkResp> { Json(OkResp { ok: true, error: None }) }
fn err_resp(msg: impl Into<String>) -> Json<OkResp> { Json(OkResp { ok: false, error: Some(msg.into()) }) }

async fn open_explorer(
    State(mgr): State<Arc<AppManager>>,
    Path(id): Path<u32>,
) -> Json<OkResp> {
    let Some(dir) = mgr.get_project_dir(id) else { return err_resp("App not found"); };
    if !std::path::Path::new(&dir).exists() { return err_resp(format!("Path not found: {}", dir)); }
    let result = tokio::task::spawn_blocking(move || {
        #[cfg(target_os = "windows")]
        {
            // Use ShellExecute via `cmd /C start` so the new Explorer window is brought to the foreground.
            // The empty "" after start is the window title (required when the first arg is quoted).
            std::process::Command::new("cmd")
                .args(["/C", "start", "", &dir])
                .spawn()
                .map(|_| ())
                .map_err(|e| e.to_string())
        }
        #[cfg(target_os = "macos")]
        { std::process::Command::new("open").arg(&dir).spawn().map(|_| ()).map_err(|e| e.to_string()) }
        #[cfg(all(unix, not(target_os = "macos")))]
        { std::process::Command::new("xdg-open").arg(&dir).spawn().map(|_| ()).map_err(|e| e.to_string()) }
    }).await.unwrap_or_else(|e| Err(e.to_string()));
    match result { Ok(()) => ok_resp(), Err(e) => err_resp(e) }
}

async fn open_terminal(
    State(mgr): State<Arc<AppManager>>,
    Path(id): Path<u32>,
) -> Json<OkResp> {
    let Some(dir) = mgr.get_project_dir(id) else { return err_resp("App not found"); };
    if !std::path::Path::new(&dir).exists() { return err_resp(format!("Path not found: {}", dir)); }
    let result = tokio::task::spawn_blocking(move || -> Result<(), String> {
        #[cfg(target_os = "windows")]
        {
            // Prefer Windows Terminal if available, fall back to powershell
            let wt = std::process::Command::new("wt")
                .args(["-d", &dir])
                .spawn();
            if wt.is_ok() { return Ok(()); }
            std::process::Command::new("cmd")
                .args(["/C", "start", "powershell", "-NoExit", "-Command", &format!("Set-Location -LiteralPath '{}'", dir.replace('\'', "''"))])
                .spawn()
                .map(|_| ())
                .map_err(|e| e.to_string())
        }
        #[cfg(target_os = "macos")]
        {
            std::process::Command::new("open")
                .args(["-a", "Terminal", &dir])
                .spawn()
                .map(|_| ())
                .map_err(|e| e.to_string())
        }
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            // Try a few common terminals
            for prog in ["x-terminal-emulator", "gnome-terminal", "konsole", "xterm"] {
                let mut cmd = std::process::Command::new(prog);
                let spawned = match prog {
                    "gnome-terminal" => cmd.args(["--working-directory", &dir]).spawn(),
                    _ => cmd.args(["-e", &format!("bash -c 'cd \"{}\" && exec bash'", dir)]).spawn(),
                };
                if spawned.is_ok() { return Ok(()); }
            }
            Err("No terminal emulator found".into())
        }
    }).await.unwrap_or_else(|e| Err(e.to_string()));
    match result { Ok(()) => ok_resp(), Err(e) => err_resp(e) }
}

// ─── Self-update check (GitHub releases) ───────────────────────────

const UPDATE_REPO: &str = "BipulRaman/AppNest";
const UPDATE_RELEASES_URL: &str = "https://github.com/BipulRaman/AppNest/releases";

#[derive(Serialize)]
struct UpdateInfo {
    current: String,
    latest: Option<String>,
    update_available: bool,
    release_url: String,
    asset_url: Option<String>,
    error: Option<String>,
}

#[derive(Deserialize)]
struct GhRelease {
    tag_name: Option<String>,
    html_url: Option<String>,
    prerelease: Option<bool>,
    draft: Option<bool>,
    assets: Option<Vec<GhAsset>>,
}

#[derive(Deserialize)]
struct GhAsset {
    name: Option<String>,
    browser_download_url: Option<String>,
}

fn parse_version(s: &str) -> Vec<u32> {
    s.trim_start_matches('v')
        .split(|c: char| !c.is_ascii_digit())
        .filter(|p| !p.is_empty())
        .map(|p| p.parse::<u32>().unwrap_or(0))
        .collect()
}

fn version_gt(a: &str, b: &str) -> bool {
    let av = parse_version(a);
    let bv = parse_version(b);
    for i in 0..av.len().max(bv.len()) {
        let x = av.get(i).copied().unwrap_or(0);
        let y = bv.get(i).copied().unwrap_or(0);
        if x != y { return x > y; }
    }
    false
}

async fn check_update() -> Json<UpdateInfo> {
    let current = env!("CARGO_PKG_VERSION").to_string();
    let url = format!("https://api.github.com/repos/{}/releases/latest", UPDATE_REPO);
    let ua = format!("AppNest/{}", current);
    // ureq is blocking; run on the blocking pool so we don't stall the async runtime.
    let result: Result<GhRelease, String> = tokio::task::spawn_blocking(move || {
        let agent = ureq::AgentBuilder::new()
            .timeout(std::time::Duration::from_secs(8))
            .user_agent(&ua)
            .build();
        match agent.get(&url).call() {
            Ok(resp) => resp.into_json::<GhRelease>().map_err(|e| format!("parse: {}", e)),
            Err(ureq::Error::Status(code, _)) => Err(format!("github status {}", code)),
            Err(e) => Err(format!("request: {}", e)),
        }
    })
    .await
    .unwrap_or_else(|e| Err(format!("join: {}", e)));

    match result {
        Ok(rel) => {
            let is_bad = rel.draft.unwrap_or(false) || rel.prerelease.unwrap_or(false);
            let latest = rel.tag_name.clone().unwrap_or_default();
            let asset_url = rel.assets.as_ref().and_then(|assets| {
                assets.iter().find_map(|a| {
                    let name = a.name.as_deref().unwrap_or("");
                    if name.eq_ignore_ascii_case("appnest.exe") {
                        a.browser_download_url.clone()
                    } else { None }
                })
            });
            let release_url = rel.html_url.unwrap_or_else(|| UPDATE_RELEASES_URL.into());
            let update_available = !is_bad && !latest.is_empty() && version_gt(&latest, &current);
            Json(UpdateInfo {
                current,
                latest: if latest.is_empty() { None } else { Some(latest) },
                update_available,
                release_url,
                asset_url,
                error: None,
            })
        }
        Err(e) => Json(UpdateInfo {
            current,
            latest: None,
            update_available: false,
            release_url: UPDATE_RELEASES_URL.into(),
            asset_url: None,
            error: Some(e),
        }),
    }
}

#[derive(Deserialize)]
struct OpenUrlReq { url: Option<String> }

async fn open_update_page(Json(body): Json<OpenUrlReq>) -> Json<OkResp> {
    let target = body.url.unwrap_or_else(|| UPDATE_RELEASES_URL.into());
    if !target.starts_with("https://github.com/BipulRaman/AppNest/") {
        return err_resp("URL not allowed");
    }
    match tokio::task::spawn_blocking(move || open::that_detached(&target).map_err(|e| e.to_string()))
        .await
        .unwrap_or_else(|e| Err(e.to_string()))
    {
        Ok(()) => ok_resp(),
        Err(e) => err_resp(e),
    }
}
