//! Tauri command surface. Every command is a real operation on real state;
//! nothing here fabricates success.

use crate::acp::{spawn_session, ProbeOutcome, SessionSpec};
use crate::agents;
use crate::app::AppState;
use crate::artifacts;
use crate::handoff::{self, HandoffDraft};
use crate::model::{
    now_iso, AgentOverride, AgentReport, ArtifactRef, GitState, Handoff, HandoffVerificationCheck,
    Message, MessageBody, PermissionChoice, Project, RequirementRef, SessionEvent, Settings, Task,
    TaskStatus, TaskSummary,
};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tauri::{AppHandle, Manager, State};

#[derive(Debug, Clone, Serialize)]
pub struct LaunchSpec {
    pub executable: String,
    pub args: Vec<String>,
    pub display: String,
    pub via_npm_adapter: bool,
    /// Environment variables to pass through (names configured by the user,
    /// values taken from LJ Hub's own environment at launch time only).
    pub env: Vec<(String, String)>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AppInfo {
    pub name: String,
    pub version: String,
    pub data_dir: String,
    pub platform: String,
    pub arch: String,
    pub git_available: bool,
    pub protocol: String,
    /// Documentation helper: when LJ_HUB_SCREENSHOT_OPEN is set, the UI opens
    /// that view right after start so screenshots can be taken without
    /// synthetic clicks. Purely local, no effect unless the variable is set.
    pub screenshot_open: Option<String>,
}

#[tauri::command]
pub fn app_info(state: State<'_, AppState>) -> AppInfo {
    AppInfo {
        name: "LJ Hub".into(),
        version: env!("CARGO_PKG_VERSION").into(),
        data_dir: state.data_dir.display().to_string(),
        platform: std::env::consts::OS.to_string(),
        arch: std::env::consts::ARCH.to_string(),
        git_available: agents::which("git").is_some(),
        protocol: "ACP v1 (agent-client-protocol 3.x)".into(),
        screenshot_open: std::env::var("LJ_HUB_SCREENSHOT_OPEN").ok(),
    }
}

// ---------------------------------------------------------------- projects

#[tauri::command]
pub fn list_projects(state: State<'_, AppState>) -> Vec<Project> {
    state
        .state
        .lock()
        .map(|s| s.projects.clone())
        .unwrap_or_default()
}

#[tauri::command]
pub fn add_project(state: State<'_, AppState>, path: String) -> Result<Project, String> {
    let dir = PathBuf::from(shellexpand(&path));
    if !dir.is_dir() {
        return Err(format!("目录不存在：{}", dir.display()));
    }
    let canonical = dir.canonicalize().map_err(|e| e.to_string())?;
    let name = canonical
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| canonical.display().to_string());

    let mut project = Project {
        id: crate::model::new_id("proj"),
        name,
        path: canonical.display().to_string(),
        git_root: None,
        git_remote: None,
        created_at: now_iso(),
        last_opened_at: now_iso(),
    };

    if let Some(root) = crate::git::repo_root(&canonical) {
        project.git_remote = crate::git::remote_url(&root);
        project.git_root = Some(root.display().to_string());
    }

    // Reuse the existing record when the same directory is opened again.
    if let Ok(mut store) = state.state.lock() {
        if let Some(existing) = store.projects.iter_mut().find(|p| p.path == project.path) {
            existing.last_opened_at = now_iso();
            existing.git_root = project.git_root.clone();
            existing.git_remote = project.git_remote.clone();
            let updated = existing.clone();
            let _ = state.store.save_state(&store);
            return Ok(updated);
        }
        store.projects.push(project.clone());
        state.store.save_state(&store).map_err(|e| e.to_string())?;
    }
    Ok(project)
}

#[tauri::command]
pub fn remove_project(state: State<'_, AppState>, project_id: String) -> Result<(), String> {
    if let Ok(mut store) = state.state.lock() {
        store.projects.retain(|p| p.id != project_id);
        // Task files stay on disk on purpose: removing a project from the list
        // must not destroy history.
        state.store.save_state(&store).map_err(|e| e.to_string())?;
    }
    state.refresh_index();
    Ok(())
}

fn project_by_id(state: &AppState, project_id: &str) -> Result<Project, String> {
    state
        .state
        .lock()
        .ok()
        .and_then(|s| s.projects.iter().find(|p| p.id == project_id).cloned())
        .ok_or_else(|| format!("未知项目：{project_id}"))
}

pub fn project_root_for(project: &Project) -> PathBuf {
    project_root(project)
}

fn project_root(project: &Project) -> PathBuf {
    project
        .git_root
        .clone()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(&project.path))
}

#[tauri::command]
pub fn git_state(state: State<'_, AppState>, project_id: String) -> Result<GitState, String> {
    let project = project_by_id(&state, &project_id)?;
    Ok(crate::git::state(&PathBuf::from(&project.path)))
}

#[tauri::command]
pub fn git_diff(
    state: State<'_, AppState>,
    project_id: String,
    path: String,
) -> Result<String, String> {
    let project = project_by_id(&state, &project_id)?;
    crate::git::diff_for(&PathBuf::from(&project.path), &path, 400_000)
}

#[tauri::command]
pub fn recent_commits(
    state: State<'_, AppState>,
    project_id: String,
    count: usize,
) -> Result<Vec<String>, String> {
    let project = project_by_id(&state, &project_id)?;
    Ok(crate::git::recent_commits(
        &PathBuf::from(&project.path),
        count.clamp(1, 50),
    ))
}

// ------------------------------------------------------------------ agents

fn overrides_for(state: &AppState, id: &str) -> Option<AgentOverride> {
    state
        .state
        .lock()
        .ok()
        .and_then(|s| s.settings.agent_overrides.get(id).cloned())
}

