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

// ─── Windows Job Object helper ──────────────────────────────────────
//
// On Windows, `child.wait()` only tracks the *direct* child we spawned.
// For shell-based run commands (powershell -> cmd -> npm -> node -> ...),
// the direct child often exits while its descendants keep running, which
// made AppNest mark apps "Stopped" while dev servers were still alive.
//
// Fix: assign each spawned run process to a Job Object with
// JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE. All descendants automatically inherit
// the job, so we can:
//   * use `active_processes()` as the real liveness signal, and
//   * use `terminate()` on Stop to kill the whole tree atomically.
#[cfg(windows)]
mod winjob {
    use std::sync::Arc;
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectBasicAccountingInformation,
        JobObjectExtendedLimitInformation, QueryInformationJobObject, SetInformationJobObject,
        TerminateJobObject, JOBOBJECT_BASIC_ACCOUNTING_INFORMATION,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };

    pub struct Job(HANDLE);

    // HANDLE is a raw pointer; the OS object behind it is safe to share
    // across threads — only one thread closes it (Drop).
    unsafe impl Send for Job {}
    unsafe impl Sync for Job {}

    impl Job {
        pub fn create() -> Option<Arc<Job>> {
            unsafe {
                let h = CreateJobObjectW(std::ptr::null(), std::ptr::null());
                if h == 0 {
                    return None;
                }
                let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
                info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                let _ = SetInformationJobObject(
                    h,
                    JobObjectExtendedLimitInformation,
                    &info as *const _ as *const _,
                    std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                );
                Some(Arc::new(Job(h)))
            }
        }

        pub fn assign(&self, process: HANDLE) -> bool {
            unsafe { AssignProcessToJobObject(self.0, process) != 0 }
        }

        pub fn active_processes(&self) -> u32 {
            unsafe {
                let mut info: JOBOBJECT_BASIC_ACCOUNTING_INFORMATION = std::mem::zeroed();
                let mut returned: u32 = 0;
                if QueryInformationJobObject(
                    self.0,
                    JobObjectBasicAccountingInformation,
                    &mut info as *mut _ as *mut _,
                    std::mem::size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>() as u32,
                    &mut returned,
                ) == 0
                {
                    return 0;
                }
                info.ActiveProcesses
            }
        }

        pub fn terminate(&self) {
            unsafe {
                TerminateJobObject(self.0, 1);
            }
        }
    }

    impl Drop for Job {
        fn drop(&mut self) {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
}

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
    /// Windows Job Object the run process and all its descendants belong to.
    /// Used both for liveness tracking (active_processes) and for atomic
    /// tree-kill on Stop. None on non-Windows or before a run starts.
    #[cfg(windows)]
    job: Option<Arc<winjob::Job>>,
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
    /// Serializes save() calls so concurrent writers can't race on the
    /// apps.json.tmp → apps.json rename and silently lose updates.
    save_lock: Mutex<()>,
    data_file: PathBuf,
    logs_dir: PathBuf,
    rt_handle: Handle,
    /// Set true if `load()` couldn't parse apps.json. While set, `save()`
    /// becomes a no-op so we don't overwrite a config we couldn't read.
    corrupt_load_lock: AtomicBool,
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
            save_lock: Mutex::new(()),
            data_file,
            logs_dir,
            rt_handle,
            corrupt_load_lock: AtomicBool::new(false),
        }
    }

    pub fn load(&self) {
        let dir = self.data_file.parent().unwrap();
        let _ = fs::create_dir_all(dir);
        if !self.data_file.exists() {
            return;
        }
        let content = match fs::read_to_string(&self.data_file) {
            Ok(c) => c,
            Err(e) => {
                self.log_server(&format!("load: failed to read {}: {} — keeping existing state", self.data_file.display(), e));
                self.corrupt_load_lock.store(true, Ordering::SeqCst);
                return;
            }
        };
        let saved: Vec<SavedApp> = match serde_json::from_str(&content) {
            Ok(v) => v,
            Err(e) => {
                // Refuse to clobber a config we can't understand. Move it aside
                // so the user can recover it, mark the manager as "load failed"
                // so subsequent save() calls become no-ops, and surface the
                // problem in the server log instead of silently wiping data.
                let backup = self.data_file.with_extension(format!(
                    "corrupt-{}.json",
                    now_secs()
                ));
                let _ = fs::copy(&self.data_file, &backup);
                self.log_server(&format!(
                    "load: apps.json is corrupt ({}). Backed up to {}. Saves are disabled until file is fixed or removed.",
                    e, backup.display()
                ));
                self.corrupt_load_lock.store(true, Ordering::SeqCst);
                return;
            }
        };
        let mut state = self.state.lock().unwrap();
        let mut seen_ids: std::collections::HashSet<u32> = std::collections::HashSet::new();
        let mut dropped_dupes: Vec<u32> = Vec::new();
        for a in saved {
            // A hand-edited apps.json could contain two rows with the same
            // id. HashMap::insert would silently overwrite — making one of
            // the apps disappear with no signal. Keep the FIRST occurrence
            // (the lower-order entry after our pretty-printed sort), drop
            // the duplicate, and disable saves so we don't persist the
            // truncated set on top of the user's hand-edited file.
            if !seen_ids.insert(a.id) {
                dropped_dupes.push(a.id);
                continue;
            }
            if a.id >= state.next_id {
                state.next_id = a.id.saturating_add(1);
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
                    #[cfg(windows)]
                    job: None,
                }
            });
        }
        if !dropped_dupes.is_empty() {
            // Don't clobber the user's apps.json with our deduped subset.
            self.corrupt_load_lock.store(true, Ordering::SeqCst);
            drop(state);
            self.log_server(&format!(
                "load: apps.json contains duplicate id(s) {:?}. Kept the first occurrence of each. Saves are disabled until the file is fixed.",
                dropped_dupes
            ));
        }
    }

    fn save(&self) {
        // Serialize save() across callers so concurrent writers can't race
        // on the apps.json.tmp → apps.json rename. Without this, two API
        // requests landing simultaneously can both write the tmp file and
        // both attempt the rename, with the second one falling back to
        // direct-write — losing the atomicity guarantee entirely.
        let _save_guard = self.save_lock.lock().unwrap();

        // If load() failed to parse the on-disk file we refuse to overwrite
        // it — otherwise any later add_app/delete_app/reorder would persist a
        // file that wipes out the user's real config (see load()).
        if self.corrupt_load_lock.load(Ordering::SeqCst) {
            return;
        }
        // Snapshot what we need from state under a short borrow, then release
        // the state mutex BEFORE doing disk IO so other API requests aren't
        // blocked behind a slow filesystem.
        let saved_owned: Vec<SavedApp> = {
            let state = self.state.lock().unwrap();
            let mut v: Vec<SavedApp> = state.apps.values().map(|a| a.entry.clone()).collect();
            v.sort_by_key(|e| (e.order, e.id));
            v
        };
        let json = match serde_json::to_string_pretty(&saved_owned) {
            Ok(s) => s,
            Err(e) => {
                self.log_server(&format!("save: serialize failed: {}", e));
                return;
            }
        };
        if let Some(parent) = self.data_file.parent() {
            let _ = fs::create_dir_all(parent);
        }
        // Write atomically: write to a tmp file then rename, so a crash
        // mid-write can never leave a half-written apps.json on disk.
        let tmp = self.data_file.with_extension("json.tmp");
        if let Err(e) = fs::write(&tmp, &json) {
            self.log_server(&format!("save: write tmp failed: {}", e));
            return;
        }
        if let Err(e) = fs::rename(&tmp, &self.data_file) {
            // Some Windows AV tools transiently lock the destination during
            // rename; fall back to a direct write so we don't lose the change.
            let _ = fs::write(&self.data_file, &json);
            self.log_server(&format!("save: rename failed (used direct write): {}", e));
        }
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
        // Require the request to cover EXACTLY the current id set; otherwise
        // we'd renumber a subset and leave the unlisted apps colliding with
        // the new range, producing non-deterministic ordering on the next
        // list call.
        let known: std::collections::HashSet<u32> = state.apps.keys().copied().collect();
        let asked: std::collections::HashSet<u32> = ids.iter().copied().collect();
        if asked.len() != ids.len() {
            return Err("reorder: duplicate ids".into());
        }
        if asked != known {
            return Err("reorder: id set must match the current apps exactly".into());
        }
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
        // saturating_add stops at u32::MAX; if we ever roll back to MAX a
        // second time we'd overwrite the existing app at MAX. Detect that
        // and surface it via server.log instead. (Realistically unreachable
        // — 4 billion adds — but cheap to defend against.)
        let next = state.next_id.saturating_add(1);
        if next == state.next_id {
            // Already saturated. Bail out without inserting; callers see
            // their previous id-of-record, which is wrong, but at least
            // we don't silently corrupt an existing app.
            drop(state);
            self.log_server("add_app: id space exhausted (u32::MAX). Refusing to insert.");
            return 0;
        }
        state.next_id = next;
        let max_order = state.apps.values().map(|a| a.entry.order).max().unwrap_or(0);
        let mut entry = app;
        entry.id = id;
        entry.name = entry.name.trim().to_string();
        entry.project_dir = entry.project_dir.trim().to_string();
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
                #[cfg(windows)]
                job: None,
            }
        });
        drop(state);
        self.save();
        id
    }

    pub fn update_app(&self, id: u32, updates: SavedApp) -> Result<(), String> {
        // Capture the trimmed new name BEFORE we move other fields out of
        // `updates`, so we can compare against the old name for a possible
        // log-file rename below without needing a second state lookup.
        let new_name = updates.name.trim().to_string();
        // Hold the state lock for the WHOLE update including the file
        // rename. Without this, an append_log() from a still-running app
        // could observe the new name (via app_log_name) between us writing
        // it and us renaming the file, create a fresh `<new>.log`, and
        // then our rename would see the destination exists and bail —
        // splitting the history across two files forever. The rename
        // itself is a directory-entry update (microseconds), so holding
        // the lock through it is safe.
        let mut state = self.state.lock().unwrap();
        let app = state.apps.get_mut(&id).ok_or("App not found")?;
        // Snapshot the COMPLETE old entry so we can roll the runtime back
        // atomically if the log-file rename fails. Without this we would
        // half-apply the update (port/env/etc. take effect with the OLD
        // name) and the user would see an error toast despite their
        // changes silently sticking — confusing data corruption.
        let old_entry = app.entry.clone();
        let old_name = old_entry.name.clone();
        app.entry.name = new_name.clone();
        app.entry.project_dir = updates.project_dir.trim().to_string();
        app.entry.project_type = updates.project_type;
        app.entry.build_steps = updates.build_steps;
        app.entry.run_command = updates.run_command;
        app.entry.static_dir = updates.static_dir;
        app.entry.port = updates.port;
        app.entry.env_vars = updates.env_vars;
        app.entry.auto_start = updates.auto_start;
        app.entry.script_file = updates.script_file;
        if old_name != new_name {
            let old_san = sanitize_log_name(&old_name, id);
            let new_san = sanitize_log_name(&new_name, id);
            if old_san != new_san {
                let old_path = self.log_file_path_for(&old_san);
                let new_path = self.log_file_path_for(&new_san);
                if old_path.exists() && !new_path.exists() {
                    if let Err(e) = fs::rename(&old_path, &new_path) {
                        // Rename failed (typically AV-scan lock on Windows).
                        // Roll the COMPLETE in-memory entry back to its
                        // pre-update state so the user's changes don't
                        // half-apply with the old name still attached.
                        // Then save() so apps.json matches in-memory.
                        let msg = format!(
                            "Could not rename log file (kept old name '{}'): {}",
                            old_name, e
                        );
                        if let Some(app) = state.apps.get_mut(&id) {
                            app.entry = old_entry;
                        }
                        drop(state);
                        self.log_server(&format!("update_app: {}", msg));
                        // save() persists the rolled-back state so the
                        // on-disk apps.json stays consistent with what
                        // the dashboard will read on the next list call.
                        self.save();
                        return Err(msg);
                    }
                }
            }
        }
        drop(state);
        self.save();
        Ok(())
    }

    pub fn delete_app(&self, id: u32) -> Result<(), String> {
        // Take the runtime out of the map under the lock, then release the
        // lock BEFORE running stop_runtime — stop_runtime can sleep ~300 ms
        // (Linux SIGTERM grace) and shells out to taskkill on Windows. We
        // don't want every other API request blocked behind it.
        let mut victim = {
            let mut state = self.state.lock().unwrap();
            match state.apps.remove(&id) {
                Some(v) => v,
                None => return Err("App not found".into()),
            }
        };
        stop_runtime(&mut victim);
        self.save();
        Ok(())
    }

    pub async fn start_app(self: &Arc<Self>, id: u32, skip_build: bool) -> Result<(), String> {
        let entry = {
            let mut state = self.state.lock().unwrap();
            let app = state.apps.get_mut(&id).ok_or("App not found")?;
            // Reject any concurrent start while a previous start is still in
            // flight — without this, a second click (or duplicate API call)
            // re-enters this function, clears the logs, overwrites build_pid
            // with a brand new spawn, and orphans the original build process.
            if app.status == "running" {
                return Err("Already running".into());
            }
            if app.status == "building" {
                return Err("Already starting".into());
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
                // Re-check that the app still exists before mutating state
                // OR appending to its log file. A concurrent delete_app
                // could have removed it while start_process was awaiting,
                // and append_log would otherwise fall back to writing to a
                // `app-<id>.log` file the user doesn't know about.
                let still_present = {
                    let mut state = self.state.lock().unwrap();
                    if let Some(app) = state.apps.get_mut(&id) {
                        // start_process only ever returns Err BEFORE flipping
                        // status to "running", so we always reset the
                        // "building" state here. (The previous `!= "running"`
                        // guard was dead code, kept as a defensive comment
                        // to remind future maintainers of this invariant.)
                        app.status = "stopped".into();
                        app.pid = None;
                        app.started_at = None;
                        true
                    } else {
                        false
                    }
                };
                if still_present {
                    // Surface the failure in the app's run-log so the user
                    // can actually see why it didn't start (otherwise the log
                    // modal is empty and the only signal is the error toast).
                    self.append_log(id, &format!("Start failed: {}", e));
                }
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
                            // `stop_app` will normally have called kill_tree on
                            // the build_pid before flipping cancel, but there is
                            // a tiny window between cmd.spawn() and us recording
                            // build_pid where stop_app could observe None. Kill
                            // the direct child here unconditionally so we never
                            // end up awaiting a still-running process forever.
                            let _ = child.start_kill();
                            // Bound the wait so a stuck child can't keep us
                            // pinned forever — 5 s is plenty for a SIGKILL'd
                            // process to be reaped.
                            let _ = tokio::time::timeout(
                                std::time::Duration::from_secs(5),
                                child.wait(),
                            ).await;
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
            let abs_str = abs_static.to_string_lossy().to_string();

            // Bind synchronously so a failed bind (e.g. port in use) is
            // surfaced as a hard error to the caller — not silently logged
            // while the dashboard shows the app as Running with no server.
            //
            // Bind to 127.0.0.1 only. The previous 0.0.0.0 default exposed
            // every static-mode build artifact to the entire LAN, which is
            // a footgun on coffee-shop networks. Apps that genuinely need
            // LAN exposure can use Command-mode and bind their own server.
            let listener = tokio::net::TcpListener::bind(format!("127.0.0.1:{}", port))
                .await
                .map_err(|e| format!("Failed to bind port {}: {}", port, e))?;

            let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
            let logs_c = logs.clone();
            let mgr_c = Arc::clone(self);
            let app_id = id;
            let abs_str_for_task = abs_str.clone();

            self.rt_handle.spawn(async move {
                use tower_http::services::{ServeDir, ServeFile};
                let index = PathBuf::from(&abs_str_for_task).join("index.html");
                let serve = ServeDir::new(&abs_str_for_task)
                    .append_index_html_on_directories(true)
                    .fallback(ServeFile::new(index));
                let app = axum::Router::new().fallback_service(serve);

                let start_msg = format!(
                    "Static server at http://127.0.0.1:{}  Serving: {}", port, abs_str_for_task
                );
                mgr_c.append_log(app_id, &start_msg);
                logs_c.push(stamped(&start_msg));

                // Track whether shutdown was requested explicitly (i.e. via
                // the oneshot sender held in app.static_shutdown). If so,
                // stop_app / delete_app / restart already handled the state
                // transition and we must NOT touch the AppRuntime —
                // otherwise a Stop→Start cycle would race this watcher
                // into clobbering the new run's state with "stopped" (the
                // same shape as the script/cmd owning_pid guard elsewhere
                // in this file).
                let shutdown_was_requested = std::sync::Arc::new(AtomicBool::new(false));
                let shutdown_flag = shutdown_was_requested.clone();
                axum::serve(listener, app)
                    .with_graceful_shutdown(async move {
                        let _ = shutdown_rx.await;
                        shutdown_flag.store(true, Ordering::SeqCst);
                    })
                    .await.ok();

                if shutdown_was_requested.load(Ordering::SeqCst) {
                    // stop_app / delete_app / restart_app already handled
                    // the state change. Don't double-write — we'd race
                    // against any newer start that has already flipped
                    // status back to "running".
                    return;
                }
                // The server died on its own (listener error, panic, etc).
                // Reflect that as stopped, but only if the runtime is still
                // ours — if a fresh start has installed a new shutdown_tx,
                // app.static_shutdown will be Some(...) and we leave it.
                let mut state = mgr_c.state.lock().unwrap();
                if let Some(app) = state.apps.get_mut(&app_id) {
                    if app.static_shutdown.is_none() {
                        app.status = "stopped".into();
                        app.started_at = None;
                    }
                }
            });

            let mut state = self.state.lock().unwrap();
            if let Some(app) = state.apps.get_mut(&id) {
                // If a Stop click landed between us calling bind() and
                // acquiring this lock, stop_app has already set the cancel
                // flag and flipped status to "stopped". Honour it instead
                // of silently overwriting back to "running" — otherwise
                // the user's Stop is lost and we leak a server task that
                // nobody can shut down (its shutdown_tx never gets
                // installed into app.static_shutdown).
                if app.cancel_requested.load(Ordering::SeqCst) {
                    drop(state);
                    // Drop shutdown_tx so the spawned axum task notices
                    // the closed channel and exits via graceful_shutdown.
                    drop(shutdown_tx);
                    return Err("Cancelled by user".into());
                }
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
                    // Prefer pwsh (PowerShell 7+) when available — on newer
                    // Windows installs Windows PowerShell 5.1 (`powershell`)
                    // is no longer guaranteed to be present. Fall back to it
                    // when pwsh isn't on PATH.
                    #[cfg(windows)]
                    let exe = if which_on_path("pwsh").is_some() { "pwsh" } else { "powershell" };
                    #[cfg(not(windows))]
                    let exe = "pwsh";
                    let mut c = tokio::process::Command::new(exe);
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

            // On Windows, put the script process into a Job Object so its
            // entire descendant tree (cmd -> npm -> node -> ...) becomes
            // observable as a single unit. Liveness comes from the job's
            // active-process count, not from the direct child's exit code.
            #[cfg(windows)]
            let job = {
                let j = winjob::Job::create();
                if let Some(jref) = &j {
                    let h = child.raw_handle();
                    if let Some(h) = h {
                        jref.assign(h as windows_sys::Win32::Foundation::HANDLE);
                    }
                }
                j
            };

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
            #[cfg(windows)]
            let job_for_exit = job.clone();

            // Flip status to "running" BEFORE spawning the exit-watcher,
            // otherwise a fast-exiting child can race the watcher's
            // "stopped" write ahead of this block and leave the dashboard
            // stuck on "running" with a dead PID forever.
            //
            // If a Stop click landed during the spawn window, honour it:
            // tear down the just-spawned child instead of overwriting back
            // to "running". stop_app already set status to "stopped" so we
            // just leave it alone after the kill.
            let cancelled = {
                let mut state = self.state.lock().unwrap();
                let mut cancelled = false;
                if let Some(app) = state.apps.get_mut(&id) {
                    if app.cancel_requested.load(Ordering::SeqCst) {
                        cancelled = true;
                    } else {
                        app.status = "running".into();
                        app.pid = Some(pid);
                        app.started_at = Some(now_secs());
                        #[cfg(windows)]
                        { app.job = job.clone(); }
                    }
                }
                cancelled
            };
            if cancelled {
                #[cfg(windows)]
                if let Some(j) = &job { j.terminate(); }
                kill_tree_batch(&[pid]);
                // Reap so we don't leak a zombie. Bound the wait so an
                // unkillable child can't pin this task forever.
                let _ = tokio::time::timeout(
                    std::time::Duration::from_secs(5),
                    child.wait(),
                ).await;
                return Err("Cancelled by user".into());
            }

            // Capture the pid that THIS watcher is responsible for. After a
            // Stop → Start cycle, the previous run's watcher might still be
            // awaiting child.wait() when the new run flips status to
            // "running" with a fresh pid. Without this guard the stale
            // watcher would overwrite the new run's state with "stopped".
            let owning_pid = pid;
            tokio::spawn(async move {
                let result = child.wait().await;
                // On Windows the direct child can exit while descendants
                // (node, webpack-dev-server, ...) are still alive in the
                // job. Wait until the job is empty before flipping status.
                #[cfg(windows)]
                {
                    if let Some(j) = &job_for_exit {
                        while j.active_processes() > 0 {
                            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                        }
                    }
                }
                let msg = match result {
                    Ok(s) => format!("Script exited with code {}", s),
                    Err(e) => format!("Script error: {}", e),
                };
                // Skip all logging + state mutation if the app was deleted
                // while we were awaiting wait(). Without this we'd write
                // an orphan exit message to a fallback `app-<id>.log` for
                // an app the user just removed.
                let still_present = mgr.state.lock().unwrap().apps.contains_key(&id);
                if !still_present {
                    return;
                }
                mgr.append_log(id, &msg);
                mgr.log_server(&format!("{} (id={}) script exited", app_name, id));
                logs_exit.push(stamped(&msg));
                let mut state = mgr.state.lock().unwrap();
                if let Some(app) = state.apps.get_mut(&id) {
                    // Only flip to stopped if WE are still the active run.
                    // A user could have stopped + restarted while we were
                    // awaiting wait(); in that case app.pid is now the
                    // newer process and we must not touch it.
                    if app.pid == Some(owning_pid) {
                        app.status = "stopped".into();
                        app.pid = None;
                        app.started_at = None;
                        #[cfg(windows)]
                        { app.job = None; }
                    }
                }
            });

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

        // See script-mode comment above — same Job Object treatment for
        // command-mode runs so descendants are tracked atomically.
        #[cfg(windows)]
        let job = {
            let j = winjob::Job::create();
            if let Some(jref) = &j {
                let h = child.raw_handle();
                if let Some(h) = h {
                    jref.assign(h as windows_sys::Win32::Foundation::HANDLE);
                }
            }
            j
        };

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
        #[cfg(windows)]
        let job_for_exit = job.clone();

        // Same race fix as script mode: set "running" first, THEN spawn the
        // watcher, so the watcher's "stopped" write always lands after.
        // Same cancel-window check as script mode — if Stop landed between
        // cmd.spawn() and us acquiring the lock, kill the just-spawned
        // child instead of silently overwriting back to "running".
        let cancelled = {
            let mut state = self.state.lock().unwrap();
            let mut cancelled = false;
            if let Some(app) = state.apps.get_mut(&id) {
                if app.cancel_requested.load(Ordering::SeqCst) {
                    cancelled = true;
                } else {
                    app.status = "running".into();
                    app.pid = Some(pid);
                    app.started_at = Some(now_secs());
                    #[cfg(windows)]
                    { app.job = job.clone(); }
                }
            }
            cancelled
        };
        if cancelled {
            #[cfg(windows)]
            if let Some(j) = &job { j.terminate(); }
            kill_tree_batch(&[pid]);
            let _ = tokio::time::timeout(
                std::time::Duration::from_secs(5),
                child.wait(),
            ).await;
            return Err("Cancelled by user".into());
        }

        // See script mode — guard against a stop+restart cycle racing the
        // older watcher into clobbering the newer run's state.
        let owning_pid = pid;
        tokio::spawn(async move {
            let result = child.wait().await;
            #[cfg(windows)]
            {
                if let Some(j) = &job_for_exit {
                    while j.active_processes() > 0 {
                        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                    }
                }
            }
            let msg = match result {
                Ok(s) => format!("Process exited with code {}", s),
                Err(e) => format!("Process error: {}", e),
            };
            // Skip all logging + state mutation if the app was deleted
            // while we were awaiting wait(). Without this we'd write an
            // orphan exit message to a fallback `app-<id>.log` for an app
            // the user just removed.
            let still_present = mgr.state.lock().unwrap().apps.contains_key(&id);
            if !still_present {
                return;
            }
            mgr.append_log(id, &msg);
            mgr.log_server(&format!("{} (id={}) exited", app_name, id));
            logs_exit.push(stamped(&msg));
            let mut state = mgr.state.lock().unwrap();
            if let Some(app) = state.apps.get_mut(&id) {
                if app.pid == Some(owning_pid) {
                    app.status = "stopped".into();
                    app.pid = None;
                    app.started_at = None;
                    #[cfg(windows)]
                    { app.job = None; }
                }
            }
        });

        self.log_server(&format!("Started: {} (id={}, pid={})", entry.name, id, pid));
        Ok(())
    }

    pub fn stop_app(&self, id: u32) -> Result<(), String> {
        // Take what we need under one short-lived lock, then release the
        // lock before calling kill_tree. kill_tree can sleep up to ~300 ms
        // on Linux (SIGTERM grace before SIGKILL) and shells out to
        // taskkill on Windows — holding the global state mutex through that
        // means every other API request blocks for the duration.
        let to_kill_pids: Vec<u32>;
        let static_shutdown: Option<tokio::sync::oneshot::Sender<()>>;
        #[cfg(windows)]
        let job: Option<Arc<winjob::Job>>;
        {
            let mut state = self.state.lock().unwrap();
            let app = state.apps.get_mut(&id).ok_or("App not found")?;
            // Idempotent: a Stop click on an already-stopped app is a no-op,
            // not an error. Returning Err here produced confusing toast
            // messages when the user double-clicked Stop or when a previous
            // exit-watcher had already flipped status to "stopped".
            if app.status != "running" && app.status != "building" {
                return Ok(());
            }
            // Set the cancel flag first so the build poll-loop sees it as
            // soon as it next wakes up.
            app.cancel_requested.store(true, Ordering::SeqCst);
            #[cfg(windows)]
            { job = app.job.take(); }
            let mut pids = Vec::with_capacity(2);
            if let Some(p) = app.pid.take() { pids.push(p); }
            if let Some(p) = app.build_pid.take() { pids.push(p); }
            to_kill_pids = pids;
            static_shutdown = app.static_shutdown.take();
            app.status = "stopped".into();
            app.started_at = None;
        }
        #[cfg(windows)]
        if let Some(j) = job { j.terminate(); }
        kill_tree_batch(&to_kill_pids);
        if let Some(tx) = static_shutdown { let _ = tx.send(()); }
        Ok(())
    }

    /// Convenience wrapper that stops then starts an app. Currently unused
    /// from server.rs (the handler runs the stop on a blocking pool to
    /// avoid stalling a tokio worker, then awaits start_app on the async
    /// runtime), but kept so future callers don't have to re-derive the
    /// pattern.
    #[allow(dead_code)]
    pub async fn restart_app(self: &Arc<Self>, id: u32, skip_build: bool) -> Result<(), String> {
        let _ = self.stop_app(id);
        self.start_app(id, skip_build).await
    }

    pub fn get_logs(&self, id: u32) -> Result<(String, String), String> {
        // Clone the LogSink Arcs under the outer lock, then snapshot WITHOUT
        // holding it — snapshot takes its own inner mutex and we don't want
        // the outer state lock contended for hundreds of microseconds while
        // we copy 2000 lines.
        let (logs, build_logs) = {
            let state = self.state.lock().unwrap();
            let app = state.apps.get(&id).ok_or("App not found")?;
            (app.logs.clone(), app.build_logs.clone())
        };
        Ok((logs.snapshot(), build_logs.snapshot()))
    }

    /// Subscribe to real-time log events for an app.
    /// Returns a snapshot of the current logs plus a receiver for future lines.
    pub fn subscribe_logs(&self, id: u32) -> Result<(String, String, tokio::sync::broadcast::Receiver<LogLine>), String> {
        // Subscribe FIRST so any line pushed between snapshot and subscribe
        // can't be lost. We tolerate a tiny duplicate window (the same line
        // appearing in both the snapshot and the live stream) — the dashboard
        // overwrites the buffer with the snapshot anyway.
        let (logs, build_logs, rx) = {
            let state = self.state.lock().unwrap();
            let app = state.apps.get(&id).ok_or("App not found")?;
            (app.logs.clone(), app.build_logs.clone(), app.log_tx.subscribe())
        };
        Ok((logs.snapshot(), build_logs.snapshot(), rx))
    }

    pub async fn start_all(self: &Arc<Self>) {
        // Detect duplicate ports across the apps we're about to start in
        // parallel — if two apps share a port, we'd let both go through the
        // full build only for one to fail at bind time. Reject the dupe up
        // front and log it instead of wasting the build cycles.
        let (ids, dupe_ports) = self.collect_startable_ids();
        for (port, name) in dupe_ports {
            self.log_server(&format!("start_all: skipped \"{}\" (port {} already used by another auto-start)", name, port));
        }
        // Kick all builds off concurrently. Each start_app handles its own
        // status transitions; if a build is heavy we don't want one slow
        // npm install to block the next four apps from even beginning.
        let mut handles = Vec::with_capacity(ids.len());
        for id in ids {
            let mgr = Arc::clone(self);
            handles.push(tokio::spawn(async move { let _ = mgr.start_app(id, false).await; }));
        }
        for h in handles { let _ = h.await; }
    }

    pub async fn auto_start_all(self: &Arc<Self>) {
        // Same dedupe-by-port treatment as start_all, but only consider
        // apps with auto_start = true.
        let (ids, dupe_ports) = self.collect_autostart_ids();
        for (port, name) in dupe_ports {
            self.log_server(&format!("auto_start_all: skipped \"{}\" (port {} already used by another auto-start)", name, port));
        }
        let mut handles = Vec::with_capacity(ids.len());
        for id in ids {
            let mgr = Arc::clone(self);
            handles.push(tokio::spawn(async move { let _ = mgr.start_app(id, false).await; }));
        }
        for h in handles { let _ = h.await; }
    }

    /// Build the (winners, losers) split for parallel batch startup.
    /// Apps that share a port are deduped: the one with the lowest `order`
    /// wins, all later ones are reported as losers so the user sees why
    /// they didn't start.
    fn collect_startable_ids(&self) -> (Vec<u32>, Vec<(u16, String)>) {
        let state = self.state.lock().unwrap();
        let mut candidates: Vec<&AppRuntime> = state
            .apps
            .values()
            .filter(|a| a.status == "stopped")
            .collect();
        candidates.sort_by_key(|a| a.entry.order);
        let mut seen_ports: std::collections::HashSet<u16> = std::collections::HashSet::new();
        let mut winners = Vec::new();
        let mut losers = Vec::new();
        for a in candidates {
            match a.entry.port {
                Some(p) if !seen_ports.insert(p) => losers.push((p, a.entry.name.clone())),
                _ => winners.push(a.entry.id),
            }
        }
        (winners, losers)
    }

    fn collect_autostart_ids(&self) -> (Vec<u32>, Vec<(u16, String)>) {
        let state = self.state.lock().unwrap();
        let mut candidates: Vec<&AppRuntime> = state
            .apps
            .values()
            .filter(|a| a.entry.auto_start)
            .collect();
        candidates.sort_by_key(|a| a.entry.order);
        let mut seen_ports: std::collections::HashSet<u16> = std::collections::HashSet::new();
        let mut winners = Vec::new();
        let mut losers = Vec::new();
        for a in candidates {
            match a.entry.port {
                Some(p) if !seen_ports.insert(p) => losers.push((p, a.entry.name.clone())),
                _ => winners.push(a.entry.id),
            }
        }
        (winners, losers)
    }

    pub fn stop_all(&self) {
        // Same lock-discipline as stop_app: collect the killable handles
        // under the mutex, then perform the actual kills (which can sleep
        // and run subprocesses) without holding the lock.
        let mut all_pids: Vec<u32> = Vec::new();
        let mut shutdowns: Vec<tokio::sync::oneshot::Sender<()>> = Vec::new();
        #[cfg(windows)]
        let mut jobs: Vec<Arc<winjob::Job>> = Vec::new();
        {
            let mut state = self.state.lock().unwrap();
            for (_id, app) in state.apps.iter_mut() {
                app.cancel_requested.store(true, Ordering::SeqCst);
                #[cfg(windows)]
                if let Some(j) = app.job.take() { jobs.push(j); }
                if let Some(p) = app.pid.take() { all_pids.push(p); }
                if let Some(p) = app.build_pid.take() { all_pids.push(p); }
                if let Some(s) = app.static_shutdown.take() { shutdowns.push(s); }
                app.status = "stopped".into();
                app.started_at = None;
            }
        }
        #[cfg(windows)]
        for j in jobs { j.terminate(); }
        for tx in shutdowns { let _ = tx.send(()); }
        // Batched kill: SIGTERM all pids in parallel, sleep ONCE, then
        // SIGKILL the survivors. This drops the wall-clock cost of stopping
        // N apps from N × 300 ms to a single 300 ms grace window.
        kill_tree_batch(&all_pids);
    }

    // ── File-based Logs ─────────────────────────────────────────

    fn app_log_name(&self, id: u32) -> String {
        let state = self.state.lock().unwrap();
        let raw = state.apps.get(&id)
            .map(|a| a.entry.name.clone())
            .unwrap_or_else(|| format!("app-{}", id));
        drop(state);
        sanitize_log_name(&raw, id)
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
                    std::thread::sleep(std::time::Duration::from_millis(1u64 << attempt));
                }
                Err(_) => return,
            }
        }
    }

    pub fn get_app_log(&self, id: u32) -> Result<String, String> {
        let state = self.state.lock().unwrap();
        let raw = state.apps.get(&id)
            .map(|a| a.entry.name.clone())
            .ok_or("App not found")?;
        drop(state);
        let name = sanitize_log_name(&raw, id);
        let path = self.log_file_path_for(&name);
        Ok(if path.exists() { tail_file(&path, 512 * 1024) } else { String::new() })
    }

    pub fn get_server_log(&self) -> String {
        let path = self.logs_dir.join("server.log");
        if path.exists() { tail_file(&path, 256 * 1024) } else { String::new() }
    }

    pub fn log_server(&self, line: &str) {
        use std::io::Write;
        // Same retry shape as append_log — noisy lifecycle paths can race on
        // the server.log handle (especially on Windows where another process
        // tailing the file may briefly hold a sharing lock). Without a
        // retry, important entries get silently dropped under load.
        let path = self.logs_dir.join("server.log");
        for attempt in 0..5u32 {
            match fs::OpenOptions::new().create(true).append(true).open(&path) {
                Ok(mut f) => {
                    let _ = writeln!(f, "[{}] {}", local_timestamp(), line.trim_end());
                    let _ = f.flush();
                    return;
                }
                Err(_) if attempt < 4 => {
                    std::thread::sleep(std::time::Duration::from_millis(1u64 << attempt));
                }
                Err(_) => return,
            }
        }
    }
}

