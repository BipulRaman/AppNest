use crate::manager::{sanitize_for_header, sanitize_log_name, AppManager, SavedApp};
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderValue, Method, Request, StatusCode, Uri};
use axum::middleware::{self, Next};
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

/// CSRF guard for mutating requests. Browsers will only let a same-origin
/// page send custom headers; cross-origin `fetch()` from a malicious site
/// triggers a CORS preflight that we'll never accept (no CORS layer is
/// configured). Any non-GET request that lacks the AppNest header is
/// rejected so a drive-by website can't start/stop apps in the user's
/// browser. GET / HEAD / OPTIONS / SSE traffic is left untouched.
async fn require_csrf_header(
    req: Request<Body>,
    next: Next,
) -> Result<Response, StatusCode> {
    let method = req.method();
    if matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS) {
        return Ok(next.run(req).await);
    }
    let expected = HeaderValue::from_static("AppNest");
    match req.headers().get("x-requested-with") {
        Some(v) if v == expected => Ok(next.run(req).await),
        _ => Err(StatusCode::FORBIDDEN),
    }
}

pub async fn run(manager: Arc<AppManager>) {
    // Keep a clone for post-router lifecycle logging — the original is
    // moved into `.with_state(manager)` below.
    let mgr_for_log = manager.clone();
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
        .route("/api/update-apply", post(apply_update))
        .route("/api/logs", get(get_server_logs))
        .fallback(static_handler)
        .layer(middleware::from_fn(require_csrf_header))
        .with_state(manager);

    let listener = match tokio::net::TcpListener::bind("127.0.0.1:1234").await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("Failed to bind port 1234: {}", e);
            mgr_for_log.log_server(&format!("server: failed to bind 127.0.0.1:1234: {}", e));
            return;
        }
    };
    // Don't .unwrap() — a runtime serve error would tear down the server
    // thread silently while the tray loop kept running, leaving the
    // dashboard a zombie. Log it through the same channel as the rest of
    // server lifecycle so the user can see it in the Server Logs view.
    if let Err(e) = axum::serve(listener, app).await {
        eprintln!("axum::serve exited with error: {}", e);
        mgr_for_log.log_server(&format!("server: axum::serve exited with error: {}", e));
    }
}

