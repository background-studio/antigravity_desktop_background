use std::{
    fs::{self, File},
    io::{Read, Seek, SeekFrom},
    net::TcpListener,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    injector::{main_page_ready, read_browser_identity, InjectorEngine},
    managed_launch::{
        normalize_executable_path, snapshot_executable_processes, terminate_process, Observation,
        ProcessRecord, ProcessTracker, WatcherAction, DEBUG_PORT_WAIT, MSG_AUTO_APPLIED,
        MSG_DEBUG_TIMEOUT, MSG_EXISTING, MSG_SUSPENDED, MSG_TAKING_OVER, MSG_WAITING,
    },
    models::RuntimeStatus,
    payload::PayloadBundle,
    settings::write_json_transaction,
};

const PREFERRED_DEBUG_PORT: u16 = 9257;
const RUNTIME_SCHEMA_VERSION: u8 = 1;
const EXECUTABLE_NAME: &str = "Antigravity.exe";

#[derive(Clone, Debug, PartialEq, Eq)]
struct AntigravityInstall {
    root: String,
    executable: String,
    version: String,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct RuntimeState {
    schema_version: u8,
    port: u16,
    browser_id: String,
    executable: String,
    version: String,
    created_at: String,
}

fn normalized_path(path: &str) -> String {
    normalize_executable_path(path)
}

fn antigravity_root() -> Option<PathBuf> {
    // 单元测试绝不能解析到真实安装目录：restore/apply 会结束并重启用户正在使用的 Antigravity。
    #[cfg(test)]
    {
        Some(std::env::temp_dir().join("antigravity-background-studio-test-missing-install"))
    }
    #[cfg(not(test))]
    {
        std::env::var_os("LOCALAPPDATA")
            .map(|local| PathBuf::from(local).join("Programs").join("Antigravity"))
    }
}

/// 从 `resources/app.asar` 的头部直接读取 `package.json` 中的客户端版本号（如 `2.35.1`）；
/// 失败时回退到安装根目录的 `version` 文件或 `"unknown"`。
fn read_app_version(root: &Path) -> String {
    if let Some(version) = read_asar_package_version(&root.join("resources").join("app.asar")) {
        return version;
    }
    if let Ok(raw) = fs::read_to_string(root.join("version")) {
        let trimmed = raw.trim();
        if !trimmed.is_empty() && trimmed.len() <= 64 {
            return trimmed.to_string();
        }
    }
    "unknown".to_string()
}

fn read_asar_package_version(asar_path: &Path) -> Option<String> {
    let mut file = File::open(asar_path).ok()?;
    let mut prefix = [0u8; 16];
    file.read_exact(&mut prefix).ok()?;
    let header_size = u32::from_le_bytes(prefix[4..8].try_into().ok()?) as u64;
    let str_len = u32::from_le_bytes(prefix[12..16].try_into().ok()?) as usize;
    if str_len == 0 || str_len > 8 * 1024 * 1024 {
        return None;
    }
    let mut header_bytes = vec![0u8; str_len];
    file.read_exact(&mut header_bytes).ok()?;
    let header: Value = serde_json::from_slice(&header_bytes).ok()?;
    let pkg = header.pointer("/files/package.json")?;
    if pkg.get("unpacked").and_then(Value::as_bool) == Some(true) {
        return None;
    }
    let offset = pkg.get("offset")?.as_str()?.parse::<u64>().ok()?;
    let size = pkg.get("size")?.as_u64()? as usize;
    if size == 0 || size > 64 * 1024 {
        return None;
    }
    let data_offset = 8u64.checked_add(header_size)?.checked_add(offset)?;
    file.seek(SeekFrom::Start(data_offset)).ok()?;
    let mut pkg_bytes = vec![0u8; size];
    file.read_exact(&mut pkg_bytes).ok()?;
    let pkg_json: Value = serde_json::from_slice(&pkg_bytes).ok()?;
    pkg_json
        .get("version")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty() && value.len() <= 64)
        .map(str::to_string)
}

