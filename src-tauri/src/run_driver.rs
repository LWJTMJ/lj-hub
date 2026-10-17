//! The relay driver: executes a [`Run`] step by step without any human
//! copy-pasting, and without any model deciding the control flow.
//!
//! Control flow lives in `scheduler.rs` (pure, testable). This module only
//! performs side effects: start an agent turn, record Git evidence, build a
//! handoff, ask the reviewer, and persist every transition.
//!
//! Guarantees:
//! - one action per tick, persisted immediately, so a crash loses at most the
//!   in-flight agent turn (which is then surfaced as a human decision);
//! - a step is never executed twice for the same attempt: `attempts` is
//!   incremented before the turn starts and the previous result is checked;
//! - the total number of agent turns per run is bounded by the policy;
//! - a reviewer's FAIL produces a *rework* prompt containing the concrete
//!   findings, bounded by `max_loops`, after which it stops for a human.

use crate::app::AppState;
use crate::handoff::{self, HandoffDraft};
use crate::model::{now_iso, TaskStatus};
use crate::runs::{is_driveable, RunStore};
use crate::scheduler::{
    compose_step_prompt, decide_review, parse_review_markers, planner_prompt, review_loop_state,
    reviewer_prompt, select_runnable, LoopOutcome, Run, RunStatus, SchedulingContext, StepMode,
    StepSpec, StepStatus, CONSUMED_STEP_MAX_CHARS,
};
use crate::sessions::{extract_step_summary, run_agent_turn, TurnRequest};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tauri::{AppHandle, Manager};

/// Pause between ticks. Short enough to feel immediate, long enough not to spin.
const TICK_PAUSE: Duration = Duration::from_millis(700);

#[derive(Debug, Clone, Default)]
pub struct TickReport {
    /// Something was actually done in this tick.
    pub progressed: bool,
    /// A short description of what happened (for the UI log).
    pub detail: String,
    /// Set when the run can make no further automatic progress.
    pub stop_reason: Option<String>,
}

fn store(state: &AppState) -> RunStore {
    RunStore::new(&state.data_dir)
}

fn load(state: &AppState, run_id: &str) -> Result<Run, String> {
    store(state).load(run_id)
}

fn save(state: &AppState, run: &mut Run) -> Result<(), String> {
    run.updated_at = now_iso();
    store(state).save(run)
}

fn log_activity(state: &AppState, run_id: &str, message: &str) {
    state.record_run_log(run_id, message);
    let _ = state.emit_run_event(run_id, message);
}

// ------------------------------------------------------------------ planning

/// Ask the planner agent for a plan and store it. Stays a Draft: the user
/// reviews the plan before anything executes.
pub async fn plan_run(state: &AppState, run_id: &str) -> Result<Run, String> {
    let mut run = load(state, run_id)?;
    if is_driveable(&run) && run.status == RunStatus::Running {
        return Err("运行已开始，不能重新规划".to_string());
    }
    let planner = run.agent.clone();
    let agents = available_agent_ids(state);
    let workspace = PathBuf::from(run.workspace.root.clone());
    if !workspace.is_dir() {
        return Err(format!(
            "运行的工作目录不可用：{}",
            if workspace.as_os_str().is_empty() {
                "(未设置)".to_string()
            } else {
                workspace.display().to_string()
            }
        ));
    }

    log_activity(state, run_id, &format!("规划中：由 {planner} 生成计划"));
    let plan_title = format!("{} · 规划", run.title);
    let planner_instruction = planner_prompt(&run.objective, &agents);
    let turn = run_agent_turn(
        state,
        None,
        TurnRequest {
            project_id: &run.project_id,
            agent_id: &planner,
            workspace: &workspace,
            title: &plan_title,
            prompt: &planner_instruction,
            timeout: Duration::from_secs(900),
            run_id: Some(run_id),
            step_id: Some("plan"),
        },
    )
    .await?;

    let plan = crate::scheduler::parse_plan(&turn.text)
        .map_err(|errors| format!("规划者没有输出可解析的计划：{}", errors.join("；")))?;
    let problems = crate::scheduler::validate_plan(&plan, &agents);
    if !problems.is_empty() {
        return Err(format!("规划结果未通过校验：{}", problems.join("；")));
    }

    let workspace_ref = run.workspace.clone();
    let steps = plan
        .steps
        .iter()
        .map(|spec| crate::scheduler::StepState::new(spec, workspace_ref.clone()))
        .collect();
    run.plan = plan;
    run.steps = steps;
    run.stop_reason = None;
    save(state, &mut run)?;
    log_activity(
        state,
        run_id,
        &format!(
            "计划已生成：{} 个阶段 / {} 个步骤",
            run.plan.phases.len(),
            run.plan.steps.len()
        ),
    );
    Ok(run)
}

