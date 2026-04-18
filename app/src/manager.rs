use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tokio::io::AsyncBufReadExt;
use tokio::runtime::Handle;

#[cfg(windows)]
use std::os::windows::process::CommandExt;

// ─── Persisted App Config (replaces YAML) ───────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedApp {
    pub id: u32,
    pub name: String,
    pub project_dir: String,
    pub project_type: String,
    #[serde(default)]
    pub build_steps: Vec<String>,
    #[serde(default)]
    pub run_command: Option<String>,
    #[serde(default)]
    pub static_dir: Option<String>,
    #[serde(default)]
    pub port: Option<u16>,
    #[serde(default)]
    pub env_vars: HashMap<String, String>,
    #[serde(default)]
    pub auto_start: bool,
    #[serde(default)]
    pub script_file: Option<String>,
    #[serde(default)]
    pub order: u32,
}

// ─── Runtime State ──────────────────────────────────────────────────

const LOG_CAP: usize = 2000;
const LOG_BROADCAST_CAP: usize = 256;

#[derive(Debug, Clone, Serialize)]
pub struct LogLine {
    pub kind: &'static str, // "run" or "build"
    pub text: String,
}

#[derive(Clone)]
pub struct LogSink {
    buf: Arc<Mutex<VecDeque<String>>>,
    tx: tokio::sync::broadcast::Sender<LogLine>,
    kind: &'static str,
}

impl LogSink {
    fn new(tx: tokio::sync::broadcast::Sender<LogLine>, kind: &'static str) -> Self {
        Self {
            buf: Arc::new(Mutex::new(VecDeque::with_capacity(LOG_CAP))),
            tx,
            kind,
        }
    }
    pub fn push(&self, text: String) {
        {
            let mut v = self.buf.lock().unwrap();
            v.push_back(text.clone());
            if v.len() > LOG_CAP { v.pop_front(); }
        }
        let _ = self.tx.send(LogLine { kind: self.kind, text });
    }
    fn clear(&self) { self.buf.lock().unwrap().clear(); }
    fn snapshot(&self) -> String {
        let v = self.buf.lock().unwrap();
        let mut out = String::new();
        for s in v.iter() { out.push_str(s); out.push('\n'); }
        out
    }
}

struct AppRuntime {
    entry: SavedApp,
    status: String,
    pid: Option<u32>,
    /// PID of the *build* step currently running (if any). Separate from `pid`
    /// because builds and runs are distinct child processes and we want to be
    /// able to cancel a build-in-progress without affecting any later run.
    build_pid: Option<u32>,
    /// Set by `stop_app` when the user asks to cancel a starting/building app.
    /// The build loop checks this between steps and before awaiting the next
    /// child, so a Stop click interrupts the whole chain promptly.
    cancel_requested: Arc<AtomicBool>,
    static_shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    logs: LogSink,
    build_logs: LogSink,
    log_tx: tokio::sync::broadcast::Sender<LogLine>,
    started_at: Option<u64>, // unix seconds when entered "running"
}

struct ManagerState {
    apps: HashMap<u32, AppRuntime>,
    next_id: u32,
}

// ─── API Response ───────────────────────────────────────────────────

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppResponse {
    pub id: u32,
    pub name: String,
    pub project_dir: String,
    #[serde(rename = "type")]
    pub project_type: String,
    pub build_steps: Vec<String>,
    pub run_command: Option<String>,
    pub static_dir: Option<String>,
    pub port: Option<u16>,
    pub env_vars: HashMap<String, String>,
    pub status: String,
    pub pid: Option<u32>,
    pub building: bool,
    pub auto_start: bool,
    pub script_file: Option<String>,
    pub order: u32,
    pub started_at: Option<u64>,
    pub uptime_seconds: Option<u64>,
}

// ─── App Manager ────────────────────────────────────────────────────

pub struct AppManager {
    state: Mutex<ManagerState>,
    data_file: PathBuf,
    logs_dir: PathBuf,
    rt_handle: Handle,
}

impl AppManager {
    pub fn new(rt_handle: Handle) -> Self {
        let app_data = default_data_root().unwrap_or_else(|| {
            std::env::current_exe()
                .ok()
                .and_then(|p| p.parent().map(|p| p.to_path_buf()))
                .unwrap_or_else(|| std::env::current_dir().unwrap())
        });

        let base_dir = app_data.join("AppNest");
        let data_file = base_dir.join("apps.json");
        let logs_dir = base_dir.join("logs");
        let _ = fs::create_dir_all(&logs_dir);

        Self {
            state: Mutex::new(ManagerState {
                apps: HashMap::new(),
                next_id: 1,
            }),
            data_file,
            logs_dir,
            rt_handle,
        }
    }

