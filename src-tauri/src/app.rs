//! Application state and Tauri wiring.
//!
//! `AppState` is the single place that owns live sessions, cached agent probe
//! results and the local store. Every event coming back from an agent is
//! persisted through `record_event` before the UI is notified, so what the
//! window shows and what is on disk cannot drift apart.

use crate::acp::{LiveSession, PermissionRegistry, SessionSpec};
use crate::agents;
use crate::model::{
    now_iso, AgentReport, AgentStatus, EventEnvelope, Message, MessageBody, PermissionChoice,
    SessionEvent, StoreState, Task, TaskStatus,
};
use crate::store::Store;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

pub struct AppState {
    /// Shared so a `'static` event recorder can be handed to agent sessions
    /// without keeping a reference to the whole application state.
    pub store: Arc<Store>,
    pub state: Arc<Mutex<StoreState>>,
    pub open_streams: Arc<Mutex<HashMap<String, String>>>,
    pub app: Arc<Mutex<Option<tauri::AppHandle>>>,
    pub data_dir: PathBuf,
    pub sessions: Mutex<HashMap<String, Arc<LiveSession>>>,
    pub permissions: PermissionRegistry,
    /// Cached results of real ACP handshakes, keyed by agent id.
    pub probe_results: Mutex<HashMap<String, AgentReport>>,
    /// Run ids that currently have a driver worker alive.
    pub run_workers: Mutex<std::collections::HashSet<String>>,
    /// Recent driver activity per run, so the UI can show what happened even
    /// when the transcript lives in another agent's session.
    pub run_logs: Mutex<HashMap<String, Vec<String>>>,
}

impl AppState {
    pub fn new(data_dir: PathBuf) -> Self {
        let store = Arc::new(Store::new(&data_dir));
        let mut state = store.load_state();
        store.rebuild_index(&mut state, &[]);
        Self {
            store,
            data_dir,
            state: Arc::new(Mutex::new(state)),
            open_streams: Arc::new(Mutex::new(HashMap::new())),
            app: Arc::new(Mutex::new(None)),
            sessions: Mutex::new(HashMap::new()),
            permissions: PermissionRegistry::default(),
            probe_results: Mutex::new(HashMap::new()),
            run_workers: Mutex::new(std::collections::HashSet::new()),
            run_logs: Mutex::new(HashMap::new()),
        }
    }

    pub fn set_app(&self, handle: tauri::AppHandle) {
        if let Ok(mut guard) = self.app.lock() {
            *guard = Some(handle);
        }
    }

    /// Append a line to a run's activity log (bounded, newest last).
    pub fn record_run_log(&self, run_id: &str, message: &str) {
        if let Ok(mut logs) = self.run_logs.lock() {
            let entry = logs.entry(run_id.to_string()).or_default();
            entry.push(format!("{} {}", now_iso(), message));
            let len = entry.len();
            if len > 500 {
                entry.drain(0..len - 500);
            }
        }
    }

    pub fn run_log(&self, run_id: &str) -> Vec<String> {
        self.run_logs
            .lock()
            .ok()
            .and_then(|logs| logs.get(run_id).cloned())
            .unwrap_or_default()
    }

    /// Notify the UI that something about a run changed.
    pub fn emit_run_event(&self, run_id: &str, message: &str) -> Result<(), String> {
        if let Ok(guard) = self.app.lock() {
            if let Some(app) = guard.as_ref() {
                use tauri::Emitter;
                app.emit(
                    "lj://run-event",
                    serde_json::json!({ "run_id": run_id, "message": message }),
                )
                .map_err(|e| e.to_string())?;
            }
        }
        Ok(())
    }

    /// On start-up, reconcile every stored run so a crash cannot leave a step
    /// looking like it is still executing, and surface what needs a human.
    pub fn reconcile_runs(&self) -> Vec<String> {
        let store = crate::runs::RunStore::new(&self.data_dir);
        let (runs, problems) = store.list();
        let mut notes: Vec<String> = problems
            .iter()
            .map(|p| format!("运行记录 {} 无法解析：{}", p.run_id, p.detail))
            .collect();
        for mut run in runs {
            if matches!(run.status, crate::scheduler::RunStatus::Running) {
                let before = run.status;
                let step_notes = crate::runs::RunStore::reconcile_after_restart(&mut run);
                if !step_notes.is_empty() {
                    if let Err(e) = store.save(&run) {
                        notes.push(format!("运行 {} 保存失败：{e}", run.id));
                    } else {
                        notes.push(format!(
                            "运行「{}」上次处于 {:?}，已按中断处理（需人工确认 {} 个步骤）",
                            run.title,
                            before,
                            step_notes.len()
                        ));
                    }
                }
            }
        }
        notes
    }