fn discover_antigravity() -> Result<AntigravityInstall, String> {
    let root = antigravity_root().ok_or_else(|| "无法读取 LOCALAPPDATA。".to_string())?;
    discover_antigravity_in(&root)
}

fn discover_antigravity_in(root: &Path) -> Result<AntigravityInstall, String> {
    let executable = root.join(EXECUTABLE_NAME);
    let asar = root.join("resources").join("app.asar");
    if !executable.is_file() || !asar.is_file() {
        return Err(
            "未找到官方 Antigravity 桌面应用（预期路径：%LOCALAPPDATA%\\Programs\\Antigravity\\Antigravity.exe）。"
                .to_string(),
        );
    }
    let root_norm = normalized_path(&root.to_string_lossy());
    let exe_norm = normalized_path(&executable.to_string_lossy());
    if !exe_norm.starts_with(&root_norm) || !exe_norm.ends_with("\\antigravity.exe") {
        return Err("Antigravity 安装路径未通过校验。".to_string());
    }
    Ok(AntigravityInstall {
        root: root.to_string_lossy().to_string(),
        executable: executable.to_string_lossy().to_string(),
        version: read_app_version(root),
    })
}

fn process_records_for(install: &AntigravityInstall) -> Result<Vec<ProcessRecord>, String> {
    snapshot_executable_processes(&install.executable)
}

fn process_ids_for(install: &AntigravityInstall) -> Result<Vec<u32>, String> {
    Ok(process_records_for(install)?
        .into_iter()
        .map(|process| process.key.pid)
        .collect())
}

fn debug_launched(install: &AntigravityInstall) -> Result<bool, String> {
    Ok(process_records_for(install)?
        .iter()
        .any(|process| process.has_debug_args))
}

fn debug_ports_for(install: &AntigravityInstall) -> Result<Vec<u16>, String> {
    let mut ports = process_records_for(install)?
        .into_iter()
        .filter_map(|process| process.debug_port)
        .collect::<Vec<_>>();
    ports.sort_unstable();
    ports.dedup();
    Ok(ports)
}

fn is_disconnected_error(error: &str) -> bool {
    error.contains("CDP 会话已关闭")
        || error.contains("CDP 浏览器身份已变化")
        || error.contains("CDP 命令超时")
        || error.contains("CDP 连接超时")
        || error.contains("CDP 返回 HTTP")
}

fn session_ready(port: u16, browser_id: &str) -> bool {
    main_page_ready(port, browser_id).unwrap_or(false)
}

/// 只结束映像路径与本次发现的 `Antigravity.exe` 完全一致、且非 `RunAsNode` sidecar 的进程。
/// 纯 Win32 API 结束，不依赖外部 PowerShell。
fn stop_verified_antigravity(install: &AntigravityInstall) -> Result<(), String> {
    let records = process_records_for(install)?;
    // 先结束主浏览器进程，再清理其余子进程（renderer/gpu/utility）。
    for record in records.iter().filter(|record| record.is_browser_main) {
        terminate_process(record.key.pid);
    }
    for record in records.iter().filter(|record| !record.is_browser_main) {
        terminate_process(record.key.pid);
    }
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let remaining = process_ids_for(install)?;
        if remaining.is_empty() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err("Antigravity 未能在 15 秒内完全退出。".to_string());
        }
        for pid in remaining {
            terminate_process(pid);
        }
        thread::sleep(Duration::from_millis(250));
    }
}

fn launch_antigravity(install: &AntigravityInstall, arguments: &[String]) -> Result<(), String> {
    let executable = PathBuf::from(&install.executable);
    if !executable.is_file() {
        return Err("Antigravity 可执行文件不存在。".to_string());
    }
    Command::new(&executable)
        .args(arguments)
        .current_dir(executable.parent().unwrap_or(Path::new(&install.root)))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("启动 Antigravity 失败：{error}"))?;
    Ok(())
}