    pub fn load(&self) {
        let dir = self.data_file.parent().unwrap();
        let _ = fs::create_dir_all(dir);
        if !self.data_file.exists() {
            return;
        }
        if let Ok(content) = fs::read_to_string(&self.data_file) {
            if let Ok(saved) = serde_json::from_str::<Vec<SavedApp>>(&content) {
                let mut state = self.state.lock().unwrap();
                for a in saved {
                    if a.id >= state.next_id {
                        state.next_id = a.id + 1;
                    }
                    state.apps.insert(a.id, {
                        let (log_tx, _) = tokio::sync::broadcast::channel(LOG_BROADCAST_CAP);
                        AppRuntime {
                            entry: a,
                            status: "stopped".into(),
                            pid: None,
                            build_pid: None,
                            cancel_requested: Arc::new(AtomicBool::new(false)),
                            static_shutdown: None,
                            logs: LogSink::new(log_tx.clone(), "run"),
                            build_logs: LogSink::new(log_tx.clone(), "build"),
                            log_tx,
                            started_at: None,
                        }
                    });
                }
            }
        }
    }

    fn save(&self) {
        let state = self.state.lock().unwrap();
        let saved: Vec<&SavedApp> = state.apps.values().map(|a| &a.entry).collect();
        let _ = fs::create_dir_all(self.data_file.parent().unwrap());
        let _ = fs::write(&self.data_file, serde_json::to_string_pretty(&saved).unwrap());
    }

    pub fn list_apps(&self) -> Vec<AppResponse> {
        let state = self.state.lock().unwrap();
        let mut list: Vec<_> = state.apps.values().collect();
        list.sort_by_key(|a| a.entry.order);
        let now = now_secs();
        list.into_iter().map(|a| AppResponse {
            id: a.entry.id,
            name: a.entry.name.clone(),
            project_dir: a.entry.project_dir.clone(),
            project_type: a.entry.project_type.clone(),
            build_steps: a.entry.build_steps.clone(),
            run_command: a.entry.run_command.clone(),
            static_dir: a.entry.static_dir.clone(),
            port: a.entry.port,
            env_vars: a.entry.env_vars.clone(),
            status: a.status.clone(),
            pid: a.pid,
            building: a.status == "building",
            auto_start: a.entry.auto_start,
            script_file: a.entry.script_file.clone(),
            order: a.entry.order,
            started_at: a.started_at,
            uptime_seconds: a.started_at.map(|s| now.saturating_sub(s)),
        }).collect()
    }

    pub fn get_project_dir(&self, id: u32) -> Option<String> {
        let state = self.state.lock().unwrap();
        state.apps.get(&id).map(|a| a.entry.project_dir.clone())
    }

    pub fn reorder_apps(&self, ids: Vec<u32>) -> Result<(), String> {
        let mut state = self.state.lock().unwrap();
        for (i, id) in ids.iter().enumerate() {
            if let Some(app) = state.apps.get_mut(id) {
                app.entry.order = i as u32;
            }
        }
        drop(state);
        self.save();
        Ok(())
    }

    pub fn add_app(&self, app: SavedApp) -> u32 {
        let mut state = self.state.lock().unwrap();
        let id = state.next_id;
        state.next_id += 1;
        let max_order = state.apps.values().map(|a| a.entry.order).max().unwrap_or(0);
        let mut entry = app;
        entry.id = id;
        entry.order = if state.apps.is_empty() { 0 } else { max_order + 1 };
        state.apps.insert(id, {
            let (log_tx, _) = tokio::sync::broadcast::channel(LOG_BROADCAST_CAP);
            AppRuntime {
                entry,
                status: "stopped".into(),
                pid: None,
                build_pid: None,
                cancel_requested: Arc::new(AtomicBool::new(false)),
                static_shutdown: None,
                logs: LogSink::new(log_tx.clone(), "run"),
                build_logs: LogSink::new(log_tx.clone(), "build"),
                log_tx,
                started_at: None,
            }
        });
        drop(state);
        self.save();
        id
    }

    pub fn update_app(&self, id: u32, updates: SavedApp) -> Result<(), String> {
        let mut state = self.state.lock().unwrap();
        let app = state.apps.get_mut(&id).ok_or("App not found")?;
        app.entry.name = updates.name;
        app.entry.project_dir = updates.project_dir;
        app.entry.project_type = updates.project_type;
        app.entry.build_steps = updates.build_steps;
        app.entry.run_command = updates.run_command;
        app.entry.static_dir = updates.static_dir;
        app.entry.port = updates.port;
        app.entry.env_vars = updates.env_vars;
        app.entry.auto_start = updates.auto_start;
        app.entry.script_file = updates.script_file;
        drop(state);
        self.save();
        Ok(())
    }