    pub fn refresh_index(&self) {
        let live: Vec<String> = self
            .sessions
            .lock()
            .map(|s| s.keys().cloned().collect())
            .unwrap_or_default();
        if let Ok(mut state) = self.state.lock() {
            self.store.rebuild_index(&mut state, &live);
            let _ = self.store.save_state(&state);
        }
    }

    pub fn load_task(&self, task_id: &str) -> Option<Task> {
        self.store.load_task(task_id)
    }

    pub fn save_task(&self, task: &Task) {
        if self.store.save_task(task).is_ok() {
            self.refresh_index();
        }
    }

    /// A `'static` handle that can persist session events. Handed to agent
    /// session callbacks so they never borrow the application state.
    pub fn recorder(&self) -> EventRecorder {
        EventRecorder {
            store: self.store.clone(),
            state: self.state.clone(),
            open_streams: self.open_streams.clone(),
            app: self.app.clone(),
        }
    }

    /// Persist an event as session history and notify the UI.
    pub fn record_event(&self, task_id: &str, event: SessionEvent) {
        self.recorder().record_event(task_id, event);
    }

    pub fn is_live(&self, task_id: &str) -> bool {
        self.sessions
            .lock()
            .map(|s| s.get(task_id).map(|l| l.is_alive()).unwrap_or(false))
            .unwrap_or(false)
    }

    /// Record the user's own message immediately, so the transcript shows what
    /// was asked even when the agent fails or the app is closed mid-turn.
    pub fn record_user_message(&self, task_id: &str, text: &str) {
        if let Some(mut task) = self.store.load_task(task_id) {
            task.messages.push(Message {
                id: crate::model::new_id("msg"),
                role: "user".into(),
                body: MessageBody::User {
                    text: text.to_string(),
                },
                created_at: now_iso(),
            });
            task.updated_at = now_iso();
            let _ = self.store.save_task(&task);
            self.refresh_index();
        }
    }

    pub fn stop_session(&self, task_id: &str) {
        if let Ok(mut sessions) = self.sessions.lock() {
            if let Some(session) = sessions.remove(task_id) {
                session.shutdown();
                session.close_pipeline();
            }
        }
        self.refresh_index();
    }

    /// Resolve how an agent should be launched on this machine.
    pub fn launch_spec(&self, agent_id: &str) -> Result<crate::commands::LaunchSpec, String> {
        let def = agents::find_definition(agent_id)
            .ok_or_else(|| format!("未登记的 Agent：{agent_id}"))?;
        let over = self
            .state
            .lock()
            .ok()
            .and_then(|s| s.settings.agent_overrides.get(agent_id).cloned());

        if def.transport == crate::model::TransportKind::OpenApiHttp {
            return Err(
                "WorkBuddy 通过官方 Open API 接入，不是本地 stdio Agent；请在设置中配置凭据后使用云端任务入口。"
                    .to_string(),
            );
        }

        let mut args: Vec<String> = def.acp_args.iter().map(|s| s.to_string()).collect();
        if let Some(o) = over.as_ref() {
            if !o.args.is_empty() {
                args = o.args.clone();
            }
        }

        let env: Vec<(String, String)> = over
            .as_ref()
            .map(|o| {
                o.env_passthrough
                    .iter()
                    .filter_map(|name| {
                        // Only names are configured by the user; the value comes
                        // from LJ Hub's own environment and is never stored.
                        std::env::var(name).ok().map(|value| (name.clone(), value))
                    })
                    .collect()
            })
            .unwrap_or_default();

        if let Some((path, overridden)) = agents::resolve_executable(def, over.as_ref()) {
            return Ok(crate::commands::LaunchSpec {
                executable: path.display().to_string(),
                args,
                display: if overridden {
                    format!("{}（用户自定义路径）", path.display())
                } else {
                    path.display().to_string()
                },
                via_npm_adapter: false,
                env,
            });
        }

        // Optional, explicitly enabled pinned npm adapter path.
        let allow_npm = over.as_ref().map(|o| o.allow_npm_adapter).unwrap_or(false);
        if allow_npm {
            if let Some(spec) = def.npm_adapter {
                if let Some(npx) = agents::which("npx") {
                    return Ok(crate::commands::LaunchSpec {
                        executable: npx.display().to_string(),
                        args: vec!["-y".to_string(), spec.to_string()],
                        display: format!("npx -y {spec}"),
                        via_npm_adapter: true,
                        env,
                    });
                }
            }
        }

        Err(match def.npm_adapter {
            Some(spec) => format!(
                "未找到 {agent_id} 的可执行文件。可在 Agent 设置中指定路径，或启用固定版本的官方适配器（npx -y {spec}）。"
            ),
            None => format!("未找到 {agent_id} 的可执行文件，请先安装官方 CLI。"),
        })
    }
}

