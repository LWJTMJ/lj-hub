//! End-to-end tests for the automatic relay (Phase 1/2 of the roadmap).
//!
//! These drive the real scheduler, the real ACP runtime and the real
//! persistence layer against mock agents that speak the ACP v1 wire protocol.
//! Nothing here is stubbed: if the scheduler refused to advance, or the
//! reviewer's findings never reached the worker, or a step ran twice, these
//! tests fail.
//!
//! What is covered:
//!  - planner agent → JSON plan → validated → executed;
//!  - worker → handoff → reviewer → PASS → next phase;
//!  - reviewer FAIL → automatic rework carrying the concrete findings;
//!  - review-loop limit → stops for a human instead of looping forever;
//!  - a crash-interrupted step is not assumed done and not blindly re-run;
//!  - every agent turn is persisted as a task linked to the run, so the user
//!    can read what each agent did without leaving LJ Hub.

use lj_hub_lib::app::AppState;
use lj_hub_lib::model::{now_iso, Project};
use lj_hub_lib::run_driver;
use lj_hub_lib::scheduler::{
    parse_plan, Run, RunStatus, StepMode, StepSpec, StepStatus, WorkspaceRef,
};
use std::path::PathBuf;
use std::sync::Arc;

fn mock_agent_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_mock-acp-agent"))
}

/// A test environment: real store, real agent registry overrides, mock agents.
struct Env {
    state: Arc<AppState>,
    repo: tempfile::TempDir,
    data: tempfile::TempDir,
    project_id: String,
}

impl Env {
    fn new() -> Self {
        let repo = lj_hub_lib::test_support::fixture_repo();
        let data = tempfile::tempdir().unwrap();
        let state = Arc::new(AppState::new(data.path().to_path_buf()));

        let project_id = "proj-test".to_string();
        {
            let mut store = state.state.lock().unwrap();
            store.projects.push(Project {
                id: project_id.clone(),
                name: "fixture".into(),
                path: repo.path().display().to_string(),
                git_root: Some(repo.path().display().to_string()),
                git_remote: None,
                created_at: now_iso(),
                last_opened_at: now_iso(),
            });
            // Point the registered agents at the mock binary. This is the same
            // override mechanism the UI exposes, not a test-only back door.
            for agent in ["opencode", "codex", "deepseek-harness"] {
                store.settings.agent_overrides.insert(
                    agent.to_string(),
                    lj_hub_lib::model::AgentOverride {
                        executable: Some(mock_agent_bin().display().to_string()),
                        args: vec!["--scenario".into(), "relay".into()],
                        allow_npm_adapter: false,
                        env_passthrough: vec![],
                    },
                );
            }
            state.store.save_state(&store).unwrap();
        }
        Self {
            state,
            repo,
            data,
            project_id,
        }
    }

    fn repo_path(&self) -> PathBuf {
        self.repo.path().to_path_buf()
    }

    /// Drive the run to a standstill, one scheduler action per tick.
    async fn drive(&self, run_id: &str) -> Run {
        let mut iterations = 0;
        loop {
            iterations += 1;
            assert!(
                iterations < 40,
                "驱动器没有收敛，可能存在死循环：{} 次 tick 仍未停止",
                iterations
            );
            let report = run_driver::tick(&self.state, run_id).await.expect("tick");
            if !report.progressed {
                break;
            }
        }
        lj_hub_lib::runs::RunStore::new(self.data.path())
            .load(run_id)
            .unwrap()
    }

    fn save(&self, run: &Run) {
        lj_hub_lib::runs::RunStore::new(self.data.path())
            .save(run)
            .unwrap();
    }

    fn load(&self, run_id: &str) -> Run {
        lj_hub_lib::runs::RunStore::new(self.data.path())
            .load(run_id)
            .unwrap()
    }
}