/// Local detection only: executable + version. The status stays NOT_TESTED
/// until a real ACP handshake has been performed.
#[tauri::command]
pub async fn detect_agents(app: AppHandle) -> Result<Vec<AgentReport>, String> {
    let state = app.state::<AppState>();
    let mut reports = Vec::new();
    for def in agents::AGENTS {
        let over = overrides_for(&state, def.id);
        let mut report = agents::local_report(def, over.as_ref());
        if let Some(exe) = report.executable.clone() {
            let version_args: Vec<&str> = def.version_args.to_vec();
            match agents::probe_version(Path::new(&exe), &version_args).await {
                Ok(version) => report.version = Some(version),
                Err(e) => {
                    if def.id == "codex" {
                        // The pinned npm adapter is expected to be launched by
                        // npx; a missing `--version` on the resolved shim is
                        // not itself an error.
                        report.message = format!("{}（版本信息不可用：{e}）", report.message);
                    } else {
                        report.status = crate::model::AgentStatus::Error;
                        report.message = format!("可执行文件存在，但无法读取版本：{e}");
                    }
                }
            }
        }
        reports.push(report);
    }
    if let Ok(mut cache) = state.probe_results.lock() {
        for report in &reports {
            if let Some(cached) = cache.get(&report.id) {
                if cached.executable == report.executable {
                    // Keep previously verified handshake results.
                    continue;
                }
            }
            cache.remove(&report.id);
        }
        for report in &reports {
            if report.status == crate::model::AgentStatus::NotTested
                || report.status == crate::model::AgentStatus::NotInstalled
                || report.status == crate::model::AgentStatus::Error
            {
                cache.insert(report.id.clone(), report.clone());
            }
        }
    }
    Ok(reports)
}

#[tauri::command]
pub async fn list_agents(app: AppHandle) -> Result<Vec<AgentReport>, String> {
    let reports = detect_agents(app.clone()).await?;
    let state = app.state::<AppState>();
    // Merge in verified handshake results, which are the only source of
    // AVAILABLE / AUTH_REQUIRED.
    let cache = state
        .probe_results
        .lock()
        .map(|c| c.clone())
        .unwrap_or_default();
    Ok(reports
        .into_iter()
        .map(|mut report| {
            if let Some(verified) = cache.get(&report.id) {
                if verified.executable == report.executable
                    && matches!(
                        verified.status,
                        crate::model::AgentStatus::Available
                            | crate::model::AgentStatus::AuthRequired
                            | crate::model::AgentStatus::Error
                    )
                {
                    report.status = verified.status;
                    report.message = verified.message.clone();
                    report.auth_methods = verified.auth_methods.clone();
                    report.agent_info = verified.agent_info.clone();
                    report.capabilities = verified.capabilities.clone();
                    report.last_tested_at = verified.last_tested_at.clone();
                }
            }
            report
        })
        .collect())
}

/// The only operation that can move an agent to AVAILABLE: a real ACP
/// handshake against the real executable.
#[tauri::command]
pub async fn test_agent(app: AppHandle, agent_id: String) -> Result<AgentReport, String> {
    let state = app.state::<AppState>();
    let def =
        agents::find_definition(&agent_id).ok_or_else(|| format!("未登记的 Agent：{agent_id}"))?;
    let over = overrides_for(&state, &agent_id);
    let mut report = agents::local_report(def, over.as_ref());

    if def.transport == crate::model::TransportKind::OpenApiHttp {
        // WorkBuddy is verified through its own Open API status call instead.
        return workbuddy_status(app.clone()).await;
    }

    let spec = state.launch_spec(&agent_id)?;
    report.executable = Some(spec.executable.clone());
    report.acp_args = spec.args.clone();
    report.overridden = over
        .as_ref()
        .map(|o| o.executable.is_some())
        .unwrap_or(false);

    let cwd = std::env::var("HOME").map(PathBuf::from).unwrap_or_default();
    let outcome: ProbeOutcome = crate::acp::probe(
        PathBuf::from(&spec.executable),
        spec.args.clone(),
        cwd,
        std::time::Duration::from_secs(90),
    )
    .await;

    report.status = outcome.status;
    report.message = outcome.message;
    report.agent_info = outcome.agent_info;
    report.auth_methods = outcome.auth_methods;
    report.capabilities = outcome.capabilities;
    report.last_tested_at = Some(now_iso());
    report.verification.executable_resolved = report.executable.is_some();
    report.verification.handshake_ok = matches!(
        outcome.status,
        crate::model::AgentStatus::Available | crate::model::AgentStatus::AuthRequired
    );
    // A handshake alone never makes an agent schedulable: a real model turn
    // must have been proven (see `verify_agent_turn`).
    report.verification.schedulable = report.verification.real_turn_ok == Some(true);
    if matches!(outcome.status, crate::model::AgentStatus::Available) {
        report.message.push_str(
            "（握手成功；真实模型回合尚未验证，需在设置中执行“验证真实回合”后才能参与自动接力）",
        );
    }
    if spec.via_npm_adapter {
        report
            .message
            .push_str("（通过固定版本的官方 npm 适配器启动）");
    }

    if let Ok(mut cache) = state.probe_results.lock() {
        cache.insert(agent_id, report.clone());
    }
    Ok(report)
}

/// Prove that an agent can complete a **real model turn** (not just a
/// handshake). Opt-in in the UI because it spends the user's quota; the result
/// is what allows an agent to take part in the automatic relay.
#[tauri::command]
pub async fn verify_agent_turn(app: AppHandle, agent_id: String) -> Result<AgentReport, String> {
    let state = app.state::<AppState>();
    let workdir = state.data_dir.join("verify").join(&agent_id);
    std::fs::create_dir_all(&workdir).map_err(|e| e.to_string())?;

    let mut report = list_agents(app.clone())
        .await?
        .into_iter()
        .find(|r| r.id == agent_id)
        .ok_or_else(|| format!("未登记的 Agent：{agent_id}"))?;

    let prompt = "Reply with exactly the two characters: OK";
    let turn = crate::sessions::run_agent_turn(
        &state,
        None,
        crate::sessions::TurnRequest {
            project_id: project_for_verification(&state)?.as_str(),
            agent_id: &agent_id,
            workspace: &workdir,
            title: &format!("{agent_id} · 真实回合验证"),
            prompt,
            timeout: std::time::Duration::from_secs(180),
            run_id: None,
            step_id: Some("verify-turn"),
        },
    )
    .await;

    report.verification.handshake_ok = matches!(
        report.status,
        crate::model::AgentStatus::Available | crate::model::AgentStatus::AuthRequired
    );
    report.verification.real_turn_at = Some(now_iso());
    match turn {
        Ok(result) if !result.text.trim().is_empty() => {
            report.verification.real_turn_ok = Some(true);
            report.verification.real_turn_error = None;
            report.verification.schedulable = true;
            report.message = format!(
                "真实模型回合成功（{} 字），可用于自动接力。",
                result.text.chars().count()
            );
        }
        Ok(_) => {
            report.verification.real_turn_ok = Some(false);
            report.verification.real_turn_error = Some("回合结束但没有产生任何回答".to_string());
            report.verification.schedulable = false;
            report.message = "真实模型回合没有产生回答，不能参与自动接力。".to_string();
        }
        Err(error) => {
            report.verification.real_turn_ok = Some(false);
            report.verification.real_turn_error = Some(error.clone());
            report.verification.schedulable = false;
            report.message = format!("真实模型回合失败：{error}");
        }
    }
    if let Ok(mut cache) = state.probe_results.lock() {
        let mut merged = report.clone();
        // Keep the handshake verdict; only the verification block changed.
        if let Some(previous) = cache.get(&agent_id) {
            merged.status = previous.status;
            merged.auth_methods = previous.auth_methods.clone();
            merged.capabilities = previous.capabilities.clone();
            merged.agent_info = previous.agent_info.clone();
        }
        cache.insert(agent_id, merged.clone());
        report = merged;
    }
    Ok(report)
}

