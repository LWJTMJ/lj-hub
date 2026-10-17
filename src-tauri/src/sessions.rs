//! Shared session plumbing used by both the interactive UI and the run driver.
//!
//! Extracted so the automatic relay path and the manual chat path go through
//! exactly the same code: the same handshake, the same event pipeline, the same
//! persistence. If the relay used a private shortcut, "it works in chat" would
//! stop implying "it works in a run".

use crate::acp::{spawn_session, LiveSession, SessionSpec};
use crate::app::AppState;
use crate::model::{now_iso, MessageBody, SessionEvent, Task, TaskStatus};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Build the session spec for a task, optionally overriding the working
/// directory (used when a step runs in a separate Git worktree).
pub fn build_spec(
    state: &AppState,
    task: &Task,
    workspace_root: PathBuf,
) -> Result<SessionSpec, String> {
    let mut spec = crate::app::build_spec(state, task, workspace_root)?;
    // Only pass the native session id when the agent supports restoring it;
    // this is decided by the ACP handshake inside the runtime.
    spec.resume_native = task.native_session_id.clone();
    Ok(spec)
}

/// Guarantee a live session for this task. `workspace` defaults to the
/// project's root directory.
///
/// Taking `&AppState` (instead of an `AppHandle`) keeps the relay driver
/// runnable in tests and independent of the UI shell; `ui` is only used to
/// push notifications.
pub async fn ensure_session_in(
    app: &AppState,
    ui: Option<&tauri::AppHandle>,
    task_id: &str,
    workspace: Option<&Path>,
) -> Result<Arc<LiveSession>, String> {
    let state = app;
    if let Ok(sessions) = state.sessions.lock() {
        if let Some(existing) = sessions.get(task_id) {
            if existing.is_alive() {
                return Ok(existing.clone());
            }
        }
    }
    state.stop_session(task_id);

    let task = state
        .store
        .load_task(task_id)
        .ok_or_else(|| format!("未知任务：{task_id}"))?;
    let project = state
        .state
        .lock()
        .ok()
        .and_then(|s| s.projects.iter().find(|p| p.id == task.project_id).cloned())
        .ok_or_else(|| format!("未知项目：{}", task.project_id))?;

    let root = match workspace {
        Some(path) => path.to_path_buf(),
        None => crate::commands::project_root_for(&project),
    };
    if !root.is_dir() {
        return Err(format!("工作目录不存在：{}", root.display()));
    }

    let spec = build_spec(state, &task, root)?;
    let _ = ui;
    // The recorder holds only shared handles, so the callback is 'static and
    // works both in the app and in tests.
    let recorder = state.recorder();
    let task_id_owned = task_id.to_string();
    let callback = Arc::new(move |event: SessionEvent| {
        recorder.record_event(&task_id_owned, event);
    });

    let perms = state.permissions.clone();
    let session = spawn_session(spec, callback, perms)
        .await
        .map_err(|e| format!("启动会话失败：{e}"))?;

    let session = Arc::new(session);
    if let Ok(mut sessions) = state.sessions.lock() {
        sessions.insert(task_id.to_string(), session.clone());
    }
    state.refresh_index();
    Ok(session)
}

pub async fn ensure_session(
    app: &AppState,
    ui: Option<&tauri::AppHandle>,
    task_id: &str,
) -> Result<Arc<LiveSession>, String> {
    ensure_session_in(app, ui, task_id, None).await
}

/// Create a task record that backs one agent turn.
pub fn create_task_for(
    state: &AppState,
    project_id: &str,
    agent_id: &str,
    title: &str,
    run_id: Option<&str>,
    step_id: Option<&str>,
) -> Result<Task, String> {
    let task = Task {
        id: crate::model::new_id("task"),
        project_id: project_id.to_string(),
        agent_id: agent_id.to_string(),
        title: title.to_string(),
        status: TaskStatus::Active,
        native_session_id: None,
        native_session_restored: false,
        created_at: now_iso(),
        updated_at: now_iso(),
        messages: Vec::new(),
        handoff: None,
        last_git: None,
        run_id: run_id.map(|s| s.to_string()),
        run_step_id: step_id.map(|s| s.to_string()),
    };
    state.store.save_task(&task).map_err(|e| e.to_string())?;
    state.refresh_index();
    Ok(task)
}

#[derive(Debug, Clone)]
pub struct AgentTurn {
    pub task_id: String,
    /// The agent's own words for this turn, joined in order.
    pub text: String,
    pub stop_reason: String,
    /// True when the turn ended because the wall-clock limit was hit.
    pub timed_out: bool,
}

/// Everything needed to run one agent turn.
#[derive(Debug, Clone)]
pub struct TurnRequest<'a> {
    pub project_id: &'a str,
    pub agent_id: &'a str,
    pub workspace: &'a Path,
    pub title: &'a str,
    pub prompt: &'a str,
    pub timeout: std::time::Duration,
    pub run_id: Option<&'a str>,
    pub step_id: Option<&'a str>,
}

/// Run exactly one agent turn: create the task, start the session, send the
/// prompt, wait for the turn to finish. Used by both the planner and each step.
pub async fn run_agent_turn(
    app: &AppState,
    ui: Option<&tauri::AppHandle>,
    request: TurnRequest<'_>,
) -> Result<AgentTurn, String> {
    let TurnRequest {
        project_id,
        agent_id,
        workspace,
        title,
        prompt,
        timeout,
        run_id,
        step_id,
    } = request;
    let task = create_task_for(app, project_id, agent_id, title, run_id, step_id)?;
    let task_id = task.id.clone();

    let session = ensure_session_in(app, ui, &task_id, Some(workspace)).await?;
    app.record_user_message(&task_id, prompt);

    let outcome = tokio::time::timeout(timeout, session.prompt_text(prompt)).await;
    let (stop_reason, timed_out, error) = match outcome {
        Err(_) => {
            // Stop the turn we started; never leave it running unattended.
            session.cancel();
            ("timeout".to_string(), true, None)
        }
        Ok(Ok(reply)) => (reply.stop_reason, false, reply.error),
        Ok(Err(e)) => ("error".to_string(), false, Some(e)),
    };

    // Stop the session *before* reading the transcript: stopping closes the
    // event pipeline, which flushes any buffered stream text. Reading earlier
    // could silently lose the agent's last lines (a real bug this ordering
    // exists to prevent).
    app.stop_session(&task_id);

    let text = collect_agent_text(
        app.store
            .load_task(&task_id)
            .map(|t| t.messages)
            .unwrap_or_default(),
    );

    if let Some(error) = error {
        if text.trim().is_empty() {
            return Err(format!("Agent 回合失败：{error}"));
        }
    }

    Ok(AgentTurn {
        task_id,
        text,
        stop_reason,
        timed_out,
    })
}

/// Join the agent's own messages into one string for this turn.
pub fn collect_agent_text(messages: Vec<crate::model::Message>) -> String {
    let mut out = Vec::new();
    for message in &messages {
        if let MessageBody::Agent { text } = &message.body {
            out.push(text.trim().to_string());
        }
    }
    out.into_iter()
        .filter(|t| !t.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Extract the `STEP_SUMMARY: …` line an agent is asked to end with.
pub fn extract_step_summary(text: &str) -> Option<String> {
    text.lines()
        .rev()
        .find_map(|line| {
            let trimmed = line.trim().trim_start_matches(['*', '-', ' ']);
            trimmed
                .strip_prefix("STEP_SUMMARY:")
                .map(|rest| rest.trim().to_string())
        })
        .filter(|s| !s.is_empty())
}
