# 审计与设计：自动接力（Phase 0 交付）

本文件是 Phase 0 的对照报告，以及 Phase 1–3 的最小实现方案与已完成情况。
所有结论都基于**实际源码与实际运行结果**，标注了证据来源。

---

## 一、对照审计

### 1. FreeBuddy（`maojindao55/freebuddy`，审计版本 v0.10.45 / `5e117a5`）

审计方式：只读克隆到临时目录，逐文件阅读指定源码，并由独立审计
交叉核对（引用 `文件:行号`）。仓库根目录**没有 LICENSE 文件**，仅 README 声明 MIT；
`third_party/deepseek-harness/` 内是上游补丁与 JS overlay，**未附许可文本**，
因此本仓库**没有复制 FreeBuddy 任何代码**，只借鉴机制。

真实已实现（关键项）：

| 机制 | 事实 | 位置 |
|---|---|---|
| 运行/步骤状态机 | Run: `pending_approval, running, paused, blocked, completed, failed, killed, partial`；Step: `pending, running, done, failed, skipped, blocked` | `packages/protocol/src/workflow.ts:45-61` |
| 调度选择（纯函数） | 只考虑第一个未完成阶段；阶段内按 `parallelism` 填槽；依赖需 terminal-ok；**全局一次只允许一个 write** | `packages/workflow-core/src/scheduler.ts:25-79` |
| 阶段门 | `all_done / manual_approval / review_required` | `scheduler.ts:91-112` |
| 审核循环 | `decideReviewLoop` / `decideImplementReviewLoop`，默认 3 轮，超限 → `partial` | `scheduler.ts:117-127, 423-440` |
| 控制标记 | `REVIEW_STATUS: PASS|FAIL`、`<<<REVIEW_PASS>>>`、`UNRESOLVED: n` | `scheduler.ts:368-418` |
| 上游上下文注入 | 24k 总预算 / 12k 每步 / ≤32 块；截断保留 60% 头 + 40% 尾并加标记 | `scheduler.ts:481-513` |
| 步骤摘要 | 截断时**保留控制标记** | `scheduler.ts:368-378` |
| Worktree | `<dataDir>/task-worktrees/<repo>-<hash>/<taskKey>`，`git worktree add --detach`，**无分支、无合并、无冲突处理、无 worktree 表** | `electron/cli/taskWorkspace.ts:212-243` |
| 交接简报 | originalGoal 2000 字符、最近 3 条用户消息、最后助手摘要 2000、文件变更 ≤50、8 条摘录；预算 64 KiB；**裁剪是静默的**（8→4→2→0 等），没有“要求可能丢失”的提示 | `electron/cli/handoffBriefExtractor.ts:130-236` |
| 委派（真实程序化路径） | 每个委派回合注入 `freebuddy-delegate` MCP stdio server，Bearer 能力令牌 POST 到本机端点；有 self-delegate/深度/回弹/相似度守卫；FIFO 队列 + 并发上限 | `electron/delegationToolService.ts:25-136`、`packages/delegation-core/src/protocol/guards.ts:21-44` |
| 崩溃恢复 | RUNNING 步骤**直接标 blocked 后重跑**（re-execution，不是重新核验） | `electron/cli/workflows.ts`（`markRunningWorkflowsBlocked`）、`workflow-runtime/src/runtime.ts:463-505` |
| 步骤超时/自动重试 | **不存在**（`retry` 只有类型定义，从未实现） | `packages/protocol/src/workflow.ts:235-238` |
| 结果 Git 核验 | **不存在**：无 HEAD/未提交/哈希校验，只信任 agent 自我标记 | 全仓库 grep 无相关实现 |
| WorkBuddy 接入 | **没有**：仅 package.json 描述里出现 “WorkBuddy-style” | `package.json:6` |
| TRAE 接入 | 全仓库零匹配 | — |
| Qoder / CodeBuddy | 真实适配器：`qodercli`、`codebuddy`（Tencent npm） | `electron/cli/adapters.ts:199-226` |

仅存在于文档/计划、不可当作已交付：模板 2.0 节点/边引擎（条件与审批节点从未求值）、
`WorkflowTemplateNode.retry`、多 Agent 活动视图。

### 2. LJ Hub（本次开发前，`9e4f08c`）

已有并保留：ACP 会话运行时（真实握手、流式、取消、权限、fs 沙箱）、
Agent 登记与真实握手测试、Git 只读检查、交接三阶段 + 处置、产物登记与 SHA-256、
本地 JSON 原子持久化、WorkBuddy Open API 适配器、macOS 打包与 CI。

真实缺口（本次补齐）：没有任务调度器、没有自动接力、没有步骤级持久化与恢复、
没有内容指纹（只比文件名/HEAD）、没有 worktree、没有真实回合验证的分级。

### 3. 结论：值得借鉴 vs 不值得重复

**借鉴（已按我们自己的实现落地）**：阶段顺序 + 并行度槽位、全局单写者、依赖门控、
审核循环与上限、控制标记、有界上下文 + 可见截断标记、摘要保留控制标记。

**不借鉴 / 刻意不照抄**：

- 静默裁剪交接内容（我们改为：硬性要求与禁止事项**永不截断**，其余截断必须可见）；
- 只信任 agent 的自我标记（我们额外做 Git + 内容指纹核验，并拒绝接受漂移）；
- 中断后直接重跑（我们先核对事实，交人工确认，避免重复副作用）；
- 无分支的 detached worktree（我们建真实分支并记录映射，合并可回滚）；
- 复制其客户端（Electron/React 体量与我们的 Tauri 定位不符）。

---

## 二、Phase 1–3 已实现的设计

### 状态机（`src/scheduler.rs`，纯函数，13 个单测）

