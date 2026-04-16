use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
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
}

// ─── Runtime State ──────────────────────────────────────────────────

const LOG_CAP: usize = 2000;

struct AppRuntime {
    entry: SavedApp,
    status: String,
    pid: Option<u32>,
    static_shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    logs: Arc<Mutex<VecDeque<String>>>,
    build_logs: Arc<Mutex<VecDeque<String>>>,
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
        let app_data = std::env::var("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                std::env::current_exe()
                    .ok()
                    .and_then(|p| p.parent().map(|p| p.to_path_buf()))
                    .unwrap_or_else(|| std::env::current_dir().unwrap())
            });

        let base_dir = app_data.join("MyServers");
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
                    state.apps.insert(a.id, AppRuntime {
                        entry: a,
                        status: "stopped".into(),
                        pid: None,
                        static_shutdown: None,
                        logs: Arc::new(Mutex::new(VecDeque::with_capacity(LOG_CAP))),
                        build_logs: Arc::new(Mutex::new(VecDeque::with_capacity(LOG_CAP))),
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
        state.apps.values().map(|a| AppResponse {
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
        }).collect()
    }

    pub fn add_app(&self, app: SavedApp) -> u32 {
        let mut state = self.state.lock().unwrap();
        let id = state.next_id;
        state.next_id += 1;
        let mut entry = app;
        entry.id = id;
        state.apps.insert(id, AppRuntime {
            entry,
            status: "stopped".into(),
            pid: None,
            static_shutdown: None,
            logs: Arc::new(Mutex::new(VecDeque::with_capacity(LOG_CAP))),
            build_logs: Arc::new(Mutex::new(VecDeque::with_capacity(LOG_CAP))),
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
            app.build_logs.lock().unwrap().clear();
            app.logs.lock().unwrap().clear();
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

        self.start_process(id, &entry).await
    }

    async fn run_build(&self, entry: &SavedApp) -> Result<(), String> {
        if entry.build_steps.is_empty() {
            return Ok(());
        }
        let cwd = &entry.project_dir;
        let env = build_env(entry);
        let build_logs = {
            let state = self.state.lock().unwrap();
            state.apps.get(&entry.id).map(|a| a.build_logs.clone())
        };
        let build_logs = build_logs.ok_or("App not found")?;

        for step in &entry.build_steps {
            build_logs.lock().unwrap().push_back(stamped(&format!("▶ Running: {}", step)));
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

            let mut child = cmd.spawn().map_err(|e| e.to_string())?;

            if let Some(stdout) = child.stdout.take() {
                let bl = build_logs.clone();
                tokio::spawn(async move {
                    let mut reader = tokio::io::BufReader::new(stdout);
                    let mut line = String::new();
                    while reader.read_line(&mut line).await.unwrap_or(0) > 0 {
                        let mut v = bl.lock().unwrap();
                        v.push_back(stamped(&line));
                        if v.len() > LOG_CAP { v.pop_front(); }
                        line.clear();
                    }
                });
            }
            if let Some(stderr) = child.stderr.take() {
                let bl = build_logs.clone();
                tokio::spawn(async move {
                    let mut reader = tokio::io::BufReader::new(stderr);
                    let mut line = String::new();
                    while reader.read_line(&mut line).await.unwrap_or(0) > 0 {
                        let mut v = bl.lock().unwrap();
                        v.push_back(stamped(&line));
                        if v.len() > LOG_CAP { v.pop_front(); }
                        line.clear();
                    }
                });
            }

            let status = child.wait().await.map_err(|e| e.to_string())?;
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

            self.rt_handle.spawn(async move {
                use tower_http::services::{ServeDir, ServeFile};
                let index = PathBuf::from(&abs_str).join("index.html");
                let serve = ServeDir::new(&abs_str)
                    .append_index_html_on_directories(true)
                    .fallback(ServeFile::new(index));
                let app = axum::Router::new().fallback_service(serve);

                logs_c.lock().unwrap().push_back(stamped(&format!(
                    "Static server at http://localhost:{}  Serving: {}", port, abs_str
                )));

                if let Ok(listener) = tokio::net::TcpListener::bind(format!("0.0.0.0:{}", port)).await {
                    axum::serve(listener, app)
                        .with_graceful_shutdown(async { let _ = shutdown_rx.await; })
                        .await.ok();
                } else {
                    logs_c.lock().unwrap().push_back(stamped(&format!("Failed to bind port {}", port)));
                }
            });

            let mut state = self.state.lock().unwrap();
            if let Some(app) = state.apps.get_mut(&id) {
                app.status = "running".into();
                app.static_shutdown = Some(shutdown_tx);
                app.pid = None;
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
                        let mut v = l.lock().unwrap();
                        v.push_back(stamped(&line));
                        if v.len() > LOG_CAP { v.pop_front(); }
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
                        let mut v = l.lock().unwrap();
                        v.push_back(stamped(&line));
                        if v.len() > LOG_CAP { v.pop_front(); }
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
                logs_exit.lock().unwrap().push_back(stamped(&msg));
                let mut state = mgr.state.lock().unwrap();
                if let Some(app) = state.apps.get_mut(&id) {
                    app.status = "stopped".into();
                    app.pid = None;
                }
            });

            let mut state = self.state.lock().unwrap();
            if let Some(app) = state.apps.get_mut(&id) {
                app.status = "running".into();
                app.pid = Some(pid);
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
                    let mut v = l.lock().unwrap();
                    v.push_back(stamped(&line));
                    if v.len() > LOG_CAP { v.pop_front(); }
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
                    let mut v = l.lock().unwrap();
                    v.push_back(stamped(&line));
                    if v.len() > LOG_CAP { v.pop_front(); }
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
            logs_exit.lock().unwrap().push_back(stamped(&msg));
            let mut state = mgr.state.lock().unwrap();
            if let Some(app) = state.apps.get_mut(&id) {
                app.status = "stopped".into();
                app.pid = None;
            }
        });

        let mut state = self.state.lock().unwrap();
        if let Some(app) = state.apps.get_mut(&id) {
            app.status = "running".into();
            app.pid = Some(pid);
        }
        self.log_server(&format!("Started: {} (id={}, pid={})", entry.name, id, pid));
        Ok(())
    }

    pub fn stop_app(&self, id: u32) -> Result<(), String> {
        let mut state = self.state.lock().unwrap();
        let app = state.apps.get_mut(&id).ok_or("App not found")?;
        if app.status != "running" {
            return Err("Not running".into());
        }
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
        let logs = {
            let v = app.logs.lock().unwrap();
            let mut out = String::new();
            for s in v.iter() { out.push_str(s); out.push('\n'); }
            out
        };
        let build_logs = {
            let v = app.build_logs.lock().unwrap();
            let mut out = String::new();
            for s in v.iter() { out.push_str(s); out.push('\n'); }
            out
        };
        Ok((logs, build_logs))
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
        if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(self.log_file_path_for(&name)) {
            let _ = writeln!(f, "[{}] {}", local_timestamp(), line.trim_end());
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
        let s = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap_or_default().as_secs();
        format!("{}", s)
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
        extra_paths.push(global_prefix.to_string());
        extra_paths.push(format!("{}\\node_modules\\.bin", global_prefix));
    }

    // Find all PATH-like keys (Windows is case-insensitive but HashMap isn't)
    let path_key = env.keys()
        .find(|k| k.eq_ignore_ascii_case("PATH"))
        .cloned()
        .unwrap_or_else(|| "PATH".to_string());
    let existing = env.get(&path_key).cloned().unwrap_or_default();
    let new_path = format!("{};{}", extra_paths.join(";"), existing);
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
    if let Some(pid) = app.pid.take() {
        kill_tree(pid);
    }
    if let Some(tx) = app.static_shutdown.take() {
        let _ = tx.send(());
    }
    app.status = "stopped".into();
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
        let _ = std::process::Command::new("kill").args(["-TERM", "--", &format!("-{}", pid)]).output();
    }
}
