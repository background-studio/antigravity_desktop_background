use std::{
    collections::{HashMap, HashSet},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

pub const MSG_WAITING: &str = "已启用，等待 Antigravity 启动";
pub const MSG_EXISTING: &str = "Antigravity 已在运行，点立即接管可重启";
pub const MSG_TAKING_OVER: &str = "正在接管 Antigravity";
pub const MSG_AUTO_APPLIED: &str = "背景已自动应用";
pub const MSG_SUSPENDED: &str = "托管已暂停";
pub const MSG_UNCONFIGURED: &str = "尚未配置背景";
pub const MSG_DEBUG_TIMEOUT: &str = "调试会话未在 45 秒内就绪，请关闭 Antigravity 后重试";
pub const MSG_BUSY: &str = "正在接管/应用中";

const CONFIRM_TICKS: u8 = 2;
pub const DEBUG_PORT_WAIT: Duration = Duration::from_secs(45);

pub struct BusyGuard<'a> {
    flag: &'a AtomicBool,
}

impl<'a> BusyGuard<'a> {
    pub fn acquire(flag: &'a AtomicBool) -> Option<Self> {
        if flag.swap(true, Ordering::AcqRel) {
            None
        } else {
            Some(Self { flag })
        }
    }
}

impl Drop for BusyGuard<'_> {
    fn drop(&mut self) {
        self.flag.store(false, Ordering::Release);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ProcessKey {
    pub pid: u32,
    pub created_at: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessRecord {
    pub key: ProcessKey,
    pub has_debug_args: bool,
    pub debug_port: Option<u16>,
    pub is_browser_main: bool,
}

impl ProcessRecord {
    #[cfg(test)]
    pub fn new(pid: u32, created_at: u64) -> Self {
        Self {
            key: ProcessKey { pid, created_at },
            has_debug_args: false,
            debug_port: None,
            is_browser_main: true,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Observation {
    pub processes: Vec<ProcessRecord>,
    pub has_ready_debug_session: bool,
    pub engine_alive: bool,
    pub now: Instant,
}

impl Default for Observation {
    fn default() -> Self {
        Self {
            processes: Vec::new(),
            has_ready_debug_session: false,
            engine_alive: false,
            now: Instant::now(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WatcherAction {
    Wait,
    ReportExistingUnmanaged,
    WaitForDebugPort,
    ReportDebugTimeout,
    Attach,
    Takeover,
    KeepActive,
    ReleaseAndWait,
    Suspend,
}

#[derive(Debug, Default)]
pub struct WatcherState {
    paused: bool,
    baseline: Option<HashSet<ProcessKey>>,
    in_flight: bool,
    session_active: bool,
    pending_ticks: u8,
    debug_wait_started: Option<Instant>,
    debug_stalled: bool,
}

impl WatcherState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn suspend(&mut self) {
        self.paused = true;
        self.in_flight = false;
        self.pending_ticks = 0;
        self.clear_debug_wait();
    }

    pub fn rearm(&mut self) {
        self.paused = false;
    }

    pub fn arm_after_configure(&mut self, processes: &[ProcessKey]) {
        if self.paused {
            return;
        }
        if self.baseline.is_none() {
            self.baseline = Some(processes.iter().copied().collect());
        }
    }

    pub fn mark_managed_active(&mut self, processes: &[ProcessKey]) {
        self.paused = false;
        self.in_flight = false;
        self.session_active = true;
        self.pending_ticks = 0;
        self.clear_debug_wait();
        self.baseline = Some(processes.iter().copied().collect());
    }

    pub fn takeover_failed(&mut self, processes: &[ProcessKey]) {
        self.in_flight = false;
        self.session_active = false;
        self.pending_ticks = 0;
        self.clear_debug_wait();
        self.baseline = Some(processes.iter().copied().collect());
    }

    pub fn sync_to_current(&mut self, observation: &Observation) -> WatcherAction {
        self.in_flight = false;
        self.session_active = false;
        self.pending_ticks = 0;
        self.clear_debug_wait();
        self.baseline = Some(HashSet::new());
        self.decide(observation)
    }

    fn clear_debug_wait(&mut self) {
        self.debug_wait_started = None;
        self.debug_stalled = false;
    }

    fn wait_for_debug_port(&mut self, now: Instant, keys: HashSet<ProcessKey>) -> WatcherAction {
        let started = *self.debug_wait_started.get_or_insert(now);
        if now.saturating_duration_since(started) >= DEBUG_PORT_WAIT {
            self.in_flight = false;
            self.session_active = false;
            self.debug_stalled = true;
            self.pending_ticks = 0;
            self.baseline = Some(keys);
            return WatcherAction::ReportDebugTimeout;
        }
        WatcherAction::WaitForDebugPort
    }

    pub fn decide(&mut self, observation: &Observation) -> WatcherAction {
        if self.paused {
            return WatcherAction::Suspend;
        }

        let keys = keys_of(&observation.processes);
        let has_debug_args = observation
            .processes
            .iter()
            .any(|process| process.has_debug_args);

        if keys.is_empty() {
            let cleanup = self.session_active || self.in_flight || self.debug_stalled;
            self.in_flight = false;
            self.session_active = false;
            self.pending_ticks = 0;
            self.clear_debug_wait();
            self.baseline = Some(HashSet::new());
            return if cleanup {
                WatcherAction::ReleaseAndWait
            } else {
                WatcherAction::Wait
            };
        }

        if observation.engine_alive && self.session_active {
            self.in_flight = false;
            self.pending_ticks = 0;
            self.clear_debug_wait();
            match &mut self.baseline {
                Some(baseline) => baseline.extend(keys),
                None => self.baseline = Some(keys),
            }
            return WatcherAction::KeepActive;
        }

        if observation.has_ready_debug_session || observation.engine_alive {
            self.in_flight = false;
            self.session_active = true;
            self.pending_ticks = 0;
            self.clear_debug_wait();
            self.baseline = Some(keys);
            return WatcherAction::Attach;
        }

        if self.debug_stalled {
            return WatcherAction::ReportDebugTimeout;
        }

        if self.in_flight || has_debug_args {
            return self.wait_for_debug_port(observation.now, keys);
        }

        if self.session_active {
            self.session_active = false;
            self.pending_ticks = 0;
            self.baseline = Some(keys);
            return WatcherAction::ReleaseAndWait;
        }

        if self.baseline.is_none() {
            self.baseline = Some(keys);
            return WatcherAction::ReportExistingUnmanaged;
        }

        let baseline_alive = keys.iter().any(|key| {
            self.baseline
                .as_ref()
                .is_some_and(|baseline| baseline.contains(key))
        });
        if baseline_alive {
            self.pending_ticks = 0;
            return WatcherAction::ReportExistingUnmanaged;
        }

        self.pending_ticks = self.pending_ticks.saturating_add(1);
        if self.pending_ticks >= CONFIRM_TICKS {
            self.in_flight = true;
            self.pending_ticks = 0;
            return WatcherAction::Takeover;
        }
        WatcherAction::Wait
    }
}

pub fn keys_of(processes: &[ProcessRecord]) -> HashSet<ProcessKey> {
    processes.iter().map(|process| process.key).collect()
}

pub fn process_key_list(processes: &[ProcessRecord]) -> Vec<ProcessKey> {
    processes.iter().map(|process| process.key).collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfirmedLaunch {
    Attach,
    Takeover,
    CancelExited,
    CancelStale,
}

pub fn confirm_launch_after_encode(
    planned: WatcherAction,
    original: &HashSet<ProcessKey>,
    current: &Observation,
) -> ConfirmedLaunch {
    let current_keys = keys_of(&current.processes);
    if current_keys.is_empty() {
        return ConfirmedLaunch::CancelExited;
    }
    if current.engine_alive || current.has_ready_debug_session {
        return ConfirmedLaunch::Attach;
    }
    if planned != WatcherAction::Takeover {
        return ConfirmedLaunch::CancelStale;
    }
    if original.iter().any(|key| current_keys.contains(key)) {
        ConfirmedLaunch::Takeover
    } else {
        ConfirmedLaunch::CancelStale
    }
}

pub fn normalize_executable_path(path: &str) -> String {
    path.replace('/', "\\")
        .trim_end_matches('\\')
        .to_ascii_lowercase()
}

pub fn parse_remote_debugging_port(command_line: &str) -> Option<u16> {
    let marker = "--remote-debugging-port=";
    let rest = command_line.split(marker).nth(1)?;
    let token = rest
        .trim_start_matches('"')
        .chars()
        .take_while(|character| character.is_ascii_digit())
        .collect::<String>();
    token.parse().ok()
}

/// Antigravity 会用 `ELECTRON_RUN_AS_NODE=1` 复用 `Antigravity.exe` 跑独立 sidecar 脚本
/// （如 `--import=file:///.../sidecar_sdk/register_loader.js main.mjs`）。
/// 这类进程不是 Electron 浏览器进程，绝不能被误判为主进程或随接管被结束。
pub fn is_run_as_node_sidecar(command_line: Option<&str>) -> bool {
    let Some(line) = command_line else {
        return false;
    };
    let lower = line.to_ascii_lowercase();
    lower.contains("sidecar_sdk")
        || lower.contains("register_loader.js")
        || (lower.contains("--import=") && !lower.contains("--type="))
}

fn process_record(key: ProcessKey, command_line: Option<&str>) -> Option<ProcessRecord> {
    if is_run_as_node_sidecar(command_line) {
        return None;
    }
    let debug_port = command_line.and_then(parse_remote_debugging_port);
    let is_browser_main = command_line.is_some_and(|line| !line.contains("--type="));
    Some(ProcessRecord {
        key,
        has_debug_args: debug_port.is_some()
            || command_line.is_some_and(|line| line.contains("--remote-debugging-port=")),
        debug_port,
        is_browser_main,
    })
}

pub fn snapshot_executable_processes(executable: &str) -> Result<Vec<ProcessRecord>, String> {
    #[cfg(windows)]
    {
        windows_snapshot::snapshot_executable_processes(executable)
    }
    #[cfg(not(windows))]
    {
        let _ = executable;
        Ok(Vec::new())
    }
}

pub fn terminate_process(pid: u32) -> bool {
    #[cfg(windows)]
    {
        windows_snapshot::terminate_process(pid)
    }
    #[cfg(not(windows))]
    {
        let _ = pid;
        false
    }
}

fn target_names(executable: &str) -> Result<(String, String), String> {
    let target = normalize_executable_path(executable);
    let file_name = std::path::Path::new(&target)
        .file_name()
        .map(|name| name.to_string_lossy().to_ascii_lowercase())
        .ok_or_else(|| "目标可执行路径无效。".to_string())?;
    Ok((target, file_name))
}

pub enum Probe {
    Match(ProcessKey, Option<String>),
    Other,
    Unavailable,
}

pub trait ProcessSource {
    fn process_ids(&self) -> Result<Vec<u32>, String>;
    fn named_processes(&self, file_name: &str) -> Result<Vec<(u32, bool)>, String>;
    fn probe(&self, pid: u32, target: &str) -> Probe;
    fn still_matches(&self, pid: u32, target: &str, created_at: u64) -> bool;
    fn command_line(&self, pid: u32) -> Option<String>;
}

pub struct SystemProcesses;

const FULL_SCAN_INTERVAL: Duration = Duration::from_secs(10);

struct Tracked {
    key: ProcessKey,
    command_line: Option<String>,
}

pub struct ProcessTracker {
    target: String,
    known: HashMap<u32, Option<Tracked>>,
    last_full_scan: Option<Instant>,
}

impl Default for ProcessTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl ProcessTracker {
    pub fn new() -> Self {
        Self {
            target: String::new(),
            known: HashMap::new(),
            last_full_scan: None,
        }
    }

    pub fn scan(&mut self, executable: &str) -> Result<Vec<ProcessRecord>, String> {
        self.scan_with(&SystemProcesses, executable, Instant::now())
    }

    pub fn scan_with(
        &mut self,
        source: &impl ProcessSource,
        executable: &str,
        now: Instant,
    ) -> Result<Vec<ProcessRecord>, String> {
        let (target, file_name) = target_names(executable)?;
        if target != self.target {
            self.known.clear();
            self.last_full_scan = None;
            self.target = target;
        }
        let full_scan_due = self.last_full_scan.map_or(true, |last| {
            now.saturating_duration_since(last) >= FULL_SCAN_INTERVAL
        });
        if full_scan_due {
            self.full_scan(source, &file_name)?;
            self.last_full_scan = Some(now);
        } else {
            self.incremental_scan(source)?;
        }
        let mut records = Vec::new();
        for (pid, tracked) in &mut self.known {
            if let Some(tracked) = tracked {
                if tracked.command_line.is_none() {
                    tracked.command_line = source.command_line(*pid);
                }
                if let Some(record) = process_record(tracked.key, tracked.command_line.as_deref()) {
                    records.push(record);
                }
            }
        }
        records.sort_by_key(|record| record.key.pid);
        Ok(records)
    }

    fn full_scan(&mut self, source: &impl ProcessSource, file_name: &str) -> Result<(), String> {
        let processes = source.named_processes(file_name)?;
        let mut previous = std::mem::take(&mut self.known);
        for (pid, named) in processes {
            if !named {
                self.known.insert(pid, None);
                continue;
            }
            if let Some(Some(tracked)) = previous.remove(&pid) {
                if source.still_matches(pid, &self.target, tracked.key.created_at) {
                    self.known.insert(pid, Some(tracked));
                    continue;
                }
            }
            self.probe_into(source, pid);
        }
        Ok(())
    }

    fn incremental_scan(&mut self, source: &impl ProcessSource) -> Result<(), String> {
        let ids = source.process_ids()?;
        let live: HashSet<u32> = ids.iter().copied().collect();
        let target = &self.target;
        self.known.retain(|pid, tracked| {
            live.contains(pid)
                && tracked.as_ref().map_or(true, |tracked| {
                    source.still_matches(*pid, target, tracked.key.created_at)
                })
        });
        for pid in ids {
            if !self.known.contains_key(&pid) {
                self.probe_into(source, pid);
            }
        }
        Ok(())
    }

    fn probe_into(&mut self, source: &impl ProcessSource, pid: u32) {
        match source.probe(pid, &self.target) {
            Probe::Match(key, command_line) => {
                self.known.insert(pid, Some(Tracked { key, command_line }));
            }
            Probe::Other => {
                self.known.insert(pid, None);
            }
            Probe::Unavailable => {}
        }
    }
}

#[cfg(windows)]
impl ProcessSource for SystemProcesses {
    fn process_ids(&self) -> Result<Vec<u32>, String> {
        windows_snapshot::process_ids()
    }

    fn named_processes(&self, file_name: &str) -> Result<Vec<(u32, bool)>, String> {
        let mut processes = Vec::new();
        windows_snapshot::for_each_process(|pid, name| {
            processes.push((pid, name.eq_ignore_ascii_case(file_name)));
        })?;
        Ok(processes)
    }

    fn probe(&self, pid: u32, target: &str) -> Probe {
        let Some(process) = windows_snapshot::open_process(pid) else {
            return Probe::Other;
        };
        let Some(image) = windows_snapshot::image_path(&process) else {
            return Probe::Unavailable;
        };
        if normalize_executable_path(&image) != target {
            return Probe::Other;
        }
        match windows_snapshot::creation_time(&process) {
            Some(created_at) => Probe::Match(
                ProcessKey { pid, created_at },
                windows_snapshot::command_line(&process),
            ),
            None => Probe::Unavailable,
        }
    }

    fn still_matches(&self, pid: u32, target: &str, created_at: u64) -> bool {
        let Some(process) = windows_snapshot::open_process(pid) else {
            return false;
        };
        windows_snapshot::image_path(&process)
            .is_some_and(|image| normalize_executable_path(&image) == target)
            && windows_snapshot::creation_time(&process) == Some(created_at)
    }

    fn command_line(&self, pid: u32) -> Option<String> {
        windows_snapshot::command_line(&windows_snapshot::open_process(pid)?)
    }
}

#[cfg(not(windows))]
impl ProcessSource for SystemProcesses {
    fn process_ids(&self) -> Result<Vec<u32>, String> {
        Ok(Vec::new())
    }
    fn named_processes(&self, _file_name: &str) -> Result<Vec<(u32, bool)>, String> {
        Ok(Vec::new())
    }
    fn probe(&self, _pid: u32, _target: &str) -> Probe {
        Probe::Unavailable
    }
    fn still_matches(&self, _pid: u32, _target: &str, _created_at: u64) -> bool {
        false
    }
    fn command_line(&self, _pid: u32) -> Option<String> {
        None
    }
}

#[cfg(windows)]
mod windows_snapshot {
    use super::{
        normalize_executable_path, process_record, target_names, ProcessKey, ProcessRecord,
    };
    use windows_sys::Win32::Foundation::{CloseHandle, FILETIME, HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };
    use windows_sys::Win32::System::ProcessStatus::K32EnumProcesses;
    use windows_sys::Win32::System::Threading::{
        GetProcessTimes, OpenProcess, QueryFullProcessImageNameW, TerminateProcess,
        PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE,
    };

    const PROCESS_COMMAND_LINE_INFORMATION: u32 = 60;

    #[repr(C)]
    struct UnicodeString {
        length: u16,
        maximum_length: u16,
        buffer: *const u16,
    }

    #[link(name = "ntdll")]
    extern "system" {
        fn NtQueryInformationProcess(
            process_handle: HANDLE,
            information_class: u32,
            process_information: *mut core::ffi::c_void,
            process_information_length: u32,
            return_length: *mut u32,
        ) -> i32;
    }

    pub struct HandleGuard(HANDLE);

    impl Drop for HandleGuard {
        fn drop(&mut self) {
            if !self.0.is_null() && self.0 != INVALID_HANDLE_VALUE {
                unsafe {
                    CloseHandle(self.0);
                }
            }
        }
    }

    fn utf16_to_string(buffer: &[u16]) -> String {
        let end = buffer
            .iter()
            .position(|unit| *unit == 0)
            .unwrap_or(buffer.len());
        String::from_utf16_lossy(&buffer[..end])
    }

    fn query_image_path(handle: HANDLE) -> Option<String> {
        let mut buffer = [0u16; 1024];
        let mut size = buffer.len() as u32;
        let ok = unsafe { QueryFullProcessImageNameW(handle, 0, buffer.as_mut_ptr(), &mut size) };
        if ok == 0 || size == 0 {
            return None;
        }
        Some(String::from_utf16_lossy(&buffer[..size as usize]))
    }

    fn query_creation_time(handle: HANDLE) -> Option<u64> {
        let mut creation = FILETIME {
            dwLowDateTime: 0,
            dwHighDateTime: 0,
        };
        let mut exit = creation;
        let mut kernel = creation;
        let mut user = creation;
        let ok =
            unsafe { GetProcessTimes(handle, &mut creation, &mut exit, &mut kernel, &mut user) };
        if ok == 0 {
            return None;
        }
        Some(((creation.dwHighDateTime as u64) << 32) | u64::from(creation.dwLowDateTime))
    }

    fn query_command_line(handle: HANDLE) -> Option<String> {
        let mut needed = 0u32;
        unsafe {
            NtQueryInformationProcess(
                handle,
                PROCESS_COMMAND_LINE_INFORMATION,
                std::ptr::null_mut(),
                0,
                &mut needed,
            );
        }
        let size = if needed == 0 { 8192 } else { needed.max(16) };
        let mut buffer = vec![0u8; size as usize];
        let mut returned = 0u32;
        let status = unsafe {
            NtQueryInformationProcess(
                handle,
                PROCESS_COMMAND_LINE_INFORMATION,
                buffer.as_mut_ptr().cast(),
                buffer.len() as u32,
                &mut returned,
            )
        };
        if status != 0 || buffer.len() < std::mem::size_of::<UnicodeString>() {
            return None;
        }
        let header = unsafe { &*(buffer.as_ptr() as *const UnicodeString) };
        if header.buffer.is_null() || header.length == 0 {
            return None;
        }
        let units = (header.length as usize) / 2;
        let slice = unsafe { std::slice::from_raw_parts(header.buffer, units) };
        Some(String::from_utf16_lossy(slice))
    }

    pub fn open_process(pid: u32) -> Option<HandleGuard> {
        if pid == 0 {
            return None;
        }
        let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        if handle.is_null() || handle == INVALID_HANDLE_VALUE {
            return None;
        }
        Some(HandleGuard(handle))
    }

    pub fn terminate_process(pid: u32) -> bool {
        if pid == 0 {
            return false;
        }
        let handle = unsafe { OpenProcess(PROCESS_TERMINATE, 0, pid) };
        if handle.is_null() || handle == INVALID_HANDLE_VALUE {
            return false;
        }
        let _guard = HandleGuard(handle);
        unsafe { TerminateProcess(handle, 0) != 0 }
    }

    pub fn image_path(process: &HandleGuard) -> Option<String> {
        query_image_path(process.0)
    }

    pub fn creation_time(process: &HandleGuard) -> Option<u64> {
        query_creation_time(process.0)
    }

    pub fn command_line(process: &HandleGuard) -> Option<String> {
        query_command_line(process.0)
    }

    pub fn for_each_process(mut visit: impl FnMut(u32, &str)) -> Result<(), String> {
        let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
        if snapshot == INVALID_HANDLE_VALUE || snapshot.is_null() {
            return Err("无法创建进程快照。".to_string());
        }
        let _guard = HandleGuard(snapshot);
        let mut entry = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            cntUsage: 0,
            th32ProcessID: 0,
            th32DefaultHeapID: 0,
            th32ModuleID: 0,
            cntThreads: 0,
            th32ParentProcessID: 0,
            pcPriClassBase: 0,
            dwFlags: 0,
            szExeFile: [0; 260],
        };
        let mut ok = unsafe { Process32FirstW(snapshot, &mut entry) };
        while ok != 0 {
            visit(entry.th32ProcessID, &utf16_to_string(&entry.szExeFile));
            ok = unsafe { Process32NextW(snapshot, &mut entry) };
        }
        Ok(())
    }

    pub fn process_ids() -> Result<Vec<u32>, String> {
        let mut ids = vec![0u32; 1024];
        loop {
            let capacity = (ids.len() * std::mem::size_of::<u32>()) as u32;
            let mut written = 0u32;
            if unsafe { K32EnumProcesses(ids.as_mut_ptr(), capacity, &mut written) } == 0 {
                return Err("无法枚举进程。".to_string());
            }
            if written < capacity {
                ids.truncate(written as usize / std::mem::size_of::<u32>());
                return Ok(ids);
            }
            ids.resize(ids.len() * 2, 0);
        }
    }

    pub fn snapshot_executable_processes(executable: &str) -> Result<Vec<ProcessRecord>, String> {
        let (target, target_name) = target_names(executable)?;
        let mut records = Vec::new();
        for_each_process(|pid, name| {
            if !name.eq_ignore_ascii_case(&target_name) {
                return;
            }
            let Some(process) = open_process(pid) else {
                return;
            };
            if let (Some(image), Some(created_at)) = (image_path(&process), creation_time(&process))
            {
                if normalize_executable_path(&image) == target {
                    if let Some(record) = process_record(
                        ProcessKey { pid, created_at },
                        command_line(&process).as_deref(),
                    ) {
                        records.push(record);
                    }
                }
            }
        })?;
        Ok(records)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters_run_as_node_sidecar_processes() {
        assert!(is_run_as_node_sidecar(Some(
            r#""C:\Users\Me\AppData\Local\Programs\antigravity\Antigravity.exe" --import=file:///C:/Users/Me/.gemini/antigravity/lib/node_modules/sidecar_sdk/register_loader.js main.mjs"#
        )));
        assert!(!is_run_as_node_sidecar(Some(
            r#""C:\Users\Me\AppData\Local\Programs\antigravity\Antigravity.exe" --remote-debugging-port=9257"#
        )));
        assert!(!is_run_as_node_sidecar(Some(
            r#""C:\Users\Me\AppData\Local\Programs\antigravity\Antigravity.exe" --type=renderer"#
        )));
        assert!(process_record(
            ProcessKey {
                pid: 12,
                created_at: 100
            },
            Some(r#""Antigravity.exe" --import=file:///x/sidecar_sdk/register_loader.js main.mjs"#),
        )
        .is_none());
        let main = process_record(
            ProcessKey {
                pid: 13,
                created_at: 101,
            },
            Some(r#""Antigravity.exe" --remote-debugging-port=9257"#),
        )
        .unwrap();
        assert!(main.is_browser_main);
        assert_eq!(main.debug_port, Some(9257));
    }

    #[test]
    fn existing_processes_report_unmanaged_until_takeover() {
        let mut watcher = WatcherState::new();
        let obs = Observation {
            processes: vec![ProcessRecord::new(10, 1)],
            ..Observation::default()
        };
        assert_eq!(watcher.decide(&obs), WatcherAction::ReportExistingUnmanaged);
    }

    #[test]
    fn new_launch_after_empty_triggers_takeover() {
        let mut watcher = WatcherState::new();
        assert_eq!(watcher.decide(&Observation::default()), WatcherAction::Wait);
        let obs = Observation {
            processes: vec![ProcessRecord::new(20, 2)],
            ..Observation::default()
        };
        assert_eq!(watcher.decide(&obs), WatcherAction::Wait);
        assert_eq!(watcher.decide(&obs), WatcherAction::Takeover);
    }
}