async fn static_handler(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };
    // Make sure embedded JS/CSS/JSON updates land immediately after a
    // user upgrades the binary — without these headers, browsers happily
    // serve stale cached app.js / presets.json across versions.
    let cache_header = if path == "index.html" || path.ends_with(".html") {
        "no-cache, no-store, must-revalidate"
    } else {
        "no-cache"
    };
    match Assets::get(path) {
        Some(c) => {
            let mime = from_path(path).first_or_octet_stream();
            let body = match c.data {
                std::borrow::Cow::Borrowed(b) => Body::from(b),
                std::borrow::Cow::Owned(v) => Body::from(v),
            };
            (
                StatusCode::OK,
                [
                    (header::CONTENT_TYPE, mime.as_ref().to_owned()),
                    (header::CACHE_CONTROL, cache_header.to_owned()),
                ],
                body,
            ).into_response()
        }
        None => match Assets::get("index.html") {
            Some(c) => {
                let body = match c.data {
                    std::borrow::Cow::Borrowed(b) => Body::from(b),
                    std::borrow::Cow::Owned(v) => Body::from(v),
                };
                (
                    StatusCode::OK,
                    [
                        (header::CONTENT_TYPE, "text/html".to_owned()),
                        (header::CACHE_CONTROL, "no-cache, no-store, must-revalidate".to_owned()),
                    ],
                    body,
                ).into_response()
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
    #[serde(default)]
    swagger_file: Option<String>,
    #[serde(default)]
    color: Option<String>,
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

/// Validate the user-supplied half of an AppReq. Both the name and the
/// project directory must be non-empty after trimming — otherwise we'd end
/// up with a blank app row whose log file would be literally `.log` and
/// whose project_dir lookup would always fail. Done at the API boundary so
/// the manager's invariants stay simple.
fn validate_app_req(body: &AppReq) -> Result<(), &'static str> {
    if body.name.trim().is_empty() {
        return Err("Name is required");
    }
    // API Mock mode only needs the swagger file — project_dir is optional
    // because the user often just wants to point at a spec sitting anywhere
    // on disk without registering a "project folder".
    let is_apimock = body.swagger_file.as_deref().map(|s| !s.trim().is_empty()).unwrap_or(false);
    if !is_apimock && body.project_dir.trim().is_empty() {
        return Err("Project directory is required");
    }
    if body.project_type.trim().is_empty() {
        return Err("Project type is required");
    }
    if let Some(ref c) = body.color {
        if !c.is_empty() && !ALLOWED_COLORS.contains(&c.as_str()) {
            return Err("Unknown card color");
        }
    }
    Ok(())
}

/// Curated palette of card-tint names the UI is allowed to send. We store
/// names (not hex values) so the CSS layer can map each to different
/// shades for light vs. dark theme without a data migration. Anything
/// outside this set is rejected at the API boundary.
const ALLOWED_COLORS: &[&str] = &[
    "slate", "red", "orange", "amber", "yellow",
    "green", "teal", "cyan", "blue", "indigo", "purple", "pink",
];

// ─── Handlers ───────────────────────────────────────────────────────

async fn list_apps(State(mgr): State<Arc<AppManager>>) -> impl IntoResponse {
    Json(mgr.list_apps())
}

async fn add_app(State(mgr): State<Arc<AppManager>>, Json(body): Json<AppReq>) -> impl IntoResponse {
    if let Err(e) = validate_app_req(&body) { return err(e); }
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
        swagger_file: body.swagger_file,
        order: 0,
        color: body.color.filter(|s| !s.is_empty()),
    };
    let id = mgr.add_app(entry);
    // 0 is the sentinel returned by add_app when the u32 id space is
    // exhausted (4 billion previous adds). Surface it as a hard error
    // so the dashboard doesn't show a phantom row that never materializes.
    if id == 0 {
        return err("Cannot add app: id space exhausted");
    }
    ok_id("Added", id)
}

#[derive(Deserialize)]
struct ReorderReq {
    ids: Vec<u32>,
}

/// Upper bound on the number of ids accepted by /api/apps/reorder. The
/// dashboard will never send more than the user's app count, but a
/// hand-crafted POST with `{"ids": [..1_000_000..]}` would otherwise let
/// us allocate a multi-megabyte Vec<u32> before the manager rejects it.
const REORDER_MAX_IDS: usize = 10_000;

async fn reorder_apps(State(mgr): State<Arc<AppManager>>, Json(body): Json<ReorderReq>) -> impl IntoResponse {
    if body.ids.len() > REORDER_MAX_IDS {
        return err("too many ids");
    }
    match mgr.reorder_apps(body.ids) {
        Ok(()) => ok("Reordered"),
        Err(e) => err(&e),
    }
}

async fn update_app(State(mgr): State<Arc<AppManager>>, Path(id): Path<u32>, Json(body): Json<AppReq>) -> impl IntoResponse {
    if let Err(e) = validate_app_req(&body) { return err(e); }
    // The id and order fields here are placeholders — the manager copies
    // only the user-mutable fields out of `entry` and ignores both.
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
        swagger_file: body.swagger_file,
        order: 0,
        color: body.color.filter(|s| !s.is_empty()),
    };
    match mgr.update_app(id, entry) {
        Ok(()) => ok("Updated"),
        Err(e) => err(&e),
    }
}