步骤状态：`PENDING → RUNNING → (AWAITING_HANDOFF → VERIFYING →)* DONE`
失败与人工分支：`FAILED`（可重试，受 `max_attempts` 限制）、`CANCELLED`、`NEEDS_HUMAN`。
运行状态：`DRAFT / RUNNING / PARTIAL / DONE / FAILED / CANCELLED / NEEDS_HUMAN`。

不依赖大模型的判定：能否开始（阶段 + 槽位 + 依赖 + 单写者）、审核结论如何处理
（`LoopOutcome::{Loop, Finish, Partial}`）、何时停止。

### 驱动（`src/run_driver.rs`）

每次 tick 只做一件事并立即落盘：

1. `AWAITING_HANDOFF` 的步骤 → 由验收 Agent 验收（先重新核验，再提问）；
2. 否则选择下一个可运行步骤 → 创建任务记录 → 在指定工作目录启动 ACP 会话 →
   发送**组合提示**（目标 + 硬性要求 + 禁止事项 + 上游有界上下文 + 返工问题）→
   等待结束 → 记录 Git 证据与内容指纹 → 生成交接资料 → 进入验收或完成。

关键保证：

- **幂等**：`attempts` 在回合开始前自增；已完成步骤不会被再次选中；
- **有界**：每次尝试有超时；`max_attempts` 限制重试；`max_loops` 限制返工；
  整个运行有 `max_step_executions` 上限；
- **可追溯**：状态变化写入 `history`（含原因），每个回合的对话都作为任务保存，
  界面可直接打开阅读（`run_id` / `run_step_id` 回链）；
- **不伪造**：验收失败、回合失败、工作区漂移都写 `NEEDS_HUMAN` 并保留提供方原文错误。

### 恢复（`src/runs.rs`）

启动时对上次处于 `RUNNING` 的运行做核对：`RUNNING/VERIFYING/AWAITING_HANDOFF`
的步骤一律转为 `NEEDS_HUMAN`，并在原因里附上开始时记录的 HEAD / 改动数 / 内容指纹，
提示"无法确认是否完成，请人工决定重试或标记完成"。
`PENDING` 步骤保持可恢复。运行记录损坏会**报告**而不是静默忽略。

### 并行安全（`src/worktree.rs`，7 个单测）

- 每个运行可在独立 worktree + 真实分支（`lj-hub/<slug>-<id>`）中执行；
- 主工作区有未提交改动时**拒绝创建**（避免把别人的成果带进隔离区）；
- 索引记录 项目 → 仓库 → worktree → 分支 → 运行 的映射，重启后仍可解释；
- 集成为显式动作：目标分支必须匹配、目标必须干净、先用 `merge-tree` 干跑，
  真合并失败**自动 `merge --abort` 回滚**并报告冲突文件；
- 删除 worktree 时若仍有未提交改动则拒绝，除非用户明确选择丢弃。

### Agent 能力分级（本次新增）

`AgentVerification { executable_resolved, handshake_ok, real_turn_ok, schedulable }`

- 握手成功**不等于**能跑真实回合；真实回合成功**才**允许参与自动接力；
- 真实回合失败（例如缺少 `DEEPSEEK_API_KEY`）会被记住并**不再调度该 Agent**，
  界面显示原始错误；
- 需要供应商环境变量的 CLI，可在 Agent 设置里填写**变量名**（不存值），
  LJ Hub 只在自己进程环境里取值并传给子进程。

---

## 三、实测结果（本机，2026-10-17）

| 项目 | 结果 |
|---|---|
| 模拟 Agent 端到端接力 | 计划 → 执行 → 交接 → 验收 FAIL → 自动返工（带具体问题）→ 验收 PASS → 下一阶段 → 完成；无需人工 |
| 崩溃恢复 | 中断步骤 → `NEEDS_HUMAN` 且不被自动重跑；人工重试后正常完成 |
| 循环上限 | `max_loops=1` 时验收失败即停止，原因包含验收者原文 |
| 取消 | 运行取消后不再自行推进 |
| **真实接力（可完成）** | executor=OpenCode、reviewer=OpenCode：真实执行回合产出 `relay-check.txt`，真实验收回合 `REVIEW_STATUS: PASS`，运行 `DONE`（约 129 秒，2 次真实模型回合） |
| **真实接力（受阻）** | executor=OpenCode 成功产出文件；reviewer=DeepSeek Harness 真实回合失败（缺 `DEEPSEEK_API_KEY`）→ 步骤 `NEEDS_HUMAN`，保留原始错误，未伪造成功 |

测试合计：68 单测 + 9 ACP 协议 + 4 真实 Agent + 6 接力（含 2 个忽略的真实回合测试）。
`cargo clippy -D warnings` 与 `vue-tsc` 均通过。

---

## 四、仍未解决 / 下一步

1. **真实两厂商接力**：本机只有 OpenCode 具备可用的真实模型凭据；
   DeepSeek Harness 的 ACP 握手正常但真实回合缺 key。需要用户提供环境变量，
   或等待该 CLI 的凭据机制，LJ Hub 不会代为索取或存储密钥。
2. 并行执行目前只在同一运行内按阶段与单写者规则串行；跨运行的并发需要显式
   把不同运行放进不同 worktree（机制已就绪，尚未在 UI 上自动化调度）。
3. 验收结论依赖 Agent 输出的标记；若某个 Agent 不遵守格式，会落到"未给出可识别结论"
   并交人工（这是刻意的保守选择）。
4. WorkBuddy 云端任务的 ACP 通道（SSE + POST）仍未实现。
5. 计划质量取决于规划 Agent；LJ Hub 只做校验（未知 Agent、环、重复 id、上限），
   不做"计划是否合理"的价值判断。