fn debug_arguments(port: u16) -> Vec<String> {
    vec![
        "--remote-debugging-address=127.0.0.1".to_string(),
        format!("--remote-debugging-port={port}"),
    ]
}

fn launch_with_debug_port(install: &AntigravityInstall, port: u16) -> Result<String, String> {
    launch_antigravity(install, &debug_arguments(port))?;
    let started = Instant::now();
    let deadline = started + DEBUG_PORT_WAIT;
    loop {
        if let Ok(browser_id) = read_browser_identity(port) {
            if session_ready(port, &browser_id) {
                return Ok(browser_id);
            }
        }
        if started.elapsed() > Duration::from_secs(8) && process_ids_for(install)?.is_empty() {
            return Err(
                "Antigravity 启动后已退出，可能有其他 Antigravity 实例占用了单实例锁。".to_string(),
            );
        }
        if Instant::now() >= deadline {
            return Err("Antigravity 已启动，但调试端口上的主界面未在 45 秒内就绪。".to_string());
        }
        thread::sleep(Duration::from_millis(250));
    }
}

fn select_port(preferred: u16) -> Result<u16, String> {
    for port in preferred..=preferred.saturating_add(100) {
        if TcpListener::bind(("127.0.0.1", port)).is_ok() {
            return Ok(port);
        }
    }
    Err("无法为 Antigravity 分配本机调试端口。".to_string())
}

pub struct ManagedProbe {
    pub processes: Vec<ProcessRecord>,
    pub has_ready_debug_session: bool,
    pub engine_alive: bool,
}

impl ManagedProbe {
    pub fn observation(&self) -> Observation {
        Observation {
            processes: self.processes.clone(),
            has_ready_debug_session: self.has_ready_debug_session,
            engine_alive: self.engine_alive,
            now: Instant::now(),
        }
    }
}

pub struct AntigravityController {
    state_path: PathBuf,
    engine: Option<InjectorEngine>,
    state: Option<RuntimeState>,
    status: RuntimeStatus,
    install_cache: Option<AntigravityInstall>,
    install_checked_at: Option<Instant>,
    tracker: ProcessTracker,
    watcher_paused: bool,
}

const INSTALL_RECHECK_INTERVAL: Duration = Duration::from_secs(30);

impl AntigravityController {
    pub fn load(data_directory: &Path) -> Self {
        let state_path = data_directory.join("runtime.json");
        let state = fs::read_to_string(&state_path)
            .ok()
            .and_then(|content| serde_json::from_str::<RuntimeState>(&content).ok())
            .filter(|state| {
                state.schema_version == RUNTIME_SCHEMA_VERSION && !state.browser_id.is_empty()
            });
        Self {
            state_path,
            engine: None,
            state,
            status: RuntimeStatus::default(),
            install_cache: None,
            install_checked_at: None,
            tracker: ProcessTracker::new(),
            watcher_paused: false,
        }
    }

    pub fn status(&self) -> RuntimeStatus {
        let mut status = self.status.clone();
        if let Some(engine) = &self.engine {
            status.active_targets = engine.active_targets();
            status.target_ids = engine.target_ids();
        } else {
            status.active_targets = 0;
            status.target_ids.clear();
        }
        status
    }

    pub fn watcher_paused(&self) -> bool {
        self.watcher_paused
    }

    pub fn set_watcher_paused(&mut self, paused: bool) {
        self.watcher_paused = paused;
    }

    pub fn set_managed_status(&mut self, phase: &str, message: &str) {
        self.status.phase = phase.to_string();
        self.status.message = message.to_string();
        if phase != "error" {
            self.status.last_error = None;
        }
    }

    pub fn set_managed_error(&mut self, error: impl Into<String>) {
        let error = error.into();
        self.status.phase = "error".to_string();
        self.status.message = error.clone();
        self.status.last_error = Some(error);
    }