// ─── Helpers ────────────────────────────────────────────────────────

/// Sanitize a user-supplied app name for use as a filesystem path component.
/// Strips any character that isn't ASCII alphanumeric / `.` / `_` / `-`,
/// flattens whitespace to `_`, and falls back to `app-<id>` when the result
/// would be empty, `.`, or `..`. This blocks path traversal (`..\..\..`),
/// absolute-path replacement (PathBuf::join semantics), and reserved names
/// from being routed through the on-disk log file.
pub fn sanitize_log_name(raw: &str, fallback_id: u32) -> String {
    let mut out = String::with_capacity(raw.len());
    for ch in raw.chars() {
        match ch {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '.' | '_' | '-' => out.push(ch),
            ' ' | '\t' => out.push('_'),
            _ => {} // drop everything else (slashes, drive letters, NULs, CRLF, ...)
        }
    }
    // Strip leading dots so we never produce dotfiles or names that look
    // like the parent dir (".", "..").
    while out.starts_with('.') { out.remove(0); }
    if out.is_empty() {
        return format!("app-{}", fallback_id);
    }
    // Windows treats CON, PRN, AUX, NUL, COM1–9, LPT1–9 as reserved device
    // names — opening `CON.log` can fail or hang the syscall. The check
    // applies to the stem (everything before the first dot). We prefix `_`
    // on every platform so log file names stay portable across hosts.
    let stem = out.split('.').next().unwrap_or("");
    if is_windows_reserved_device(stem) {
        out.insert(0, '_');
    }
    // Cap length so an enormous name can't blow up filesystem limits.
    if out.len() > 80 { out.truncate(80); }
    out
}