    pub fn delete_app(&self, id: u32) -> Result<(), String> {
        let mut state = self.state.lock().unwrap();
        if let Some(mut app) = state.apps.remove(&id) {
            stop_runtime(&mut app);
        } else {
            return Err("App not found".into());
        }
        drop(state);
        self.save();
        Ok(())
    }

    pub async fn start_app(self: &Arc<Self>, id: u32, skip_build: bool) -> Result<(), String> {
        let entry = {
            let mut state = self.state.lock().unwrap();
            let app = state.apps.get_mut(&id).ok_or("App not found")?;
            if app.status == "running" {
                return Err("Already running".into());
            }
            app.status = "building".into();
            app.build_logs.clear();
            app.logs.clear();
            // Reset the cancel flag so a prior Stop during a failed start
            // doesn't immediately abort this fresh attempt.
            app.cancel_requested.store(false, Ordering::SeqCst);
            app.entry.clone()
        };

        if !skip_build {
            if let Err(e) = self.run_build(&entry).await {
                let mut state = self.state.lock().unwrap();
                if let Some(app) = state.apps.get_mut(&id) {
                    app.status = "stopped".into();
                }
                return Err(format!("Build failed: {}", e));
            }
        }

        // If `start_process` hits any early-return error path (spawn failure,
        // script not found, command not on PATH on macOS/Linux, etc.) the app
        // is still marked "building" from a few lines up. Make sure that on
        // ANY failure we reset the visible status back to "stopped" so the
        // dashboard doesn't get stuck showing "Starting…" forever.
        match self.start_process(id, &entry).await {
            Ok(()) => Ok(()),
            Err(e) => {
                let mut state = self.state.lock().unwrap();
                if let Some(app) = state.apps.get_mut(&id) {
                    if app.status != "running" {
                        app.status = "stopped".into();
                        app.pid = None;
                        app.started_at = None;
                    }
                }
                // Surface the failure in the app's run-log so the user can
                // actually see why it didn't start (otherwise the log modal
                // is empty and the only signal is the error toast).
                self.append_log(id, &format!("Start failed: {}", e));
                Err(e)
            }
        }
    }

    async fn run_build(self: &Arc<Self>, entry: &SavedApp) -> Result<(), String> {
        if entry.build_steps.is_empty() {
            return Ok(());
        }
        let cwd = &entry.project_dir;
        let env = build_env(entry);
        let (build_logs, cancel) = {
            let state = self.state.lock().unwrap();
            let app = state.apps.get(&entry.id).ok_or("App not found")?;
            (app.build_logs.clone(), app.cancel_requested.clone())
        };

        for step in &entry.build_steps {
            // Honour any pending Stop *before* we spawn the next step so we
            // don't start fresh children after the user asked to cancel.
            if cancel.load(Ordering::SeqCst) {
                return Err("Cancelled by user".into());
            }

            build_logs.push(stamped(&format!("▶ Running: {}", step)));
            self.append_log(entry.id, &format!("[BUILD] {}", step));

            let mut cmd = if cfg!(windows) {
                let mut c = tokio::process::Command::new("cmd");
                c.args(["/c", step]);
                c
            } else {
                let mut c = tokio::process::Command::new("sh");
                c.args(["-c", step]);
                c
            };
            cmd.current_dir(cwd);
            cmd.envs(&env);
            cmd.stdout(std::process::Stdio::piped());
            cmd.stderr(std::process::Stdio::piped());
            #[cfg(windows)]
            { cmd.creation_flags(0x08000000); } // CREATE_NO_WINDOW
            #[cfg(unix)]
            apply_unix_session(&mut cmd);

            let mut child = cmd.spawn().map_err(|e| e.to_string())?;
            let build_pid = child.id().unwrap_or(0);

            // Record the PID so `stop_app` can kill the running step's
            // process-tree when a Stop click comes in mid-build.
            if build_pid != 0 {
                let mut state = self.state.lock().unwrap();
                if let Some(app) = state.apps.get_mut(&entry.id) {
                    app.build_pid = Some(build_pid);
                }
            }

            if let Some(stdout) = child.stdout.take() {
                let bl = build_logs.clone();
                let mgr = Arc::clone(self);
                let app_id = entry.id;
                tokio::spawn(async move {
                    let mut reader = tokio::io::BufReader::new(stdout);
                    let mut line = String::new();
                    while reader.read_line(&mut line).await.unwrap_or(0) > 0 {
                        mgr.append_log(app_id, &line);
                        bl.push(stamped(&line));
                        line.clear();
                    }
                });
            }
            if let Some(stderr) = child.stderr.take() {
                let bl = build_logs.clone();
                let mgr = Arc::clone(self);
                let app_id = entry.id;
                tokio::spawn(async move {
                    let mut reader = tokio::io::BufReader::new(stderr);
                    let mut line = String::new();
                    while reader.read_line(&mut line).await.unwrap_or(0) > 0 {
                        mgr.append_log(app_id, &format!("[ERR] {}", &line));
                        bl.push(stamped(&line));
                        line.clear();
                    }
                });
            }

            // Race the step's completion against the cancel flag. We poll the
            // flag on a short interval instead of plumbing a full Notify
            // through the whole manager — keeps this change self-contained.
            let wait_result = loop {
                tokio::select! {
                    biased;
                    res = child.wait() => break res,
                    _ = tokio::time::sleep(std::time::Duration::from_millis(150)) => {
                        if cancel.load(Ordering::SeqCst) {
                            // `stop_app` already called kill_tree on the
                            // build_pid, which triggers our process group.
                            // Await the exit so we don't leak a zombie, then
                            // surface the cancellation.
                            let _ = child.wait().await;
                            let mut state = self.state.lock().unwrap();
                            if let Some(app) = state.apps.get_mut(&entry.id) {
                                app.build_pid = None;
                            }
                            return Err("Cancelled by user".into());
                        }
                    }
                }
            };

            // Step finished on its own — clear the tracked PID.
            {
                let mut state = self.state.lock().unwrap();
                if let Some(app) = state.apps.get_mut(&entry.id) {
                    app.build_pid = None;
                }
            }

            let status = wait_result.map_err(|e| e.to_string())?;
            if !status.success() {
                return Err(format!("Step failed (exit {}): {}", status, step));
            }
        }
        Ok(())
    }

