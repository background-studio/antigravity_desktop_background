use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};

use serde::Serialize;
use serde_json::{json, Value};

use crate::{
    controller::AntigravityController,
    lock,
    managed_launch::{
        confirm_launch_after_encode, keys_of, process_key_list, BusyGuard, ConfirmedLaunch,
        WatcherAction, WatcherState, MSG_BUSY, MSG_EXISTING, MSG_UNCONFIGURED,
    },
    media::download_configured_media,
    models::RuntimeStatus,
    payload::{build_active_payload_from_bytes, ActivePayload, PayloadBundle},
    plugin::{hello_result, PLUGIN_ID, PLUGIN_PROTOCOL},
    protocol::{parse_configure, ConfigureSpec},
};

pub fn data_directory() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("AntigravityBackgroundStudio")
}

#[derive(Clone)]
struct ConfiguredSession {
    revision: String,
    bundle: PayloadBundle,
}

fn update_configured_session(
    session: &mut Option<ConfiguredSession>,
    spec: &ConfigureSpec,
    payload: ActivePayload,
) -> Result<PayloadBundle, String> {
    if let Some(target_id) = &spec.target_id {
        let current = session
            .as_mut()
            .filter(|item| item.bundle.independent_windows)
            .ok_or_else(|| "请先配置独立窗口模式。".to_string())?;
        current.bundle.windows.insert(target_id.clone(), payload);
        Ok(current.bundle.clone())
    } else {
        let windows = session
            .as_mut()
            .filter(|item| item.bundle.independent_windows && spec.independent_windows)
            .map(|item| std::mem::take(&mut item.bundle.windows))
            .unwrap_or_default();
        let bundle = PayloadBundle {
            default: payload,
            independent_windows: spec.independent_windows,
            windows,
        };
        *session = Some(ConfiguredSession {
            revision: spec.revision.clone(),
            bundle: bundle.clone(),
        });
        Ok(bundle)
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct StatusResult {
    plugin_protocol: u32,
    plugin_id: &'static str,
    version: &'static str,
    phase: String,
    message: String,
    active_targets: u32,
    paused: bool,
    configured: bool,
    revision: Option<String>,
    independent_windows: bool,
    target_ids: Vec<String>,
    configured_target_ids: Vec<String>,
}

pub struct WorkerState {
    controller: Arc<Mutex<AntigravityController>>,
    runtime_status: Mutex<RuntimeStatus>,
    watcher: Mutex<WatcherState>,
    managed_busy: AtomicBool,
    configured: Mutex<Option<ConfiguredSession>>,
    shutting_down: AtomicBool,
}

impl WorkerState {
    pub fn load() -> Result<Self, String> {
        Self::load_from(data_directory())
    }

    pub fn load_from(data_directory: PathBuf) -> Result<Self, String> {
        std::fs::create_dir_all(&data_directory).map_err(|error| error.to_string())?;
        let controller = AntigravityController::load(&data_directory);
        let runtime_status = controller.status();
        Ok(Self {
            controller: Arc::new(Mutex::new(controller)),
            runtime_status: Mutex::new(runtime_status),
            watcher: Mutex::new(WatcherState::new()),
            managed_busy: AtomicBool::new(false),
            configured: Mutex::new(None),
            shutting_down: AtomicBool::new(false),
        })
    }

    pub fn is_shutting_down(&self) -> bool {
        self.shutting_down.load(Ordering::SeqCst)
    }

    pub fn is_configured(&self) -> Result<bool, String> {
        Ok(lock(&self.configured)?.is_some())
    }

    pub fn rearm_managed_watcher(&self) -> Result<(), String> {
        lock(&self.watcher)?.rearm();
        lock(&self.controller)?.set_watcher_paused(false);
        Ok(())
    }

    pub fn suspend_managed_watcher(&self) -> Result<(), String> {
        lock(&self.watcher)?.suspend();
        lock(&self.controller)?.set_watcher_paused(true);
        Ok(())
    }

    pub fn sync_managed_active(&self) -> Result<(), String> {
        let probe = lock(&self.controller)?.probe_managed()?;
        lock(&self.watcher)?.mark_managed_active(&process_key_list(&probe.processes));
        Ok(())
    }

    pub fn sync_managed_failure(&self) -> Result<(), String> {
        let processes = lock(&self.controller)?
            .probe_managed()
            .map(|probe| process_key_list(&probe.processes))
            .unwrap_or_default();
        lock(&self.watcher)?.takeover_failed(&processes);
        Ok(())
    }

    pub fn runtime_status(&self) -> Result<RuntimeStatus, String> {
        if let Ok(controller) = self.controller.try_lock() {
            let status = controller.status();
            *lock(&self.runtime_status)? = status.clone();
            self.prune_closed_window_configs(&status.target_ids);
            return Ok(status);
        }
        Ok(lock(&self.runtime_status)?.clone())
    }

    pub fn refresh_runtime_status(&self) -> Result<RuntimeStatus, String> {
        let status = lock(&self.controller)?.status();
        *lock(&self.runtime_status)? = status.clone();
        self.prune_closed_window_configs(&status.target_ids);
        Ok(status)
    }

    fn prune_closed_window_configs(&self, live_target_ids: &[String]) {
        if let Ok(mut guard) = self.configured.lock() {
            if let Some(session) = guard.as_mut() {
                if session.bundle.independent_windows && !session.bundle.windows.is_empty() {
                    if live_target_ids.is_empty() {
                        // 目标已退出或未连接时暂不清空？按协议：窗口关闭后释放对应配置槽。
                        // 仅当已知当前连接正常且 live_target_ids 非空或已回到 idle 时释放。
                    } else {
                        session
                            .bundle
                            .windows
                            .retain(|id, _| live_target_ids.contains(id));
                    }
                }
            }
        }
    }

    fn clear_window_configs(&self) {
        if let Ok(mut guard) = self.configured.lock() {
            if let Some(session) = guard.as_mut() {
                session.bundle.windows.clear();
            }
        }
    }

    fn active_bundle(&self) -> Result<PayloadBundle, String> {
        lock(&self.configured)?
            .as_ref()
            .map(|configured| configured.bundle.clone())
            .ok_or_else(|| MSG_UNCONFIGURED.to_string())
    }

    pub fn status_value(&self) -> Result<Value, String> {
        let status = self.runtime_status()?;
        let (configured, revision, independent_windows, mut configured_target_ids) = {
            let guard = lock(&self.configured)?;
            match guard.as_ref() {
                Some(session) => (
                    true,
                    Some(session.revision.clone()),
                    session.bundle.independent_windows,
                    session.bundle.windows.keys().cloned().collect::<Vec<_>>(),
                ),
                None => (false, None, false, Vec::new()),
            }
        };
        configured_target_ids.sort();
        let message = if status.phase == "active"
            && independent_windows
            && status.active_targets == 0
            && !status.target_ids.is_empty()
        {
            format!(
                "已发现 {} 个 Antigravity 窗口，等待宿主分配独立背景",
                status.target_ids.len()
            )
        } else {
            status.message
        };
        let payload = StatusResult {
            plugin_protocol: PLUGIN_PROTOCOL,
            plugin_id: PLUGIN_ID,
            version: env!("CARGO_PKG_VERSION"),
            phase: status.phase.clone(),
            message,
            active_targets: status.active_targets,
            paused: status.phase == "paused",
            configured,
            revision,
            independent_windows,
            target_ids: status.target_ids,
            configured_target_ids,
        };
        serde_json::to_value(payload).map_err(|error| error.to_string())
    }

    pub fn hello() -> Value {
        hello_result()
    }

    pub async fn configure(self: &Arc<Self>, params: Value) -> Result<Value, String> {
        let spec = parse_configure(&params)?;
        let bytes = download_configured_media(&spec).await?;
        let state = Arc::clone(self);
        tokio::task::spawn_blocking(move || {
            let payload = build_active_payload_from_bytes(
                bytes,
                &spec.media.kind,
                &spec.media.mime_type,
                &spec.display,
            )?;
            state.store_and_maybe_hot_update_blocking(spec, payload)?;
            state.status_value()
        })
        .await
        .map_err(|error| error.to_string())?
    }

    fn store_and_maybe_hot_update_blocking(
        &self,
        spec: ConfigureSpec,
        payload: ActivePayload,
    ) -> Result<(), String> {
        let bundle = {
            let mut configured = lock(&self.configured)?;
            update_configured_session(&mut configured, &spec, payload)?
        };
        // 保持 controller -> watcher 加锁顺序，杜绝 AB-BA 死锁。
        let processes = lock(&self.controller)
            .ok()
            .and_then(|mut controller| controller.probe_managed().ok())
            .map(|probe| process_key_list(&probe.processes))
            .unwrap_or_default();
        if let Ok(mut watcher) = lock(&self.watcher) {
            watcher.arm_after_configure(&processes);
        }
        if let Ok(mut controller) = lock(&self.controller) {
            if !controller.watcher_paused()
                && !matches!(controller.status().phase.as_str(), "active" | "paused")
            {
                controller.set_managed_status("idle", crate::managed_launch::MSG_WAITING);
            }
        }
        let phase = self.runtime_status()?.phase;
        if should_hot_update(&phase) {
            let Some(_busy) = BusyGuard::acquire(&self.managed_busy) else {
                return Err(MSG_BUSY.to_string());
            };
            let result = lock(&self.controller)?.apply(bundle, false);
            let _ = self.refresh_runtime_status();
            match result {
                Ok(_) => {
                    let _ = self.sync_managed_active();
                }
                Err(error) => {
                    let _ = self.sync_managed_failure();
                    return Err(error);
                }
            }
        } else {
            let reconnected = match lock(&self.controller) {
                Ok(mut controller) if !controller.watcher_paused() => {
                    let _ = controller.reconnect_saved(bundle);
                    true
                }
                _ => false,
            };
            if reconnected {
                let _ = self.refresh_runtime_status();
            }
        }
        let _ = self.refresh_runtime_status();
        Ok(())
    }

    pub fn apply_blocking(&self) -> Result<Value, String> {
        let Some(_busy) = BusyGuard::acquire(&self.managed_busy) else {
            return Err(MSG_BUSY.to_string());
        };
        let _ = self.rearm_managed_watcher();
        let bundle = match self.active_bundle() {
            Ok(bundle) => bundle,
            Err(error) => {
                let _ = self.sync_managed_failure();
                if let Ok(mut controller) = lock(&self.controller) {
                    controller.set_managed_status("idle", MSG_UNCONFIGURED);
                }
                let _ = self.refresh_runtime_status();
                return Err(error);
            }
        };
        let first = lock(&self.controller)?.apply(bundle.clone(), false);
        let _ = self.refresh_runtime_status();
        let result = match first {
            Ok(_) => Ok(()),
            Err(error) if error.contains("需要重启一次") => {
                let retry = lock(&self.controller)?.apply(bundle, true);
                let _ = self.refresh_runtime_status();
                retry.map(|_| ())
            }
            Err(error) => Err(error),
        };
        if result.is_ok() {
            let _ = self.sync_managed_active();
        } else {
            let _ = self.sync_managed_failure();
        }
        result?;
        self.status_value()
    }

    pub fn pause_blocking(&self) -> Result<Value, String> {
        let Some(_busy) = BusyGuard::acquire(&self.managed_busy) else {
            return Err(MSG_BUSY.to_string());
        };
        let _ = self.suspend_managed_watcher();
        lock(&self.controller)?.pause()?;
        let _ = self.refresh_runtime_status();
        self.status_value()
    }

    pub fn restore_blocking(&self) -> Result<Value, String> {
        let Some(_busy) = BusyGuard::acquire(&self.managed_busy) else {
            return Err(MSG_BUSY.to_string());
        };
        let _ = self.suspend_managed_watcher();
        lock(&self.controller)?.restore()?;
        self.clear_window_configs();
        let _ = self.refresh_runtime_status();
        self.status_value()
    }

    pub fn shutdown(&self) -> Result<Value, String> {
        self.shutting_down.store(true, Ordering::SeqCst);
        Ok(json!({ "shutdown": true, "keptTarget": true }))
    }

    pub fn tick_managed_launch(&self) -> Result<(), String> {
        if self.shutting_down.load(Ordering::SeqCst) || self.managed_busy.load(Ordering::Acquire) {
            return Ok(());
        }
        if !self.is_configured()? {
            if let Ok(mut controller) = self.controller.try_lock() {
                if !controller.watcher_paused() && controller.status().phase != "paused" {
                    controller.set_managed_status("idle", MSG_UNCONFIGURED);
                }
            }
            let _ = self.refresh_runtime_status();
            return Ok(());
        }
        let mut controller = match self.controller.try_lock() {
            Ok(controller) => controller,
            Err(_) => return Ok(()),
        };
        if controller.watcher_paused() {
            drop(controller);
            lock(&self.watcher)?.suspend();
            return Ok(());
        }
        let probe = controller.probe_managed()?;
        let action = {
            let mut watcher = lock(&self.watcher)?;
            watcher.decide(&probe.observation())
        };
        match action {
            WatcherAction::KeepActive => {
                drop(controller);
                let _ = self.refresh_runtime_status();
                Ok(())
            }
            WatcherAction::Suspend => Ok(()),
            WatcherAction::Wait
            | WatcherAction::ReportExistingUnmanaged
            | WatcherAction::WaitForDebugPort
            | WatcherAction::ReportDebugTimeout => {
                let before = controller.status();
                controller.apply_watcher_action_status(action);
                let changed = before.phase != controller.status().phase
                    || before.message != controller.status().message;
                drop(controller);
                if changed {
                    let _ = self.refresh_runtime_status();
                }
                Ok(())
            }
            WatcherAction::ReleaseAndWait => {
                controller.release_stale_session()?;
                drop(controller);
                self.clear_window_configs();
                let _ = self.refresh_runtime_status();
                Ok(())
            }
            WatcherAction::Attach | WatcherAction::Takeover => {
                let Some(_busy) = BusyGuard::acquire(&self.managed_busy) else {
                    return Ok(());
                };
                let original_keys = keys_of(&probe.processes);
                controller.apply_watcher_action_status(action);
                drop(controller);
                let _ = self.refresh_runtime_status();
                let restart = action == WatcherAction::Takeover;
                let result: Result<bool, String> = (|| {
                    let bundle = self.active_bundle()?;
                    let mut controller = lock(&self.controller)?;
                    let after = controller.probe_managed()?;
                    let confirmed =
                        confirm_launch_after_encode(action, &original_keys, &after.observation());
                    match confirmed {
                        ConfirmedLaunch::Attach => {
                            controller.attach_without_restart(bundle)?;
                            Ok(true)
                        }
                        ConfirmedLaunch::Takeover => {
                            controller.auto_takeover(bundle)?;
                            Ok(true)
                        }
                        ConfirmedLaunch::CancelExited | ConfirmedLaunch::CancelStale => {
                            if confirmed == ConfirmedLaunch::CancelExited {
                                let _ = controller.release_stale_session();
                                self.clear_window_configs();
                            }
                            let observation = after.observation();
                            drop(controller);
                            let next = lock(&self.watcher)?.sync_to_current(&observation);
                            if let Ok(mut controller) = lock(&self.controller) {
                                controller.apply_watcher_action_status(next);
                            }
                            Ok(false)
                        }
                    }
                })();
                match result {
                    Ok(true) => {
                        let _ = self.sync_managed_active();
                        let _ = self.refresh_runtime_status();
                        Ok(())
                    }
                    Ok(false) => {
                        let _ = self.refresh_runtime_status();
                        Ok(())
                    }
                    Err(error) if error.contains(MSG_UNCONFIGURED) => {
                        let _ = self.sync_managed_failure();
                        if let Ok(mut controller) = lock(&self.controller) {
                            controller.set_managed_status("idle", MSG_UNCONFIGURED);
                        }
                        let _ = self.refresh_runtime_status();
                        Ok(())
                    }
                    Err(error) if !restart && error.contains("需要重启一次") => {
                        let _ = self.sync_managed_failure();
                        if let Ok(mut controller) = lock(&self.controller) {
                            controller.set_managed_status("idle", MSG_EXISTING);
                        }
                        let _ = self.refresh_runtime_status();
                        Ok(())
                    }
                    Err(error) => {
                        let _ = self.sync_managed_failure();
                        let _ = self.refresh_runtime_status();
                        Err(error)
                    }
                }
            }
        }
    }
}

pub fn should_hot_update(phase: &str) -> bool {
    phase == "active"
}

pub fn start_managed_launch_worker(state: Arc<WorkerState>) {
    tokio::spawn(async move {
        loop {
            if state.shutting_down.load(Ordering::SeqCst) {
                break;
            }
            let tick_state = Arc::clone(&state);
            let outcome =
                tokio::task::spawn_blocking(move || tick_state.tick_managed_launch()).await;
            match outcome {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    if let Ok(mut controller) = lock(&state.controller) {
                        let status = controller.status();
                        if status.phase != "error" || status.message != error {
                            controller.set_managed_error(error);
                        }
                    }
                    let _ = state.refresh_runtime_status();
                    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                    continue;
                }
                Err(error) => {
                    eprintln!("托管探测任务失败：{error}");
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{DisplaySettings, MediaKind};
    use crate::protocol::hex_sha256;
    use serde_json::json;
    use std::io::Write;
    use std::net::TcpListener;
    use std::thread;
    use uuid::Uuid;

    fn temp_state() -> WorkerState {
        let dir = std::env::temp_dir().join(format!("antigravity-worker-{}", Uuid::new_v4()));
        WorkerState::load_from(dir).unwrap()
    }

    fn serve_body(body: &[u8]) -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let body = body.to_vec();
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 1024];
            let _ = std::io::Read::read(&mut stream, &mut request);
            let header = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            stream.write_all(header.as_bytes()).unwrap();
            stream.write_all(&body).unwrap();
        });
        port
    }

    #[test]
    fn targeted_updates_are_atomic_and_preserve_other_windows() {
        let payload = |tag: &[u8]| {
            build_active_payload_from_bytes(
                tag.to_vec(),
                &MediaKind::Image,
                "image/png",
                &DisplaySettings::default(),
            )
            .unwrap()
        };
        let mut spec = parse_configure(&json!({
            "schemaVersion": 1,
            "revision": "base",
            "independentWindows": true,
            "media": {
                "url": "http://127.0.0.1:9/x",
                "kind": "image",
                "mimeType": "image/png",
                "sha256": "a".repeat(64),
                "byteSize": 1
            },
            "display": {}
        }))
        .unwrap();
        let mut session = None;
        spec.target_id = Some("WIN_A".to_string());
        assert!(update_configured_session(&mut session, &spec, payload(b"a")).is_err());

        spec.target_id = None;
        update_configured_session(&mut session, &spec, payload(b"base")).unwrap();
        spec.target_id = Some("WIN_A".to_string());
        update_configured_session(&mut session, &spec, payload(b"a1")).unwrap();
        spec.target_id = Some("WIN_B".to_string());
        update_configured_session(&mut session, &spec, payload(b"b1")).unwrap();

        let rev_b = session.as_ref().unwrap().bundle.windows["WIN_B"]
            .revision
            .clone();
        spec.target_id = Some("WIN_A".to_string());
        update_configured_session(&mut session, &spec, payload(b"a2")).unwrap();
        assert_eq!(
            session.as_ref().unwrap().bundle.windows["WIN_B"].revision,
            rev_b
        );

        // 基础 configure 不覆盖已分配的窗口
        spec.target_id = None;
        update_configured_session(&mut session, &spec, payload(b"base2")).unwrap();
        assert_eq!(session.as_ref().unwrap().bundle.windows.len(), 2);
        assert_eq!(
            session.as_ref().unwrap().bundle.windows["WIN_B"].revision,
            rev_b
        );

        // 切回非独立模式时清空定向窗口配置
        spec.independent_windows = false;
        update_configured_session(&mut session, &spec, payload(b"legacy")).unwrap();
        assert!(session.as_ref().unwrap().bundle.windows.is_empty());
    }

    #[tokio::test]
    async fn configure_stores_revision_and_independent_flag() {
        let state = Arc::new(temp_state());
        let body = b"worker-png";
        let port = serve_body(body);
        let status = state
            .configure(json!({
                "schemaVersion": 1,
                "revision": "rev-hot",
                "independentWindows": true,
                "media": {
                    "url": format!("http://127.0.0.1:{port}/a.png"),
                    "kind": "image",
                    "mimeType": "image/png",
                    "sha256": hex_sha256(body),
                    "byteSize": body.len()
                },
                "display": { "opacity": 0.5 }
            }))
            .await
            .unwrap();
        assert_eq!(status["configured"], true);
        assert_eq!(status["revision"], "rev-hot");
        assert_eq!(status["independentWindows"], true);
        assert_eq!(status["targetIds"], json!([]));
        assert_eq!(status["configuredTargetIds"], json!([]));
    }

    #[test]
    fn pause_restore_and_shutdown() {
        let state = temp_state();
        let paused = state.pause_blocking().unwrap();
        assert_eq!(paused["paused"], true);
        assert_eq!(paused["phase"], "paused");

        let restored = state.restore_blocking().unwrap();
        assert_eq!(restored["phase"], "idle");
        assert_eq!(restored["paused"], false);

        let shutdown = state.shutdown().unwrap();
        assert_eq!(shutdown["shutdown"], true);
        assert_eq!(shutdown["keptTarget"], true);
        assert!(state.is_shutting_down());
    }
}