async fn delete_app(State(mgr): State<Arc<AppManager>>, Path(id): Path<u32>) -> impl IntoResponse {
    // delete_app calls into stop_runtime which can sleep ~300 ms (Linux
    // SIGTERM grace) and shells out to taskkill on Windows; run it on the
    // blocking pool so we don't park a tokio worker.
    let mgr_c = mgr.clone();
    let result = tokio::task::spawn_blocking(move || mgr_c.delete_app(id))
        .await
        .unwrap_or_else(|e| Err(e.to_string()));
    match result {
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
    // kill_tree blocks for up to ~300 ms (Linux SIGTERM grace) and shells
    // out to taskkill on Windows. Push it to the blocking pool to keep the
    // tokio worker free to handle other requests.
    let mgr_c = mgr.clone();
    let result = tokio::task::spawn_blocking(move || mgr_c.stop_app(id))
        .await
        .unwrap_or_else(|e| Err(e.to_string()));
    match result {
        Ok(()) => ok("Stopped"),
        Err(e) => err(&e),
    }
}

async fn restart_app(State(mgr): State<Arc<AppManager>>, Path(id): Path<u32>, Query(q): Query<StartQuery>) -> impl IntoResponse {
    let skip = q.skip_build.as_deref() == Some("true");
    // restart_app's stop half blocks the tokio worker for the SIGTERM
    // grace; do the stop on the blocking pool, then await start_app
    // normally so build streaming stays on the async runtime.
    let mgr_c = mgr.clone();
    let _ = tokio::task::spawn_blocking(move || mgr_c.stop_app(id)).await;
    match mgr.start_app(id, skip).await {
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

    // We need to re-snapshot when the broadcast channel lags so the dashboard
    // doesn't carry a permanent hole in its log view. Capture an Arc of the
    // manager + id we can move into the stream for that purpose.
    let mgr_for_stream = mgr.clone();

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
                    // Re-subscribe + re-snapshot so the client recovers any
                    // lines that were dropped while the channel was full.
                    yield Ok(Event::default().event("lag").data(n.to_string()));
                    if let Ok((logs_snap, build_snap, new_rx)) = mgr_for_stream.subscribe_logs(id) {
                        rx = new_rx;
                        let payload = serde_json::json!({
                            "logs": logs_snap,
                            "buildLogs": build_snap,
                        });
                        yield Ok(Event::default().event("snapshot").data(payload.to_string()));
                    } else {
                        // App was deleted while we were resyncing. Tell the
                        // dashboard explicitly so it stops EventSource auto-
                        // reconnecting against an id that no longer exists.
                        yield Ok(Event::default().event("deleted").data(""));
                        break;
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    // Sender dropped \u2014 either the app was deleted or the
                    // server is shutting down. Either way, signal the
                    // browser to stop reconnecting.
                    yield Ok(Event::default().event("deleted").data(""));
                    break;
                }
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
    // Sanitize the on-disk log identifier the same way the manager does, then
    // strip header-unsafe characters before placing it inside the
    // Content-Disposition value. Without this an app named `x"\r\nSet-Cookie:`
    // would let a caller inject arbitrary response headers.
    let raw_name = mgr.list_apps().iter().find(|a| a.id == id)
        .map(|a| a.name.clone())
        .unwrap_or_else(|| format!("app-{}", id));
    let safe_name = sanitize_for_header(&sanitize_log_name(&raw_name, id));
    match mgr.get_app_log(id) {
        Ok(log) => {
            let fname = format!("{}-logs.log", safe_name);
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
    let path = tokio::task::spawn_blocking(pick_folder_blocking)
        .await
        .unwrap_or(None);
    Json(PickResp { path })
}

#[derive(Deserialize)]
struct PickQ { ext: Option<String> }

async fn pick_file(Query(q): Query<PickQ>) -> impl IntoResponse {
    // Default to "script" — callers without an ext are picking the script-mode
    // file. The previous "yml" default greyed out everything in the dialog
    // because YAML support was removed.
    let ext = q.ext.unwrap_or_else(|| "script".into());
    let path = tokio::task::spawn_blocking(move || pick_file_blocking(&ext))
        .await
        .unwrap_or(None);
    Json(PickResp { path })
}

// --- Platform-specific pickers ----------------------------------------------
//
// On Windows and Linux `rfd` works fine from a worker thread. On macOS `rfd`
// wraps NSOpenPanel, which REQUIRES the main thread and an active
// NSApplication run loop — neither of which we have in the current headless
// macOS build. Calling it from a tokio worker panics with:
//   "You are running RFD in NonWindowed environment, it is impossible to
//    spawn dialog from thread different than main in this env."
// So on macOS we shell out to AppleScript (`osascript`), which gives us a
// real native Finder picker and doesn't care what thread we're on.

#[cfg(not(target_os = "macos"))]
fn pick_folder_blocking() -> Option<String> {
    #[cfg(windows)]
    let _guard = win_dialog_foreground::spawn();
    rfd::FileDialog::new()
        .set_title("Select Project Folder")
        .pick_folder()
        .map(|p| p.to_string_lossy().to_string())
}

#[cfg(not(target_os = "macos"))]
fn pick_file_blocking(ext: &str) -> Option<String> {
    #[cfg(windows)]
    let _guard = win_dialog_foreground::spawn();
    let mut d = rfd::FileDialog::new().set_title("Select File");
    if ext == "script" {
        d = d.add_filter("Scripts", &["ps1", "bat", "cmd", "sh"]);
    } else if ext == "swagger" {
        d = d.add_filter("Swagger / OpenAPI", &["json"]);
    }
    d.pick_file().map(|p| p.to_string_lossy().to_string())
}

// Windows: native pickers (rfd) inherit no parent because AppNest has no
// app window — only a tray icon. With no owner, Windows places the dialog
// in z-order based on which process currently owns the foreground, and
// since the *browser* owns it when the user clicks "Browse" the dialog
// can appear behind the browser window. Fix: launch a short-lived helper
// thread that polls for any standard-dialog window belonging to this
// process and force-brings it to the foreground using the SetWindowPos
// TOPMOST → NOTOPMOST trick (the only reliable workaround that doesn't
// require AppNest itself to already own the foreground).
#[cfg(windows)]
mod win_dialog_foreground {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use windows_sys::Win32::Foundation::HWND;
    use windows_sys::Win32::System::Threading::GetCurrentProcessId;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        BringWindowToTop, FindWindowExW, GetWindowThreadProcessId, IsWindowVisible,
        SetForegroundWindow, SetWindowPos, HWND_NOTOPMOST, HWND_TOPMOST, SWP_NOMOVE, SWP_NOSIZE,
        SWP_SHOWWINDOW,
    };

    /// Guard that stops the helper thread when dropped (i.e. when the
    /// caller returns from the blocking pick_*_blocking call).
    pub struct Guard {
        stop: Arc<AtomicBool>,
    }

    impl Drop for Guard {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::SeqCst);
        }
    }

    pub fn spawn() -> Guard {
        let stop = Arc::new(AtomicBool::new(false));
        let stop_thread = stop.clone();
        std::thread::Builder::new()
            .name("appnest-dialog-foreground".into())
            .spawn(move || {
                let our_pid = unsafe { GetCurrentProcessId() };
                // The standard Win32 dialog window class is "#32770".
                let class: Vec<u16> = "#32770\0".encode_utf16().collect();
                let mut last_promoted: HWND = 0;
                // Poll up to ~3 s. The dialog usually appears within ~150 ms;
                // we keep checking in case the user opens / closes child
                // browse dialogs (e.g. Common Item dialog spawns sub-windows
                // when navigating shell namespaces) so each gets promoted.
                for _ in 0..60 {
                    if stop_thread.load(Ordering::SeqCst) {
                        return;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(50));
                    let mut hwnd: HWND = 0;
                    loop {
                        hwnd = unsafe {
                            FindWindowExW(0, hwnd, class.as_ptr(), std::ptr::null())
                        };
                        if hwnd == 0 {
                            break;
                        }
                        let mut pid: u32 = 0;
                        unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };
                        if pid != our_pid {
                            continue;
                        }
                        if unsafe { IsWindowVisible(hwnd) } == 0 {
                            continue;
                        }
                        if hwnd == last_promoted {
                            continue;
                        }
                        // TOPMOST → NOTOPMOST flicker is the only documented
                        // workaround that reliably bypasses the Windows
                        // foreground-stealing prevention rules.
                        unsafe {
                            BringWindowToTop(hwnd);
                            SetWindowPos(
                                hwnd,
                                HWND_TOPMOST,
                                0, 0, 0, 0,
                                SWP_NOMOVE | SWP_NOSIZE | SWP_SHOWWINDOW,
                            );
                            SetWindowPos(
                                hwnd,
                                HWND_NOTOPMOST,
                                0, 0, 0, 0,
                                SWP_NOMOVE | SWP_NOSIZE | SWP_SHOWWINDOW,
                            );
                            let _ = SetForegroundWindow(hwnd);
                        }
                        last_promoted = hwnd;
                    }
                }
            })
            .ok();
        Guard { stop }
    }
}