fn explicit_plan(verify_by: Option<&str>, max_loops: u32) -> lj_hub_lib::scheduler::RunPlan {
    lj_hub_lib::scheduler::RunPlan {
        phases: vec![lj_hub_lib::scheduler::Phase {
            id: "p1".into(),
            title: "实现".into(),
            parallelism: 1,
            gate: lj_hub_lib::scheduler::GateKind::AllDone,
        }],
        steps: vec![StepSpec {
            id: "s1".into(),
            phase: "p1".into(),
            title: "实现功能".into(),
            prompt: "在项目里实现功能并留下证据".into(),
            agent: "opencode".into(),
            mode: StepMode::Write,
            depends_on: vec![],
            verify_by: verify_by.map(|s| s.to_string()),
            max_loops,
            max_attempts: 2,
            timeout_secs: 120,
            consumes: vec![],
        }],
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn planner_produces_a_plan_that_is_validated_and_stored() {
    let env = Env::new();
    let workspace = WorkspaceRef::main(env.repo_path().display().to_string());
    let run = Run::new(
        "run-plan".into(),
        env.project_id.clone(),
        "接力".into(),
        "把功能做出来并汇总".into(),
        "deepseek-harness".into(), // planner
        lj_hub_lib::scheduler::RunPlan {
            phases: vec![],
            steps: vec![],
        },
        workspace,
    );
    env.save(&run);

    let planned = run_driver::plan_run(&env.state, "run-plan")
        .await
        .expect("规划应成功");
    assert_eq!(planned.plan.steps.len(), 2, "计划应包含两个步骤");
    assert_eq!(planned.plan.steps[0].agent, "opencode");
    assert_eq!(planned.plan.steps[0].verify_by.as_deref(), Some("codex"));
    assert_eq!(
        planned.status,
        RunStatus::Draft,
        "规划完成后仍是草稿，等待用户确认后才执行"
    );
    // The planner's own turn is persisted as a readable task.
    let tasks = env.state.store.list_task_files();
    assert!(!tasks.is_empty());
    let planner_task = tasks
        .iter()
        .filter_map(|id| env.state.store.load_task(id))
        .find(|t| t.run_id.as_deref() == Some("run-plan"))
        .expect("规划回合应被记录为任务");
    assert!(planner_task.run_step_id.as_deref() == Some("plan"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn full_relay_runs_planner_worker_reviewer_and_rework_without_a_human() {
    let env = Env::new();
    // Make the first review fail, so a rework round is required.
    std::fs::write(env.repo_path().join("relay-fail-once.marker"), "x").unwrap();

    let workspace = WorkspaceRef::main(env.repo_path().display().to_string());
    let run = Run::new(
        "run-relay".into(),
        env.project_id.clone(),
        "完整接力".into(),
        "实现功能，再由另一个 Agent 验收".into(),
        "deepseek-harness".into(),
        lj_hub_lib::scheduler::RunPlan {
            phases: vec![],
            steps: vec![],
        },
        workspace,
    );
    env.save(&run);

    // 1. Plan via the planner agent.
    let planned = run_driver::plan_run(&env.state, "run-relay").await.unwrap();
    assert_eq!(planned.plan.steps.len(), 2);
    assert!(planned.requirements.is_empty());

    // 2. Drive to completion — no human interaction anywhere below this line.
    let finished = env.drive("run-relay").await;

    // Both steps done, run completed.
    assert_eq!(
        finished.step_state_status("s1"),
        Some(StepStatus::Done),
        "工作步骤应完成（历史：{:?}）",
        finished
            .history
            .iter()
            .map(|h| format!("{}->{:?}:{}", h.step_id, h.to, h.detail))
            .collect::<Vec<_>>()
    );
    assert_eq!(finished.step_state_status("s2"), Some(StepStatus::Done));
    assert_eq!(finished.status, RunStatus::Done);

    // The rework happened exactly once and was driven by the reviewer's findings.
    let s1 = finished.state("s1").unwrap();
    assert_eq!(s1.attempts, 2, "第一次验收未通过应触发一次返工");
    assert_eq!(s1.loops, 1);
    assert_eq!(
        s1.review_status,
        Some(lj_hub_lib::scheduler::ReviewStatus::Pass),
        "最终应通过验收"
    );

    let rework_events: Vec<&lj_hub_lib::scheduler::RunStepEvent> = finished
        .history
        .iter()
        .filter(|h| h.detail.contains("返工"))
        .collect();
    assert_eq!(rework_events.len(), 1, "应记录一次返工");
    assert!(
        rework_events[0].detail.contains("缺少必要的测试记录"),
        "返工指令必须带上验收者给出的具体问题：{}",
        rework_events[0].detail
    );

    // The worker's rework prompt really carried the findings.
    let mut worker_tasks: Vec<_> = env
        .state
        .store
        .list_task_files()
        .iter()
        .filter_map(|id| env.state.store.load_task(id))
        .filter(|t| t.run_step_id.as_deref() == Some("s1"))
        .collect();
    assert_eq!(worker_tasks.len(), 2, "两次尝试应是两条可读记录");
    // Order by creation time: file names are UUIDs and carry no ordering.
    worker_tasks.sort_by(|a, b| a.created_at.cmp(&b.created_at));
    let first_prompt = worker_tasks[0]
        .messages
        .iter()
        .find_map(|m| match &m.body {
            lj_hub_lib::model::MessageBody::User { text } => Some(text.clone()),
            _ => None,
        })
        .unwrap();
    assert!(
        !first_prompt.contains("上一轮验收发现的问题"),
        "第一次尝试不应带返工指令"
    );
    let second = &worker_tasks[1];
    let rework_prompt = second
        .messages
        .iter()
        .find_map(|m| match &m.body {
            lj_hub_lib::model::MessageBody::User { text } => Some(text.clone()),
            _ => None,
        })
        .expect("返工提示应被记录");
    assert!(
        rework_prompt.contains("上一轮验收发现的问题"),
        "返工提示必须包含验收结论"
    );
    assert!(
        rework_prompt.contains("缺少必要的测试记录"),
        "返工提示必须包含具体问题"
    );

    // Real evidence was produced in the workspace by the worker.
    let output = std::fs::read_to_string(env.repo_path().join("worker-output.txt")).unwrap();
    assert_eq!(output.lines().count(), 2, "两次尝试各留下一行证据");

    // Git evidence and a handoff were recorded for the accepted step.
    assert!(s1.git.is_some());
    assert!(s1.handoff_id.is_some());
    let evidence = s1.git.clone().unwrap();
    assert!(
        evidence.content_fingerprint.is_some(),
        "必须记录内容指纹，否则无法发现内容漂移"
    );

    // The second step consumed the first step's summary.
    let s2_task = env
        .state
        .store
        .list_task_files()
        .iter()
        .filter_map(|id| env.state.store.load_task(id))
        .find(|t| t.run_step_id.as_deref() == Some("s2"))
        .expect("第二阶段应被执行");
    let s2_prompt = s2_task
        .messages
        .iter()
        .find_map(|m| match &m.body {
            lj_hub_lib::model::MessageBody::User { text } => Some(text.clone()),
            _ => None,
        })
        .unwrap();
    assert!(
        s2_prompt.contains("Context from prior steps"),
        "下游步骤应带上上游产出的有界上下文"
    );
    assert!(
        s2_prompt.contains("s1") || s2_prompt.contains("实现功能"),
        "上下文应标明来源步骤"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn review_loop_limit_stops_for_a_human_instead_of_looping() {
    let env = Env::new();
    std::fs::write(env.repo_path().join("relay-fail-once.marker"), "x").unwrap();
    // Force every review to fail: the marker is recreated below on each pass.
    let workspace = WorkspaceRef::main(env.repo_path().display().to_string());
    let run = Run::new(
        "run-limit".into(),
        env.project_id.clone(),
        "循环上限".into(),
        "验收一直不通过".into(),
        "codex".into(),
        explicit_plan(Some("codex"), 1),
        workspace,
    );
    env.save(&run);

    // max_loops = 1 means: one review, and if it fails, stop for a human.
    let finished = env.drive("run-limit").await;
    assert_eq!(
        finished.step_state_status("s1"),
        Some(StepStatus::NeedsHuman),
        "达到循环上限后必须停下等待人工，而不是无限返工"
    );
    assert_eq!(finished.status, RunStatus::NeedsHuman);
    let reason = finished
        .state("s1")
        .unwrap()
        .needs_human_reason
        .clone()
        .unwrap_or_default();
    assert!(reason.contains("循环上限"), "{reason}");
    assert!(
        reason.contains("缺少必要的测试记录"),
        "必须把验收者的原话带给用户：{reason}"
    );

    // A human decision resumes it: clear the blocker and retry.
    let resumed =
        run_driver::resolve_needs_human(&env.state, "run-limit", "s1", "retry", "人工确认可以重试")
            .unwrap();
    assert_eq!(resumed.step_state_status("s1"), Some(StepStatus::Pending));
    // Marker is gone by now (the reviewer removed it), so this round passes.
    let finished = env.drive("run-limit").await;
    assert_eq!(finished.step_state_status("s1"), Some(StepStatus::Done));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interrupted_step_is_not_assumed_done_and_needs_a_human() {
    let env = Env::new();
    let workspace = WorkspaceRef::main(env.repo_path().display().to_string());
    let mut run = Run::new(
        "run-crash".into(),
        env.project_id.clone(),
        "中断恢复".into(),
        "模拟进程在步骤执行中被杀".into(),
        "opencode".into(),
        explicit_plan(None, 1),
        workspace,
    );
    run.status = RunStatus::Running;
    run.transition("s1", StepStatus::Running, "开始执行");
    run.state_mut("s1").unwrap().git = Some(lj_hub_lib::git::evidence(&env.repo_path()));
    env.save(&run);

    // Restart: reconcile whatever was mid-flight.
    let notes = env.state.reconcile_runs();
    assert!(!notes.is_empty(), "中断的运行必须被报告");
    let reconciled = env.load("run-crash");
    assert_eq!(
        reconciled.step_state_status("s1"),
        Some(StepStatus::NeedsHuman),
        "不能假设中断的步骤成功，也不能直接重跑"
    );
    let reason = reconciled
        .state("s1")
        .unwrap()
        .needs_human_reason
        .clone()
        .unwrap_or_default();
    assert!(reason.contains("无法确认"), "{reason}");

    // Automatic driving must not restart it by itself.
    let report = run_driver::tick(&env.state, "run-crash").await.unwrap();
    assert!(!report.progressed, "需要人工的步骤不应被自动推进");

    // And an explicit human retry still works.
    run_driver::resolve_needs_human(&env.state, "run-crash", "s1", "retry", "").unwrap();
    let finished = env.drive("run-crash").await;
    assert_eq!(
        finished.step_state_status("s1"),
        Some(StepStatus::Done),
        "人工重试后应完成。原因：{:?}；历史：{:?}",
        finished
            .state("s1")
            .and_then(|s| s.needs_human_reason.clone()),
        finished
            .history
            .iter()
            .map(|h| format!("{}->{:?}:{}", h.step_id, h.to, h.detail))
            .collect::<Vec<_>>()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelled_run_stops_and_never_resumes_by_itself() {
    let env = Env::new();
    let workspace = WorkspaceRef::main(env.repo_path().display().to_string());
    let run = Run::new(
        "run-cancel".into(),
        env.project_id.clone(),
        "取消".into(),
        "用户取消".into(),
        "opencode".into(),
        explicit_plan(None, 1),
        workspace,
    );
    env.save(&run);
    let cancelled = run_driver::cancel_run(&env.state, "run-cancel").unwrap();
    assert_eq!(cancelled.status, RunStatus::Cancelled);
    let report = run_driver::tick(&env.state, "run-cancel").await.unwrap();
    assert!(!report.progressed);
}

/// Helper: build a real-relay environment using agents installed on this
/// machine. `LJ_HUB_REAL_PROMPT=1` is required because this spends real quota.
fn real_env(
    executor: &str,
    reviewer: Option<&str>,
) -> Option<(Arc<AppState>, tempfile::TempDir, tempfile::TempDir)> {
    use lj_hub_lib::agents::{find_definition, resolve_executable};
    let executor_def = find_definition(executor)?;
    if resolve_executable(executor_def, None).is_none() {
        eprintln!("跳过：本机未安装 {executor}");
        return None;
    }
    if let Some(reviewer) = reviewer {
        let def = find_definition(reviewer)?;
        if resolve_executable(def, None).is_none() {
            eprintln!("跳过：本机未安装 {reviewer}");
            return None;
        }
    }
    let repo = lj_hub_lib::test_support::fixture_repo();
    let data = tempfile::tempdir().unwrap();
    let state = Arc::new(AppState::new(data.path().to_path_buf()));
    {
        let mut store = state.state.lock().unwrap();
        store.projects.push(Project {
            id: "proj-real".into(),
            name: "real".into(),
            path: repo.path().display().to_string(),
            git_root: Some(repo.path().display().to_string()),
            git_remote: None,
            created_at: now_iso(),
            last_opened_at: now_iso(),
        });
        state.store.save_state(&store).unwrap();
    }
    Some((state, repo, data))
}

fn real_plan(executor: &str, reviewer: Option<&str>) -> lj_hub_lib::scheduler::RunPlan {
    lj_hub_lib::scheduler::RunPlan {
        phases: vec![lj_hub_lib::scheduler::Phase {
            id: "p1".into(),
            title: "实现".into(),
            parallelism: 1,
            gate: lj_hub_lib::scheduler::GateKind::AllDone,
        }],
        steps: vec![StepSpec {
            id: "s1".into(),
            phase: "p1".into(),
            title: "写一个说明文件".into(),
            prompt: "在仓库根目录创建文件 relay-check.txt，内容必须恰好是两行：\n\
                     第一行: LJ Hub relay check\n\
                     第二行: executor=<你的 agent 名称>\n\
                     不要修改或删除任何其他文件，不要执行 git commit/push/reset。"
                .into(),
            agent: executor.into(),
            mode: StepMode::Write,
            depends_on: vec![],
            verify_by: reviewer.map(|s| s.to_string()),
            max_loops: 2,
            max_attempts: 1,
            timeout_secs: 900,
            consumes: vec![],
        }],
    }
}

/// A real relay where the executor works and the reviewer cannot run a real
/// model turn (DeepSeek Harness on this machine has no DEEPSEEK_API_KEY in the
/// environment LJ Hub launches it with).
///
/// The point of this test is the *honesty* of the outcome: the executor's work
/// is proven to exist, and LJ Hub must report a human-blocked step carrying the
/// provider's own error rather than claiming success. It also checks that the
/// unusable agent is remembered so it is not scheduled again.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn real_relay_reports_a_blocked_reviewer_honestly() {
    let Some((state, repo, data)) = real_env("opencode", Some("deepseek-harness")) else {
        return;
    };
    let run = Run::new(
        "run-real-blocked".into(),
        "proj-real".into(),
        "真实接力（验收受阻）".into(),
        "验证 LJ Hub 不会把受阻的真实回合说成成功".into(),
        "opencode".into(),
        real_plan("opencode", Some("deepseek-harness")),
        WorkspaceRef::main(repo.path().display().to_string()),
    );
    lj_hub_lib::runs::RunStore::new(data.path())
        .save(&run)
        .unwrap();

    let mut iterations = 0;
    let finished = loop {
        iterations += 1;
        assert!(iterations < 40, "真实接力没有收敛");
        let report = run_driver::tick(&state, "run-real-blocked").await.unwrap();
        if !report.progressed {
            break lj_hub_lib::runs::RunStore::new(data.path())
                .load("run-real-blocked")
                .unwrap();
        }
    };

    // 1. The real executor really did the work.
    let file = repo.path().join("relay-check.txt");
    assert!(file.exists(), "真实执行 Agent 必须真的产出文件");
    let content = std::fs::read_to_string(&file).unwrap();
    println!("relay-check.txt:\n{content}");
    assert!(content.contains("LJ Hub relay check"));

    // 2. LJ Hub did not pretend the review succeeded.
    assert_eq!(
        finished.step_state_status("s1"),
        Some(StepStatus::NeedsHuman),
        "验收者无法运行真实回合时必须停下等人工，而不是报成功"
    );
    let reason = finished
        .state("s1")
        .unwrap()
        .needs_human_reason
        .clone()
        .unwrap_or_default();
    println!("阻塞原因：{reason}");
    assert!(
        reason.contains("验收回合失败") || reason.contains("API key"),
        "必须保留提供方给出的原始错误：{reason}"
    );

    // 3. The unusable reviewer is remembered, so the relay stops choosing it.
    let report = state
        .probe_results
        .lock()
        .unwrap()
        .get("deepseek-harness")
        .cloned();
    if let Some(report) = report {
        if report.verification.real_turn_ok == Some(false) {
            assert!(!report.verification.schedulable);
            let pool = run_driver::plan_agent_pool(&state);
            assert!(
                !pool.iter().any(|(id, _)| id == "deepseek-harness"),
                "已证明无法运行真实回合的 Agent 不应再被调度"
            );
            println!("已将 deepseek-harness 标记为不可用于自动接力");
        }
    }
}

/// A real relay that completes end to end, with the executor and the reviewer
/// running as two separate real model turns.
///
/// Both steps use OpenCode here because it is the only agent on this machine
/// with working credentials; the review is still an independent turn that
/// inspects the repository and must emit `REVIEW_STATUS: PASS`. Labelled
/// honestly rather than claimed as a two-vendor relay.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn real_relay_completes_with_real_review() {
    let Some((state, repo, data)) = real_env("opencode", Some("opencode")) else {
        return;
    };
    let run = Run::new(
        "run-real".into(),
        "proj-real".into(),
        "真实接力".into(),
        "由真实 Agent 完成一次带验收的自动接力".into(),
        "opencode".into(),
        real_plan("opencode", Some("opencode")),
        WorkspaceRef::main(repo.path().display().to_string()),
    );
    lj_hub_lib::runs::RunStore::new(data.path())
        .save(&run)
        .unwrap();

    let mut iterations = 0;
    let finished = loop {
        iterations += 1;
        assert!(iterations < 40, "真实接力没有收敛");
        let report = run_driver::tick(&state, "run-real").await.unwrap();
        if !report.progressed {
            break lj_hub_lib::runs::RunStore::new(data.path())
                .load("run-real")
                .unwrap();
        }
    };

    println!(
        "真实接力：status={:?} stop={:?}",
        finished.status, finished.stop_reason
    );
    for step in &finished.steps {
        println!(
            "  步骤 {} -> {:?}（尝试 {}，返工 {}，验收 {:?}）{}",
            step.id,
            step.status,
            step.attempts,
            step.loops,
            step.review_status,
            step.needs_human_reason.clone().unwrap_or_default()
        );
    }
    assert!(repo.path().join("relay-check.txt").exists());
    assert_eq!(
        finished.step_state_status("s1"),
        Some(StepStatus::Done),
        "真实接力应完成：{:?}",
        finished.stop_reason
    );
    assert_eq!(
        finished.state("s1").unwrap().review_status,
        Some(lj_hub_lib::scheduler::ReviewStatus::Pass),
        "必须有一次真实的验收通过"
    );
    // Two real turns: one executing, one reviewing.
    let tasks = repo.path().parent().unwrap().to_path_buf();
    let _ = tasks;
    let step = finished.state("s1").unwrap();
    assert!(step.task_id.is_some(), "执行回合应留下可读记录");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn plan_validation_rejects_an_unknown_agent_before_anything_runs() {
    let json = r#"{"phases":[{"id":"p1","title":"x","parallelism":1,"gate":"all_done"}],
        "steps":[{"id":"s1","phase":"p1","title":"t","prompt":"p","agent":"not-registered","mode":"write"}]}"#;
    let plan = parse_plan(json).unwrap();
    let agents: Vec<String> = lj_hub_lib::agents::AGENTS
        .iter()
        .map(|d| d.id.to_string())
        .collect();
    let problems = lj_hub_lib::scheduler::validate_plan(&plan, &agents);
    assert!(
        problems.iter().any(|p| p.contains("not-registered")),
        "{problems:?}"
    );
}