/// The verification turn needs a project id; reuse any existing project, and
/// create a throwaway one when the user has none yet.
fn project_for_verification(state: &AppState) -> Result<String, String> {
    let mut store = state.state.lock().map_err(|e| e.to_string())?;
    if let Some(project) = store.projects.first() {
        return Ok(project.id.clone());
    }
    let dir = state.data_dir.join("verify");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let project = Project {
        id: crate::model::new_id("proj"),
        name: "LJ Hub 验证目录".into(),
        path: dir.display().to_string(),
        git_root: None,
        git_remote: None,
        created_at: now_iso(),
        last_opened_at: now_iso(),
    };
    let id = project.id.clone();
    store.projects.push(project);
    state.store.save_state(&store).map_err(|e| e.to_string())?;
    Ok(id)
}

#[tauri::command]
pub fn set_agent_override(
    state: State<'_, AppState>,
    agent_id: String,
    executable: Option<String>,
    args: Option<Vec<String>>,
    allow_npm_adapter: Option<bool>,
    env_passthrough: Option<Vec<String>>,
) -> Result<(), String> {
    if agents::find_definition(&agent_id).is_none() {
        return Err(format!("未登记的 Agent：{agent_id}"));
    }
    let mut store = state.state.lock().map_err(|e| e.to_string())?;
    let entry = store
        .settings
        .agent_overrides
        .entry(agent_id.clone())
        .or_default();
    entry.executable = executable.filter(|s| !s.trim().is_empty());
    entry.args = args.unwrap_or_default();
    if let Some(allow) = allow_npm_adapter {
        entry.allow_npm_adapter = allow;
    }
    if let Some(names) = env_passthrough {
        // Store names only, and only sane ones.
        entry.env_passthrough = names
            .into_iter()
            .map(|n| n.trim().to_string())
            .filter(|n| {
                !n.is_empty()
                    && n.chars()
                        .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
            })
            .collect();
    }
    state.store.save_state(&store).map_err(|e| e.to_string())?;
    drop(store);
    // Any previous verification is invalid once the launch command changes.
    if let Ok(mut cache) = state.probe_results.lock() {
        cache.remove(&agent_id);
    }
    Ok(())
}

#[tauri::command]
pub fn clear_agent_override(state: State<'_, AppState>, agent_id: String) -> Result<(), String> {
    let mut store = state.state.lock().map_err(|e| e.to_string())?;
    store.settings.agent_overrides.remove(&agent_id);
    state.store.save_state(&store).map_err(|e| e.to_string())?;
    if let Ok(mut cache) = state.probe_results.lock() {
        cache.remove(&agent_id);
    }
    Ok(())
}

// --------------------------------------------------------------- workbuddy

// ------------------------------------------------------------------- runs