    async fn start_process(self: &Arc<Self>, id: u32, entry: &SavedApp) -> Result<(), String> {
        let logs = {
            let state = self.state.lock().unwrap();
            state.apps.get(&id).map(|a| a.logs.clone())
        }.ok_or("App not found")?;

        let cwd = &entry.project_dir;

        // Static serving mode
        if let Some(ref static_dir) = entry.static_dir {
            let abs_static = Path::new(cwd).join(static_dir);
            if !abs_static.exists() {
                let mut state = self.state.lock().unwrap();
                if let Some(app) = state.apps.get_mut(&id) {
                    app.status = "stopped".into();
                }
                return Err(format!("Static dir not found: {}", abs_static.display()));
            }

            let port = entry.port.unwrap_or(3000);
            let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
            let abs_str = abs_static.to_string_lossy().to_string();
            let logs_c = logs.clone();
            let mgr_c = Arc::clone(self);
            let app_id = id;

            self.rt_handle.spawn(async move {
                use tower_http::services::{ServeDir, ServeFile};
                let index = PathBuf::from(&abs_str).join("index.html");
                let serve = ServeDir::new(&abs_str)
                    .append_index_html_on_directories(true)
                    .fallback(ServeFile::new(index));
                let app = axum::Router::new().fallback_service(serve);

                let start_msg = format!(
                    "Static server at http://localhost:{}  Serving: {}", port, abs_str
                );
                mgr_c.append_log(app_id, &start_msg);
                logs_c.push(stamped(&start_msg));

                if let Ok(listener) = tokio::net::TcpListener::bind(format!("0.0.0.0:{}", port)).await {
                    axum::serve(listener, app)
                        .with_graceful_shutdown(async { let _ = shutdown_rx.await; })
                        .await.ok();
                } else {
                    let fail_msg = format!("Failed to bind port {}", port);
                    mgr_c.append_log(app_id, &fail_msg);
                    logs_c.push(stamped(&fail_msg));
                }
            });

            let mut state = self.state.lock().unwrap();
            if let Some(app) = state.apps.get_mut(&id) {
                app.status = "running".into();
                app.static_shutdown = Some(shutdown_tx);
                app.pid = None;
                app.started_at = Some(now_secs());
            }
            self.log_server(&format!("Started static: {} on port {}", entry.name, port));
            return Ok(());
        }

        // Script mode
        if let Some(ref script) = entry.script_file {
            let abs_script = if Path::new(script).is_absolute() {
                PathBuf::from(script)
            } else {
                Path::new(cwd).join(script)
            };
            if !abs_script.exists() {
                let mut state = self.state.lock().unwrap();
                if let Some(app) = state.apps.get_mut(&id) {
                    app.status = "stopped".into();
                }
                return Err(format!("Script not found: {}", abs_script.display()));
            }
            let ext = abs_script.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
            let script_str = abs_script.to_string_lossy().to_string();
            let env = build_env(entry);

            let mut cmd = match ext.as_str() {
                "ps1" => {
                    let mut c = tokio::process::Command::new("powershell");
                    c.args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File", &script_str]);
                    c
                }
                "bat" | "cmd" => {
                    let mut c = tokio::process::Command::new("cmd");
                    c.args(["/c", &script_str]);
                    c
                }
                "sh" | "bash" => {
                    let mut c = tokio::process::Command::new("sh");
                    c.args(["-c", &script_str]);
                    c
                }
                _ => {
                    // Default: run via OS shell
                    if cfg!(windows) {
                        let mut c = tokio::process::Command::new("cmd");
                        c.args(["/c", &script_str]);
                        c
                    } else {
                        let mut c = tokio::process::Command::new("sh");
                        c.args(["-c", &script_str]);
                        c
                    }
                }
            };
            cmd.current_dir(cwd);
            cmd.envs(&env);
            cmd.stdout(std::process::Stdio::piped());
            cmd.stderr(std::process::Stdio::piped());
            #[cfg(windows)]
            { cmd.creation_flags(0x08000200); } // CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP
            #[cfg(unix)]
            apply_unix_session(&mut cmd);

            let mut child = cmd.spawn().map_err(|e| e.to_string())?;
            let pid = child.id().unwrap_or(0);

            if let Some(stdout) = child.stdout.take() {
                let l = logs.clone();
                let mgr = Arc::clone(self);
                tokio::spawn(async move {
                    let mut reader = tokio::io::BufReader::new(stdout);
                    let mut line = String::new();
                    while reader.read_line(&mut line).await.unwrap_or(0) > 0 {
                        mgr.append_log(id, &line);
                        l.push(stamped(&line));
                        line.clear();
                    }
                });
            }
            if let Some(stderr) = child.stderr.take() {
                let l = logs.clone();
                let mgr = Arc::clone(self);
                tokio::spawn(async move {
                    let mut reader = tokio::io::BufReader::new(stderr);
                    let mut line = String::new();
                    while reader.read_line(&mut line).await.unwrap_or(0) > 0 {
                        mgr.append_log(id, &format!("[ERR] {}", &line));
                        l.push(stamped(&line));
                        line.clear();
                    }
                });
            }

            let mgr = Arc::clone(self);
            let logs_exit = logs.clone();
            let app_name = entry.name.clone();
            tokio::spawn(async move {
                let result = child.wait().await;
                let msg = match result {
                    Ok(s) => format!("Script exited with code {}", s),
                    Err(e) => format!("Script error: {}", e),
                };
                mgr.append_log(id, &msg);
                mgr.log_server(&format!("{} (id={}) script exited", app_name, id));
                logs_exit.push(stamped(&msg));
                let mut state = mgr.state.lock().unwrap();
                if let Some(app) = state.apps.get_mut(&id) {
                    app.status = "stopped".into();
                    app.pid = None;
                    app.started_at = None;
                }
            });

            let mut state = self.state.lock().unwrap();
            if let Some(app) = state.apps.get_mut(&id) {
                app.status = "running".into();
                app.pid = Some(pid);
                app.started_at = Some(now_secs());
            }
            self.log_server(&format!("Started script: {} (id={}, pid={})", entry.name, id, pid));
            return Ok(());
        }

        // Command mode
        let cmd_str = entry.run_command.as_deref().ok_or("No run command or script specified")?;
        let env = build_env(entry);

        let mut cmd = if cfg!(windows) {
            let mut c = tokio::process::Command::new("cmd");
            c.args(["/c", cmd_str]);
            c
        } else {
            let mut c = tokio::process::Command::new("sh");
            c.args(["-c", cmd_str]);
            c
        };
        cmd.current_dir(cwd);
        cmd.envs(&env);
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());
        #[cfg(windows)]
        { cmd.creation_flags(0x08000200); } // CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP
        #[cfg(unix)]
        apply_unix_session(&mut cmd);