/// Persists session events and notifies the UI. Holds only shared handles, so
/// it is `Send + Sync + 'static` and can be owned by an agent session thread.
#[derive(Clone)]
pub struct EventRecorder {
    store: Arc<Store>,
    state: Arc<Mutex<StoreState>>,
    open_streams: Arc<Mutex<HashMap<String, String>>>,
    app: Arc<Mutex<Option<tauri::AppHandle>>>,
}

impl EventRecorder {
    pub fn record_event(&self, task_id: &str, event: SessionEvent) {
        if let Some(mut task) = self.store.load_task(task_id) {
            if let Ok(mut streams) = self.open_streams.lock() {
                apply_event(&mut task, &event, &mut streams);
            }
            task.updated_at = now_iso();
            let _ = self.store.save_task(&task);
            self.refresh_index();
        }
        if let Ok(guard) = self.app.lock() {
            if let Some(app) = guard.as_ref() {
                use tauri::Emitter;
                let _ = app.emit(
                    "lj://session-event",
                    EventEnvelope {
                        task_id: task_id.to_string(),
                        event,
                    },
                );
            }
        }
    }

    pub fn emit_run_event(&self, run_id: &str, message: &str) -> Result<(), String> {
        if let Ok(guard) = self.app.lock() {
            if let Some(app) = guard.as_ref() {
                use tauri::Emitter;
                app.emit(
                    "lj://run-event",
                    serde_json::json!({ "run_id": run_id, "message": message }),
                )
                .map_err(|e| e.to_string())?;
            }
        }
        Ok(())
    }

    fn refresh_index(&self) {
        if let Ok(mut state) = self.state.lock() {
            self.store.rebuild_index(&mut state, &[]);
            let _ = self.store.save_state(&state);
        }
    }
}