#[cfg(target_os = "macos")]
fn pick_folder_blocking() -> Option<String> {
    run_osascript(
        r#"try
    set chosen to choose folder with prompt "Select Project Folder"
    POSIX path of chosen
on error number -128
    return ""
end try"#,
    )
}

#[cfg(target_os = "macos")]
fn pick_file_blocking(ext: &str) -> Option<String> {
    // Build an `of type {"yml","yaml"}` clause where appropriate so the
    // Finder picker greys out unrelated files, matching the rfd behavior.
    let of_type = match ext {
        "script" => r#" of type {"ps1","bat","cmd","sh"}"#,
        "swagger" => r#" of type {"json"}"#,
        _ => "",
    };
    let script = format!(
        r#"try
    set chosen to choose file with prompt "Select File"{}
    POSIX path of chosen
on error number -128
    return ""
end try"#,
        of_type
    );
    run_osascript(&script)
}

#[cfg(target_os = "macos")]
fn run_osascript(script: &str) -> Option<String> {
    let out = std::process::Command::new("osascript")
        .arg("-e")
        .arg(script)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() { None } else { Some(s) }
}

// ─── Open path in system explorer / terminal ───────────────────────

#[derive(Serialize)]
struct OkResp { ok: bool, error: Option<String> }

fn ok_resp() -> Json<OkResp> { Json(OkResp { ok: true, error: None }) }
fn err_resp(msg: impl Into<String>) -> Json<OkResp> { Json(OkResp { ok: false, error: Some(msg.into()) }) }
/// Spawn a Command and reap it in a background thread so its `Child`
/// handle isn't dropped immediately.
///
/// Without this, every "Open Folder" / "Open Terminal" click would leave
/// a `<defunct>` zombie process on Linux/macOS until AppNest itself exits
/// (Rust's `Child::drop` does NOT call wait()). The explorer.exe / wt /
/// xdg-open / `open` launchers exit within a few ms; the spawned reaper
/// thread is therefore short-lived and self-cleaning.
fn spawn_and_reap(mut cmd: std::process::Command) -> std::io::Result<()> {
    let mut child = cmd.spawn()?;
    // Name the thread so it shows up in process listings / debuggers as
    // "appnest-reap" rather than the unhelpful default "<unnamed>".
    let _ = std::thread::Builder::new()
        .name("appnest-reap".into())
        .spawn(move || { let _ = child.wait(); });
    Ok(())
}
async fn open_explorer(
    State(mgr): State<Arc<AppManager>>,
    Path(id): Path<u32>,
) -> Json<OkResp> {
    let Some(dir) = mgr.get_project_dir(id) else { return err_resp("App not found"); };
    if !std::path::Path::new(&dir).exists() { return err_resp(format!("Path not found: {}", dir)); }
    // explorer.exe interprets some leading-`/` arguments as flags
    // (`/select,`, `/e,`, `/root,`). pick_folder always returns absolute
    // paths, but a hand-crafted update_app PUT could set projectDir to
    // "/select,C:\\Windows". Reject any directory that doesn't look like a
    // bare filesystem path before handing it to the OS shell helpers.
    #[cfg(target_os = "windows")]
    {
        let trimmed = dir.trim_start();
        if trimmed.starts_with('/') || trimmed.starts_with('-') {
            return err_resp("Refusing to open suspicious path");
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        if dir.trim_start().starts_with('-') {
            return err_resp("Refusing to open suspicious path");
        }
    }
    let result = tokio::task::spawn_blocking(move || {
        #[cfg(target_os = "windows")]
        {
            // Spawn explorer.exe directly with the path as a single argv
            // element. This avoids the previous `cmd /C start "" <dir>`
            // shape, where a directory containing &, |, ^, ", or starting
            // with / could be reinterpreted as cmd metacharacters or as
            // flags to `start`.
            let mut c = std::process::Command::new("explorer.exe");
            c.arg(&dir);
            spawn_and_reap(c).map_err(|e| e.to_string())
        }
        #[cfg(target_os = "macos")]
        {
            let mut c = std::process::Command::new("open");
            c.arg(&dir);
            spawn_and_reap(c).map_err(|e| e.to_string())
        }
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            let mut c = std::process::Command::new("xdg-open");
            c.arg(&dir);
            spawn_and_reap(c).map_err(|e| e.to_string())
        }
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
            // Prefer Windows Terminal if available, fall back to powershell.
            let mut wt_cmd = std::process::Command::new("wt");
            wt_cmd.args(["-d", &dir]);
            if spawn_and_reap(wt_cmd).is_ok() { return Ok(()); }
            // Fallback: launch PowerShell directly (no `cmd /C start` wrapper)
            // and pass the directory via an environment variable instead of
            // string-interpolating it into the command line. This means a
            // path containing single quotes, `$`, backticks, semicolons or
            // newlines can never break out of the command.
            //
            // Prefer pwsh (PowerShell 7+) when present — modern Windows 11
            // installs may not ship Windows PowerShell 5.1.
            let exe = if crate::manager::which_on_path("pwsh").is_some() { "pwsh" } else { "powershell" };
            let mut ps = std::process::Command::new(exe);
            ps.args([
                "-NoExit",
                "-NoProfile",
                "-Command",
                "Set-Location -LiteralPath $env:APPNEST_CWD",
            ]);
            ps.env("APPNEST_CWD", &dir);
            spawn_and_reap(ps).map_err(|e| e.to_string())
        }
        #[cfg(target_os = "macos")]
        {
            let mut c = std::process::Command::new("open");
            c.args(["-a", "Terminal", &dir]);
            spawn_and_reap(c).map_err(|e| e.to_string())
        }
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            // Probe modern + legacy terminal emulators in rough order of
            // popularity. Each entry is (binary_name, args-factory). We feed
            // the working directory via each terminal's documented cwd flag
            // when one exists, and for old-school terminals that only support
            // `-e` we use `bash -c '...' bash <dir>` so the directory arrives
            // as $1 — it is NEVER interpolated into the script source, so a
            // path containing quotes, $, backticks, or newlines is harmless.
            type Args = Vec<String>;
            let launchers: [(&str, Box<dyn Fn(&str) -> Args>); 11] = [
                ("alacritty",        Box::new(|d: &str| vec!["--working-directory".into(), d.into()])),
                ("kitty",            Box::new(|d: &str| vec!["--directory".into(), d.into()])),
                ("wezterm",          Box::new(|d: &str| vec!["start".into(), "--cwd".into(), d.into()])),
                ("foot",             Box::new(|d: &str| vec!["--working-directory".into(), d.into()])),
                ("tilix",            Box::new(|d: &str| vec!["-w".into(), d.into()])),
                ("xfce4-terminal",   Box::new(|d: &str| vec![format!("--working-directory={}", d)])),
                ("gnome-terminal",   Box::new(|d: &str| vec![format!("--working-directory={}", d)])),
                ("konsole",          Box::new(|d: &str| vec!["--workdir".into(), d.into()])),
                ("terminator",       Box::new(|d: &str| vec![format!("--working-directory={}", d)])),
                ("x-terminal-emulator",
                    Box::new(|d: &str| vec![
                        "-e".into(),
                        "bash".into(),
                        "-c".into(),
                        "cd \"$1\" && exec bash".into(),
                        "appnest".into(),
                        d.into(),
                    ])),
                ("xterm",            Box::new(|d: &str| vec![
                        "-e".into(),
                        "bash".into(),
                        "-c".into(),
                        "cd \"$1\" && exec bash".into(),
                        "appnest".into(),
                        d.into(),
                    ])),
            ];

            for (prog, make_args) in &launchers {
                let args = make_args(&dir);
                let mut c = std::process::Command::new(prog);
                c.args(&args);
                if spawn_and_reap(c).is_ok() { return Ok(()); }
            }
            Err("No supported terminal emulator found. Tried: alacritty, kitty, wezterm, foot, tilix, xfce4-terminal, gnome-terminal, konsole, terminator, x-terminal-emulator, xterm.".into())
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

/// Returns true if the given GitHub-release asset filename matches the binary
/// we should offer for download on the current platform. We match by the
/// platform suffix so the assertion works whether the asset is named
/// `appnest-windows-x86_64.exe` (unversioned, legacy) or
/// `appnest-1.1.0-windows-x86_64.exe` (versioned, current release pipeline).
/// We also accept the very old plain `appnest.exe` for releases cut before
/// the cross-platform build landed.
fn is_platform_asset(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    #[cfg(target_os = "windows")]
    {
        n == "appnest.exe" || n.ends_with("windows-x86_64.exe")
    }
    #[cfg(target_os = "linux")]
    {
        n.ends_with("linux-x86_64.tar.gz")
    }
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        n.ends_with("macos-arm64.tar.gz")
    }
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    {
        n.ends_with("macos-x86_64.tar.gz")
    }
    #[cfg(not(any(
        target_os = "windows",
        target_os = "linux",
        all(target_os = "macos", any(target_arch = "aarch64", target_arch = "x86_64")),
    )))]
    {
        let _ = n;
        false
    }
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
        let tls_connector = match native_tls::TlsConnector::new() {
            Ok(c) => std::sync::Arc::new(c),
            Err(e) => return Err(format!("tls init: {}", e)),
        };
        let agent = ureq::AgentBuilder::new()
            .timeout(std::time::Duration::from_secs(8))
            .user_agent(&ua)
            .tls_connector(tls_connector)
            .build();
        match agent.get(&url).call() {
            Ok(resp) => resp.into_json::<GhRelease>().map_err(|e| format!("parse: {}", e)),
            Err(ureq::Error::Status(code, resp)) => {
                // Surface the response body for the common 403 rate-limit
                // case so the user can see "API rate limit exceeded…" and
                // its reset timestamp instead of just `github status 403`.
                // Cap the read so a misbehaving server can't make us OOM.
                use std::io::Read;
                let mut body = String::new();
                let _ = resp.into_reader().take(8 * 1024).read_to_string(&mut body);
                let snippet: String = body.chars().take(200).collect();
                if snippet.is_empty() {
                    Err(format!("github status {}", code))
                } else {
                    Err(format!("github status {}: {}", code, snippet))
                }
            }
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
                    if is_platform_asset(name) {
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
    // Validate the URL structurally: scheme must be https, host must be
    // exactly github.com, and the path must live under /BipulRaman/AppNest/.
    // The previous `starts_with("https://github.com/BipulRaman/AppNest/")`
    // accepted things like `https://github.com/BipulRaman/AppNest/../evil`.
    if !is_allowed_update_url(&target) {
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

fn is_allowed_update_url(target: &str) -> bool {
    // Hand-rolled parse to avoid pulling in a `url` crate just for this.
    let Some(rest) = target.strip_prefix("https://") else { return false; };
    let (host_port, path_and_rest) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let host = host_port.split(':').next().unwrap_or("");
    if !host.eq_ignore_ascii_case("github.com") {
        return false;
    }
    // Strip query/fragment, then split path segments and reject any that
    // would let the URL escape the AppNest repo subtree.
    let path = path_and_rest
        .split(['?', '#'])
        .next()
        .unwrap_or("/");
    let mut segs = path.split('/').filter(|s| !s.is_empty());
    // GitHub treats repo names case-insensitively ("BipulRaman/AppNest" ==
    // "bipulraman/appnest" both resolve to the same repo). Match the same
    // policy so a user pasting a lowercase URL doesn't see "URL not
    // allowed".
    if !segs.next().map(|s| s.eq_ignore_ascii_case("BipulRaman")).unwrap_or(false) {
        return false;
    }
    if !segs.next().map(|s| s.eq_ignore_ascii_case("AppNest")).unwrap_or(false) {
        return false;
    }
    // Forbid `..` anywhere in the remaining path so nothing can climb out.
    for s in segs {
        if s == ".." || s == "." { return false; }
    }
    true
}

// ───────────────────────────── In-app self-update ─────────────────────────────
//
// `/api/update-apply` downloads the GitHub release asset matching the current
// platform, replaces the running executable in-place (via the `self_update`
// crate, which uses `self_replace` under the hood — safe to do while the
// process is running), then stops all managed child apps and respawns the
// new binary before exiting. The frontend is expected to surface progress
// and to handle the very brief window during which the HTTP port is down
// between the old and new processes.

fn platform_asset_substring() -> &'static str {
    #[cfg(target_os = "windows")]
    { "windows-x86_64.exe" }
    #[cfg(target_os = "linux")]
    { "linux-x86_64.tar.gz" }
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    { "macos-arm64.tar.gz" }
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    { "macos-x86_64.tar.gz" }
    #[cfg(not(any(
        target_os = "windows",
        target_os = "linux",
        all(target_os = "macos", any(target_arch = "aarch64", target_arch = "x86_64")),
    )))]
    { "" }
}