        let mut child = cmd.spawn().map_err(|e| e.to_string())?;
        let pid = child.id().unwrap_or(0);

        if let Some(stdout) = child.stdout.take() {
            let l = logs.clone();
            let mgr = Arc::clone(self);
            tokio::spawn(async move {
                let mut reader = tokio::io::BufReader::new(stdout);
                let mut line = String::new();
                while reader.read_line(&mut line).await.unwrap_or(0) > 0 {
                    mgr.append_log(id, &line);
                    l.push(stamped(&line));
                    line.clear();
                }
            });
        }

        if let Some(stderr) = child.stderr.take() {
            let l = logs.clone();
            let mgr = Arc::clone(self);
            tokio::spawn(async move {
                let mut reader = tokio::io::BufReader::new(stderr);
                let mut line = String::new();
                while reader.read_line(&mut line).await.unwrap_or(0) > 0 {
                    mgr.append_log(id, &format!("[ERR] {}", &line));
                    l.push(stamped(&line));
                    line.clear();
                }
            });
        }

        let mgr = Arc::clone(self);
        let logs_exit = logs.clone();
        let app_name = entry.name.clone();
        tokio::spawn(async move {
            let result = child.wait().await;
            let msg = match result {
                Ok(s) => format!("Process exited with code {}", s),
                Err(e) => format!("Process error: {}", e),
            };
            mgr.append_log(id, &msg);
            mgr.log_server(&format!("{} (id={}) exited", app_name, id));
            logs_exit.push(stamped(&msg));
            let mut state = mgr.state.lock().unwrap();
            if let Some(app) = state.apps.get_mut(&id) {
                app.status = "stopped".into();
                app.pid = None;
                app.started_at = None;
            }
        });