#[derive(Debug, Clone, Serialize)]
pub struct RunSummary {
    pub id: String,
    pub project_id: String,
    pub title: String,
    pub objective: String,
    pub status: crate::scheduler::RunStatus,
    pub steps_total: usize,
    pub steps_done: usize,
    pub steps_needs_human: usize,
    pub stop_reason: Option<String>,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct RunDetail {
    pub run: crate::scheduler::Run,
    pub log: Vec<String>,
    /// The agent's own words for each step, so the UI can show what actually
    /// happened without leaving LJ Hub.
    pub step_tasks: Vec<StepTaskLink>,
    pub blocked: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct StepTaskLink {
    pub step_id: String,
    pub task_id: String,
    pub title: String,
    pub agent: String,
    pub status: String,
}

fn run_store(state: &AppState) -> crate::runs::RunStore {
    crate::runs::RunStore::new(&state.data_dir)
}

fn summarize(run: &crate::scheduler::Run) -> RunSummary {
    RunSummary {
        id: run.id.clone(),
        project_id: run.project_id.clone(),
        title: run.title.clone(),
        objective: run.objective.clone(),
        status: run.status,
        steps_total: run.steps.len(),
        steps_done: run
            .steps
            .iter()
            .filter(|s| s.status == crate::scheduler::StepStatus::Done)
            .count(),
        steps_needs_human: run
            .steps
            .iter()
            .filter(|s| s.status == crate::scheduler::StepStatus::NeedsHuman)
            .count(),
        stop_reason: run.stop_reason.clone(),
        updated_at: run.updated_at.clone(),
    }
}

#[tauri::command]
pub fn list_runs(state: State<'_, AppState>) -> Result<Vec<RunSummary>, String> {
    let (runs, problems) = run_store(&state).list();
    if !problems.is_empty() {
        // Surface unreadable run files instead of hiding them.
        return Err(format!(
            "有 {} 个运行记录无法解析：{}",
            problems.len(),
            problems
                .iter()
                .map(|p| format!("{} ({})", p.run_id, p.detail))
                .collect::<Vec<_>>()
                .join("；")
        ));
    }
    Ok(runs.iter().map(summarize).collect())
}

#[tauri::command]
pub fn get_run(state: State<'_, AppState>, run_id: String) -> Result<RunDetail, String> {
    let run = run_store(&state).load(&run_id)?;
    let mut step_tasks = Vec::new();
    for step in &run.steps {
        let Some(task_id) = step.task_id.clone() else {
            continue;
        };
        if let Some(task) = state.store.load_task(&task_id) {
            step_tasks.push(StepTaskLink {
                step_id: step.id.clone(),
                task_id: task.id.clone(),
                title: task.title.clone(),
                agent: task.agent_id.clone(),
                status: format!("{:?}", task.status),
            });
        }
    }
    let blocked = run
        .steps
        .iter()
        .filter(|s| s.status == crate::scheduler::StepStatus::NeedsHuman)
        .map(|s| {
            format!(
                "{}：{}",
                s.id,
                s.needs_human_reason
                    .clone()
                    .unwrap_or_else(|| "需要人工确认".into())
            )
        })
        .collect();
    Ok(RunDetail {
        log: state.run_log(&run_id),
        run,
        step_tasks,
        blocked,
    })
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateRunInput {
    pub project_id: String,
    pub title: String,
    pub objective: String,
    /// Agent used for planning; also the default reviewer for steps that do not
    /// override it.
    pub agent: String,
    /// Optional pre-made plan (JSON). When absent, `plan_run` asks the planner.
    #[serde(default)]
    pub plan_json: Option<String>,
    #[serde(default)]
    pub requirements: Vec<RequirementRef>,
    #[serde(default)]
    pub forbidden: Vec<String>,
    /// Optional isolated worktree for every step of this run.
    #[serde(default)]
    pub use_worktree: bool,
}

#[tauri::command]
pub fn create_run(
    state: State<'_, AppState>,
    input: CreateRunInput,
) -> Result<crate::scheduler::Run, String> {
    let project = project_by_id(&state, &input.project_id)?;
    let root = project_root(&project);

    let workspace = if input.use_worktree {
        let repo = project
            .git_root
            .clone()
            .ok_or_else(|| "该项目不是 Git 仓库，无法创建隔离工作区".to_string())?;
        let worktree = crate::worktree::create_for_run(
            std::path::Path::new(&repo),
            &state.data_dir,
            &input.title,
        )?;
        crate::scheduler::WorkspaceRef {
            root: worktree.path.display().to_string(),
            kind: "worktree".into(),
            branch: Some(worktree.branch),
        }
    } else {
        crate::scheduler::WorkspaceRef::main(root.display().to_string())
    };

    let plan = match input.plan_json.as_deref() {
        Some(json) if !json.trim().is_empty() => {
            let plan = crate::scheduler::parse_plan(json)
                .map_err(|errors| format!("计划解析失败：{}", errors.join("；")))?;
            let agents: Vec<String> = crate::agents::AGENTS
                .iter()
                .map(|d| d.id.to_string())
                .collect();
            let problems = crate::scheduler::validate_plan(&plan, &agents);
            if !problems.is_empty() {
                return Err(format!("计划未通过校验：{}", problems.join("；")));
            }
            plan
        }
        _ => crate::scheduler::RunPlan {
            phases: Vec::new(),
            steps: Vec::new(),
        },
    };

    let mut run = crate::scheduler::Run::new(
        crate::model::new_id("run"),
        project.id.clone(),
        input.title.clone(),
        input.objective.clone(),
        input.agent.clone(),
        plan,
        workspace,
    );
    run.requirements = input.requirements;
    run.forbidden = input.forbidden;

    let store = run_store(&state);
    store.save(&run)?;
    state.record_run_log(&run.id, "运行已创建（草稿）");
    Ok(run)
}

#[tauri::command]
pub async fn plan_run(app: AppHandle, run_id: String) -> Result<crate::scheduler::Run, String> {
    let result = {
        let state = app.state::<AppState>();
        crate::run_driver::plan_run(&state, &run_id).await
    };
    let state = app.state::<AppState>();
    let _ = state.emit_run_event(&run_id, "计划已更新");
    result
}

#[tauri::command]
pub fn start_run(app: AppHandle, run_id: String) -> Result<crate::scheduler::Run, String> {
    let state = app.state::<AppState>();
    let mut run = run_store(&state).load(&run_id)?;
    if run.plan.steps.is_empty() {
        return Err("该运行还没有计划，请先生成计划".to_string());
    }
    let problems = crate::scheduler::validate_plan(&run.plan, &[]);
    if !problems.is_empty() {
        return Err(format!("计划未通过校验：{}", problems.join("；")));
    }
    run.status = crate::scheduler::RunStatus::Running;
    run.stop_reason = None;
    run_store(&state).save(&run)?;
    state.record_run_log(&run_id, "开始自动推进");
    {
        let mut workers = state.run_workers.lock().map_err(|e| e.to_string())?;
        workers.insert(run_id.clone());
    }
    crate::run_driver::start_worker(app.clone(), run_id.clone(), Default::default());
    Ok(run)
}

#[tauri::command]
pub fn pause_run(app: AppHandle, run_id: String) -> Result<crate::scheduler::Run, String> {
    let state = app.state::<AppState>();
    {
        let mut workers = state.run_workers.lock().map_err(|e| e.to_string())?;
        workers.remove(&run_id);
    }
    state.record_run_log(&run_id, "已暂停自动推进（进行中的 agent 回合不会被强杀）");
    let _ = state.emit_run_event(&run_id, "已暂停");
    run_store(&state).load(&run_id)
}

#[tauri::command]
pub fn cancel_run(app: AppHandle, run_id: String) -> Result<crate::scheduler::Run, String> {
    let state = app.state::<AppState>();
    crate::run_driver::cancel_run(&state, &run_id)
}

#[tauri::command]
pub fn resolve_run_step(
    app: AppHandle,
    run_id: String,
    step_id: String,
    action: String,
    note: String,
) -> Result<crate::scheduler::Run, String> {
    let state = app.state::<AppState>();
    let run = crate::run_driver::resolve_needs_human(&state, &run_id, &step_id, &action, &note)?;
    Ok(run)
}

#[tauri::command]
pub fn delete_run(state: State<'_, AppState>, run_id: String) -> Result<(), String> {
    run_store(&state).delete(&run_id)
}

#[tauri::command]
pub fn runs_needing_human(state: State<'_, AppState>) -> Vec<(String, String, String)> {
    run_store(&state).needs_human()
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkBuddyStatus {
    pub configured: bool,
    pub has_token: bool,
    pub has_client_credentials: bool,
    pub base_url: String,
    pub status: crate::model::AgentStatus,
    pub message: String,
    pub online: Option<bool>,
    pub credentials_path: String,
}

async fn workbuddy_impl(state: &AppState) -> WorkBuddyStatus {
    let config = crate::workbuddy::load_config(&state.data_dir);
    let mut status = WorkBuddyStatus {
        configured: config.client_id.is_some() || config.has_token(),
        has_token: config.has_token(),
        has_client_credentials: config.client_id.is_some() && config.client_secret.is_some(),
        base_url: config.base(),
        status: crate::model::AgentStatus::ConfigRequired,
        message: String::new(),
        online: None,
        credentials_path: crate::workbuddy::credentials_path(&state.data_dir)
            .display()
            .to_string(),
    };

    if !config.has_token() {
        status.message = "尚未安装 WorkBuddy 访问令牌。该接入需要 WorkBuddy 开放平台审核通过的第三方应用，\
                          并完成 OAuth 授权换取 access_token；LJ Hub 不会伪造连通状态，也不会内置任何应用密钥。"
            .to_string();
        return status;
    }

    match crate::workbuddy::WorkBuddyClient::new(config) {
        Ok(client) => match client.local_assistant_status().await {
            Ok(local) => {
                status.status = crate::model::AgentStatus::Available;
                status.online = Some(local.online);
                status.message = if local.online {
                    "本地助理在线，Open API 调用成功。".to_string()
                } else {
                    "Open API 调用成功，但 PC 端本地助理当前不在线。".to_string()
                };
            }
            Err(e) => {
                status.status = e.status();
                status.message = e.message();
            }
        },
        Err(e) => {
            status.status = e.status();
            status.message = e.message();
        }
    }
    status
}

#[tauri::command]
pub async fn workbuddy_status(app: AppHandle) -> Result<AgentReport, String> {
    let state = app.state::<AppState>();
    let status = workbuddy_impl(&state).await;
    let mut report = agents::base_report(agents::find_definition("workbuddy").unwrap());
    report.status = status.status;
    report.message = status.message;
    report.last_tested_at = Some(now_iso());
    Ok(report)
}

#[tauri::command]
pub async fn workbuddy_status_detail(app: AppHandle) -> Result<WorkBuddyStatus, String> {
    let state = app.state::<AppState>();
    Ok(workbuddy_impl(&state).await)
}

#[tauri::command]
pub fn workbuddy_authorize_url(app: AppHandle, scope: Option<String>) -> Result<String, String> {
    let state = app.state::<AppState>();
    let config = crate::workbuddy::load_config(&state.data_dir);
    let client = crate::workbuddy::WorkBuddyClient::new(config).map_err(|e| e.message())?;
    let state_value = crate::model::new_id("state");
    client
        .authorize_url(scope.as_deref(), &state_value)
        .map_err(|e| e.message())
}

#[tauri::command]
pub async fn workbuddy_exchange_code(
    app: AppHandle,
    code: String,
    redirect_uri: String,
) -> Result<(), String> {
    let state = app.state::<AppState>();
    let mut config = crate::workbuddy::load_config(&state.data_dir);
    if config.redirect_uri.is_none() {
        config.redirect_uri = Some(redirect_uri.clone());
    }
    let client = crate::workbuddy::WorkBuddyClient::new(config.clone()).map_err(|e| e.message())?;
    let token = client
        .exchange_code(&code, &redirect_uri)
        .await
        .map_err(|e| e.message())?;
    config.access_token = token.access_token;
    config.refresh_token = token.refresh_token.or(config.refresh_token);
    config.open_id = token.open_id.or(config.open_id);
    crate::workbuddy::save_config(&state.data_dir, &config)
}

#[tauri::command]
pub fn workbuddy_save_token(
    app: AppHandle,
    access_token: String,
    client_id: Option<String>,
    client_secret: Option<String>,
    redirect_uri: Option<String>,
) -> Result<(), String> {
    let state = app.state::<AppState>();
    let mut config = crate::workbuddy::load_config(&state.data_dir);
    if !access_token.trim().is_empty() {
        config.access_token = Some(access_token.trim().to_string());
    }
    if let Some(id) = client_id.filter(|s| !s.trim().is_empty()) {
        config.client_id = Some(id.trim().to_string());
    }
    if let Some(secret) = client_secret.filter(|s| !s.trim().is_empty()) {
        config.client_secret = Some(secret.trim().to_string());
    }
    if let Some(uri) = redirect_uri.filter(|s| !s.trim().is_empty()) {
        config.redirect_uri = Some(uri.trim().to_string());
    }
    crate::workbuddy::save_config(&state.data_dir, &config)
}

// ------------------------------------------------------------------- tasks

#[tauri::command]
pub fn list_tasks(state: State<'_, AppState>, project_id: String) -> Vec<TaskSummary> {
    state.refresh_index();
    let tasks = state
        .state
        .lock()
        .map(|s| s.tasks.clone())
        .unwrap_or_default();
    tasks
        .into_iter()
        .filter(|t| t.project_id == project_id)
        .collect()
}

#[tauri::command]
pub fn create_task(
    state: State<'_, AppState>,
    project_id: String,
    agent_id: String,
    title: String,
) -> Result<Task, String> {
    let project = project_by_id(&state, &project_id)?;
    if agents::find_definition(&agent_id).is_none() {
        return Err(format!("未登记的 Agent：{agent_id}"));
    }
    let task = Task {
        id: crate::model::new_id("task"),
        project_id: project.id.clone(),
        agent_id,
        title: if title.trim().is_empty() {
            "新任务".to_string()
        } else {
            title
        },
        status: TaskStatus::Active,
        native_session_id: None,
        native_session_restored: false,
        created_at: now_iso(),
        updated_at: now_iso(),
        messages: Vec::new(),
        handoff: None,
        last_git: Some(crate::git::evidence(&project_root(&project))),
        run_id: None,
        run_step_id: None,
    };
    state.store.save_task(&task).map_err(|e| e.to_string())?;
    state.refresh_index();
    Ok(task)
}

#[tauri::command]
pub fn get_task(state: State<'_, AppState>, task_id: String) -> Result<Task, String> {
    state
        .store
        .load_task(&task_id)
        .ok_or_else(|| format!("未知任务：{task_id}"))
}

#[tauri::command]
pub fn rename_task(
    state: State<'_, AppState>,
    task_id: String,
    title: String,
) -> Result<Task, String> {
    let mut task = state
        .store
        .load_task(&task_id)
        .ok_or_else(|| format!("未知任务：{task_id}"))?;
    task.title = title;
    task.updated_at = now_iso();
    state.save_task(&task);
    Ok(task)
}

/// Deleting a task removes its local history file. The agent's own session is
/// never touched, and the UI says so before calling this.
#[tauri::command]
pub fn delete_task(state: State<'_, AppState>, task_id: String) -> Result<(), String> {
    state.stop_session(&task_id);
    state
        .store
        .delete_task(&task_id)
        .map_err(|e| e.to_string())?;
    state.refresh_index();
    Ok(())
}

// ---------------------------------------------------------------- sessions

#[derive(Debug, Clone, Serialize)]
pub struct SessionStartInfo {
    pub live: bool,
    pub native_session_id: Option<String>,
    pub native_session_restored: bool,
    pub agent_info: Option<String>,
    pub auth_methods: Vec<String>,
    pub capabilities: crate::model::AgentCapabilitiesReport,
    pub detail: String,
    pub launch: String,
}

fn task_with_project(state: &AppState, task_id: &str) -> Result<(Task, Project), String> {
    let task = state
        .store
        .load_task(task_id)
        .ok_or_else(|| format!("未知任务：{task_id}"))?;
    let project = project_by_id(state, &task.project_id)?;
    Ok((task, project))
}

/// Guarantee a live session for this task, starting the agent if needed.
async fn ensure_session(
    app: &AppHandle,
    task_id: &str,
) -> Result<Arc<crate::acp::LiveSession>, String> {
    let state = app.state::<AppState>();
    if let Ok(sessions) = state.sessions.lock() {
        if let Some(existing) = sessions.get(task_id) {
            if existing.is_alive() {
                return Ok(existing.clone());
            }
        }
    }
    state.stop_session(task_id);

    let (task, project) = task_with_project(&state, task_id)?;
    let root = project_root(&project);
    let spec: SessionSpec = crate::app::build_spec(&state, &task, root)?;

    let handle = app.clone();
    let task_id_owned = task_id.to_string();
    let callback = Arc::new(move |event: SessionEvent| {
        let state = handle.state::<AppState>();
        state.record_event(&task_id_owned, event);
    });

    // Permission answers from the UI are routed through the shared registry.
    let perms = app.state::<AppState>().permissions.clone();

    let session = spawn_session(spec, callback, perms)
        .await
        .map_err(|e| format!("启动 {task_id} 的 Agent 会话失败：{e}"))?;

    let session = Arc::new(session);
    if let Ok(mut sessions) = state.sessions.lock() {
        sessions.insert(task_id.to_string(), session.clone());
    }
    state.refresh_index();
    Ok(session)
}

#[tauri::command]
pub async fn start_session(app: AppHandle, task_id: String) -> Result<SessionStartInfo, String> {
    let state = app.state::<AppState>();
    let (task, project) = task_with_project(&state, &task_id)?;
    let spec = crate::app::build_spec(&state, &task, project_root(&project))?;
    let launch = format!("{} {}", spec.executable.display(), spec.args.join(" "));

    let session = ensure_session(&app, &task_id).await?;
    let native = session.native_session_id();
    let restored = state
        .store
        .load_task(&task_id)
        .map(|t| t.native_session_restored)
        .unwrap_or(false);
    let cached = state
        .probe_results
        .lock()
        .ok()
        .and_then(|c| c.get(&task.agent_id).cloned());

    Ok(SessionStartInfo {
        live: session.is_alive(),
        native_session_id: native,
        native_session_restored: restored,
        agent_info: cached.as_ref().and_then(|c| c.agent_info.clone()),
        auth_methods: cached.map(|c| c.auth_methods).unwrap_or_default(),
        capabilities: crate::model::AgentCapabilitiesReport::default(),
        detail: "会话已建立".to_string(),
        launch,
    })
}

#[derive(Debug, Clone, Serialize)]
pub struct PromptReply {
    pub stop_reason: String,
    pub error: Option<String>,
}

#[tauri::command]
pub async fn send_prompt(
    app: AppHandle,
    task_id: String,
    text: String,
) -> Result<PromptReply, String> {
    if text.trim().is_empty() {
        return Err("消息为空".to_string());
    }
    let session = ensure_session(&app, &task_id).await?;
    app.state::<AppState>().record_user_message(&task_id, &text);
    let outcome = session
        .prompt(vec![agent_client_protocol::schema::v1::ContentBlock::Text(
            agent_client_protocol::schema::v1::TextContent::new(text),
        )])
        .await?;
    Ok(PromptReply {
        stop_reason: outcome.stop_reason,
        error: outcome.error,
    })
}

#[tauri::command]
pub fn cancel_prompt(app: AppHandle, task_id: String) -> Result<(), String> {
    let state = app.state::<AppState>();
    let session = state
        .sessions
        .lock()
        .ok()
        .and_then(|s| s.get(&task_id).cloned())
        .ok_or_else(|| "当前没有活动会话，无法取消".to_string())?;
    session.cancel();
    Ok(())
}

#[tauri::command]
pub fn respond_permission(
    app: AppHandle,
    task_id: String,
    request_id: String,
    option_id: Option<String>,
) -> Result<bool, String> {
    let state = app.state::<AppState>();
    let outcome = option_id.clone().unwrap_or_else(|| "cancelled".to_string());
    // Resolve first so a duplicate click cannot grant twice.
    let resolved = state.permissions.resolve(&request_id, option_id.clone());
    state.record_event(
        &task_id,
        SessionEvent::PermissionResolved {
            request_id: request_id.clone(),
            outcome: outcome.clone(),
        },
    );
    state.record_event(
        &task_id,
        SessionEvent::Status {
            status: "permission_resolved".into(),
            message: format!("权限请求 {request_id} 已处理：{outcome}"),
        },
    );
    Ok(resolved)
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionStatus {
    pub live: bool,
    pub native_session_id: Option<String>,
    pub native_session_restored: bool,
    pub has_history: bool,
    pub pending_permissions: Vec<String>,
}

#[tauri::command]
pub fn session_status(state: State<'_, AppState>, task_id: String) -> SessionStatus {
    let live = state.is_live(&task_id);
    let task = state.store.load_task(&task_id);
    SessionStatus {
        live,
        native_session_id: task.as_ref().and_then(|t| t.native_session_id.clone()),
        native_session_restored: task
            .as_ref()
            .map(|t| t.native_session_restored)
            .unwrap_or(false),
        has_history: task.map(|t| !t.messages.is_empty()).unwrap_or(false),
        pending_permissions: state.permissions.pending_ids(),
    }
}

#[tauri::command]
pub fn stop_session(app: AppHandle, task_id: String) -> Result<(), String> {
    app.state::<AppState>().stop_session(&task_id);
    Ok(())
}

// ----------------------------------------------------------------- handoff

#[derive(Debug, Clone, Deserialize)]
pub struct HandoffDraftInput {
    pub to_agent: Option<String>,
    pub objective: String,
    #[serde(default)]
    pub requirements: Vec<RequirementRef>,
    #[serde(default)]
    pub forbidden: Vec<String>,
    #[serde(default)]
    pub completed: Vec<String>,
    #[serde(default)]
    pub decisions: Vec<String>,
    #[serde(default)]
    pub failed_attempts: Vec<String>,
    #[serde(default)]
    pub remaining: Vec<String>,
    #[serde(default)]
    pub next_steps: Vec<String>,
    /// Artifact ids already registered for this task.
    #[serde(default)]
    pub artifact_ids: Vec<String>,
    /// Paths to register as artifacts as part of preparing the handoff.
    #[serde(default)]
    pub register_paths: Vec<String>,
}

#[tauri::command]
pub fn prepare_handoff(
    state: State<'_, AppState>,
    task_id: String,
    draft: HandoffDraftInput,
) -> Result<Handoff, String> {
    let (mut task, project) = task_with_project(&state, &task_id)?;
    let root = project_root(&project);

    let mut artifacts_for_handoff: Vec<ArtifactRef> = task
        .handoff
        .as_ref()
        .map(|h| h.artifacts.clone())
        .unwrap_or_default();
    artifacts_for_handoff.retain(|a| draft.artifact_ids.contains(&a.id));

    for path in &draft.register_paths {
        let artifact = artifacts::register(
            &root,
            &crate::model::new_id("art"),
            path,
            "file",
            "交接登记",
            false,
        )?;
        artifacts_for_handoff.push(artifact);
    }

    let handoff = handoff::prepare(
        &task,
        &root,
        artifacts_for_handoff,
        HandoffDraft {
            to_agent: draft.to_agent,
            objective: draft.objective,
            requirements: draft.requirements,
            forbidden: draft.forbidden,
            completed: draft.completed,
            decisions: draft.decisions,
            failed_attempts: draft.failed_attempts,
            remaining: draft.remaining,
            next_steps: draft.next_steps,
        },
    );
    task.last_git = Some(handoff.git_evidence.clone());
    task.handoff = Some(handoff.clone());
    task.status = TaskStatus::HandoffPrepared;
    task.updated_at = now_iso();
    state.save_task(&task);
    Ok(handoff)
}

#[tauri::command]
pub fn verify_handoff(
    state: State<'_, AppState>,
    task_id: String,
) -> Result<Vec<HandoffVerificationCheck>, String> {
    let (task, project) = task_with_project(&state, &task_id)?;
    let handoff = task
        .handoff
        .ok_or_else(|| "该任务还没有交接资料".to_string())?;
    Ok(handoff::verify(&handoff, &project_root(&project)))
}

#[derive(Debug, Clone, Serialize)]
pub struct AcceptResult {
    pub accepted: bool,
    pub message: String,
    pub handoff: Handoff,
}

#[tauri::command]
pub fn accept_handoff(
    state: State<'_, AppState>,
    task_id: String,
    receiver_agent: String,
    note: String,
) -> Result<AcceptResult, String> {
    let (mut task, project) = task_with_project(&state, &task_id)?;
    let root = project_root(&project);
    // The receiving side records whether it restored the agent's native session;
    // LJ Hub reports this honestly instead of implying a model-level handoff.
    let native_restored =
        state.is_live(&task_id) && task.native_session_id.is_some() && task.native_session_restored;
    let outcome = handoff::accept(
        &state.store,
        &mut task,
        &root,
        &receiver_agent,
        &note,
        native_restored,
    )?;
    let handoff = task
        .handoff
        .clone()
        .ok_or_else(|| "交接记录丢失".to_string())?;
    Ok(AcceptResult {
        accepted: outcome.accepted,
        message: outcome.message,
        handoff,
    })
}

#[tauri::command]
pub fn handoff_markdown(state: State<'_, AppState>, task_id: String) -> Result<String, String> {
    let task = state
        .store
        .load_task(&task_id)
        .ok_or_else(|| format!("未知任务：{task_id}"))?;
    let handoff = task
        .handoff
        .ok_or_else(|| "该任务还没有交接资料".to_string())?;
    Ok(handoff::render_markdown(&handoff))
}

#[tauri::command]
pub fn register_artifact(
    state: State<'_, AppState>,
    task_id: String,
    path: String,
    kind: String,
    note: String,
    allow_large: bool,
) -> Result<ArtifactRef, String> {
    let (mut task, project) = task_with_project(&state, &task_id)?;
    let root = project_root(&project);
    let artifact = artifacts::register(
        &root,
        &crate::model::new_id("art"),
        &path,
        if kind.trim().is_empty() {
            "file"
        } else {
            &kind
        },
        &note,
        allow_large,
    )?;
    match task.handoff.as_mut() {
        Some(handoff) => handoff.artifacts.push(artifact.clone()),
        None => {
            let draft = HandoffDraft {
                objective: task.title.clone(),
                ..Default::default()
            };
            let mut handoff = handoff::prepare(&task, &root, vec![artifact.clone()], draft);
            handoff.stage = crate::model::HandoffStage::Prepared;
            task.handoff = Some(handoff);
        }
    }
    task.updated_at = now_iso();
    state.save_task(&task);
    Ok(artifact)
}

#[derive(Debug, Clone, Serialize)]
pub struct ArtifactVerification {
    pub id: String,
    pub accessible: bool,
    pub size: Option<u64>,
    pub sha256: Option<String>,
    pub matches_recorded: Option<bool>,
    pub detail: String,
}

#[tauri::command]
pub fn verify_artifact(
    state: State<'_, AppState>,
    task_id: String,
    artifact_id: String,
    force_hash: bool,
) -> Result<ArtifactVerification, String> {
    let task = state
        .store
        .load_task(&task_id)
        .ok_or_else(|| format!("未知任务：{task_id}"))?;
    let handoff = task
        .handoff
        .ok_or_else(|| "该任务还没有交接资料".to_string())?;
    let artifact = handoff
        .artifacts
        .iter()
        .find(|a| a.id == artifact_id)
        .ok_or_else(|| format!("未登记的产物：{artifact_id}"))?
        .clone();
    let result = artifacts::verify(&artifact, force_hash);
    Ok(ArtifactVerification {
        id: artifact.id,
        accessible: result.accessible,
        size: result.size,
        sha256: result.sha256,
        matches_recorded: result.matches_recorded,
        detail: result.detail,
    })
}

#[tauri::command]
pub fn remove_artifact(
    state: State<'_, AppState>,
    task_id: String,
    artifact_id: String,
) -> Result<(), String> {
    let mut task = state
        .store
        .load_task(&task_id)
        .ok_or_else(|| format!("未知任务：{task_id}"))?;
    if let Some(handoff) = task.handoff.as_mut() {
        handoff.artifacts.retain(|a| a.id != artifact_id);
    }
    state.save_task(&task);
    Ok(())
}

// ------------------------------------------ existing requirement documents

#[derive(Debug, Clone, Serialize)]
pub struct RequirementFile {
    pub path: String,
    pub exists: bool,
    pub content: Option<String>,
    pub size: Option<u64>,
    pub sha256: Option<String>,
    pub modified: Option<String>,
    /// LJ Hub only ever reads these files in v0.1.
    pub read_only: bool,
}

/// Read (never write) an existing requirements/handoff document, so LJ Hub can
/// reference the user's current AI workflow instead of migrating it.
#[tauri::command]
pub fn read_requirement_file(
    state: State<'_, AppState>,
    task_id: String,
    relative_path: String,
) -> Result<RequirementFile, String> {
    let (_task, project) = task_with_project(&state, &task_id)?;
    let root = project_root(&project);
    let path = if Path::new(&relative_path).is_absolute() {
        PathBuf::from(&relative_path)
    } else {
        root.join(&relative_path)
    };
    if !path.exists() {
        return Ok(RequirementFile {
            path: path.display().to_string(),
            exists: false,
            content: None,
            size: None,
            sha256: None,
            modified: None,
            read_only: true,
        });
    }
    let content = std::fs::read_to_string(&path).map_err(|e| format!("读取失败：{e}"))?;
    let (size, sha256) = artifacts::hash_file(&path)?;
    let modified = std::fs::metadata(&path)
        .ok()
        .and_then(|m| m.modified().ok())
        .map(|t| {
            let duration = t.duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
            chrono::DateTime::from_timestamp(duration.as_secs() as i64, 0)
                .map(|d| d.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
                .unwrap_or_default()
        });
    Ok(RequirementFile {
        path: path.display().to_string(),
        exists: true,
        content: Some(content),
        size: Some(size),
        sha256: Some(sha256),
        modified,
        read_only: true,
    })
}

/// Parse requirement lines out of an existing document (read-only import).
#[tauri::command]
pub fn import_requirements(
    state: State<'_, AppState>,
    task_id: String,
    relative_path: String,
) -> Result<Vec<RequirementRef>, String> {
    let file = read_requirement_file(state, task_id, relative_path)?;
    let content = file.content.unwrap_or_default();
    Ok(parse_requirements(&content, &file.path))
}

/// Recognize the common `- [P0] ID: text` / `- [P0] text` list style used by
/// existing AI workspaces, plus "## P0" headings.
pub fn parse_requirements(content: &str, source: &str) -> Vec<RequirementRef> {
    let mut out = Vec::new();
    let mut current_priority = String::new();
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("## ") {
            let heading = rest.trim();
            if heading.to_ascii_uppercase().contains('P')
                && heading.chars().any(|c| c.is_ascii_digit())
                && heading.len() <= 12
            {
                current_priority = heading.to_string();
            }
            continue;
        }
        let item = trimmed
            .strip_prefix("- ")
            .or_else(|| trimmed.strip_prefix("* "));
        let Some(item) = item else { continue };
        let (priority, text) = if item.starts_with('[') {
            match item.find(']') {
                Some(idx) => (item[1..idx].to_string(), item[idx + 1..].trim().to_string()),
                None => (current_priority.clone(), item.to_string()),
            }
        } else {
            (current_priority.clone(), item.to_string())
        };
        if text.is_empty() {
            continue;
        }
        let id = text
            .split([':', '：'])
            .next()
            .map(|s| s.trim())
            .filter(|s| s.len() <= 40 && !s.contains(' '))
            .unwrap_or("")
            .to_string();
        out.push(RequirementRef {
            id: if id.is_empty() {
                format!("req-{}", out.len() + 1)
            } else {
                id
            },
            summary: text,
            state: if priority.is_empty() {
                "unknown".to_string()
            } else {
                "active".to_string()
            },
            source: format!("{source}#{priority}"),
        });
    }
    out
}

// ---------------------------------------------------------------- settings

#[tauri::command]
pub fn get_settings(state: State<'_, AppState>) -> Settings {
    state
        .state
        .lock()
        .map(|s| s.settings.clone())
        .unwrap_or_default()
}

/// Remember where the user was, so the next launch reopens it.
#[tauri::command]
pub fn set_last_opened(
    state: State<'_, AppState>,
    project_id: Option<String>,
    task_id: Option<String>,
) -> Result<(), String> {
    let mut store = state.state.lock().map_err(|e| e.to_string())?;
    if let Some(project_id) = project_id {
        store.settings.last_project_id = Some(project_id);
    }
    store.settings.last_task_id = task_id;
    state.store.save_state(&store).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn set_theme(state: State<'_, AppState>, theme: String) -> Result<(), String> {
    let mut store = state.state.lock().map_err(|e| e.to_string())?;
    store.settings.theme = theme;
    state.store.save_state(&store).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn set_locale(state: State<'_, AppState>, locale: String) -> Result<(), String> {
    let mut store = state.state.lock().map_err(|e| e.to_string())?;
    store.settings.locale = locale;
    state.store.save_state(&store).map_err(|e| e.to_string())
}

fn shellexpand(path: &str) -> String {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(rest).display().to_string();
        }
    }
    path.to_string()
}

#[allow(dead_code)]
fn unused_types(_: Message, _: MessageBody, _: PermissionChoice) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requirements_are_parsed_from_list_style() {
        let content = "# 要求\n\n## P0\n\n- [P0] SEC-1: 不得上传密钥\n- 只有文本，没有编号\n\n## P1\n- [P1] UI-2: 保持轻量\n";
        let refs = parse_requirements(content, "USER_REQUIREMENTS.md");
        assert_eq!(refs.len(), 3);
        assert_eq!(refs[0].id, "SEC-1");
        assert_eq!(refs[0].state, "active");
        assert!(refs[0].source.contains("USER_REQUIREMENTS.md#P0"));
        assert_eq!(refs[2].source, "USER_REQUIREMENTS.md#P1");
    }

    #[test]
    fn empty_document_yields_no_requirements() {
        assert!(parse_requirements("", "x.md").is_empty());
        assert!(parse_requirements("普通段落\n没有列表", "x.md").is_empty());
    }

    #[test]
    fn home_paths_are_expanded() {
        let expanded = shellexpand("~/Documents");
        assert!(!expanded.starts_with('~'));
    }
}