fn platform_bin_name() -> &'static str {
    #[cfg(windows)] { "appnest.exe" }
    #[cfg(not(windows))] { "appnest" }
}

/// If the running executable's filename embeds the *previous* version (the
/// convention used by GitHub-release downloads — `appnest-1.1.1-windows-x86_64.exe`,
/// `appnest-1.1.1-linux-x86_64`, …), rename the on-disk file so the version
/// segment matches `new_version`. Returns the path that should be used to
/// respawn the process.
///
/// Files that don't match the `appnest-<digits.digits…>-` prefix are left
/// alone (e.g. a stable `appnest.exe` install). Errors are returned to the
/// caller so they can be logged and the un-renamed path used as a fallback.
fn rename_versioned_exe(new_version: &str) -> std::io::Result<std::path::PathBuf> {
    let cur = std::env::current_exe()?;
    let stem = cur
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("");
    let ext = cur.extension().and_then(|s| s.to_str()).unwrap_or("");

    let Some(after_prefix) = stem.strip_prefix("appnest-") else {
        return Ok(cur);
    };
    let Some(dash) = after_prefix.find('-') else {
        return Ok(cur);
    };
    let old_ver = &after_prefix[..dash];
    // Only rewrite if the first segment really looks like a version. This
    // avoids molesting names like `appnest-test-build.exe`.
    if old_ver.is_empty() || !old_ver.chars().all(|c| c.is_ascii_digit() || c == '.') {
        return Ok(cur);
    }
    if old_ver == new_version {
        return Ok(cur);
    }
    let rest = &after_prefix[dash..]; // includes leading '-'
    let new_stem = format!("appnest-{}{}", new_version, rest);
    let new_name = if ext.is_empty() {
        new_stem
    } else {
        format!("{}.{}", new_stem, ext)
    };
    let new_path = cur.with_file_name(&new_name);
    if new_path == cur {
        return Ok(cur);
    }
    // NTFS allows renaming a running executable; POSIX always does. If the
    // target already exists (left over from a previous attempt), bail out
    // rather than clobber it.
    if new_path.exists() {
        return Ok(cur);
    }
    std::fs::rename(&cur, &new_path)?;
    Ok(new_path)
}