    fn cached_install(&mut self) -> Result<AntigravityInstall, String> {
        let fresh = self
            .install_checked_at
            .is_some_and(|checked| checked.elapsed() < INSTALL_RECHECK_INTERVAL);
        if let Some(install) = &self.install_cache {
            if fresh && Path::new(&install.executable).is_file() {
                return Ok(install.clone());
            }
        }
        let install = discover_antigravity()?;
        self.install_cache = Some(install.clone());
        self.install_checked_at = Some(Instant::now());
        Ok(install)
    }

    fn state_matches(state: &RuntimeState, install: &AntigravityInstall) -> bool {
        normalized_path(&state.executable) == normalized_path(&install.executable)
            && read_browser_identity(state.port).ok().as_deref() == Some(state.browser_id.as_str())
            && session_ready(state.port, &state.browser_id)
    }

    fn saved_session_is_live(&self, install: &AntigravityInstall) -> bool {
        self.state
            .as_ref()
            .is_some_and(|state| Self::state_matches(state, install))
    }

    fn live_debug_session_ready(processes: &[ProcessRecord]) -> bool {
        for process in processes {
            let Some(port) = process.debug_port else {
                continue;
            };
            let Ok(browser_id) = read_browser_identity(port) else {
                continue;
            };
            if session_ready(port, &browser_id) {
                return true;
            }
        }
        false
    }

    pub fn probe_managed(&mut self) -> Result<ManagedProbe, String> {
        let install = self.cached_install()?;
        let processes = self.tracker.scan(&install.executable)?;
        let engine_alive = self
            .engine
            .as_ref()
            .is_some_and(InjectorEngine::is_connected);
        let has_ready_debug_session = engine_alive
            || self.saved_session_is_live(&install)
            || Self::live_debug_session_ready(&processes);
        self.status.antigravity_version = Some(install.version);
        Ok(ManagedProbe {
            processes,
            has_ready_debug_session,
            engine_alive,
        })
    }

    pub fn apply_watcher_action_status(&mut self, action: WatcherAction) {
        match action {
            WatcherAction::Wait => {
                if !matches!(self.status.phase.as_str(), "active" | "paused") {
                    self.set_managed_status("idle", MSG_WAITING);
                }
            }
            WatcherAction::ReleaseAndWait => {
                self.set_managed_status("idle", MSG_WAITING);
            }
            WatcherAction::ReportExistingUnmanaged => {
                self.set_managed_status("idle", MSG_EXISTING);
            }
            WatcherAction::WaitForDebugPort | WatcherAction::Takeover | WatcherAction::Attach => {
                self.set_managed_status("starting", MSG_TAKING_OVER);
            }
            WatcherAction::ReportDebugTimeout => {
                self.set_managed_error(MSG_DEBUG_TIMEOUT);
            }
            WatcherAction::KeepActive | WatcherAction::Suspend => {}
        }
    }

    pub fn auto_takeover(&mut self, bundle: PayloadBundle) -> Result<RuntimeStatus, String> {
        self.set_managed_status("starting", MSG_TAKING_OVER);
        self.apply(bundle, true)?;
        self.set_managed_status("active", MSG_AUTO_APPLIED);
        Ok(self.status())
    }

    pub fn attach_without_restart(
        &mut self,
        bundle: PayloadBundle,
    ) -> Result<RuntimeStatus, String> {
        self.set_managed_status("starting", MSG_TAKING_OVER);
        self.apply(bundle, false)?;
        if self.status.phase == "active" {
            self.set_managed_status("active", MSG_AUTO_APPLIED);
        }
        Ok(self.status())
    }

    pub fn release_stale_session(&mut self) -> Result<RuntimeStatus, String> {
        self.engine = None;
        self.write_state(None)?;
        self.status.active_targets = 0;
        self.status.target_ids.clear();
        self.set_managed_status("idle", MSG_WAITING);
        Ok(self.status())
    }