        let mut state = self.state.lock().unwrap();
        if let Some(app) = state.apps.get_mut(&id) {
            app.status = "running".into();
            app.pid = Some(pid);
            app.started_at = Some(now_secs());
        }
        self.log_server(&format!("Started: {} (id={}, pid={})", entry.name, id, pid));
        Ok(())
    }

    pub fn stop_app(&self, id: u32) -> Result<(), String> {
        let mut state = self.state.lock().unwrap();
        let app = state.apps.get_mut(&id).ok_or("App not found")?;
        // Allow stopping in both "running" AND "building" — a Stop click
        // during a long build (e.g. npm install) should cancel that build
        // and kill any children it spawned, not silently error out.
        if app.status != "running" && app.status != "building" {
            return Err("Not running".into());
        }
        // Signal the build loop (if any) to bail out at the next checkpoint.
        app.cancel_requested.store(true, Ordering::SeqCst);
        stop_runtime(app);
        Ok(())
    }

    pub async fn restart_app(self: &Arc<Self>, id: u32, skip_build: bool) -> Result<(), String> {
        {
            let mut state = self.state.lock().unwrap();
            if let Some(app) = state.apps.get_mut(&id) {
                stop_runtime(app);
            }
        }
        self.start_app(id, skip_build).await
    }

    pub fn get_logs(&self, id: u32) -> Result<(String, String), String> {
        let state = self.state.lock().unwrap();
        let app = state.apps.get(&id).ok_or("App not found")?;
        Ok((app.logs.snapshot(), app.build_logs.snapshot()))
    }

    /// Subscribe to real-time log events for an app.
    /// Returns a snapshot of the current logs plus a receiver for future lines.
    pub fn subscribe_logs(&self, id: u32) -> Result<(String, String, tokio::sync::broadcast::Receiver<LogLine>), String> {
        let state = self.state.lock().unwrap();
        let app = state.apps.get(&id).ok_or("App not found")?;
        Ok((app.logs.snapshot(), app.build_logs.snapshot(), app.log_tx.subscribe()))
    }

    pub async fn start_all(self: &Arc<Self>) {
        let ids: Vec<u32> = {
            let state = self.state.lock().unwrap();
            state.apps.values().filter(|a| a.status == "stopped").map(|a| a.entry.id).collect()
        };
        for id in ids { let _ = self.start_app(id, false).await; }
    }

    pub async fn auto_start_all(self: &Arc<Self>) {
        let ids: Vec<u32> = {
            let state = self.state.lock().unwrap();
            state.apps.values().filter(|a| a.entry.auto_start).map(|a| a.entry.id).collect()
        };
        for id in ids { let _ = self.start_app(id, false).await; }
    }

    pub fn stop_all(&self) {
        let mut state = self.state.lock().unwrap();
        for app in state.apps.values_mut() {
            stop_runtime(app);
        }
    }

    // ── File-based Logs ─────────────────────────────────────────

    fn app_log_name(&self, id: u32) -> String {
        let state = self.state.lock().unwrap();
        state.apps.get(&id)
            .map(|a| a.entry.name.replace(' ', "_"))
            .unwrap_or_else(|| format!("app-{}", id))
    }

    fn log_file_path_for(&self, name: &str) -> PathBuf {
        self.logs_dir.join(format!("{}.log", name))
    }

    pub fn append_log(&self, id: u32, line: &str) {
        use std::io::Write;
        let name = self.app_log_name(id);
        let path = self.log_file_path_for(&name);
        // On Windows, concurrent open-for-append from many tasks can
        // intermittently fail with a sharing violation. Retry a few times
        // before dropping the line so persisted logs don't lose entries.
        for attempt in 0..5u32 {
            match fs::OpenOptions::new().create(true).append(true).open(&path) {
                Ok(mut f) => {
                    let _ = writeln!(f, "[{}] {}", local_timestamp(), line.trim_end());
                    let _ = f.flush();
                    return;
                }
                Err(_) if attempt < 4 => {
                    std::thread::sleep(std::time::Duration::from_millis(2 << attempt));
                }
                Err(_) => return,
            }
        }
    }

    pub fn get_app_log(&self, id: u32) -> Result<String, String> {
        let state = self.state.lock().unwrap();
        let name = state.apps.get(&id)
            .map(|a| a.entry.name.replace(' ', "_"))
            .ok_or("App not found")?;
        drop(state);
        let path = self.log_file_path_for(&name);
        Ok(if path.exists() { tail_file(&path, 512 * 1024) } else { String::new() })
    }

    pub fn get_server_log(&self) -> String {
        let path = self.logs_dir.join("server.log");
        if path.exists() { tail_file(&path, 256 * 1024) } else { String::new() }
    }

    pub fn log_server(&self, line: &str) {
        use std::io::Write;
        if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(self.logs_dir.join("server.log")) {
            let _ = writeln!(f, "[{}] {}", local_timestamp(), line.trim_end());
        }
    }
}