/// Fold one session event into the stored task. Kept separate from `AppState`
/// so it is directly testable.
pub fn apply_event(
    task: &mut Task,
    event: &SessionEvent,
    open_streams: &mut HashMap<String, String>,
) {
    match event {
        SessionEvent::Chunk { stream, text } => {
            if text.is_empty() {
                return;
            }
            let key = format!("{}::{}", task.id, stream);
            let existing = open_streams
                .get(&key)
                .and_then(|id| task.messages.iter_mut().find(|m| &m.id == id));
            match existing {
                Some(message) => match &mut message.body {
                    MessageBody::Agent { text: t }
                    | MessageBody::Thought { text: t }
                    | MessageBody::User { text: t } => t.push_str(text),
                    _ => {}
                },
                None => {
                    let id = crate::model::new_id("msg");
                    let body = match stream.as_str() {
                        "thought" => MessageBody::Thought {
                            text: text.clone(),
                        },
                        "user" => MessageBody::User { text: text.clone() },
                        _ => MessageBody::Agent { text: text.clone() },
                    };
                    task.messages.push(Message {
                        id: id.clone(),
                        role: stream.clone(),
                        body,
                        created_at: now_iso(),
                    });
                    open_streams.insert(key, id);
                }
            }
        }
        SessionEvent::ToolCall { tool } => {
            if let MessageBody::Tool { tool_call_id, .. } = tool {
                if let Some(message) = task.messages.iter_mut().rev().find(|m| {
                    matches!(&m.body, MessageBody::Tool { tool_call_id: id, .. } if id == tool_call_id)
                }) {
                    message.body = tool.clone();
                    return;
                }
            }
            task.messages.push(Message {
                id: crate::model::new_id("msg"),
                role: "tool".into(),
                body: tool.clone(),
                created_at: now_iso(),
            });
        }
        SessionEvent::Plan { entries } => {
            task.messages.push(Message {
                id: crate::model::new_id("msg"),
                role: "plan".into(),
                body: MessageBody::Plan {
                    entries: entries.clone(),
                },
                created_at: now_iso(),
            });
        }
        SessionEvent::PermissionRequest {
            request_id,
            title,
            options,
        } => {
            task.messages.push(Message {
                id: crate::model::new_id("msg"),
                role: "permission".into(),
                body: MessageBody::Permission {
                    request_id: request_id.clone(),
                    title: title.clone(),
                    options: options.clone(),
                    outcome: "pending".into(),
                },
                created_at: now_iso(),
            });
        }
        SessionEvent::PermissionResolved {
            request_id,
            outcome,
        } => {
            if let Some(message) = task.messages.iter_mut().rev().find(|m| {
                matches!(&m.body, MessageBody::Permission { request_id: id, .. } if id == request_id)
            }) {
                if let MessageBody::Permission { outcome: o, .. } = &mut message.body {
                    *o = outcome.clone();
                }
            }
        }
        SessionEvent::Mode { mode } => {
            task.messages.push(Message {
                id: crate::model::new_id("msg"),
                role: "mode".into(),
                body: MessageBody::Mode { mode: mode.clone() },
                created_at: now_iso(),
            });
        }
        SessionEvent::Usage { text } => {
            task.messages.push(Message {
                id: crate::model::new_id("msg"),
                role: "usage".into(),
                body: MessageBody::Usage { text: text.clone() },
                created_at: now_iso(),
            });
        }
        SessionEvent::NativeSession {
            native_session_id,
            restored,
            detail,
        } => {
            task.native_session_id = Some(native_session_id.clone());
            task.native_session_restored = *restored;
            task.messages.push(Message {
                id: crate::model::new_id("msg"),
                role: "system".into(),
                body: MessageBody::SessionLink {
                    native_session_id: native_session_id.clone(),
                    restored: *restored,
                    detail: detail.clone(),
                },
                created_at: now_iso(),
            });
        }
        SessionEvent::Status { status, message } => {
            if status == "session_started" || status == "cancelling" {
                task.messages.push(Message {
                    id: crate::model::new_id("msg"),
                    role: "system".into(),
                    body: MessageBody::System {
                        text: message.clone(),
                    },
                    created_at: now_iso(),
                });
            }
        }
        SessionEvent::PromptDone { .. } => {
            open_streams.retain(|key, _| !key.starts_with(&format!("{}::", task.id)));
            if task.status == TaskStatus::Active {
                task.updated_at = now_iso();
            }
        }
        SessionEvent::Error { message } => {
            open_streams.retain(|key, _| !key.starts_with(&format!("{}::", task.id)));
            task.messages.push(Message {
                id: crate::model::new_id("msg"),
                role: "error".into(),
                body: MessageBody::Error {
                    text: message.clone(),
                },
                created_at: now_iso(),
            });
        }
    }
}

/// Build the session spec for a task, ensuring the agent is genuinely usable.
pub fn build_spec(
    state: &AppState,
    task: &Task,
    project_root: PathBuf,
) -> Result<SessionSpec, String> {
    let spec = state.launch_spec(&task.agent_id)?;
    // A session may only start when the agent actually passed a handshake, or
    // is at least installed (in which case the handshake happens now).
    let report = state
        .probe_results
        .lock()
        .ok()
        .and_then(|m| m.get(&task.agent_id).cloned());
    if let Some(report) = report {
        match report.status {
            AgentStatus::Available | AgentStatus::NotTested => {}
            other => {
                return Err(format!(
                    "{} 当前状态为 {:?}：{}",
                    report.display_name, other, report.message
                ))
            }
        }
    }
    Ok(SessionSpec {
        task_id: task.id.clone(),
        agent_id: task.agent_id.clone(),
        project_root,
        executable: PathBuf::from(&spec.executable),
        args: spec.args.clone(),
        extra_env: spec.env.clone(),
        // Only pass the native id when LJ Hub actually recorded one; the agent
        // decides whether it can honour it.
        resume_native: task.native_session_id.clone(),
    })
}