async fn apply_update(State(manager): State<Arc<AppManager>>) -> Json<OkResp> {
    let current = env!("CARGO_PKG_VERSION").to_string();
    let asset_target = platform_asset_substring();
    if asset_target.is_empty() {
        return err_resp("self-update not supported on this platform");
    }

    // The download + on-disk swap is fully blocking (network I/O + file
    // rename), so off-load it from the async runtime.
    let result: Result<String, String> = tokio::task::spawn_blocking(move || {
        let updater = self_update::backends::github::Update::configure()
            .repo_owner("BipulRaman")
            .repo_name("AppNest")
            .bin_name(platform_bin_name())
            .target(asset_target)
            .show_download_progress(false)
            .show_output(false)
            .no_confirm(true)
            .current_version(&current)
            .build()
            .map_err(|e| format!("configure: {}", e))?;
        let status = updater.update().map_err(|e| format!("update: {}", e))?;
        Ok(status.version().to_string())
    })
    .await
    .unwrap_or_else(|e| Err(format!("join: {}", e)));

    match result {
        Ok(new_version) => {
            manager.log_server(&format!(
                "update: replaced binary on disk, now v{} — restarting",
                new_version
            ));

            // If the user launched a release-page download whose filename
            // encodes the old version (e.g. `appnest-1.1.1-windows-x86_64.exe`),
            // rename the file on disk so the filename stays truthful. NTFS
            // allows renaming a currently-running executable. We still spawn
            // the *new* path on restart so the next self-check sees the
            // correct name. Any taskbar / Start-menu shortcut pinned to the
            // old name will need to be re-pinned — this is the deliberate
            // trade-off of Option A.
            let restart_path = match rename_versioned_exe(&new_version) {
                Ok(p) => {
                    if let Ok(orig) = std::env::current_exe() {
                        if p != orig {
                            manager.log_server(&format!(
                                "update: renamed {} → {}",
                                orig.display(),
                                p.display()
                            ));
                        }
                    }
                    p
                }
                Err(e) => {
                    manager.log_server(&format!("update: rename skipped: {}", e));
                    std::env::current_exe().unwrap_or_default()
                }
            };

            // Give the HTTP response time to flush, then stop all managed
            // children and relaunch ourselves. On Windows the running .exe
            // was atomically renamed to *.old by self_replace, so spawning
            // the (possibly renamed) path picks up the *new* binary that
            // was moved into place.
            let mgr = manager.clone();
            tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_millis(750)).await;
                mgr.stop_all();
                // Brief grace period for OS to reap child processes before
                // the new instance tries to bind the same port.
                tokio::time::sleep(std::time::Duration::from_millis(400)).await;
                let _ = std::process::Command::new(&restart_path).spawn();
                std::process::exit(0);
            });
            Json(OkResp { ok: true, error: Some(format!("Updated to v{}", new_version)) })
        }
        Err(e) => {
            // Surface the full underlying cause (TLS error, proxy 403,
            // AV-deleted file, permission denied on the install dir, …)
            // both in the HTTP response *and* the in-app Server Logs so
            // the user can copy/paste it for IT instead of just seeing
            // "Update failed".
            manager.log_server(&format!("update: failed: {}", e));
            err_resp(e)
        }
    }
}