    fn drop_disconnected_engine(&mut self) {
        if self
            .engine
            .as_ref()
            .is_some_and(|engine| !engine.is_connected())
        {
            self.engine = None;
        }
    }

    fn write_state(&mut self, state: Option<RuntimeState>) -> Result<(), String> {
        self.state = state;
        match &self.state {
            Some(state) => write_json_transaction(&self.state_path, state),
            None => match fs::remove_file(&self.state_path) {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(error) => Err(error.to_string()),
            },
        }
    }

    fn remember_session(
        &mut self,
        install: &AntigravityInstall,
        port: u16,
        browser_id: &str,
    ) -> Result<(), String> {
        self.write_state(Some(RuntimeState {
            schema_version: RUNTIME_SCHEMA_VERSION,
            port,
            browser_id: browser_id.to_string(),
            executable: install.executable.clone(),
            version: install.version.clone(),
            created_at: Utc::now().to_rfc3339(),
        }))
    }

    fn try_attach_saved(&mut self, install: &AntigravityInstall) -> bool {
        let Some(state) = &self.state else {
            return false;
        };
        if !Self::state_matches(state, install) {
            return false;
        }
        self.engine = Some(InjectorEngine::new(state.port, state.browser_id.clone()));
        true
    }

    pub fn reconnect_saved(&mut self, bundle: PayloadBundle) -> Result<bool, String> {
        if self.state.is_none() {
            return Ok(false);
        }
        let install = self.cached_install()?;
        if !self.try_attach_saved(&install) {
            self.write_state(None)?;
            self.status = RuntimeStatus::default();
            return Ok(false);
        }
        let result = self
            .engine
            .as_mut()
            .expect("engine set after saved session validation")
            .start(bundle);
        match result {
            Ok(()) => {
                self.status.phase = "active".to_string();
                self.status.message = "已自动恢复背景会话".to_string();
                self.status.antigravity_version = Some(install.version);
                self.status.last_error = None;
                Ok(true)
            }
            Err(error) => {
                self.engine = None;
                self.status.phase = "error".to_string();
                self.status.message = error.clone();
                self.status.last_error = Some(error.clone());
                Err(error)
            }
        }
    }

    pub fn apply(
        &mut self,
        bundle: PayloadBundle,
        restart_existing: bool,
    ) -> Result<RuntimeStatus, String> {
        self.watcher_paused = false;
        self.status.phase = "starting".to_string();
        self.status.message = "正在连接 Antigravity".to_string();
        self.status.last_error = None;
        let result: Result<RuntimeStatus, String> = (|| {
            let install = self.cached_install()?;
            self.drop_disconnected_engine();
            if let Some(engine) = &self.engine {
                match engine.update(bundle.clone()) {
                    Ok(()) => {
                        self.status.phase = "active".to_string();
                        self.status.message = "背景已实时应用".to_string();
                        self.status.antigravity_version = Some(install.version);
                        return Ok(self.status());
                    }
                    Err(error) if is_disconnected_error(&error) => {
                        self.engine = None;
                    }
                    Err(error) => return Err(error),
                }
            }
            if self.try_attach_saved(&install) {
                self.engine
                    .as_mut()
                    .expect("engine set after attach")
                    .start(bundle)?;
                self.status.phase = "active".to_string();
                self.status.message = "已重新连接背景会话".to_string();
                self.status.antigravity_version = Some(install.version);
                return Ok(self.status());
            }
            for port in debug_ports_for(&install)? {
                let Ok(browser_id) = read_browser_identity(port) else {
                    continue;
                };
                if !session_ready(port, &browser_id) {
                    continue;
                }
                self.remember_session(&install, port, &browser_id)?;
                let mut engine = InjectorEngine::new(port, browser_id);
                engine.start(bundle.clone())?;
                self.engine = Some(engine);
                self.status.phase = "active".to_string();
                self.status.message = "已重新连接背景会话".to_string();
                self.status.antigravity_version = Some(install.version);
                return Ok(self.status());
            }
            let running = process_ids_for(&install)?;
            if !running.is_empty() && !restart_existing {
                return Err("Antigravity 需要重启一次以启用背景。".to_string());
            }
            if !running.is_empty() {
                stop_verified_antigravity(&install)?;
            }
            let port = select_port(PREFERRED_DEBUG_PORT)?;
            let browser_id = match launch_with_debug_port(&install, port) {
                Ok(browser_id) => browser_id,
                Err(error) => {
                    let recovery = stop_verified_antigravity(&install)
                        .and_then(|_| launch_antigravity(&install, &[]));
                    return Err(match recovery {
                        Ok(()) => format!("调试端口启动失败，已恢复官方 Antigravity：{error}"),
                        Err(recovery_error) => format!(
                            "调试端口启动失败：{error}；恢复官方 Antigravity 也失败：{recovery_error}"
                        ),
                    });
                }
            };
            self.remember_session(&install, port, &browser_id)?;
            let mut engine = InjectorEngine::new(port, browser_id);
            engine.start(bundle)?;
            self.engine = Some(engine);
            self.status.phase = "active".to_string();
            self.status.message = "背景已应用".to_string();
            self.status.antigravity_version = Some(install.version);
            Ok(self.status())
        })();
        if let Err(error) = &result {
            self.status.phase = if error.contains("需要重启一次") {
                "idle".to_string()
            } else {
                "error".to_string()
            };
            self.status.message = error.clone();
            self.status.last_error = Some(error.clone());
        }
        result
    }