// ─── Helpers ────────────────────────────────────────────────────────

/// Read at most the last `max_bytes` of a file, aligned to a line boundary.
fn tail_file(path: &Path, max_bytes: u64) -> String {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut f) = fs::File::open(path) else { return String::new() };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    if len > max_bytes {
        let _ = f.seek(SeekFrom::Start(len - max_bytes));
        let mut buf = String::with_capacity(max_bytes as usize);
        let _ = f.read_to_string(&mut buf);
        // Skip partial first line
        if let Some(pos) = buf.find('\n') {
            buf.drain(..=pos);
        }
        buf
    } else {
        let mut buf = String::with_capacity(len as usize);
        let _ = f.read_to_string(&mut buf);
        buf
    }
}

fn local_timestamp() -> String {
    #[cfg(windows)]
    {
        use std::mem::zeroed;
        #[repr(C)]
        struct ST { y: u16, m: u16, _dow: u16, d: u16, h: u16, min: u16, s: u16, _ms: u16 }
        extern "system" { fn GetLocalTime(st: *mut ST); }
        unsafe {
            let mut st: ST = zeroed();
            GetLocalTime(&mut st);
            format!("{:04}-{:02}-{:02} {:02}:{:02}:{:02}", st.y, st.m, st.d, st.h, st.min, st.s)
        }
    }
    #[cfg(not(windows))]
    {
        use std::time::SystemTime;
        // POSIX `struct tm` (fields we need). `localtime_r` fills it in the
        // process's local timezone. The trailing fields (gmtoff, zone) differ
        // across platforms but we only read the leading ones, so this is safe
        // as long as we pass a fully-zeroed buffer of the right size.
        #[repr(C)]
        struct Tm {
            sec: i32,
            min: i32,
            hour: i32,
            mday: i32,
            mon: i32,
            year: i32,
            wday: i32,
            yday: i32,
            isdst: i32,
            // Padding for gmtoff + zone on glibc/musl/macOS (largest we need).
            _pad: [u8; 32],
        }
        extern "C" {
            fn localtime_r(timep: *const i64, result: *mut Tm) -> *mut Tm;
        }

        let secs = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let mut tm = Tm {
            sec: 0, min: 0, hour: 0, mday: 0, mon: 0, year: 0,
            wday: 0, yday: 0, isdst: 0, _pad: [0; 32],
        };
        let ok = unsafe { !localtime_r(&secs, &mut tm).is_null() };
        if ok {
            format!(
                "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
                tm.year + 1900,
                tm.mon + 1,
                tm.mday,
                tm.hour,
                tm.min,
                tm.sec,
            )
        } else {
            // Fallback: epoch seconds if libc call somehow failed.
            format!("{}", secs)
        }
    }
}

fn stamped(line: &str) -> String {
    format!("[{}] {}", local_timestamp(), line.trim_end())
}

fn npm_global_prefix() -> &'static str {
    use std::sync::OnceLock;
    static PREFIX: OnceLock<String> = OnceLock::new();
    PREFIX.get_or_init(|| {
        #[cfg(windows)]
        {
            std::process::Command::new("cmd")
                .args(["/c", "npm prefix -g"])
                .creation_flags(0x08000000)
                .output()
                .ok()
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                .unwrap_or_default()
        }
        #[cfg(not(windows))]
        {
            std::process::Command::new("sh")
                .args(["-c", "npm prefix -g"])
                .output()
                .ok()
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                .unwrap_or_default()
        }
    })
}