/// True if `s` is one of the Windows reserved device names (case-insensitive,
/// stem only). Used by `sanitize_log_name` to keep log paths portable.
fn is_windows_reserved_device(s: &str) -> bool {
    matches!(
        s.to_ascii_lowercase().as_str(),
        "con" | "prn" | "aux" | "nul"
        | "com1" | "com2" | "com3" | "com4" | "com5"
        | "com6" | "com7" | "com8" | "com9"
        | "lpt1" | "lpt2" | "lpt3" | "lpt4" | "lpt5"
        | "lpt6" | "lpt7" | "lpt8" | "lpt9"
    )
}

/// Sanitize a value being placed inside an HTTP header. Strips CR/LF (CRLF
/// header injection) and double-quotes (which would terminate quoted-string
/// header values). Used by the Content-Disposition filename for log exports.
pub fn sanitize_for_header(raw: &str) -> String {
    raw.chars()
        .filter(|c| !matches!(*c, '\r' | '\n' | '"' | '\\' | '\0'))
        .collect()
}

/// Read at most the last `max_bytes` of a file, aligned to a line boundary.
fn tail_file(path: &Path, max_bytes: u64) -> String {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut f) = fs::File::open(path) else { return String::new() };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    if len > max_bytes {
        let _ = f.seek(SeekFrom::Start(len - max_bytes));
        // Read raw bytes — read_to_string would refuse if the seek landed on
        // the second byte of a multi-byte UTF-8 char (very common with
        // emoji-rich npm/pnpm output) and silently return an empty buffer.
        let mut buf = Vec::with_capacity(max_bytes as usize);
        let _ = f.take(max_bytes).read_to_end(&mut buf);
        // Skip to the first newline to drop a partial leading line.
        let start = buf.iter().position(|&b| b == b'\n').map(|i| i + 1).unwrap_or(0);
        // Lossy UTF-8 decoding so any partial char fragments at the boundary
        // get replaced with U+FFFD instead of dropping the whole tail.
        String::from_utf8_lossy(&buf[start..]).into_owned()
    } else {
        let mut buf = Vec::with_capacity(len as usize);
        let _ = f.read_to_end(&mut buf);
        String::from_utf8_lossy(&buf).into_owned()
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
        // POSIX `struct tm`. The trailing fields differ across platforms
        // (glibc/musl have `long tm_gmtoff` + `const char *tm_zone`,
        // macOS matches glibc; older musl drops the zone pointer). We
        // only read the leading 9 ints, but the libc still writes the
        // full struct — so the buffer MUST be at least as large as the
        // platform's `struct tm` or we corrupt the stack.
        //
        // Sizes we need to cover (bytes after the 9 ints):
        //   * 8 (long, 64-bit) + 8 (ptr) + alignment padding = up to 24
        // We round up to 64 bytes of trailing padding, which is
        // unconditionally larger than any real `struct tm` we'll meet.
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
            _pad: [u8; 64],
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
            wday: 0, yday: 0, isdst: 0, _pad: [0; 64],
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

/// Trigger the one-time `npm prefix -g` probe on a blocking thread so the
/// first `start_app` call doesn't block the tokio executor for hundreds of
/// milliseconds while we shell out to npm.
pub fn prewarm_npm_prefix() {
    // Name the thread so it shows up in process listings / debuggers as
    // "appnest-prewarm-npm" rather than the unhelpful default "<unnamed>".
    let _ = std::thread::Builder::new()
        .name("appnest-prewarm-npm".into())
        .spawn(|| { let _ = npm_global_prefix(); });
}

/// Return Some(_) if `name` resolves to an executable on PATH. Used to pick
/// `pwsh` over `powershell` on Windows installs where Windows PowerShell 5.1
/// has been removed.
#[cfg(windows)]
pub fn which_on_path(name: &str) -> Option<std::path::PathBuf> {
    let path = std::env::var_os("PATH")?;
    let exts: Vec<String> = std::env::var("PATHEXT")
        .unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into())
        .split(';')
        .map(|s| s.to_ascii_lowercase())
        .filter(|s| !s.is_empty())
        .collect();
    for dir in std::env::split_paths(&path) {
        let bare = dir.join(name);
        if bare.is_file() { return Some(bare); }
        for ext in &exts {
            let candidate = dir.join(format!("{}{}", name, ext));
            if candidate.is_file() { return Some(candidate); }
        }
    }
    None
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
    //
    // On Windows we prefer terminating the Job Object — that atomically
    // kills the entire descendant tree (cmd → npm → node → webpack-dev-server)
    // in one syscall, which `taskkill /T /F` can race against on long chains.
    #[cfg(windows)]
    {
        if let Some(job) = app.job.take() {
            job.terminate();
        }
    }
    // Batch both pids into one kill_tree call so we share the single
    // 300 ms SIGTERM grace window on Linux instead of paying it twice.
    let mut pids = Vec::with_capacity(2);
    if let Some(p) = app.pid.take() { pids.push(p); }
    if let Some(p) = app.build_pid.take() { pids.push(p); }
    kill_tree_batch(&pids);
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

/// Send SIGTERM to every pid in `pids`, sleep ONCE for the grace window,
/// then SIGKILL the survivors. This is what `stop_all`, `stop_app`, and
/// `stop_runtime` use instead of N \u00d7 (TERM, sleep 300ms, KILL) which
/// would multiply the wall clock by the number of pids being stopped.
fn kill_tree_batch(pids: &[u32]) {
    // Filter out unsafe / unknown pids BEFORE doing anything. A stray `0`
    // can leak in if `child.id()` returned None and a caller stored it as
    // `Some(0)` — on Unix `kill -- -0` SIGTERMs the entire process group
    // of the calling process (i.e. AppNest itself), and on Windows
    // `taskkill /PID 0` would target the System Idle pseudo-process. We
    // also drop pid 1 on Unix as belt-and-braces against init.
    let safe: Vec<u32> = pids
        .iter()
        .copied()
        .filter(|p| {
            if *p == 0 { return false; }
            #[cfg(not(windows))]
            { if *p == 1 { return false; } }
            true
        })
        .collect();
    if safe.is_empty() { return; }
    #[cfg(windows)]
    {
        // Windows: taskkill /T /F is fire-and-forget (no grace window),
        // so we don't gain anything from batching beyond skipping the
        // empty case. Issue them in parallel via a single fan-out.
        for pid in &safe {
            let _ = std::process::Command::new("taskkill")
                .args(["/T", "/F", "/PID", &pid.to_string()])
                .creation_flags(0x08000000)
                .output();
        }
    }
    #[cfg(not(windows))]
    {
        // SIGTERM the whole batch first so all process groups get the
        // signal at the same time, then sleep ONCE before the SIGKILL
        // pass. Without this, stopping N apps would multiply the 300 ms
        // grace window by N.
        for pid in &safe {
            let pgid = format!("-{}", pid);
            let _ = std::process::Command::new("kill")
                .args(["-TERM", "--", &pgid])
                .output();
        }
        std::thread::sleep(std::time::Duration::from_millis(300));
        for pid in &safe {
            let pgid = format!("-{}", pid);
            let _ = std::process::Command::new("kill")
                .args(["-KILL", "--", &pgid])
                .output();
        }
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