pub fn launch_missing_help(def_display: &str, def_hint: Option<&str>) -> String {
    match def_hint {
        Some(hint) => format!("{def_display} 尚不可用：{hint}"),
        None => format!("{def_display} 尚不可用"),
    }
}

pub fn default_permission_outcome(outcome: &Option<String>) -> String {
    outcome.clone().unwrap_or_else(|| "cancelled".to_string())
}

pub fn permission_choices_of(options: &[PermissionChoice]) -> Vec<String> {
    options.iter().map(|o| o.option_id.clone()).collect()
}

pub fn run() {
    use tauri::Manager;
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        // Never log credentials: the whole application logs through here.
        .with_target(false)
        .init();

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let data_dir = app
                .path()
                .app_data_dir()
                .unwrap_or_else(|_| PathBuf::from("/tmp/lj-hub"));
            std::fs::create_dir_all(&data_dir).ok();
            let state = AppState::new(data_dir);
            state.set_app(app.handle().clone());
            state.refresh_index();
            // A crash or a quit during a run must never look like success:
            // reconcile persisted runs before the UI can read them.
            for note in state.reconcile_runs() {
                tracing::warn!("{note}");
                state.record_run_log("startup", &note);
            }
            app.manage(state);

            // Documentation helper: `LJ_HUB_SCREENSHOT_MODE=topmost` raises the
            // window above other applications **without activating it**, so a
            // real screenshot can be taken while another app keeps focus.
            // Without the variable this code does nothing.
            if std::env::var("LJ_HUB_SCREENSHOT_MODE").is_ok() {
                // Applied after the window has been shown: touching the window
                // flags during setup() prevents it from appearing on macOS.
                if let Some(window) = app.get_webview_window("main") {
                    std::thread::spawn(move || {
                        std::thread::sleep(std::time::Duration::from_millis(2000));
                        let _ = window.show();
                        let _ = window.set_size(tauri::LogicalSize::new(1360.0, 800.0));
                        let _ = window.set_position(tauri::LogicalPosition::new(24.0, 48.0));
                        let _ = window.set_always_on_top(true);
                    });
                }
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            crate::commands::app_info,
            crate::commands::list_projects,
            crate::commands::add_project,
            crate::commands::remove_project,
            crate::commands::git_state,
            crate::commands::git_diff,
            crate::commands::recent_commits,
            crate::commands::list_agents,
            crate::commands::detect_agents,
            crate::commands::test_agent,
            crate::commands::set_agent_override,
            crate::commands::verify_agent_turn,
            crate::commands::clear_agent_override,
            crate::commands::workbuddy_status,
            crate::commands::workbuddy_status_detail,
            crate::commands::workbuddy_authorize_url,
            crate::commands::workbuddy_exchange_code,
            crate::commands::workbuddy_save_token,
            crate::commands::list_tasks,
            crate::commands::create_task,
            crate::commands::get_task,
            crate::commands::rename_task,
            crate::commands::delete_task,
            crate::commands::start_session,
            crate::commands::send_prompt,
            crate::commands::cancel_prompt,
            crate::commands::respond_permission,
            crate::commands::session_status,
            crate::commands::stop_session,
            crate::commands::prepare_handoff,
            crate::commands::verify_handoff,
            crate::commands::accept_handoff,
            crate::commands::handoff_markdown,
            crate::commands::register_artifact,
            crate::commands::verify_artifact,
            crate::commands::remove_artifact,
            crate::commands::read_requirement_file,
            crate::commands::list_runs,
            crate::commands::get_run,
            crate::commands::create_run,
            crate::commands::plan_run,
            crate::commands::start_run,
            crate::commands::pause_run,
            crate::commands::cancel_run,
            crate::commands::resolve_run_step,
            crate::commands::delete_run,
            crate::commands::runs_needing_human,
            crate::commands::get_settings,
            crate::commands::set_last_opened,
            crate::commands::set_theme,
            crate::commands::set_locale,
            crate::commands::import_requirements,
        ])
        .run(tauri::generate_context!())
        .expect("LJ Hub failed to start");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{PermissionChoice, TaskStatus};

    fn task() -> Task {
        Task {
            id: "t1".into(),
            project_id: "p1".into(),
            agent_id: "opencode".into(),
            title: "t".into(),
            status: TaskStatus::Active,
            native_session_id: None,
            native_session_restored: false,
            created_at: now_iso(),
            updated_at: now_iso(),
            messages: Vec::new(),
            handoff: None,
            last_git: None,
            run_id: None,
            run_step_id: None,
        }
    }

    #[test]
    fn streamed_chunks_merge_into_one_message() {
        let mut t = task();
        let mut open = HashMap::new();
        apply_event(
            &mut t,
            &SessionEvent::Chunk {
                stream: "agent".into(),
                text: "你".into(),
            },
            &mut open,
        );
        apply_event(
            &mut t,
            &SessionEvent::Chunk {
                stream: "agent".into(),
                text: "好".into(),
            },
            &mut open,
        );
        assert_eq!(t.messages.len(), 1);
        match &t.messages[0].body {
            MessageBody::Agent { text } => assert_eq!(text, "你好"),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn thought_and_answer_stay_separate() {
        let mut t = task();
        let mut open = HashMap::new();
        apply_event(
            &mut t,
            &SessionEvent::Chunk {
                stream: "thought".into(),
                text: "想".into(),
            },
            &mut open,
        );
        apply_event(
            &mut t,
            &SessionEvent::Chunk {
                stream: "agent".into(),
                text: "答".into(),
            },
            &mut open,
        );
        assert_eq!(t.messages.len(), 2);
        assert!(matches!(t.messages[0].body, MessageBody::Thought { .. }));
        assert!(matches!(t.messages[1].body, MessageBody::Agent { .. }));
    }

    #[test]
    fn tool_call_updates_do_not_duplicate_the_card() {
        let mut t = task();
        let mut open = HashMap::new();
        let tool = |status: &str| SessionEvent::ToolCall {
            tool: MessageBody::Tool {
                tool_call_id: "tool-1".into(),
                title: "read".into(),
                tool_kind: "Read".into(),
                status: status.into(),
                locations: vec![],
                raw_input: None,
                raw_output: None,
            },
        };
        apply_event(&mut t, &tool("in_progress"), &mut open);
        apply_event(&mut t, &tool("completed"), &mut open);
        assert_eq!(t.messages.len(), 1);
        match &t.messages[0].body {
            MessageBody::Tool { status, .. } => assert_eq!(status, "completed"),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn permission_requests_are_recorded_and_resolved() {
        let mut t = task();
        let mut open = HashMap::new();
        apply_event(
            &mut t,
            &SessionEvent::PermissionRequest {
                request_id: "perm-1".into(),
                title: "删除文件".into(),
                options: vec![PermissionChoice {
                    option_id: "reject-once".into(),
                    name: "拒绝".into(),
                    kind: "reject_once".into(),
                }],
            },
            &mut open,
        );
        apply_event(
            &mut t,
            &SessionEvent::PermissionResolved {
                request_id: "perm-1".into(),
                outcome: "cancelled".into(),
            },
            &mut open,
        );
        match &t.messages[0].body {
            MessageBody::Permission { outcome, .. } => assert_eq!(outcome, "cancelled"),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn native_session_event_sets_flags_and_message() {
        let mut t = task();
        let mut open = HashMap::new();
        apply_event(
            &mut t,
            &SessionEvent::NativeSession {
                native_session_id: "abc".into(),
                restored: false,
                detail: "该 Agent 未声明 session/load 能力".into(),
            },
            &mut open,
        );
        assert_eq!(t.native_session_id.as_deref(), Some("abc"));
        assert!(!t.native_session_restored);
        assert!(matches!(
            t.messages[0].body,
            MessageBody::SessionLink {
                restored: false,
                ..
            }
        ));
    }
}