fn build_env(entry: &SavedApp) -> HashMap<String, String> {
    let mut env: HashMap<String, String> = std::env::vars().collect();
    env.extend(entry.env_vars.iter().map(|(k, v)| (k.clone(), v.clone())));

    // Prepend node_modules/.bin to PATH so local tools (tsc, vite, etc.) are found
    // Also include npm global prefix for globally installed tools
    let nm_bin = Path::new(&entry.project_dir).join("node_modules").join(".bin");
    let mut extra_paths = vec![nm_bin.to_string_lossy().to_string()];

    // Add npm global bin (where global packages like tsc might be)
    let global_prefix = npm_global_prefix();
    if !global_prefix.is_empty() {
        let prefix_path = Path::new(global_prefix);
        extra_paths.push(prefix_path.to_string_lossy().to_string());
        extra_paths.push(prefix_path.join("node_modules").join(".bin").to_string_lossy().to_string());
    }

    // Find all PATH-like keys (Windows is case-insensitive but HashMap isn't)
    let path_key = env.keys()
        .find(|k| k.eq_ignore_ascii_case("PATH"))
        .cloned()
        .unwrap_or_else(|| "PATH".to_string());
    let existing = env.get(&path_key).cloned().unwrap_or_default();
    let sep = if cfg!(windows) { ";" } else { ":" };
    let new_path = format!("{}{}{}", extra_paths.join(sep), sep, existing);
    env.insert(path_key, new_path);

    if let Some(port) = entry.port {
        let t = entry.project_type.to_lowercase();
        if t == "dotnet" {
            env.entry("ASPNETCORE_URLS".into()).or_insert_with(|| format!("http://localhost:{}", port));
        } else {
            env.entry("PORT".into()).or_insert_with(|| port.to_string());
        }
    }
    env
}

fn stop_runtime(app: &mut AppRuntime) {
    // Kill the run process (if running) AND the in-flight build step (if
    // building). They're distinct PIDs; a Stop click while "Starting…"
    // needs to tear down whichever one is currently alive.
    if let Some(pid) = app.pid.take() {
        kill_tree(pid);
    }
    if let Some(pid) = app.build_pid.take() {
        kill_tree(pid);
    }
    if let Some(tx) = app.static_shutdown.take() {
        let _ = tx.send(());
    }
    app.status = "stopped".into();
    app.started_at = None;
}

/// Resolve the per-user directory where AppNest should store its config and logs.
/// Windows: %APPDATA% (e.g. C:\Users\<you>\AppData\Roaming)
/// macOS:   ~/Library/Application Support
/// Linux:   $XDG_DATA_HOME or ~/.local/share
fn default_data_root() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        std::env::var_os("APPDATA").map(PathBuf::from)
    }
    #[cfg(target_os = "macos")]
    {
        std::env::var_os("HOME")
            .map(|h| PathBuf::from(h).join("Library").join("Application Support"))
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        if let Some(xdg) = std::env::var_os("XDG_DATA_HOME") {
            Some(PathBuf::from(xdg))
        } else {
            std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local").join("share"))
        }
    }
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn kill_tree(pid: u32) {
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("taskkill")
            .args(["/T", "/F", "/PID", &pid.to_string()])
            .creation_flags(0x08000000)
            .output();
    }
    #[cfg(not(windows))]
    {
        // Our spawned children are session leaders (see `apply_unix_session`),
        // so their PID == their PGID. We send SIGTERM to the whole group so
        // grandchildren (e.g. `npm → node`, `dotnet → app`) die too, not just
        // the top-level shell. A short grace period, then SIGKILL anything
        // still running so nothing is left orphaned when the user stops an app.
        let pgid = format!("-{}", pid);
        let _ = std::process::Command::new("kill")
            .args(["-TERM", "--", &pgid])
            .output();
        std::thread::sleep(std::time::Duration::from_millis(300));
        let _ = std::process::Command::new("kill")
            .args(["-KILL", "--", &pgid])
            .output();
    }
}

/// On Unix, put the child into its own session so it becomes a process-group
/// leader (PGID == PID). That lets `kill_tree` SIGTERM/SIGKILL the whole tree
/// via `kill -- -<pid>`. Without this, a `sh -c "npm start"` inherits our PGID
/// and grandchildren (node, dotnet, etc.) are orphaned when we try to stop it.
#[cfg(unix)]
fn apply_unix_session(cmd: &mut tokio::process::Command) {
    use std::os::unix::process::CommandExt;
    extern "C" {
        fn setsid() -> i32;
    }
    unsafe {
        cmd.pre_exec(|| {
            // Best-effort: if we're already a session leader (very rare for
            // child processes) setsid returns -1 with EPERM — ignore it.
            let _ = setsid();
            Ok(())
        });
    }
}