/// Agents the relay may use: resolvable, not proven broken, and not known to
/// lack a working real turn. A handshake alone is not enough.
pub fn plan_agent_pool(state: &AppState) -> Vec<(String, bool)> {
    let overrides = state
        .state
        .lock()
        .map(|s| s.settings.agent_overrides.clone())
        .unwrap_or_default();
    let cache = state
        .probe_results
        .lock()
        .map(|c| c.clone())
        .unwrap_or_default();
    crate::agents::AGENTS
        .iter()
        .filter(|def| !def.compatibility_only)
        .filter_map(|def| {
            let over = overrides.get(def.id);
            let resolvable = crate::agents::resolve_executable(def, over).is_some()
                || over
                    .and_then(|o| o.executable.as_deref())
                    .map(|e| !e.trim().is_empty())
                    .unwrap_or(false);
            if !resolvable {
                return None;
            }
            let verified = cache.get(def.id);
            if let Some(report) = verified {
                if report.verification.real_turn_ok == Some(false) {
                    // Proven unable to run a real turn: never schedule it.
                    return None;
                }
            }
            let schedulable = verified
                .map(|r| r.verification.schedulable)
                .unwrap_or(false);
            Some((def.id.to_string(), schedulable))
        })
        .collect()
}

fn available_agent_ids(state: &AppState) -> Vec<String> {
    let verified = state
        .probe_results
        .lock()
        .map(|cache| {
            cache
                .iter()
                .filter(|(_, report)| report.status == crate::model::AgentStatus::Available)
                .map(|(id, _)| id.clone())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let overrides = state
        .state
        .lock()
        .map(|s| s.settings.agent_overrides.clone())
        .unwrap_or_default();
    let mut ids: Vec<String> = crate::agents::AGENTS
        .iter()
        .filter(|def| {
            if def.compatibility_only {
                return false;
            }
            let over = overrides.get(def.id);
            // Usable = resolvable on this machine, or explicitly configured by
            // the user, or already proven by a real handshake (added below).
            crate::agents::resolve_executable(def, over).is_some()
                || over
                    .map(|o| {
                        o.executable
                            .as_deref()
                            .map(|e| !e.trim().is_empty())
                            .unwrap_or(false)
                    })
                    .unwrap_or(false)
        })
        .map(|def| def.id.to_string())
        .collect();
    for id in verified {
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    ids
}

// --------------------------------------------------------------------- ticks

/// Perform at most one scheduling action. Returns what happened.
pub async fn tick(state: &AppState, run_id: &str) -> Result<TickReport, String> {
    let mut run = load(state, run_id)?;
    if matches!(run.status, RunStatus::Cancelled | RunStatus::Done) {
        return Ok(TickReport {
            progressed: false,
            detail: "运行已结束".into(),
            stop_reason: Some(format!("{:?}", run.status)),
        });
    }
    if run.plan.steps.is_empty() {
        return Ok(TickReport {
            progressed: false,
            detail: "尚无计划".into(),
            stop_reason: Some("运行还没有计划：请先生成或导入计划".into()),
        });
    }
    if run.status == RunStatus::Draft {
        run.status = RunStatus::Running;
        save(state, &mut run)?;
    }
    if run.executions() >= run.policy.max_step_executions {
        run.status = RunStatus::NeedsHuman;
        run.stop_reason = Some(format!(
            "已达到本次运行的最大执行次数（{}），停止以避免无界消耗",
            run.policy.max_step_executions
        ));
        save(state, &mut run)?;
        log_activity(state, run_id, run.stop_reason.as_deref().unwrap_or(""));
        return Ok(TickReport {
            progressed: false,
            detail: "达到执行上限".into(),
            stop_reason: run.stop_reason.clone(),
        });
    }

    // 1. Finish the review of a step that is waiting for its reviewer.
    if let Some(step_id) = run
        .steps
        .iter()
        .find(|s| s.status == StepStatus::AwaitingHandoff)
        .map(|s| s.id.clone())
    {
        let detail = review_step(state, &mut run, &step_id).await?;
        save(state, &mut run)?;
        run.refresh_status();
        save(state, &mut run)?;
        return Ok(TickReport {
            progressed: true,
            detail,
            stop_reason: None,
        });
    }

    // 2. Start the next runnable step.
    let ctx = SchedulingContext {
        write_busy: run
            .steps
            .iter()
            .any(|s| s.status.is_active() && step_is_write(&run, &s.id)),
    };
    let runnable = select_runnable(&run, &ctx);
    if let Some(next) = runnable.first() {
        let detail = execute_step(state, &mut run, &next.step_id).await?;
        save(state, &mut run)?;
        run.refresh_status();
        save(state, &mut run)?;
        return Ok(TickReport {
            progressed: true,
            detail,
            stop_reason: None,
        });
    }

    // 3. Nothing to run: explain why we stopped.
    run.refresh_status();
    let blocked: Vec<String> = run
        .steps
        .iter()
        .filter(|s| s.status == StepStatus::NeedsHuman)
        .map(|s| s.id.clone())
        .collect();
    let failed: Vec<String> = run
        .steps
        .iter()
        .filter(|s| s.status == StepStatus::Failed)
        .map(|s| s.id.clone())
        .collect();
    let stop_reason = if !blocked.is_empty() {
        Some(format!("等待人工处理：{}", blocked.join(", ")))
    } else if !failed.is_empty() {
        Some(format!("存在失败且不再重试的步骤：{}", failed.join(", ")))
    } else if run.status == RunStatus::Done {
        Some("全部步骤已完成".to_string())
    } else {
        Some("没有可执行的步骤（依赖或阶段门未满足）".to_string())
    };
    run.stop_reason = stop_reason.clone();
    if blocked.is_empty() && failed.is_empty() && run.status == RunStatus::Running {
        // Nothing runnable but not finished: treat as blocked on dependencies,
        // which needs a human too (a plan bug rather than an agent failure).
        run.status = RunStatus::NeedsHuman;
    }
    save(state, &mut run)?;
    log_activity(state, run_id, stop_reason.as_deref().unwrap_or("运行停止"));
    Ok(TickReport {
        progressed: false,
        detail: "无自动可推进项".into(),
        stop_reason,
    })
}

fn step_is_write(run: &Run, step_id: &str) -> bool {
    run.plan
        .step(step_id)
        .map(|s| s.mode == StepMode::Write)
        .unwrap_or(false)
}

/// Background worker: drives a run until it can no longer progress on its own.
pub fn start_worker(ui: AppHandle, run_id: String, stop: Arc<AtomicBool>) {
    tauri::async_runtime::spawn(async move {
        let state = ui.state::<AppState>();
        loop {
            if stop.load(Ordering::SeqCst) {
                break;
            }
            let still_registered = state
                .run_workers
                .lock()
                .map(|workers| workers.contains(&run_id))
                .unwrap_or(false);
            if !still_registered {
                break;
            }
            let report = match tick(&state, &run_id).await {
                Ok(report) => report,
                Err(error) => {
                    log_activity(&state, &run_id, &format!("驱动出错：{error}"));
                    TickReport {
                        progressed: false,
                        detail: error,
                        stop_reason: Some("驱动出错，已停止自动推进".into()),
                    }
                }
            };
            if !report.progressed {
                break;
            }
            tokio::time::sleep(TICK_PAUSE).await;
        }
        if let Ok(mut workers) = state.run_workers.lock() {
            workers.remove(&run_id);
        }
        let _ = state.emit_run_event(&run_id, "自动推进已暂停");
    });
}

// -------------------------------------------------------------- step execution

pub fn run_project_root(state: &AppState, project_id: &str) -> Result<PathBuf, String> {
    let project = state
        .state
        .lock()
        .ok()
        .and_then(|s| s.projects.iter().find(|p| p.id == project_id).cloned())
        .ok_or_else(|| format!("未知项目：{project_id}"))?;
    Ok(crate::commands::project_root_for(&project))
}

/// What still has to happen after this step, taken from the plan itself.
///
/// This is what makes a handoff useful to the next agent: instead of a generic
/// "continue", the receiver sees the concrete downstream steps and who runs
/// them. Steps already finished are not listed as remaining work.
fn downstream_summary(run: &Run, step_id: &str) -> (Vec<String>, Vec<String>) {
    let mut dependents: Vec<String> = run
        .plan
        .steps
        .iter()
        .filter(|spec| spec.id != step_id && spec.depends_on.iter().any(|d| d == step_id))
        .filter(|spec| {
            run.state(&spec.id)
                .map(|s| s.status != StepStatus::Done)
                .unwrap_or(true)
        })
        .map(|spec| format!("{}（由 {} 执行）", spec.title, spec.agent))
        .collect();

    if dependents.is_empty() {
        // No explicit dependency: anything later in the phase order counts.
        let my_phase_index = run.plan.phases.iter().position(|p| {
            p.id == run
                .plan
                .step(step_id)
                .map(|s| s.phase.clone())
                .unwrap_or_default()
        });
        for (index, phase) in run.plan.phases.iter().enumerate() {
            if Some(index) <= my_phase_index {
                continue;
            }
            for spec in run.plan.steps.iter().filter(|s| s.phase == phase.id) {
                if run
                    .state(&spec.id)
                    .map(|s| s.status != StepStatus::Done)
                    .unwrap_or(true)
                {
                    dependents.push(format!("{}（由 {} 执行）", spec.title, spec.agent));
                }
            }
        }
    }
    dependents.truncate(10);

    if dependents.is_empty() {
        return (
            vec!["本步骤是该计划的最后一个步骤".to_string()],
            vec!["等待用户确认整体结果".to_string()],
        );
    }
    let next_steps = dependents.clone();
    (dependents, next_steps)
}

fn outputs_from_run(run: &Run) -> BTreeMap<String, (String, String)> {
    let mut outputs = BTreeMap::new();
    for state in &run.steps {
        if let Some(summary) = state.summary.clone() {
            let title = run
                .plan
                .step(&state.id)
                .map(|s| s.title.clone())
                .unwrap_or_else(|| state.id.clone());
            outputs.insert(state.id.clone(), (title, summary));
        }
    }
    outputs
}

async fn execute_step(state: &AppState, run: &mut Run, step_id: &str) -> Result<String, String> {
    let spec: StepSpec = run
        .plan
        .step(step_id)
        .cloned()
        .ok_or_else(|| format!("计划缺少步骤 {step_id}"))?;
    let workspace = run
        .state(step_id)
        .map(|s| PathBuf::from(s.workspace.root.clone()))
        .ok_or_else(|| format!("步骤 {step_id} 没有工作区"))?;
    if !workspace.is_dir() {
        let message = format!("工作目录不存在：{}", workspace.display());
        run.transition(step_id, StepStatus::NeedsHuman, message.clone());
        if let Some(state) = run.state_mut(step_id) {
            state.needs_human_reason = Some(message.clone());
        }
        return Ok(message);
    }

    // Record the attempt *before* running, so a crash cannot look like a step
    // that never ran.
    let attempt = {
        let state = run.state_mut(step_id).expect("checked above");
        state.attempts += 1;
        state.started_at = Some(now_iso());
        state.failure_reason = None;
        state.review_status = None;
        state.unresolved = None;
        state.attempts
    };
    let start_evidence = crate::git::evidence(&workspace);
    if let Some(state) = run.state_mut(step_id) {
        state.git = Some(start_evidence.clone());
    }
    let findings = run
        .state(step_id)
        .map(|s| s.findings.clone())
        .unwrap_or_default();
    run.transition(
        step_id,
        StepStatus::Running,
        format!("第 {attempt} 次尝试，由 {} 执行", spec.agent),
    );
    log_activity(
        state,
        &run.id,
        &format!(
            "步骤 {} 开始（{} · 第 {} 次）",
            spec.title, spec.agent, attempt
        ),
    );

    let prompt = compose_step_prompt(run, &spec, &outputs_from_run(run), &findings);

    let timeout = Duration::from_secs(spec.timeout_secs.max(30));
    let step_title = format!("{} · {}", run.title, spec.title);
    let turn = match run_agent_turn(
        state,
        None,
        TurnRequest {
            project_id: &run.project_id,
            agent_id: &spec.agent,
            workspace: &workspace,
            title: &step_title,
            prompt: &prompt,
            timeout,
            run_id: Some(&run.id),
            step_id: Some(step_id),
        },
    )
    .await
    {
        Ok(turn) => turn,
        Err(error) => return Ok(record_step_failure(state, run, step_id, &spec, &error)),
    };

    let summary = extract_step_summary(&turn.text)
        .unwrap_or_else(|| crate::scheduler::bound_text(&turn.text, 2_000));
    let end_evidence = crate::git::evidence(&workspace);
    let changed = end_evidence.content_fingerprint != start_evidence.content_fingerprint;

    if turn.timed_out {
        let reason = format!(
            "执行超过 {} 秒未结束，已请求取消。工作区{}发生变化。",
            spec.timeout_secs,
            if changed { "已" } else { "未" }
        );
        if let Some(state) = run.state_mut(step_id) {
            state.git = Some(end_evidence);
            state.task_id = Some(turn.task_id.clone());
        }
        return Ok(record_step_failure(state, run, step_id, &spec, &reason));
    }

    // Handoff material: prepared by the sender, and verified by the receiver
    // before the reviewer step accepts anything.
    let (downstream_remaining, downstream_next) = downstream_summary(run, step_id);
    let handoff = {
        let mut task = state
            .store
            .load_task(&turn.task_id)
            .ok_or_else(|| "任务记录丢失".to_string())?;
        let mut remaining = Vec::new();
        let mut next_steps = Vec::new();
        if let Some(verifier) = spec.verify_by.as_ref() {
            remaining.push(format!("等待 {verifier} 验收本步骤"));
            next_steps.push(format!("由 {verifier} 验收：检查 Git/文件事实后再继续"));
        }
        remaining.extend(downstream_remaining.clone());
        next_steps.extend(downstream_next.clone());
        let draft = HandoffDraft {
            to_agent: spec.verify_by.clone(),
            objective: format!("{}：{}", run.objective, spec.title),
            requirements: run.requirements.clone(),
            forbidden: run.forbidden.clone(),
            completed: vec![summary.clone()],
            decisions: Vec::new(),
            failed_attempts: findings.clone(),
            remaining,
            next_steps,
        };
        let handoff = handoff::prepare(&task, &workspace, Vec::new(), draft);
        task.handoff = Some(handoff.clone());
        task.last_git = Some(end_evidence.clone());
        task.status = TaskStatus::HandoffPrepared;
        task.updated_at = now_iso();
        state.save_task(&task);
        handoff
    };

    let verify_required = spec.verify_by.is_some();
    {
        let state = run.state_mut(step_id).expect("checked above");
        state.task_id = Some(turn.task_id.clone());
        state.summary = Some(summary.clone());
        state.git = Some(end_evidence.clone());
        state.handoff_id = Some(handoff.id.clone());
    }
    // Verification happens in the driver (Git/content checks), not by the agent.
    let checks = handoff::verify(&handoff, &workspace);
    let failed_checks: Vec<String> = checks
        .iter()
        .filter(|c| !c.passed)
        .map(|c| format!("{}：{}", c.name, c.detail))
        .collect();

    if !failed_checks.is_empty() {
        // The material the sender produced does not describe the repository.
        // Never let a reviewer accept that: surface it immediately.
        let reason = format!("交接自检未通过：{}", failed_checks.join("；"));
        if let Some(state) = run.state_mut(step_id) {
            state.needs_human_reason = Some(reason.clone());
        }
        run.transition(step_id, StepStatus::NeedsHuman, reason.clone());
        log_activity(state, &run.id, &format!("步骤 {} {reason}", spec.title));
        return Ok(reason);
    }

    if verify_required {
        run.transition(
            step_id,
            StepStatus::AwaitingHandoff,
            format!(
                "交接资料已生成，等待 {} 验收（changed={}）",
                spec.verify_by.as_deref().unwrap_or("验收者"),
                changed
            ),
        );
        log_activity(
            state,
            &run.id,
            &format!("步骤 {} 完成，等待验收", spec.title),
        );
    } else {
        run.transition(
            step_id,
            StepStatus::Done,
            format!(
                "完成（无验收者；工作区{}变化）",
                if changed { "已" } else { "未" }
            ),
        );
        log_activity(state, &run.id, &format!("步骤 {} 完成", spec.title));
    }
    Ok(format!("步骤 {} 执行结束", spec.title))
}

/// Errors that mean "this agent cannot run a real turn right now", not "this
/// particular step had bad luck". Recorded so the relay stops choosing it and
/// the UI can explain why.
fn looks_like_credential_failure(message: &str) -> bool {
    let lower = message.to_lowercase();
    [
        "no api key",
        "api key",
        "unauthorized",
        "authentication",
        "invalid_token",
        "401",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

fn record_agent_unusable(state: &AppState, agent_id: &str, error: &str) {
    if let Ok(mut cache) = state.probe_results.lock() {
        let entry = cache.entry(agent_id.to_string()).or_insert_with(|| {
            crate::agents::base_report(
                crate::agents::find_definition(agent_id).unwrap_or(&crate::agents::AGENTS[0]),
            )
        });
        entry.verification.real_turn_ok = Some(false);
        entry.verification.real_turn_error = Some(error.to_string());
        entry.verification.schedulable = false;
        entry.verification.real_turn_at = Some(now_iso());
    }
    log_activity(
        state,
        "",
        &format!("Agent {agent_id} 的真实回合失败，已标记为不可用于自动接力：{error}"),
    );
}

fn record_step_failure(
    state: &AppState,
    run: &mut Run,
    step_id: &str,
    spec: &StepSpec,
    reason: &str,
) -> String {
    if looks_like_credential_failure(reason) {
        record_agent_unusable(state, &spec.agent, reason);
    }
    let attempts = run.state(step_id).map(|s| s.attempts).unwrap_or(1);
    if attempts < spec.max_attempts.max(1) {
        run.transition(
            step_id,
            StepStatus::Failed,
            format!("第 {attempts} 次尝试失败：{reason}（将重试）"),
        );
        if let Some(state) = run.state_mut(step_id) {
            state.status = StepStatus::Pending; // eligible again next tick
            state.failure_reason = Some(reason.to_string());
        }
        log_activity(
            state,
            &run.id,
            &format!(
                "步骤 {} 第 {} 次失败，将重试：{reason}",
                spec.title, attempts
            ),
        );
    } else {
        run.transition(
            step_id,
            StepStatus::NeedsHuman,
            format!("已用尽 {attempts} 次尝试：{reason}"),
        );
        if let Some(state) = run.state_mut(step_id) {
            state.failure_reason = Some(reason.to_string());
            state.needs_human_reason = Some(format!(
                "步骤 {} 连续 {attempts} 次失败，已停止自动重试：{reason}",
                spec.title
            ));
        }
        log_activity(
            state,
            &run.id,
            &format!("步骤 {} 已停止自动重试：{reason}", spec.title),
        );
    }
    format!("步骤 {} 失败：{reason}", spec.title)
}

// ------------------------------------------------------------------ reviewing

async fn review_step(state: &AppState, run: &mut Run, step_id: &str) -> Result<String, String> {
    let spec: StepSpec = run
        .plan
        .step(step_id)
        .cloned()
        .ok_or_else(|| format!("计划缺少步骤 {step_id}"))?;
    let Some(reviewer) = spec.verify_by.clone() else {
        run.transition(step_id, StepStatus::Done, "没有验收者");
        return Ok("无需验收".into());
    };
    let workspace = run
        .state(step_id)
        .map(|s| PathBuf::from(s.workspace.root.clone()))
        .ok_or_else(|| format!("步骤 {step_id} 没有工作区"))?;

    run.transition(
        step_id,
        StepStatus::Verifying,
        format!("由 {reviewer} 验收"),
    );

    // Re-check the sender's evidence against the repository *now*: if the tree
    // moved on since the handoff was prepared, the reviewer would be judging
    // stale material. Refuse instead of pretending.
    let handoff = {
        run.state(step_id)
            .and_then(|s| s.task_id.clone())
            .and_then(|task_id| state.store.load_task(&task_id))
            .and_then(|task| task.handoff)
    };
    let Some(handoff) = handoff else {
        let reason = "找不到交接资料，无法验收".to_string();
        run.transition(step_id, StepStatus::NeedsHuman, reason.clone());
        if let Some(state) = run.state_mut(step_id) {
            state.needs_human_reason = Some(reason.clone());
        }
        return Ok(reason);
    };

    let checks = handoff::verify(&handoff, &workspace);
    let failed: Vec<String> = checks
        .iter()
        .filter(|c| !c.passed)
        .map(|c| format!("{}：{}", c.name, c.detail))
        .collect();
    if !failed.is_empty() {
        let reason = format!("验收前发现工作区已漂移，拒绝接手：{}", failed.join("；"));
        run.transition(step_id, StepStatus::NeedsHuman, reason.clone());
        if let Some(state) = run.state_mut(step_id) {
            state.needs_human_reason = Some(reason.clone());
            state.findings = failed.clone();
        }
        log_activity(state, &run.id, &reason);
        return Ok(reason);
    }

    let git_summary = {
        let evidence = &run
            .state(step_id)
            .and_then(|s| s.git.clone())
            .unwrap_or_else(|| crate::git::evidence(&workspace));
        format!(
            "HEAD: {}\n分支: {}\n未提交改动: {}\n内容指纹: {}\n覆盖范围: {}",
            evidence.head.as_deref().unwrap_or("(无)"),
            evidence.branch.as_deref().unwrap_or("(无)"),
            if evidence.dirty {
                evidence.dirty_files.join("; ")
            } else {
                "无".into()
            },
            evidence
                .content_fingerprint
                .as_deref()
                .unwrap_or("(未记录)"),
            evidence.fingerprint_detail.as_deref().unwrap_or("(未知)")
        )
    };

    let markdown = handoff::render_markdown(&handoff);
    let prompt = reviewer_prompt(
        &spec.title,
        &crate::scheduler::bound_text(&markdown, CONSUMED_STEP_MAX_CHARS),
        &git_summary,
    );

    let review_title = format!("{} · 验收 {}", run.title, spec.title);
    let review_step_id = format!("{step_id}#review");
    let turn = match run_agent_turn(
        state,
        None,
        TurnRequest {
            project_id: &run.project_id,
            agent_id: &reviewer,
            workspace: &workspace,
            title: &review_title,
            prompt: &prompt,
            timeout: Duration::from_secs(spec.timeout_secs.max(30)),
            run_id: Some(&run.id),
            step_id: Some(&review_step_id),
        },
    )
    .await
    {
        Ok(turn) => turn,
        Err(error) => {
            let reason = format!("验收回合失败：{error}");
            run.transition(step_id, StepStatus::NeedsHuman, reason.clone());
            if let Some(state) = run.state_mut(step_id) {
                state.needs_human_reason = Some(reason.clone());
            }
            return Ok(reason);
        }
    };

    let (status, unresolved) = parse_review_markers(&turn.text);
    let loops_done = run.state(step_id).map(|s| s.loops).unwrap_or(0);
    let reviewer_ok = status.is_some();

    if let Some(state) = run.state_mut(step_id) {
        state.review_status = status;
        state.unresolved = unresolved;
    }

    match decide_review(status, reviewer_ok, loops_done, spec.max_loops) {
        LoopOutcome::Finish => {
            run.transition(step_id, StepStatus::Done, "验收通过");
            if let Some(state) = run.state_mut(step_id) {
                state.finished_at = Some(now_iso());
                state.findings.clear();
            }
            log_activity(state, &run.id, &format!("步骤 {} 验收通过", spec.title));
            Ok(format!("步骤 {} 验收通过", spec.title))
        }
        LoopOutcome::Loop => {
            let findings = extract_findings(&turn.text);
            let loops = loops_done + 1;
            if let Some(state) = run.state_mut(step_id) {
                state.loops = loops;
                state.findings = findings.clone();
                state.status = StepStatus::Pending;
                state.started_at = None;
            }
            run.history.push(crate::scheduler::RunStepEvent {
                at: now_iso(),
                step_id: step_id.to_string(),
                from: StepStatus::Verifying,
                to: StepStatus::Pending,
                detail: format!(
                    "验收未通过（第 {loops} 轮），生成返工指令：{}",
                    findings.join("；")
                ),
            });
            log_activity(
                state,
                &run.id,
                &format!(
                    "步骤 {} 验收未通过（第 {loops}/{} 轮），已自动生成返工任务",
                    spec.title, spec.max_loops
                ),
            );
            Ok(format!("验收未通过，已安排返工（第 {loops} 轮）"))
        }
        LoopOutcome::Partial => {
            let reason = format!(
                "验收未通过且已达到循环上限（{} 轮），停止自动返工，等待人工处理。\n{}",
                spec.max_loops,
                review_loop_state(&turn.text)
            );
            if let Some(state) = run.state_mut(step_id) {
                state.needs_human_reason = Some(reason.clone());
                state.findings = extract_findings(&turn.text);
            }
            run.transition(step_id, StepStatus::NeedsHuman, reason.clone());
            log_activity(state, &run.id, &reason);
            Ok(reason)
        }
    }
}

/// Keep the reviewer's concrete complaints; they become the rework prompt.
fn extract_findings(text: &str) -> Vec<String> {
    let mut findings: Vec<String> = text
        .lines()
        .map(|line| line.trim())
        .filter(|line| {
            (line.starts_with('-') || line.starts_with('*') || line.starts_with("1."))
                && line.len() > 3
        })
        .map(|line| {
            line.trim_start_matches(['-', '*', ' ', '.', '1'])
                .trim()
                .to_string()
        })
        .filter(|line| !line.is_empty())
        .collect();
    if findings.is_empty() {
        let trimmed = text.trim();
        if !trimmed.is_empty() {
            findings.push(crate::scheduler::bound_text(trimmed, 1_500));
        }
    }
    findings.truncate(20);
    findings
}

// -------------------------------------------------------------- user actions

/// Resume a run that stopped for a human: clear the blocker and continue.
pub fn resolve_needs_human(
    state: &AppState,
    run_id: &str,
    step_id: &str,
    action: &str,
    note: &str,
) -> Result<Run, String> {
    let mut run = load(state, run_id)?;
    let target = run
        .state_mut(step_id)
        .ok_or_else(|| format!("未知步骤：{step_id}"))?;
    match action {
        "retry" => {
            target.status = StepStatus::Pending;
            target.needs_human_reason = None;
            target.failure_reason = None;
            target.attempts = 0;
        }
        "mark_done" => {
            target.status = StepStatus::Done;
            target.finished_at = Some(now_iso());
            target.needs_human_reason = None;
        }
        "cancel" => {
            target.status = StepStatus::Cancelled;
            target.finished_at = Some(now_iso());
            target.needs_human_reason = None;
        }
        other => return Err(format!("不支持的处置方式：{other}")),
    }
    let new_status = target.status;
    run.history.push(crate::scheduler::RunStepEvent {
        at: now_iso(),
        step_id: step_id.to_string(),
        from: StepStatus::NeedsHuman,
        to: new_status,
        detail: format!("人工处置：{action} {note}"),
    });
    run.stop_reason = None;
    run.refresh_status();
    save(state, &mut run)?;
    log_activity(state, run_id, &format!("人工处置步骤 {step_id}：{action}"));
    Ok(run)
}

pub fn cancel_run(state: &AppState, run_id: &str) -> Result<Run, String> {
    let mut run = load(state, run_id)?;
    for step_state in run.steps.iter_mut() {
        if !matches!(
            step_state.status,
            StepStatus::Done | StepStatus::Cancelled | StepStatus::Failed
        ) {
            let from = step_state.status;
            step_state.status = StepStatus::Cancelled;
            step_state.finished_at = Some(now_iso());
            run.history.push(crate::scheduler::RunStepEvent {
                at: now_iso(),
                step_id: step_state.id.clone(),
                from,
                to: StepStatus::Cancelled,
                detail: "用户取消运行".into(),
            });
        }
        // Stop any live session belonging to this step too.
        if let Some(task_id) = step_state.task_id.clone() {
            state.stop_session(&task_id);
        }
    }
    run.status = RunStatus::Cancelled;
    run.stop_reason = Some("用户取消".to_string());
    save(state, &mut run)?;
    if let Ok(mut workers) = state.run_workers.lock() {
        workers.remove(run_id);
    }
    log_activity(state, run_id, "运行已取消");
    Ok(run)
}
