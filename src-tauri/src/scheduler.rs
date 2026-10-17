//! Run scheduling: a model-free state machine for multi-agent relays.
//!
//! A *run* is one long development task split into steps that different agents
//! execute in turn (planner → worker → reviewer → rework …). The state machine
//! here is deliberately pure: it never talks to a model, so basic progress
//! (what may start, what is blocked, when to rework, when to stop) is
//! deterministic, testable and works even if no agent is reachable.
//!
//! Design notes borrowed from FreeBuddy (MIT) after auditing its
//! `packages/workflow-core/src/scheduler.ts`:
//!  - phases run sequentially, steps inside a phase may run in parallel up to
//!    the phase's `parallelism`;
//!  - a *global* one-writer-at-a-time rule: two steps that write are never
//!    allowed to run concurrently, even in different phases;
//!  - dependencies are declared per step and must be terminal-ok;
//!  - review loops end as loop / finish / partial with an explicit limit;
//!  - upstream context attached to a prompt is bounded, with the truncation
//!    made visible instead of silently dropping text.
//!
//! LJ Hub keeps its own implementation and its own extra guarantees (see
//! [`compose_step_prompt`]: user requirements and forbidden items are never
//! truncated, and every step records Git evidence).

use crate::model::{now_iso, GitEvidence, RequirementRef};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap};

/// Bound on the whole "context from prior steps" block in a composed prompt.
pub const CONSUMED_CONTEXT_MAX_CHARS: usize = 24_000;
/// Bound on a single upstream step's attached output.
pub const CONSUMED_STEP_MAX_CHARS: usize = 12_000;
/// Maximum number of upstream blocks attached to one prompt.
pub const CONSUMED_MAX_BLOCKS: usize = 32;
pub const TRUNCATION_MARKER: &str = "\n\n...[上下文因长度被截断]...\n\n";

/// Default hard limits; a plan may lower but not silently raise them.
pub const DEFAULT_MAX_LOOPS: u32 = 3;
pub const DEFAULT_MAX_ATTEMPTS: u32 = 2;
pub const DEFAULT_STEP_TIMEOUT_SECS: u64 = 3600;
pub const MAX_ALLOWED_LOOPS: u32 = 10;
pub const MAX_ALLOWED_ATTEMPTS: u32 = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum StepStatus {
    /// Nothing started yet; eligible once dependencies and slots allow.
    Pending,
    /// An agent session is working on it.
    Running,
    /// Work finished, handoff material produced, waiting for the reviewer.
    AwaitingHandoff,
    /// The reviewer is checking the handoff.
    Verifying,
    /// Accepted (review passed, or no reviewer configured).
    Done,
    /// Failed; may be retried until `max_attempts`.
    Failed,
    /// Stopped by the user.
    Cancelled,
    /// Cannot proceed without a human decision. Never auto-retried.
    NeedsHuman,
}

impl StepStatus {
    /// Terminal states never run again and never block dependents.
    pub fn is_terminal_ok(self) -> bool {
        matches!(self, StepStatus::Done)
    }

    /// States that occupy a scheduler slot.
    pub fn is_active(self) -> bool {
        matches!(
            self,
            StepStatus::Running | StepStatus::AwaitingHandoff | StepStatus::Verifying
        )
    }