    pub fn pause(&mut self) -> Result<RuntimeStatus, String> {
        self.watcher_paused = true;
        if let Some(engine) = &self.engine {
            engine.pause()?;
        }
        self.status.phase = "paused".to_string();
        self.status.message = MSG_SUSPENDED.to_string();
        self.status.last_error = None;
        Ok(self.status())
    }

    /// 移除注入；仅当 Antigravity 确实带着调试端口运行时才重启为官方普通模式，
    /// 普通启动的 Antigravity 本来就是官方外观，绝不打断用户的会话。
    pub fn restore(&mut self) -> Result<RuntimeStatus, String> {
        self.watcher_paused = true;
        self.status.phase = "restoring".to_string();
        self.status.message = "正在恢复官方外观".to_string();
        self.status.last_error = None;
        let result: Result<RuntimeStatus, String> = (|| {
            if let Some(mut engine) = self.engine.take() {
                engine.stop()?;
            }
            if let Ok(install) = self.cached_install() {
                if debug_launched(&install)? {
                    stop_verified_antigravity(&install)?;
                    launch_antigravity(&install, &[])?;
                }
                self.status.antigravity_version = Some(install.version);
            }
            self.write_state(None)?;
            self.status.phase = "idle".to_string();
            self.status.message = "已恢复官方外观".to_string();
            self.status.active_targets = 0;
            self.status.target_ids.clear();
            Ok(self.status())
        })();
        if let Err(error) = &result {
            self.status.phase = "error".to_string();
            self.status.message = error.clone();
            self.status.last_error = Some(error.clone());
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn discovers_valid_install_and_reads_fallback_version() {
        let root = std::env::temp_dir().join(format!("antigravity-install-{}", Uuid::new_v4()));
        fs::create_dir_all(root.join("resources")).unwrap();
        fs::write(root.join("Antigravity.exe"), b"exe").unwrap();
        fs::write(root.join("resources").join("app.asar"), b"not-asar").unwrap();
        fs::write(root.join("version"), "2.35.1\n").unwrap();

        let install = discover_antigravity_in(&root).unwrap();
        assert_eq!(install.version, "2.35.1");
        assert!(install.executable.ends_with("Antigravity.exe"));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn rejects_missing_install() {
        let missing = std::env::temp_dir().join(format!("antigravity-missing-{}", Uuid::new_v4()));
        assert!(discover_antigravity_in(&missing).is_err());
    }
}