    pub fn is_blocking_dependents(self) -> bool {
        matches!(
            self,
            StepStatus::Pending
                | StepStatus::Running
                | StepStatus::AwaitingHandoff
                | StepStatus::Verifying
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StepMode {
    /// Does not modify the workspace (analysis, review, planning).
    Read,
    /// Modifies the workspace; only one may run at a time across the whole run.
    Write,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum RunStatus {
    Draft,
    Running,
    /// Every step finished but at least one ended in a non-OK terminal state.
    Partial,
    Done,
    Failed,
    Cancelled,
    /// Blocked on a human decision.
    NeedsHuman,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GateKind {
    AllDone,
    ReviewRequired,
    ManualApproval,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Phase {
    pub id: String,
    pub title: String,
    /// How many steps of this phase may run at once (minimum 1).
    pub parallelism: u32,
    pub gate: GateKind,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepSpec {
    pub id: String,
    pub phase: String,
    pub title: String,
    /// What this step must accomplish, as written by the planner/user.
    pub prompt: String,
    pub agent: String,
    pub mode: StepMode,
    #[serde(default)]
    pub depends_on: Vec<String>,
    /// Agent that checks this step's result. Empty means "no review".
    #[serde(default)]
    pub verify_by: Option<String>,
    #[serde(default = "default_max_loops")]
    pub max_loops: u32,
    #[serde(default = "default_max_attempts")]
    pub max_attempts: u32,
    #[serde(default = "default_timeout")]
    pub timeout_secs: u64,
    /// Upstream steps whose (bounded) output is attached to this step's prompt.
    #[serde(default)]
    pub consumes: Vec<String>,
}

fn default_max_loops() -> u32 {
    DEFAULT_MAX_LOOPS
}
fn default_max_attempts() -> u32 {
    DEFAULT_MAX_ATTEMPTS
}
fn default_timeout() -> u64 {
    DEFAULT_STEP_TIMEOUT_SECS
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunPlan {
    pub phases: Vec<Phase>,
    pub steps: Vec<StepSpec>,
}

impl RunPlan {
    pub fn step(&self, id: &str) -> Option<&StepSpec> {
        self.steps.iter().find(|s| s.id == id)
    }
    pub fn phase(&self, id: &str) -> Option<&Phase> {
        self.phases.iter().find(|p| p.id == id)
    }
}

/// Which workspace a step runs in.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceRef {
    /// Absolute path the agent session is started with.
    pub root: String,
    /// `main` for the project directory itself, `worktree` for an isolated
    /// checkout created for this run.
    pub kind: String,
    /// Branch name when `kind == "worktree"`.
    pub branch: Option<String>,
}

impl WorkspaceRef {
    pub fn main(root: impl Into<String>) -> Self {
        Self {
            root: root.into(),
            kind: "main".into(),
            branch: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ReviewStatus {
    Pass,
    Fail,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepState {
    pub id: String,
    pub status: StepStatus,
    pub attempts: u32,
    /// Completed review loops (a FAIL → rework → review counts as one).
    pub loops: u32,
    /// LJ Hub task id backing the current/last execution (never the agent's own
    /// session id: those are not portable between agents).
    pub task_id: Option<String>,
    pub summary: Option<String>,
    pub review_status: Option<ReviewStatus>,
    pub unresolved: Option<u32>,
    /// Concrete findings the reviewer produced; fed into the rework prompt.
    #[serde(default)]
    pub findings: Vec<String>,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub git: Option<GitEvidence>,
    pub handoff_id: Option<String>,
    pub workspace: WorkspaceRef,
    pub failure_reason: Option<String>,
    pub needs_human_reason: Option<String>,
}

impl StepState {
    pub fn new(spec: &StepSpec, workspace: WorkspaceRef) -> Self {
        Self {
            id: spec.id.clone(),
            status: StepStatus::Pending,
            attempts: 0,
            loops: 0,
            task_id: None,
            summary: None,
            review_status: None,
            unresolved: None,
            findings: Vec::new(),
            started_at: None,
            finished_at: None,
            git: None,
            handoff_id: None,
            workspace,
            failure_reason: None,
            needs_human_reason: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunPolicy {
    /// Stop after this many total agent turns to bound cost.
    pub max_step_executions: u32,
    /// Whether rework steps are created automatically after a FAIL.
    pub auto_rework: bool,
}

impl Default for RunPolicy {
    fn default() -> Self {
        Self {
            max_step_executions: 40,
            auto_rework: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunStepEvent {
    pub at: String,
    pub step_id: String,
    pub from: StepStatus,
    pub to: StepStatus,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Run {
    pub id: String,
    pub project_id: String,
    pub title: String,
    pub objective: String,
    pub agent: String,
    pub plan: RunPlan,
    /// Where every step of this run works. Stored on the run (not derived from
    /// its steps) so a run can be planned before any step exists.
    #[serde(default = "default_main_workspace")]
    pub workspace: WorkspaceRef,
    #[serde(default)]
    pub steps: Vec<StepState>,
    pub status: RunStatus,
    #[serde(default)]
    pub policy: RunPolicy,
    /// Requirements carried into every step prompt, verbatim.
    #[serde(default)]
    pub requirements: Vec<RequirementRef>,
    /// Hard constraints, carried into every step prompt, verbatim.
    #[serde(default)]
    pub forbidden: Vec<String>,
    #[serde(default)]
    pub history: Vec<RunStepEvent>,
    pub created_at: String,
    pub updated_at: String,
    /// Why the run stopped needing automation.
    pub stop_reason: Option<String>,
}

fn default_main_workspace() -> WorkspaceRef {
    WorkspaceRef {
        root: String::new(),
        kind: "main".into(),
        branch: None,
    }
}

impl Run {
    pub fn new(
        id: String,
        project_id: String,
        title: String,
        objective: String,
        agent: String,
        plan: RunPlan,
        workspace: WorkspaceRef,
    ) -> Self {
        let steps = plan
            .steps
            .iter()
            .map(|spec| StepState::new(spec, workspace.clone()))
            .collect();
        Self {
            id,
            project_id,
            title,
            objective,
            agent,
            plan,
            workspace,
            steps,
            status: RunStatus::Draft,
            policy: RunPolicy::default(),
            requirements: Vec::new(),
            forbidden: Vec::new(),
            history: Vec::new(),
            created_at: now_iso(),
            updated_at: now_iso(),
            stop_reason: None,
        }
    }

    pub fn state(&self, step_id: &str) -> Option<&StepState> {
        self.steps.iter().find(|s| s.id == step_id)
    }

    pub fn state_mut(&mut self, step_id: &str) -> Option<&mut StepState> {
        self.steps.iter_mut().find(|s| s.id == step_id)
    }

    pub fn executions(&self) -> u32 {
        self.steps.iter().map(|s| s.attempts).sum()
    }

    /// Move a step to a new status and record why. This is the only place that
    /// mutates status, so every transition is auditable.
    pub fn transition(&mut self, step_id: &str, to: StepStatus, detail: impl Into<String>) {
        let detail = detail.into();
        let from = match self.state_mut(step_id) {
            Some(state) => {
                let from = state.status;
                state.status = to;
                from
            }
            None => return,
        };
        if from != to {
            self.history.push(RunStepEvent {
                at: now_iso(),
                step_id: step_id.to_string(),
                from,
                to,
                detail,
            });
        }
        self.updated_at = now_iso();
    }

    /// Recompute the run status from its steps. Called after every transition.
    pub fn refresh_status(&mut self) {
        if self.status == RunStatus::Cancelled {
            return;
        }
        let any_needs_human = self
            .steps
            .iter()
            .any(|s| s.status == StepStatus::NeedsHuman);
        let any_active = self.steps.iter().any(|s| s.status.is_active());
        let any_pending = self.steps.iter().any(|s| s.status == StepStatus::Pending);
        let any_failed = self.steps.iter().any(|s| s.status == StepStatus::Failed);

        let all_finished = self.steps.iter().all(|s| {
            matches!(
                s.status,
                StepStatus::Done
                    | StepStatus::Failed
                    | StepStatus::Cancelled
                    | StepStatus::NeedsHuman
            )
        });

        self.status = if any_needs_human && !any_active && !any_pending {
            RunStatus::NeedsHuman
        } else if any_active || any_pending {
            RunStatus::Running
        } else if all_finished {
            if any_failed || any_needs_human {
                RunStatus::Partial
            } else {
                RunStatus::Done
            }
        } else {
            self.status
        };
        self.updated_at = now_iso();
    }
}

// ---------------------------------------------------------------- planning

/// Validate a parsed plan. Returns a list of human-readable problems; an empty
/// list means the plan is safe to execute.
pub fn validate_plan(plan: &RunPlan, known_agents: &[String]) -> Vec<String> {
    let mut problems = Vec::new();
    if plan.phases.is_empty() {
        problems.push("计划没有任何阶段（phases）".to_string());
    }
    if plan.steps.is_empty() {
        problems.push("计划没有任何步骤（steps）".to_string());
    }

    let mut phase_ids = BTreeSet::new();
    for phase in &plan.phases {
        if !phase_ids.insert(phase.id.clone()) {
            problems.push(format!("阶段 id 重复：{}", phase.id));
        }
    }

    let mut step_ids = BTreeSet::new();
    for step in &plan.steps {
        if !step_ids.insert(step.id.clone()) {
            problems.push(format!("步骤 id 重复：{}", step.id));
        }
        if !phase_ids.contains(&step.phase) {
            problems.push(format!(
                "步骤 {} 引用了不存在的阶段 {}",
                step.id, step.phase
            ));
        }
        if !known_agents.is_empty() && !known_agents.iter().any(|a| a == &step.agent) {
            problems.push(format!(
                "步骤 {} 指定的 Agent `{}` 未登记",
                step.id, step.agent
            ));
        }
        if let Some(verifier) = &step.verify_by {
            if !known_agents.is_empty() && !known_agents.iter().any(|a| a == verifier) {
                problems.push(format!(
                    "步骤 {} 的验收 Agent `{}` 未登记",
                    step.id, verifier
                ));
            }
        }
        if step.max_loops > MAX_ALLOWED_LOOPS {
            problems.push(format!(
                "步骤 {} 的 max_loops={} 超过上限 {MAX_ALLOWED_LOOPS}",
                step.id, step.max_loops
            ));
        }
        if step.max_attempts > MAX_ALLOWED_ATTEMPTS {
            problems.push(format!(
                "步骤 {} 的 max_attempts={} 超过上限 {MAX_ALLOWED_ATTEMPTS}",
                step.id, step.max_attempts
            ));
        }
        if step.prompt.trim().is_empty() {
            problems.push(format!("步骤 {} 没有任务描述", step.id));
        }
        for dep in &step.depends_on {
            if dep == &step.id {
                problems.push(format!("步骤 {} 依赖自己", step.id));
            } else if !plan.steps.iter().any(|s| &s.id == dep) {
                problems.push(format!("步骤 {} 依赖不存在的步骤 {}", step.id, dep));
            }
        }
        for consumed in &step.consumes {
            if !plan.steps.iter().any(|s| &s.id == consumed) {
                problems.push(format!(
                    "步骤 {} 引用了不存在的上游输出 {}",
                    step.id, consumed
                ));
            }
        }
    }

    // Dependency cycles (DFS with colours).
    let mut color: HashMap<&str, u8> = HashMap::new();
    fn visit<'a>(
        id: &'a str,
        plan: &'a RunPlan,
        color: &mut HashMap<&'a str, u8>,
        problems: &mut Vec<String>,
    ) {
        match color.get(id).copied().unwrap_or(0) {
            1 => {
                problems.push(format!("依赖存在环：{id}"));
                return;
            }
            2 => return,
            _ => {}
        }
        color.insert(id, 1);
        if let Some(step) = plan.step(id) {
            for dep in &step.depends_on {
                visit(dep, plan, color, problems);
            }
        }
        color.insert(id, 2);
    }
    for step in &plan.steps {
        visit(&step.id, plan, &mut color, &mut problems);
    }

    // A plan that can never make progress in parallel is fine, but a plan whose
    // parallelism exceeds 1 while containing dependent writes is a smell we
    // surface rather than silently serialise.
    let write_steps: Vec<&StepSpec> = plan
        .steps
        .iter()
        .filter(|s| s.mode == StepMode::Write)
        .collect();
    if write_steps.len() > 1 {
        for phase in &plan.phases {
            if phase.parallelism > 1 {
                let writes_in_phase = write_steps.iter().filter(|s| s.phase == phase.id).count();
                if writes_in_phase > 1 {
                    problems.push(format!(
                        "阶段 {} 声明并行度 {}，但包含 {} 个写入步骤；写入步骤会被强制串行执行",
                        phase.id, phase.parallelism, writes_in_phase
                    ));
                }
            }
        }
    }

    problems
}

/// Parse a plan out of an agent's answer.
///
/// The answer must contain a JSON object with `phases` and `steps`; fenced
/// ```json blocks are preferred, a bare object is accepted as a fallback.
pub fn parse_plan(text: &str) -> Result<RunPlan, Vec<String>> {
    let candidates = extract_json_candidates(text);
    if candidates.is_empty() {
        return Err(vec![
            "回答中找不到 JSON 计划；需要包含 phases 与 steps 的 JSON 对象".to_string(),
        ]);
    }
    let mut errors = Vec::new();
    for candidate in candidates {
        match serde_json::from_str::<RunPlan>(&candidate) {
            Ok(plan) => return Ok(plan),
            Err(e) => errors.push(format!("JSON 解析失败：{e}")),
        }
    }
    Err(errors)
}

fn extract_json_candidates(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    // Fenced blocks first.
    let mut rest = text;
    while let Some(start) = rest.find("```") {
        let after = &rest[start + 3..];
        let body_start = after.find('\n').map(|i| i + 1).unwrap_or(after.len());
        let body = &after[body_start..];
        if let Some(end) = body.find("```") {
            let block = body[..end].trim();
            if block.starts_with('{') {
                out.push(block.to_string());
            }
            rest = &body[end + 3..];
        } else {
            break;
        }
    }
    // Then a brace-balanced scan of the whole text.
    let bytes = text.as_bytes();
    let mut depth = 0usize;
    let mut begin = None;
    let mut in_string = false;
    let mut escaped = false;
    for (index, byte) in bytes.iter().enumerate() {
        let ch = *byte as char;
        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }
        match ch {
            '"' => in_string = true,
            '{' => {
                if depth == 0 {
                    begin = Some(index);
                }
                depth += 1;
            }
            '}' if depth > 0 => {
                depth -= 1;
                if depth == 0 {
                    if let Some(start) = begin {
                        let candidate = &text[start..=index];
                        if candidate.contains("\"steps\"") && candidate.contains("\"phases\"") {
                            out.push(candidate.to_string());
                        }
                    }
                    begin = None;
                }
            }
            _ => {}
        }
    }
    out
}

/// Prompt that asks a planner agent for a machine-readable plan.
pub fn planner_prompt(objective: &str, available_agents: &[String]) -> String {
    format!(
        "ROLE: PLANNER\n\
         你是本次协作任务的规划者。请把下面的目标拆分成可执行、可验证的步骤。\n\n\
         目标：\n{objective}\n\n\
         可用 Agent id：{}\n\
         （普通编码工作优先交给成本较低的 Agent；规划、关键决策与验收交给能力最强的 Agent。）\n\n\
         只输出一个 JSON 对象，不要输出任何解释文字。格式：\n\
         {{\n  \"phases\": [{{\"id\": \"p1\", \"title\": \"阶段名\", \"parallelism\": 1, \"gate\": \"all_done\"}}],\n  \
         \"steps\": [{{\n    \"id\": \"s1\", \"phase\": \"p1\", \"title\": \"步骤名\",\n    \
         \"prompt\": \"给执行 Agent 的具体指令（它看不到本规划对话）\",\n    \"agent\": \"agent-id\",\n    \
         \"mode\": \"write\" 或 \"read\",\n    \"depends_on\": [],\n    \"verify_by\": \"agent-id 或 null\",\n    \
         \"max_loops\": 3, \"max_attempts\": 2, \"consumes\": []\n  }}]\n}}\n\n\
         约束：\n\
         - 同一个文件可能被修改的步骤必须放在同一阶段并且 mode 为 write；\n\
         - 写入步骤会被强制串行执行，不要依赖并行写入；\n\
         - 需要修改代码的步骤必须有 verify_by，由另一个 Agent 验收；\n\
         - 每个 step 的 prompt 必须自包含：执行者看不到你现在的对话；\n\
         - 不要输出 markdown 代码块以外的文字。",
        available_agents.join(", ")
    )
}

/// Prompt that asks a reviewer agent to check the handoff material.
pub fn reviewer_prompt(step_title: &str, handoff_markdown: &str, git_summary: &str) -> String {
    format!(
        "ROLE: REVIEWER\n\
         你是验收者。请核对下面这份交接资料与实际仓库状态，判断产物是否满足要求。\n\n\
         被验收的步骤：{step_title}\n\n\
         当前 Git 事实：\n{git_summary}\n\n\
         交接资料：\n{handoff_markdown}\n\n\
         请按以下规则作答，必须包含机器可读标记：\n\
         - 通过：最后一行输出 `REVIEW_STATUS: PASS`\n\
         - 不通过：先列出具体问题，再输出 `REVIEW_STATUS: FAIL` 与 `UNRESOLVED: <问题数量>`\n\
         不要修改任何文件；只做检查并给出结论。"
    )
}

// ------------------------------------------------------------- scheduling

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunnableStep {
    pub step_id: String,
    pub phase_id: String,
}

#[derive(Debug, Clone, Default)]
pub struct SchedulingContext {
    /// True if a write step is currently active anywhere in the run.
    pub write_busy: bool,
}

/// Decide which steps may start right now.
///
/// Rules (all enforced here, not by prompting an agent):
/// 1. only the first phase that still has unfinished work is considered;
/// 2. at most `parallelism` steps of that phase may be active at once;
/// 3. `depends_on` must all be terminal-ok;
/// 4. at most one *write* step may be active in the entire run.
pub fn select_runnable(run: &Run, ctx: &SchedulingContext) -> Vec<RunnableStep> {
    let mut out = Vec::new();
    let mut write_started = ctx.write_busy;

    for phase in &run.plan.phases {
        let phase_steps: Vec<&StepSpec> = run
            .plan
            .steps
            .iter()
            .filter(|s| s.phase == phase.id)
            .collect();
        if phase_steps.is_empty() {
            continue;
        }

        let states: Vec<&StepState> = phase_steps
            .iter()
            .filter_map(|spec| run.state(&spec.id))
            .collect();

        // A step is "settled" if it is terminal or waiting on a human.
        let phase_finished = states.iter().all(|s| {
            matches!(
                s.status,
                StepStatus::Done
                    | StepStatus::Cancelled
                    | StepStatus::Failed
                    | StepStatus::NeedsHuman
                    | StepStatus::AwaitingHandoff
                    | StepStatus::Verifying
            )
        });
        // AwaitingHandoff/Verifying still count as in-progress for *starting*
        // new steps of the phase, but do not block the phase from being
        // considered.
        let phase_still_working = states.iter().any(|s| {
            matches!(
                s.status,
                StepStatus::Pending
                    | StepStatus::Running
                    | StepStatus::Failed
                    | StepStatus::AwaitingHandoff
                    | StepStatus::Verifying
            )
        });
        if phase_finished || !phase_still_working {
            continue;
        }

        let running = states.iter().filter(|s| s.status.is_active()).count();
        let mut slots = phase.parallelism.max(1).saturating_sub(running as u32);

        for spec in &phase_steps {
            if slots == 0 {
                break;
            }
            let Some(state) = run.state(&spec.id) else {
                continue;
            };
            if state.status != StepStatus::Pending {
                continue;
            }
            let deps_ok = spec.depends_on.iter().all(|dep| {
                run.state(dep)
                    .map(|s| s.status.is_terminal_ok())
                    .unwrap_or(false)
            });
            if !deps_ok {
                continue;
            }
            if spec.mode == StepMode::Write && write_started {
                // Never start a second writer; the caller re-runs selection
                // after a writer finishes.
                continue;
            }
            out.push(RunnableStep {
                step_id: spec.id.clone(),
                phase_id: phase.id.clone(),
            });
            slots -= 1;
            if spec.mode == StepMode::Write {
                write_started = true;
            }
        }
        // Only the first unfinished phase is considered in one pass.
        return out;
    }
    out
}

// ------------------------------------------------------------- review loop

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LoopOutcome {
    /// Reviewer asked for changes; a rework attempt should run.
    Loop,
    /// Reviewer passed, or there is nothing to rework.
    Finish,
    /// Loop budget exhausted: a human must decide.
    Partial,
}

/// Decide what happens after a reviewer finished.
pub fn decide_review(
    review_status: Option<ReviewStatus>,
    reviewer_finished_ok: bool,
    loops_done: u32,
    max_loops: u32,
) -> LoopOutcome {
    if !reviewer_finished_ok {
        return LoopOutcome::Partial;
    }
    match review_status {
        Some(ReviewStatus::Pass) => LoopOutcome::Finish,
        None => LoopOutcome::Finish,
        Some(ReviewStatus::Fail) => {
            let effective = max_loops.max(1);
            if loops_done + 1 < effective {
                LoopOutcome::Loop
            } else {
                LoopOutcome::Partial
            }
        }
    }
}

/// Parse the machine-readable markers out of a reviewer answer.
pub fn parse_review_markers(text: &str) -> (Option<ReviewStatus>, Option<u32>) {
    let upper = text.to_uppercase();
    let fail = upper.contains("<<<REVIEW_FAIL>>>")
        || upper.contains("[[REVIEW:FAIL]]")
        || last_status(&upper) == Some(ReviewStatus::Fail);
    let pass = upper.contains("<<<REVIEW_PASS>>>")
        || upper.contains("[[REVIEW:PASS]]")
        || last_status(&upper) == Some(ReviewStatus::Pass);

    // Last marker wins, matching FreeBuddy's behaviour for conflicting output.
    let status = if fail && !pass {
        Some(ReviewStatus::Fail)
    } else if pass && !fail {
        Some(ReviewStatus::Pass)
    } else if pass && fail {
        last_status(&upper)
    } else {
        None
    };

    let upper = text.to_uppercase();
    let mut unresolved_values: Vec<u32> = Vec::new();
    for (index, _) in upper.match_indices("UNRESOLVED") {
        let tail = &upper[index..];
        let digits: String = tail
            .chars()
            .skip_while(|c| !c.is_ascii_digit())
            .take_while(|c| c.is_ascii_digit())
            .collect();
        if let Ok(value) = digits.parse::<u32>() {
            unresolved_values.push(value);
        }
    }
    let unresolved = unresolved_values.last().copied();

    (status, unresolved)
}

/// Short, human readable statement of where a review loop stands, used in the
/// "needs a human" message so the operator sees what the reviewer said.
pub fn review_loop_state(reviewer_text: &str) -> String {
    let (status, unresolved) = parse_review_markers(reviewer_text);
    let mut out = String::new();
    out.push_str(match status {
        Some(ReviewStatus::Pass) => "验收结论：通过",
        Some(ReviewStatus::Fail) => "验收结论：未通过",
        None => "验收结论：未给出可识别的结论标记",
    });
    if let Some(count) = unresolved {
        out.push_str(&format!("；未解决问题数：{count}"));
    }
    let excerpt = bound_text(reviewer_text.trim(), 800);
    if !excerpt.is_empty() {
        out.push_str(&format!("\n验收原文摘要：\n{excerpt}"));
    }
    out
}

fn last_status(upper: &str) -> Option<ReviewStatus> {
    let mut found = None;
    for (index, _) in upper.match_indices("REVIEW") {
        let tail = &upper[index..];
        if let Some(rest) = tail.split("STATUS").nth(1) {
            let rest = rest.trim_start_matches(|c: char| {
                c == ':' || c == '-' || c == '_' || c.is_whitespace()
            });
            if rest.starts_with("FAIL") {
                found = Some(ReviewStatus::Fail);
            } else if rest.starts_with("PASS") {
                found = Some(ReviewStatus::Pass);
            }
        }
    }
    found
}

// ------------------------------------------------------------ context build

/// Attach bounded upstream output to a base prompt.
///
/// Unlike a plain summary, this keeps a visible truncation marker and never
/// exceeds the stated budget, so an agent can tell that context was cut and go
/// read the original itself.
pub fn augment_with_consumed(
    base: &str,
    consumes: &[String],
    outputs: &BTreeMap<String, (String, String)>, // id -> (title, text)
) -> String {
    if consumes.is_empty() {
        return base.to_string();
    }
    let mut refs: Vec<(String, String)> = Vec::new();
    let mut seen = BTreeSet::new();
    for id in consumes {
        if refs.len() >= CONSUMED_MAX_BLOCKS {
            break;
        }
        if !seen.insert(id.clone()) {
            continue;
        }
        if let Some((title, text)) = outputs.get(id) {
            let text = text.trim();
            if !text.is_empty() {
                let header = if title.len() > 160 {
                    format!("--- {}… ---\n", &title[..159])
                } else {
                    format!("--- {title} ---\n")
                };
                refs.push((header, text.to_string()));
            }
        }
    }
    if refs.is_empty() {
        return base.to_string();
    }

    let prefix = "\n\nContext from prior steps:\n";
    let separators = refs.len().saturating_sub(1) * 2;
    let fixed: usize =
        prefix.len() + separators + refs.iter().map(|(header, _)| header.len()).sum::<usize>();
    let mut remaining = CONSUMED_CONTEXT_MAX_CHARS.saturating_sub(fixed);

    let mut blocks = Vec::new();
    for (index, (header, text)) in refs.iter().enumerate() {
        let remaining_blocks = refs.len() - index;
        let fair_share = remaining / remaining_blocks;
        let limit = CONSUMED_STEP_MAX_CHARS.min(fair_share);
        let bounded = bound_text(text, limit);
        remaining = remaining.saturating_sub(bounded.len());
        blocks.push(format!("{header}{bounded}"));
    }

    format!("{base}{prefix}{}", blocks.join("\n\n"))
}

/// Head/tail truncation with a visible marker (never silently cut).
pub fn bound_text(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    if max_chars <= TRUNCATION_MARKER.chars().count() {
        return text.chars().take(max_chars).collect();
    }
    let available = max_chars - TRUNCATION_MARKER.chars().count();
    let head = (available as f64 * 0.6).ceil() as usize;
    let tail = available.saturating_sub(head);
    let head_text: String = text.chars().take(head).collect();
    let tail_text: String = text
        .chars()
        .rev()
        .take(tail)
        .collect::<Vec<char>>()
        .into_iter()
        .rev()
        .collect();
    format!("{head_text}{TRUNCATION_MARKER}{tail_text}")
}

/// Compose the prompt a step's agent receives.
///
/// Hard guarantees:
/// - `requirements` and `forbidden` are always included **in full** (they are
///   not subject to the context budget, because dropping a hard requirement is
///   the failure mode this whole design exists to avoid);
/// - upstream context is bounded and the truncation is visible.
pub fn compose_step_prompt(
    run: &Run,
    spec: &StepSpec,
    outputs: &BTreeMap<String, (String, String)>,
    rework_findings: &[String],
) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "ROLE: WORKER\n你在 LJ Hub 的自动接力流程中执行一个已定义的步骤。\n\n\
         整体目标：{}\n\n当前步骤：{}\n\n本步骤要求：\n{}\n\n",
        run.objective, spec.title, spec.prompt
    ));

    if !run.requirements.is_empty() {
        out.push_str("仍然有效的用户要求（必须遵守，不得省略）：\n");
        for req in &run.requirements {
            out.push_str(&format!(
                "- [{}] {} （来源：{}）\n",
                req.id, req.summary, req.source
            ));
        }
        out.push('\n');
    }

    if !run.forbidden.is_empty() {
        out.push_str("禁止事项（硬性约束）：\n");
        for item in &run.forbidden {
            out.push_str(&format!("- {item}\n"));
        }
        out.push('\n');
    }

    if !rework_findings.is_empty() {
        out.push_str("上一轮验收发现的问题（必须逐条处理）：\n");
        for finding in rework_findings {
            out.push_str(&format!("- {finding}\n"));
        }
        out.push('\n');
    }

    out.push_str(&format!(
        "工作目录：{}\n工作模式：{}\n\n",
        spec.workspace_hint(),
        match spec.mode {
            StepMode::Write => "write（会修改文件）",
            StepMode::Read => "read（只读分析，不要修改文件）",
        }
    ));

    out.push_str(
        "规则：\n\
         - 只在这个工作目录内工作；\n\
         - 不要执行 git push、不要 reset --hard、不要 clean -fd；\n\
         - 完成后在最后一行输出一行摘要，格式：`STEP_SUMMARY: <一句话结果>`。\n",
    );

    augment_with_consumed(&out, &spec.consumes, outputs)
}

impl StepSpec {
    /// Textual hint about where the agent should work; the authoritative
    /// directory is passed to the session, this is only for the prompt body.
    fn workspace_hint(&self) -> String {
        "(由会话的 cwd 指定，见运行记录中的 workspace)".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::RequirementRef;

    fn phase(id: &str, parallelism: u32) -> Phase {
        Phase {
            id: id.into(),
            title: id.into(),
            parallelism,
            gate: GateKind::AllDone,
        }
    }

    fn step(id: &str, phase_id: &str, mode: StepMode) -> StepSpec {
        StepSpec {
            id: id.into(),
            phase: phase_id.into(),
            title: id.into(),
            prompt: format!("do {id}"),
            agent: "worker".into(),
            mode,
            depends_on: vec![],
            verify_by: None,
            max_loops: DEFAULT_MAX_LOOPS,
            max_attempts: DEFAULT_MAX_ATTEMPTS,
            timeout_secs: DEFAULT_STEP_TIMEOUT_SECS,
            consumes: vec![],
        }
    }

    fn run_with(plan: RunPlan) -> Run {
        Run::new(
            "run-1".into(),
            "proj-1".into(),
            "t".into(),
            "objective".into(),
            "planner".into(),
            plan,
            WorkspaceRef::main("/tmp/ws"),
        )
    }

    #[test]
    fn plan_validation_catches_broken_plans() {
        let mut plan = RunPlan {
            phases: vec![phase("p1", 1)],
            steps: vec![
                step("s1", "p1", StepMode::Write),
                step("s2", "p2", StepMode::Write),
            ],
        };
        plan.steps[1].depends_on = vec!["ghost".into()];
        plan.steps[0].verify_by = Some("nobody".into());
        let problems = validate_plan(&plan, &["worker".into(), "planner".into()]);
        assert!(problems.iter().any(|p| p.contains("不存在的阶段 p2")));
        assert!(problems.iter().any(|p| p.contains("ghost")));
        assert!(problems.iter().any(|p| p.contains("nobody")));
    }

    #[test]
    fn dependency_cycles_are_rejected() {
        let mut plan = RunPlan {
            phases: vec![phase("p1", 1)],
            steps: vec![
                step("a", "p1", StepMode::Read),
                step("b", "p1", StepMode::Read),
            ],
        };
        plan.steps[0].depends_on = vec!["b".into()];
        plan.steps[1].depends_on = vec!["a".into()];
        let problems = validate_plan(&plan, &[]);
        assert!(problems.iter().any(|p| p.contains("环")), "{problems:?}");
    }

    #[test]
    fn only_one_write_step_runs_at_a_time() {
        let plan = RunPlan {
            phases: vec![phase("p1", 3)],
            steps: vec![
                step("w1", "p1", StepMode::Write),
                step("w2", "p1", StepMode::Write),
                step("r1", "p1", StepMode::Read),
            ],
        };
        let run = run_with(plan);
        let picked = select_runnable(&run, &SchedulingContext::default());
        assert_eq!(picked.len(), 2, "一个写入步骤 + 一个只读步骤");
        assert_eq!(
            picked.iter().filter(|p| p.step_id.starts_with('w')).count(),
            1
        );
        assert!(picked.iter().any(|p| p.step_id == "r1"));
    }

    #[test]
    fn phases_run_sequentially_and_dependencies_gate() {
        let mut plan = RunPlan {
            phases: vec![phase("p1", 1), phase("p2", 1)],
            steps: vec![
                step("a", "p1", StepMode::Read),
                step("b", "p2", StepMode::Read),
            ],
        };
        plan.steps[1].depends_on = vec!["a".into()];
        let mut run = run_with(plan);
        let picked = select_runnable(&run, &SchedulingContext::default());
        assert_eq!(picked.len(), 1);
        assert_eq!(
            picked[0].step_id, "a",
            "第二阶段的步骤在第三阶段之前不能开始"
        );

        run.transition("a", StepStatus::Done, "ok");
        let picked = select_runnable(&run, &SchedulingContext::default());
        assert_eq!(picked.len(), 1);
        assert_eq!(picked[0].step_id, "b");
    }

    #[test]
    fn awaiting_handoff_blocks_new_steps_in_the_same_phase() {
        let plan = RunPlan {
            phases: vec![phase("p1", 1)],
            steps: vec![
                step("a", "p1", StepMode::Write),
                step("b", "p1", StepMode::Read),
            ],
        };
        let mut run = run_with(plan);
        run.transition("a", StepStatus::AwaitingHandoff, "waiting for review");
        let picked = select_runnable(&run, &SchedulingContext::default());
        assert!(picked.is_empty(), "并行度为 1 时不应同时开第二个步骤");
    }

    #[test]
    fn review_loop_respects_the_limit() {
        assert_eq!(
            decide_review(Some(ReviewStatus::Pass), true, 0, 3),
            LoopOutcome::Finish
        );
        assert_eq!(
            decide_review(Some(ReviewStatus::Fail), true, 0, 3),
            LoopOutcome::Loop
        );
        assert_eq!(
            decide_review(Some(ReviewStatus::Fail), true, 2, 3),
            LoopOutcome::Partial,
            "达到循环上限后必须交给人工，而不是继续重试"
        );
        assert_eq!(
            decide_review(Some(ReviewStatus::Fail), false, 0, 3),
            LoopOutcome::Partial
        );
    }

    #[test]
    fn review_markers_are_parsed_with_last_one_winning() {
        let (status, unresolved) =
            parse_review_markers("问题 1\nREVIEW_STATUS: FAIL\nUNRESOLVED: 2");
        assert_eq!(status, Some(ReviewStatus::Fail));
        assert_eq!(unresolved, Some(2));

        let (status, _) = parse_review_markers("先 FAIL\nREVIEW_STATUS: PASS");
        assert_eq!(status, Some(ReviewStatus::Pass));

        let (status, _) = parse_review_markers("<<<REVIEW_PASS>>>");
        assert_eq!(status, Some(ReviewStatus::Pass));

        let (status, _) = parse_review_markers("没有任何标记");
        assert_eq!(status, None);
    }

    #[test]
    fn requirements_and_forbidden_items_are_never_truncated() {
        let plan = RunPlan {
            phases: vec![phase("p1", 1)],
            steps: vec![step("s1", "p1", StepMode::Read)],
        };
        let mut run = run_with(plan);
        // A very long hard requirement that would never survive a summary.
        let long_requirement = "R".repeat(8_000);
        run.requirements = vec![RequirementRef {
            id: "SEC-1".into(),
            summary: long_requirement.clone(),
            state: "active".into(),
            source: "USER_REQUIREMENTS.md".into(),
        }];
        run.forbidden = vec!["不得上传密钥".into()];

        let outputs = BTreeMap::new();
        let prompt = compose_step_prompt(&run, run.plan.step("s1").unwrap(), &outputs, &[]);
        assert!(
            prompt.contains(&long_requirement),
            "硬性要求必须原样出现在提示里"
        );
        assert!(prompt.contains("不得上传密钥"));
    }

    #[test]
    fn upstream_context_is_bounded_and_marked() {
        let huge = "X".repeat(60_000);
        let mut outputs = BTreeMap::new();
        outputs.insert("s0".to_string(), ("big step".to_string(), huge.clone()));
        let base = "BASE PROMPT";
        let composed = augment_with_consumed(base, &["s0".to_string()], &outputs);
        assert!(composed.starts_with(base));
        assert!(composed.contains(TRUNCATION_MARKER), "截断必须可见");
        assert!(composed.len() < huge.len() + base.len());
        assert!(composed.len() < CONSUMED_CONTEXT_MAX_CHARS + base.len() + 500);
    }

    #[test]
    fn progress_is_recorded_as_history() {
        let plan = RunPlan {
            phases: vec![phase("p1", 1)],
            steps: vec![step("s1", "p1", StepMode::Read)],
        };
        let mut run = run_with(plan);
        run.status = RunStatus::Running;
        run.transition("s1", StepStatus::Running, "started");
        run.transition("s1", StepStatus::Done, "accepted");
        assert_eq!(run.history.len(), 2);
        assert_eq!(run.history[0].from, StepStatus::Pending);
        assert_eq!(run.history[1].to, StepStatus::Done);
        run.refresh_status();
        assert_eq!(run.status, RunStatus::Done);
    }

    #[test]
    fn needs_human_is_terminal_and_visible() {
        let plan = RunPlan {
            phases: vec![phase("p1", 1)],
            steps: vec![step("s1", "p1", StepMode::Write)],
        };
        let mut run = run_with(plan);
        run.status = RunStatus::Running;
        run.transition("s1", StepStatus::NeedsHuman, "agent unreachable");
        run.refresh_status();
        assert_eq!(run.status, RunStatus::NeedsHuman);
        // A human-blocked step must not be picked up again automatically.
        assert!(select_runnable(&run, &SchedulingContext::default()).is_empty());
    }

    #[test]
    fn planner_prompt_lists_agents_and_demands_json() {
        let prompt = planner_prompt("实现登录", &["opencode".into(), "planner".into()]);
        assert!(prompt.contains("ROLE: PLANNER"));
        assert!(prompt.contains("opencode, planner"));
        assert!(prompt.contains("\"steps\""));
    }

    #[test]
    fn plan_json_is_extracted_from_noisy_output() {
        let answer = "好的，这是计划：\n```json\n{\n \"phases\":[{\"id\":\"p1\",\"title\":\"a\",\"parallelism\":1,\"gate\":\"all_done\"}],\n \"steps\":[{\"id\":\"s1\",\"phase\":\"p1\",\"title\":\"b\",\"prompt\":\"do it\",\"agent\":\"worker\",\"mode\":\"write\"}]\n}\n```\n需要我继续吗？";
        let plan = parse_plan(answer).expect("plan parsed");
        assert_eq!(plan.steps.len(), 1);
        assert_eq!(plan.steps[0].agent, "worker");
        assert_eq!(plan.steps[0].max_loops, DEFAULT_MAX_LOOPS);

        assert!(parse_plan("没有 JSON").is_err());
    }
}
